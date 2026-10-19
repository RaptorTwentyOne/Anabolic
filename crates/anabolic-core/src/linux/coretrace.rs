//! Which threads ran on each logical processor, sampled by the kernel
//! (perf events, cap_perfmon): about a thousand times a second a busy
//! processor reports the thread it is running. Context switch tracing would
//! be exact but costs far more on a busy machine; the samples answer "who
//! is using this core" just as well, and only run while someone looks.
//!
//! The samples count processor cycles where the processor can (hybrid
//! Intel processors each through their own core type's counter): a
//! processor that sleeps runs no cycles, so idle ones are not woken to be
//! asked. Without hardware counters (a virtual machine), a timer samples
//! instead and idle samples are left out. Either way the counts say who
//! shares the busy time; the collector scales them by the measured use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::perf::{self, Attr, KernelThreads, Record, Sample, Trace};

use super::*;

/// Samples per second on a busy processor.
const RATE: u64 = 1000;
const PAGES: usize = 8;
const SAMPLE_TYPE: u64 = perf::SAMPLE_TID | perf::SAMPLE_CPU;

#[derive(Default)]
struct Shared {
    /// Per processor (by its place among those online): tid -> samples.
    samples: Vec<HashMap<u32, u32>>,
    /// tid -> the process it is charged to.
    owners: HashMap<u32, u32>,
    images: HashMap<u32, Arc<str>>,
}

pub struct CoreTracer {
    shared: Arc<Mutex<Shared>>,
    _trace: Trace,
}

/// A PMU's type number, for hybrid processors' core types.
fn pmu(name: &str) -> Option<(u64, Vec<u32>)> {
    let base = format!("/sys/bus/event_source/devices/{name}");
    let kind = std::fs::read_to_string(format!("{base}/type")).ok()?;
    let cpus = std::fs::read_to_string(format!("{base}/cpus")).ok()?;
    Some((
        kind.trim().parse().ok()?,
        crate::topology::parse_list(cpus.trim())
            .into_iter()
            .map(|c| c as u32)
            .collect(),
    ))
}

/// Cycles on `cpu`, through its own core type's counter on a hybrid
/// processor (the generic event would count on one type only).
fn cycles(cpu: u32, hybrid: &[(u64, Vec<u32>)]) -> Attr {
    let pmu = hybrid
        .iter()
        .find(|(_, cpus)| cpus.contains(&cpu))
        .map_or(0, |(kind, _)| *kind);
    let mut attr = Attr::new(perf::TYPE_HARDWARE, (pmu << 32) | perf::HW_CPU_CYCLES);
    attr.sample = RATE;
    attr.flags |= perf::FREQ;
    attr
}

/// A timer firing every millisecond, idle samples left out.
fn clock() -> Attr {
    let mut attr = Attr::new(perf::TYPE_SOFTWARE, perf::SW_CPU_CLOCK);
    attr.sample = 1_000_000_000 / RATE;
    attr.flags |= perf::EXCLUDE_IDLE;
    attr
}

impl CoreTracer {
    pub fn start(cpus: usize) -> Result<Self, String> {
        let online = perf::online_cpus();
        let hybrid: Vec<(u64, Vec<u32>)> = ["cpu_core", "cpu_atom"]
            .into_iter()
            .filter_map(pmu)
            .collect();
        let shared = Arc::new(Mutex::new(Shared {
            samples: vec![HashMap::new(); cpus.max(online.len())],
            ..Default::default()
        }));
        let start = |event: &dyn Fn(u32) -> Attr| {
            let sink = shared.clone();
            let online = online.clone();
            let mut kernel = KernelThreads::default();
            Trace::start(
                "cores",
                |cpu| {
                    let mut attr = event(cpu);
                    attr.sample_type = SAMPLE_TYPE;
                    // New processes' names, for those gone before a scan.
                    attr.flags |= perf::COMM | perf::COMM_EXEC;
                    perf::watermark(&mut attr, PAGES);
                    vec![attr]
                },
                PAGES,
                move |record| match record {
                    Record::Sample(body) => {
                        let Some(s) = Sample::parse(body, SAMPLE_TYPE) else {
                            return;
                        };
                        let Some(slot) = online.iter().position(|&c| c == s.cpu) else {
                            return;
                        };
                        // The idle task is tid 0 on every processor.
                        let owner = kernel.owner(s.pid);
                        if let Ok(mut shared) = sink.lock() {
                            if let Some(map) = shared.samples.get_mut(slot) {
                                *map.entry(s.tid).or_default() += 1;
                            }
                            if s.tid != 0 {
                                shared.owners.insert(s.tid, owner);
                            }
                        }
                    }
                    Record::Comm { pid, tid, name } if pid == tid => {
                        if let Ok(mut shared) = sink.lock() {
                            shared.images.insert(pid, Arc::from(name));
                        }
                    }
                    _ => {}
                },
            )
        };
        let trace = start(&|cpu| cycles(cpu, &hybrid)).or_else(|_| start(&|_| clock()))?;
        Ok(Self {
            shared,
            _trace: trace,
        })
    }

    pub fn take(&self) -> CoreSamples {
        let Ok(mut shared) = self.shared.lock() else {
            return CoreSamples::default();
        };
        CoreSamples {
            per_cpu: shared
                .samples
                .iter_mut()
                .map(|s| s.drain().collect())
                .collect(),
            owners: std::mem::take(&mut shared.owners),
            images: std::mem::take(&mut shared.images),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Needs cap_perfmon (or root); skips itself otherwise.
    #[test]
    fn sees_a_busy_thread() {
        let cpus = perf::online_cpus().len();
        let tracer = match CoreTracer::start(cpus) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("not allowed to trace ({e}), skipping");
                return;
            }
        };
        let tid = unsafe { libc::gettid() } as u32;
        let started = Instant::now();
        let mut x = 0u64;
        while started.elapsed() < Duration::from_millis(300) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        std::thread::sleep(Duration::from_millis(200));
        let samples = tracer.take();
        let mine: u32 = samples
            .per_cpu
            .iter()
            .flatten()
            .filter(|(t, _)| *t == tid)
            .map(|(_, n)| n)
            .sum();
        assert!(mine > 50, "{mine} samples of a thread busy for 300 ms");
        assert_eq!(samples.owners.get(&tid), Some(&std::process::id()));
    }
}
