//! The kernel's own tracing, through perf_event_open: per-processor ring
//! buffers the kernel writes samples and tracepoint events into, read on a
//! thread of Anabolic's own. No module is loaded and nothing is injected:
//! this is what `perf` itself uses.
//!
//! System-wide events need cap_perfmon (or root, or a lowered
//! kernel.perf_event_paranoid); tracepoints are found in tracefs, which only
//! root reads unless cap_dac_read_search is given.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

/// perf_event_attr, as of PERF_ATTR_SIZE_VER8.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Attr {
    pub kind: u32,
    pub size: u32,
    pub config: u64,
    /// The sample period, or with `FREQ` the samples per second.
    pub sample: u64,
    pub sample_type: u64,
    pub read_format: u64,
    pub flags: u64,
    /// Events, or with `WATERMARK` bytes, before poll() wakes the reader.
    pub wakeup: u32,
    pub bp_type: u32,
    pub config1: u64,
    pub config2: u64,
    pub branch_sample_type: u64,
    pub sample_regs_user: u64,
    pub sample_stack_user: u32,
    pub clockid: i32,
    pub sample_regs_intr: u64,
    pub aux_watermark: u32,
    pub sample_max_stack: u16,
    pub reserved_2: u16,
    pub aux_sample_size: u32,
    pub reserved_3: u32,
    pub sig_data: u64,
    pub config3: u64,
}

pub const TYPE_HARDWARE: u32 = 0;
pub const TYPE_SOFTWARE: u32 = 1;
pub const TYPE_TRACEPOINT: u32 = 2;

pub const HW_CPU_CYCLES: u64 = 0;
pub const SW_CPU_CLOCK: u64 = 0;

pub const SAMPLE_TID: u64 = 1 << 1;
pub const SAMPLE_TIME: u64 = 1 << 2;
pub const SAMPLE_CPU: u64 = 1 << 7;
pub const SAMPLE_RAW: u64 = 1 << 10;

pub const DISABLED: u64 = 1 << 0;
pub const EXCLUDE_IDLE: u64 = 1 << 7;
pub const COMM: u64 = 1 << 9;
pub const FREQ: u64 = 1 << 10;
pub const WATERMARK: u64 = 1 << 14;
pub const SAMPLE_ID_ALL: u64 = 1 << 18;
pub const COMM_EXEC: u64 = 1 << 24;
pub const USE_CLOCKID: u64 = 1 << 25;

const RECORD_LOST: u32 = 2;
const RECORD_COMM: u32 = 3;
const RECORD_SAMPLE: u32 = 9;

/// perf_event_open flags: close the descriptor on exec.
const FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

const IOC_ENABLE: libc::c_ulong = 0x2400;
const IOC_SET_OUTPUT: libc::c_ulong = 0x2405;

impl Attr {
    /// An attribute for `kind`/`config`, timed by CLOCK_MONOTONIC so its
    /// times compare with the rest of the system's.
    pub fn new(kind: u32, config: u64) -> Self {
        Self {
            kind,
            size: size_of::<Attr>() as u32,
            config,
            flags: DISABLED | USE_CLOCKID | SAMPLE_ID_ALL,
            clockid: libc::CLOCK_MONOTONIC,
            ..Default::default()
        }
    }
}

/// perf_event_open(2).
pub fn open(attr: &Attr, pid: i32, cpu: i32) -> io::Result<OwnedFd> {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            attr as *const Attr,
            pid,
            cpu,
            -1,
            FLAG_FD_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

/// Why a system-wide event was refused, in words that say what to give.
pub fn refused(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => {
            let paranoid = std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
                .map(|p| p.trim().to_string())
                .unwrap_or_else(|_| "?".into());
            format!(
                "needs cap_perfmon (kernel.perf_event_paranoid is {paranoid}): grant Anabolic \
                 its rights in Settings"
            )
        }
        Some(libc::ENOENT) | Some(libc::EOPNOTSUPP) => {
            "this kernel or processor does not offer that event".into()
        }
        Some(libc::EMFILE) => "too many open files for one event per processor".into(),
        _ => crate::util::error_text(e),
    }
}

