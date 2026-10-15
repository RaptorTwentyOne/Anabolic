//! GPU engine utilisation and video memory from the kernel graphics
//! statistics interface (D3DKMT), the same source Task Manager reads.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTERINFO, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2, D3DKMT_NODEMETADATA,
    D3DKMT_QUERYADAPTERINFO, D3DKMT_QUERYSTATISTICS, D3DKMT_QUERYSTATISTICS_ADAPTER,
    D3DKMT_QUERYSTATISTICS_NODE, D3DKMT_QUERYSTATISTICS_PROCESS_NODE,
    D3DKMT_QUERYSTATISTICS_SEGMENT, D3DKMT_QUERYSTATISTICS_TYPE, D3DKMTCloseAdapter,
    D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo, D3DKMTQueryStatistics, KMTQAITYPE_NODEMETADATA,
};
use windows::Win32::Foundation::{HANDLE, LUID};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::System::ProcessStatus::K32EnumProcesses;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, THREAD_PRIORITY_BELOW_NORMAL,
};

use crate::model::ProcKey;
use crate::util::{OwnedHandle, filetime_to_i64, from_wide};

#[derive(Clone, Debug)]
pub struct EngineStats {
    pub name: Arc<str>,
    /// Busy share of the interval, 0..=100.
    pub usage: f32,
}

#[derive(Clone, Debug, Default)]
pub struct AdapterGpuStats {
    pub name: Arc<str>,
    /// The busiest engine, which is what Task Manager reports as "GPU".
    pub usage: f32,
    pub engines: Vec<EngineStats>,
    pub dedicated_used: u64,
    pub dedicated_total: u64,
    pub shared_used: u64,
    pub shared_total: u64,
}

fn query(
    kind: D3DKMT_QUERYSTATISTICS_TYPE,
    luid: LUID,
    process: HANDLE,
    id: u32,
) -> Option<D3DKMT_QUERYSTATISTICS> {
    // SAFETY: plain data; every union variant is valid when zeroed.
    let mut q: D3DKMT_QUERYSTATISTICS = unsafe { std::mem::zeroed() };
    q.Type = kind;
    q.AdapterLuid = luid;
    q.hProcess = process;
    // QueryNode, QuerySegment and QueryProcessNode all start with their id.
    q.Anonymous.QueryNode.NodeId = id;
    // The call fills in QueryResult despite the const pointer in its signature.
    unsafe { D3DKMTQueryStatistics(&q) }.is_ok().then_some(q)
}

struct Node {
    name: Arc<str>,
    prev: i64,
}

struct Adapter {
    luid: LUID,
    name: Arc<str>,
    nodes: Vec<Node>,
    segments: u32,
    dedicated_total: u64,
    shared_total: u64,
}

/// Engine names on this adapter, deduplicated ("Copy", "Copy 2").
fn node_names(handle: u32, count: u32) -> Vec<Arc<str>> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    (0..count)
        .map(|node| {
            // SAFETY: plain data.
            let mut meta: D3DKMT_NODEMETADATA = unsafe { std::mem::zeroed() };
            meta.NodeOrdinalAndAdapterIndex = node;
            let mut info = D3DKMT_QUERYADAPTERINFO {
                hAdapter: handle,
                Type: KMTQAITYPE_NODEMETADATA,
                pPrivateDriverData: (&raw mut meta).cast(),
                PrivateDriverDataSize: size_of::<D3DKMT_NODEMETADATA>() as u32,
            };
            let mut name = String::new();
            if unsafe { D3DKMTQueryAdapterInfo(&mut info) }.is_ok() {
                // Copy out of the packed struct before borrowing.
                let (friendly, engine) = (meta.NodeData.FriendlyName, meta.NodeData.EngineType);
                name = from_wide(&friendly);
                if name.is_empty() {
                    // Many drivers leave the name blank; the type is what Task Manager shows then.
                    name = match engine.0 {
                        1 => "3D",
                        2 => "Video Decode",
                        3 => "Video Encode",
                        4 => "Video Processing",
                        5 => "Scene Assembly",
                        6 => "Copy",
                        7 => "Overlay",
                        8 => "Crypto",
                        _ => "",
                    }
                    .to_string();
                }
            }
            if name.is_empty() {
                name = format!("Engine {node}");
            }
            let n = seen.entry(name.clone()).or_default();
            *n += 1;
            Arc::from(if *n > 1 { format!("{name} {n}") } else { name })
        })
        .collect()
}

pub struct GpuSampler {
    adapters: Vec<Adapter>,
    last: Option<Instant>,
}

