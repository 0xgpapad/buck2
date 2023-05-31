/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::any::Any;
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;

use allocative::Allocative;
use dashmap::mapref::entry::Entry;
use derivative::Derivative;
use dupe::Dupe;
use futures::future::BoxFuture;
use futures::future::Either;
use futures::FutureExt;
use more_futures::cancellation::CancellationContext;
use more_futures::spawn::spawn_cancellable;
use more_futures::spawn::DropCancelAndTerminationObserver;
use more_futures::spawn::StrongJoinHandle;
use more_futures::spawn::WeakFutureError;
use parking_lot::Mutex;
use parking_lot::MutexGuard;

use crate::api::activation_tracker::ActivationData;
use crate::api::computations::DiceComputations;
use crate::api::data::DiceData;
use crate::api::error::DiceResult;
use crate::api::key::Key;
use crate::api::projection::ProjectionKey;
use crate::api::user_data::UserComputationData;
use crate::ctx::DiceComputationsImpl;
use crate::impls::cache::SharedCache;
use crate::impls::core::state::CoreStateHandle;
use crate::impls::core::versions::VersionEpoch;
use crate::impls::dep_trackers::RecordingDepsTracker;
use crate::impls::dice::DiceModern;
use crate::impls::evaluator::AsyncEvaluator;
use crate::impls::evaluator::SyncEvaluator;
use crate::impls::events::DiceEventDispatcher;
use crate::impls::incremental::IncrementalEngine;
use crate::impls::key::CowDiceKeyHashed;
use crate::impls::key::DiceKey;
use crate::impls::key::ParentKey;
use crate::impls::opaque::OpaqueValueModern;
use crate::impls::task::dice::MaybeCancelled;
use crate::impls::task::sync_dice_task;
use crate::impls::task::PreviouslyCancelledTask;
use crate::impls::transaction::ActiveTransactionGuard;
use crate::impls::transaction::TransactionUpdater;
use crate::impls::user_cycle::UserCycleDetectorData;
use crate::impls::value::DiceComputedValue;
use crate::impls::value::DiceValidity;
use crate::impls::value::MaybeValidDiceValue;
use crate::result::CancellableResult;
use crate::result::Cancelled;
use crate::versions::VersionNumber;
use crate::DiceError;
use crate::DiceTransactionUpdater;
use crate::HashSet;
use crate::UserCycleDetectorGuard;

/// Context that is the base for which all requests start from
#[derive(Allocative, Dupe, Clone)]
pub(crate) struct BaseComputeCtx {
    // we need to give off references of `DiceComputation` so hold this for now, but really once we
    // get rid of the enum, we just hold onto the base data directly and do some ref casts
    data: DiceComputations,
    live_version_guard: ActiveTransactionGuard,
}

impl BaseComputeCtx {
    pub(crate) fn new(
        per_live_version_ctx: SharedLiveTransactionCtx,
        user_data: Arc<UserComputationData>,
        dice: Arc<DiceModern>,
        cycles: UserCycleDetectorData,
        live_version_guard: ActiveTransactionGuard,
    ) -> Self {
        Self {
            data: DiceComputations(DiceComputationsImpl::Modern(PerComputeCtx::new(
                ParentKey::None,
                per_live_version_ctx,
                user_data,
                dice,
                cycles,
            ))),
            live_version_guard,
        }
    }

    pub(crate) fn get_version(&self) -> VersionNumber {
        self.data.0.get_version()
    }

    pub(crate) fn into_updater(self) -> DiceTransactionUpdater {
        self.data.0.into_updater()
    }

    pub(crate) fn as_computations(&self) -> &DiceComputations {
        &self.data
    }
}

impl Deref for BaseComputeCtx {
    type Target = PerComputeCtx;

    fn deref(&self) -> &Self::Target {
        match &self.data.0 {
            DiceComputationsImpl::Legacy(_) => {
                unreachable!("legacy dice instead of modern")
            }
            DiceComputationsImpl::Modern(ctx) => ctx,
        }
    }
}

/// Context given to the `compute` function of a `Key`.
#[derive(Allocative, Dupe, Clone)]
pub(crate) struct PerComputeCtx {
    data: Arc<PerComputeCtxData>,
}

#[derive(Allocative)]
pub(crate) struct PerComputeCtxData {
    async_evaluator: AsyncEvaluator,
    dep_trackers: Mutex<RecordingDepsTracker>, // If we make PerComputeCtx &mut, we can get rid of this mutex after some refactoring
    parent_key: ParentKey,
    #[allocative(skip)]
    cycles: UserCycleDetectorData,
    // Same as above, PerComputeCtx isn't actually geting shared.
    #[allocative(skip)]
    evaluation_data: Mutex<EvaluationData>,
}

