// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! How long interrupts keep the processors from running programs, the way
//! LatencyMon measures Windows' DPCs and ISRs: every hardware interrupt
//! handler and every softirq the kernel runs, timed from the kernel's
//! tracepoints (perf events, cap_perfmon) and charged to the handler that
//! ran (a driver's interrupt, `nvme0`, `i915`, `iwlwifi`) or the softirq's
//! kind (network receive, block completion, timers).
//!
//! On Linux a hardware interrupt handler is Windows' ISR; the softirqs that
//! finish its work later are its DPCs. A softirq that a hardware interrupt
//! cuts into is charged its own time only.
//!
//! Like the other kernel traces it only runs while someone is looking.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::perf::{self, Attr, Field, Record, Sample, Trace};

use super::*;

/// Longer than this is a spike worth listing with its time.
const SPIKE_MS: f64 = 1.0;
/// No interrupt runs this long; a measurement past it pairs the wrong events.
const PLAUSIBLE_MS: f64 = 1000.0;
const MAX_SPIKES: usize = 200;
const PAGES: usize = 64;
const SAMPLE_TYPE: u64 = perf::SAMPLE_TIME | perf::SAMPLE_CPU | perf::SAMPLE_RAW;

/// The softirq vectors, by number.
const SOFTIRQS: [&str; 10] = [
    "HI", "TIMER", "NET_TX", "NET_RX", "BLOCK", "IRQ_POLL", "TASKLET", "SCHED", "HRTIMER", "RCU",
];

/// Time spent in one handler, in ns.
#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    count: u64,
    ns: u64,
    max: u64,
    histogram: [u64; 5],
}

impl Acc {
    fn add(&mut self, ns: u64) {
        self.count += 1;
        self.ns += ns;
        self.max = self.max.max(ns);
        let ms = ns as f64 / 1e6;
        let bucket = BUCKETS_MS.iter().position(|&b| ms < b).unwrap_or(4);
        self.histogram[bucket] += 1;
    }

    fn merge(&mut self, other: &Acc) {
        self.count += other.count;
        self.ns += other.ns;
        self.max = self.max.max(other.max);
        for (a, b) in self.histogram.iter_mut().zip(other.histogram) {
            *a += b;
        }
    }
}

#[derive(Clone)]
struct RawSpike {
    end: u64,
    driver: Arc<str>,
    kind: Kind,
    cpu: u16,
    ns: u64,
}

/// What the reader gathered since the collector last took it.
#[derive(Default)]
struct Batch {
    /// (handler, kind) -> time.
    handlers: HashMap<(Arc<str>, Kind), Acc>,
    per_cpu_max: Vec<u64>,
    spikes: Vec<RawSpike>,
    discarded: u64,
    lost: u64,
    /// The interrupt lines each driver's handlers serve.
    irqs: HashMap<Arc<str>, BTreeSet<u32>>,
}

/// One processor's handlers under way.
#[derive(Default)]
struct Open {
    /// A hardware interrupt: its handler and when it began.
    irq: Option<(Arc<str>, u64)>,
    /// A softirq: its vector, when it began, and the hardware interrupt
    /// time inside it so far.
    softirq: Option<(u32, u64, u64)>,
}

/// The tracepoints' ids and the fields read from them.
struct Events {
    irq_entry: u64,
    irq_exit: u64,
    softirq_entry: u64,
    softirq_exit: u64,
    irq: Field,
    name: Field,
    vec: Field,
}

/// The driver an interrupt handler belongs to: its name without the queue
/// a per-queue interrupt adds (`nvme0q12` → `nvme0`, `enp59s0-rx-3` →
/// `enp59s0`, `iwlwifi:queue_2` → `iwlwifi`).
fn driver_of(handler: &str) -> &str {
    let stem = handler.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() == handler.len() {
        return handler;
    }
    for marker in [
        "-TxRx-", "-rx-", "-tx-", ":queue_", "-queue-", "_queue_", "q",
    ] {
        if let Some(name) = stem.strip_suffix(marker)
            && !name.is_empty()
        {
            // `q` only after a device number, as in nvme0q3.
            if marker != "q" || name.ends_with(|c: char| c.is_ascii_digit()) {
                return name;
            }
        }
    }
    handler
}

