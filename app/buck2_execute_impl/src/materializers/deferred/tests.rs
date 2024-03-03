/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;

use buck2_common::file_ops::FileMetadata;
use buck2_execute::digest_config::DigestConfig;
use buck2_execute::directory::insert_file;
use buck2_execute::directory::ActionDirectoryBuilder;
use buck2_execute::materialize::materializer::DeferredMaterializerSubscription;
use dupe::Dupe;

use super::Version;
use super::VersionTracker;
use super::*;

#[test]
fn test_find_artifacts() -> anyhow::Result<()> {
    let artifact1 = ProjectRelativePathBuf::unchecked_new("foo/bar/baz".to_owned());
    let artifact2 = ProjectRelativePathBuf::unchecked_new("foo/bar/bar/qux".to_owned());
    let artifact3 = ProjectRelativePathBuf::unchecked_new("foo/bar/bar/quux".to_owned());
    let artifact4 = ProjectRelativePathBuf::unchecked_new("foo/bar/qux/quuz".to_owned());
    let non_artifact1 = ProjectRelativePathBuf::unchecked_new("foo/bar/qux".to_owned());
    let non_artifact2 = ProjectRelativePathBuf::unchecked_new("foo/bar/bar/corge".to_owned());

    let file = FileMetadata::empty(DigestConfig::testing_default().cas_digest_config());

    // Build deps with artifacts 1-3, and non-artifacts 1-2
    let mut builder = ActionDirectoryBuilder::empty();
    insert_file(&mut builder, &artifact1.join_normalized("f1")?, file.dupe())?;
    insert_file(
        &mut builder,
        &artifact2.join_normalized("d/f1")?,
        file.dupe(),
    )?;
    insert_file(&mut builder, &artifact3, file.dupe())?;
    insert_file(&mut builder, &non_artifact2, file.dupe())?;
    builder.mkdir(&non_artifact1)?;

    // Build tree with artifacts 1-4
    let mut tree: FileTree<()> = FileTree::new();
    tree.insert(artifact1.iter().map(|f| f.to_owned()), ());
    tree.insert(artifact2.iter().map(|f| f.to_owned()), ());
    tree.insert(artifact3.iter().map(|f| f.to_owned()), ());
    tree.insert(artifact4.iter().map(|f| f.to_owned()), ());

    let expected_artifacts: HashSet<_> =
        vec![artifact1, artifact2, artifact3].into_iter().collect();
    let found_artifacts: HashSet<_> = tree.find_artifacts(&builder).into_iter().collect();
    assert_eq!(found_artifacts, expected_artifacts);
    Ok(())
}

#[test]
fn test_remove_path() {
    fn insert(tree: &mut FileTree<String>, path: &str) {
        tree.insert(
            ProjectRelativePath::unchecked_new(path)
                .iter()
                .map(|f| f.to_owned()),
            path.to_owned(),
        );
    }

    let mut tree: FileTree<String> = FileTree::new();
    insert(&mut tree, "a/b/c/d");
    insert(&mut tree, "a/b/c/e");
    insert(&mut tree, "a/c");

    let removed_subtree = tree.remove_path(ProjectRelativePath::unchecked_new("a/b"));
    // Convert to HashMap<String, String> so it's easier to test
    let removed_subtree: HashMap<String, String> = removed_subtree
        .map(|(k, v)| (k.as_str().to_owned(), v))
        .collect();

    assert_eq!(removed_subtree.len(), 2);
    assert_eq!(removed_subtree.get("a/b/c/d"), Some(&"a/b/c/d".to_owned()));
    assert_eq!(removed_subtree.get("a/b/c/e"), Some(&"a/b/c/e".to_owned()));
}

mod state_machine {
    use std::path::Path;

    use anyhow::Context;
    use assert_matches::assert_matches;
    use buck2_execute::directory::Symlink;
    use buck2_execute::directory::INTERNER;
    use buck2_util::threads::ignore_stack_overflow_checks_for_future;
    use parking_lot::Mutex;
    use tokio::time::sleep;
    use tokio::time::Duration as TokioDuration;

    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    enum Op {
        Clean,
        Materialize,
        MaterializeError,
    }

    struct StubIoHandler {
        log: Mutex<Vec<(Op, ProjectRelativePathBuf)>>,
        fail: Mutex<bool>,
        fail_paths: Mutex<Vec<ProjectRelativePathBuf>>,
        // If set, add a sleep when materializing to simulate a long materialization period
        materialization_config: HashMap<ProjectRelativePathBuf, TokioDuration>,
        digest_config: DigestConfig,
    }

