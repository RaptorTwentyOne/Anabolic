//! Who reads and writes which disk, and which file: kernel disk I/O events
//! (ETW, admin) joined with file-name events. Windows' per-process disk
//! counters say how much, never where; this says both, like Resource Monitor.
//! Runs only while a disk view is open.
//!
//! The kernel names a file object when it is created, so files opened before
//! tracing started arrive unnamed. For those a helper thread now and then
//! starts and stops a second session: on stop the kernel lists every open
//! file object (half a million on a busy machine), and only the ones we are
//! missing are kept.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::System::Diagnostics::Etw::{
    DiskIoGuid, EVENT_TRACE_FLAG_DISK_FILE_IO, EVENT_TRACE_FLAG_DISK_IO, EVENT_TRACE_FLAG_PROCESS,
    EVENT_TRACE_FLAG_THREAD, EVENT_TRACE_SYSTEM_LOGGER_MODE, FileIoGuid, ProcessGuid, ThreadGuid,
};
use windows::Win32::System::Performance::QueryPerformanceFrequency;

use crate::etw::{
    self, OPCODE_PROCESS_RUNDOWN, OPCODE_PROCESS_START, Session, process_image, u32_at, u64_at,
};
use crate::hardening::set_current_thread_priority;

use super::*;

/// Whether the trace says which files a disk's traffic was for.
pub const NAMES_FILES: bool = true;

const OPCODE_READ: u8 = 10;
const OPCODE_WRITE: u8 = 11;
const OPCODE_FILE_NAME: u8 = 0;
const OPCODE_FILE_CREATE: u8 = 32;
const OPCODE_FILE_DELETE: u8 = 35;
const OPCODE_FILE_RUNDOWN: u8 = 36;
const OPCODE_THREAD_START: u8 = 1;
const OPCODE_THREAD_RUNDOWN: u8 = 3;
/// Names kept before starting over; the rundown recovers what is still used.
const MAX_NAMES: usize = 200_000;
/// Unnamed file objects a rundown could not name, not asked about again.
const MAX_TRIED: usize = 100_000;
/// Rundowns cost about a second of kernel time; never more often than this.
const RUNDOWN_EVERY: Duration = Duration::from_secs(20);
/// Unnamed file objects are collected this long before a rundown, so the
/// burst of I/O right after tracing starts is named by one rundown.
const RUNDOWN_BATCH: Duration = Duration::from_millis(1500);

/// (disk, issuing thread, file object)
type IoKey = (u32, u32, u64);

#[derive(Default)]
struct Shared {
    /// The name is taken when the I/O happens: by the next `take` the file
    /// may be closed and its object reused.
    io: HashMap<IoKey, (IoTotals, Option<Arc<str>>)>,
    names: HashMap<u64, Arc<str>>,
    /// File objects freed since the last `take`; their names go then, once
    /// the I/O they did has been named.
    freed: Vec<u64>,
    owners: HashMap<u32, u32>,
    images: HashMap<u32, Arc<str>>,
    /// File objects that did disk I/O without a name yet.
    unnamed: HashSet<u64>,
    tried: HashSet<u64>,
    late: Vec<(u64, Arc<str>)>,
}

pub struct DiskTracer {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    _session: Session,
}

