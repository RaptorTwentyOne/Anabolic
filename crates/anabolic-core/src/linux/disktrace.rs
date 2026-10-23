// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Who reads and writes each physical disk: the block layer's tracepoints
//! (perf events, cap_perfmon) report every request as it goes to the disk
//! and as it completes, with the task that issued it. Matched up, they give
//! each process' traffic and response times per disk.
//!
//! The block layer knows sectors, not files: which file a request was for
//! is not known at that level, so the trace counts per process only.
//! Cached writes reach the disk later through the kernel's flusher threads,
//! which count under the kernel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::perf::{self, Attr, Field, KernelThreads, Record, Sample, Trace};

use super::*;

/// Whether the trace says which files a disk's traffic was for.
pub const NAMES_FILES: bool = false;

const PAGES: usize = 16;
const SAMPLE_TYPE: u64 = perf::SAMPLE_TID | perf::SAMPLE_TIME | perf::SAMPLE_RAW;
/// A request not completed within this is taken for lost.
const STALE_NS: u64 = 30_000_000_000;

/// A request on its way to the disk.
struct Issued {
    at: u64,
    disk: u32,
    tid: u32,
    bytes: u64,
    write: bool,
}

/// The tracepoints' ids and the fields read from them.
struct Events {
    issue: u64,
    complete: u64,
    dev: Field,
    sector: Field,
    nr_sector: Field,
    bytes: Option<Field>,
    rwbs: Field,
    comm: Field,
}

#[derive(Default)]
struct Shared {
    /// (disk, tid) -> traffic since the last take.
    io: HashMap<(u32, u32), IoTotals>,
    owners: HashMap<u32, u32>,
    images: HashMap<u32, Arc<str>>,
    lost: u64,
}

pub struct DiskTracer {
    shared: Arc<Mutex<Shared>>,
    _trace: Trace,
}

/// The kernel's device numbers (dev_t inside the kernel: 12 bits of major,
/// 20 of minor) of each disk and its partitions, by disk number.
fn device_numbers() -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    let parse = |text: &str| -> Option<u32> {
        let (major, minor) = text.trim().split_once(':')?;
        Some((major.parse::<u32>().ok()? << 20) | minor.parse::<u32>().ok()?)
    };
    for (number, name) in crate::disk::physical_disks().iter().enumerate() {
        let base = format!("/sys/block/{name}");
        if let Some(dev) = std::fs::read_to_string(format!("{base}/dev"))
            .ok()
            .and_then(|t| parse(&t))
        {
            out.insert(dev, number as u32);
        }
        let Ok(dir) = std::fs::read_dir(&base) else {
            continue;
        };
        for part in dir.flatten() {
            if let Some(dev) = std::fs::read_to_string(part.path().join("dev"))
                .ok()
                .and_then(|t| parse(&t))
            {
                out.insert(dev, number as u32);
            }
        }
    }
    out
}

/// What one event does to the requests under way and the totals.
fn on_event(
    events: &Events,
    disks: &HashMap<u32, u32>,
    issued: &mut HashMap<(u32, u64), Issued>,
    kernel: &mut KernelThreads,
    shared: &mut Shared,
    s: &Sample<'_>,
) {
    let Some(id) = perf::event_id(s.raw) else {
        return;
    };
    let (Some(dev), Some(sector)) = (events.dev.u32(s.raw), events.sector.u64(s.raw)) else {
        return;
    };
    if id == events.issue {
        let Some(&disk) = disks.get(&dev) else {
            return;
        };
        let rwbs = events.rwbs.str(s.raw).unwrap_or("");
        // Reads and writes; flushes and discards move no data.
        let write = rwbs.contains('W');
        if !write && !rwbs.contains('R') {
            return;
        }
        let bytes = events
            .bytes
            .and_then(|f| f.u32(s.raw))
            .filter(|&b| b > 0)
            .map_or_else(
                || events.nr_sector.u32(s.raw).unwrap_or(0) as u64 * 512,
                u64::from,
            );
        if s.tid != 0 {
            shared.owners.insert(s.tid, kernel.owner(s.pid));
            if s.pid == s.tid
                && let Some(comm) = events.comm.str(s.raw)
            {
                shared.images.insert(s.pid, Arc::from(comm));
            }
        }
        if issued.len() > 100_000 {
            // Completions went missing (events lost): start over.
            issued.retain(|_, r| s.time.saturating_sub(r.at) < STALE_NS);
        }
        issued.insert(
            (dev, sector),
            Issued {
                at: s.time,
                disk,
                tid: s.tid,
                bytes,
                write,
            },
        );
    } else if id == events.complete
        && let Some(r) = issued.remove(&(dev, sector))
    {
        let t = shared.io.entry((r.disk, r.tid)).or_default();
        if r.write {
            t.write += r.bytes;
        } else {
            t.read += r.bytes;
        }
        t.ops += 1;
        t.latency += s.time.saturating_sub(r.at) as f64 / 1e9;
    }
}