impl GpuSampler {
    /// Hardware adapters that DXGI also reports (skips the basic render driver).
    pub fn new() -> Self {
        // LUID -> (name, dedicated, shared, physical identity)
        type Physical = (u32, u32, u32, u32, u64);
        let mut dxgi: HashMap<(u32, i32), (String, u64, u64, Physical)> = HashMap::new();
        if let Ok(factory) = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
            let mut i = 0;
            while let Ok(adapter) = unsafe { factory.EnumAdapters1(i) } {
                if let Ok(d) = unsafe { adapter.GetDesc1() }
                    && d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0
                {
                    let physical = (
                        d.VendorId,
                        d.DeviceId,
                        d.SubSysId,
                        d.Revision,
                        d.DedicatedVideoMemory as u64,
                    );
                    dxgi.insert(
                        (d.AdapterLuid.LowPart, d.AdapterLuid.HighPart),
                        (
                            from_wide(&d.Description),
                            d.DedicatedVideoMemory as u64,
                            d.SharedSystemMemory as u64,
                            physical,
                        ),
                    );
                }
                i += 1;
            }
        }

        let mut adapters = Vec::new();
        let mut list = D3DKMT_ENUMADAPTERS2 {
            NumAdapters: 0,
            pAdapters: std::ptr::null_mut(),
        };
        if unsafe { D3DKMTEnumAdapters2(&mut list) }.is_ok() && list.NumAdapters > 0 {
            let mut infos = vec![D3DKMT_ADAPTERINFO::default(); list.NumAdapters as usize];
            list.pAdapters = infos.as_mut_ptr();
            if unsafe { D3DKMTEnumAdapters2(&mut list) }.is_ok() {
                infos.truncate(list.NumAdapters as usize);
                let mut seen = std::collections::HashSet::new();
                for info in &infos {
                    let luid = info.AdapterLuid;
                    // One physical GPU can show up under several LUIDs (and
                    // a LUID once per display source); count it once.
                    if let Some((name, dedicated, shared, physical)) =
                        dxgi.get(&(luid.LowPart, luid.HighPart))
                        && seen.insert(*physical)
                        && let Some(q) =
                            query(D3DKMT_QUERYSTATISTICS_ADAPTER, luid, HANDLE::default(), 0)
                    {
                        let stats = unsafe { q.QueryResult.AdapterInformation };
                        adapters.push(Adapter {
                            luid,
                            name: Arc::from(name.as_str()),
                            nodes: node_names(info.hAdapter, stats.NodeCount)
                                .into_iter()
                                .map(|name| Node { name, prev: 0 })
                                .collect(),
                            segments: stats.NbSegments,
                            dedicated_total: *dedicated,
                            shared_total: *shared,
                        });
                    }
                }
            }
            for info in &infos {
                let close = D3DKMT_CLOSEADAPTER {
                    hAdapter: info.hAdapter,
                };
                unsafe {
                    let _ = D3DKMTCloseAdapter(&close);
                }
            }
        }
        Self {
            adapters,
            last: None,
        }
    }

    /// Engines per adapter, for sizing history buffers.
    pub fn layout(&self) -> Vec<usize> {
        self.adapters.iter().map(|a| a.nodes.len()).collect()
    }

    /// (adapter LUID, engine count), for the per-process sampler.
    fn targets(&self) -> Vec<(LUID, u32)> {
        self.adapters
            .iter()
            .map(|a| (a.luid, a.nodes.len() as u32))
            .collect()
    }

    pub fn sample(&mut self) -> Vec<AdapterGpuStats> {
        let now = Instant::now();
        let elapsed = self
            .last
            .replace(now)
            .map(|t| (now - t).as_nanos() as f64 / 100.0);
        self.adapters
            .iter_mut()
            .map(|a| {
                let mut out = AdapterGpuStats {
                    name: a.name.clone(),
                    dedicated_total: a.dedicated_total,
                    shared_total: a.shared_total,
                    ..Default::default()
                };
                for (id, node) in a.nodes.iter_mut().enumerate() {
                    let running = query(
                        D3DKMT_QUERYSTATISTICS_NODE,
                        a.luid,
                        HANDLE::default(),
                        id as u32,
                    )
                    .map(|q| unsafe { q.QueryResult.NodeInformation.GlobalInformation.RunningTime })
                    .unwrap_or(node.prev);
                    let usage = match elapsed {
                        Some(e) if e > 0.0 => ((running - node.prev).max(0) as f64 * 100.0 / e)
                            .clamp(0.0, 100.0) as f32,
                        _ => 0.0,
                    };
                    node.prev = running;
                    out.usage = out.usage.max(usage);
                    out.engines.push(EngineStats {
                        name: node.name.clone(),
                        usage,
                    });
                }
                for id in 0..a.segments {
                    if let Some(q) = query(
                        D3DKMT_QUERYSTATISTICS_SEGMENT,
                        a.luid,
                        HANDLE::default(),
                        id,
                    ) {
                        let segment = unsafe { q.QueryResult.SegmentInformation };
                        // Aperture segments live in system memory.
                        if segment.Aperture != 0 {
                            out.shared_used += segment.BytesResident;
                        } else {
                            out.dedicated_used += segment.BytesResident;
                        }
                    }
                }
                out
            })
            .collect()
    }
}

