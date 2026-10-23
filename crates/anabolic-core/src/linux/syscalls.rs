// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! System calls, the way strace shows them: every call a process makes,
//! with its arguments, its result and the time it took, from the kernel's
//! raw_syscalls tracepoints. The process is never stopped and its memory is
//! never read: the kernel reports a call's number and its six argument
//! registers. What a call passes in memory (paths, buffers) is left out,
//! but for the file names newer kernels copy into the calls' own
//! tracepoints (syscalls:sys_enter_openat and the like); descriptors are
//! named by what they are open on.
//!
//! A followed thread gets each event once per processor; the kernel copies
//! them to every thread and process it starts, and writes them into one
//! buffer per processor: what `perf trace -p` does.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::IpAddr;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::perf::{self, Attr, Field, Record, Sample, TaskRings};
use crate::procfs;

/// A system call's name by its number (x86-64).
#[cfg(target_arch = "x86_64")]
pub fn name(nr: u32) -> Option<&'static str> {
    NAMES.get(nr as usize).copied().filter(|n| !n.is_empty())
}

#[cfg(not(target_arch = "x86_64"))]
pub fn name(_nr: u32) -> Option<&'static str> {
    None
}

/// A system call's number by its name (x86-64).
#[cfg(target_arch = "x86_64")]
pub fn number(name: &str) -> Option<u32> {
    NAMES.iter().position(|n| *n == name).map(|i| i as u32)
}

#[cfg(not(target_arch = "x86_64"))]
pub fn number(_name: &str) -> Option<u32> {
    None
}

/// An errno's symbol, "ENOENT"; the kernel's own codes for a call to be
/// restarted after a signal too.
pub fn errno_name(errno: u32) -> Option<&'static str> {
    match errno {
        512 => Some("ERESTARTSYS"),
        513 => Some("ERESTARTNOINTR"),
        514 => Some("ERESTARTNOHAND"),
        515 => Some("ENOIOCTLCMD"),
        516 => Some("ERESTART_RESTARTBLOCK"),
        _ => ERRNOS
            .get(errno as usize)
            .copied()
            .filter(|n| !n.is_empty()),
    }
}

// ---------------------------------------------------------------------------
// What the arguments are.

/// How an argument reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    /// A file descriptor.
    Fd,
    /// A folder's descriptor, or AT_FDCWD.
    DirFd,
    /// A file name in the program's memory: its address, or the name when
    /// the kernel copies it.
    Path,
    Ptr,
    /// A count of bytes.
    Size,
    /// A C int.
    Int,
    /// A 64-bit number (offsets, lengths).
    Long,
    /// Flags or a number without names here.
    Hex,
    Pid,
    Mode,
    OpenFlags,
    /// O_CLOEXEC, O_NONBLOCK and the like, without an access mode.
    FdFlags,
    AtFlags,
    /// faccessat2's flags.
    AccessAt,
    AccessMode,
    Prot,
    MapFlags,
    Madvise,
    CloneFlags,
    FutexOp,
    Signal,
    SigHow,
    Family,
    SockType,
    MsgFlags,
    Shutdown,
    Fcntl,
    Whence,
    Clock,
    EpollOp,
    Ioctl,
    WaitOptions,
    Prctl,
}

/// What a call gives back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ret {
    Int,
    /// A new file descriptor.
    Fd,
    /// Bytes read or written.
    Size,
    Addr,
    Pid,
    /// It does not return (exit).
    Never,
}

/// The arguments of the calls decoded here, and what they return; others
/// show their registers as numbers.
pub fn signature(name: &str) -> Option<(&'static [Arg], Ret)> {
    use Arg::*;
    Some(match name {
        "read" | "write" => (&[Fd, Ptr, Size], Ret::Size),
        "pread64" | "pwrite64" => (&[Fd, Ptr, Size, Long], Ret::Size),
        "readv" | "writev" => (&[Fd, Ptr, Int], Ret::Size),
        "preadv" | "pwritev" => (&[Fd, Ptr, Int, Long], Ret::Size),
        "preadv2" | "pwritev2" => (&[Fd, Ptr, Int, Long, Long, Hex], Ret::Size),
        "open" => (&[Path, OpenFlags, Mode], Ret::Fd),
        "openat" => (&[DirFd, Path, OpenFlags, Mode], Ret::Fd),
        "openat2" => (&[DirFd, Path, Ptr, Size], Ret::Fd),
        "creat" => (&[Path, Mode], Ret::Fd),
        "open_by_handle_at" => (&[Fd, Ptr, OpenFlags], Ret::Fd),
        "close" => (&[Fd], Ret::Int),
        "close_range" => (&[Int, Int, Hex], Ret::Int),
        "stat" | "lstat" | "statfs" => (&[Path, Ptr], Ret::Int),
        "fstat" | "fstatfs" => (&[Fd, Ptr], Ret::Int),
        "newfstatat" => (&[DirFd, Path, Ptr, AtFlags], Ret::Int),
        "statx" => (&[DirFd, Path, AtFlags, Hex, Ptr], Ret::Int),
        "access" => (&[Path, AccessMode], Ret::Int),
        "faccessat" => (&[DirFd, Path, AccessMode], Ret::Int),
        "faccessat2" => (&[DirFd, Path, AccessMode, AccessAt], Ret::Int),
        "readlink" => (&[Path, Ptr, Size], Ret::Int),
        "readlinkat" => (&[DirFd, Path, Ptr, Size], Ret::Int),
        "lseek" => (&[Fd, Long, Whence], Ret::Int),
        "mmap" => (&[Ptr, Size, Prot, MapFlags, Fd, Hex], Ret::Addr),
        "munmap" | "mlock" | "munlock" => (&[Ptr, Size], Ret::Int),
        "mprotect" => (&[Ptr, Size, Prot], Ret::Int),
        "mremap" => (&[Ptr, Size, Size, Hex, Ptr], Ret::Addr),
        "madvise" => (&[Ptr, Size, Madvise], Ret::Int),
        "msync" => (&[Ptr, Size, Hex], Ret::Int),
        "brk" => (&[Ptr], Ret::Addr),
        "ioctl" => (&[Fd, Ioctl, Ptr], Ret::Int),
        "poll" => (&[Ptr, Int, Int], Ret::Int),
        "ppoll" => (&[Ptr, Int, Ptr, Ptr, Size], Ret::Int),
        "select" => (&[Int, Ptr, Ptr, Ptr, Ptr], Ret::Int),
        "pselect6" => (&[Int, Ptr, Ptr, Ptr, Ptr, Ptr], Ret::Int),
        "epoll_create" => (&[Int], Ret::Fd),
        "epoll_create1" | "inotify_init1" => (&[FdFlags], Ret::Fd),
        "epoll_ctl" => (&[Fd, EpollOp, Fd, Ptr], Ret::Int),
        "epoll_wait" => (&[Fd, Ptr, Int, Int], Ret::Int),
        "epoll_pwait" => (&[Fd, Ptr, Int, Int, Ptr, Size], Ret::Int),
        "epoll_pwait2" => (&[Fd, Ptr, Int, Ptr, Ptr, Size], Ret::Int),
        "futex" => (&[Ptr, FutexOp, Int, Ptr, Ptr, Int], Ret::Int),
        "futex_wait" => (&[Ptr, Hex, Hex, Hex, Ptr, Clock], Ret::Int),
        "futex_wake" => (&[Ptr, Hex, Int, Hex], Ret::Int),
        "futex_waitv" => (&[Ptr, Int, Hex, Ptr, Clock], Ret::Int),
        "set_robust_list" => (&[Ptr, Size], Ret::Int),
        "set_tid_address" => (&[Ptr], Ret::Pid),
        "rseq" => (&[Ptr, Size, Hex, Hex], Ret::Int),
        "clone" => (&[CloneFlags, Ptr, Ptr, Ptr, Ptr], Ret::Pid),
        "clone3" => (&[Ptr, Size], Ret::Pid),
        "fork" | "vfork" => (&[], Ret::Pid),
        "execve" => (&[Path, Ptr, Ptr], Ret::Int),
        "execveat" => (&[DirFd, Path, Ptr, Ptr, AtFlags], Ret::Int),
        "exit" | "exit_group" => (&[Int], Ret::Never),
        "wait4" => (&[Pid, Ptr, WaitOptions, Ptr], Ret::Pid),
        "waitid" => (&[Int, Pid, Ptr, WaitOptions, Ptr], Ret::Int),
        "kill" | "tkill" => (&[Pid, Signal], Ret::Int),
        "tgkill" => (&[Pid, Pid, Signal], Ret::Int),
        "rt_sigaction" => (&[Signal, Ptr, Ptr, Size], Ret::Int),
        "rt_sigprocmask" => (&[SigHow, Ptr, Ptr, Size], Ret::Int),
        "rt_sigtimedwait" => (&[Ptr, Ptr, Ptr, Size], Ret::Int),
        "rt_sigsuspend" => (&[Ptr, Size], Ret::Int),
        "sigaltstack" => (&[Ptr, Ptr], Ret::Int),
        "rt_sigreturn" | "pause" | "sync" | "sched_yield" | "getuid" | "geteuid" | "getgid"
        | "getegid" => (&[], Ret::Int),
        "getpid" | "gettid" | "getppid" | "getpgrp" | "setsid" => (&[], Ret::Pid),
        "socket" => (&[Family, SockType, Int], Ret::Fd),
        "socketpair" => (&[Family, SockType, Int, Ptr], Ret::Int),
        "connect" | "bind" => (&[Fd, Ptr, Int], Ret::Int),
        "accept" => (&[Fd, Ptr, Ptr], Ret::Fd),
        "accept4" => (&[Fd, Ptr, Ptr, FdFlags], Ret::Fd),
        "listen" => (&[Fd, Int], Ret::Int),
        "shutdown" => (&[Fd, Shutdown], Ret::Int),
        "sendto" => (&[Fd, Ptr, Size, MsgFlags, Ptr, Int], Ret::Size),
        "recvfrom" => (&[Fd, Ptr, Size, MsgFlags, Ptr, Ptr], Ret::Size),
        "sendmsg" | "recvmsg" => (&[Fd, Ptr, MsgFlags], Ret::Size),
        "sendmmsg" => (&[Fd, Ptr, Int, MsgFlags], Ret::Int),
        "recvmmsg" => (&[Fd, Ptr, Int, MsgFlags, Ptr], Ret::Int),
        "getsockopt" => (&[Fd, Int, Int, Ptr, Ptr], Ret::Int),
        "setsockopt" => (&[Fd, Int, Int, Ptr, Int], Ret::Int),
        "getsockname" | "getpeername" => (&[Fd, Ptr, Ptr], Ret::Int),
        "fcntl" => (&[Fd, Fcntl, Hex], Ret::Int),
        "dup" => (&[Fd], Ret::Fd),
        "dup2" => (&[Fd, Fd], Ret::Fd),
        "dup3" => (&[Fd, Fd, FdFlags], Ret::Fd),
        "pipe" => (&[Ptr], Ret::Int),
        "pipe2" => (&[Ptr, FdFlags], Ret::Int),
        "getdents" | "getdents64" => (&[Fd, Ptr, Size], Ret::Int),
        "fsync" | "fdatasync" | "syncfs" | "fchdir" => (&[Fd], Ret::Int),
        "ftruncate" => (&[Fd, Long], Ret::Int),
        "truncate" => (&[Path, Long], Ret::Int),
        "fallocate" => (&[Fd, Hex, Long, Long], Ret::Int),
        "fadvise64" => (&[Fd, Long, Long, Int], Ret::Int),
        "flock" => (&[Fd, Hex], Ret::Int),
        "unlink" | "rmdir" | "chdir" | "chroot" => (&[Path], Ret::Int),
        "unlinkat" => (&[DirFd, Path, AtFlags], Ret::Int),
        "rename" | "link" | "symlink" => (&[Path, Path], Ret::Int),
        "renameat" => (&[DirFd, Path, DirFd, Path], Ret::Int),
        "renameat2" => (&[DirFd, Path, DirFd, Path, Hex], Ret::Int),
        "linkat" => (&[DirFd, Path, DirFd, Path, AtFlags], Ret::Int),
        "symlinkat" => (&[Path, DirFd, Path], Ret::Int),
        "mkdir" | "chmod" => (&[Path, Mode], Ret::Int),
        "mkdirat" | "fchmodat" => (&[DirFd, Path, Mode], Ret::Int),
        "fchmod" => (&[Fd, Mode], Ret::Int),
        "chown" | "lchown" => (&[Path, Int, Int], Ret::Int),
        "fchown" => (&[Fd, Int, Int], Ret::Int),
        "fchownat" => (&[DirFd, Path, Int, Int, AtFlags], Ret::Int),
        "utimensat" => (&[DirFd, Path, Ptr, AtFlags], Ret::Int),
        "getxattr" | "lgetxattr" => (&[Path, Ptr, Ptr, Size], Ret::Int),
        "fgetxattr" => (&[Fd, Ptr, Ptr, Size], Ret::Int),
        "getcwd" => (&[Ptr, Size], Ret::Int),
        "umask" => (&[Mode], Ret::Int),
        "nanosleep" | "gettimeofday" => (&[Ptr, Ptr], Ret::Int),
        "clock_nanosleep" => (&[Clock, Hex, Ptr, Ptr], Ret::Int),
        "clock_gettime" | "clock_getres" => (&[Clock, Ptr], Ret::Int),
        "setpgid" => (&[Pid, Pid], Ret::Int),
        "getpgid" | "getsid" => (&[Pid], Ret::Pid),
        "sched_getaffinity" | "sched_setaffinity" => (&[Pid, Size, Ptr], Ret::Int),
        "sched_setscheduler" => (&[Pid, Int, Ptr], Ret::Int),
        "sched_getscheduler" => (&[Pid], Ret::Int),
        "getrandom" => (&[Ptr, Size, Hex], Ret::Size),
        "memfd_create" => (&[Ptr, Hex], Ret::Fd),
        "eventfd2" => (&[Int, FdFlags], Ret::Fd),
        "timerfd_create" => (&[Clock, FdFlags], Ret::Fd),
        "timerfd_settime" => (&[Fd, Hex, Ptr, Ptr], Ret::Int),
        "timerfd_gettime" => (&[Fd, Ptr], Ret::Int),
        "signalfd4" => (&[Fd, Ptr, Size, FdFlags], Ret::Fd),
        "inotify_add_watch" => (&[Fd, Path, Hex], Ret::Int),
        "inotify_rm_watch" => (&[Fd, Int], Ret::Int),
        "prctl" => (&[Prctl, Hex, Hex, Hex, Hex], Ret::Int),
        "arch_prctl" => (&[Hex, Ptr], Ret::Int),
        "prlimit64" => (&[Pid, Int, Ptr, Ptr], Ret::Int),
        "getrlimit" | "setrlimit" | "getrusage" => (&[Int, Ptr], Ret::Int),
        "sysinfo" | "uname" | "times" => (&[Ptr], Ret::Int),
        "sendfile" => (&[Fd, Fd, Ptr, Size], Ret::Size),
        "copy_file_range" | "splice" => (&[Fd, Ptr, Fd, Ptr, Size, Hex], Ret::Size),
        "io_uring_setup" => (&[Int, Ptr], Ret::Fd),
        "io_uring_enter" => (&[Fd, Int, Int, Hex, Ptr, Size], Ret::Int),
        "io_uring_register" => (&[Fd, Int, Ptr, Int], Ret::Int),
        "pidfd_open" => (&[Pid, Hex], Ret::Fd),
        "pidfd_send_signal" => (&[Fd, Signal, Ptr, Hex], Ret::Int),
        "pidfd_getfd" => (&[Fd, Int, Hex], Ret::Fd),
        "process_vm_readv" | "process_vm_writev" => (&[Pid, Ptr, Int, Ptr, Int, Hex], Ret::Size),
        "ptrace" => (&[Int, Pid, Ptr, Ptr], Ret::Int),
        "perf_event_open" => (&[Ptr, Pid, Int, Fd, Hex], Ret::Fd),
        "membarrier" => (&[Hex, Hex, Int], Ret::Int),
        _ => return None,
    })
}

