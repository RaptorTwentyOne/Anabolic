// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A disk speed test in the manner of CrystalDiskMark: a test file
//! written once, then read and written past the system's cache
//! (unbuffered) in large blocks one after another and in
//! small blocks at random places, with one request at a time and with
//! many at once, each for a few seconds. The file is the test's only
//! trace: it goes when the test ends, or when Anabolic does (Windows
//! deletes it on close).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg_attr(windows, path = "windows/diskbench.rs")]
#[cfg_attr(target_os = "linux", path = "linux/diskbench.rs")]
mod imp;

const MB: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    /// 1 MB blocks one after another, eight in flight.
    Seq8,
    Seq1,
    /// 4 KB blocks at random places, 32 in flight.
    Rnd32,
    Rnd1,
}

impl Test {
    pub const ALL: [Test; 4] = [Test::Seq8, Test::Seq1, Test::Rnd32, Test::Rnd1];

    /// CrystalDiskMark's names for the same tests.
    pub fn label(self) -> &'static str {
        match self {
            Test::Seq8 => "SEQ1M Q8",
            Test::Seq1 => "SEQ1M Q1",
            Test::Rnd32 => "RND4K Q32",
            Test::Rnd1 => "RND4K Q1",
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Test::Seq8 => "Large files, many requests at once",
            Test::Seq1 => "Large files, one request at a time",
            Test::Rnd32 => "Small pieces, many at once",
            Test::Rnd1 => "Small pieces, one at a time",
        }
    }

    fn block(self) -> usize {
        match self {
            Test::Seq8 | Test::Seq1 => MB as usize,
            Test::Rnd32 | Test::Rnd1 => 4096,
        }
    }

    fn depth(self) -> usize {
        match self {
            Test::Seq8 => 8,
            Test::Seq1 | Test::Rnd1 => 1,
            Test::Rnd32 => 32,
        }
    }

    fn random(self) -> bool {
        matches!(self, Test::Rnd32 | Test::Rnd1)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Speed {
    pub bytes_per_sec: f64,
    pub iops: f64,
    /// The middle request's time, microseconds.
    pub latency_us: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub test: Test,
    pub read: Option<Speed>,
    pub write: Option<Speed>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub stage: String,
    pub fraction: f32,
    pub rows: Vec<Row>,
    pub done: bool,
    pub error: Option<String>,
}

/// Where to put the test file on a volume ("D:"): its root, or the temp
/// folder on the system's drive, where the root needs an administrator.
pub fn place(volume: &str) -> PathBuf {
    let temp = std::env::temp_dir();
    let on_temp = temp
        .to_string_lossy()
        .to_uppercase()
        .starts_with(&volume.to_uppercase());
    let dir = if on_temp || volume.is_empty() || volume == "/" {
        temp
    } else if cfg!(windows) {
        PathBuf::from(format!("{volume}\\"))
    } else {
        PathBuf::from(volume)
    };
    dir.join("anabolic-speed-test.tmp")
}

/// A buffer aligned to 4 KB, as unbuffered I/O needs.
struct Aligned {
    raw: Vec<u8>,
    at: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize, seed: u64) -> Aligned {
        let mut raw = vec![0u8; len + 4096];
        let at = raw.as_ptr().align_offset(4096);
        // Data that does not compress: a controller that compresses
        // would make zeros look fast.
        let mut x = seed | 1;
        for b in raw.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        Aligned { raw, at, len }
    }

    fn get(&mut self) -> &mut [u8] {
        &mut self.raw[self.at..self.at + self.len]
    }
}

fn set(progress: &Mutex<Progress>, f: impl FnOnce(&mut Progress)) {
    if let Ok(mut p) = progress.lock() {
        f(&mut p);
    }
}

