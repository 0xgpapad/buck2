/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

#![feature(error_generic_member_access)]

//! Common core components of buck2

#![feature(fs_try_exists)]
#![feature(io_error_more)]
#![feature(is_sorted)]
#![feature(never_type)]

#[cfg(test)]
#[macro_use]
extern crate maplit;

pub mod buckd_connection;
pub mod cas_digest;
pub mod client_utils;
pub mod convert;
pub mod daemon_dir;
pub mod dice;
pub mod events;
pub mod external_symlink;
pub mod file_ops;
pub mod find_buildfile;
pub mod home_buck_tmp;
pub mod ignores;
pub mod invocation_paths;
pub mod invocation_roots;
pub mod io;
pub mod kill_util;
pub mod legacy_configs;
pub mod liveliness_observer;
pub mod local_resource_state;
pub mod memory;
pub mod package_boundary;
pub mod package_listing;
pub mod pattern;
pub mod sqlite;
pub mod target_aliases;
pub mod temp_path;