/// The calls that return the bytes they read or wrote.
fn moves_bytes(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "readv"
            | "pread64"
            | "preadv"
            | "preadv2"
            | "recvfrom"
            | "recvmsg"
            | "write"
            | "writev"
            | "pwrite64"
            | "pwritev"
            | "pwritev2"
            | "sendto"
            | "sendmsg"
            | "sendfile"
            | "copy_file_range"
            | "splice"
    )
}

/// The calls whose file names newer kernels copy into their own
/// tracepoints, followed for that: each costs a kernel event per thread and
/// processor.
const NAMED: [&str; 10] = [
    "openat",
    "open",
    "execve",
    "newfstatat",
    "statx",
    "access",
    "faccessat2",
    "readlinkat",
    "unlinkat",
    "renameat2",
];

// ---------------------------------------------------------------------------
// Reading arguments.

/// Flags by name, in order (combinations first); what is left in hex.
fn bits(out: &mut String, mut v: u64, names: &[(u64, &str)]) {
    for &(bit, name) in names {
        if bit != 0 && v & bit == bit {
            if !out.is_empty() {
                out.push('|');
            }
            out.push_str(name);
            v &= !bit;
        }
    }
    if v != 0 {
        if !out.is_empty() {
            out.push('|');
        }
        out.push_str(&format!("{v:#x}"));
    }
    if out.is_empty() {
        out.push('0');
    }
}

fn flags(v: u64, names: &[(u64, &str)]) -> String {
    let mut out = String::new();
    bits(&mut out, v, names);
    out
}

/// A value by name, else the number.
fn named(v: i64, names: &[(i64, &str)]) -> String {
    names
        .iter()
        .find(|(n, _)| *n == v)
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| v.to_string())
}

const fn c(v: libc::c_int) -> u64 {
    v as u32 as u64
}

const OPEN_FLAGS: [(u64, &str); 19] = [
    (c(libc::O_TMPFILE), "O_TMPFILE"),
    (c(libc::O_SYNC), "O_SYNC"),
    (c(libc::O_CREAT), "O_CREAT"),
    (c(libc::O_EXCL), "O_EXCL"),
    (c(libc::O_NOCTTY), "O_NOCTTY"),
    (c(libc::O_TRUNC), "O_TRUNC"),
    (c(libc::O_APPEND), "O_APPEND"),
    (c(libc::O_NONBLOCK), "O_NONBLOCK"),
    (c(libc::O_DSYNC), "O_DSYNC"),
    (c(libc::O_ASYNC), "O_ASYNC"),
    (c(libc::O_DIRECT), "O_DIRECT"),
    (c(libc::O_LARGEFILE), "O_LARGEFILE"),
    (c(libc::O_DIRECTORY), "O_DIRECTORY"),
    (c(libc::O_NOFOLLOW), "O_NOFOLLOW"),
    (c(libc::O_NOATIME), "O_NOATIME"),
    (c(libc::O_CLOEXEC), "O_CLOEXEC"),
    (c(libc::O_PATH), "O_PATH"),
    // The kernel's own, on x86-64: what glibc leaves out of O_LARGEFILE.
    (0o100000, "O_LARGEFILE"),
    (0, ""),
];

const MAP_FLAGS: [(u64, &str); 17] = [
    (c(libc::MAP_SHARED_VALIDATE), "MAP_SHARED_VALIDATE"),
    (c(libc::MAP_SHARED), "MAP_SHARED"),
    (c(libc::MAP_PRIVATE), "MAP_PRIVATE"),
    (c(libc::MAP_FIXED_NOREPLACE), "MAP_FIXED_NOREPLACE"),
    (c(libc::MAP_FIXED), "MAP_FIXED"),
    (c(libc::MAP_ANONYMOUS), "MAP_ANONYMOUS"),
    (c(libc::MAP_GROWSDOWN), "MAP_GROWSDOWN"),
    (c(libc::MAP_DENYWRITE), "MAP_DENYWRITE"),
    (c(libc::MAP_EXECUTABLE), "MAP_EXECUTABLE"),
    (c(libc::MAP_LOCKED), "MAP_LOCKED"),
    (c(libc::MAP_NORESERVE), "MAP_NORESERVE"),
    (c(libc::MAP_POPULATE), "MAP_POPULATE"),
    (c(libc::MAP_NONBLOCK), "MAP_NONBLOCK"),
    (c(libc::MAP_STACK), "MAP_STACK"),
    (c(libc::MAP_HUGETLB), "MAP_HUGETLB"),
    (c(libc::MAP_SYNC), "MAP_SYNC"),
    (0, ""),
];

const CLONE_FLAGS: [(u64, &str); 24] = [
    (c(libc::CLONE_VM), "CLONE_VM"),
    (c(libc::CLONE_FS), "CLONE_FS"),
    (c(libc::CLONE_FILES), "CLONE_FILES"),
    (c(libc::CLONE_SIGHAND), "CLONE_SIGHAND"),
    (c(libc::CLONE_PIDFD), "CLONE_PIDFD"),
    (c(libc::CLONE_PTRACE), "CLONE_PTRACE"),
    (c(libc::CLONE_VFORK), "CLONE_VFORK"),
    (c(libc::CLONE_PARENT), "CLONE_PARENT"),
    (c(libc::CLONE_THREAD), "CLONE_THREAD"),
    (c(libc::CLONE_NEWNS), "CLONE_NEWNS"),
    (c(libc::CLONE_SYSVSEM), "CLONE_SYSVSEM"),
    (c(libc::CLONE_SETTLS), "CLONE_SETTLS"),
    (c(libc::CLONE_PARENT_SETTID), "CLONE_PARENT_SETTID"),
    (c(libc::CLONE_CHILD_CLEARTID), "CLONE_CHILD_CLEARTID"),
    (c(libc::CLONE_DETACHED), "CLONE_DETACHED"),
    (c(libc::CLONE_UNTRACED), "CLONE_UNTRACED"),
    (c(libc::CLONE_CHILD_SETTID), "CLONE_CHILD_SETTID"),
    (c(libc::CLONE_NEWCGROUP), "CLONE_NEWCGROUP"),
    (c(libc::CLONE_NEWUTS), "CLONE_NEWUTS"),
    (c(libc::CLONE_NEWIPC), "CLONE_NEWIPC"),
    (c(libc::CLONE_NEWUSER), "CLONE_NEWUSER"),
    (c(libc::CLONE_NEWPID), "CLONE_NEWPID"),
    (c(libc::CLONE_NEWNET), "CLONE_NEWNET"),
    (c(libc::CLONE_IO), "CLONE_IO"),
];

const AT_FLAGS: [(u64, &str); 8] = [
    (0x100, "AT_SYMLINK_NOFOLLOW"),
    (0x200, "AT_REMOVEDIR"),
    (0x400, "AT_SYMLINK_FOLLOW"),
    (0x800, "AT_NO_AUTOMOUNT"),
    (0x1000, "AT_EMPTY_PATH"),
    (0x2000, "AT_STATX_FORCE_SYNC"),
    (0x4000, "AT_STATX_DONT_SYNC"),
    (0x8000, "AT_RECURSIVE"),
];

const MSG_FLAGS: [(u64, &str); 12] = [
    (0x1, "MSG_OOB"),
    (0x2, "MSG_PEEK"),
    (0x4, "MSG_DONTROUTE"),
    (0x8, "MSG_CTRUNC"),
    (0x20, "MSG_TRUNC"),
    (0x40, "MSG_DONTWAIT"),
    (0x80, "MSG_EOR"),
    (0x100, "MSG_WAITALL"),
    (0x4000, "MSG_NOSIGNAL"),
    (0x8000, "MSG_MORE"),
    (0x10000, "MSG_WAITFORONE"),
    (0x4000_0000, "MSG_CMSG_CLOEXEC"),
];

