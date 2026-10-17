//! How long drivers keep the processors from running anything else: every
//! deferred procedure call (DPC) and interrupt service routine (ISR) the
//! kernel runs, timed by the kernel trace (ETW, admin) and charged to the
//! driver whose code it was (LatencyMon's measurement). A DPC or ISR that runs
//! for a millisecond holds up everything else on its processor, audio
//! buffers and game frames included, which is how a misbehaving network,
//! storage or graphics driver turns into crackles and stutters.
//!
//! A busy machine runs tens of thousands of them a second, so events are
//! added up on the consumer thread in plain counters keyed by routine
//! address, handed over in batches, and named per driver only once a refresh.
//! Like the other kernel traces it only runs while someone is looking.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use windows::Win32::System::Diagnostics::Etw::{
    EVENT_TRACE_FLAG_DPC, EVENT_TRACE_FLAG_INTERRUPT, EVENT_TRACE_SYSTEM_LOGGER_MODE, PerfInfoGuid,
};

use crate::etw::{self, QpcClock, Session};
use crate::modules::{self, ModuleInfo};

const OPCODE_THREADED_DPC: u8 = 66;
const OPCODE_ISR: u8 = 67;
const OPCODE_DPC: u8 = 68;
const OPCODE_TIMER_DPC: u8 = 69;

/// Upper bounds of the histogram's buckets in ms; the last bucket is open.
pub const BUCKETS_MS: [f64; 4] = [0.1, 0.5, 1.0, 2.0];
/// Longer than this is a spike worth listing with its time.
const SPIKE_MS: f64 = 1.0;
/// No DPC or interrupt runs this long: the DPC watchdog would have stopped
/// Windows long before. A measurement past it is a bad start time.
const PLAUSIBLE_MS: f64 = 1000.0;
const MAX_SPIKES: usize = 200;
/// Handed to the collector at least this often (in events, or ms of trace time).
const BATCH_EVENTS: u32 = 4096;
const BATCH_MS: f64 = 100.0;
/// Drivers load rarely; an address no module covers asks again after this.
const MODULES_EVERY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Deferred procedure call, threaded and timer ones included.
    Dpc,
    /// Interrupt service routine.
    Isr,
}

/// One kind of work a driver did.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// Since tracing started.
    pub count: u64,
    /// Over the last refresh.
    pub per_sec: f32,
    /// Share of one logical processor over the last refresh.
    pub busy: f32,
    /// The longest single one since tracing started, in µs.
    pub highest_us: f32,
    /// The longest over the last refresh, in µs.
    pub recent_highest_us: f32,
    /// Since tracing started, by length: under 0.1 ms, 0.5, 1, 2 ms, longer.
    pub histogram: [u64; 5],
}

#[derive(Clone, Debug)]
pub struct DriverLatency {
    /// The driver's file name (`nvlddmkm.sys`), or "unknown".
    pub driver: Arc<str>,
    pub path: Arc<str>,
    pub dpc: Activity,
    pub isr: Activity,
}

impl DriverLatency {
    pub fn highest_us(&self) -> f32 {
        self.dpc.highest_us.max(self.isr.highest_us)
    }
}

/// A DPC or ISR longer than a millisecond.
#[derive(Clone, Debug)]
pub struct LatencySpike {
    /// When it ended.
    pub at: SystemTime,
    pub driver: Arc<str>,
    pub kind: Kind,
    pub cpu: u16,
    pub us: f32,
}

#[derive(Clone, Debug, Default)]
pub struct LatencyStats {
    /// How long tracing has run.
    pub measured: Duration,
    /// The longest single one first.
    pub drivers: Vec<DriverLatency>,
    /// Per logical processor, the longest DPC or ISR since tracing started (µs).
    pub per_cpu_highest_us: Vec<f32>,
    /// Newest last.
    pub spikes: Vec<LatencySpike>,
    /// No driver addresses could be read (Windows hides them without
    /// administrator rights), so nothing is named.
    pub unnamed: bool,
    /// Events the session dropped so far (it produced faster than we read).
    pub lost_events: u32,
    /// Events whose start time could not be right (zero, from before the
    /// trace began, or longer than any DPC can run), left out.
    pub discarded: u64,
}

