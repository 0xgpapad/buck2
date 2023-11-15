/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

#![cfg(windows)]

use winapi::um::handleapi::CloseHandle;
use winapi::um::winnt::HANDLE;

/// Close handle on drop.
pub struct WinapiHandle {
    handle: HANDLE,
}

unsafe impl Send for WinapiHandle {}
unsafe impl Sync for WinapiHandle {}

impl WinapiHandle {
    /// Unsafe because it closes the handle on drop.
    pub unsafe fn new(handle: HANDLE) -> WinapiHandle {
        WinapiHandle { handle }
    }

    pub fn handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for WinapiHandle {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                let res = CloseHandle(self.handle);
                assert!(res != 0, "CloseHandle failed");
            }
        };
    }
}