/// Feeds one event into the processor's open handlers and the batch.
fn on_event(
    events: &Events,
    open: &mut Vec<Open>,
    batch: &mut Batch,
    names: &mut HashMap<String, Arc<str>>,
    s: &Sample<'_>,
) {
    let Some(id) = perf::event_id(s.raw) else {
        return;
    };
    let cpu = s.cpu as usize;
    if open.len() <= cpu {
        open.resize_with(cpu + 1, Open::default);
    }
    let slot = &mut open[cpu];
    let plausible = (PLAUSIBLE_MS * 1e6) as u64;
    let finish = |batch: &mut Batch, driver: Arc<str>, kind: Kind, began: u64| {
        let Some(ns) = s.time.checked_sub(began).filter(|&ns| ns <= plausible) else {
            batch.discarded += 1;
            return 0;
        };
        batch
            .handlers
            .entry((driver.clone(), kind))
            .or_default()
            .add(ns);
        if batch.per_cpu_max.len() <= cpu {
            batch.per_cpu_max.resize(cpu + 1, 0);
        }
        batch.per_cpu_max[cpu] = batch.per_cpu_max[cpu].max(ns);
        if ns as f64 >= SPIKE_MS * 1e6 && batch.spikes.len() < MAX_SPIKES * 4 {
            batch.spikes.push(RawSpike {
                end: s.time,
                driver,
                kind,
                cpu: cpu as u16,
                ns,
            });
        }
        ns
    };
    if id == events.irq_entry {
        let handler = events.name.str(s.raw).unwrap_or("?");
        let name = match names.get(handler) {
            Some(n) => n.clone(),
            None => {
                let n: Arc<str> = Arc::from(driver_of(handler));
                names.insert(handler.to_string(), n.clone());
                n
            }
        };
        if let Some(irq) = events.irq.u32(s.raw) {
            batch.irqs.entry(name.clone()).or_default().insert(irq);
        }
        slot.irq = Some((name, s.time));
    } else if id == events.irq_exit {
        // An exit whose entry came before the trace began is left out.
        if let Some((driver, began)) = slot.irq.take() {
            let ns = finish(batch, driver, Kind::Isr, began);
            if let Some((_, _, inside)) = &mut slot.softirq {
                *inside += ns;
            }
        }
    } else if id == events.softirq_entry {
        let vec = events.vec.u32(s.raw).unwrap_or(u32::MAX);
        slot.softirq = Some((vec, s.time, 0));
    } else if id == events.softirq_exit
        && let Some((vec, began, inside)) = slot.softirq.take()
    {
        let driver = softirq_name(vec, names);
        // Its own time: the hardware interrupts inside it are theirs.
        finish(batch, driver, Kind::Dpc, began + inside);
    }
}

fn softirq_name(vec: u32, names: &mut HashMap<String, Arc<str>>) -> Arc<str> {
    let name = SOFTIRQS
        .get(vec as usize)
        .map_or_else(|| format!("softirq {vec}"), |n| format!("{n} softirq"));
    names
        .entry(name.clone())
        .or_insert_with(|| Arc::from(name))
        .clone()
}

/// Cumulative per handler, on the collector thread.
#[derive(Default)]
struct Totals {
    dpc: Acc,
    isr: Acc,
    /// Its interrupt lines ("IRQ 145, 146"), or what kind of softirq.
    irqs: BTreeSet<u32>,
}

impl Totals {
    fn path(&self, driver: &str) -> Arc<str> {
        if driver.ends_with("softirq") {
            return Arc::from("deferred work (softirq)");
        }
        let lines: Vec<String> = self.irqs.iter().map(u32::to_string).collect();
        Arc::from(match lines.len() {
            0 => String::new(),
            1 => format!("IRQ {}", lines[0]),
            _ => format!("IRQ {}", lines.join(", ")),
        })
    }
}

pub struct LatencyTracer {
    shared: Arc<Mutex<Batch>>,
    _trace: Trace,
    started: Instant,
    last_take: Instant,
    totals: HashMap<Arc<str>, Totals>,
    per_cpu_highest: Vec<u64>,
    spikes: VecDeque<LatencySpike>,
    discarded: u64,
    lost: u64,
}