/// x86 numbering.
const SIGNALS: [&str; 32] = [
    "0",
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGILL",
    "SIGTRAP",
    "SIGABRT",
    "SIGBUS",
    "SIGFPE",
    "SIGKILL",
    "SIGUSR1",
    "SIGSEGV",
    "SIGUSR2",
    "SIGPIPE",
    "SIGALRM",
    "SIGTERM",
    "SIGSTKFLT",
    "SIGCHLD",
    "SIGCONT",
    "SIGSTOP",
    "SIGTSTP",
    "SIGTTIN",
    "SIGTTOU",
    "SIGURG",
    "SIGXCPU",
    "SIGXFSZ",
    "SIGVTALRM",
    "SIGPROF",
    "SIGWINCH",
    "SIGIO",
    "SIGPWR",
    "SIGSYS",
];

fn signal(v: u64) -> String {
    let n = v as u32 as i32;
    match n {
        0..32 => SIGNALS[n as usize].to_string(),
        32..=64 => format!("SIGRT_{}", n - 32),
        _ => n.to_string(),
    }
}

const FUTEX_OPS: [&str; 14] = [
    "FUTEX_WAIT",
    "FUTEX_WAKE",
    "FUTEX_FD",
    "FUTEX_REQUEUE",
    "FUTEX_CMP_REQUEUE",
    "FUTEX_WAKE_OP",
    "FUTEX_LOCK_PI",
    "FUTEX_UNLOCK_PI",
    "FUTEX_TRYLOCK_PI",
    "FUTEX_WAIT_BITSET",
    "FUTEX_WAKE_BITSET",
    "FUTEX_WAIT_REQUEUE_PI",
    "FUTEX_CMP_REQUEUE_PI",
    "FUTEX_LOCK_PI2",
];

fn futex_op(v: u64) -> String {
    let v = v as u32;
    let Some(name) = FUTEX_OPS.get((v & 127) as usize) else {
        return format!("{v:#x}");
    };
    let mut out = name.to_string();
    if v & 128 != 0 {
        out.push_str("_PRIVATE");
    }
    if v & 256 != 0 {
        out.push_str("|FUTEX_CLOCK_REALTIME");
    }
    if v & !(127 | 128 | 256) != 0 {
        out.push_str(&format!("|{:#x}", v & !(127 | 128 | 256)));
    }
    out
}

fn ioctl(v: u64) -> String {
    let v = v as u32;
    let known = match v {
        0x5401 => "TCGETS",
        0x5402 => "TCSETS",
        0x5403 => "TCSETSW",
        0x5404 => "TCSETSF",
        0x540E => "TIOCSCTTY",
        0x540F => "TIOCGPGRP",
        0x5410 => "TIOCSPGRP",
        0x5413 => "TIOCGWINSZ",
        0x5414 => "TIOCSWINSZ",
        0x541B => "FIONREAD",
        0x5421 => "FIONBIO",
        0x5450 => "FIONCLEX",
        0x5451 => "FIOCLEX",
        0x5452 => "FIOASYNC",
        0x8004_5430 => "TIOCGPTN",
        0x4004_5431 => "TIOCSPTLCK",
        0x8008_6601 => "FS_IOC_GETFLAGS",
        0x4008_6602 => "FS_IOC_SETFLAGS",
        0x9409 => "BTRFS_IOC_CLONE",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    // _IOC(dir, type, nr, size): the type is a letter a driver picked.
    let kind = (v >> 8) & 0xff;
    match char::from_u32(kind).filter(|c| c.is_ascii_graphic()) {
        Some(letter) => format!("{v:#x} ('{letter}' {:#x})", v & 0xff),
        None => format!("{v:#x}"),
    }
}

/// An argument as strace would write it.
pub fn format_arg(arg: Arg, v: u64) -> String {
    let int = v as u32 as i32;
    match arg {
        Arg::Fd | Arg::Int | Arg::Pid => int.to_string(),
        Arg::DirFd if int == libc::AT_FDCWD => "AT_FDCWD".into(),
        Arg::DirFd => int.to_string(),
        Arg::Path | Arg::Ptr if v == 0 => "NULL".into(),
        Arg::Path | Arg::Ptr => format!("{v:#x}"),
        Arg::Size => v.to_string(),
        Arg::Long => (v as i64).to_string(),
        Arg::Hex if v == 0 => "0".into(),
        Arg::Hex => format!("{v:#x}"),
        Arg::Mode if v & 0o7777 == 0 => "0".into(),
        Arg::Mode => format!("0{:o}", v & 0o7777),
        Arg::OpenFlags => {
            let v = u64::from(v as u32);
            let mut out = match v & 3 {
                0 => "O_RDONLY",
                1 => "O_WRONLY",
                2 => "O_RDWR",
                _ => "O_ACCMODE",
            }
            .to_string();
            if v & !3 != 0 {
                bits(&mut out, v & !3, &OPEN_FLAGS);
            }
            out
        }
        Arg::FdFlags => flags(u64::from(v as u32), &OPEN_FLAGS),
        Arg::AtFlags => flags(u64::from(v as u32), &AT_FLAGS),
        Arg::AccessAt => flags(
            u64::from(v as u32),
            &[
                (0x200, "AT_EACCESS"),
                (0x100, "AT_SYMLINK_NOFOLLOW"),
                (0x1000, "AT_EMPTY_PATH"),
            ],
        ),
        Arg::AccessMode if v == 0 => "F_OK".into(),
        Arg::AccessMode => flags(v, &[(4, "R_OK"), (2, "W_OK"), (1, "X_OK")]),
        Arg::Prot if v == 0 => "PROT_NONE".into(),
        Arg::Prot => flags(
            v,
            &[
                (1, "PROT_READ"),
                (2, "PROT_WRITE"),
                (4, "PROT_EXEC"),
                (0x0100_0000, "PROT_GROWSDOWN"),
                (0x0200_0000, "PROT_GROWSUP"),
            ],
        ),
        Arg::MapFlags => flags(u64::from(v as u32), &MAP_FLAGS),
        Arg::Madvise => named(
            int.into(),
            &[
                (0, "MADV_NORMAL"),
                (1, "MADV_RANDOM"),
                (2, "MADV_SEQUENTIAL"),
                (3, "MADV_WILLNEED"),
                (4, "MADV_DONTNEED"),
                (8, "MADV_FREE"),
                (9, "MADV_REMOVE"),
                (10, "MADV_DONTFORK"),
                (11, "MADV_DOFORK"),
                (12, "MADV_MERGEABLE"),
                (13, "MADV_UNMERGEABLE"),
                (14, "MADV_HUGEPAGE"),
                (15, "MADV_NOHUGEPAGE"),
                (16, "MADV_DONTDUMP"),
                (17, "MADV_DODUMP"),
                (18, "MADV_WIPEONFORK"),
                (19, "MADV_KEEPONFORK"),
                (20, "MADV_COLD"),
                (21, "MADV_PAGEOUT"),
                (22, "MADV_POPULATE_READ"),
                (23, "MADV_POPULATE_WRITE"),
                (24, "MADV_DONTNEED_LOCKED"),
                (25, "MADV_COLLAPSE"),
                (102, "MADV_GUARD_INSTALL"),
                (103, "MADV_GUARD_REMOVE"),
            ],
        ),
        Arg::CloneFlags => {
            let mut out = String::new();
            bits(&mut out, v & !0xff, &CLONE_FLAGS);
            if out == "0" {
                out.clear();
            }
            if v & 0xff != 0 {
                if !out.is_empty() {
                    out.push('|');
                }
                out.push_str(&signal(v & 0xff));
            }
            if out.is_empty() {
                out.push('0');
            }
            out
        }
        Arg::FutexOp => futex_op(v),
        Arg::Signal => signal(v),
        Arg::SigHow => named(
            int.into(),
            &[(0, "SIG_BLOCK"), (1, "SIG_UNBLOCK"), (2, "SIG_SETMASK")],
        ),
        Arg::Family => named(
            int.into(),
            &[
                (0, "AF_UNSPEC"),
                (1, "AF_UNIX"),
                (2, "AF_INET"),
                (10, "AF_INET6"),
                (16, "AF_NETLINK"),
                (17, "AF_PACKET"),
                (31, "AF_BLUETOOTH"),
                (38, "AF_ALG"),
                (40, "AF_VSOCK"),
                (44, "AF_XDP"),
            ],
        ),
        Arg::SockType => {
            let v = u64::from(v as u32);
            let mut out = named(
                (v & 0xf) as i64,
                &[
                    (1, "SOCK_STREAM"),
                    (2, "SOCK_DGRAM"),
                    (3, "SOCK_RAW"),
                    (4, "SOCK_RDM"),
                    (5, "SOCK_SEQPACKET"),
                    (6, "SOCK_DCCP"),
                    (10, "SOCK_PACKET"),
                ],
            );
            if v & !0xf != 0 {
                bits(
                    &mut out,
                    v & !0xf,
                    &[
                        (c(libc::SOCK_NONBLOCK), "SOCK_NONBLOCK"),
                        (c(libc::SOCK_CLOEXEC), "SOCK_CLOEXEC"),
                    ],
                );
            }
            out
        }
        Arg::MsgFlags => flags(u64::from(v as u32), &MSG_FLAGS),
        Arg::Shutdown => named(
            int.into(),
            &[(0, "SHUT_RD"), (1, "SHUT_WR"), (2, "SHUT_RDWR")],
        ),
        Arg::Fcntl => named(
            int.into(),
            &[
                (0, "F_DUPFD"),
                (1, "F_GETFD"),
                (2, "F_SETFD"),
                (3, "F_GETFL"),
                (4, "F_SETFL"),
                (5, "F_GETLK"),
                (6, "F_SETLK"),
                (7, "F_SETLKW"),
                (8, "F_SETOWN"),
                (9, "F_GETOWN"),
                (10, "F_SETSIG"),
                (11, "F_GETSIG"),
                (15, "F_SETOWN_EX"),
                (16, "F_GETOWN_EX"),
                (36, "F_OFD_GETLK"),
                (37, "F_OFD_SETLK"),
                (38, "F_OFD_SETLKW"),
                (1024, "F_SETLEASE"),
                (1025, "F_GETLEASE"),
                (1026, "F_NOTIFY"),
                (1030, "F_DUPFD_CLOEXEC"),
                (1031, "F_SETPIPE_SZ"),
                (1032, "F_GETPIPE_SZ"),
                (1033, "F_ADD_SEALS"),
                (1034, "F_GET_SEALS"),
            ],
        ),
        Arg::Whence => named(
            int.into(),
            &[
                (0, "SEEK_SET"),
                (1, "SEEK_CUR"),
                (2, "SEEK_END"),
                (3, "SEEK_DATA"),
                (4, "SEEK_HOLE"),
            ],
        ),
        Arg::Clock => named(
            int.into(),
            &[
                (0, "CLOCK_REALTIME"),
                (1, "CLOCK_MONOTONIC"),
                (2, "CLOCK_PROCESS_CPUTIME_ID"),
                (3, "CLOCK_THREAD_CPUTIME_ID"),
                (4, "CLOCK_MONOTONIC_RAW"),
                (5, "CLOCK_REALTIME_COARSE"),
                (6, "CLOCK_MONOTONIC_COARSE"),
                (7, "CLOCK_BOOTTIME"),
                (8, "CLOCK_REALTIME_ALARM"),
                (9, "CLOCK_BOOTTIME_ALARM"),
                (11, "CLOCK_TAI"),
            ],
        ),
        Arg::EpollOp => named(
            int.into(),
            &[
                (1, "EPOLL_CTL_ADD"),
                (2, "EPOLL_CTL_DEL"),
                (3, "EPOLL_CTL_MOD"),
            ],
        ),
        Arg::Ioctl => ioctl(v),
        Arg::WaitOptions => flags(
            u64::from(v as u32),
            &[
                (1, "WNOHANG"),
                (2, "WUNTRACED"),
                (4, "WEXITED"),
                (8, "WCONTINUED"),
                (0x0100_0000, "WNOWAIT"),
                (0x2000_0000, "__WNOTHREAD"),
                (0x4000_0000, "__WALL"),
                (0x8000_0000, "__WCLONE"),
            ],
        ),
        Arg::Prctl => named(
            int.into(),
            &[
                (1, "PR_SET_PDEATHSIG"),
                (2, "PR_GET_PDEATHSIG"),
                (3, "PR_GET_DUMPABLE"),
                (4, "PR_SET_DUMPABLE"),
                (15, "PR_SET_NAME"),
                (16, "PR_GET_NAME"),
                (22, "PR_SET_SECCOMP"),
                (23, "PR_CAPBSET_READ"),
                (29, "PR_SET_TIMERSLACK"),
                (35, "PR_SET_MM"),
                (36, "PR_SET_CHILD_SUBREAPER"),
                (38, "PR_SET_NO_NEW_PRIVS"),
                (39, "PR_GET_NO_NEW_PRIVS"),
                (47, "PR_CAP_AMBIENT"),
                (0x5356_4d41, "PR_SET_VMA"),
            ],
        ),
    }
}

/// A name as C would write it, cut short when long.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for (i, ch) in text.chars().enumerate() {
        if i == 200 {
            out.push_str("\"…");
            return out;
        }
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Calls.

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Returned(i64),
    /// exit and exit_group: it does not come back.
    Gone,
    /// Still inside when the recording stopped.
    Waiting,
    /// Its return was lost: the trace fell behind.
    Unknown,
}

