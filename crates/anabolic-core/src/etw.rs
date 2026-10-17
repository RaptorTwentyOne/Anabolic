//! Minimal real-time ETW: a private trace session, providers enabled on it,
//! and a consumer thread that hands every event to a callback.

use core::ffi::c_void;
use core::mem::size_of;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, CloseTrace, ControlTraceW, ENABLE_TRACE_PARAMETERS,
    ENABLE_TRACE_PARAMETERS_VERSION_2, EVENT_CONTROL_CODE_CAPTURE_STATE,
    EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_FILTER_DESCRIPTOR, EVENT_FILTER_TYPE_EVENT_ID,
    EVENT_RECORD, EVENT_TRACE_CONTROL_QUERY, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG,
    EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2,
    OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP,
    PROCESS_TRACE_MODE_REAL_TIME, PROCESSTRACE_HANDLE, ProcessTrace, QueryAllTracesW, StartTraceW,
    WNODE_FLAG_TRACED_GUID,
};
use windows::core::{GUID, PCWSTR, PWSTR};

use crate::util::wide;

type Callback = Box<dyn FnMut(&EVENT_RECORD) + Send>;

/// EVENT_TRACE_PROPERTIES followed by room for the session name, 8-aligned.
fn properties(name: &[u16]) -> Vec<u64> {
    let bytes = size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2;
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    let props = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
    unsafe {
        (*props).Wnode.BufferSize = (buf.len() * 8) as u32;
        (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // QueryPerformanceCounter timestamps.
        (*props).Wnode.ClientContext = 1;
        (*props).LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
    }
    buf
}

fn stop_by_name(name: &[u16]) {
    let mut buf = properties(name);
    unsafe {
        let _ = ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            PCWSTR(name.as_ptr()),
            buf.as_mut_ptr().cast(),
            EVENT_TRACE_CONTROL_STOP,
        );
    }
}

/// Session names are `{prefix}-{pid}` so several Anabolic instances do not
/// stop each other's traces. Sessions outlive a process that is killed; this
/// stops the ones whose owner is gone.
pub fn stop_orphans(prefix: &str) {
    const SLOTS: usize = 64;
    let blank: Vec<u16> = vec![0; 1024];
    let mut buffers: Vec<Vec<u64>> = (0..SLOTS).map(|_| properties(&blank)).collect();
    for buf in &mut buffers {
        let props = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        unsafe {
            (*props).LogFileNameOffset = 0;
        }
    }
    let mut pointers: Vec<*mut EVENT_TRACE_PROPERTIES> =
        buffers.iter_mut().map(|b| b.as_mut_ptr().cast()).collect();
    let mut count = 0u32;
    if unsafe { QueryAllTracesW(&mut pointers, &mut count) } != ERROR_SUCCESS {
        return;
    }
    let head = format!("{prefix}-");
    for &props in pointers.iter().take(count as usize) {
        let name = unsafe {
            let at = props
                .cast::<u8>()
                .add((*props).LoggerNameOffset as usize)
                .cast::<u16>();
            crate::util::from_wide(std::slice::from_raw_parts(at, blank.len()))
        };
        // `{prefix}-{pid}`, or `{prefix}-{pid}-{instance}`.
        let Some(pid) = name
            .strip_prefix(&head)
            .and_then(|p| p.split('-').next()?.parse::<u32>().ok())
        else {
            continue;
        };
        if !process_alive(pid) {
            stop_by_name(&wide(&name));
        }
    }
}

fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::STILL_ACTIVE;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let h = crate::util::OwnedHandle(h);
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(h.raw(), &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32
}

/// Events and real-time buffers a running session dropped, by its name.
pub fn lost_by_name(name: &[u16]) -> (u32, u32) {
    let mut buf = properties(name);
    let props = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
    let err = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            PCWSTR(name.as_ptr()),
            props,
            EVENT_TRACE_CONTROL_QUERY,
        )
    };
    if err != ERROR_SUCCESS {
        return (0, 0);
    }
    unsafe { ((*props).EventsLost, (*props).RealTimeBuffersLost) }
}

/// A session's buffers: size in KB, how many to start with, and how many
/// it may grow to when the consumer falls behind (0: ETW's default).
#[derive(Clone, Copy, Debug)]
pub struct Buffers {
    pub kb: u32,
    pub min: u32,
    pub max: u32,
}

impl Default for Buffers {
    fn default() -> Self {
        Self {
            kb: 256,
            min: 16,
            max: 0,
        }
    }
}

/// A running trace session; stopped when dropped.
pub struct Session {
    name: Vec<u16>,
    handle: CONTROLTRACE_HANDLE,
}

impl Session {
    /// `extra_mode` and `enable_flags` are only needed for kernel (system
    /// logger) sessions.
    pub fn start(prefix: &str, extra_mode: u32, enable_flags: u32) -> Result<Self, String> {
        Self::start_named(
            prefix,
            format!("{prefix}-{}", std::process::id()),
            extra_mode,
            enable_flags,
            Buffers::default(),
        )
    }

