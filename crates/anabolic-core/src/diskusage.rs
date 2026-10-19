//! Turns raw disk trace records into per-disk process and file tables,
//! smoothed so a burst stays readable for a few seconds and idle rows linger
//! for a minute before they go.

use std::collections::HashMap;
use std::sync::Arc;

use crate::disktrace::{DiskSample, IoTotals};
use crate::model::{DiskActivity, DiskFile, DiskUse, DiskUser};
use crate::util::DeviceMap;

/// Seconds over which rates are averaged.
const SMOOTHING: f64 = 4.0;
/// Refreshes an idle row stays listed.
const LINGER: u64 = 60;
const MAX_PROCESSES: usize = 40;
const MAX_FILES: usize = 80;
/// Shown for a file object the kernel never named to us.
pub const UNNAMED: &str = "(unnamed file)";
/// Drive letters can come and go.
const DEVICES_EVERY: u64 = 60;

#[derive(Default)]
struct Acc {
    read_rate: f64,
    write_rate: f64,
    response_ms: f32,
    read_total: u64,
    write_total: u64,
    /// Refresh of the last request; 0 before the first.
    last: u64,
    // This refresh so far.
    read: u64,
    write: u64,
    ops: u32,
    latency: f64,
}

impl Acc {
    fn add(&mut self, t: &IoTotals) {
        self.read += t.read;
        self.write += t.write;
        self.ops += t.ops;
        self.latency += t.latency;
    }

    fn settle(&mut self, secs: f64, alpha: f64, seq: u64) {
        let smooth = |rate: &mut f64, bytes: u64| {
            *rate += alpha * (bytes as f64 / secs - *rate);
            // Let a decayed tail reach zero so idle rows read as idle.
            if *rate < 1.0 {
                *rate = 0.0;
            }
        };
        smooth(&mut self.read_rate, self.read);
        smooth(&mut self.write_rate, self.write);
        if self.ops > 0 {
            let average = (self.latency / self.ops as f64 * 1000.0) as f32;
            self.response_ms = if self.last == 0 {
                average
            } else {
                self.response_ms + 0.5 * (average - self.response_ms)
            };
            self.last = seq;
        }
        self.read_total += self.read;
        self.write_total += self.write;
        self.read = 0;
        self.write = 0;
        self.ops = 0;
        self.latency = 0.0;
    }

    fn usage(&self) -> DiskUse {
        DiskUse {
            read_rate: self.read_rate,
            write_rate: self.write_rate,
            response_ms: self.response_ms,
            read_total: self.read_total,
            write_total: self.write_total,
        }
    }
}

fn merge(a: &mut DiskUse, b: &DiskUse) {
    a.read_rate += b.read_rate;
    a.write_rate += b.write_rate;
    a.response_ms = a.response_ms.max(b.response_ms);
    a.read_total += b.read_total;
    a.write_total += b.write_total;
}

fn busiest(a: &DiskUse, b: &DiskUse) -> std::cmp::Ordering {
    b.rate()
        .partial_cmp(&a.rate())
        .unwrap_or(std::cmp::Ordering::Equal)
        .then((b.read_total + b.write_total).cmp(&(a.read_total + a.write_total)))
}

fn slot(disks: &mut HashMap<u32, DiskActivity>, disk: u32) -> &mut DiskActivity {
    disks.entry(disk).or_insert_with(|| DiskActivity {
        disk,
        ..Default::default()
    })
}

struct FileAcc {
    /// NT path, once known.
    name: Option<Arc<str>>,
    acc: Acc,
}

#[derive(Default)]
pub struct DiskUsage {
    /// (disk, pid)
    processes: HashMap<(u32, u32), Acc>,
    /// (disk, pid, file object)
    files: HashMap<(u32, u32, u64), FileAcc>,
    /// pid -> last known name, for processes that have exited since.
    names: HashMap<u32, Arc<str>>,
    /// NT path -> what is shown.
    shown: HashMap<Arc<str>, Arc<str>>,
    devices: DeviceMap,
    devices_at: Option<u64>,
}

