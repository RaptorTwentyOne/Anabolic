// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! On Linux the page cache is dropped through /proc/sys/vm/drop_caches
//! (after a sync, so nothing written is lost), as root. Linux keeps no
//! working sets to empty and no modified list apart from what sync writes,
//! and a dump of the running kernel needs kdump, which Anabolic does not
//! drive.

use std::path::Path;

use crate::memtools::{KernelDump, Purge};

pub fn purge(what: Purge) -> Result<(), String> {
    match what {
        Purge::Standby | Purge::StandbyLowPriority | Purge::FileCache => {
            unsafe { libc::sync() };
            // 1: the page cache; dentries and inodes are left.
            std::fs::write("/proc/sys/vm/drop_caches", "1")
                .map_err(|e| format!("/proc/sys/vm/drop_caches: {e} (it needs root)"))
        }
        Purge::Modified => {
            unsafe { libc::sync() };
            Ok(())
        }
        Purge::WorkingSets => Err("Linux keeps no working sets to empty.".into()),
    }
}

pub fn kernel_dump(_dir: &Path, _user_pages: bool) -> Result<KernelDump, String> {
    Err("A dump of the running kernel needs kdump on Linux; Anabolic does not write one.".into())
}