/// File names the kernel copied, by the argument each is.
pub type Copied = Vec<(u8, Box<str>)>;

/// One system call.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub pid: u32,
    pub tid: u32,
    pub nr: u32,
    /// When it began (CLOCK_MONOTONIC, ns); when it ended if its start was
    /// not seen.
    pub time: u64,
    /// When it ended, or was last known to be inside.
    pub end: u64,
    /// The argument registers; None when its start was not seen (it began
    /// before the recording).
    pub args: Option<[u64; 6]>,
    /// The file names the kernel copied, by argument.
    pub names: Copied,
    pub outcome: Outcome,
}

/// What a call is about: rows of the same thing merge.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Object {
    None,
    Fd(i32),
    /// A descriptor it made.
    NewFd(i32),
    /// A file it named.
    Name(Box<str>),
    Pid(i32),
    Addr(u64),
}

impl Call {
    pub fn name(&self) -> &'static str {
        name(self.nr).unwrap_or("?")
    }

    fn signature(&self) -> Option<(&'static [Arg], Ret)> {
        signature(self.name())
    }

    pub fn ret(&self) -> Option<i64> {
        match self.outcome {
            Outcome::Returned(r) => Some(r),
            _ => None,
        }
    }

    /// The error it failed with.
    pub fn errno(&self) -> Option<u32> {
        self.ret()
            .filter(|r| (-4095..0).contains(r))
            .map(|r| (-r) as u32)
    }

    /// 0 for success, else the errno; None without a result.
    pub fn status(&self) -> Option<u32> {
        self.ret().map(|_| self.errno().unwrap_or(0))
    }

    /// Time inside it, when both ends were seen.
    pub fn took(&self) -> Option<Duration> {
        (self.args.is_some() && self.ret().is_some())
            .then(|| Duration::from_nanos(self.end.saturating_sub(self.time)))
    }

    /// Bytes read or written.
    pub fn bytes(&self) -> u64 {
        match self.ret() {
            Some(r) if r > 0 && moves_bytes(self.name()) => r as u64,
            _ => 0,
        }
    }

    fn arg(&self, i: usize) -> Option<u64> {
        self.args.map(|a| a[i])
    }

    pub fn object(&self) -> Object {
        let Some(args) = &self.args else {
            return Object::None;
        };
        let Some((sig, ret)) = self.signature() else {
            return Object::None;
        };
        if let Some((_, name)) = self.names.first() {
            return Object::Name(name.clone());
        }
        if let Some(i) = sig.iter().position(|a| *a == Arg::Fd) {
            return Object::Fd(args[i] as u32 as i32);
        }
        let made = self.ret().filter(|r| (0..=i64::from(i32::MAX)).contains(r));
        if let Some(r) = made {
            match ret {
                Ret::Fd => return Object::NewFd(r as i32),
                Ret::Pid if matches!(self.name(), "clone" | "clone3" | "fork" | "vfork") => {
                    return Object::Pid(r as i32);
                }
                _ => {}
            }
        }
        if let Some(i) = sig.iter().position(|a| *a == Arg::Pid) {
            return Object::Pid(args[i] as u32 as i32);
        }
        if self.name().starts_with("futex") {
            return Object::Addr(args[0]);
        }
        Object::None
    }

    /// The call as strace writes it: `openat(AT_FDCWD, "/etc/hosts",
    /// O_RDONLY|O_CLOEXEC) = 3`.
    pub fn text(&self) -> String {
        let name = self.name();
        let mut out = match name {
            "?" => format!("syscall_{}(", self.nr),
            _ => format!("{name}("),
        };
        let sig = self.signature();
        match (&self.args, sig) {
            (None, _) => out.push('…'),
            (Some(args), Some((sig, _))) => {
                // A mode only matters when a file is made.
                let makes = |flags: u64| {
                    let flags = flags as u32 as i32;
                    flags & libc::O_CREAT != 0 || flags & libc::O_TMPFILE == libc::O_TMPFILE
                };
                let sig = match name {
                    "open" | "openat" if !makes(args[sig.len() - 2]) => &sig[..sig.len() - 1],
                    _ => sig,
                };
                for (i, arg) in sig.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    match self.names.iter().find(|(at, _)| *at as usize == i) {
                        Some((_, text)) => out.push_str(&quoted(text)),
                        None => out.push_str(&format_arg(*arg, args[i])),
                    }
                }
            }
            (Some(args), None) => {
                // Unknown arguments: the registers, up to the last one set.
                let used = args.iter().rposition(|&a| a != 0).map_or(0, |i| i + 1);
                for (i, &a) in args[..used].iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&if a < 4096 {
                        a.to_string()
                    } else {
                        format!("{a:#x}")
                    });
                }
            }
        }
        out.push(')');
        let ret_kind = sig.map_or(Ret::Int, |s| s.1);
        match self.outcome {
            Outcome::Returned(r) => {
                out.push_str(" = ");
                match self.errno() {
                    Some(e) => {
                        out.push_str("-1 ");
                        out.push_str(&errno_name(e).map_or_else(|| e.to_string(), str::to_string));
                    }
                    None if ret_kind == Ret::Addr => out.push_str(&format!("{r:#x}")),
                    None => out.push_str(&r.to_string()),
                }
            }
            Outcome::Gone => {}
            Outcome::Waiting | Outcome::Unknown => out.push_str(" = ?"),
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Naming descriptors.

/// What the followed processes' descriptors are open on, learned as their
/// calls go by.
#[derive(Default)]
pub struct Names {
    /// (process, descriptor) -> what /proc said it was open on.
    fds: HashMap<(u32, i32), Arc<str>>,
    /// The last time each descriptor is closed among the records at hand:
    /// what /proc says now is not what an earlier call used.
    closing: HashMap<(u32, i32), u64>,
    /// The descriptor the call at hand made, already named.
    fresh: Option<(u32, i32)>,
    /// Sockets by inode: their connection, in words.
    sockets: HashMap<u64, Arc<str>>,
    sockets_read: Option<Instant>,
}

impl Names {
    /// What a call is about, in words: the file or connection behind its
    /// descriptor, the file it names, the process it acts on.
    pub fn path(&mut self, call: &Call, object: &Object) -> Arc<str> {
        let pid = call.pid;
        match object {
            Object::Fd(fd) if *fd >= 0 => self.fd(pid, *fd, call.end, false),
            Object::NewFd(fd) => self.fd(pid, *fd, call.end, true),
            Object::Name(name) => self.full(call, name).into(),
            Object::Pid(p) if *p > 0 => match procfs::stat(*p as u32) {
                Some(s) => format!("PID {p} ({})", s.comm).into(),
                None => format!("PID {p}").into(),
            },
            Object::Addr(a) => format!("{a:#x}").into(),
            _ if matches!(call.name(), "execve" | "execveat") && call.status() == Some(0) => {
                procfs::exe(pid).unwrap_or_default().into()
            }
            _ => "".into(),
        }
    }

    /// A relative name made whole: against the folder descriptor the call
    /// gave, or the process' current folder.
    fn full(&mut self, call: &Call, name: &str) -> String {
        if name.starts_with('/') || name.is_empty() {
            return name.to_string();
        }
        let dir_fd = call.signature().and_then(|(sig, _)| {
            let i = sig.iter().position(|a| *a == Arg::DirFd)?;
            Some(call.arg(i)? as u32 as i32)
        });
        let dir: Option<Arc<str>> = match dir_fd {
            Some(fd) if fd != libc::AT_FDCWD => {
                Some(self.fd(call.pid, fd, call.end, false)).filter(|d| d.starts_with('/'))
            }
            _ => std::fs::read_link(format!("/proc/{}/cwd", call.pid))
                .ok()
                .map(|p| p.to_string_lossy().into()),
        };
        match dir {
            Some(dir) if dir.ends_with('/') => format!("{dir}{name}"),
            Some(dir) => format!("{dir}/{name}"),
            None => name.to_string(),
        }
    }

    /// What a descriptor is open on. `fresh`: the call at hand made it, so
    /// what was known of the number is stale.
    fn fd(&mut self, pid: u32, fd: i32, at: u64, fresh: bool) -> Arc<str> {
        if !fresh && let Some(name) = self.fds.get(&(pid, fd)).cloned() {
            return self.shown(&name);
        }
        // Closed (and maybe reused) since: /proc would name another file.
        let closed = self.closing.get(&(pid, fd)).is_some_and(|&t| t > at);
        match std::fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
            Ok(target) if !closed => {
                let name: Arc<str> = target.to_string_lossy().into();
                if self.fds.len() > 200_000 {
                    self.fds.clear();
                }
                self.fds.insert((pid, fd), name.clone());
                if fresh {
                    self.fresh = Some((pid, fd));
                }
                self.shown(&name)
            }
            _ => format!("fd {fd}").into(),
        }
    }

    /// A socket's connection, else the name /proc gives.
    fn shown(&mut self, name: &Arc<str>) -> Arc<str> {
        let Some(inode) = name
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.parse::<u64>().ok())
        else {
            return name.clone();
        };
        if !self.sockets.contains_key(&inode)
            && self
                .sockets_read
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            self.read_sockets();
        }
        self.sockets
            .get(&inode)
            .cloned()
            .unwrap_or_else(|| name.clone())
    }

    fn read_sockets(&mut self) {
        use crate::nettrace::Proto;
        const LISTEN: u8 = 10;
        self.sockets_read = Some(Instant::now());
        self.sockets.clear();
        let at = |(ip, port): (IpAddr, u16)| match ip {
            IpAddr::V6(v6) => format!("[{v6}]:{port}"),
            v4 => format!("{v4}:{port}"),
        };
        for proto in [Proto::Tcp, Proto::Udp] {
            for s in crate::netlink::dump(proto).unwrap_or_default() {
                if s.inode == 0 {
                    continue;
                }
                let text = match proto {
                    Proto::Tcp if s.state == LISTEN => {
                        format!("TCP, listening on port {}", s.local.1)
                    }
                    Proto::Tcp => format!("TCP {}", at(s.remote)),
                    Proto::Udp if s.remote.1 == 0 => format!("UDP, port {}", s.local.1),
                    Proto::Udp => format!("UDP {}", at(s.remote)),
                };
                self.sockets.insert(s.inode, text.into());
            }
        }
    }

    /// What a finished call changed about the process' descriptors.
    fn learn(&mut self, call: &Call) {
        let fresh = self.fresh.take();
        let pid = call.pid;
        if call.outcome == Outcome::Gone && call.name() == "exit_group" {
            self.forget(pid);
            return;
        }
        let (Some(args), Some(ret)) = (call.args, call.ret()) else {
            return;
        };
        if call.errno().is_some() {
            return;
        }
        let fd = |i: usize| args[i] as u32 as i32;
        match call.name() {
            "close" => {
                self.fds.remove(&(pid, fd(0)));
            }
            "close_range" => {
                let (from, to) = (args[0] as u32, args[1] as u32);
                self.fds
                    .retain(|&(p, f), _| p != pid || !(from..=to).contains(&(f as u32)));
            }
            "dup2" | "dup3" => self.copy(pid, fd(0), fd(1)),
            "dup" => self.copy(pid, fd(0), ret as i32),
            "fcntl" if matches!(args[1] as u32, 0 | 1030) => self.copy(pid, fd(0), ret as i32),
            "execve" | "execveat" => self.forget(pid),
            _ => {
                // A new descriptor: what was known of its number is stale.
                // When the kernel copied the name it was opened by, the
                // calls after it on the descriptor are about that file
                // (it may be closed before /proc could say so).
                if call.signature().is_some_and(|(_, r)| r == Ret::Fd)
                    && fresh != Some((pid, ret as i32))
                {
                    let opened = call
                        .names
                        .first()
                        .map(|(_, name)| self.full(call, name))
                        .filter(|path| path.starts_with('/'));
                    match opened {
                        Some(path) => {
                            self.fds.insert((pid, ret as i32), path.into());
                        }
                        None => {
                            self.fds.remove(&(pid, ret as i32));
                        }
                    }
                }
            }
        }
    }

    fn copy(&mut self, pid: u32, from: i32, to: i32) {
        match self.fds.get(&(pid, from)).cloned() {
            Some(name) => {
                self.fds.insert((pid, to), name);
            }
            None => {
                self.fds.remove(&(pid, to));
            }
        }
    }

    /// A process ended or started another program.
    pub fn forget(&mut self, pid: u32) {
        self.fds.retain(|&(p, _), _| p != pid);
    }
}

