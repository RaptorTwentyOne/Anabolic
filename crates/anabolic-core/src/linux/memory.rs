//! System-wide memory figures from /proc/meminfo, in Windows' terms where
//! there is a match: what is committed, what the cache holds, and the
//! lists free, modified (dirty) and standby (cached) memory sit on.

use crate::model::{MemoryLists, MemoryStats};

pub struct MemorySampler {
    /// The most committed memory seen since Anabolic started: Linux keeps
    /// no peak of its own.
    peak: u64,
}

impl Default for MemorySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySampler {
    pub fn new() -> Self {
        Self { peak: 0 }
    }

    pub fn sample(&mut self) -> MemoryStats {
        let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
            return MemoryStats::default();
        };
        let kb = |key: &str| crate::procfs::status_kb(&text, key).unwrap_or(0);
        let total = kb("MemTotal");
        let free = kb("MemFree");
        let cached = kb("Cached") + kb("Buffers");
        let dirty = kb("Dirty") + kb("Writeback");
        let commit = kb("Committed_AS");
        self.peak = self.peak.max(commit);
        MemoryStats {
            total,
            available: kb("MemAvailable"),
            commit,
            // Enforced only with overcommit off; otherwise programs may
            // reserve more than there is, and nothing fails until pages
            // are actually used (then the OOM killer steps in).
            commit_limit: if strict_overcommit() {
                kb("CommitLimit")
            } else {
                0
            },
            commit_peak: self.peak,
            // The kernel's own memory: what it can give back when asked,
            // and what it cannot.
            paged_pool: kb("SReclaimable"),
            nonpaged_pool: kb("SUnreclaim") + kb("KernelStack") + kb("PageTables"),
            system_cache: cached,
            lists: Some(MemoryLists {
                zeroed: 0,
                free,
                modified: dirty,
                standby: (cached + kb("SReclaimable")).saturating_sub(dirty),
            }),
        }
    }
}

/// vm.overcommit_memory = 2: reservations beyond CommitLimit fail.
fn strict_overcommit() -> bool {
    std::fs::read_to_string("/proc/sys/vm/overcommit_memory").is_ok_and(|v| v.trim() == "2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plausible_figures() {
        let stats = MemorySampler::new().sample();
        assert!(stats.total > 0 && stats.available > 0 && stats.available < stats.total);
        assert!(stats.commit > 0);
        assert!(stats.commit_peak >= stats.commit);
        let lists = stats.lists.unwrap();
        assert!(lists.free > 0 && lists.free < stats.total);
    }
}