/// The processors online.
pub fn online_cpus() -> Vec<u32> {
    let online = std::fs::read_to_string("/sys/devices/system/cpu/online").unwrap_or_default();
    crate::topology::parse_list(online.trim())
        .into_iter()
        .map(|c| c as u32)
        .collect()
}

/// One processor's ring buffer, mapped from the kernel.
pub struct Ring {
    fd: OwnedFd,
    base: *mut u8,
    len: usize,
    data_offset: usize,
    data_size: usize,
    /// A record that runs past the end of the buffer, put back together.
    scratch: Vec<u8>,
}

// The mapping belongs to this ring alone; it moves to the reader thread.
unsafe impl Send for Ring {}

/// struct perf_event_mmap_page: where the kernel's head and our tail are.
const DATA_HEAD: usize = 1024;
const DATA_TAIL: usize = 1032;
const DATA_OFFSET: usize = 1040;
const DATA_SIZE: usize = 1048;

impl Ring {
    /// Maps `pages` pages of data (a power of two) for `fd`.
    pub fn map(fd: OwnedFd, pages: usize) -> io::Result<Ring> {
        let page = crate::procfs::page_size() as usize;
        let len = page * (pages + 1);
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let base = base.cast::<u8>();
        let field = |at: usize| unsafe { base.add(at).cast::<u64>().read_volatile() } as usize;
        // Kernels before 4.1 leave these zero: the data follows the first page.
        let (data_offset, data_size) = match (field(DATA_OFFSET), field(DATA_SIZE)) {
            (_, 0) => (page, len - page),
            (offset, size) => (offset, size),
        };
        Ok(Ring {
            fd,
            base,
            len,
            data_offset,
            data_size,
            scratch: Vec::new(),
        })
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    /// Hands every record written since the last call to `each`, then
    /// gives the space back to the kernel.
    pub fn drain(&mut self, mut each: impl FnMut(Record<'_>)) {
        let head_ptr = unsafe { self.base.add(DATA_HEAD).cast::<u64>() };
        let tail_ptr = unsafe { self.base.add(DATA_TAIL).cast::<u64>() };
        let head =
            unsafe { std::sync::atomic::AtomicU64::from_ptr(head_ptr) }.load(Ordering::Acquire);
        let mut tail = unsafe { tail_ptr.read_volatile() };
        let data =
            unsafe { std::slice::from_raw_parts(self.base.add(self.data_offset), self.data_size) };
        while tail + 8 <= head {
            let at = (tail % self.data_size as u64) as usize;
            let header = read_wrapped(data, at, 8);
            let kind = u32::from_ne_bytes(header[0..4].try_into().unwrap());
            let size = u16::from_ne_bytes(header[6..8].try_into().unwrap()) as usize;
            if size < 8 {
                // Not a record: give up on what is left rather than stall.
                tail = head;
                break;
            }
            if tail + size as u64 > head {
                break;
            }
            let body: &[u8] = if at + size <= data.len() {
                &data[at + 8..at + size]
            } else {
                self.scratch = read_wrapped(data, at, size);
                &self.scratch[8..]
            };
            each(Record::parse(kind, body));
            tail += size as u64;
        }
        unsafe { std::sync::atomic::AtomicU64::from_ptr(tail_ptr) }.store(tail, Ordering::Release);
    }
}

/// `len` bytes from `at`, continuing at the start of the buffer.
fn read_wrapped(data: &[u8], at: usize, len: usize) -> Vec<u8> {
    let first = len.min(data.len() - at);
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&data[at..at + first]);
    out.extend_from_slice(&data[..len - first]);
    out
}

impl Drop for Ring {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base.cast(), self.len);
        }
    }
}

