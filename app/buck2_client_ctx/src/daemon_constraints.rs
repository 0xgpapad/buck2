/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use buck2_common::legacy_configs::init::DaemonStartupConfig;
use buck2_core::buck2_env;
use buck2_core::ci::ci_identifiers;

use crate::version::BuckVersion;

pub fn gen_daemon_constraints(
    daemon_startup_config: &DaemonStartupConfig,
) -> anyhow::Result<buck2_cli_proto::DaemonConstraints> {
    Ok(buck2_cli_proto::DaemonConstraints {
        version: version(),
        user_version: user_version()?,
        daemon_id: buck2_events::daemon_id::DAEMON_UUID.to_string(),
        daemon_startup_config: Some(daemon_startup_config.serialize()?),
        extra: None,
    })
}

pub fn version() -> String {
    BuckVersion::get_unique_id().to_owned()
}

/// Used to make sure that daemons are restarted between CI jobs if they don't properly clean up
/// after themselves.
pub fn user_version() -> anyhow::Result<Option<String>> {
    // This shouldn't really be necessary, but we used to check it so we'll keep it for now.
    if let Some(id) = buck2_env!("SANDCASTLE_ID", applicability = internal)? {
        return Ok(Some(id.to_owned()));
    }
    // The `ci_identifiers` function reports better identifiers earlier, so taking the first one is
    // enough
    Ok(ci_identifiers()?.find_map(|x| x.1).map(|s| s.to_owned()))
}