#[allow(clippy::manual_async_fn, unused)]
impl PerComputeCtx {
    pub(crate) fn new(
        parent_key: ParentKey,
        per_live_version_ctx: SharedLiveTransactionCtx,
        user_data: Arc<UserComputationData>,
        dice: Arc<DiceModern>,
        cycles: UserCycleDetectorData,
    ) -> Self {
        Self {
            data: Arc::new(PerComputeCtxData {
                async_evaluator: AsyncEvaluator {
                    per_live_version_ctx,
                    user_data,
                    dice,
                },
                dep_trackers: Mutex::new(RecordingDepsTracker::new()),
                parent_key,
                cycles,
                evaluation_data: Mutex::new(EvaluationData::none()),
            }),
        }
    }

    /// Gets all the result of of the given computation key.
    /// recorded as dependencies of the current computation for which this
    /// context is for.
    pub(crate) fn compute<'a, K>(
        &'a self,
        key: &'a K,
    ) -> impl Future<Output = DiceResult<<K as Key>::Value>> + 'a
    where
        K: Key,
    {
        self.compute_opaque(key)
            .map(|r| r.map(|opaque| opaque.into_value()))
    }

    /// Compute "opaque" value where the value is only accessible via projections.
    /// Projections allow accessing derived results from the "opaque" value,
    /// where the dependency of reading a projection is the projection value rather
    /// than the entire opaque value.
    pub(crate) fn compute_opaque<'b, 'a: 'b, K>(
        &'a self,
        key: &'b K,
    ) -> impl Future<Output = DiceResult<OpaqueValueModern<K>>> + 'b
    where
        K: Key,
    {
        let dice_key = self
            .data
            .async_evaluator
            .dice
            .key_index
            .index(CowDiceKeyHashed::key_ref(key));

        self.data
            .async_evaluator
            .per_live_version_ctx
            .compute_opaque(
                dice_key,
                self.data.parent_key,
                &self.data.async_evaluator,
                self.data
                    .cycles
                    .subrequest(dice_key, &self.data.async_evaluator.dice.key_index),
            )
            .map(move |cancellable_result| {
                let cancellable = cancellable_result.map(move |dice_value| {
                    OpaqueValueModern::new(self, dice_key, dice_value.value().dupe())
                });

                cancellable.map_err(|e| DiceError::cancelled())
            })
    }

    /// Compute "projection" based on deriving value
    pub(crate) fn project<K>(
        &self,
        key: &K,
        base_key: DiceKey,
        base: MaybeValidDiceValue,
    ) -> DiceResult<K::Value>
    where
        K: ProjectionKey,
    {
        let dice_key = self
            .data
            .async_evaluator
            .dice
            .key_index
            .index(CowDiceKeyHashed::proj_ref(base_key, key));

        let r = self
            .data
            .async_evaluator
            .per_live_version_ctx
            .compute_projection(
                dice_key,
                self.data.parent_key,
                self.data.async_evaluator.dice.state_handle.dupe(),
                SyncEvaluator::new(
                    self.data.async_evaluator.user_data.dupe(),
                    self.data.async_evaluator.dice.dupe(),
                    base,
                ),
                DiceEventDispatcher::new(
                    self.data.async_evaluator.user_data.tracker.dupe(),
                    self.data.async_evaluator.dice.dupe(),
                ),
            );

        let r = match r {
            Ok(r) => r,
            Err(_cancelled) => return Err(DiceError::cancelled()),
        };

        self.data
            .dep_trackers
            .lock()
            .record(dice_key, r.value().validity());

        Ok(r.value()
            .downcast_maybe_transient::<K::Value>()
            .expect("Type mismatch when computing key")
            .dupe())
    }

    /// temporarily here while we figure out why dice isn't paralleling computations so that we can
    /// use this in tokio spawn. otherwise, this shouldn't be here so that we don't need to clone
    /// the Arc, which makes lifetimes weird.
    pub(crate) fn temporary_spawn<F, R>(
        &self,
        f: F,
    ) -> Either<
        StrongJoinHandle<BoxFuture<'static, Result<R, WeakFutureError>>>,
        DropCancelAndTerminationObserver<R>,
    >
    where
        F: for<'a> FnOnce(&'a DiceComputations, &'a CancellationContext) -> BoxFuture<'a, R>
            + Send
            + 'static,
        R: Send + 'static,
    {
        let duped = self.dupe();

        spawn_cancellable(
            |cancellations| {
                async move {
                    f(
                        &DiceComputations(DiceComputationsImpl::Modern(duped)),
                        cancellations,
                    )
                    .await
                }
                .boxed()
            },
            self.data.async_evaluator.user_data.spawner.as_ref(),
            &self.data.async_evaluator.user_data,
            debug_span!(parent: None, "spawned_task",),
        )
        .into_drop_cancel()
        .right_future()
    }

    /// Data that is static per the entire lifetime of Dice. These data are initialized at the
    /// time that Dice is initialized via the constructor.
    pub(crate) fn global_data(&self) -> &DiceData {
        &self.data.async_evaluator.dice.global_data
    }

    /// Data that is static for the lifetime of the current request context. This lifetime is
    /// the lifetime of the top-level `DiceComputation` used for all requests.
    /// The data is also specific to each request context, so multiple concurrent requests can
    /// each have their own individual data.
    pub(crate) fn per_transaction_data(&self) -> &UserComputationData {
        &self.data.async_evaluator.user_data
    }

    pub(crate) fn get_version(&self) -> VersionNumber {
        self.data.async_evaluator.per_live_version_ctx.get_version()
    }

    pub(crate) fn into_updater(self) -> TransactionUpdater {
        TransactionUpdater::new(
            self.data.async_evaluator.dice.dupe(),
            self.data.async_evaluator.user_data.dupe(),
        )
    }

    pub(super) fn dep_trackers(&self) -> MutexGuard<'_, RecordingDepsTracker> {
        self.data.dep_trackers.lock()
    }

    pub(crate) fn store_evaluation_data<T: Send + Sync + 'static>(
        &self,
        value: T,
    ) -> DiceResult<()> {
        let mut evaluation_data = self.data.evaluation_data.lock();
        if evaluation_data.0.is_some() {
            return Err(DiceError::duplicate_activation_data());
        }
        evaluation_data.0 = Some(Box::new(value) as _);
        Ok(())
    }

    pub(crate) fn finalize(self) -> ((HashSet<DiceKey>, DiceValidity), EvaluationData) {
        // TODO need to clean up these ctxs so we have less runtime errors from Arc references
        let data = Arc::try_unwrap(self.data)
            .map_err(|_| "Error: tried to finalize when there are more references")
            .unwrap();

        data.cycles.finished_computing_key(
            &data.async_evaluator.dice.key_index,
            data.async_evaluator.user_data.cycle_detector.as_deref(),
        );

        (
            data.dep_trackers.into_inner().collect_deps(),
            data.evaluation_data.into_inner(),
        )
    }

    pub(crate) fn cycle_guard<T: UserCycleDetectorGuard>(&self) -> DiceResult<Option<&T>> {
        self.data.cycles.cycle_guard()
    }
}