impl DiskTracer {
    pub fn start() -> Result<Self, String> {
        let issue = perf::tracepoint("block", "block_rq_issue")?;
        let complete = perf::tracepoint("block", "block_rq_complete")?;
        let events = Events {
            issue: issue.id,
            complete: complete.id,
            dev: issue.field("dev")?,
            sector: issue.field("sector")?,
            nr_sector: issue.field("nr_sector")?,
            // Since Linux 5.x; before that, sectors only.
            bytes: issue.field("bytes").ok(),
            rwbs: issue.field("rwbs")?,
            comm: issue.field("comm")?,
        };
        // Both events lay out dev and sector alike.
        if complete.field("dev")? != events.dev || complete.field("sector")? != events.sector {
            return Err("this kernel's block tracepoints are laid out unexpectedly".into());
        }
        let disks = device_numbers();
        if disks.is_empty() {
            return Err("no physical disk to trace".into());
        }
        let ids = [issue.id, complete.id];
        let shared = Arc::new(Mutex::new(Shared::default()));
        let sink = shared.clone();
        let mut issued: HashMap<(u32, u64), Issued> = HashMap::new();
        let mut kernel = KernelThreads::default();
        let trace = Trace::start(
            "disks",
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
                let Ok(mut shared) = sink.lock() else {
                    return;
                };
                match record {
                    Record::Sample(body) => {
                        if let Some(s) = Sample::parse(body, SAMPLE_TYPE) {
                            on_event(&events, &disks, &mut issued, &mut kernel, &mut shared, &s);
                        }
                    }
                    Record::Lost(n) => shared.lost += n,
                    _ => {}
                }
            },
        )?;
        Ok(Self {
            shared,
            _trace: trace,
        })
    }

    /// Events lost (the reader fell behind), and buffers (none here).
    pub fn lost(&self) -> (u32, u32) {
        let lost = self.shared.lock().map_or(0, |s| s.lost);
        (lost.min(u32::MAX as u64) as u32, 0)
    }

    pub fn take(&self) -> DiskSample {
        let Ok(mut s) = self.shared.lock() else {
            return DiskSample::default();
        };
        let records = std::mem::take(&mut s.io)
            .into_iter()
            .map(|((disk, tid), totals)| IoRecord {
                disk,
                tid,
                file_object: 0,
                file: None,
                totals,
            })
            .collect();
        DiskSample {
            records,
            owners: std::mem::take(&mut s.owners),
            images: std::mem::take(&mut s.images),
            late_names: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(offset: usize, size: usize) -> Field {
        Field {
            offset,
            size,
            data_loc: false,
        }
    }

    /// block_rq_issue as Linux 6.x lays it out.
    fn raw(id: u16, dev: u32, sector: u64, bytes: u32, rwbs: &[u8], comm: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0..2].copy_from_slice(&id.to_ne_bytes());
        r[8..12].copy_from_slice(&dev.to_ne_bytes());
        r[16..24].copy_from_slice(&sector.to_ne_bytes());
        r[24..28].copy_from_slice(&(bytes / 512).to_ne_bytes());
        r[28..32].copy_from_slice(&bytes.to_ne_bytes());
        r[36..36 + rwbs.len()].copy_from_slice(rwbs);
        r[44..44 + comm.len()].copy_from_slice(comm);
        r
    }

    #[test]
    fn pairs_requests_with_their_completions() {
        let events = Events {
            issue: 1,
            complete: 2,
            dev: field(8, 4),
            sector: field(16, 8),
            nr_sector: field(24, 4),
            bytes: Some(field(28, 4)),
            rwbs: field(36, 8),
            comm: field(44, 16),
        };
        let nvme = (259 << 20) | 5;
        let disks = HashMap::from([(nvme, 0u32)]);
        let (mut issued, mut kernel, mut shared) =
            (HashMap::new(), KernelThreads::default(), Shared::default());
        let me = std::process::id();
        let mut feed = |raw: Vec<u8>, time: u64| {
            let s = Sample {
                pid: me,
                tid: me,
                time,
                cpu: 0,
                raw: &raw,
            };
            on_event(&events, &disks, &mut issued, &mut kernel, &mut shared, &s);
        };
        feed(raw(1, nvme, 100, 4096, b"R", b"cat"), 1_000_000);
        feed(raw(1, nvme, 200, 8192, b"WS", b"cat"), 1_100_000);
        // A flush moves no data; another device is not a disk of ours.
        feed(raw(1, nvme, 0, 0, b"FF", b"cat"), 1_100_000);
        feed(raw(1, 7 << 20, 300, 4096, b"R", b"cat"), 1_100_000);
        feed(raw(2, nvme, 100, 0, b"R", b""), 1_500_000);
        feed(raw(2, nvme, 200, 0, b"WS", b""), 3_100_000);
        let t = shared.io[&(0, me)];
        assert_eq!((t.read, t.write, t.ops), (4096, 8192, 2));
        assert!((t.latency - 0.0025).abs() < 1e-9);
        assert!(issued.is_empty());
        assert_eq!(shared.owners[&me], me);
        assert_eq!(&*shared.images[&me], "cat");
    }

    #[test]
    fn numbers_disks_like_the_sampler() {
        let numbers = device_numbers();
        let disks = crate::disk::physical_disks();
        if !disks.is_empty() {
            assert!(numbers.values().any(|&n| n == 0));
            assert!(numbers.values().all(|&n| (n as usize) < disks.len()));
        }
    }
}