/// NUL-terminated UTF-16 starting at `at`.
fn wide_at(d: &[u8], at: usize) -> Option<String> {
    let units: Vec<u16> = d
        .get(at..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// FileIo_Name: FileObject, FileName.
fn file_name(d: &[u8]) -> Option<(u64, String)> {
    let object = u64_at(d, 0)?;
    let name = wide_at(d, 8)?;
    (!name.is_empty()).then_some((object, name))
}

impl DiskTracer {
    pub fn start() -> Result<Self, String> {
        let flags = EVENT_TRACE_FLAG_DISK_IO.0
            | EVENT_TRACE_FLAG_DISK_FILE_IO.0
            | EVENT_TRACE_FLAG_THREAD.0
            | EVENT_TRACE_FLAG_PROCESS.0;
        let session = Session::start("Anabolic-Disk", EVENT_TRACE_SYSTEM_LOGGER_MODE, flags)?;
        let mut frequency = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut frequency);
        }
        let ticks_per_second = frequency.max(1) as f64;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let sink = shared.clone();
        session.consume(move |record| {
            let header = &record.EventHeader;
            let opcode = header.EventDescriptor.Opcode;
            let d = etw::user_data(record);
            if header.ProviderId == DiskIoGuid && (opcode == OPCODE_READ || opcode == OPCODE_WRITE)
            {
                // DiskNumber, IrpFlags, TransferSize, Reserved, ByteOffset,
                // FileObject, Irp, HighResResponseTime, IssuingThreadId.
                let (Some(disk), Some(size), Some(file)) =
                    (u32_at(d, 0), u32_at(d, 8), u64_at(d, 24))
                else {
                    return;
                };
                let response = u64_at(d, 40).unwrap_or(0) as f64 / ticks_per_second;
                let tid = u32_at(d, 48).unwrap_or(header.ThreadId);
                let Ok(mut s) = sink.lock() else { return };
                let name = s.names.get(&file).cloned();
                if file != 0 && name.is_none() && !s.tried.contains(&file) {
                    s.unnamed.insert(file);
                }
                let (t, named) = s.io.entry((disk, tid, file)).or_default();
                if named.is_none() {
                    *named = name;
                }
                if opcode == OPCODE_READ {
                    t.read += size as u64;
                } else {
                    t.write += size as u64;
                }
                t.ops += 1;
                t.latency += response;
            } else if header.ProviderId == FileIoGuid {
                match opcode {
                    OPCODE_FILE_NAME | OPCODE_FILE_CREATE | OPCODE_FILE_RUNDOWN => {
                        if let Some((object, name)) = file_name(d)
                            && let Ok(mut s) = sink.lock()
                        {
                            if s.names.len() >= MAX_NAMES {
                                s.names.clear();
                            }
                            s.names.insert(object, Arc::from(name));
                            // Reused before the last name was dropped.
                            s.freed.retain(|&o| o != object);
                        }
                    }
                    // The file object is gone; its address will be reused.
                    OPCODE_FILE_DELETE => {
                        if let Some(object) = u64_at(d, 0)
                            && let Ok(mut s) = sink.lock()
                        {
                            s.freed.push(object);
                            s.tried.remove(&object);
                        }
                    }
                    _ => {}
                }
            } else if header.ProviderId == ThreadGuid
                && matches!(opcode, OPCODE_THREAD_START | OPCODE_THREAD_RUNDOWN)
            {
                // ProcessId, TThreadId, ...
                if let (Some(pid), Some(tid)) = (u32_at(d, 0), u32_at(d, 4))
                    && let Ok(mut s) = sink.lock()
                {
                    s.owners.insert(tid, pid);
                }
            } else if header.ProviderId == ProcessGuid
                && matches!(opcode, OPCODE_PROCESS_START | OPCODE_PROCESS_RUNDOWN)
                && let Some((pid, image)) = process_image(d)
                && let Ok(mut s) = sink.lock()
            {
                s.images.insert(pid, Arc::from(image));
            }
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let (helper_shared, helper_stop) = (shared.clone(), stop.clone());
        thread::Builder::new()
            .name("anabolic-disk-names".into())
            .spawn(move || name_helper(&helper_shared, &helper_stop))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            stop,
            _session: session,
        })
    }

    /// Events the kernel dropped because we fell behind.
    pub fn lost(&self) -> (u32, u32) {
        self._session.lost()
    }

    pub fn take(&self) -> DiskSample {
        let Ok(mut s) = self.shared.lock() else {
            return DiskSample::default();
        };
        let io = std::mem::take(&mut s.io);
        let records = io
            .into_iter()
            .map(|((disk, tid, file_object), (totals, name))| IoRecord {
                disk,
                tid,
                file_object,
                // Named after the I/O (a name event came later)?
                file: name.or_else(|| s.names.get(&file_object).cloned()),
                totals,
            })
            .collect();
        for object in std::mem::take(&mut s.freed) {
            s.names.remove(&object);
        }
        DiskSample {
            records,
            owners: std::mem::take(&mut s.owners),
            images: std::mem::take(&mut s.images),
            late_names: std::mem::take(&mut s.late),
        }
    }
}

