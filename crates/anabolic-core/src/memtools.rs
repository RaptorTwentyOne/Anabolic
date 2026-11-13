// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What the system holds in memory, given back, in the manner of RAMMap:
//! the standby list (files read before, kept in case they are read again),
//! the modified list (written but not yet on disk), every process' working
//! set, and the system's file cache. And a dump of the running kernel for a
//! debugger, without stopping the system, as Task Manager makes one. All of
//! it needs an administrator.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg_attr(windows, path = "windows/memtools.rs")]
#[cfg_attr(target_os = "linux", path = "linux/memtools.rs")]
mod imp;
pub use imp::{kernel_dump, purge};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purge {
    /// The standby list: cached file pages no one uses now. They are read
    /// from disk again when wanted. On Linux, the page cache.
    Standby,
    /// The standby list's lowest priority pages only: what the system
    /// would give up first anyway.
    StandbyLowPriority,
    /// Written pages not yet on disk: written now.
    Modified,
    /// Every process' working set: their pages go to the standby and
    /// modified lists, and come back as each process touches them.
    WorkingSets,
    /// The system's own file cache working set.
    FileCache,
}

impl Purge {
    pub const ALL: [Purge; 5] = [
        Purge::StandbyLowPriority,
        Purge::Standby,
        Purge::Modified,
        Purge::WorkingSets,
        Purge::FileCache,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Purge::Standby => "Empty the standby list",
            Purge::StandbyLowPriority => "Empty the low-priority standby list",
            Purge::Modified => "Write the modified list",
            Purge::WorkingSets => "Empty working sets",
            Purge::FileCache => "Empty the file cache",
        }
    }

    pub fn explain(self) -> &'static str {
        match self {
            Purge::Standby => {
                "Frees the files kept in RAM in case they are read again. Programs read them \
                 from disk the next time: slower for a while. Some do it before a game for \
                 fewer hitches when memory is short; with enough RAM it changes nothing."
            }
            Purge::StandbyLowPriority => {
                "Frees only what Windows would give up first anyway: the gentlest of these."
            }
            Purge::Modified => "Writes what programs changed in memory to disk now.",
            Purge::WorkingSets => {
                "Takes every process' pages out of RAM; each brings back what it touches. \
                 Everything is slower for a while: rarely what you want."
            }
            Purge::FileCache => {
                "Takes the system's file cache out of RAM: files are read from disk again."
            }
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Purge::Standby => "standby",
            Purge::StandbyLowPriority => "standby_low_priority",
            Purge::Modified => "modified",
            Purge::WorkingSets => "working_sets",
            Purge::FileCache => "file_cache",
        }
    }

    pub fn from_key(key: &str) -> Option<Purge> {
        Purge::ALL.into_iter().find(|p| p.key() == key)
    }
}

#[derive(Clone, Debug)]
pub struct KernelDump {
    pub path: PathBuf,
    pub bytes: u64,
    pub took: Duration,
}

/// The name of a kernel dump written now: Kernel-2026-11-13-174512.dmp.
pub fn kernel_dump_name() -> String {
    let now = crate::sysinfo::local_time(crate::sysinfo::now_filetime()).unwrap_or_default();
    let stamp: String = now
        .chars()
        .map(|c| if c.is_ascii_digit() { c } else { '-' })
        .collect();
    let stamp = stamp.trim_matches('-');
    format!("Kernel-{stamp}.dmp")
}

/// Where a kernel dump goes: the folder, and the file in it.
pub fn kernel_dump_path(dir: &Path) -> PathBuf {
    dir.join(kernel_dump_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_things() {
        for p in Purge::ALL {
            assert_eq!(Purge::from_key(p.key()), Some(p));
            assert!(!p.explain().is_empty());
        }
        let name = kernel_dump_name();
        assert!(
            name.starts_with("Kernel-") && name.ends_with(".dmp"),
            "{name}"
        );
        assert!(!name.contains(':') && !name.contains(' '), "{name}");
    }
}
