//! The flight recorder: what the machine was doing, kept for a few days in
//! 10-second buckets, so a stall can be looked at after it is over. A bucket
//! holds the machine's totals, the processes that led in CPU, memory, disk,
//! I/O, network or GPU, the processes that started or exited, and the ones
//! with a window that was not responding.
//!
//! The collector hands every snapshot to [`Recorder::observe`]: one pass
//! over the process list, no I/O. A finished bucket goes to a writer thread
//! as one text record, appended to a file per local day (`YYYY-MM-DD.txt`);
//! days past the retention are deleted. Records only while Anabolic runs.
//!
//! The format is plain text, one line per item, so a torn write costs one
//! line and the files can be read by eye:
//!
//! ```text
//! B start secs cpu_avg cpu_max mem_used_kb mem_total_kb commit_kb disk_read_kb disk_write_kb disk_active net_rx_kb net_tx_kb gpu_max
//! P pid cpu_avg cpu_max private_kb io_kb disk_kb net_kb gpu_avg name
//! S count pid name    (started in the bucket; pid of the first)
//! X count pid name    (exited in the bucket)
//! N count pid name    (not responding in `count` samples of the bucket)
//! ```

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{ProcKey, ProcState, Snapshot};

pub const BUCKET_SECS: u64 = 10;
/// Distinct names of started (or exited) processes kept in a bucket, so a
/// storm of short-lived processes cannot swell the file.
const MAX_EVENTS: usize = 20;
/// Leaders kept in a bucket: CPU is what stalls are usually about; the
/// other categories keep fewer to hold a day near 5 MB.
const TOP_CPU: usize = 5;
const TOP: usize = 3;
const MB: f64 = 1024.0 * 1024.0;
const HEADER: &str = "# anabolic history 1";
/// A gap longer than this between samples is a sleep, not a measurement.
const MAX_GAP_SECS: f64 = 30.0;

/// One process' share of a bucket.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcRecord {
    pub pid: u32,
    pub name: Arc<str>,
    /// Share of the whole machine, percent: the bucket's average and peak.
    pub cpu_avg: f32,
    pub cpu_max: f32,
    /// Largest private bytes seen.
    pub private: u64,
    /// Bytes moved in the bucket.
    pub io: u64,
    pub disk: u64,
    pub net: u64,
    /// Average GPU use, percent.
    pub gpu_avg: f32,
}

/// Processes of one name that started (or exited) in a bucket.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// The first one's.
    pub pid: u32,
    pub count: u32,
    pub name: Arc<str>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bucket {
    /// Start, seconds since the Unix epoch.
    pub start: u64,
    /// Seconds actually sampled.
    pub secs: f32,
    pub cpu_avg: f32,
    pub cpu_max: f32,
    /// Largest values seen in the bucket.
    pub memory_used: u64,
    pub memory_total: u64,
    pub commit: u64,
    /// Bytes moved in the bucket.
    pub disk_read: u64,
    pub disk_write: u64,
    /// The busiest disk's highest active time, percent.
    pub disk_active: f32,
    pub net_rx: u64,
    pub net_tx: u64,
    /// The busiest GPU's highest use, percent.
    pub gpu_max: f32,
    pub processes: Vec<ProcRecord>,
    pub started: Vec<Event>,
    pub exited: Vec<Event>,
    /// `count`: samples in which a window of it was not responding.
    pub hung: Vec<Event>,
}

impl Bucket {
    fn write(&self, out: &mut String) {
        let kb = |b: u64| b / 1024;
        let _ = writeln!(
            out,
            "B {} {:.1} {:.2} {:.2} {} {} {} {} {} {:.1} {} {} {:.1}",
            self.start,
            self.secs,
            self.cpu_avg,
            self.cpu_max,
            kb(self.memory_used),
            kb(self.memory_total),
            kb(self.commit),
            kb(self.disk_read),
            kb(self.disk_write),
            self.disk_active,
            kb(self.net_rx),
            kb(self.net_tx),
            self.gpu_max,
        );
        for p in &self.processes {
            let _ = writeln!(
                out,
                "P {} {:.2} {:.2} {} {} {} {} {:.1} {}",
                p.pid,
                p.cpu_avg,
                p.cpu_max,
                kb(p.private),
                kb(p.io),
                kb(p.disk),
                kb(p.net),
                p.gpu_avg,
                p.name,
            );
        }
        for (tag, events) in [("S", &self.started), ("X", &self.exited), ("N", &self.hung)] {
            for e in events {
                let _ = writeln!(out, "{tag} {} {} {}", e.count, e.pid, e.name);
            }
        }
    }
}