/// One record from a ring buffer.
pub enum Record<'a> {
    /// A sample: its fields as asked for in `sample_type`.
    Sample(&'a [u8]),
    /// The kernel dropped this many records: the reader fell behind.
    Lost(u64),
    /// A task got its name (an exec, or a rename).
    Comm {
        pid: u32,
        tid: u32,
        name: &'a str,
    },
    Other,
}

impl<'a> Record<'a> {
    fn parse(kind: u32, body: &'a [u8]) -> Record<'a> {
        match kind {
            RECORD_SAMPLE => Record::Sample(body),
            RECORD_LOST => Record::Lost(u64_at(body, 8).unwrap_or(0)),
            RECORD_COMM => {
                let (Some(pid), Some(tid)) = (u32_at(body, 0), u32_at(body, 4)) else {
                    return Record::Other;
                };
                let name = body.get(8..).unwrap_or_default();
                let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                Record::Comm {
                    pid,
                    tid,
                    name: std::str::from_utf8(&name[..end]).unwrap_or(""),
                }
            }
            _ => Record::Other,
        }
    }
}

pub fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

pub fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

pub fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// A sample's fields, for the sample types asked for here, in the order
/// the kernel writes them (TID, TIME, CPU, RAW).
#[derive(Debug, Default, PartialEq)]
pub struct Sample<'a> {
    pub pid: u32,
    pub tid: u32,
    /// CLOCK_MONOTONIC, in ns.
    pub time: u64,
    pub cpu: u32,
    pub raw: &'a [u8],
}

impl<'a> Sample<'a> {
    pub fn parse(body: &'a [u8], sample_type: u64) -> Option<Sample<'a>> {
        let mut s = Sample::default();
        let mut at = 0;
        if sample_type & SAMPLE_TID != 0 {
            s.pid = u32_at(body, at)?;
            s.tid = u32_at(body, at + 4)?;
            at += 8;
        }
        if sample_type & SAMPLE_TIME != 0 {
            s.time = u64_at(body, at)?;
            at += 8;
        }
        if sample_type & SAMPLE_CPU != 0 {
            s.cpu = u32_at(body, at)?;
            at += 8;
        }
        if sample_type & SAMPLE_RAW != 0 {
            let size = u32_at(body, at)? as usize;
            s.raw = body.get(at + 4..at + 4 + size)?;
        }
        Some(s)
    }
}

/// Where tracefs is mounted.
fn tracefs() -> Result<&'static str, String> {
    let dirs = ["/sys/kernel/tracing", "/sys/kernel/debug/tracing"];
    if let Some(dir) = dirs
        .into_iter()
        .find(|dir| std::path::Path::new(&format!("{dir}/events")).exists())
    {
        return Ok(dir);
    }
    // Mounted (as usual), but only root may look inside.
    Err(if std::path::Path::new(dirs[0]).exists() {
        "only root may read the kernel's tracepoints (needs cap_dac_read_search)".into()
    } else {
        "the kernel's tracing file system is not mounted".into()
    })
}

/// A field of a tracepoint's raw record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub offset: usize,
    pub size: usize,
    /// A `__data_loc` string: the field holds its offset and length.
    pub data_loc: bool,
}

impl Field {
    pub fn u32(&self, raw: &[u8]) -> Option<u32> {
        match self.size {
            1 => raw.get(self.offset).map(|&b| u32::from(b)),
            2 => u16_at(raw, self.offset).map(u32::from),
            _ => u32_at(raw, self.offset),
        }
    }

    pub fn u64(&self, raw: &[u8]) -> Option<u64> {
        match self.size {
            8 => u64_at(raw, self.offset),
            _ => self.u32(raw).map(u64::from),
        }
    }