/// Context that is shared for all current live computations of the same version.
#[derive(Allocative, Derivative, Dupe, Clone)]
#[derivative(Debug)]
pub(crate) struct SharedLiveTransactionCtx {
    version: VersionNumber,
    version_epoch: VersionEpoch,
    #[derivative(Debug = "ignore")]
    cache: SharedCache,
}

#[allow(clippy::manual_async_fn, unused)]
impl SharedLiveTransactionCtx {
    pub(crate) fn new(v: VersionNumber, version_epoch: VersionEpoch, cache: SharedCache) -> Self {
        Self {
            version: v,
            version_epoch,
            cache,
        }
    }

    /// Compute "opaque" value where the value is only accessible via projections.
    /// Projections allow accessing derived results from the "opaque" value,
    /// where the dependency of reading a projection is the projection value rather
    /// than the entire opaque value.
    pub(crate) fn compute_opaque(
        &self,
        key: DiceKey,
        parent_key: ParentKey,
        eval: &AsyncEvaluator,
        cycles: UserCycleDetectorData,
    ) -> impl Future<Output = CancellableResult<DiceComputedValue>> {
        match self.cache.get(key) {
            Some(Entry::Occupied(mut occupied)) => {
                match occupied.get().depended_on_by(parent_key) {
                    MaybeCancelled::Ok(promise) => {
                        debug!(msg = "shared state is waiting on existing task", k = ?key, v = ?self.version, v_epoch = ?self.version_epoch);

                        promise
                    },
                    MaybeCancelled::Cancelled => {
                        debug!(msg = "shared state has a cancelled task, spawning new one", k = ?key, v = ?self.version, v_epoch = ?self.version_epoch);

                        let eval = eval.dupe();
                        let events = DiceEventDispatcher::new(
                            eval.user_data.tracker.dupe(),
                            eval.dice.dupe(),
                        );

                        take_mut::take(occupied.get_mut(), |previous| {
                            IncrementalEngine::spawn_for_key(
                                key,
                                self.version_epoch,
                                eval,
                                cycles,
                                events,
                                 Some(PreviouslyCancelledTask {
                                    previous,
                                }),
                            )
                        });

                        occupied
                            .get()
                            .depended_on_by(parent_key)
                            .not_cancelled()
                            .expect("just created")
                    }
                }
                .left_future()
            }
            Some(Entry::Vacant(vacant)) => {
                debug!(msg = "shared state is empty, spawning new task", k = ?key, v = ?self.version, v_epoch = ?self.version_epoch);

                let eval = eval.dupe();
                let events =
                    DiceEventDispatcher::new(eval.user_data.tracker.dupe(), eval.dice.dupe());

                let task = IncrementalEngine::spawn_for_key(
                    key,
                    self.version_epoch,
                    eval,
                    cycles,
                    events,
                    None,
                );

                let fut = task
                    .depended_on_by(parent_key)
                    .not_cancelled()
                    .expect("just created");

                vacant.insert(task);

                fut.left_future()
            }
            None => {
                let v = self.version;
                let v_epoch = self.version_epoch;
                async move {
                    debug!(msg = "computing shared state is cancelled", k = ?key, v = ?v, v_epoch = ?v_epoch);
                    tokio::task::yield_now().await;

                    Err(Cancelled)
                }
                    .right_future()
            },
        }
    }