/// Keys are code addresses: mixing them is all the hashing they need.
#[derive(Default)]
struct AddressHasher(u64);

impl Hasher for AddressHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }
}

type ByAddress<V> = HashMap<u64, V, BuildHasherDefault<AddressHasher>>;

/// Time spent in one routine, in counter ticks.
#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    count: u64,
    ticks: u64,
    max: u64,
    histogram: [u64; 5],
}

impl Acc {
    fn add(&mut self, ticks: u64, bounds: &[u64; 4]) {
        self.count += 1;
        self.ticks += ticks;
        self.max = self.max.max(ticks);
        let bucket = bounds.iter().position(|&b| ticks < b).unwrap_or(4);
        self.histogram[bucket] += 1;
    }

    fn merge(&mut self, other: &Acc) {
        self.count += other.count;
        self.ticks += other.ticks;
        self.max = self.max.max(other.max);
        for (a, b) in self.histogram.iter_mut().zip(other.histogram) {
            *a += b;
        }
    }
}

#[derive(Clone, Copy)]
struct RawSpike {
    end: i64,
    routine: u64,
    kind: Kind,
    cpu: u16,
    ticks: u64,
}

/// What the consumer gathered since the collector last took it.
#[derive(Default)]
struct Batch {
    dpc: ByAddress<Acc>,
    isr: ByAddress<Acc>,
    per_cpu_max: Vec<u64>,
    spikes: Vec<RawSpike>,
    discarded: u64,
}

impl Batch {
    fn merge(&mut self, other: &mut Batch) {
        for (mine, theirs) in [
            (&mut self.dpc, &mut other.dpc),
            (&mut self.isr, &mut other.isr),
        ] {
            for (routine, acc) in theirs.drain() {
                mine.entry(routine).or_default().merge(&acc);
            }
        }
        if self.per_cpu_max.len() < other.per_cpu_max.len() {
            self.per_cpu_max.resize(other.per_cpu_max.len(), 0);
        }
        for (a, b) in self.per_cpu_max.iter_mut().zip(other.per_cpu_max.drain(..)) {
            *a = (*a).max(b);
        }
        let room = (MAX_SPIKES * 4).saturating_sub(self.spikes.len());
        self.spikes.extend(other.spikes.drain(..).take(room));
        self.discarded += std::mem::take(&mut other.discarded);
    }
}

/// The kind of a PerfInfo event, its start time and routine address. The
/// event's own timestamp is when it ended.
fn parse(opcode: u8, data: &[u8]) -> Option<(Kind, u64, u64)> {
    let kind = match opcode {
        OPCODE_DPC | OPCODE_TIMER_DPC | OPCODE_THREADED_DPC => Kind::Dpc,
        OPCODE_ISR => Kind::Isr,
        _ => return None,
    };
    // InitialTime, Routine (kernel events are always 64-bit here), ...
    Some((kind, etw::u64_at(data, 0)?, etw::u64_at(data, 8)?))
}

/// Cumulative per driver, on the collector thread.
#[derive(Default)]
struct Totals {
    dpc: Acc,
    isr: Acc,
    path: Arc<str>,
}

pub struct LatencyTracer {
    shared: Arc<Mutex<Batch>>,
    clock: QpcClock,
    session: Session,
    started: Instant,
    last_take: Instant,
    modules: Vec<ModuleInfo>,
    modules_at: Instant,
    totals: HashMap<Arc<str>, Totals>,
    per_cpu_highest: Vec<u64>,
    spikes: VecDeque<LatencySpike>,
    discarded: u64,
}