    impl StubIoHandler {
        fn take_log(&self) -> Vec<(Op, ProjectRelativePathBuf)> {
            std::mem::take(&mut *self.log.lock())
        }

        fn set_fail(&self, fail: bool) {
            *self.fail.lock() = fail;
        }

        fn set_fail_on(&self, paths: Vec<ProjectRelativePathBuf>) {
            *self.fail_paths.lock() = paths;
        }

        pub fn new(materialization_config: HashMap<ProjectRelativePathBuf, TokioDuration>) -> Self {
            Self {
                log: Default::default(),
                fail: Default::default(),
                fail_paths: Default::default(),
                materialization_config,
                digest_config: DigestConfig::testing_default(),
            }
        }
    }

    #[async_trait]
    impl IoHandler for StubIoHandler {
        fn write<'a>(
            self: &Arc<Self>,
            _path: ProjectRelativePathBuf,
            _write: Arc<WriteFile>,
            _version: Version,
            _command_sender: MaterializerSender<Self>,
            _cancellations: &'a CancellationContext<'a>,
        ) -> BoxFuture<'a, Result<(), SharedMaterializingError>> {
            unimplemented!()
        }

        fn clean_path<'a>(
            self: &Arc<Self>,
            path: ProjectRelativePathBuf,
            version: Version,
            command_sender: MaterializerSender<Self>,
            _cancellations: &'a CancellationContext,
        ) -> BoxFuture<'a, Result<(), buck2_error::Error>> {
            self.log.lock().push((Op::Clean, path.clone()));

            async move {
                let _ignored = command_sender.send_low_priority(
                    LowPriorityMaterializerCommand::CleanupFinished {
                        path,
                        version,
                        result: Ok(()),
                    },
                );
                Ok(())
            }
            .boxed()
        }

        async fn materialize_entry(
            self: &Arc<Self>,
            path: ProjectRelativePathBuf,
            _method: Arc<ArtifactMaterializationMethod>,
            _entry: ActionDirectoryEntry<ActionSharedDirectory>,
            _event_dispatcher: EventDispatcher,
            _cancellations: &CancellationContext,
        ) -> Result<(), MaterializeEntryError> {
            // Simulate a non-immediate materialization if configured
            match self.materialization_config.get(&path) {
                Some(duration) => {
                    sleep(*duration).await;
                }
                None => (),
            }

            if (*self.fail_paths.lock()).contains(&path) || *self.fail.lock() {
                self.log.lock().push((Op::MaterializeError, path));
                Err(anyhow::anyhow!("Injected error").into())
            } else {
                self.log.lock().push((Op::Materialize, path));
                Ok(())
            }
        }

        fn create_ttl_refresh(
            self: &Arc<Self>,
            _tree: &ArtifactTree,
            _min_ttl: Duration,
        ) -> Option<BoxFuture<'static, anyhow::Result<()>>> {
            unimplemented!()
        }

        fn buck_out_path(&self) -> &ProjectRelativePathBuf {
            unimplemented!()
        }

        fn io_executor(&self) -> &dyn BlockingExecutor {
            unimplemented!()
        }

        fn re_client_manager(&self) -> &Arc<ReConnectionManager> {
            unimplemented!()
        }

        fn fs(&self) -> &ProjectRoot {
            unimplemented!()
        }

        fn digest_config(&self) -> DigestConfig {
            self.digest_config
        }
    }

    /// A stub command sender. We are calling materializer methods directly so that's all we need.
    fn channel() -> (
        MaterializerSender<StubIoHandler>,
        MaterializerReceiver<StubIoHandler>,
    ) {
        // We don't use those counts in tests.
        static SENT: AtomicUsize = AtomicUsize::new(0);
        static RECEIVED: AtomicUsize = AtomicUsize::new(0);

        let (hi_send, hi_recv) = mpsc::unbounded_channel();
        let (lo_send, lo_recv) = mpsc::unbounded_channel();
        let counters = MaterializerCounters {
            sent: &SENT,
            received: &RECEIVED,
        };

        (
            MaterializerSender {
                high_priority: Cow::Owned(hi_send),
                low_priority: Cow::Owned(lo_send),
                counters,
            },
            MaterializerReceiver {
                high_priority: hi_recv,
                low_priority: lo_recv,
                counters,
            },
        )
    }

    fn make_path(p: &str) -> ProjectRelativePathBuf {
        ProjectRelativePath::new(p).unwrap().to_owned()
    }

    fn make_processor(
        materialization_config: HashMap<ProjectRelativePathBuf, TokioDuration>,
    ) -> (
        DeferredMaterializerCommandProcessor<StubIoHandler>,
        MaterializerReceiver<StubIoHandler>,
    ) {
        let (command_sender, command_receiver) = channel();

        (
            DeferredMaterializerCommandProcessor {
                io: Arc::new(StubIoHandler::new(materialization_config)),
                sqlite_db: None,
                rt: Handle::current(),
                defer_write_actions: true,
                log_buffer: LogBuffer::new(1),
                version_tracker: VersionTracker::new(),
                command_sender,
                tree: ArtifactTree::new(),
                subscriptions: MaterializerSubscriptions::new(),
                ttl_refresh_history: Default::default(),
                ttl_refresh_instance: Default::default(),
                cancellations: CancellationContext::testing(),
                stats: Arc::new(DeferredMaterializerStats::default()),
                access_times_buffer: Default::default(),
                verbose_materializer_log: true,
            },
            command_receiver,
        )
    }

    #[tokio::test]
    async fn test_declare_reuse() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async {
            let (mut dm, _) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();

            let path = make_path("foo/bar");
            let value = ArtifactValue::file(digest_config.empty_file());

            dm.declare(
                &path,
                value.dupe(),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, path.clone())]);

            let res = dm
                .materialize_artifact(&path, EventDispatcher::null())
                .context("Expected a future")?
                .await;
            assert_eq!(dm.io.take_log(), &[(Op::Materialize, path.clone())]);

            dm.materialization_finished(
                path.clone(),
                Utc::now(),
                dm.version_tracker.current(),
                res,
            );
            assert_eq!(dm.io.take_log(), &[]);

            // When redeclaring the same artifact nothing happens.
            dm.declare(
                &path,
                value.dupe(),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[]);

            // When declaring the same artifact but under it, we clean it and it's a new artifact.
            let path2 = make_path("foo/bar/baz");
            dm.declare(
                &path2,
                value.dupe(),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, path2.clone())]);

            let _ignore = dm
                .materialize_artifact(&path2, EventDispatcher::null())
                .context("Expected a future")?
                .await;
            assert_eq!(dm.io.take_log(), &[(Op::Materialize, path2.clone())]);

            Ok(())
        })
        .await
    }

    fn make_artifact_value_with_symlink_dep(
        target_path: &ProjectRelativePathBuf,
        target_from_symlink: &RelativePathBuf,
        digest_config: DigestConfig,
    ) -> anyhow::Result<ArtifactValue> {
        let mut deps = ActionDirectoryBuilder::empty();
        let target = ActionDirectoryEntry::Leaf(ActionDirectoryMember::File(FileMetadata::empty(
            digest_config.cas_digest_config(),
        )));
        deps.insert(target_path.as_forward_relative_path(), target)?;
        let symlink_value = ArtifactValue::new(
            ActionDirectoryEntry::Leaf(ActionDirectoryMember::Symlink(Arc::new(Symlink::new(
                target_from_symlink.clone(),
            )))),
            Some(
                deps.fingerprint(digest_config.as_directory_serializer())
                    .shared(&*INTERNER),
            ),
        );
        Ok(symlink_value)
    }

    #[tokio::test]
    async fn test_materialize_symlink_and_target() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async {
            // Construct a tree with a symlink and its target, materialize both at once
            let symlink_path = make_path("foo/bar_symlink");
            let target_path = make_path("foo/bar_target");
            let target_from_symlink = RelativePathBuf::from_path(Path::new("bar_target"))?;

            let mut materialization_config = HashMap::new();
            // Materialize the symlink target slowly so that we actually hit the logic point where we
            // await for symlink targets and the entry materialization
            materialization_config.insert(target_path.clone(), TokioDuration::from_millis(100));

            let (mut dm, _) = make_processor(materialization_config);
            let digest_config = dm.io.digest_config();

            // Declare symlink target
            dm.declare(
                &target_path,
                ArtifactValue::file(digest_config.empty_file()),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, target_path.clone())]);

            // Declare symlink
            let symlink_value = make_artifact_value_with_symlink_dep(
                &target_path,
                &target_from_symlink,
                digest_config,
            )?;
            dm.declare(
                &symlink_path,
                symlink_value,
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, symlink_path.clone())]);

            dm.materialize_artifact(&symlink_path, EventDispatcher::null())
                .context("Expected a future")?
                .await
                .map_err(|_| anyhow::anyhow!("error materializing"))?;

            let logs = dm.io.take_log();
            if cfg!(unix) {
                assert_eq!(
                    logs,
                    &[
                        (Op::Materialize, symlink_path.clone()),
                        (Op::Materialize, target_path.clone())
                    ]
                );
            } else {
                assert_eq!(
                    logs,
                    &[
                        (Op::Materialize, target_path.clone()),
                        (Op::Materialize, symlink_path.clone())
                    ]
                );
            }
            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_materialize_symlink_first_then_target() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async {
            // Materialize a symlink, then materialize the target. Test that we still
            // materialize deps if the main artifact has already been materialized.
            let symlink_path = make_path("foo/bar_symlink");
            let target_path = make_path("foo/bar_target");
            let target_from_symlink = RelativePathBuf::from_path(Path::new("bar_target"))?;

            let mut materialization_config = HashMap::new();
            // Materialize the symlink target slowly so that we actually hit the logic point where we
            // await for symlink targets and the entry materialization
            materialization_config.insert(target_path.clone(), TokioDuration::from_millis(100));

            let (mut dm, _) = make_processor(materialization_config);
            let digest_config = dm.io.digest_config();

            // Declare symlink
            let symlink_value = make_artifact_value_with_symlink_dep(
                &target_path,
                &target_from_symlink,
                digest_config,
            )?;
            dm.declare(
                &symlink_path,
                symlink_value,
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, symlink_path.clone())]);

            // Materialize the symlink, at this point the target is not in the tree so it's ignored
            let res = dm
                .materialize_artifact(&symlink_path, EventDispatcher::null())
                .context("Expected a future")?
                .await;

            let logs = dm.io.take_log();
            assert_eq!(logs, &[(Op::Materialize, symlink_path.clone())]);

            // Mark the symlink as materialized
            dm.materialization_finished(
                symlink_path.clone(),
                Utc::now(),
                dm.version_tracker.current(),
                res,
            );
            assert_eq!(dm.io.take_log(), &[]);

            // Declare symlink target
            dm.declare(
                &target_path,
                ArtifactValue::file(digest_config.empty_file()),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, target_path.clone())]);

            // Materialize the symlink again.
            // This time, we don't re-materialize the symlink as that's already been done.
            // But we still materialize the target as that has not been materialized yet.
            dm.materialize_artifact(&symlink_path, EventDispatcher::null())
                .context("Expected a future")?
                .await
                .map_err(|_| anyhow::anyhow!("error materializing"))?;

            let logs = dm.io.take_log();
            assert_eq!(logs, &[(Op::Materialize, target_path.clone())]);

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_subscription_create_destroy() {
        let (mut dm, mut channel) = make_processor(Default::default());

        let handle = {
            let (sender, recv) = oneshot::channel();
            MaterializerSubscriptionOperation::Create { sender }.execute(&mut dm);
            recv.await.unwrap()
        };

        assert!(dm.subscriptions.has_subscription(&handle));

        drop(handle);

        while let Ok(cmd) = channel.high_priority.try_recv() {
            dm.process_one_command(cmd);
        }

        assert!(!dm.subscriptions.has_any_subscriptions());
    }

    #[tokio::test]
    async fn test_subscription_notifications() {
        ignore_stack_overflow_checks_for_future(async {
            let (mut dm, mut channel) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();
            let value = ArtifactValue::file(digest_config.empty_file());

            let mut handle = {
                let (sender, recv) = oneshot::channel();
                MaterializerSubscriptionOperation::Create { sender }.execute(&mut dm);
                recv.await.unwrap()
            };

            let foo_bar = make_path("foo/bar");
            let foo_bar_baz = make_path("foo/bar/baz");
            let bar = make_path("bar");
            let qux = make_path("qux");

            dm.declare_existing(&foo_bar, value.dupe());

            handle.subscribe_to_paths(vec![foo_bar_baz.clone(), bar.clone()]);
            while let Ok(cmd) = channel.high_priority.try_recv() {
                dm.process_one_command(cmd);
            }

            dm.declare_existing(&bar, value.dupe());
            dm.declare_existing(&foo_bar_baz, value.dupe());
            dm.declare_existing(&qux, value.dupe());

            let mut paths = Vec::new();
            while let Ok(path) = handle.receiver().try_recv() {
                paths.push(path);
            }

            assert_eq!(paths, vec![foo_bar_baz.clone(), bar, foo_bar_baz]);
        })
        .await
    }

    #[tokio::test]
    async fn test_subscription_subscribe_also_materializes() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async {
            let (mut dm, mut channel) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();
            let value = ArtifactValue::file(digest_config.empty_file());

            let mut handle = {
                let (sender, recv) = oneshot::channel();
                MaterializerSubscriptionOperation::Create { sender }.execute(&mut dm);
                recv.await.unwrap()
            };

            let foo_bar = make_path("foo/bar");

            dm.declare(
                &foo_bar,
                value.dupe(),
                Box::new(ArtifactMaterializationMethod::Test),
            );

            handle.subscribe_to_paths(vec![foo_bar.clone()]);
            while let Ok(cmd) = channel.high_priority.try_recv() {
                dm.process_one_command(cmd);
            }

            // We need to yield to let the materialization task run. If we had a handle to it, we'd
            // just await it, but the subscription isn't retaining those handles.
            let mut log = Vec::new();
            while log.len() < 2 {
                log.extend(dm.io.take_log());
                tokio::task::yield_now().await;
            }

            assert_eq!(
                &log,
                &[
                    (Op::Clean, foo_bar.clone()),
                    (Op::Materialize, foo_bar.clone())
                ]
            );

            // Drain low priority commands. This should include our materialization finished message,
            // at which point we'll notify the subscription handle.
            while let Ok(cmd) = channel.low_priority.try_recv() {
                dm.process_one_low_priority_command(cmd);
            }

            let mut paths = Vec::new();
            while let Ok(path) = handle.receiver().try_recv() {
                paths.push(path);
            }

            assert_eq!(paths, vec![foo_bar]);

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_subscription_unsubscribe() {
        ignore_stack_overflow_checks_for_future(async {
            let (mut dm, mut channel) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();
            let value1 = ArtifactValue::file(digest_config.empty_file());
            let value2 = ArtifactValue::dir(digest_config.empty_directory());

            let mut handle = {
                let (sender, recv) = oneshot::channel();
                MaterializerSubscriptionOperation::Create { sender }.execute(&mut dm);
                recv.await.unwrap()
            };

            let path = make_path("foo/bar");

            handle.subscribe_to_paths(vec![path.clone()]);
            while let Ok(cmd) = channel.high_priority.try_recv() {
                dm.process_one_command(cmd);
            }

            dm.declare_existing(&path, value1.dupe());

            handle.unsubscribe_from_paths(vec![path.clone()]);
            while let Ok(cmd) = channel.high_priority.try_recv() {
                dm.process_one_command(cmd);
            }

            dm.declare_existing(&path, value2.dupe());

            let mut paths = Vec::new();
            while let Ok(path) = handle.receiver().try_recv() {
                paths.push(path);
            }

            // Expect only one notification
            assert_eq!(paths, vec![path]);
        })
        .await
    }

    #[tokio::test]
    async fn test_invalidate_error() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async{
            let (mut dm, _) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();

            let path = make_path("test/invalidate/failure");
            let value1 = ArtifactValue::file(digest_config.empty_file());
            let value2 = ArtifactValue::dir(digest_config.empty_directory());

            // Start from having something.
            dm.declare_existing(&path, value1);

            // This will collect the existing future and invalidate, and then fail in doing so.
            dm.declare(&path, value2, Box::new(ArtifactMaterializationMethod::Test));

            // Now we check that materialization fails. This needs to wait on the previous clean.
            let res = dm
                .materialize_artifact(&path, EventDispatcher::null())
                .context("Expected a future")?
                .await;

            assert_matches!(
            res,
            Err(SharedMaterializingError::Error(e)) if format!("{:#}", e).contains("Injected error")
        );

            // We do not actually get to materializing or cleaning.
            assert_eq!(dm.io.take_log(), &[]);

            Ok(())
        }).await
    }

    #[tokio::test]
    async fn test_materialize_dep_error() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async {
            // Construct a tree with a symlink and its target, materialize both at once
            let symlink_path = make_path("foo/bar_symlink");
            let target_path = make_path("foo/bar_target");
            let target_from_symlink = RelativePathBuf::from_path(Path::new("bar_target"))?;

            let (mut dm, mut channel) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();

            let target_value = ArtifactValue::file(digest_config.empty_file());
            let symlink_value = make_artifact_value_with_symlink_dep(
                &target_path,
                &target_from_symlink,
                digest_config,
            )?;
            // Declare and materialize symlink and target
            dm.declare(
                &target_path,
                target_value.clone(),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            dm.declare(
                &symlink_path,
                symlink_value.clone(),
                Box::new(ArtifactMaterializationMethod::Test),
            );
            dm.materialize_artifact(&symlink_path, EventDispatcher::null())
                .context("Expected a future")?
                .await
                .map_err(|err| anyhow::anyhow!("error materializing {:?}", err))?;
            assert_eq!(
                dm.io.take_log(),
                &[
                    (Op::Clean, target_path.clone()),
                    (Op::Clean, symlink_path.clone()),
                    (Op::Materialize, target_path.clone()),
                    (Op::Materialize, symlink_path.clone()),
                ]
            );

            // Process materialization_finished, change symlink stage to materialized
            while let Ok(cmd) = channel.low_priority.try_recv() {
                dm.process_one_low_priority_command(cmd);
            }

            // Change symlink target value and re-declare
            let content = b"not empty";
            let meta = FileMetadata {
                digest: TrackedFileDigest::from_content(content, digest_config.cas_digest_config()),
                is_executable: false,
            };
            let target_value = ArtifactValue::file(meta);
            dm.declare(
                &target_path,
                target_value,
                Box::new(ArtifactMaterializationMethod::Test),
            );
            assert_eq!(dm.io.take_log(), &[(Op::Clean, target_path.clone())]);

            // Request to materialize symlink, fail to materialize target
            dm.io.set_fail_on(vec![target_path.clone()]);
            let res = dm
                .materialize_artifact(&symlink_path, EventDispatcher::null())
                .context("Expected a future")?
                .await;
            assert_matches!(
            res,
            Err(SharedMaterializingError::Error(e)) if format!("{:#}", e).contains("Injected error")
        );
            assert_eq!(
                dm.io.take_log(),
                &[(Op::MaterializeError, target_path.clone())]
            );
            // Process materialization_finished, _only_ target is cleaned, not symlink
            while let Ok(cmd) = channel.low_priority.try_recv() {
                dm.process_one_low_priority_command(cmd);
            }
            assert_eq!(dm.io.take_log(), &[(Op::Clean, target_path.clone())]);

            // Request symlink again, target is materialized and symlink materialization succeeds
            dm.io.set_fail_on(vec![]);
            dm.materialize_artifact(&symlink_path, EventDispatcher::null())
                .context("Expected a future")?
                .await
                .map_err(|err| anyhow::anyhow!("error materializing 2 {:?}", err))?;
            assert_eq!(dm.io.take_log(), &[(Op::Materialize, target_path.clone()), ]);
            Ok(())
        }).await
    }

    #[tokio::test]
    async fn test_retry() -> anyhow::Result<()> {
        ignore_stack_overflow_checks_for_future(async {
            let (mut dm, mut channel) = make_processor(Default::default());
            let digest_config = dm.io.digest_config();

            let path = make_path("test");
            let value1 = ArtifactValue::file(digest_config.empty_file());

            // Declare a value.
            dm.declare(&path, value1, Box::new(ArtifactMaterializationMethod::Test));

            // Make materializations fail
            dm.io.set_fail(true);

            // Materializing it fails.
            let res = dm
                .materialize_artifact(&path, EventDispatcher::null())
                .context("Expected a future")?
                .await;

            assert_matches!(
                res,
                Err(SharedMaterializingError::Error(e)) if format!("{:#}", e).contains("Injected error")
            );

            // Unset fail, but we haven't processed materialization_finished yet so this does nothing.
            dm.io.set_fail(false);

            // Rejoining the existing future fails.
            let res = dm
                .materialize_artifact(&path, EventDispatcher::null())
                .context("Expected a future")?
                .await;

            assert_matches!(
                res,
                Err(SharedMaterializingError::Error(e)) if format!("{:#}", e).contains("Injected error")
            );

            // Now process cleanup_finished_vacant and materialization_finished.
            let mut processed = 0;

            while let Ok(cmd) = channel.low_priority.try_recv() {
                eprintln!("got cmd = {:?}", cmd);
                dm.process_one_low_priority_command(cmd);
                processed += 1;
            }

            assert_eq!(processed, 2);

            // Materializing works now:
            let res = dm
                .materialize_artifact(&path, EventDispatcher::null())
                .context("Expected a future")?
                .await;

            assert_matches!(res, Ok(()));

            Ok(())
        }).await
    }
}
