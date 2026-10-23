// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Why a program does not respond.
//!
//! - [`hung_windows`]: the windows the system itself considers not
//!   responding. It reads window state only and touches no process, so it
//!   runs every tick.
//! - [`wait_chains`]: what a process' threads are waiting for, link by
//!   link and across processes. It opens the threads, so it runs only when
//!   asked.

use crate::model::ProcKey;
use std::time::Duration;

#[cfg_attr(windows, path = "windows/waits.rs")]
#[cfg_attr(target_os = "linux", path = "linux/waits.rs")]
mod imp;
pub use imp::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HungWindow {
    pub pid: u32,
    /// The thread that stopped taking messages.
    pub tid: u32,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Blocked,
    /// Only the process is known, not the thread (an RPC or COM server).
    PidOnly,
    Owned,
    NotOwned,
    Abandoned,
    NoAccess,
    Error,
    Unknown,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Blocked => "blocked",
            Status::PidOnly => "a server process",
            Status::Owned => "owned",
            Status::NotOwned => "not owned",
            Status::Abandoned => "abandoned",
            Status::NoAccess => "no access",
            Status::Error => "error",
            Status::Unknown => "unknown",
        }
    }
}

/// What a thread can wait on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    CriticalSection,
    SendMessage,
    Mutex,
    Alpc,
    Com,
    Thread,
    Process,
    ComActivation,
    Socket,
    Smb,
    Unknown,
    /// Linux: a lock on a file (flock, fcntl), held by another process.
    FileLock,
    /// Linux: a pipe, for the program at its other end.
    Pipe,
    /// Linux: a local socket, for the program at its other end.
    LocalSocket,
    /// Linux: a lock or condition inside the process (a futex): which
    /// thread will wake it the kernel does not say.
    Futex,
    /// Linux: inside the kernel, uninterruptibly (state D).
    Kernel,
    /// Linux: stopped by a signal until it is continued.
    Stopped,
}

impl Wait {
    pub fn label(self) -> &'static str {
        match self {
            Wait::CriticalSection => "a critical section",
            Wait::SendMessage => "a window message it sent (SendMessage)",
            Wait::Mutex => "a mutex",
            Wait::Alpc => "an ALPC call (RPC)",
            Wait::Com => "a COM call",
            Wait::Thread => "a thread to exit",
            Wait::Process => "a process to exit",
            Wait::ComActivation => "a COM server to start",
            Wait::Socket => "network I/O",
            Wait::Smb => "a network file share (SMB)",
            Wait::Unknown => "something Windows cannot name",
            Wait::FileLock => "a file lock",
            Wait::Pipe => "a pipe",
            Wait::LocalSocket => "a local socket",
            Wait::Futex => "a lock inside the process (futex)",
            Wait::Kernel => "the kernel: a device or file system that has not answered",
            Wait::Stopped => "a signal to continue: it is stopped",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Thread {
        pid: u32,
        tid: u32,
        status: Status,
        context_switches: u32,
    },
    Object {
        wait: Wait,
        /// The object's name, when it has one.
        name: String,
        status: Status,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    /// Starts with the analysed thread; alternates thread, object, thread…
    pub links: Vec<Link>,
    /// The chain loops back on itself: a deadlock.
    pub cycle: bool,
}

#[derive(Clone, Debug)]
pub struct WaitReport {
    pub key: ProcKey,
    /// Threads waiting on something, deadlocks first. The thread asked
    /// about (a hung window's) comes first in its group, even when it runs.
    pub chains: Vec<Chain>,
    pub threads: usize,
    /// Threads not asked about: running, or waiting idle (for work, in a
    /// sleep), so no chain can lead out of them.
    pub idle: usize,
    /// Threads Windows could not analyse (exited meanwhile, no access).
    pub failed: usize,
    /// Threads whose chain took too long (it led into something that did
    /// not answer), or that the time limit left out.
    pub skipped: usize,
    pub took: Duration,
}