impl LatencyTracer {
    pub fn start() -> Result<Self, String> {
        let flags = EVENT_TRACE_FLAG_DPC.0 | EVENT_TRACE_FLAG_INTERRUPT.0;
        let session = Session::start("Anabolic-Latency", EVENT_TRACE_SYSTEM_LOGGER_MODE, flags)?;
        let clock = QpcClock::new();
        let ticks = |ms: f64| clock.from_ms(ms) as u64;
        let bounds = BUCKETS_MS.map(ticks);
        let spike = ticks(SPIKE_MS);
        let batch_ticks = clock.from_ms(BATCH_MS);
        let plausible = ticks(PLAUSIBLE_MS);
        // Some events carry a start time of zero (seen on this author's
        // machine: the "DPC" then lasted as long as Windows had been up).
        // Anything that started before the trace did is not to be trusted.
        let began = QpcClock::ticks() as u64;
        let shared = Arc::new(Mutex::new(Batch::default()));
        let sink = shared.clone();
        // The consumer's own batch, handed over now and then.
        let mut local = Batch::default();
        let mut pending = 0u32;
        let mut handed = 0i64;
        session.consume(move |record| {
            let header = &record.EventHeader;
            if header.ProviderId != PerfInfoGuid {
                return;
            }
            let Some((kind, initial, routine)) =
                parse(header.EventDescriptor.Opcode, etw::user_data(record))
            else {
                return;
            };
            let end = header.TimeStamp;
            // A start after the end would be a clock mix-up; skip it, and
            // impossible starts and lengths too.
            let ticks = match (end as u64).checked_sub(initial) {
                Some(t) if initial >= began && t <= plausible => t,
                _ => {
                    local.discarded += 1;
                    return;
                }
            };
            let cpu = unsafe { record.BufferContext.Anonymous.ProcessorIndex };
            let map = match kind {
                Kind::Dpc => &mut local.dpc,
                Kind::Isr => &mut local.isr,
            };
            map.entry(routine).or_default().add(ticks, &bounds);
            let slot = cpu as usize;
            if local.per_cpu_max.len() <= slot {
                local.per_cpu_max.resize(slot + 1, 0);
            }
            local.per_cpu_max[slot] = local.per_cpu_max[slot].max(ticks);
            if ticks >= spike && local.spikes.len() < MAX_SPIKES {
                local.spikes.push(RawSpike {
                    end,
                    routine,
                    kind,
                    cpu,
                    ticks,
                });
            }
            pending += 1;
            if pending >= BATCH_EVENTS || end - handed >= batch_ticks {
                if let Ok(mut shared) = sink.lock() {
                    shared.merge(&mut local);
                }
                pending = 0;
                handed = end;
            }
        })?;
        let now = Instant::now();
        Ok(Self {
            shared,
            clock,
            session,
            started: now,
            last_take: now,
            modules: modules::kernel_modules(&crate::winfile::DeviceMap::build()),
            modules_at: now,
            totals: HashMap::new(),
            per_cpu_highest: Vec::new(),
            spikes: VecDeque::new(),
            discarded: 0,
        })
    }