impl DiskUsage {
    /// Folds in one refresh of records. `owner` maps a thread to its process,
    /// `name` a live process to its name.
    pub fn update(
        &mut self,
        sample: DiskSample,
        owner: impl Fn(u32) -> Option<u32>,
        name: impl Fn(u32) -> Option<Arc<str>>,
        seq: u64,
        secs: f64,
    ) -> Vec<DiskActivity> {
        if self.devices_at.is_none_or(|at| seq - at >= DEVICES_EVERY) {
            self.devices = DeviceMap::build();
            self.shown.clear();
            self.devices_at = Some(seq);
        }
        self.names.extend(sample.images);
        if !sample.late_names.is_empty() {
            let late: HashMap<u64, Arc<str>> = sample.late_names.into_iter().collect();
            for ((_, _, object), f) in &mut self.files {
                if f.name.is_none() {
                    f.name = late.get(object).cloned();
                }
            }
        }
        for r in &sample.records {
            let pid = owner(r.tid).unwrap_or(u32::MAX);
            self.processes
                .entry((r.disk, pid))
                .or_default()
                .add(&r.totals);
            // A trace that knows no files would list one "no file" row per
            // process.
            if !crate::disktrace::NAMES_FILES {
                continue;
            }
            let f = self
                .files
                .entry((r.disk, pid, r.file_object))
                .or_insert_with(|| FileAcc {
                    name: None,
                    acc: Acc::default(),
                });
            if r.file.is_some() {
                f.name = r.file.clone();
            }
            f.acc.add(&r.totals);
        }

        let alpha = 1.0 - (-secs / SMOOTHING).exp();
        for acc in self.processes.values_mut() {
            acc.settle(secs, alpha, seq);
        }
        for f in self.files.values_mut() {
            f.acc.settle(secs, alpha, seq);
        }
        self.processes.retain(|_, a| seq - a.last < LINGER);
        self.files.retain(|_, f| seq - f.acc.last < LINGER);

        let unknown: Arc<str> = Arc::from("<unknown thread>");
        let mut process_name = |pid: u32| -> Arc<str> {
            if pid == u32::MAX {
                return unknown.clone();
            }
            match name(pid) {
                Some(n) => {
                    self.names.insert(pid, n.clone());
                    n
                }
                None => self
                    .names
                    .get(&pid)
                    .cloned()
                    .unwrap_or_else(|| Arc::from(format!("<exited {pid}>"))),
            }
        };

        let mut disks: HashMap<u32, DiskActivity> = HashMap::new();
        for (&(disk, pid), acc) in &self.processes {
            slot(&mut disks, disk).processes.push(DiskUser {
                pid,
                name: process_name(pid),
                io: acc.usage(),
            });
        }

        // Several file objects can stand for one file: group by path.
        let no_object: Arc<str> = Arc::from("(no file: file system or driver I/O)");
        let unnamed: Arc<str> = Arc::from(UNNAMED);
        let mut grouped: HashMap<(u32, u32, Arc<str>), DiskUse> = HashMap::new();
        for (&(disk, pid, object), f) in &self.files {
            let path = match (&f.name, object) {
                (_, 0) => no_object.clone(),
                (None, _) => unnamed.clone(),
                (Some(nt), _) => match self.shown.get(nt) {
                    Some(p) => p.clone(),
                    None => {
                        let p: Arc<str> = self
                            .devices
                            .to_dos_path(nt)
                            .map_or_else(|| nt.clone(), Arc::from);
                        self.shown.insert(nt.clone(), p.clone());
                        p
                    }
                },
            };
            merge(
                grouped.entry((disk, pid, path)).or_default(),
                &f.acc.usage(),
            );
        }
        for ((disk, pid, path), io) in grouped {
            slot(&mut disks, disk).files.push(DiskFile {
                path,
                pid,
                process: process_name(pid),
                io,
            });
        }

        let mut out: Vec<DiskActivity> = disks.into_values().collect();
        for a in &mut out {
            a.processes.sort_by(|x, y| busiest(&x.io, &y.io));
            a.processes.truncate(MAX_PROCESSES);
            a.files.sort_by(|x, y| busiest(&x.io, &y.io));
            a.files.truncate(MAX_FILES);
        }
        out.sort_by_key(|a| a.disk);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disktrace::IoRecord;

    fn record(disk: u32, tid: u32, object: u64, file: Option<&str>, write: u64) -> IoRecord {
        IoRecord {
            disk,
            tid,
            file_object: object,
            file: file.map(Arc::from),
            totals: IoTotals {
                read: 0,
                write,
                ops: 1,
                latency: 0.002,
            },
        }
    }

    #[test]
    fn groups_by_disk_process_and_file_and_names_late() {
        let mut usage = DiskUsage::default();
        let owner = |tid: u32| match tid {
            1 => Some(100),
            2 => Some(200),
            _ => None,
        };
        let name = |pid: u32| Some(Arc::from(format!("p{pid}.exe")));
        let sample = DiskSample {
            records: vec![
                record(0, 1, 0xA, Some(r"\Device\Nowhere\a.bin"), 1000),
                record(0, 1, 0xB, Some(r"\Device\Nowhere\a.bin"), 500),
                record(0, 2, 0xC, None, 300),
                record(1, 9, 0, None, 50),
            ],
            ..Default::default()
        };
        let out = usage.update(sample, owner, name, 1, 1.0);
        assert_eq!(out.len(), 2);
        let d0 = &out[0];
        assert_eq!(d0.disk, 0);
        assert_eq!(&*d0.processes[0].name, "p100.exe");
        assert_eq!(d0.processes[0].io.write_total, 1500);
        assert_eq!(&*out[1].processes[0].name, "<unknown thread>");
        if !crate::disktrace::NAMES_FILES {
            // Traffic per process only: no file rows at all.
            assert!(out.iter().all(|d| d.files.is_empty()));
            return;
        }
        // Two file objects for one path make one row.
        let a = d0.files.iter().find(|f| f.path.ends_with("a.bin")).unwrap();
        assert_eq!(a.io.write_total, 1500);
        assert!(d0.files.iter().any(|f| f.pid == 200 && &*f.path == UNNAMED));

        // A rundown names the file afterwards.
        let sample = DiskSample {
            late_names: vec![(0xC, Arc::from(r"\Device\Nowhere\b.bin"))],
            ..Default::default()
        };
        let out = usage.update(sample, owner, name, 2, 1.0);
        let d0 = &out[0];
        assert!(
            d0.files
                .iter()
                .any(|f| f.pid == 200 && f.path.ends_with("b.bin"))
        );
        // Rates decay but rows linger.
        assert!(d0.processes[0].io.write_rate < 1500.0);
    }
}