    /// Like `start`, for a kind of session this process may run several of
    /// at once: each instance gets a name of its own, and buffers to suit.
    pub fn start_instance(
        prefix: &str,
        instance: u32,
        extra_mode: u32,
        enable_flags: u32,
        buffers: Buffers,
    ) -> Result<Self, String> {
        let name = format!("{prefix}-{}-{instance}", std::process::id());
        Self::start_named(prefix, name, extra_mode, enable_flags, buffers)
    }

    fn start_named(
        prefix: &str,
        name: String,
        extra_mode: u32,
        enable_flags: u32,
        buffers: Buffers,
    ) -> Result<Self, String> {
        stop_orphans(prefix);
        let name = wide(&name);
        // Left behind by an earlier session of this same process.
        stop_by_name(&name);
        let mut buf = properties(&name);
        let props = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        unsafe {
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE | extra_mode;
            (*props).FlushTimer = 1;
            (*props).BufferSize = buffers.kb;
            (*props).MinimumBuffers = buffers.min;
            // 0 leaves the maximum to ETW.
            (*props).MaximumBuffers = buffers.max;
            (*props).EnableFlags = EVENT_TRACE_FLAG(enable_flags);
        }
        let mut handle = CONTROLTRACE_HANDLE::default();
        let err = unsafe { StartTraceW(&mut handle, PCWSTR(name.as_ptr()), props) };
        if err != ERROR_SUCCESS {
            return Err(crate::util::error_text(&windows::core::Error::from(
                err.to_hresult(),
            )));
        }
        Ok(Self { name, handle })
    }

    /// Events and real-time buffers the session dropped so far (it was
    /// producing faster than we consumed).
    pub fn lost(&self) -> (u32, u32) {
        lost_by_name(&self.name)
    }

    /// The session's name (NUL-terminated UTF-16), for `lost_by_name` from
    /// a thread that does not own the session.
    pub fn name(&self) -> Vec<u16> {
        self.name.clone()
    }

    pub fn enable(&self, provider: &GUID, level: u8, keywords: u64) -> Result<(), String> {
        self.control(
            provider,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
            level,
            keywords,
        )
    }

    /// Like `enable`, but only the events with these ids reach the session.
    /// For chatty providers whose keywords cannot single out the few events
    /// wanted: ETW drops the rest before they are buffered.
    pub fn enable_ids(
        &self,
        provider: &GUID,
        level: u8,
        keywords: u64,
        ids: &[u16],
    ) -> Result<(), String> {
        // EVENT_FILTER_EVENT_ID: FilterIn (BOOLEAN), Reserved, Count (u16),
        // then the ids.
        let mut filter: Vec<u16> = Vec::with_capacity(2 + ids.len());
        filter.push(1);
        filter.push(ids.len() as u16);
        filter.extend_from_slice(ids);
        let mut descriptor = EVENT_FILTER_DESCRIPTOR {
            Ptr: filter.as_ptr() as u64,
            Size: (filter.len() * 2) as u32,
            Type: EVENT_FILTER_TYPE_EVENT_ID,
        };
        let parameters = ENABLE_TRACE_PARAMETERS {
            Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
            EnableFilterDesc: &mut descriptor,
            FilterDescCount: 1,
            ..Default::default()
        };
        let err = unsafe {
            EnableTraceEx2(
                self.handle,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                level,
                keywords,
                0,
                0,
                Some(&parameters),
            )
        };
        if err == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(crate::util::error_text(&windows::core::Error::from(
                err.to_hresult(),
            )))
        }
    }

    /// Asks an enabled provider to log its current state (a rundown).
    pub fn capture_state(&self, provider: &GUID, keywords: u64) -> Result<(), String> {
        self.control(provider, EVENT_CONTROL_CODE_CAPTURE_STATE.0, 0, keywords)
    }

    fn control(&self, provider: &GUID, code: u32, level: u8, keywords: u64) -> Result<(), String> {
        let err =
            unsafe { EnableTraceEx2(self.handle, provider, code, level, keywords, 0, 0, None) };
        if err == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(crate::util::error_text(&windows::core::Error::from(
                err.to_hresult(),
            )))
        }
    }

    /// Delivers every event to `callback` on a new thread until the session stops.
    pub fn consume(
        &self,
        callback: impl FnMut(&EVENT_RECORD) + Send + 'static,
    ) -> Result<JoinHandle<()>, String> {
        let context = Box::into_raw(Box::new(Box::new(callback) as Callback));
        // SAFETY: plain data; the unions are filled in below.
        let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
        logfile.LoggerName = PWSTR(self.name.as_ptr().cast_mut());
        // Raw timestamps: counter ticks, comparable with the ones some
        // payloads carry (a DPC's start) and with QpcClock. Without it they
        // arrive converted to system time.
        logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME
            | PROCESS_TRACE_MODE_EVENT_RECORD
            | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        logfile.Anonymous2.EventRecordCallback = Some(trampoline);
        logfile.Context = context.cast::<c_void>();
        let trace = unsafe { OpenTraceW(&mut logfile) };
        if trace.Value == u64::MAX {
            drop(unsafe { Box::from_raw(context) });
            return Err(windows::core::Error::from_thread().message());
        }
        let context = context as usize;
        thread::Builder::new()
            .name("anabolic-etw".into())
            .spawn(move || {
                let trace = PROCESSTRACE_HANDLE { Value: trace.Value };
                unsafe {
                    // Returns once the session is stopped.
                    let _ = ProcessTrace(&[trace], None, None);
                    let _ = CloseTrace(trace);
                    drop(Box::from_raw(context as *mut Callback));
                }
            })
            .map_err(|e| e.to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        stop_by_name(&self.name);
    }
}

