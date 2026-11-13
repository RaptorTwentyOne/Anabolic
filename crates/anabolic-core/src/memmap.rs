// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A process' memory, region by region, in the manner of VMMap: what each
//! allocation of its address space is (a program or DLL, a mapped file,
//! a heap, a thread's stack, private data), how much of it is committed and
//! how much is in RAM now, and what may be done with it. Two readings a
//! while apart show what grew. This opens the process to ask (on Linux it
//! reads /proc/PID/smaps).

use std::time::Duration;

use crate::model::ProcKey;

#[cfg_attr(windows, path = "windows/memmap.rs")]
#[cfg_attr(target_os = "linux", path = "linux/memmap.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// A program or DLL (or, on Linux, a shared library or the program).
    Image,
    /// A file mapped into memory.
    MappedFile,
    /// Memory shared with other processes, not backed by a file.
    Shareable,
    /// A heap: what the program allocates piece by piece.
    Heap,
    /// A thread's stack.
    Stack,
    /// The rest of what the process allocated for itself.
    #[default]
    PrivateData,
    /// Reserved address space that cannot be used: the tail of an
    /// allocation smaller than the 64 KB granularity.
    Unusable,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Image,
        Kind::MappedFile,
        Kind::Shareable,
        Kind::Heap,
        Kind::Stack,
        Kind::PrivateData,
        Kind::Unusable,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Image => "Program and DLLs",
            Kind::MappedFile => "Mapped files",
            Kind::Shareable => "Shareable",
            Kind::Heap => "Heaps",
            Kind::Stack => "Stacks",
            Kind::PrivateData => "Private data",
            Kind::Unusable => "Unusable",
        }
    }
}

/// One allocation: address space reserved at once, in one or more blocks of
/// one state and protection each.
#[derive(Clone, Debug, Default)]
pub struct Allocation {
    pub base: u64,
    pub size: u64,
    pub kind: Kind,
    /// Committed: backed by RAM or the page file (or the file it maps).
    pub committed: u64,
    /// In RAM now, and of that the part only this process uses.
    pub working_set: u64,
    pub private_ws: u64,
    /// The protection most of it has: "Read/Write", "Execute/Read", …
    pub protection: String,
    /// What it is: a file's path, "Heap 2", "Stack of thread 1234".
    pub detail: String,
    pub blocks: u32,
}

/// One kind's totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub size: u64,
    pub committed: u64,
    pub working_set: u64,
    pub private_ws: u64,
    pub count: u32,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryMap {
    pub key: ProcKey,
    pub allocations: Vec<Allocation>,
    /// When it was read, as a FILETIME.
    pub at: i64,
    pub took: Duration,
}

impl MemoryMap {
    pub fn totals(&self, kind: Kind) -> Totals {
        let mut t = Totals::default();
        for a in self.allocations.iter().filter(|a| a.kind == kind) {
            t.size += a.size;
            t.committed += a.committed;
            t.working_set += a.working_set;
            t.private_ws += a.private_ws;
            t.count += 1;
        }
        t
    }

    pub fn total(&self) -> Totals {
        let mut t = Totals::default();
        for k in Kind::ALL {
            let x = self.totals(k);
            t.size += x.size;
            t.committed += x.committed;
            t.working_set += x.working_set;
            t.private_ws += x.private_ws;
            t.count += x.count;
        }
        t
    }
}

/// What grew between two readings of the same process: each kind's change
/// in committed memory, and the allocations that grew most (new ones
/// included), largest first.
#[derive(Clone, Debug, Default)]
pub struct Growth {
    pub by_kind: Vec<(Kind, i64)>,
    /// The allocation, and how much more is committed than before.
    pub grew: Vec<(Allocation, i64)>,
    pub seconds: f64,
}

pub fn growth(before: &MemoryMap, after: &MemoryMap) -> Growth {
    let by_kind = Kind::ALL
        .iter()
        .map(|&k| {
            (
                k,
                after.totals(k).committed as i64 - before.totals(k).committed as i64,
            )
        })
        .filter(|(_, d)| *d != 0)
        .collect();
    let mut old: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    for a in &before.allocations {
        old.insert(a.base, a.committed);
    }
    let mut grew: Vec<(Allocation, i64)> = after
        .allocations
        .iter()
        .map(|a| {
            let was = old.get(&a.base).copied().unwrap_or(0);
            (a.clone(), a.committed as i64 - was as i64)
        })
        .filter(|(_, d)| *d > 0)
        .collect();
    grew.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
    grew.truncate(50);
    Growth {
        by_kind,
        grew,
        seconds: (after.at - before.at) as f64 / 10_000_000.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc(base: u64, kind: Kind, committed: u64) -> Allocation {
        Allocation {
            base,
            size: committed * 2,
            kind,
            committed,
            ..Allocation::default()
        }
    }

    #[test]
    fn adds_up_and_finds_what_grew() {
        let before = MemoryMap {
            allocations: vec![
                alloc(0x1000, Kind::Heap, 100),
                alloc(0x9000, Kind::Image, 50),
            ],
            at: 0,
            ..MemoryMap::default()
        };
        let after = MemoryMap {
            allocations: vec![
                alloc(0x1000, Kind::Heap, 300),
                alloc(0x5000, Kind::PrivateData, 40),
                alloc(0x9000, Kind::Image, 50),
            ],
            at: 600_000_000,
            ..MemoryMap::default()
        };
        assert_eq!(after.totals(Kind::Heap).committed, 300);
        assert_eq!(after.total().committed, 390);
        assert_eq!(after.total().count, 3);
        let g = growth(&before, &after);
        assert_eq!(g.seconds, 60.0);
        assert_eq!(g.by_kind, vec![(Kind::Heap, 200), (Kind::PrivateData, 40)]);
        assert_eq!(g.grew.len(), 2);
        assert_eq!(g.grew[0].0.base, 0x1000);
        assert_eq!(g.grew[0].1, 200);
    }
}
