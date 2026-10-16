//! Leaks: processes whose handles, private memory or threads keep growing.
//! A sample a minute per process, two hours kept; a counter that rose
//! steadily (a straight line fits it well) and by a real amount over at
//! least ten minutes is reported. Bursts that come and go, and programs
//! that grow to a size and stay there, are not. Fed from the snapshots the
//! collector takes anyway; no process is opened.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::model::{ProcKey, ProcState, Snapshot};

/// One sample a minute: leaks are slow, and memory here is per process.
const STEP: Duration = Duration::from_secs(60);
/// Two hours of samples.
const KEEP: usize = 120;
/// No verdict on less than this.
const MIN_SPAN: Duration = Duration::from_secs(10 * 60);
/// How well a straight line must fit (R²): steady growth, not a jump.
const MIN_FIT: f64 = 0.8;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Metric {
    Handles,
    Memory,
    Threads,
}

impl Metric {
    pub const ALL: [Metric; 3] = [Metric::Handles, Metric::Memory, Metric::Threads];

    pub fn label(self) -> &'static str {
        match self {
            Metric::Handles => "handles",
            Metric::Memory => "private memory",
            Metric::Threads => "threads",
        }
    }

    /// How much it must have grown (in total, and per hour) to count.
    fn thresholds(self) -> (f64, f64) {
        match self {
            Metric::Handles => (2_000.0, 1_000.0),
            Metric::Memory => (256.0 * 1024.0 * 1024.0, 200.0 * 1024.0 * 1024.0),
            Metric::Threads => (100.0, 100.0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Leak {
    pub key: ProcKey,
    pub name: Arc<str>,
    pub metric: Metric,
    pub per_hour: f64,
    /// Over how long it was seen growing.
    pub span: Duration,
    pub from: f64,
    pub to: f64,
}

impl Leak {
    /// How far past the threshold it is, to put the worst first.
    fn severity(&self) -> f64 {
        self.per_hour / self.metric.thresholds().1
    }
}

#[derive(Clone, Copy)]
struct Sample {
    /// Seconds since the tracker started.
    at: f64,
    handles: f64,
    memory: f64,
    threads: f64,
}

impl Sample {
    fn get(&self, m: Metric) -> f64 {
        match m {
            Metric::Handles => self.handles,
            Metric::Memory => self.memory,
            Metric::Threads => self.threads,
        }
    }
}

struct Tracked {
    name: Arc<str>,
    samples: VecDeque<Sample>,
}

#[derive(Default)]
pub struct Growth {
    tracked: HashMap<ProcKey, Tracked>,
    /// Seconds of snapshots seen, summed from their intervals.
    clock: f64,
    next_at: f64,
    last_seq: u64,
    leaks: Vec<Leak>,
}

/// Least-squares slope (per second) and R² of `points`.
pub fn fit(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = points.len() as f64;
    if n < 3.0 {
        return None;
    }
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in points {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
        syy += (y - my) * (y - my);
    }
    if sxx == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let r2 = if syy == 0.0 {
        0.0
    } else {
        (sxy * sxy) / (sxx * syy)
    };
    Some((slope, r2))
}

/// Whether `samples` show `metric` leaking.
fn judge(samples: &VecDeque<Sample>, metric: Metric) -> Option<(f64, Duration, f64, f64)> {
    let first = samples.front()?;
    let last = samples.back()?;
    let span = last.at - first.at;
    if span < MIN_SPAN.as_secs_f64() {
        return None;
    }
    let points: Vec<(f64, f64)> = samples.iter().map(|s| (s.at, s.get(metric))).collect();
    let (slope, r2) = fit(&points)?;
    let per_hour = slope * 3600.0;
    let rise = last.get(metric) - first.get(metric);
    let (min_rise, min_rate) = metric.thresholds();
    // Still rising at the end: the last third grew too.
    let tail = &points[points.len() * 2 / 3..];
    let tail_rising = fit(tail).is_some_and(|(s, _)| s > 0.0);
    (r2 >= MIN_FIT && rise >= min_rise && per_hour >= min_rate && tail_rising).then(|| {
        (
            per_hour,
            Duration::from_secs_f64(span),
            first.get(metric),
            last.get(metric),
        )
    })
}

impl Growth {
    /// Takes a sample from each snapshot a minute apart.
    pub fn observe(&mut self, snap: &Snapshot) {
        if snap.seq == self.last_seq {
            return;
        }
        self.last_seq = snap.seq;
        self.clock += snap.interval.as_secs_f64();
        if self.clock < self.next_at {
            return;
        }
        self.next_at = self.clock + STEP.as_secs_f64();
        for p in snap
            .processes
            .iter()
            .filter(|p| p.state != ProcState::Exited && !p.is_pseudo())
        {
            let t = self.tracked.entry(p.key).or_insert_with(|| Tracked {
                name: p.name.clone(),
                samples: VecDeque::new(),
            });
            t.samples.push_back(Sample {
                at: self.clock,
                handles: p.handles as f64,
                memory: p.private_bytes as f64,
                threads: p.threads as f64,
            });
            if t.samples.len() > KEEP {
                t.samples.pop_front();
            }
        }
        // Gone processes: their last sample is older than this one.
        let now = self.clock;
        self.tracked
            .retain(|_, t| t.samples.back().is_some_and(|s| s.at == now));
        self.leaks = self
            .tracked
            .iter()
            .flat_map(|(key, t)| {
                Metric::ALL.into_iter().filter_map(move |metric| {
                    let (per_hour, span, from, to) = judge(&t.samples, metric)?;
                    Some(Leak {
                        key: *key,
                        name: t.name.clone(),
                        metric,
                        per_hour,
                        span,
                        from,
                        to,
                    })
                })
            })
            .collect();
        self.leaks
            .sort_by(|a, b| b.severity().total_cmp(&a.severity()));
    }

    /// Steady growth seen now, the worst first.
    pub fn leaks(&self) -> &[Leak] {
        &self.leaks
    }

    pub fn leaks_of(&self, key: ProcKey) -> impl Iterator<Item = &Leak> {
        self.leaks.iter().filter(move |l| l.key == key)
    }

    /// How long a process has been watched for.
    pub fn watched(&self, key: ProcKey) -> Option<Duration> {
        let t = self.tracked.get(&key)?;
        let span = t.samples.back()?.at - t.samples.front()?.at;
        Some(Duration::from_secs_f64(span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(values: impl Iterator<Item = f64>) -> VecDeque<Sample> {
        values
            .enumerate()
            .map(|(i, v)| Sample {
                at: i as f64 * 60.0,
                handles: v,
                memory: 0.0,
                threads: 0.0,
            })
            .collect()
    }

    #[test]
    fn a_straight_climb_is_a_leak() {
        // +100 handles a minute for 30 minutes: 6,000 an hour.
        let s = samples((0..30).map(|i| 1_000.0 + i as f64 * 100.0));
        let (per_hour, span, from, to) = judge(&s, Metric::Handles).expect("a leak");
        assert!((per_hour - 6_000.0).abs() < 1.0, "{per_hour}");
        assert_eq!(span, Duration::from_secs(29 * 60));
        assert_eq!((from, to), (1_000.0, 3_900.0));
    }

    #[test]
    fn noise_steps_and_plateaus_are_not() {
        // Up and down around a level.
        let noisy = samples((0..40).map(|i| 5_000.0 + if i % 2 == 0 { 800.0 } else { -800.0 }));
        assert!(judge(&noisy, Metric::Handles).is_none());
        // One jump, then flat: grew to a size and stays.
        let step = samples((0..40).map(|i| if i < 20 { 1_000.0 } else { 9_000.0 }));
        assert!(judge(&step, Metric::Handles).is_none());
        // Grew, then levelled off for the last third.
        let plateau = samples((0..45).map(|i| 1_000.0 + (i.min(25) as f64) * 200.0));
        assert!(judge(&plateau, Metric::Handles).is_none());
        // Too slow to matter: 300 handles an hour.
        let slow = samples((0..60).map(|i| 1_000.0 + i as f64 * 5.0));
        assert!(judge(&slow, Metric::Handles).is_none());
        // Too short to tell.
        let short = samples((0..8).map(|i| 1_000.0 + i as f64 * 1_000.0));
        assert!(judge(&short, Metric::Handles).is_none());
    }

    #[test]
    fn fits_a_line() {
        let (slope, r2) = fit(&[(0.0, 1.0), (1.0, 3.0), (2.0, 5.0)]).unwrap();
        assert!((slope - 2.0).abs() < 1e-9);
        assert!((r2 - 1.0).abs() < 1e-9);
    }
}