/// Reads records back; lines that do not parse (a torn write, a newer
/// format) are skipped.
pub fn parse(text: &str) -> Vec<Bucket> {
    let mut buckets = Vec::new();
    let mut current: Option<Bucket> = None;
    for line in text.lines() {
        let Some((tag, rest)) = line.split_once(' ') else {
            continue;
        };
        match tag {
            "B" => {
                buckets.extend(current.take());
                current = parse_header(rest);
            }
            "P" => {
                if let (Some(b), Some(p)) = (current.as_mut(), parse_process(rest)) {
                    b.processes.push(p);
                }
            }
            "S" | "X" | "N" => {
                let f: Vec<&str> = rest.splitn(3, ' ').collect();
                let event = (f.len() == 3)
                    .then(|| {
                        Some(Event {
                            count: f[0].parse().ok()?,
                            pid: f[1].parse().ok()?,
                            name: Arc::from(f[2]),
                        })
                    })
                    .flatten();
                if let (Some(b), Some(event)) = (current.as_mut(), event) {
                    match tag {
                        "S" => b.started.push(event),
                        "X" => b.exited.push(event),
                        _ => b.hung.push(event),
                    }
                }
            }
            _ => {}
        }
    }
    buckets.extend(current);
    buckets
}

fn parse_header(rest: &str) -> Option<Bucket> {
    let f: Vec<&str> = rest.split_whitespace().collect();
    if f.len() < 13 {
        return None;
    }
    let kb = |s: &str| s.parse::<u64>().ok().map(|v| v * 1024);
    Some(Bucket {
        start: f[0].parse().ok()?,
        secs: f[1].parse().ok()?,
        cpu_avg: f[2].parse().ok()?,
        cpu_max: f[3].parse().ok()?,
        memory_used: kb(f[4])?,
        memory_total: kb(f[5])?,
        commit: kb(f[6])?,
        disk_read: kb(f[7])?,
        disk_write: kb(f[8])?,
        disk_active: f[9].parse().ok()?,
        net_rx: kb(f[10])?,
        net_tx: kb(f[11])?,
        gpu_max: f[12].parse().ok()?,
        ..Default::default()
    })
}

fn parse_process(rest: &str) -> Option<ProcRecord> {
    let f: Vec<&str> = rest.splitn(9, ' ').collect();
    if f.len() < 9 {
        return None;
    }
    let kb = |s: &str| s.parse::<u64>().ok().map(|v| v * 1024);
    Some(ProcRecord {
        pid: f[0].parse().ok()?,
        cpu_avg: f[1].parse().ok()?,
        cpu_max: f[2].parse().ok()?,
        private: kb(f[3])?,
        io: kb(f[4])?,
        disk: kb(f[5])?,
        net: kb(f[6])?,
        gpu_avg: f[7].parse().ok()?,
        name: Arc::from(f[8]),
    })
}

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub dir: PathBuf,
    pub keep_days: u32,
}

/// Lives on the collector thread.
pub struct Recorder {
    open: Option<Accumulator>,
    out: Option<Sender<(String, String)>>,
    writer: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(config: RecorderConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let writer = thread::Builder::new()
            .name("anabolic-recorder".into())
            .spawn(move || write_files(config, rx))
            .ok();
        Self {
            open: None,
            out: writer.is_some().then_some(tx),
            writer,
        }
    }

    /// Adds a snapshot to the open bucket, closing it first when the
    /// snapshot belongs to the next one.
    pub fn observe(&mut self, snap: &Snapshot) {
        let dt = snap.interval.as_secs_f64();
        // The first sample has no rates yet.
        if snap.seq <= 1 || dt <= 0.0 || dt > MAX_GAP_SECS {
            return;
        }
        let now = unix_now();
        let start = now - now % BUCKET_SECS;
        if self.open.as_ref().is_some_and(|a| a.start != start) {
            self.flush();
        }
        self.open
            .get_or_insert_with(|| Accumulator::new(start))
            .add(snap, dt);
    }

