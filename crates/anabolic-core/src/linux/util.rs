//! Small helpers the Linux side shares: time units, process handles that
//! cannot outlive their process' PID, and errors in words.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::model::ProcKey;

/// Kernel paths are the paths people know on Linux: nothing to translate.
#[derive(Default)]
pub struct DeviceMap;

impl DeviceMap {
    pub fn build() -> Self {
        Self
    }

    pub fn to_dos_path(&self, path: &str) -> Option<String> {
        Some(path.to_string())
    }
}

/// Seconds from 1601-01-01 (FILETIME's epoch) to 1970-01-01.
const EPOCH_GAP: i64 = 11_644_473_600;

/// Seconds since 1970 as a FILETIME: 100 ns units since 1601, UTC.
pub fn filetime_from_unix(secs: f64) -> i64 {
    ((secs + EPOCH_GAP as f64) * 10_000_000.0) as i64
}

/// A FILETIME as seconds since 1970.
pub fn unix_from_filetime(filetime: i64) -> f64 {
    filetime as f64 / 10_000_000.0 - EPOCH_GAP as f64
}

/// `bytes` random bytes from the kernel's generator, in hex: for secrets
/// such as the MCP server's access key.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let mut filled = 0;
    while filled < buf.len() {
        let n =
            unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            assert!(
                e.kind() == io::ErrorKind::Interrupted,
                "getrandom failed: {e}"
            );
            continue;
        }
        filled += n as usize;
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// "Permission denied" rather than "Permission denied (os error 13)".
pub fn error_text(e: &io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// A process opened as a pidfd. Unlike a PID it stays bound to that one
/// process: signals sent through it never reach a later process that got
/// the same number.
pub struct Pidfd(OwnedFd);

impl Pidfd {
    pub fn send(&self, signal: i32) -> io::Result<()> {
        let done = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if done < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Whether the process has ended (and is waiting to be collected, or
    /// gone): the pidfd turns readable then.
    pub fn ended(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut poll, 1, 0) > 0 }
    }

    /// Waits up to `ms` for the process to end.
    pub fn wait_ended(&self, ms: i32) -> bool {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut poll, 1, ms) > 0 }
    }
}

/// Opens `key` as a pidfd and makes sure it is still the instance `key`
/// refers to, so a recycled PID never gets someone else's process ended.
pub fn open_process(key: ProcKey) -> io::Result<Pidfd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, key.pid as libc::pid_t, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let pidfd = Pidfd(unsafe { OwnedFd::from_raw_fd(fd as i32) });
    // Read after opening: if the start time still matches, the pidfd holds
    // the process `key` names.
    if key.create_time != 0
        && crate::procfs::stat(key.pid).is_none_or(|s| s.create_time() != key.create_time)
    {
        return Err(io::Error::other(
            "the process has exited and its PID was reused",
        ));
    }
    Ok(pidfd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_is_hex_and_differs() {
        let a = random_hex(32);
        let b = random_hex(32);
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn filetime_round_trips() {
        let now = 1_760_000_000.5;
        let ft = filetime_from_unix(now);
        assert!((unix_from_filetime(ft) - now).abs() < 1e-3);
        assert_eq!(filetime_from_unix(0.0), 116_444_736_000_000_000);
    }

    #[test]
    fn opens_itself_and_refuses_a_stale_key() {
        let me = crate::procfs::stat(std::process::id()).unwrap();
        let key = ProcKey {
            pid: std::process::id(),
            create_time: me.create_time(),
        };
        let fd = open_process(key).expect("own pidfd");
        assert!(!fd.ended());
        let stale = ProcKey {
            create_time: key.create_time - 10_000_000,
            ..key
        };
        assert!(open_process(stale).is_err());
    }
}
