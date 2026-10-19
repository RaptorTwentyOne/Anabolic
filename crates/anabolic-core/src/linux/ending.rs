//! Why a process does not go away, on Linux:
//!
//! - It has ended but its parent has not collected it (a zombie): it uses
//!   nothing but its PID, and goes when the parent collects it or exits.
//! - It waits inside the kernel and cannot be interrupted (state D), often
//!   on a device or a network file system that stopped answering: not
//!   even SIGKILL lands until that wait ends.
//! - It is not Anabolic's to end: another user's process without
//!   cap_kill, a kernel thread, or init.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::model::ProcKey;
use crate::procfs;

/// What the kernel says about a process' state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    /// It has ended and waits to be collected by its parent (a zombie).
    pub deleting: bool,
    /// The system's own: a kernel thread or init, which no one ends.
    pub protected: bool,
    pub wow64: bool,
    /// The state letter /proc gives (R, S, D, Z…).
    pub exit_status: u32,
}

/// A process holding the one asked about: its parent, for a zombie.
#[derive(Clone, Debug)]
pub struct Holder {
    pub pid: u32,
    pub name: Arc<str>,
    pub handles: Vec<u64>,
    pub access: u32,
}

#[derive(Clone, Debug)]
pub struct EndReport {
    pub key: ProcKey,
    pub flags: Flags,
    pub threads: u32,
    /// Always 0: Linux has no protected processes.
    pub protection: u8,
    /// Whether a signal may be sent to it; None when that could not be
    /// asked (`open_error`).
    pub can_end: Option<bool>,
    pub open_error: Option<String>,
    /// The parent that must collect it, for a zombie.
    pub holders: Vec<Holder>,
    /// Where it waits inside the kernel, for a process in state D.
    pub guards: Vec<(String, &'static str)>,
    pub took: Duration,
}

/// Whether `pid` has ended and waits to be collected.
pub fn is_ending(pid: u32) -> bool {
    procfs::stat(pid).is_some_and(|s| s.state == b'Z')
}

/// Linux has no protected processes.
pub fn protection_label(_protection: u8) -> String {
    String::new()
}

/// Processes holding a file open (by inode): not tracked here.
pub fn object_holders(_object: u64, _except: &[u32]) -> Result<Vec<Holder>, String> {
    Err("Linux processes hold no kernel objects of other processes".into())
}

/// Whether `status` from `Flags` means it has not ended.
pub fn still_active(status: u32) -> bool {
    status != u32::from(b'Z') && status != u32::from(b'X')
}

pub fn why_alive(key: ProcKey) -> Result<EndReport, String> {
    let started = Instant::now();
    let stat = procfs::stat(key.pid)
        .filter(|s| key.create_time == 0 || s.create_time() == key.create_time)
        .ok_or("it has gone")?;
    let flags = Flags {
        deleting: stat.state == b'Z',
        protected: stat.kernel_thread() || key.pid == 1,
        wow64: false,
        exit_status: u32::from(stat.state),
    };
    // Signal 0 asks the kernel whether a signal would be allowed.
    let (can_end, open_error) = match unsafe { libc::kill(key.pid as libc::pid_t, 0) } {
        0 => (Some(true), None),
        _ => {
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EPERM) => (Some(false), None),
                _ => (None, Some(crate::util::error_text(&e))),
            }
        }
    };
    let holders = if flags.deleting {
        procfs::stat(stat.ppid)
            .map(|parent| Holder {
                pid: parent.pid,
                name: Arc::from(parent.comm),
                handles: Vec::new(),
                access: 0,
            })
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let guards = if stat.state == b'D' {
        std::fs::read_to_string(format!("/proc/{}/wchan", key.pid))
            .ok()
            .filter(|w| !w.is_empty() && w != "0")
            .map(|w| vec![(w, "waits in the kernel")])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(EndReport {
        key,
        flags,
        threads: stat.num_threads,
        protection: 0,
        can_end,
        open_error,
        holders,
        guards,
        took: started.elapsed(),
    })
}