// ---------------------------------------------------------------------------
// The trace.

/// Ring buffer pages per processor: 512 KB each.
const PAGES: usize = 128;
/// For every process: 2 MB each, as busy machines make hundreds of
/// thousands of calls a second.
const PAGES_EVERYWHERE: usize = 512;
/// Kernel events (each a descriptor) one recording opens at most.
const MOST_EVENTS: usize = 32_768;
/// Records are put in order across processors once this old (ns): one
/// written on another processor meanwhile is in its ring by then.
const HOLD: u64 = 5_000_000;

const SAMPLE_TYPE: u64 = perf::SAMPLE_TID | perf::SAMPLE_TIME | perf::SAMPLE_RAW;

/// One record from the rings.
#[derive(Debug, PartialEq)]
struct Raw {
    time: u64,
    pid: u32,
    tid: u32,
    what: What,
}

#[derive(Debug, PartialEq)]
enum What {
    Enter {
        nr: u32,
        args: [u64; 6],
    },
    Exit {
        nr: u32,
        ret: i64,
    },
    /// File names, from a call's own tracepoint.
    Names {
        nr: u32,
        names: Copied,
    },
}

/// Where the tracepoints keep what is read here.
struct Format {
    enter: u64,
    enter_nr: Field,
    enter_args: Field,
    exit: u64,
    exit_nr: Field,
    exit_ret: Field,
    /// File-name tracepoints by id: their call and where each name is.
    named: HashMap<u64, (u32, Vec<(u8, Field)>)>,
}

impl Format {
    fn read() -> Result<Format, String> {
        let enter = perf::tracepoint("raw_syscalls", "sys_enter")?;
        let exit = perf::tracepoint("raw_syscalls", "sys_exit")?;
        let mut named = HashMap::new();
        for call in NAMED {
            let (Some(nr), Ok(tp)) = (
                number(call),
                perf::tracepoint("syscalls", &format!("sys_enter_{call}")),
            ) else {
                continue;
            };
            let fields = name_fields(call, &tp.fields);
            if !fields.is_empty() {
                named.insert(tp.id, (nr, fields));
            }
        }
        Ok(Format {
            enter: enter.id,
            enter_nr: enter.field("id")?,
            enter_args: enter.field("args")?,
            exit: exit.id,
            exit_nr: exit.field("id")?,
            exit_ret: exit.field("ret")?,
            named,
        })
    }

    fn parse(&self, body: &[u8]) -> Option<Raw> {
        let s = Sample::parse(body, SAMPLE_TYPE)?;
        let id = perf::event_id(s.raw)?;
        let what = if id == self.enter {
            let mut args = [0u64; 6];
            for (i, a) in args.iter_mut().enumerate() {
                *a = perf::u64_at(s.raw, self.enter_args.offset + 8 * i)?;
            }
            What::Enter {
                nr: self.enter_nr.u64(s.raw)? as u32,
                args,
            }
        } else if id == self.exit {
            What::Exit {
                nr: self.exit_nr.u64(s.raw)? as u32,
                ret: self.exit_ret.u64(s.raw)? as i64,
            }
        } else if let Some((nr, fields)) = self.named.get(&id) {
            What::Names {
                nr: *nr,
                names: fields
                    .iter()
                    .filter_map(|(i, f)| Some((*i, f.str(s.raw)?.into())))
                    .collect(),
            }
        } else {
            return None;
        };
        Some(Raw {
            time: s.time,
            pid: s.pid,
            tid: s.tid,
            what,
        })
    }
}

/// The copied file names in a call's tracepoint, by the argument each is
/// of: a `__data_loc` string named after its argument (`__filename_val`
/// for `filename`), else the one string of a call with one file name.
fn name_fields(call: &str, fields: &HashMap<String, Field>) -> Vec<(u8, Field)> {
    let Some((sig, _)) = signature(call) else {
        return Vec::new();
    };
    let paths: Vec<usize> = (0..sig.len()).filter(|&i| sig[i] == Arg::Path).collect();
    let strings: Vec<(&String, &Field)> = fields.iter().filter(|(_, f)| f.data_loc).collect();
    let mut out = Vec::new();
    for (name, field) in &strings {
        let arg = name
            .strip_prefix("__")
            .and_then(|n| n.strip_suffix("_val"))
            .and_then(|n| fields.get(n))
            // The syscall number sits at 8, the arguments from 16 on.
            .and_then(|a| a.offset.checked_sub(16))
            .map(|off| off / 8);
        if let Some(i) = arg.filter(|i| paths.contains(i)) {
            out.push((i as u8, **field));
        }
    }
    if out.is_empty() && strings.len() == 1 && paths.len() == 1 {
        out.push((paths[0] as u8, *strings[0].1));
    }
    out.sort_by_key(|(i, _)| *i);
    out
}

/// A call under way.
struct Open {
    pid: u32,
    nr: u32,
    time: u64,
    args: [u64; 6],
    names: Copied,
}

/// Calls' starts and ends, put together, by thread.
#[derive(Default)]
struct Pairs {
    open: HashMap<u32, Open>,
    /// File names that came before their call's start.
    early: HashMap<u32, (u32, u64, Copied)>,
}

impl Pairs {
    /// Takes one record, in time order; hands each call finished to
    /// `each`, then lets `names` learn from it.
    fn take(&mut self, raw: Raw, names: &mut Names, each: &mut impl FnMut(&Call, &mut Names)) {
        let mut finish = |call: Call| {
            each(&call, names);
            names.learn(&call);
        };
        let (pid, tid, time) = (raw.pid, raw.tid, raw.time);
        match raw.what {
            What::Enter { nr, args } => {
                // A call still open here lost its end.
                if let Some(o) = self.open.remove(&tid) {
                    finish(Call {
                        pid: o.pid,
                        tid,
                        nr: o.nr,
                        time: o.time,
                        end: time,
                        args: Some(o.args),
                        names: o.names,
                        outcome: Outcome::Unknown,
                    });
                }
                let copied = self
                    .early
                    .remove(&tid)
                    .filter(|(n, at, _)| *n == nr && time.abs_diff(*at) < 1_000_000)
                    .map(|(_, _, names)| names)
                    .unwrap_or_default();
                if signature(name(nr).unwrap_or("")).is_some_and(|(_, r)| r == Ret::Never) {
                    finish(Call {
                        pid,
                        tid,
                        nr,
                        time,
                        end: time,
                        args: Some(args),
                        names: copied,
                        outcome: Outcome::Gone,
                    });
                    return;
                }
                self.open.insert(
                    tid,
                    Open {
                        pid,
                        nr,
                        time,
                        args,
                        names: copied,
                    },
                );
            }
            What::Names { nr, names: copied } => match self.open.get_mut(&tid) {
                Some(o) if o.nr == nr && o.names.is_empty() => o.names = copied,
                _ => {
                    self.early.insert(tid, (nr, time, copied));
                }
            },
            What::Exit { nr, ret } => {
                let call = match self.open.remove(&tid) {
                    Some(o) if o.nr == nr => Call {
                        pid: o.pid,
                        tid,
                        nr,
                        time: o.time,
                        end: time,
                        args: Some(o.args),
                        names: o.names,
                        outcome: Outcome::Returned(ret),
                    },
                    other => {
                        if let Some(o) = other {
                            // Not its own end: the records between went
                            // missing.
                            self.open.insert(tid, o);
                        }
                        // A new thread or process comes back from clone or
                        // fork with 0: its parent's call says it all.
                        if ret == 0
                            && matches!(name(nr), Some("clone" | "clone3" | "fork" | "vfork"))
                        {
                            return;
                        }
                        Call {
                            pid,
                            tid,
                            nr,
                            time,
                            end: time,
                            args: None,
                            names: Vec::new(),
                            outcome: Outcome::Returned(ret),
                        }
                    }
                };
                finish(call);
            }
        }
    }

    /// The calls still under way, the oldest first.
    fn unfinished(&mut self, now: u64) -> Vec<Call> {
        let mut open: Vec<(u32, Open)> = self.open.drain().collect();
        open.sort_by_key(|(_, o)| o.time);
        open.into_iter()
            .map(|(tid, o)| Call {
                pid: o.pid,
                tid,
                nr: o.nr,
                time: o.time,
                end: now,
                args: Some(o.args),
                names: o.names,
                outcome: Outcome::Waiting,
            })
            .collect()
    }
}

/// When each descriptor is last closed (or replaced) among these records.
fn closing(records: &[Raw]) -> HashMap<(u32, i32), u64> {
    let mut out: HashMap<(u32, i32), u64> = HashMap::new();
    for r in records {
        if let What::Enter { nr, args } = &r.what {
            let fd = match name(*nr) {
                Some("close") => args[0],
                Some("dup2" | "dup3") => args[1],
                _ => continue,
            };
            let at = out.entry((r.pid, fd as u32 as i32)).or_default();
            *at = (*at).max(r.time);
        }
    }
    out
}

/// The system calls of chosen processes and of what they start, read as
/// they come.
pub struct Tracer {
    rings: TaskRings,
    format: Format,
    /// Without the file-name events: too many threads to give each.
    names_left_out: bool,
    budget: usize,
    followed: HashSet<u32>,
    /// Records not yet put in order.
    pending: Vec<Raw>,
    pairs: Pairs,
    pub names: Names,
    /// Records the kernel dropped: the reader fell behind.
    pub lost: u64,
}

impl Tracer {
    /// Ready to follow processes; on the thread that reads it, which must
    /// outlive it.
    pub fn new() -> Result<Tracer, String> {
        Self::with_rings(&[PAGES, PAGES / 4, 8])
    }