unsafe extern "system" fn trampoline(record: *mut EVENT_RECORD) {
    // SAFETY: ETW passes a valid record whose UserContext is our boxed callback.
    unsafe {
        let record = &*record;
        let callback = &mut *record.UserContext.cast::<Callback>();
        callback(record);
    }
}

/// Turns event timestamps into durations and wall-clock times. Sessions here
/// stamp events with QueryPerformanceCounter ticks (ClientContext 1).
#[derive(Clone, Copy, Debug)]
pub struct QpcClock {
    /// Ticks per second.
    pub freq: i64,
    origin: i64,
    wall: std::time::SystemTime,
}

impl QpcClock {
    pub fn new() -> Self {
        let mut freq = 0i64;
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut freq);
        }
        Self {
            freq: freq.max(1),
            origin: Self::ticks(),
            wall: std::time::SystemTime::now(),
        }
    }

    /// The counter now, comparable with event timestamps.
    pub fn ticks() -> i64 {
        let mut now = 0i64;
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut now);
        }
        now
    }

    pub fn ms(&self, ticks: i64) -> f32 {
        (ticks as f64 * 1000.0 / self.freq as f64) as f32
    }

    pub fn from_ms(&self, ms: f64) -> i64 {
        (ms * self.freq as f64 / 1000.0) as i64
    }

    /// When a timestamp happened, by the clock on the wall.
    pub fn wall(&self, ticks: i64) -> std::time::SystemTime {
        let ms = (ticks - self.origin) as f64 * 1000.0 / self.freq as f64;
        let offset = std::time::Duration::from_secs_f64(ms.abs() / 1000.0);
        if ms >= 0.0 {
            self.wall + offset
        } else {
            self.wall - offset
        }
    }
}

impl Default for QpcClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Events from 32-bit processes carry 4-byte pointers.
pub fn pointer_size(record: &EVENT_RECORD) -> usize {
    const EVENT_HEADER_FLAG_32_BIT_HEADER: u16 = 0x20;
    if record.EventHeader.Flags & EVENT_HEADER_FLAG_32_BIT_HEADER != 0 {
        4
    } else {
        8
    }
}

/// A pointer-sized payload field.
pub fn pointer_at(d: &[u8], at: usize, size: usize) -> Option<u64> {
    if size == 4 {
        u32_at(d, at).map(u64::from)
    } else {
        u64_at(d, at)
    }
}

pub fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

pub fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// Process provider opcodes carrying an image name: a start, and the
/// enumeration of running processes when a session begins.
pub const OPCODE_PROCESS_START: u8 = 1;
pub const OPCODE_PROCESS_RUNDOWN: u8 = 3;

/// Process_TypeGroup1: UniqueProcessKey, ProcessId, ParentId, SessionId,
/// ExitStatus, DirectoryTableBase, Flags, UserSID, ImageFileName (ANSI), ...
/// Returns the pid and image name.
pub fn process_image(d: &[u8]) -> Option<(u32, String)> {
    let pid = u32_at(d, 8)?;
    let sid = 36;
    // The SID is a TOKEN_USER (two pointers) followed by the SID itself, or
    // a zero when there is none.
    let image = if u32_at(d, sid)? == 0 {
        sid + 4
    } else {
        let sub_authorities = *d.get(sid + 16 + 1)? as usize;
        sid + 16 + 8 + 4 * sub_authorities
    };
    let bytes = d.get(image..)?;
    let end = bytes.iter().position(|&b| b == 0)?;
    let name = String::from_utf8_lossy(&bytes[..end]).into_owned();
    (!name.is_empty()).then_some((pid, name))
}

/// The event's payload bytes.
pub fn user_data(record: &EVENT_RECORD) -> &[u8] {
    if record.UserData.is_null() {
        return &[];
    }
    unsafe {
        std::slice::from_raw_parts(record.UserData.cast::<u8>(), record.UserDataLength as usize)
    }
}