    /// Sends the open bucket on, finished or not.
    pub fn flush(&mut self) {
        let Some(acc) = self.open.take() else {
            return;
        };
        if acc.secs <= 0.0 {
            return;
        }
        let bucket = acc.finish();
        let mut text = String::new();
        bucket.write(&mut text);
        if let Some(out) = &self.out {
            let _ = out.send((local_day(bucket.start).0, text));
        }
    }
}

impl Drop for Recorder {
    /// Writes what is left before the process goes away.
    fn drop(&mut self) {
        self.flush();
        self.out = None;
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

#[derive(Default)]
struct ProcAcc {
    pid: u32,
    name: Arc<str>,
    cpu_sum: f64,
    cpu_max: f32,
    private: u64,
    io: f64,
    disk: f64,
    net: f64,
    gpu_sum: f64,
}

#[derive(Default)]
struct Accumulator {
    start: u64,
    secs: f64,
    cpu_sum: f64,
    cpu_max: f32,
    memory_used: u64,
    memory_total: u64,
    commit: u64,
    disk_read: f64,
    disk_write: f64,
    disk_active: f32,
    net_rx: f64,
    net_tx: f64,
    gpu_max: f32,
    procs: HashMap<ProcKey, ProcAcc>,
    started: Vec<(u32, Arc<str>)>,
    exited: Vec<(u32, Arc<str>)>,
    hung: Vec<(u32, Arc<str>)>,
}

/// Groups by name, most frequent first, at most `MAX_EVENTS` names.
fn group(events: Vec<(u32, Arc<str>)>) -> Vec<Event> {
    let mut by: Vec<Event> = Vec::new();
    let mut index: HashMap<Arc<str>, usize> = HashMap::new();
    for (pid, name) in events {
        match index.get(&name) {
            Some(&i) => by[i].count += 1,
            None => {
                index.insert(name.clone(), by.len());
                by.push(Event {
                    pid,
                    count: 1,
                    name,
                });
            }
        }
    }
    by.sort_by_key(|e| std::cmp::Reverse(e.count));
    by.truncate(MAX_EVENTS);
    by
}

impl Accumulator {
    fn new(start: u64) -> Self {
        Self {
            start,
            ..Default::default()
        }
    }

    fn add(&mut self, snap: &Snapshot, dt: f64) {
        let s = &snap.system;
        self.secs += dt;
        self.cpu_sum += s.cpu.total as f64 * dt;
        self.cpu_max = self.cpu_max.max(s.cpu.total);
        self.memory_used = self.memory_used.max(s.memory.used());
        self.memory_total = s.memory.total;
        self.commit = self.commit.max(s.memory.commit);
        self.disk_read += s.disk_read_rate * dt;
        self.disk_write += s.disk_write_rate * dt;
        let busiest = s.disks.iter().map(|d| d.active).fold(0.0, f32::max);
        self.disk_active = self.disk_active.max(busiest);
        self.net_rx += s.net.rx_rate * dt;
        self.net_tx += s.net.tx_rate * dt;
        let gpu = s.gpus.iter().map(|g| g.usage).fold(0.0, f32::max);
        self.gpu_max = self.gpu_max.max(gpu);

        for p in &snap.processes {
            // The idle process is the absence of work.
            if p.pid == 0 {
                continue;
            }
            if p.state == ProcState::Exited {
                if p.exited_at == snap.seq {
                    self.exited.push((p.pid, p.name.clone()));
                }
                continue;
            }
            if p.first_seen == snap.seq {
                self.started.push((p.pid, p.name.clone()));
            }
            if p.not_responding {
                self.hung.push((p.pid, p.name.clone()));
            }
            let a = self.procs.entry(p.key).or_insert_with(|| ProcAcc {
                pid: p.pid,
                name: p.name.clone(),
                ..Default::default()
            });
            a.cpu_sum += p.cpu as f64 * dt;
            a.cpu_max = a.cpu_max.max(p.cpu);
            a.private = a.private.max(p.private_bytes);
            a.io += (p.io_read_rate + p.io_write_rate + p.io_other_rate) * dt;
            a.disk += (p.disk_read_rate + p.disk_write_rate) * dt;
            a.net += (p.net_rx_rate + p.net_tx_rate) * dt;
            a.gpu_sum += p.gpu as f64 * dt;
        }
    }

    fn finish(self) -> Bucket {
        let secs = self.secs;
        let mut procs: Vec<ProcRecord> = self
            .procs
            .into_values()
            .map(|a| ProcRecord {
                pid: a.pid,
                name: a.name,
                cpu_avg: (a.cpu_sum / secs) as f32,
                cpu_max: a.cpu_max,
                private: a.private,
                io: a.io as u64,
                disk: a.disk as u64,
                net: a.net as u64,
                gpu_avg: (a.gpu_sum / secs) as f32,
            })
            .collect();
        // The leaders of each category; small ones are noise, not leaders.
        let mut keep = vec![false; procs.len()];
        let mut lead = |key: &dyn Fn(&ProcRecord) -> f64, top: usize, floor: f64| {
            let mut order: Vec<usize> = (0..procs.len()).collect();
            order.sort_by(|&a, &b| key(&procs[b]).total_cmp(&key(&procs[a])));
            for &i in order.iter().take(top) {
                if key(&procs[i]) > floor {
                    keep[i] = true;
                }
            }
        };
        // Per bucket: CPU above 0.1 % of the machine (or a short burst),
        // 100 KB/s of I/O or disk, 25 KB/s of network, 1 % of a GPU.
        lead(&|p| p.cpu_avg.max(p.cpu_max / 4.0) as f64, TOP_CPU, 0.1);
        lead(&|p| p.private as f64, TOP, 0.0);
        lead(&|p| p.io as f64, TOP, MB);
        lead(&|p| p.disk as f64, TOP, MB);
        lead(&|p| p.net as f64, TOP, MB / 4.0);
        lead(&|p| p.gpu_avg as f64, TOP, 1.0);
        let mut i = 0;
        procs.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        procs.sort_by(|a, b| b.cpu_avg.total_cmp(&a.cpu_avg));
        Bucket {
            start: self.start,
            secs: secs as f32,
            cpu_avg: (self.cpu_sum / secs) as f32,
            cpu_max: self.cpu_max,
            memory_used: self.memory_used,
            memory_total: self.memory_total,
            commit: self.commit,
            disk_read: self.disk_read as u64,
            disk_write: self.disk_write as u64,
            disk_active: self.disk_active,
            net_rx: self.net_rx as u64,
            net_tx: self.net_tx as u64,
            gpu_max: self.gpu_max,
            processes: procs,
            started: group(self.started),
            exited: group(self.exited),
            hung: group(self.hung),
        }
    }
}

fn write_files(config: RecorderConfig, records: Receiver<(String, String)>) {
    let _ = fs::create_dir_all(&config.dir);
    let mut open: Option<(String, File)> = None;
    for (day, text) in records {
        if open.as_ref().is_none_or(|(d, _)| *d != day) {
            prune(&config.dir, config.keep_days);
            let path = config.dir.join(format!("{day}.txt"));
            let new = !path.exists();
            open = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
                .map(|mut f| {
                    if new {
                        let _ = writeln!(f, "{HEADER}");
                    }
                    (day, f)
                });
        }
        if let Some((_, f)) = &mut open {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// Deletes days older than the retention.
fn prune(dir: &Path, keep_days: u32) {
    let (cutoff, _) = local_day(unix_now().saturating_sub(keep_days as u64 * 86_400));
    for day in days(dir) {
        if day.date < cutoff {
            let _ = fs::remove_file(dir.join(format!("{}.txt", day.date)));
        }
    }
}

/// A recorded day on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    /// Local date, `YYYY-MM-DD`.
    pub date: String,
    pub bytes: u64,
}

/// Recorded days, newest first.
pub fn days(dir: &Path) -> Vec<Day> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut days: Vec<Day> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let date = name.strip_suffix(".txt")?;
            let is_date = date.len() == 10
                && date.chars().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == '-'
                    } else {
                        c.is_ascii_digit()
                    }
                });
            is_date.then(|| Day {
                date: date.to_string(),
                bytes: e.metadata().map_or(0, |m| m.len()),
            })
        })
        .collect();
    days.sort_by(|a, b| b.date.cmp(&a.date));
    days
}