    /// Every process' calls but Anabolic's own, as `perf trace -a` reads
    /// them: one event per processor for the whole machine. Its own are
    /// left out by the kernel, or else would feed the trace they read.
    pub fn everywhere() -> Result<Tracer, String> {
        let mut tracer = Self::with_rings(&[PAGES_EVERYWHERE, PAGES, PAGES / 4])?;
        let attrs: Vec<Attr> = tracer
            .attrs(false)
            .into_iter()
            .map(|mut attr| {
                attr.flags &= !perf::INHERIT;
                attr
            })
            .collect();
        let own: Vec<String> = procfs::tids(std::process::id())
            .iter()
            .map(|tid| format!("common_pid != {tid}"))
            .collect();
        let filter = (!own.is_empty()).then(|| own.join(" && "));
        tracer
            .rings
            .attach_everywhere(&attrs, filter.as_deref())
            .map_err(|e| perf::refused(&e))?;
        Ok(tracer)
    }

    fn with_rings(sizes: &[usize]) -> Result<Tracer, String> {
        let format = Format::read()?;
        let rings = sizes
            .iter()
            .copied()
            .find_map(|pages| TaskRings::new(pages).ok())
            .ok_or_else(|| {
                format!(
                    "no buffer for the trace: {}",
                    crate::util::error_text(&io::Error::last_os_error())
                )
            })?;
        Ok(Tracer {
            rings,
            format,
            names_left_out: false,
            budget: MOST_EVENTS.min(raise_open_files().saturating_sub(4096)),
            followed: HashSet::new(),
            pending: Vec::new(),
            pairs: Pairs::default(),
            names: Names::default(),
            lost: 0,
        })
    }

    fn attrs(&self, on_exec: bool) -> Vec<Attr> {
        let mut ids = vec![self.format.enter, self.format.exit];
        if !self.names_left_out {
            ids.extend(self.format.named.keys());
        }
        ids.into_iter()
            .map(|id| {
                let mut attr = Attr::new(perf::TYPE_TRACEPOINT, id);
                attr.sample = 1;
                attr.sample_type = SAMPLE_TYPE;
                attr.flags |= perf::INHERIT;
                if on_exec {
                    attr.flags |= perf::ENABLE_ON_EXEC;
                }
                attr
            })
            .collect()
    }

    /// The descriptors to poll: readable when there are calls to read.
    pub fn fds(&self) -> impl Iterator<Item = i32> + '_ {
        self.rings.fds()
    }

    /// Follows these running processes, and what they start from now on.
    /// Says what could not be followed, if anything.
    pub fn follow(&mut self, pids: &[u32]) -> Option<String> {
        let own = std::process::id();
        let procs: Vec<(u32, Vec<u32>)> = pids
            .iter()
            .filter(|&&p| p != own && !self.followed.contains(&p))
            .map(|&p| (p, procfs::tids(p)))
            .filter(|(_, tids)| !tids.is_empty())
            .collect();
        let threads: usize = procs.iter().map(|(_, t)| t.len()).sum();
        let cpus = self.rings.cpus();
        let mut notes = Vec::new();
        let all = 2 + self.format.named.len();
        if !self.names_left_out
            && !self.format.named.is_empty()
            && self.rings.len() + threads * cpus * all > self.budget
        {
            self.names_left_out = true;
            notes.push(format!(
                "file names are left out: {threads} threads are too many to follow them"
            ));
        }
        let attrs = self.attrs(false);
        let per_thread = attrs.len() * cpus;
        let (mut skipped, mut refused) = (0, None);
        'procs: for (pid, tids) in procs {
            if self.rings.len() + tids.len() * per_thread > self.budget {
                skipped += 1;
                continue;
            }
            // Threads it starts while this goes on get the events from the
            // thread that starts them, if it has them already; else they
            // are found the next time round.
            let mut done = HashSet::new();
            let mut next = tids;
            for _ in 0..3 {
                let new: Vec<u32> = next.into_iter().filter(|t| done.insert(*t)).collect();
                if new.is_empty() {
                    break;
                }
                for tid in new {
                    match self.rings.attach(tid, &attrs) {
                        Ok(_) => {}
                        // Ended meanwhile.
                        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {}
                        Err(e) => {
                            refused = Some(perf::refused(&e));
                            break 'procs;
                        }
                    }
                }
                next = procfs::tids(pid);
            }
            self.followed.insert(pid);
        }
        if let Some(why) = refused {
            notes.push(why);
        }
        if skipped > 0 {
            notes.push(format!(
                "{skipped} processes are not followed: their threads would take more than {} \
                 kernel events ({cpus} processors each)",
                self.budget
            ));
        }
        (!notes.is_empty()).then(|| notes.join("; "))
    }

    /// Starts a program and follows it from its first step. Returns the
    /// program, and why its calls are not followed, if they are not.
    pub fn launch(&mut self, command: &mut Command) -> (io::Result<Child>, Option<String>) {
        let attrs = self.attrs(true);
        let rings = &mut self.rings;
        let mut problem = None;
        let child = perf::spawn_held(command, |pid| {
            if let Err(e) = rings.attach(pid, &attrs) {
                problem = Some(perf::refused(&e));
            }
        });
        if let Ok(child) = &child {
            self.followed.insert(child.id());
        }
        (child, problem)
    }

    /// Hands every call finished by now to `each`, in the order they
    /// ended, with what names their descriptors (right for that call only:
    /// the calls after change it). `all`: everything there is (the
    /// recording ends). Returns how many records were read.
    pub fn read(&mut self, all: bool, mut each: impl FnMut(&Call, &mut Names)) -> u64 {
        let horizon = if all {
            u64::MAX
        } else {
            perf::monotonic_ns().saturating_sub(HOLD)
        };
        let (mut lost, mut records) = (0, 0);
        let (format, pending) = (&self.format, &mut self.pending);
        self.rings.drain(|r| match r {
            Record::Sample(body) => {
                records += 1;
                pending.extend(format.parse(body));
            }
            Record::Lost(n) => lost += n,
            _ => {}
        });
        self.lost += lost;
        // Each processor's ring is in order; together they are not.
        self.pending.sort_by_key(|r| r.time);
        let ready = self.pending.partition_point(|r| r.time <= horizon);
        self.names.closing = closing(&self.pending);
        for raw in self.pending.drain(..ready).collect::<Vec<_>>() {
            self.pairs.take(raw, &mut self.names, &mut each);
        }
        records
    }

    /// The calls still under way, as the recording ends.
    pub fn unfinished(&mut self, mut each: impl FnMut(&Call, &mut Names)) {
        for call in self.pairs.unfinished(perf::monotonic_ns()) {
            each(&call, &mut self.names);
        }
    }
}

/// Raises this process' open-file limit as far as the trace needs, when it
/// may; returns it. (Programs Anabolic starts get it too: a few thousand
/// more do no harm.)
fn raise_open_files() -> usize {
    const NEEDED: libc::rlim_t = (MOST_EVENTS + 8192) as libc::rlim_t;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return 1024;
        }
        if limit.rlim_cur < NEEDED.min(limit.rlim_max) {
            let raised = libc::rlimit {
                rlim_cur: NEEDED.min(limit.rlim_max),
                rlim_max: limit.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                limit = raised;
            }
        }
    }
    limit.rlim_cur as usize
}

/// x86-64 system call names by number, from asm/unistd_64.h; "" where no
/// call has the number.
#[cfg(target_arch = "x86_64")]
#[rustfmt::skip]
const NAMES: [&str; 472] = [
    "read", "write", "open", "close", "stat", "fstat", "lstat", "poll", "lseek", "mmap",
    "mprotect", "munmap", "brk", "rt_sigaction", "rt_sigprocmask", "rt_sigreturn", "ioctl",
    "pread64", "pwrite64", "readv", "writev", "access", "pipe", "select", "sched_yield",
    "mremap", "msync", "mincore", "madvise", "shmget", "shmat", "shmctl", "dup", "dup2",
    "pause", "nanosleep", "getitimer", "alarm", "setitimer", "getpid", "sendfile", "socket",
    "connect", "accept", "sendto", "recvfrom", "sendmsg", "recvmsg", "shutdown", "bind",
    "listen", "getsockname", "getpeername", "socketpair", "setsockopt", "getsockopt", "clone",
    "fork", "vfork", "execve", "exit", "wait4", "kill", "uname", "semget", "semop", "semctl",
    "shmdt", "msgget", "msgsnd", "msgrcv", "msgctl", "fcntl", "flock", "fsync", "fdatasync",
    "truncate", "ftruncate", "getdents", "getcwd", "chdir", "fchdir", "rename", "mkdir",
    "rmdir", "creat", "link", "unlink", "symlink", "readlink", "chmod", "fchmod", "chown",
    "fchown", "lchown", "umask", "gettimeofday", "getrlimit", "getrusage", "sysinfo", "times",
    "ptrace", "getuid", "syslog", "getgid", "setuid", "setgid", "geteuid", "getegid",
    "setpgid", "getppid", "getpgrp", "setsid", "setreuid", "setregid", "getgroups",
    "setgroups", "setresuid", "getresuid", "setresgid", "getresgid", "getpgid", "setfsuid",
    "setfsgid", "getsid", "capget", "capset", "rt_sigpending", "rt_sigtimedwait",
    "rt_sigqueueinfo", "rt_sigsuspend", "sigaltstack", "utime", "mknod", "uselib",
    "personality", "ustat", "statfs", "fstatfs", "sysfs", "getpriority", "setpriority",
    "sched_setparam", "sched_getparam", "sched_setscheduler", "sched_getscheduler",
    "sched_get_priority_max", "sched_get_priority_min", "sched_rr_get_interval", "mlock",
    "munlock", "mlockall", "munlockall", "vhangup", "modify_ldt", "pivot_root", "_sysctl",
    "prctl", "arch_prctl", "adjtimex", "setrlimit", "chroot", "sync", "acct", "settimeofday",
    "mount", "umount2", "swapon", "swapoff", "reboot", "sethostname", "setdomainname", "iopl",
    "ioperm", "create_module", "init_module", "delete_module", "get_kernel_syms",
    "query_module", "quotactl", "nfsservctl", "getpmsg", "putpmsg", "afs_syscall", "tuxcall",
    "security", "gettid", "readahead", "setxattr", "lsetxattr", "fsetxattr", "getxattr",
    "lgetxattr", "fgetxattr", "listxattr", "llistxattr", "flistxattr", "removexattr",
    "lremovexattr", "fremovexattr", "tkill", "time", "futex", "sched_setaffinity",
    "sched_getaffinity", "set_thread_area", "io_setup", "io_destroy", "io_getevents",
    "io_submit", "io_cancel", "get_thread_area", "lookup_dcookie", "epoll_create",
    "epoll_ctl_old", "epoll_wait_old", "remap_file_pages", "getdents64", "set_tid_address",
    "restart_syscall", "semtimedop", "fadvise64", "timer_create", "timer_settime",
    "timer_gettime", "timer_getoverrun", "timer_delete", "clock_settime", "clock_gettime",
    "clock_getres", "clock_nanosleep", "exit_group", "epoll_wait", "epoll_ctl", "tgkill",
    "utimes", "vserver", "mbind", "set_mempolicy", "get_mempolicy", "mq_open", "mq_unlink",
    "mq_timedsend", "mq_timedreceive", "mq_notify", "mq_getsetattr", "kexec_load", "waitid",
    "add_key", "request_key", "keyctl", "ioprio_set", "ioprio_get", "inotify_init",
    "inotify_add_watch", "inotify_rm_watch", "migrate_pages", "openat", "mkdirat", "mknodat",
    "fchownat", "futimesat", "newfstatat", "unlinkat", "renameat", "linkat", "symlinkat",
    "readlinkat", "fchmodat", "faccessat", "pselect6", "ppoll", "unshare", "set_robust_list",
    "get_robust_list", "splice", "tee", "sync_file_range", "vmsplice", "move_pages",
    "utimensat", "epoll_pwait", "signalfd", "timerfd_create", "eventfd", "fallocate",
    "timerfd_settime", "timerfd_gettime", "accept4", "signalfd4", "eventfd2", "epoll_create1",
    "dup3", "pipe2", "inotify_init1", "preadv", "pwritev", "rt_tgsigqueueinfo",
    "perf_event_open", "recvmmsg", "fanotify_init", "fanotify_mark", "prlimit64",
    "name_to_handle_at", "open_by_handle_at", "clock_adjtime", "syncfs", "sendmmsg", "setns",
    "getcpu", "process_vm_readv", "process_vm_writev", "kcmp", "finit_module", "sched_setattr",
    "sched_getattr", "renameat2", "seccomp", "getrandom", "memfd_create", "kexec_file_load",
    "bpf", "execveat", "userfaultfd", "membarrier", "mlock2", "copy_file_range", "preadv2",
    "pwritev2", "pkey_mprotect", "pkey_alloc", "pkey_free", "statx", "io_pgetevents", "rseq",
    "uretprobe", "uprobe", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
    "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
    "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
    "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
    "", "pidfd_send_signal", "io_uring_setup", "io_uring_enter", "io_uring_register",
    "open_tree", "move_mount", "fsopen", "fsconfig", "fsmount", "fspick", "pidfd_open",
    "clone3", "close_range", "openat2", "pidfd_getfd", "faccessat2", "process_madvise",
    "epoll_pwait2", "mount_setattr", "quotactl_fd", "landlock_create_ruleset",
    "landlock_add_rule", "landlock_restrict_self", "memfd_secret", "process_mrelease",
    "futex_waitv", "set_mempolicy_home_node", "cachestat", "fchmodat2", "map_shadow_stack",
    "futex_wake", "futex_wait", "futex_requeue", "statmount", "listmount", "lsm_get_self_attr",
    "lsm_set_self_attr", "lsm_list_modules", "mseal", "setxattrat", "getxattrat",
    "listxattrat", "removexattrat", "open_tree_attr", "file_getattr", "file_setattr", "listns",
    "rseq_slice_yield",
];

