/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! Cross-platform process killing.

use crate::pid::Pid;

pub fn process_exists(pid: Pid) -> anyhow::Result<bool> {
    os_specific::process_exists(pid)
}

/// Send `KILL` or call `TerminateProcess` on the given process.
///
/// Returns a KilledProcessHandle that can be used to observe the termination of the killed process.
pub fn kill(pid: Pid) -> anyhow::Result<Option<KilledProcessHandle>> {
    match os_specific::kill(pid)? {
        Some(handle) => Ok(Some(KilledProcessHandle { handle })),
        None => Ok(None),
    }
}

pub struct KilledProcessHandle {
    #[cfg(windows)]
    handle: os_specific::WindowsKilledProcessHandle,
    #[cfg(unix)]
    handle: os_specific::UnixKilledProcessHandle,
}

impl KilledProcessHandle {
    pub fn has_exited(&self) -> anyhow::Result<bool> {
        self.handle.has_exited()
    }

    pub fn status(&self) -> Option<String> {
        self.handle.status()
    }
}

/// Get the status of a given process according to sysinfo.
pub fn get_sysinfo_status(pid: Pid) -> Option<String> {
    use sysinfo::PidExt;
    use sysinfo::ProcessExt;
    use sysinfo::ProcessRefreshKind;
    use sysinfo::System;
    use sysinfo::SystemExt;

    let pid = sysinfo::Pid::from_u32(pid.to_u32());

    let mut system = System::new();
    system.refresh_process_specifics(pid, ProcessRefreshKind::new());

    let proc = system.process(pid)?;
    Some(proc.status().to_string())
}

#[cfg(unix)]
mod os_specific {
    use anyhow::Context;
    use nix::sys::signal::Signal;

    use crate::kill::get_sysinfo_status;
    use crate::pid::Pid;

    pub(crate) fn process_exists(pid: Pid) -> anyhow::Result<bool> {
        let pid = pid.to_nix()?;
        match nix::sys::signal::kill(pid, None) {
            Ok(_) => Ok(true),
            Err(nix::errno::Errno::ESRCH) => Ok(false),
            Err(e) => Err(e)
                .with_context(|| format!("unexpected error checking if process {} exists", pid)),
        }
    }

    pub(super) fn kill(pid: Pid) -> anyhow::Result<Option<UnixKilledProcessHandle>> {
        let pid_nix = pid.to_nix()?;

        match nix::sys::signal::kill(pid_nix, Signal::SIGKILL) {
            Ok(()) => Ok(Some(UnixKilledProcessHandle { pid })),
            Err(nix::errno::Errno::ESRCH) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("Failed to kill pid {}", pid)),
        }
    }

    pub(crate) struct UnixKilledProcessHandle {
        pid: Pid,
    }

    impl UnixKilledProcessHandle {
        pub(crate) fn has_exited(&self) -> anyhow::Result<bool> {
            Ok(!process_exists(self.pid)?)
        }

        pub(crate) fn status(&self) -> Option<String> {
            get_sysinfo_status(self.pid)
        }
    }
}

#[cfg(windows)]
pub mod os_specific {
    use crate::kill::get_sysinfo_status;
    use crate::pid::Pid;
    use crate::winapi_process::WinapiProcessHandle;

    pub(crate) fn process_exists(pid: Pid) -> anyhow::Result<bool> {
        Ok(WinapiProcessHandle::open_for_info(pid).is_some())
    }

    pub(super) fn kill(pid: Pid) -> anyhow::Result<Option<WindowsKilledProcessHandle>> {
        let handle = match WinapiProcessHandle::open_for_terminate(pid) {
            Some(proc_handle) => proc_handle,
            None => return Ok(None),
        };

        handle.terminate()?;

        Ok(Some(WindowsKilledProcessHandle { handle }))
    }

    /// Windows reuses PIDs more aggressively than UNIX, so there we add an extra guard in the form
    /// of the process creation time.
    pub(crate) struct WindowsKilledProcessHandle {
        handle: WinapiProcessHandle,
    }

    impl WindowsKilledProcessHandle {
        pub(crate) fn has_exited(&self) -> anyhow::Result<bool> {
            self.handle.has_exited()
        }

        pub(crate) fn status(&self) -> Option<String> {
            // Maybe there is a better way to get this via the handle, but for now this'll do.
            get_sysinfo_status(self.handle.pid())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use buck2_util::process::background_command;

    use crate::kill::kill;
    use crate::kill::process_exists;
    use crate::pid::Pid;

    #[test]
    fn test_process_exists_kill() {
        let mut command = if !cfg!(windows) {
            let mut command = background_command("sh");
            command.args(["-c", "sleep 10000"]);
            command
        } else {
            let mut command = background_command("powershell");
            command.args(["-c", "Start-Sleep -Seconds 10000"]);
            command
        };
        let mut child = command.spawn().unwrap();
        let pid = Pid::from_u32(child.id()).unwrap();
        for _ in 0..5 {
            assert!(process_exists(pid).unwrap());
        }

        let handle = kill(pid).unwrap().unwrap();

        child.wait().unwrap();
        // Drop child to ensure the Windows handle is closed.
        drop(child);

        if !cfg!(windows) {
            assert!(handle.has_exited().unwrap());
        } else {
            let start = Instant::now();
            loop {
                if handle.has_exited().unwrap() {
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "Timed out waiting for process to die"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
