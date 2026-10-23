use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::model::ProcKey;

pub use crate::winfile::DeviceMap;

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

/// `bytes` random bytes from Windows' cryptographic generator, in hex: for
/// secrets such as the MCP server's access key.
pub fn random_hex(bytes: usize) -> String {
    use windows::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let mut buf = vec![0u8; bytes];
    let status = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    assert!(status.is_ok(), "BCryptGenRandom failed: {status:?}");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The signed-in user's SID ("S-1-5-21-..."); empty when it cannot be read.
pub fn user_sid() -> String {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return String::new();
        }
        let token = OwnedHandle(token);
        let mut buf = vec![0u64; 64];
        let mut size = 0u32;
        if GetTokenInformation(
            token.raw(),
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut size,
        )
        .is_err()
        {
            return String::new();
        }
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut text = windows::core::PWSTR::null();
        if ConvertSidToStringSidW(user.User.Sid, &mut text).is_err() {
            return String::new();
        }
        let sid = text.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

#[cfg(test)]
mod random_tests {
    #[test]
    fn random_hex_is_hex_and_differs() {
        let a = super::random_hex(32);
        let b = super::random_hex(32);
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