impl Drop for DiskTracer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Names file objects opened before tracing started, by rundown.
fn name_helper(shared: &Mutex<Shared>, stop: &AtomicBool) {
    set_current_thread_priority(crate::hardening::ThreadPriority::BelowNormal);
    let mut last: Option<Instant> = None;
    let mut pending: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(250));
        if pending.is_none() && shared.lock().is_ok_and(|s| !s.unnamed.is_empty()) {
            pending = Some(Instant::now());
        }
        let due = pending.is_some_and(|t| t.elapsed() >= RUNDOWN_BATCH)
            && last.is_none_or(|t| t.elapsed() >= RUNDOWN_EVERY);
        if !due {
            continue;
        }
        let wanted = match shared.lock() {
            Ok(mut s) => std::mem::take(&mut s.unnamed),
            Err(_) => return,
        };
        pending = None;
        last = Some(Instant::now());
        let found = match rundown(&wanted) {
            Ok(found) => found,
            Err(e) => {
                eprintln!("anabolic: file name rundown failed: {e}");
                continue;
            }
        };
        let Ok(mut s) = shared.lock() else { return };
        for object in &wanted {
            if !found.contains_key(object) {
                if s.tried.len() >= MAX_TRIED {
                    s.tried.clear();
                }
                s.tried.insert(*object);
            }
        }
        for (object, name) in found {
            s.names.insert(object, name.clone());
            s.late.push((object, name));
        }
    }
}

