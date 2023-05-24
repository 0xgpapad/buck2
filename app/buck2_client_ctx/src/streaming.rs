/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::env;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use async_trait::async_trait;
use buck2_core::env_helper::EnvHelper;
use dupe::Dupe;
use futures::future;
use futures::future::Either;
use rand::Rng;

use crate::client_ctx::ClientCommandContext;
use crate::common::CommonBuildConfigurationOptions;
use crate::common::CommonConsoleOptions;
use crate::common::CommonDaemonCommandOptions;
use crate::daemon::client::connect::BuckdConnectConstraints;
use crate::daemon::client::connect::BuckdConnectOptions;
use crate::daemon::client::connect::DaemonConstraintsRequest;
use crate::daemon::client::connect::DesiredTraceIoState;
use crate::daemon::client::BuckdClientConnector;
use crate::exit_result::gen_error_exit_code;
use crate::exit_result::ExitResult;
use crate::exit_result::FailureExitCode;
use crate::subscribers::get::get_console_with_root;
use crate::subscribers::get::try_get_build_id_writer;
use crate::subscribers::get::try_get_event_log_subscriber;
use crate::subscribers::get::try_get_re_log_subscriber;
use crate::subscribers::recorder::try_get_invocation_recorder;
use crate::subscribers::subscriber::EventSubscriber;

static USE_STREAMING_UPLOADS: EnvHelper<bool> = EnvHelper::new("BUCK2_USE_STREAMING_UPLOADS");

fn default_subscribers<T: StreamingCommand>(
    cmd: &T,
    ctx: &ClientCommandContext,
) -> anyhow::Result<Vec<Box<dyn EventSubscriber>>> {
    let console_opts = cmd.console_opts();
    let mut subscribers = vec![];
    let show_waiting_message = cmd.should_show_waiting_message();

    // Need this to get information from one subscriber (event_log)
    // and log it in another (invocation_recorder)
    let log_size_counter_bytes = Some(Arc::new(AtomicU64::new(0)));

    if let Some(v) = get_console_with_root(
        ctx.trace_id.dupe(),
        console_opts.console_type,
        ctx.verbosity,
        show_waiting_message,
        None,
        T::COMMAND_NAME,
        console_opts.superconsole_config(),
        ctx.paths()?.isolation.clone(),
    )? {
        subscribers.push(v)
    }
    let use_streaming_upload = streaming_uploads()?;
    if let Some(event_log) = try_get_event_log_subscriber(
        cmd.event_log_opts(),
        cmd.sanitize_argv(env::args().collect()),
        ctx,
        log_size_counter_bytes.clone(),
        use_streaming_upload,
    )? {
        subscribers.push(event_log)
    }
    if let Some(re_log) = try_get_re_log_subscriber(ctx)? {
        subscribers.push(re_log)
    }
    if let Some(build_id_writer) = try_get_build_id_writer(cmd.event_log_opts(), ctx)? {
        subscribers.push(build_id_writer)
    }
    if let Some(recorder) = try_get_invocation_recorder(
        ctx,
        cmd.event_log_opts(),
        T::COMMAND_NAME,
        cmd.sanitize_argv(env::args().collect()),
        log_size_counter_bytes,
        use_streaming_upload,
    )? {
        subscribers.push(recorder);
    }
    subscribers.extend(cmd.extra_subscribers());
    Ok(subscribers)
}

fn streaming_uploads() -> anyhow::Result<bool> {
    if cfg!(windows) {
        // TODO T149151673: support windows streaming upload
        return Ok(false);
    };
    let mut rng = rand::thread_rng();
    let random_number = rng.gen_range(0..100);
    Ok(USE_STREAMING_UPLOADS
        .get_copied()?
        .unwrap_or(random_number < 50))
}
/// Trait to generalize the behavior of executable buck2 commands that rely on a server.
/// This trait is most helpful when the command wants a superconsole, to stream events, etc.
/// However, this is the most robustly tested of our code paths, and there is little cost to defaulting to it.
/// As a result, prefer to default to streaming mode unless there is a compelling reason not to
/// (e.g `status`)
#[async_trait]
pub trait StreamingCommand: Sized + Send + Sync {
    /// Give the command a name for printing, debugging, etc.
    const COMMAND_NAME: &'static str;

