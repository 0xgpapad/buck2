/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use crate::env_helper::EnvHelper;

/// Are we running on sandcastle?
pub fn is_sandcastle() -> anyhow::Result<bool> {
    static SANDCASTLE: EnvHelper<String> = EnvHelper::new("SANDCASTLE");

    Ok(SANDCASTLE.get()?.is_some())
}

pub fn sandcastle_id() -> anyhow::Result<Option<&'static str>> {
    static SANDCASTLE_ID: EnvHelper<String> = EnvHelper::new("SANDCASTLE_ID");
    Ok(SANDCASTLE_ID.get()?.map(|s| s.as_str()))
}