/// Runs every test on a file of `size` bytes at `path`, `seconds` each way,
/// reporting into `progress`; `stop` ends it early.
pub fn run(
    path: &Path,
    size: u64,
    seconds: f32,
    progress: &Mutex<Progress>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let size = size / MB * MB;
    let first = imp::create(path, size)?;
    // The rest of the handles, one per request in flight, opened before
    // anything can remove the file.
    let mut handles = vec![first];
    for _ in 1..32 {
        handles.push(imp::open(path)?);
    }
    imp::forget(path);
    let handles: Vec<Arc<File>> = handles.into_iter().map(Arc::new).collect();

    // Write it once, so reads read real data.
    let mut buf = Aligned::new(MB as usize, 7);
    let mut at = 0;
    while at < size {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        imp::write_at(&handles[0], buf.get(), at).map_err(|e| e.to_string())?;
        at += MB;
        set(progress, |p| {
            p.stage = format!("Writing the test file ({} of {} MB)", at / MB, size / MB);
            p.fraction = at as f32 / size as f32 * 0.1;
        });
    }
    let steps = (Test::ALL.len() * 2) as f32;
    for (i, test) in Test::ALL.into_iter().enumerate() {
        let mut row = Row {
            test,
            read: None,
            write: None,
        };
        for (j, write) in [false, true].into_iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            set(progress, |p| {
                p.stage = format!(
                    "{} {}",
                    if write { "Writing" } else { "Reading" },
                    test.label()
                );
                p.fraction = 0.1 + 0.9 * (i * 2 + j) as f32 / steps;
            });
            let speed = measure(&handles, test, write, size, seconds, stop)?;
            if write {
                row.write = Some(speed);
            } else {
                row.read = Some(speed);
            }
            let shown = row.clone();
            set(progress, |p| {
                match p.rows.iter_mut().find(|r| r.test == test) {
                    Some(r) => *r = shown,
                    None => p.rows.push(shown),
                }
            });
        }
    }
    set(progress, |p| {
        p.stage = "Done".into();
        p.fraction = 1.0;
    });
    Ok(())
}

fn measure(
    handles: &[Arc<File>],
    test: Test,
    write: bool,
    size: u64,
    seconds: f32,
    stop: &AtomicBool,
) -> Result<Speed, String> {
    let blocks = size / test.block() as u64;
    let next = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicU64::new(0));
    let until = Instant::now() + Duration::from_secs_f32(seconds);
    let error: Arc<Mutex<Option<String>>> = Arc::default();
    let started = Instant::now();
    let mut latencies = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..test.depth())
            .map(|k| {
                let file = handles[k].clone();
                let (next, done, error) = (next.clone(), done.clone(), error.clone());
                scope.spawn(move || {
                    let mut buf = Aligned::new(test.block(), k as u64 + 11);
                    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ k as u64;
                    let mut times = Vec::new();
                    while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                        let block = if test.random() {
                            x ^= x << 13;
                            x ^= x >> 7;
                            x ^= x << 17;
                            x % blocks
                        } else {
                            next.fetch_add(1, Ordering::Relaxed) % blocks
                        };
                        let offset = block * test.block() as u64;
                        let t = Instant::now();
                        let result = if write {
                            imp::write_at(&file, buf.get(), offset)
                        } else {
                            imp::read_at(&file, buf.get(), offset)
                        };
                        if let Err(e) = result {
                            if let Ok(mut slot) = error.lock() {
                                *slot = Some(e.to_string());
                            }
                            break;
                        }
                        times.push(t.elapsed().as_micros() as u32);
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                    times
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect::<Vec<u32>>()
    });
    if let Some(e) = error.lock().ok().and_then(|e| e.clone()) {
        return Err(e);
    }
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let n = done.load(Ordering::Relaxed) as f64;
    latencies.sort_unstable();
    Ok(Speed {
        bytes_per_sec: n * test.block() as f64 / secs,
        iops: n / secs,
        latency_us: latencies.get(latencies.len() / 2).copied().unwrap_or(0) as f64,
    })
}

/// Starts a test on a thread of its own; the progress and the stop flag
/// stay with the caller.
pub fn start(path: PathBuf, size: u64, seconds: f32) -> (Arc<Mutex<Progress>>, Arc<AtomicBool>) {
    let progress: Arc<Mutex<Progress>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let (p, s) = (progress.clone(), stop.clone());
    let _ = std::thread::Builder::new()
        .name("disk-speed".into())
        .spawn(move || {
            let result = run(&path, size, seconds, &p, &s);
            imp::forget(&path);
            set(&p, |p| {
                p.done = true;
                if let Err(e) = result {
                    p.error = Some(e);
                }
            });
        });
    (progress, stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_a_small_file() {
        let path =
            std::env::temp_dir().join(format!("anabolic-speed-{}.tmp", crate::random_hex(4)));
        let progress = Mutex::new(Progress::default());
        run(&path, 64 * MB, 0.2, &progress, &AtomicBool::new(false)).unwrap();
        let p = progress.lock().unwrap();
        assert_eq!(p.rows.len(), 4);
        for r in &p.rows {
            let (read, write) = (r.read.unwrap(), r.write.unwrap());
            assert!(read.bytes_per_sec > 0.0 && write.iops > 0.0, "{r:?}");
            println!(
                "{:<10} read {:>8.0} MB/s {:>8.0} IOPS   write {:>8.0} MB/s {:>8.0} IOPS",
                r.test.label(),
                read.bytes_per_sec / MB as f64,
                read.iops,
                write.bytes_per_sec / MB as f64,
                write.iops
            );
        }
        drop(p);
        // The file goes with its handles.
        assert!(!path.exists());
    }
}