    /// A fixed char array or a `__data_loc` string, up to its NUL.
    pub fn str<'a>(&self, raw: &'a [u8]) -> Option<&'a str> {
        let bytes = if self.data_loc {
            let loc = u32_at(raw, self.offset)?;
            let (at, len) = ((loc & 0xFFFF) as usize, (loc >> 16) as usize);
            raw.get(at..at + len)?
        } else {
            raw.get(self.offset..self.offset + self.size)?
        };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..end]).ok()
    }
}

/// A tracepoint: its id, and where its fields sit in the raw record.
#[derive(Clone, Debug)]
pub struct Tracepoint {
    pub id: u64,
    pub fields: HashMap<String, Field>,
}

impl Tracepoint {
    pub fn field(&self, name: &str) -> Result<Field, String> {
        self.fields
            .get(name)
            .copied()
            .ok_or_else(|| format!("this kernel's tracepoint has no {name}"))
    }
}

/// Reads `events/<system>/<name>` from tracefs.
pub fn tracepoint(system: &str, name: &str) -> Result<Tracepoint, String> {
    let dir = tracefs()?;
    let read = |file: &str| {
        std::fs::read_to_string(format!("{dir}/events/{system}/{name}/{file}")).map_err(|e| match e
            .kind()
        {
            io::ErrorKind::PermissionDenied => {
                "only root may read the kernel's tracepoints (needs cap_dac_read_search)"
                    .to_string()
            }
            io::ErrorKind::NotFound => format!("this kernel has no {system}:{name} tracepoint"),
            _ => crate::util::error_text(&e),
        })
    };
    let id = read("id")?
        .trim()
        .parse()
        .map_err(|_| format!("{system}:{name} has no id"))?;
    Ok(Tracepoint {
        id,
        fields: parse_format(&read("format")?),
    })
}

/// The `field:` lines of a tracepoint's format file.
pub fn parse_format(format: &str) -> HashMap<String, Field> {
    let mut out = HashMap::new();
    for line in format.lines() {
        // "	field:unsigned int vec;	offset:8;	size:4;	signed:0;"
        let mut parts = line.trim().split(';').map(str::trim);
        let Some(decl) = parts.next().and_then(|d| d.strip_prefix("field:")) else {
            continue;
        };
        let (mut offset, mut size) = (None, None);
        for part in parts {
            if let Some(v) = part.strip_prefix("offset:") {
                offset = v.parse().ok();
            } else if let Some(v) = part.strip_prefix("size:") {
                size = v.parse().ok();
            }
        }
        // The name is the declaration's last word, without any [array].
        let Some(name) = decl
            .split_whitespace()
            .last()
            .map(|n| n.split('[').next().unwrap_or(n))
        else {
            continue;
        };
        if let (Some(offset), Some(size)) = (offset, size) {
            out.insert(
                name.to_string(),
                Field {
                    offset,
                    size,
                    data_loc: decl.starts_with("__data_loc"),
                },
            );
        }
    }
    out
}

/// The tracepoint id a raw record is of (`common_type`).
pub fn event_id(raw: &[u8]) -> Option<u64> {
    u16_at(raw, 0).map(u64::from)
}