    /// Compute "projection" based on deriving value
    pub(crate) fn compute_projection(
        &self,
        key: DiceKey,
        parent_key: ParentKey,
        state: CoreStateHandle,
        eval: SyncEvaluator,
        events: DiceEventDispatcher,
    ) -> CancellableResult<DiceComputedValue> {
        let promise = match self.cache.get(key) {
            Some(Entry::Occupied(mut occupied)) => {
                match occupied.get().depended_on_by(parent_key) {
                    MaybeCancelled::Ok(promise) => promise,
                    MaybeCancelled::Cancelled => {
                        let task = unsafe {
                            // SAFETY: task completed below by `IncrementalEngine::project_for_key`
                            sync_dice_task()
                        };

                        *occupied.get_mut() = task;

                        occupied
                            .get()
                            .depended_on_by(parent_key)
                            .not_cancelled()
                            .expect("just created")
                    }
                }
            }
            Some(Entry::Vacant(vacant)) => {
                let task = unsafe {
                    // SAFETY: task completed below by `IncrementalEngine::project_for_key`
                    sync_dice_task()
                };

                vacant
                    .insert(task)
                    .value()
                    .depended_on_by(parent_key)
                    .not_cancelled()
                    .expect("just created")
            }
            None => {
                // for projection keys, these are cheap and synchronous computes that should never
                // be cancelled
                let task = unsafe {
                    // SAFETY: task completed below by `IncrementalEngine::project_for_key`
                    sync_dice_task()
                };

                task.depended_on_by(parent_key)
                    .not_cancelled()
                    .expect("just created")
            }
        };

        IncrementalEngine::project_for_key(
            state,
            promise,
            key,
            self.version,
            self.version_epoch,
            eval,
            events,
        )
    }

    pub(crate) fn get_version(&self) -> VersionNumber {
        self.version
    }
}

/// Opaque data that the key may have provided during evalution via store_evaluation_data.
pub(crate) struct EvaluationData(Option<Box<dyn Any + Send + Sync + 'static>>);

impl EvaluationData {
    pub(crate) fn none() -> Self {
        Self(None)
    }

    pub(crate) fn into_activation_data(self) -> ActivationData {
        ActivationData::Evaluated(self.0)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use dashmap::mapref::entry::Entry;

    use crate::impls::core::versions::VersionEpoch;
    use crate::impls::ctx::SharedLiveTransactionCtx;
    use crate::impls::key::DiceKey;
    use crate::impls::key::ParentKey;
    use crate::impls::task::sync_dice_task;
    use crate::impls::value::DiceComputedValue;

    impl SharedLiveTransactionCtx {
        pub(crate) fn inject(&self, k: DiceKey, v: DiceComputedValue) {
            let task = unsafe {
                // SAFETY: completed immediately below
                sync_dice_task()
            };
            let _r = task
                .depended_on_by(ParentKey::None)
                .not_cancelled()
                .expect("just created")
                .get_or_complete(|| v);

            match self.cache.get(k).expect("cancelled") {
                Entry::Occupied(o) => {
                    o.replace_entry(task);
                }
                Entry::Vacant(v) => {
                    v.insert(task);
                }
            }
        }

        pub(crate) fn testing_get_epoch(&self) -> VersionEpoch {
            self.version_epoch
        }
    }
}