impl LatencyTracer {
    pub fn start() -> Result<Self, String> {
        let irq_entry = perf::tracepoint("irq", "irq_handler_entry")?;
        let irq_exit = perf::tracepoint("irq", "irq_handler_exit")?;
        let softirq_entry = perf::tracepoint("irq", "softirq_entry")?;
        let softirq_exit = perf::tracepoint("irq", "softirq_exit")?;
        let events = Events {
            irq_entry: irq_entry.id,
            irq_exit: irq_exit.id,
            softirq_entry: softirq_entry.id,
            softirq_exit: softirq_exit.id,
            irq: irq_entry.field("irq")?,
            name: irq_entry.field("name")?,
            vec: softirq_entry.field("vec")?,
        };
        let ids = [irq_entry.id, irq_exit.id, softirq_entry.id, softirq_exit.id];
        let shared = Arc::new(Mutex::new(Batch::default()));
        let sink = shared.clone();
        let mut open: Vec<Open> = Vec::new();
        let mut names: HashMap<String, Arc<str>> = HashMap::new();
        // The reader's own batch, handed over now and then.
        let mut local = Batch::default();
        let mut handed = Instant::now();
        let trace = Trace::start(
            "latency",
            |_| {
                ids.iter()
                    .map(|&id| {
                        let mut attr = Attr::new(perf::TYPE_TRACEPOINT, id);
                        attr.sample = 1;
                        attr.sample_type = SAMPLE_TYPE;
                        perf::watermark(&mut attr, PAGES);
                        attr
                    })
                    .collect()
            },
            PAGES,
            move |record| {
                match record {
                    Record::Sample(body) => {
                        if let Some(s) = Sample::parse(body, SAMPLE_TYPE) {
                            on_event(&events, &mut open, &mut local, &mut names, &s);
                        }
                    }
                    Record::Lost(n) => local.lost += n,
                    _ => {}
                }
                if handed.elapsed().as_millis() >= 100
                    && let Ok(mut shared) = sink.lock()
                {
                    shared.merge(&mut local);
                    handed = Instant::now();
                }
            },
        )?;
        let now = Instant::now();
        Ok(Self {
            shared,
            _trace: trace,
            started: now,
            last_take: now,
            totals: HashMap::new(),
            per_cpu_highest: Vec::new(),
            spikes: VecDeque::new(),
            discarded: 0,
            lost: 0,
        })
    }

    pub fn take(&mut self) -> LatencyStats {
        let batch = match self.shared.lock() {
            Ok(mut shared) => std::mem::take(&mut *shared),
            Err(_) => Batch::default(),
        };
        let now = Instant::now();
        let secs = (now - self.last_take).as_secs_f64().max(1e-3);
        self.last_take = now;
        self.discarded += batch.discarded;
        self.lost += batch.lost;
        for (driver, irqs) in &batch.irqs {
            self.totals
                .entry(driver.clone())
                .or_default()
                .irqs
                .extend(irqs);
        }
        // This refresh, per handler.
        let mut recent: HashMap<Arc<str>, (Acc, Acc)> = HashMap::new();
        for ((driver, kind), acc) in &batch.handlers {
            let totals = self.totals.entry(driver.clone()).or_default();
            let pair = recent.entry(driver.clone()).or_default();
            match kind {
                Kind::Dpc => {
                    totals.dpc.merge(acc);
                    pair.0.merge(acc);
                }
                Kind::Isr => {
                    totals.isr.merge(acc);
                    pair.1.merge(acc);
                }
            }
        }
        if self.per_cpu_highest.len() < batch.per_cpu_max.len() {
            self.per_cpu_highest.resize(batch.per_cpu_max.len(), 0);
        }
        for (a, b) in self.per_cpu_highest.iter_mut().zip(&batch.per_cpu_max) {
            *a = (*a).max(*b);
        }
        for s in &batch.spikes {
            if self.spikes.len() == MAX_SPIKES {
                self.spikes.pop_front();
            }
            self.spikes.push_back(LatencySpike {
                at: perf::wall(s.end),
                driver: s.driver.clone(),
                kind: s.kind,
                cpu: s.cpu,
                us: us(s.ns),
            });
        }
        let empty = (Acc::default(), Acc::default());
        let mut drivers: Vec<DriverLatency> = self
            .totals
            .iter()
            .map(|(driver, totals)| {
                let (dpc, isr) = recent.get(driver).unwrap_or(&empty);
                DriverLatency {
                    driver: driver.clone(),
                    path: totals.path(driver),
                    dpc: activity(&totals.dpc, dpc, secs),
                    isr: activity(&totals.isr, isr, secs),
                }
            })
            .collect();
        drivers.sort_by(|a, b| {
            b.highest_us()
                .total_cmp(&a.highest_us())
                .then_with(|| a.driver.cmp(&b.driver))
        });
        LatencyStats {
            measured: now - self.started,
            drivers,
            per_cpu_highest_us: self.per_cpu_highest.iter().map(|&t| us(t)).collect(),
            spikes: self.spikes.iter().cloned().collect(),
            unnamed: false,
            lost_events: self.lost.min(u32::MAX as u64) as u32,
            discarded: self.discarded,
        }
    }
}

