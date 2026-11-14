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

use crate::model::{MemoryLists, MemoryStats};

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
                "Takes every process' pages out of RAM. What they changed goes to the page \
                 file, often many gigabytes written in seconds, and each process brings back \
                 what it touches, so everything is slower for a while. Rarely what you want."
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

/// What a purge did, in a few words, from the memory figures before and
/// after it; None where they cannot be read.
pub fn effect(what: Purge, before: &MemoryStats, after: &MemoryStats) -> Option<String> {
    if what == Purge::FileCache {
        return Some(format!(
            "{} of file cache let go",
            size(before.system_cache.saturating_sub(after.system_cache))
        ));
    }
    let (b, a) = (before.lists?, after.lists?);
    let free = |l: &MemoryLists| l.free + l.zeroed;
    Some(match what {
        Purge::Standby | Purge::StandbyLowPriority => format!(
            "{} freed, {} free now",
            size(free(&a).saturating_sub(free(&b))),
            size(free(&a))
        ),
        Purge::Modified => format!(
            "{} written to disk",
            size(b.modified.saturating_sub(a.modified))
        ),
        Purge::WorkingSets => format!(
            "{} taken out of programs' working sets",
            size((a.standby + a.modified).saturating_sub(b.standby + b.modified))
        ),
        Purge::FileCache => unreachable!(),
    })
}

fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
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

    #[test]
    fn says_what_a_purge_did() {
        const GB: u64 = 1 << 30;
        let at = |free, modified, standby| MemoryStats {
            lists: Some(MemoryLists {
                zeroed: 0,
                free,
                modified,
                standby,
            }),
            ..Default::default()
        };
        let (before, after) = (at(0, 3 * GB, 40 * GB), at(40 * GB, 3 * GB, 0));
        assert_eq!(
            effect(Purge::Standby, &before, &after).unwrap(),
            "40.0 GB freed, 40.0 GB free now"
        );
        let after = at(0, GB / 4, 40 * GB);
        assert_eq!(
            effect(Purge::Modified, &before, &after).unwrap(),
            "2.8 GB written to disk"
        );
        assert_eq!(
            effect(Purge::Standby, &before, &MemoryStats::default()),
            None
        );
    }
}
