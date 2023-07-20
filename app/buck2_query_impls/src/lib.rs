/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

#![feature(async_closure)]
#![feature(try_blocks)]
#![feature(provide_any)]

use std::sync::Once;

pub mod analysis;
pub mod aquery;
pub mod cquery;
mod description;
pub mod dice;
pub mod frontend;
pub mod uquery;

pub fn init_late_bindings() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        analysis::environment::init_classpath_for_targets();
        analysis::environment::init_query_functions();
        analysis::eval::init_eval_analysis_query();
        aquery::find_matching_action::init_find_matching_action();
        description::init_query_environment_description_by_type();
        frontend::init_query_frontend();
        cquery::bxl::init_new_bxl_cquery_functions();
        uquery::bxl::init_new_bxl_uquery_functions();
    });
}