impl Default for GpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// GPU share per process (busiest engine), published by its own thread.
pub type GpuProcessMap = HashMap<ProcKey, f32>;

struct Tracked {
    key: ProcKey,
    handle: OwnedHandle,
    /// Running time per adapter and engine at the last sample.
    prev: Vec<Vec<i64>>,
}

/// Starts the per-process GPU sampler. It holds a query handle to every
/// process and drops it as soon as the process exits, so it never keeps
/// zombies alive itself.
pub(crate) fn spawn_process_sampler(
    sampler: &GpuSampler,
    out: Arc<ArcSwap<GpuProcessMap>>,
    interval_ms: Arc<AtomicU64>,
) {
    let targets = sampler.targets();
    if targets.is_empty() {
        return;
    }
    thread::Builder::new()
        .name("anabolic-gpu".into())
        .spawn(move || {
            crate::hardening::set_current_thread_priority(THREAD_PRIORITY_BELOW_NORMAL);
            let mut tracked: HashMap<u32, Tracked> = HashMap::new();
            let mut pids = vec![0u32; 4096];
            let mut last = Instant::now();
            loop {
                thread::sleep(Duration::from_millis(
                    interval_ms.load(Ordering::Relaxed).max(250),
                ));
                let now = Instant::now();
                let elapsed = (now - last).as_nanos() as f64 / 100.0;
                last = now;

                let mut bytes = 0u32;
                if !unsafe {
                    K32EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut bytes)
                }
                .as_bool()
                {
                    continue;
                }
                let live: Vec<u32> = pids[..bytes as usize / 4]
                    .iter()
                    .copied()
                    .filter(|&p| p > 4)
                    .collect();
                let alive: std::collections::HashSet<u32> = live.iter().copied().collect();
                // A handle kept to a process that is ending would itself keep
                // it from going away.
                let running = |h: &OwnedHandle| {
                    crate::ending::flags(h.raw())
                        .is_some_and(|f| !f.deleting && crate::ending::still_active(f.exit_status))
                };
                tracked.retain(|pid, t| alive.contains(pid) && running(&t.handle));
                for pid in live {
                    if tracked.contains_key(&pid) {
                        continue;
                    }
                    // The per-process statistics refuse limited-query handles.
                    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) })
                    else {
                        continue;
                    };
                    let handle = OwnedHandle(h);
                    if !running(&handle) {
                        continue;
                    }
                    let (mut created, mut exited, mut kernel, mut user) = Default::default();
                    if unsafe {
                        GetProcessTimes(
                            handle.raw(),
                            &mut created,
                            &mut exited,
                            &mut kernel,
                            &mut user,
                        )
                    }
                    .is_err()
                    {
                        continue;
                    }
                    let key = ProcKey {
                        pid,
                        create_time: filetime_to_i64(created),
                    };
                    let prev = targets.iter().map(|&(_, n)| vec![-1; n as usize]).collect();
                    tracked.insert(pid, Tracked { key, handle, prev });
                }

                let mut map = GpuProcessMap::new();
                for t in tracked.values_mut() {
                    let mut busiest = 0f32;
                    for (a, &(luid, nodes)) in targets.iter().enumerate() {
                        for node in 0..nodes {
                            let Some(q) = query(
                                D3DKMT_QUERYSTATISTICS_PROCESS_NODE,
                                luid,
                                t.handle.raw(),
                                node,
                            ) else {
                                continue;
                            };
                            let running =
                                unsafe { q.QueryResult.ProcessNodeInformation.RunningTime };
                            let prev = std::mem::replace(&mut t.prev[a][node as usize], running);
                            if prev >= 0 && elapsed > 0.0 {
                                let usage = ((running - prev).max(0) as f64 * 100.0 / elapsed)
                                    .clamp(0.0, 100.0)
                                    as f32;
                                busiest = busiest.max(usage);
                            }
                        }
                    }
                    if busiest > 0.0 {
                        map.insert(t.key, busiest);
                    }
                }
                out.store(Arc::new(map));
            }
        })
        .expect("failed to spawn the GPU thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_adapters() {
        let mut sampler = GpuSampler::new();
        let first = sampler.sample();
        std::thread::sleep(Duration::from_millis(100));
        let second = sampler.sample();
        assert_eq!(first.len(), second.len());
        for a in &second {
            assert!(!a.engines.is_empty(), "{} has no engines", a.name);
            assert!(a.engines.iter().all(|e| (0.0..=100.0).contains(&e.usage)));
            assert!(a.dedicated_used <= a.dedicated_total.max(a.dedicated_used));
            println!(
                "{}: {:.1}% {:?}",
                a.name,
                a.usage,
                a.engines.iter().map(|e| &*e.name).collect::<Vec<_>>()
            );
        }
    }
}