/// errno names by number, from asm-generic/errno-base.h and errno.h.
#[rustfmt::skip]
const ERRNOS: [&str; 135] = [
    "", "EPERM", "ENOENT", "ESRCH", "EINTR", "EIO", "ENXIO", "E2BIG", "ENOEXEC", "EBADF",
    "ECHILD", "EAGAIN", "ENOMEM", "EACCES", "EFAULT", "ENOTBLK", "EBUSY", "EEXIST", "EXDEV",
    "ENODEV", "ENOTDIR", "EISDIR", "EINVAL", "ENFILE", "EMFILE", "ENOTTY", "ETXTBSY", "EFBIG",
    "ENOSPC", "ESPIPE", "EROFS", "EMLINK", "EPIPE", "EDOM", "ERANGE", "EDEADLK",
    "ENAMETOOLONG", "ENOLCK", "ENOSYS", "ENOTEMPTY", "ELOOP", "", "ENOMSG", "EIDRM", "ECHRNG",
    "EL2NSYNC", "EL3HLT", "EL3RST", "ELNRNG", "EUNATCH", "ENOCSI", "EL2HLT", "EBADE", "EBADR",
    "EXFULL", "ENOANO", "EBADRQC", "EBADSLT", "", "EBFONT", "ENOSTR", "ENODATA", "ETIME",
    "ENOSR", "ENONET", "ENOPKG", "EREMOTE", "ENOLINK", "EADV", "ESRMNT", "ECOMM", "EPROTO",
    "EMULTIHOP", "EDOTDOT", "EBADMSG", "EOVERFLOW", "ENOTUNIQ", "EBADFD", "EREMCHG", "ELIBACC",
    "ELIBBAD", "ELIBSCN", "ELIBMAX", "ELIBEXEC", "EILSEQ", "ERESTART", "ESTRPIPE", "EUSERS",
    "ENOTSOCK", "EDESTADDRREQ", "EMSGSIZE", "EPROTOTYPE", "ENOPROTOOPT", "EPROTONOSUPPORT",
    "ESOCKTNOSUPPORT", "EOPNOTSUPP", "EPFNOSUPPORT", "EAFNOSUPPORT", "EADDRINUSE",
    "EADDRNOTAVAIL", "ENETDOWN", "ENETUNREACH", "ENETRESET", "ECONNABORTED", "ECONNRESET",
    "ENOBUFS", "EISCONN", "ENOTCONN", "ESHUTDOWN", "ETOOMANYREFS", "ETIMEDOUT", "ECONNREFUSED",
    "EHOSTDOWN", "EHOSTUNREACH", "EALREADY", "EINPROGRESS", "ESTALE", "EUCLEAN", "ENOTNAM",
    "ENAVAIL", "EISNAM", "EREMOTEIO", "EDQUOT", "ENOMEDIUM", "EMEDIUMTYPE", "ECANCELED",
    "ENOKEY", "EKEYEXPIRED", "EKEYREVOKED", "EKEYREJECTED", "EOWNERDEAD", "ENOTRECOVERABLE",
    "ERFKILL", "EHWPOISON", "EFTYPE",
];

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    fn nr(name: &str) -> u32 {
        number(name).unwrap()
    }

    fn call(name: &str, args: [u64; 6], outcome: Outcome) -> Call {
        Call {
            pid: std::process::id(),
            tid: 7,
            nr: nr(name),
            time: 1_000,
            end: 1_250,
            args: Some(args),
            names: Vec::new(),
            outcome,
        }
    }

    const AT_FDCWD: u64 = libc::AT_FDCWD as u32 as u64;

    #[test]
    fn knows_calls_and_errors_by_number() {
        assert_eq!(name(0), Some("read"));
        assert_eq!(name(59), Some("execve"));
        assert_eq!(name(231), Some("exit_group"));
        assert_eq!(name(400), None, "a gap in the table");
        assert_eq!(name(100_000), None);
        assert_eq!(number("openat"), Some(257));
        assert_eq!(crate::sysinfo::syscall_name(202), Some("futex"));
        assert_eq!(errno_name(2), Some("ENOENT"));
        assert_eq!(errno_name(11), Some("EAGAIN"));
        assert_eq!(errno_name(41), None);
        assert_eq!(errno_name(512), Some("ERESTARTSYS"));
    }

    #[test]
    fn reads_arguments_as_strace_writes_them() {
        let o = |v: libc::c_int| v as u32 as u64;
        assert_eq!(
            format_arg(Arg::OpenFlags, o(libc::O_RDONLY | libc::O_CLOEXEC)),
            "O_RDONLY|O_CLOEXEC"
        );
        assert_eq!(
            format_arg(
                Arg::OpenFlags,
                o(libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC)
            ),
            "O_WRONLY|O_CREAT|O_TRUNC"
        );
        // An int argument's upper half is whatever was in the register.
        assert_eq!(format_arg(Arg::DirFd, 0xdead_0000_ffff_ff9c), "AT_FDCWD");
        assert_eq!(format_arg(Arg::Fd, u64::MAX), "-1");
        assert_eq!(format_arg(Arg::Mode, 0o644), "0644");
        assert_eq!(format_arg(Arg::Ptr, 0), "NULL");
        assert_eq!(format_arg(Arg::Prot, 3), "PROT_READ|PROT_WRITE");
        assert_eq!(format_arg(Arg::Prot, 0), "PROT_NONE");
        assert_eq!(
            format_arg(Arg::MapFlags, o(libc::MAP_PRIVATE | libc::MAP_ANONYMOUS)),
            "MAP_PRIVATE|MAP_ANONYMOUS"
        );
        assert_eq!(format_arg(Arg::FutexOp, 128), "FUTEX_WAIT_PRIVATE");
        assert_eq!(
            format_arg(Arg::FutexOp, 129 | 256),
            "FUTEX_WAKE_PRIVATE|FUTEX_CLOCK_REALTIME"
        );
        assert_eq!(
            format_arg(
                Arg::CloneFlags,
                o(libc::CLONE_VM | libc::CLONE_FS | libc::SIGCHLD)
            ),
            "CLONE_VM|CLONE_FS|SIGCHLD"
        );
        assert_eq!(format_arg(Arg::Signal, 15), "SIGTERM");
        assert_eq!(format_arg(Arg::Signal, 34), "SIGRT_2");
        assert_eq!(format_arg(Arg::Ioctl, 0x5401), "TCGETS");
        assert_eq!(format_arg(Arg::Ioctl, 0xc010_64a5), "0xc01064a5 ('d' 0xa5)");
        assert_eq!(
            format_arg(Arg::SockType, 0x80801),
            "SOCK_STREAM|SOCK_NONBLOCK|SOCK_CLOEXEC"
        );
        assert_eq!(format_arg(Arg::Family, 10), "AF_INET6");
        assert_eq!(format_arg(Arg::AccessMode, 0), "F_OK");
        assert_eq!(format_arg(Arg::AccessMode, 5), "R_OK|X_OK");
        assert_eq!(
            format_arg(Arg::AtFlags, 0x1100),
            "AT_SYMLINK_NOFOLLOW|AT_EMPTY_PATH"
        );
        assert_eq!(format_arg(Arg::AtFlags, 0x10_0000), "0x100000");
        assert_eq!(format_arg(Arg::Fcntl, 1030), "F_DUPFD_CLOEXEC");
        assert_eq!(format_arg(Arg::Fcntl, 99), "99");
    }

    #[test]
    fn writes_a_call_whole() {
        let flags = (libc::O_RDONLY | libc::O_CLOEXEC) as u64;
        let mut open = call(
            "openat",
            [AT_FDCWD, 0x7ffd_1000, flags, 0, 0, 0],
            Outcome::Returned(3),
        );
        assert_eq!(
            open.text(),
            "openat(AT_FDCWD, 0x7ffd1000, O_RDONLY|O_CLOEXEC) = 3"
        );
        open.names = vec![(1, "/etc/hosts".into())];
        assert_eq!(
            open.text(),
            r#"openat(AT_FDCWD, "/etc/hosts", O_RDONLY|O_CLOEXEC) = 3"#
        );
        // The mode shows when a file is made.
        let create = call(
            "openat",
            [
                AT_FDCWD,
                0,
                (libc::O_WRONLY | libc::O_CREAT) as u64,
                0o600,
                0,
                0,
            ],
            Outcome::Returned(-2),
        );
        assert_eq!(
            create.text(),
            "openat(AT_FDCWD, NULL, O_WRONLY|O_CREAT, 0600) = -1 ENOENT"
        );
        assert_eq!((create.errno(), create.status()), (Some(2), Some(2)));
        let map = call(
            "mmap",
            [0, 8192, 3, 0x22, u64::MAX, 0],
            Outcome::Returned(0x7f00_0000_0000),
        );
        assert_eq!(
            map.text(),
            "mmap(NULL, 8192, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0) = \
             0x7f0000000000"
        );
        assert_eq!(map.status(), Some(0));
        assert_eq!(
            call("exit_group", [1, 0, 0, 0, 0, 0], Outcome::Gone).text(),
            "exit_group(1)"
        );
        assert_eq!(
            call("futex", [0x7f12, 128, 2, 0, 0, 0], Outcome::Waiting).text(),
            "futex(0x7f12, FUTEX_WAIT_PRIVATE, 2, NULL, NULL, 0) = ?"
        );
        // A call without a signature here: its registers.
        assert_eq!(
            call("kcmp", [1, 2, 0, 0, 0, 0], Outcome::Returned(0)).text(),
            "kcmp(1, 2) = 0"
        );
        let mut began = call("read", [0; 6], Outcome::Returned(120));
        began.args = None;
        assert_eq!(began.text(), "read(…) = 120");
        assert_eq!(began.took(), None);
        assert_eq!(
            call("read", [5, 0, 4096, 0, 0, 0], Outcome::Returned(120)).took(),
            Some(Duration::from_nanos(250))
        );
    }

    #[test]
    fn knows_what_a_call_is_about() {
        let read = call("read", [5, 0x1000, 4096, 0, 0, 0], Outcome::Returned(120));
        assert_eq!(read.object(), Object::Fd(5));
        assert_eq!(read.bytes(), 120);
        let write = call("write", [1, 0x1000, 3, 0, 0, 0], Outcome::Returned(-11));
        assert_eq!((write.bytes(), write.status()), (0, Some(11)));
        let mut open = call("openat", [AT_FDCWD, 0, 0, 0, 0, 0], Outcome::Returned(4));
        assert_eq!(open.object(), Object::NewFd(4));
        open.names = vec![(1, "a.txt".into())];
        assert_eq!(open.object(), Object::Name("a.txt".into()));
        let failed = call("openat", [AT_FDCWD, 0, 0, 0, 0, 0], Outcome::Returned(-13));
        assert_eq!(failed.object(), Object::None);
        let anon = call(
            "mmap",
            [0, 4096, 3, 0x22, u64::MAX, 0],
            Outcome::Returned(1 << 40),
        );
        assert_eq!(anon.object(), Object::Fd(-1));
        assert_eq!(
            call("futex", [0xabc, 129, 1, 0, 0, 0], Outcome::Returned(0)).object(),
            Object::Addr(0xabc)
        );
        assert_eq!(
            call("kill", [42, 15, 0, 0, 0, 0], Outcome::Returned(0)).object(),
            Object::Pid(42)
        );
        assert_eq!(
            call("clone", [0x1200011, 0, 0, 0, 0, 0], Outcome::Returned(4321)).object(),
            Object::Pid(4321)
        );
    }

    fn enter(time: u64, tid: u32, name: &str, args: [u64; 6]) -> Raw {
        Raw {
            time,
            pid: 100,
            tid,
            what: What::Enter { nr: nr(name), args },
        }
    }

    fn exit(time: u64, tid: u32, name: &str, ret: i64) -> Raw {
        Raw {
            time,
            pid: 100,
            tid,
            what: What::Exit { nr: nr(name), ret },
        }
    }

    fn copied(time: u64, tid: u32, name: &str, text: &str) -> Raw {
        Raw {
            time,
            pid: 100,
            tid,
            what: What::Names {
                nr: nr(name),
                names: vec![(1, text.into())],
            },
        }
    }

    #[test]
    fn puts_starts_and_ends_together() {
        let records = vec![
            enter(100, 11, "read", [5, 0, 64, 0, 0, 0]),
            exit(150, 11, "read", 5),
            enter(200, 11, "openat", [AT_FDCWD, 0, 0, 0, 0, 0]),
            copied(201, 11, "openat", "/etc/hosts"),
            exit(300, 11, "openat", 3),
            // The name before the start (another processor's ring).
            copied(399, 12, "openat", "/etc/passwd"),
            enter(400, 12, "openat", [AT_FDCWD, 0, 0, 0, 0, 0]),
            exit(450, 12, "openat", -2),
            // Began before the recording.
            exit(460, 13, "poll", 1),
            // A new thread's first return: its parent's clone says it.
            exit(470, 14, "clone", 0),
            enter(500, 16, "read", [7, 0, 1, 0, 0, 0]),
            // Its end was lost.
            enter(700, 16, "write", [1, 0, 1, 0, 0, 0]),
            exit(710, 16, "write", 1),
            enter(800, 15, "futex", [0x10, 128, 0, 0, 0, 0]),
            enter(900, 11, "exit_group", [0; 6]),
        ];
        let (mut pairs, mut names, mut calls) = (Pairs::default(), Names::default(), Vec::new());
        for raw in records {
            pairs.take(raw, &mut names, &mut |c: &Call, _: &mut Names| {
                calls.push(c.clone())
            });
        }
        let seen: Vec<(u32, &str, Outcome)> =
            calls.iter().map(|c| (c.tid, c.name(), c.outcome)).collect();
        assert_eq!(
            seen,
            [
                (11, "read", Outcome::Returned(5)),
                (11, "openat", Outcome::Returned(3)),
                (12, "openat", Outcome::Returned(-2)),
                (13, "poll", Outcome::Returned(1)),
                (16, "read", Outcome::Unknown),
                (16, "write", Outcome::Returned(1)),
                (11, "exit_group", Outcome::Gone),
            ]
        );
        assert_eq!(calls[0].took(), Some(Duration::from_nanos(50)));
        assert_eq!(calls[1].names, [(1, "/etc/hosts".into())]);
        assert_eq!(calls[2].names, [(1, "/etc/passwd".into())]);
        assert_eq!(calls[3].args, None);
        let waiting = pairs.unfinished(1000);
        assert_eq!(waiting.len(), 1);
        assert_eq!((waiting[0].tid, waiting[0].outcome), (15, Outcome::Waiting));
        assert_eq!(
            waiting[0].text(),
            "futex(0x10, FUTEX_WAIT_PRIVATE, 0, NULL, NULL, 0) = ?"
        );
    }

    #[test]
    fn names_descriptors_while_they_are_what_was_used() {
        let file = std::fs::File::open("Cargo.toml").unwrap();
        let fd = file.as_raw_fd();
        let path: Arc<str> = std::fs::canonicalize("Cargo.toml")
            .unwrap()
            .to_string_lossy()
            .into();
        let read = call("read", [fd as u64, 0, 64, 0, 0, 0], Outcome::Returned(64));
        let pid = read.pid;
        let mut names = Names::default();
        // Closed (and maybe opened again) after the call: /proc names
        // something else now.
        names.closing.insert((pid, fd), 2_000);
        assert_eq!(&*names.path(&read, &read.object()), format!("fd {fd}"));
        // Closed before it: what /proc says is what was read.
        names.closing.insert((pid, fd), 500);
        assert_eq!(names.path(&read, &read.object()), path);
        // Known now, also once closed; until it is closed by the program.
        names.closing.insert((pid, fd), 2_000);
        assert_eq!(names.path(&read, &read.object()), path);
        names.learn(&call(
            "dup2",
            [fd as u64, 999, 0, 0, 0, 0],
            Outcome::Returned(999),
        ));
        let read_dup = call("read", [999, 0, 64, 0, 0, 0], Outcome::Returned(1));
        assert_eq!(names.path(&read_dup, &read_dup.object()), path);
        names.learn(&call(
            "close",
            [fd as u64, 0, 0, 0, 0, 0],
            Outcome::Returned(0),
        ));
        assert_eq!(&*names.path(&read, &read.object()), format!("fd {fd}"));
        // A file named relative to the current folder.
        let mut stat = call(
            "newfstatat",
            [AT_FDCWD, 0, 0, 0, 0, 0],
            Outcome::Returned(0),
        );
        stat.names = vec![(1, "Cargo.toml".into())];
        assert_eq!(names.path(&stat, &stat.object()), path);
        // A socket shows its connection.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let fd = listener.as_raw_fd() as u64;
        let accept = call("accept4", [fd, 0, 0, 0, 0, 0], Outcome::Returned(-11));
        names.closing.clear();
        assert_eq!(
            &*names.path(&accept, &accept.object()),
            format!("TCP, listening on port {port}")
        );
    }

    #[test]
    fn names_a_descriptor_by_the_open_that_made_it() {
        let mut names = Names::default();
        let mut open = call("openat", [AT_FDCWD, 0, 0, 0, 0, 0], Outcome::Returned(77));
        open.names = vec![(1, "/usr/lib/libc.so.6".into())];
        let pid = open.pid;
        names.learn(&open);
        // Gone from /proc by now; the open said what it was.
        let read = call("read", [77, 0, 64, 0, 0, 0], Outcome::Returned(64));
        assert_eq!(&*names.path(&read, &read.object()), "/usr/lib/libc.so.6");
        names.learn(&call("close", [77, 0, 0, 0, 0, 0], Outcome::Returned(0)));
        assert_eq!(&*names.path(&read, &read.object()), "fd 77");
        // A failed open names nothing.
        let mut failed = call("openat", [AT_FDCWD, 0, 0, 0, 0, 0], Outcome::Returned(-2));
        failed.names = vec![(1, "/nope".into())];
        names.learn(&failed);
        assert!(!names.fds.contains_key(&(pid, -2)));
    }

    #[test]
    fn finds_the_names_a_kernel_copies() {
        let field = |offset, data_loc| Field {
            offset,
            size: if data_loc { 4 } else { 8 },
            data_loc,
        };
        let mut fields = HashMap::new();
        fields.insert("__syscall_nr".to_string(), field(8, false));
        fields.insert("dfd".to_string(), field(16, false));
        fields.insert("filename".to_string(), field(24, false));
        fields.insert("flags".to_string(), field(32, false));
        fields.insert("mode".to_string(), field(40, false));
        assert!(name_fields("openat", &fields).is_empty(), "an older kernel");
        fields.insert("__filename_val".to_string(), field(48, true));
        assert_eq!(name_fields("openat", &fields), [(1, field(48, true))]);
        // Named otherwise, but the one string of a call with one name.
        fields.remove("__filename_val");
        fields.insert("filename_str".to_string(), field(52, true));
        assert_eq!(name_fields("openat", &fields), [(1, field(52, true))]);
    }

    #[test]
    fn parses_raw_records() {
        let f = |offset, size| Field {
            offset,
            size,
            data_loc: false,
        };
        let mut named = HashMap::new();
        named.insert(
            700,
            (
                nr("openat"),
                vec![(
                    1,
                    Field {
                        offset: 48,
                        size: 4,
                        data_loc: true,
                    },
                )],
            ),
        );
        let format = Format {
            enter: 21,
            enter_nr: f(8, 8),
            enter_args: f(16, 48),
            exit: 22,
            exit_nr: f(8, 8),
            exit_ret: f(16, 8),
            named,
        };
        let sample = |raw: &[u8]| {
            let mut body = Vec::new();
            body.extend_from_slice(&100u32.to_ne_bytes());
            body.extend_from_slice(&101u32.to_ne_bytes());
            body.extend_from_slice(&5_000u64.to_ne_bytes());
            body.extend_from_slice(&(raw.len() as u32).to_ne_bytes());
            body.extend_from_slice(raw);
            body
        };
        let mut enter = vec![0u8; 64];
        enter[0..2].copy_from_slice(&21u16.to_ne_bytes());
        enter[8..16].copy_from_slice(&257u64.to_ne_bytes());
        for i in 0..6 {
            enter[16 + 8 * i..24 + 8 * i].copy_from_slice(&(i as u64 + 1).to_ne_bytes());
        }
        assert_eq!(
            format.parse(&sample(&enter)),
            Some(Raw {
                time: 5_000,
                pid: 100,
                tid: 101,
                what: What::Enter {
                    nr: 257,
                    args: [1, 2, 3, 4, 5, 6]
                }
            })
        );
        let mut exit = vec![0u8; 24];
        exit[0..2].copy_from_slice(&22u16.to_ne_bytes());
        exit[8..16].copy_from_slice(&257u64.to_ne_bytes());
        exit[16..24].copy_from_slice(&(-2i64).to_ne_bytes());
        assert_eq!(
            format.parse(&sample(&exit)).map(|r| r.what),
            Some(What::Exit { nr: 257, ret: -2 })
        );
        let mut names = vec![0u8; 64];
        names[0..2].copy_from_slice(&700u16.to_ne_bytes());
        names[48..52].copy_from_slice(&(52u32 | (11 << 16)).to_ne_bytes());
        names[52..63].copy_from_slice(b"/etc/hosts\0");
        assert_eq!(
            format.parse(&sample(&names)).map(|r| r.what),
            Some(What::Names {
                nr: 257,
                names: vec![(1, "/etc/hosts".into())]
            })
        );
        let mut other = vec![0u8; 16];
        other[0..2].copy_from_slice(&9u16.to_ne_bytes());
        assert_eq!(format.parse(&sample(&other)), None);
    }

    #[test]
    fn knows_which_descriptors_get_closed() {
        let records = vec![
            enter(10, 1, "close", [4, 0, 0, 0, 0, 0]),
            enter(20, 1, "close", [4, 0, 0, 0, 0, 0]),
            enter(30, 1, "dup2", [3, 9, 0, 0, 0, 0]),
            enter(40, 1, "read", [5, 0, 0, 0, 0, 0]),
        ];
        let closed = closing(&records);
        assert_eq!(closed.get(&(100, 4)), Some(&20));
        assert_eq!(closed.get(&(100, 9)), Some(&30));
        assert_eq!(closed.get(&(100, 5)), None);
    }
}