/// One day's buckets, oldest first.
pub fn load(dir: &Path, date: &str) -> Result<Vec<Bucket>, String> {
    let text = fs::read_to_string(dir.join(format!("{date}.txt"))).map_err(|e| e.to_string())?;
    let mut buckets = parse(&text);
    buckets.sort_by_key(|b| b.start);
    Ok(buckets)
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The local date (`YYYY-MM-DD`) of a Unix time, and the seconds since
/// local midnight.
pub fn local_day(unix: u64) -> (String, u32) {
    let filetime = (unix as i64 + 11_644_473_600) * 10_000_000;
    // "YYYY-MM-DD HH:MM:SS"; UTC is still a usable day when the local
    // time cannot be had.
    let text = crate::sysinfo::local_time(filetime).unwrap_or_else(|| {
        let secs = unix % 86_400;
        let (y, m, d) = civil_from_days((unix / 86_400) as i64);
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        )
    });
    let (day, time) = text.split_at(10.min(text.len()));
    let mut parts = time
        .trim()
        .split(':')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let (h, m, s) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    (day.to_string(), h * 3600 + m * 60 + s)
}

/// Year, month and day of a count of days since 1970-01-01 (proleptic
/// Gregorian), after Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bucket {
        Bucket {
            start: 1_760_530_000,
            secs: 10.0,
            cpu_avg: 12.5,
            cpu_max: 40.25,
            memory_used: 8 << 30,
            memory_total: 32 << 30,
            commit: 12 << 30,
            disk_read: 5 << 20,
            disk_write: 1 << 20,
            disk_active: 99.5,
            net_rx: 300 << 10,
            net_tx: 20 << 10,
            gpu_max: 33.0,
            processes: vec![ProcRecord {
                pid: 1234,
                name: Arc::from("Some Game.exe"),
                cpu_avg: 9.5,
                cpu_max: 30.0,
                private: 2 << 30,
                io: 4 << 20,
                disk: 3 << 20,
                net: 100 << 10,
                gpu_avg: 25.0,
            }],
            started: vec![Event {
                pid: 77,
                count: 3,
                name: Arc::from("cmd.exe"),
            }],
            exited: vec![Event {
                pid: 78,
                count: 1,
                name: Arc::from("conhost.exe"),
            }],
            hung: vec![Event {
                pid: 1234,
                count: 7,
                name: Arc::from("Some Game.exe"),
            }],
        }
    }

    #[test]
    fn records_read_back() {
        let mut text = String::from(HEADER);
        text.push('\n');
        sample().write(&mut text);
        sample().write(&mut text);
        assert_eq!(parse(&text), vec![sample(), sample()]);
    }

    #[test]
    fn torn_lines_are_skipped() {
        let mut text = String::new();
        sample().write(&mut text);
        text.push_str("P 99 1.0\nB 17605\n");
        let parsed = parse(&text);
        assert_eq!(parsed, vec![sample()]);
    }

    #[test]
    fn events_group_by_name() {
        let names = ["bash.exe", "git.exe", "bash.exe", "bash.exe"];
        let events = group(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (i as u32, Arc::from(*n)))
                .collect(),
        );
        assert_eq!(events.len(), 2);
        assert_eq!(
            (events[0].pid, events[0].count, &*events[0].name),
            (0, 3, "bash.exe")
        );
    }

    #[test]
    fn leaders_are_kept_and_noise_is_not() {
        let mut acc = Accumulator::new(0);
        acc.secs = 10.0;
        for (i, cpu) in [50.0, 0.0, 0.0].into_iter().enumerate() {
            acc.procs.insert(
                ProcKey {
                    pid: i as u32,
                    create_time: 0,
                },
                ProcAcc {
                    pid: i as u32,
                    name: Arc::from(format!("p{i}")),
                    cpu_sum: cpu * 10.0,
                    cpu_max: cpu as f32,
                    private: 1024 * (i as u64 + 1),
                    ..Default::default()
                },
            );
        }
        let b = acc.finish();
        // All three lead in memory (top 5); only p0 in CPU, sorted first.
        assert_eq!(b.processes.len(), 3);
        assert_eq!(b.processes[0].pid, 0);
        assert!((b.processes[0].cpu_avg - 50.0).abs() < 1e-3);
    }
}