impl Batch {
    fn merge(&mut self, other: &mut Batch) {
        for (key, acc) in other.handlers.drain() {
            self.handlers.entry(key).or_default().merge(&acc);
        }
        if self.per_cpu_max.len() < other.per_cpu_max.len() {
            self.per_cpu_max.resize(other.per_cpu_max.len(), 0);
        }
        for (a, b) in self.per_cpu_max.iter_mut().zip(other.per_cpu_max.drain(..)) {
            *a = (*a).max(b);
        }
        let room = (MAX_SPIKES * 4).saturating_sub(self.spikes.len());
        self.spikes.extend(other.spikes.drain(..).take(room));
        other.spikes.clear();
        self.discarded += std::mem::take(&mut other.discarded);
        self.lost += std::mem::take(&mut other.lost);
        for (driver, irqs) in other.irqs.drain() {
            self.irqs.entry(driver).or_default().extend(irqs);
        }
    }
}

fn us(ns: u64) -> f32 {
    ns as f32 / 1000.0
}

fn activity(total: &Acc, recent: &Acc, secs: f64) -> Activity {
    Activity {
        count: total.count,
        per_sec: (recent.count as f64 / secs) as f32,
        busy: (recent.ns as f64 / 1e9 / secs) as f32,
        highest_us: us(total.max),
        recent_highest_us: us(recent.max),
        histogram: total.histogram,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_drivers_without_their_queues() {
        assert_eq!(driver_of("nvme0q12"), "nvme0");
        assert_eq!(driver_of("enp59s0-rx-3"), "enp59s0");
        assert_eq!(driver_of("iwlwifi:queue_2"), "iwlwifi");
        assert_eq!(driver_of("i915"), "i915");
        assert_eq!(driver_of("xhci_hcd"), "xhci_hcd");
        assert_eq!(driver_of("idma64.0"), "idma64.0");
        assert_eq!(driver_of("snd_hda_intel:card0"), "snd_hda_intel:card0");
    }

    /// A raw record for one of the tracepoints below.
    fn raw(id: u16, a: u32, name: &str) -> Vec<u8> {
        let mut r = vec![0u8; 16];
        r[0..2].copy_from_slice(&id.to_ne_bytes());
        r[8..12].copy_from_slice(&a.to_ne_bytes());
        r[12..16].copy_from_slice(&(16u32 | ((name.len() as u32 + 1) << 16)).to_ne_bytes());
        r.extend_from_slice(name.as_bytes());
        r.push(0);
        r
    }

    #[test]
    fn times_interrupts_and_softirqs_per_processor() {
        let field = |offset| Field {
            offset,
            size: 4,
            data_loc: false,
        };
        let events = Events {
            irq_entry: 1,
            irq_exit: 2,
            softirq_entry: 3,
            softirq_exit: 4,
            irq: field(8),
            name: Field {
                offset: 12,
                size: 4,
                data_loc: true,
            },
            vec: field(8),
        };
        let (mut open, mut batch, mut names) = (Vec::new(), Batch::default(), HashMap::new());
        let mut feed = |id: u16, a: u32, name: &str, cpu: u32, time: u64| {
            let r = raw(id, a, name);
            let s = Sample {
                pid: 0,
                tid: 0,
                time,
                cpu,
                raw: &r,
            };
            on_event(&events, &mut open, &mut batch, &mut names, &s);
        };
        // A NET_RX softirq on processor 1, cut into by nvme's interrupt.
        feed(3, 3, "", 1, 1_000_000);
        feed(1, 145, "nvme0q3", 1, 1_200_000);
        feed(2, 145, "", 1, 1_250_000);
        feed(4, 3, "", 1, 3_000_000);
        // An exit without its entry (from before the trace) is left out.
        feed(2, 9, "", 0, 5_000_000);
        let nvme = &batch.handlers[&(Arc::from("nvme0"), Kind::Isr)];
        assert_eq!((nvme.count, nvme.ns), (1, 50_000));
        let rx = &batch.handlers[&(Arc::from("NET_RX softirq"), Kind::Dpc)];
        // 2 ms, less the 50 µs interrupt: a spike.
        assert_eq!(rx.ns, 1_950_000);
        assert_eq!(batch.spikes.len(), 1);
        assert_eq!(batch.per_cpu_max[1], 1_950_000);
        assert_eq!(rx.histogram, [0, 0, 0, 1, 0]);
    }
}
