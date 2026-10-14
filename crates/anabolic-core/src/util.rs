use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::model::ProcKey;

/// Closes the handle on drop.
pub struct OwnedHandle(pub HANDLE);

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn filetime_to_i64(ft: FILETIME) -> i64 {
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

/// Opens a process and makes sure it is still the instance `key` refers to,
/// so a recycled PID never gets someone else's process killed.
pub fn open_process(
    key: ProcKey,
    access: PROCESS_ACCESS_RIGHTS,
) -> windows::core::Result<OwnedHandle> {
    let handle =
        match unsafe { OpenProcess(access | PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) } {
            Ok(h) => OwnedHandle(h),
            // Some protected processes grant the action but not the query.
            Err(_) => OwnedHandle(unsafe { OpenProcess(access, false, key.pid)? }),
        };
    // The idle and System processes report a zero create time.
    if key.create_time != 0 {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        if unsafe {
            GetProcessTimes(
                handle.raw(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
        .is_ok()
            && filetime_to_i64(created) != key.create_time
        {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_INVALIDARG,
                "the process has exited and its PID was reused",
            ));
        }
    }
    Ok(handle)
}

/// "Access is denied. (0x80070005)" style text for an error.
pub fn error_text(e: &windows::core::Error) -> String {
    let msg = e.message();
    let msg = msg.trim();
    if msg.is_empty() {
        format!("0x{:08X}", e.code().0 as u32)
    } else {
        format!("{msg} (0x{:08X})", e.code().0 as u32)
    }
}