/// Starts and stops a kernel session only for the file rundown it logs on
/// stop. Returns the names of the `wanted` file objects. Blocks until the
/// rundown has been delivered (about a second).
fn rundown(wanted: &HashSet<u64>) -> Result<HashMap<u64, Arc<str>>, String> {
    let session = Session::start(
        "Anabolic-DiskNames",
        EVENT_TRACE_SYSTEM_LOGGER_MODE,
        EVENT_TRACE_FLAG_DISK_FILE_IO.0 | EVENT_TRACE_FLAG_DISK_IO.0,
    )?;
    let found = Arc::new(Mutex::new(HashMap::new()));
    let sink = found.clone();
    let wanted = wanted.clone();
    let consumer = session.consume(move |record| {
        let header = &record.EventHeader;
        if header.ProviderId != FileIoGuid || header.EventDescriptor.Opcode != OPCODE_FILE_RUNDOWN {
            return;
        }
        let d = etw::user_data(record);
        if u64_at(d, 0).is_some_and(|o| wanted.contains(&o))
            && let Some((object, name)) = file_name(d)
            && let Ok(mut f) = sink.lock()
        {
            f.insert(object, Arc::from(name));
        }
    })?;
    drop(session);
    let _ = consumer.join();
    Ok(std::mem::take(
        &mut *found.lock().map_err(|e| e.to_string())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Takes samples until `done` holds for everything seen so far, or the
    /// time runs out: events arrive with the session's flush, later on a
    /// loaded machine (other tests trace in parallel).
    fn collect_until(tracer: &DiskTracer, done: impl Fn(&DiskSample) -> bool) -> DiskSample {
        let mut all = DiskSample::default();
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < until {
            thread::sleep(Duration::from_millis(500));
            let s = tracer.take();
            all.records.extend(s.records);
            all.owners.extend(s.owners);
            all.images.extend(s.images);
            all.late_names.extend(s.late_names);
            if done(&all) {
                break;
            }
        }
        all
    }

    fn bytes_for(sample: &DiskSample, marker: &str, read: bool) -> u64 {
        let late: HashMap<u64, &Arc<str>> =
            sample.late_names.iter().map(|(o, n)| (*o, n)).collect();
        sample
            .records
            .iter()
            .filter(|r| {
                r.file
                    .as_ref()
                    .or(late.get(&r.file_object).copied())
                    .is_some_and(|f| f.ends_with(marker))
            })
            .map(|r| if read { r.totals.read } else { r.totals.write })
            .sum()
    }

    /// Needs an elevated test run; skips itself otherwise.
    #[test]
    fn sees_our_own_write() {
        if !crate::hardening::is_elevated() {
            eprintln!("not elevated, skipping");
            return;
        }
        let _turn = crate::etw::kernel_turn();
        let _ = crate::hardening::enable_privilege(windows::core::w!("SeSystemProfilePrivilege"));
        let tracer = DiskTracer::start().expect("could not start the trace");
        thread::sleep(Duration::from_millis(500));
        let marker = format!("anabolic-disk-{}.bin", std::process::id());
        let path = std::env::temp_dir().join(&marker);
        {
            // Write through the cache so it really hits the disk now.
            use std::os::windows::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .custom_flags(0x8000_0000) // FILE_FLAG_WRITE_THROUGH
                .open(&path)
                .unwrap();
            f.write_all(&vec![0x5Au8; 4 << 20]).unwrap();
            f.sync_all().unwrap();
        }
        let sample = collect_until(&tracer, |s| bytes_for(s, &marker, false) >= 4 << 20);
        let _ = std::fs::remove_file(&path);
        let ours = bytes_for(&sample, &marker, false);
        assert!(
            ours >= 4 << 20,
            "saw {ours} bytes written to the marker file; {} records, lost {:?}",
            sample.records.len(),
            tracer.lost()
        );
        // The process rundown at the start names us.
        let me = std::process::id();
        let image = sample.images.get(&me).map(|s| s.to_lowercase());
        assert!(
            image
                .as_deref()
                .is_some_and(|i| i.starts_with("anabolic_core")),
            "our image name: {image:?}"
        );
    }

    /// Files opened before the session started get named by the rundown.
    #[test]
    fn names_files_opened_before_tracing() {
        if !crate::hardening::is_elevated() {
            eprintln!("not elevated, skipping");
            return;
        }
        let _turn = crate::etw::kernel_turn();
        use std::io::{Read, Seek, SeekFrom};
        use std::os::windows::fs::OpenOptionsExt;
        let marker = format!("anabolic-disk-old-{}.bin", std::process::id());
        let path = std::env::temp_dir().join(&marker);
        std::fs::write(&path, vec![0xA5u8; 2 << 20]).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x2000_0000) // FILE_FLAG_NO_BUFFERING: every read hits the disk
            .open(&path)
            .unwrap();
        let _ = crate::hardening::enable_privilege(windows::core::w!("SeSystemProfilePrivilege"));
        let tracer = DiskTracer::start().expect("could not start the trace");
        thread::sleep(Duration::from_millis(500));
        let mut buf = vec![0u8; 1 << 20];
        for _ in 0..4 {
            f.seek(SeekFrom::Start(0)).unwrap();
            f.read_exact(&mut buf).unwrap();
        }
        // The helper notices within half a second; the rundown takes about one.
        let sample = collect_until(&tracer, |s| bytes_for(s, &marker, true) >= 4 << 20);
        drop(f);
        let _ = std::fs::remove_file(&path);
        let named = bytes_for(&sample, &marker, true);
        assert!(
            named >= 4 << 20,
            "saw {named} bytes read from the marker file; {} late names, lost {:?}",
            sample.late_names.len(),
            tracer.lost()
        );
    }
}
