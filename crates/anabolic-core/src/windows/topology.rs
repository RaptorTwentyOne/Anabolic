//! Which logical processors share a physical core, a cache or a NUMA node.

use windows::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, RelationAll};
use windows::Win32::System::Threading::{GetActiveProcessorCount, GetActiveProcessorGroupCount};

use super::*;

fn read<T: Copy>(buf: &[u8], at: usize) -> Option<T> {
    if at + size_of::<T>() > buf.len() {
        return None;
    }
    // SAFETY: bounds checked; unaligned read of plain data.
    Some(unsafe { buf.as_ptr().add(at).cast::<T>().read_unaligned() })
}

/// Logical processor indices of `count` GROUP_AFFINITY entries at `at`.
fn masks(buf: &[u8], at: usize, count: u16, group_base: &[usize]) -> Vec<usize> {
    let mut cpus = Vec::new();
    for i in 0..count.max(1) as usize {
        let entry = at + i * 16;
        let (Some(mask), Some(group)) = (read::<usize>(buf, entry), read::<u16>(buf, entry + 8))
        else {
            break;
        };
        let base = group_base.get(group as usize).copied().unwrap_or(0);
        cpus.extend(
            (0..usize::BITS as usize)
                .filter(|b| mask & (1 << b) != 0)
                .map(|b| base + b),
        );
    }
    cpus
}

pub fn topology() -> Topology {
    let groups = unsafe { GetActiveProcessorGroupCount() }.max(1);
    let mut group_base = Vec::new();
    let mut total = 0usize;
    for g in 0..groups {
        group_base.push(total);
        total += unsafe { GetActiveProcessorCount(g) } as usize;
    }
    let mut topo = Topology {
        cpus: vec![LogicalCpu::default(); total],
        ..Default::default()
    };

    let mut len = 0u32;
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &mut len) };
    if len == 0 {
        return topo;
    }
    let mut storage = vec![0u64; (len as usize).div_ceil(8)];
    if unsafe {
        GetLogicalProcessorInformationEx(RelationAll, Some(storage.as_mut_ptr().cast()), &mut len)
    }
    .is_err()
    {
        return topo;
    }
    let buf = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), len as usize) };

    let mut at = 0;
    while let (Some(kind), Some(size)) = (read::<i32>(buf, at), read::<u32>(buf, at + 4)) {
        if size == 0 {
            break;
        }
        let body = at + 8;
        match kind {
            // Processor core: Flags, EfficiencyClass, 20 reserved, GroupCount, masks.
            0 => {
                let efficiency = read::<u8>(buf, body + 1).unwrap_or(0);
                let count = read::<u16>(buf, body + 22).unwrap_or(1);
                let cpus = masks(buf, body + 24, count, &group_base);
                let core = topo.cores;
                topo.cores += 1;
                for &cpu in &cpus {
                    if let Some(c) = topo.cpus.get_mut(cpu) {
                        c.core = core;
                        c.efficiency_class = efficiency;
                        c.siblings = cpus.iter().copied().filter(|&o| o != cpu).collect();
                    }
                }
            }
            // NUMA node: NodeNumber, 18 reserved, GroupCount, masks.
            1 => {
                let node = read::<u32>(buf, body).unwrap_or(0);
                let count = read::<u16>(buf, body + 22).unwrap_or(1);
                for cpu in masks(buf, body + 24, count, &group_base) {
                    if let Some(c) = topo.cpus.get_mut(cpu) {
                        c.numa = node;
                    }
                }
            }
            // Cache: Level, Associativity, LineSize, CacheSize, Type, 18 reserved, GroupCount, masks.
            2 => {
                let level = read::<u8>(buf, body).unwrap_or(0);
                let cache_size = read::<u32>(buf, body + 4).unwrap_or(0);
                let kind = read::<i32>(buf, body + 8).unwrap_or(0);
                let count = read::<u16>(buf, body + 30).unwrap_or(1);
                let cpus = masks(buf, body + 32, count, &group_base);
                // Unified (0) or data (2) caches only.
                if kind == 0 || kind == 2 {
                    let index = topo.caches.len();
                    for &cpu in &cpus {
                        if let Some(c) = topo.cpus.get_mut(cpu) {
                            match level {
                                1 => c.l1d = Some(index),
                                2 => c.l2 = Some(index),
                                3 => c.l3 = Some(index),
                                _ => {}
                            }
                        }
                    }
                    topo.caches.push(Cache {
                        level,
                        size: cache_size,
                        cpus,
                    });
                }
            }
            _ => {}
        }
        at += size as usize;
    }
    topo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cpu_has_a_core_and_caches() {
        let topo = topology();
        assert_eq!(topo.cpus.len(), crate::cpu::CpuSampler::new().core_count());
        assert!(topo.cores >= 1 && topo.cores <= topo.cpus.len());
        assert!(topo.cpus.iter().all(|c| c.l2.is_some()));
        for (i, c) in topo.cpus.iter().enumerate() {
            for &s in &c.siblings {
                assert_eq!(topo.cpus[s].core, c.core, "cpu {i} sibling {s}");
            }
        }
        println!(
            "{} cores, {} cpus, {} L3 domains",
            topo.cores,
            topo.cpus.len(),
            topo.cpus
                .iter()
                .filter_map(|c| c.l3)
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }
}
