//! System-wide memory figures.

use core::mem::size_of;

use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use crate::model::{MemoryLists, MemoryStats};
use crate::ntapi::{self, SYSTEM_MEMORY_LIST_INFORMATION, SYSTEM_PERFORMANCE_INFORMATION_PREFIX};

const PAGE: u64 = 4096;

pub struct MemorySampler {
    /// The full SYSTEM_PERFORMANCE_INFORMATION is larger than the prefix we
    /// read and its size changes between builds; give the kernel plenty.
    perf_buf: Box<[u64; 256]>,
    lists_supported: bool,
}

impl Default for MemorySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySampler {
    pub fn new() -> Self {
        Self {
            perf_buf: Box::new([0; 256]),
            lists_supported: true,
        }
    }

    pub fn sample(&mut self) -> MemoryStats {
        let mut stats = MemoryStats::default();

        let mut status = MEMORYSTATUSEX {
            dwLength: size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_ok() {
            stats.total = status.ullTotalPhys;
            stats.available = status.ullAvailPhys;
            stats.commit_limit = status.ullTotalPageFile;
            stats.commit = status
                .ullTotalPageFile
                .saturating_sub(status.ullAvailPageFile);
        }

        let mut returned = 0;
        let ok = ntapi::nt_success(unsafe {
            ntapi::NtQuerySystemInformation(
                ntapi::SystemPerformanceInformation,
                self.perf_buf.as_mut_ptr().cast(),
                (self.perf_buf.len() * 8) as u32,
                &mut returned,
            )
        });
        if ok && returned as usize >= size_of::<SYSTEM_PERFORMANCE_INFORMATION_PREFIX>() {
            // SAFETY: the buffer is 8-aligned and larger than the prefix.
            let perf = unsafe {
                &*self
                    .perf_buf
                    .as_ptr()
                    .cast::<SYSTEM_PERFORMANCE_INFORMATION_PREFIX>()
            };
            stats.paged_pool = perf.PagedPoolPages as u64 * PAGE;
            stats.nonpaged_pool = perf.NonPagedPoolPages as u64 * PAGE;
            stats.system_cache = perf.ResidentSystemCachePage as u64 * PAGE;
            stats.commit_peak = perf.PeakCommitment as u64 * PAGE;
        }

        if self.lists_supported {
            let mut lists = SYSTEM_MEMORY_LIST_INFORMATION::default();
            let status = unsafe {
                ntapi::NtQuerySystemInformation(
                    ntapi::SystemMemoryListInformation,
                    (&raw mut lists).cast(),
                    size_of::<SYSTEM_MEMORY_LIST_INFORMATION>() as u32,
                    &mut returned,
                )
            };
            if ntapi::nt_success(status) {
                stats.lists = Some(MemoryLists {
                    zeroed: lists.ZeroPageCount as u64 * PAGE,
                    free: lists.FreePageCount as u64 * PAGE,
                    modified: lists.ModifiedPageCount as u64 * PAGE,
                    standby: lists.PageCountByPriority.iter().sum::<usize>() as u64 * PAGE,
                });
            } else {
                // Needs SeProfileSingleProcessPrivilege; do not keep asking.
                self.lists_supported = false;
            }
        }

        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_prefix_layout_matches_win32() {
        let mut sampler = MemorySampler::new();
        let stats = sampler.sample();
        assert!(stats.total > 0 && stats.available > 0 && stats.available < stats.total);
        assert!(stats.commit > 0 && stats.commit <= stats.commit_limit);
        // Pool sizes come from fixed offsets; sanity check they are plausible.
        assert!(stats.paged_pool > 16 * 1024 * 1024 && stats.paged_pool < stats.total);
        assert!(stats.nonpaged_pool > 1024 * 1024 && stats.nonpaged_pool < stats.total);
        assert!(stats.commit_peak >= stats.commit / 2);
    }
}