    /// Run the command.
    async fn exec_impl(
        self,
        buckd: &mut BuckdClientConnector,
        matches: &clap::ArgMatches,
        ctx: &mut ClientCommandContext<'_>,
    ) -> ExitResult;

    /// Should we only connect to existing servers (`true`), or spawn a new server if required (`false`).
    /// Defaults to `false`.
    fn existing_only() -> bool {
        false
    }

    fn trace_io(&self) -> DesiredTraceIoState {
        DesiredTraceIoState::Existing
    }

    fn console_opts(&self) -> &CommonConsoleOptions;

    fn event_log_opts(&self) -> &CommonDaemonCommandOptions;

    fn common_opts(&self) -> &CommonBuildConfigurationOptions;

    fn extra_subscribers(&self) -> Vec<Box<dyn EventSubscriber>> {
        vec![]
    }

    fn sanitize_argv(&self, argv: Vec<String>) -> Vec<String> {
        argv
    }

    /// Whether to show a "Waiting for daemon..." message in simple console during long waiting periods.
    fn should_show_waiting_message(&self) -> bool {
        true
    }
}

/// Just provides a common interface for buck subcommands for us to interact with here.
pub trait BuckSubcommand {
    fn exec(self, matches: &clap::ArgMatches, ctx: ClientCommandContext<'_>) -> ExitResult;
}

impl<T: StreamingCommand> BuckSubcommand for T {
    /// Actual call that runs a `StreamingCommand`.
    /// Handles all of the business of setting up a runtime, server, and subscribers.
    fn exec<'a>(self, matches: &clap::ArgMatches, ctx: ClientCommandContext<'a>) -> ExitResult {
        ctx.with_runtime(async move |mut ctx| {
            let work = async {
                let constraints = if T::existing_only() {
                    BuckdConnectConstraints::ExistingOnly
                } else {
                    let mut req =
                        DaemonConstraintsRequest::new(ctx.immediate_config, T::trace_io(&self))?;
                    ctx.restarter.apply_to_constraints(&mut req);
                    BuckdConnectConstraints::Constraints(req)
                };

                let mut connect_options = BuckdConnectOptions {
                    subscribers: default_subscribers(&self, &ctx)?,
                    constraints,
                };

                let mut buckd = match ctx.start_in_process_daemon.take() {
                    None => ctx.connect_buckd(connect_options).await?,
                    Some(start_in_process_daemon) => {
                        // Start in-process daemon, wait until it is ready to accept connections.
                        start_in_process_daemon()?;

                        // Do not attempt to spawn a daemon if connect failed.
                        // Connect should not fail.
                        connect_options.constraints = BuckdConnectConstraints::ExistingOnly;

                        ctx.connect_buckd(connect_options).await?
                    }
                };

                let command_result = self.exec_impl(&mut buckd, matches, &mut ctx).await;
                let command_result = command_result
                    .categorized_or_else(|| gen_error_exit_code(buckd.collect_error_cause()));

                ctx.restarter.observe(&buckd);

                command_result
            };

            // Race our work with a ctrl+c future. If we hit ctrl+c, then we'll drop the work
            // future. with_runtime sets up an AsyncCleanupContext that will allow drop
            // implementations within this future to clean up before we return from with_runtime.
            let exit = tokio::signal::ctrl_c();

            futures::pin_mut!(work);
            futures::pin_mut!(exit);

            match future::select(work, exit).await {
                Either::Left((res, _)) => res,
                Either::Right((_signal, _)) => ExitResult::from(FailureExitCode::SignalInterrupt),
            }
        })
    }
}