/// A set of events on every processor, each processor's written into one
/// ring buffer, read by a thread of its own until dropped.
pub struct Trace {
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Trace {
    /// Opens the events `attrs` gives for each processor online, maps
    /// `pages` of buffer per processor, enables them and hands every record
    /// to `each` on a reader thread.
    pub fn start(
        name: &str,
        attrs: impl Fn(u32) -> Vec<Attr>,
        pages: usize,
        mut each: impl FnMut(Record<'_>) + Send + 'static,
    ) -> Result<Trace, String> {
        let mut rings = Vec::new();
        let mut followers = Vec::new();
        for cpu in online_cpus() {
            let mut leader: Option<Ring> = None;
            for attr in &attrs(cpu) {
                let fd = open(attr, -1, cpu as i32).map_err(|e| refused(&e))?;
                match &leader {
                    None => {
                        leader =
                            Some(Ring::map(fd, pages).map_err(|e| {
                                format!("the trace's buffer could not be mapped: {e}")
                            })?);
                    }
                    Some(ring) => {
                        // Into the processor's one buffer: its events stay in order.
                        let done = unsafe {
                            libc::ioctl(fd.as_raw_fd(), IOC_SET_OUTPUT, ring.fd().as_raw_fd())
                        };
                        if done != 0 {
                            return Err(crate::util::error_text(&io::Error::last_os_error()));
                        }
                        followers.push(fd);
                    }
                }
            }
            rings.extend(leader);
        }
        if rings.is_empty() {
            return Err("no processor to trace".into());
        }
        for fd in rings.iter().map(Ring::fd).chain(&followers) {
            unsafe { libc::ioctl(fd.as_raw_fd(), IOC_ENABLE, 0) };
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let reader = thread::Builder::new()
            .name(format!("anabolic-{name}"))
            .spawn(move || {
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::AboveNormal,
                );
                // Closed with the rings when the thread ends.
                let _followers = followers;
                let mut polls: Vec<libc::pollfd> = rings
                    .iter()
                    .map(|r| libc::pollfd {
                        fd: r.fd().as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    })
                    .collect();
                while !stopped.load(Ordering::Relaxed) {
                    unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 100) };
                    for ring in &mut rings {
                        ring.drain(&mut each);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Trace {
            stop,
            reader: Some(reader),
        })
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// A buffer's wakeup: when it is a quarter full (or the reader's poll
/// times out, every 100 ms).
pub fn watermark(attr: &mut Attr, pages: usize) {
    attr.flags |= WATERMARK;
    attr.wakeup = (pages * crate::procfs::page_size() as usize / 4) as u32;
}

/// CLOCK_MONOTONIC now, in ns.
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// The wall time of a CLOCK_MONOTONIC instant.
pub fn wall(ns: u64) -> SystemTime {
    let ago = monotonic_ns().saturating_sub(ns);
    SystemTime::now() - Duration::from_nanos(ago)
}

/// Which PIDs are kernel threads, asked once each: they are folded into
/// kthreadd in the process list, so what they do is charged there.
#[derive(Default)]
pub struct KernelThreads {
    known: HashMap<u32, bool>,
}

impl KernelThreads {
    /// The process a sample's PID stands for in the process list.
    pub fn owner(&mut self, pid: u32) -> u32 {
        if pid == 0 {
            return 0;
        }
        if self.known.len() > 100_000 {
            self.known.clear();
        }
        let kernel = *self
            .known
            .entry(pid)
            .or_insert_with(|| crate::procfs::stat(pid).is_some_and(|s| s.kernel_thread()));
        if kernel { crate::scan::KERNEL_PID } else { pid }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_tracepoint_format() {
        let format = "name: irq_handler_entry\nID: 128\nformat:\n\
            \tfield:unsigned short common_type;\toffset:0;\tsize:2;\tsigned:0;\n\
            \tfield:int common_pid;\toffset:4;\tsize:4;\tsigned:1;\n\n\
            \tfield:int irq;\toffset:8;\tsize:4;\tsigned:1;\n\
            \tfield:__data_loc char[] name;\toffset:12;\tsize:4;\tsigned:0;\n\
            \tfield:char rwbs[8];\toffset:16;\tsize:8;\tsigned:0;\n\n\
            print fmt: \"irq=%d name=%s\", REC->irq, __get_str(name)\n";
        let fields = parse_format(format);
        assert_eq!(
            fields["irq"],
            Field {
                offset: 8,
                size: 4,
                data_loc: false
            }
        );
        assert!(fields["name"].data_loc);
        assert_eq!(fields["rwbs"].size, 8);
        // A raw record: common fields, irq 42, name at 24 ("nvme0q3"), rwbs "WS".
        let mut raw = vec![0u8; 32];
        raw[0..2].copy_from_slice(&128u16.to_ne_bytes());
        raw[8..12].copy_from_slice(&42i32.to_ne_bytes());
        raw[12..16].copy_from_slice(&(24u32 | (8 << 16)).to_ne_bytes());
        raw[16..18].copy_from_slice(b"WS");
        raw[24..32].copy_from_slice(b"nvme0q3\0");
        assert_eq!(event_id(&raw), Some(128));
        assert_eq!(fields["irq"].u32(&raw), Some(42));
        assert_eq!(fields["name"].str(&raw), Some("nvme0q3"));
        assert_eq!(fields["rwbs"].str(&raw), Some("WS"));
    }

    #[test]
    fn parses_samples_in_kernel_order() {
        let mut body = Vec::new();
        body.extend_from_slice(&7u32.to_ne_bytes());
        body.extend_from_slice(&8u32.to_ne_bytes());
        body.extend_from_slice(&123u64.to_ne_bytes());
        body.extend_from_slice(&3u32.to_ne_bytes());
        body.extend_from_slice(&0u32.to_ne_bytes());
        body.extend_from_slice(&4u32.to_ne_bytes());
        body.extend_from_slice(b"abcd");
        let s = Sample::parse(&body, SAMPLE_TID | SAMPLE_TIME | SAMPLE_CPU | SAMPLE_RAW).unwrap();
        assert_eq!(
            (s.pid, s.tid, s.time, s.cpu, s.raw),
            (7, 8, 123, 3, &b"abcd"[..])
        );
        let s = Sample::parse(&body[..8], SAMPLE_TID).unwrap();
        assert_eq!((s.pid, s.tid), (7, 8));
    }

    #[test]
    fn reads_wrapped_records() {
        let data = [5u8, 6, 7, 0, 0, 1, 2, 3, 4];
        assert_eq!(read_wrapped(&data, 5, 6), vec![1, 2, 3, 4, 5, 6]);
    }

    /// The ring buffer, end to end, on what anyone may trace: this thread's
    /// own user-mode time.
    #[test]
    fn samples_its_own_thread() {
        let mut attr = Attr::new(TYPE_SOFTWARE, SW_CPU_CLOCK);
        attr.sample = 100_000;
        attr.sample_type = SAMPLE_TID | SAMPLE_TIME | SAMPLE_CPU;
        // exclude_kernel and exclude_hv: what an unprivileged process may watch.
        attr.flags |= 1 << 5 | 1 << 6;
        let fd = match open(&attr, 0, -1) {
            Ok(fd) => fd,
            Err(e) => {
                eprintln!("perf_event_open refused here ({e}), skipping");
                return;
            }
        };
        let mut ring = Ring::map(fd, 8).expect("mapped");
        unsafe { libc::ioctl(ring.fd().as_raw_fd(), IOC_ENABLE, 0) };
        let started = std::time::Instant::now();
        let mut x = 0u64;
        while started.elapsed() < Duration::from_millis(50) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        let (mut samples, mut mine) = (0, 0);
        let me = std::process::id();
        ring.drain(|r| {
            if let Record::Sample(body) = r
                && let Some(s) = Sample::parse(body, attr.sample_type)
            {
                samples += 1;
                if s.pid == me && s.time > 0 {
                    mine += 1;
                }
            }
        });
        assert!(samples > 10, "only {samples} samples in 50 ms at 10 kHz");
        assert_eq!(samples, mine);
    }

    #[test]
    fn names_kernel_threads_as_the_kernel() {
        let mut k = KernelThreads::default();
        assert_eq!(k.owner(std::process::id()), std::process::id());
        // kthreadd's first child.
        if crate::procfs::stat(3).is_some_and(|s| s.kernel_thread()) {
            assert_eq!(k.owner(3), crate::scan::KERNEL_PID);
        }
    }
}