    /// The driver a routine belongs to: (name, path).
    fn driver_of(&self, routine: u64) -> (Arc<str>, Arc<str>) {
        match modules::module_at(&self.modules, routine) {
            Some(m) => (m.name.clone(), m.path.clone()),
            None => (Arc::from("unknown"), Arc::from("")),
        }
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
        let unresolved = batch
            .dpc
            .keys()
            .chain(batch.isr.keys())
            .any(|&r| modules::module_at(&self.modules, r).is_none());
        if unresolved && now - self.modules_at >= MODULES_EVERY {
            self.modules = modules::kernel_modules(&crate::winfile::DeviceMap::build());
            self.modules_at = now;
        }
        // This refresh, per driver.
        let mut recent: HashMap<Arc<str>, (Acc, Acc)> = HashMap::new();
        for (kind, map) in [(Kind::Dpc, &batch.dpc), (Kind::Isr, &batch.isr)] {
            for (&routine, acc) in map {
                let (driver, path) = self.driver_of(routine);
                let totals = self.totals.entry(driver.clone()).or_default();
                totals.path = path;
                let pair = recent.entry(driver).or_default();
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
                at: self.clock.wall(s.end),
                driver: self.driver_of(s.routine).0,
                kind: s.kind,
                cpu: s.cpu,
                us: self.us(s.ticks),
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
                    path: totals.path.clone(),
                    dpc: self.activity(&totals.dpc, dpc, secs),
                    isr: self.activity(&totals.isr, isr, secs),
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
            per_cpu_highest_us: self.per_cpu_highest.iter().map(|&t| self.us(t)).collect(),
            spikes: self.spikes.iter().cloned().collect(),
            unnamed: self.modules.is_empty(),
            lost_events: self.session.lost().0,
            discarded: self.discarded,
        }
    }

    fn us(&self, ticks: u64) -> f32 {
        self.clock.ms(ticks as i64) * 1000.0
    }

    fn activity(&self, total: &Acc, recent: &Acc, secs: f64) -> Activity {
        Activity {
            count: total.count,
            per_sec: (recent.count as f64 / secs) as f32,
            busy: (recent.ticks as f64 / self.clock.freq as f64 / secs) as f32,
            highest_us: self.us(total.max),
            recent_highest_us: self.us(recent.max),
            histogram: total.histogram,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfinfo_payloads_are_parsed() {
        let mut d = vec![0u8; 16];
        d[..8].copy_from_slice(&1_000u64.to_le_bytes());
        d[8..].copy_from_slice(&0xFFFF_F801_2345_6789u64.to_le_bytes());
        assert_eq!(
            parse(OPCODE_DPC, &d),
            Some((Kind::Dpc, 1_000, 0xFFFF_F801_2345_6789))
        );
        assert_eq!(parse(OPCODE_TIMER_DPC, &d).map(|p| p.0), Some(Kind::Dpc));
        assert_eq!(parse(OPCODE_THREADED_DPC, &d).map(|p| p.0), Some(Kind::Dpc));
        // ISRs carry more after the routine (return value, vector).
        let mut isr = d.clone();
        isr.extend_from_slice(&[1, 0x31, 0, 0]);
        assert_eq!(parse(OPCODE_ISR, &isr).map(|p| p.0), Some(Kind::Isr));
        assert_eq!(parse(46, &d), None);
        assert_eq!(parse(OPCODE_DPC, &d[..12]), None);
    }

    #[test]
    fn times_fall_into_buckets() {
        // 10 MHz counter: 1 ms = 10,000 ticks.
        let bounds = [1_000, 5_000, 10_000, 20_000];
        let mut acc = Acc::default();
        for ticks in [10, 999, 1_000, 7_000, 15_000, 20_000, 90_000] {
            acc.add(ticks, &bounds);
        }
        assert_eq!(acc.histogram, [2, 1, 1, 1, 2]);
        assert_eq!(acc.max, 90_000);
        assert_eq!(acc.count, 7);
        let mut other = Acc::default();
        other.add(100_000, &bounds);
        acc.merge(&other);
        assert_eq!(acc.max, 100_000);
        assert_eq!(acc.histogram[4], 3);
    }

    #[test]
    fn batches_merge() {
        let bounds = [1_000, 5_000, 10_000, 20_000];
        let mut a = Batch::default();
        let mut b = Batch::default();
        a.dpc.entry(1).or_default().add(500, &bounds);
        b.dpc.entry(1).or_default().add(9_000, &bounds);
        b.isr.entry(2).or_default().add(100, &bounds);
        a.per_cpu_max = vec![10, 50];
        b.per_cpu_max = vec![20, 5, 70];
        a.merge(&mut b);
        assert_eq!(a.dpc[&1].count, 2);
        assert_eq!(a.dpc[&1].max, 9_000);
        assert_eq!(a.isr[&2].count, 1);
        assert_eq!(a.per_cpu_max, vec![20, 50, 70]);
        assert!(b.dpc.is_empty());
    }

    /// Needs an elevated test run; skips itself otherwise.
    #[test]
    fn measures_drivers_live() {
        if !crate::hardening::is_elevated() {
            eprintln!("not elevated, skipping");
            return;
        }
        let _turn = crate::etw::kernel_turn();
        let mut tracer = LatencyTracer::start().expect("could not start the trace");
        std::thread::sleep(Duration::from_millis(2500));
        let stats = tracer.take();
        assert!(!stats.unnamed, "kernel modules were not readable");
        let total: u64 = stats
            .drivers
            .iter()
            .map(|d| d.dpc.count + d.isr.count)
            .sum();
        assert!(total > 100, "only {total} DPCs and ISRs in 2.5 s");
        assert!(
            stats
                .drivers
                .iter()
                .any(|d| d.driver.eq_ignore_ascii_case("ntoskrnl.exe")),
            "the kernel's own timer DPCs were not named"
        );
        // Durations are plausible: most are short.
        let short: u64 = stats
            .drivers
            .iter()
            .map(|d| d.dpc.histogram[0] + d.isr.histogram[0])
            .sum();
        assert!(
            short * 2 > total,
            "most should be under 0.1 ms: {short} of {total}"
        );
    }
}
