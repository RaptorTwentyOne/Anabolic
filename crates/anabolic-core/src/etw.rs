//! Minimal real-time ETW: a private trace session, providers enabled on it,
//! and a consumer thread that hands every event to a callback.

use core::ffi::c_void;
use core::mem::size_of;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, CloseTrace, ControlTraceW, EVENT_CONTROL_CODE_CAPTURE_STATE,
    EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_QUERY,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2, OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD,
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
        let Some(pid) = name.strip_prefix(&head).and_then(|p| p.parse::<u32>().ok()) else {
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

/// A running trace session; stopped when dropped.
pub struct Session {
    name: Vec<u16>,
    handle: CONTROLTRACE_HANDLE,
}

impl Session {
    /// `extra_mode` and `enable_flags` are only needed for kernel (system
    /// logger) sessions.
    pub fn start(prefix: &str, extra_mode: u32, enable_flags: u32) -> Result<Self, String> {
        stop_orphans(prefix);
        let name = wide(&format!("{prefix}-{}", std::process::id()));
        // Left behind by an earlier session of this same process.
        stop_by_name(&name);
        let mut buf = properties(&name);
        let props = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        unsafe {
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE | extra_mode;
            (*props).FlushTimer = 1;
            (*props).BufferSize = 256; // KB
            (*props).MinimumBuffers = 16;
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
        let mut buf = properties(&self.name);
        let props = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        let err = unsafe {
            ControlTraceW(
                CONTROLTRACE_HANDLE::default(),
                PCWSTR(self.name.as_ptr()),
                props,
                EVENT_TRACE_CONTROL_QUERY,
            )
        };
        if err != ERROR_SUCCESS {
            return (0, 0);
        }
        unsafe { ((*props).EventsLost, (*props).RealTimeBuffersLost) }
    }

    pub fn enable(&self, provider: &GUID, level: u8, keywords: u64) -> Result<(), String> {
        self.control(
            provider,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
            level,
            keywords,
        )
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
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
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
