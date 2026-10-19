//! Process enumeration from /proc: every process' stat, status and I/O
//! counters, read once per refresh. These are the kernel's own accounting:
//! no process is opened or stopped to read them.
//!
//! The kernel's threads (kworkers, kswapd, the threads behind interrupts)
//! are counted under one process, kthreadd (PID 2), the way Windows counts
//! its own under System: listed one by one they would bury the programs.
//! The details' thread list shows them each.

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::{DiskCounters, InfoLevel, IoCounters};
use crate::procfs::{self, Stat};

use super::*;

/// kthreadd: the parent of every kernel thread, and of the Interrupts
/// pseudo process.
pub const KERNEL_PID: u32 = 2;

/// Kernel traces here name the process with the thread.
pub const TRACES_NEED_THREAD_OWNERS: bool = false;

/// What the kernel's threads are called together.
const KERNEL_NAME: &str = "Kernel";

/// Read once per process: none of it changes while it lives.
struct Known {
    starttime: u64,
    uid: Option<[u8; 4]>,
    /// The login session (audit session id); 0 for none (system services).
    session: u32,
    /// Its I/O counters or open files could not be read: not tried again.
    io_denied: bool,
    fds_denied: bool,
    /// What was read the last time it ran.
    slow: Option<Slow>,
}

/// The costlier reads (memory, I/O, open files), kept while a process does
/// not run: none of them can change much without it running, and most
/// processes sleep through most refreshes.
#[derive(Clone)]
struct Slow {
    /// CPU time, faults, state and threads when it was read.
    ran: (u64, u64, u64, u64, u8, u32),
    /// Refreshes since it was read.
    age: u32,
    status: Option<Arc<str>>,
    io: Option<IoCounters>,
    disk: Option<DiskCounters>,
    fds: u32,
}

/// Read again at least this often, for what changes while a process
/// sleeps (memory reclaimed or swapped out).
const REREAD_EVERY: u32 = 10;

/// One process as read this refresh.
struct Raw {
    stat: Stat,
    status: Option<Arc<str>>,
    io: Option<IoCounters>,
    disk: Option<DiskCounters>,
    fds: u32,
}

pub struct ProcessScanner {
    entries: Vec<Raw>,
    known: HashMap<u32, Known>,
    /// The kernel threads folded into kthreadd this refresh.
    kernel_threads: Vec<u32>,
    level: InfoLevel,
}

impl Default for ProcessScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessScanner {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            known: HashMap::new(),
            kernel_threads: Vec::new(),
            level: InfoLevel::Full,
        }
    }

    /// `Full` when every process' I/O counters could be read (running as
    /// root, or with CAP_SYS_PTRACE); else `Basic`.
    pub fn level(&self) -> InfoLevel {
        self.level
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        let pids = procfs::pids();
        if pids.is_empty() {
            return Err("/proc could not be read".into());
        }
        self.entries.clear();
        self.kernel_threads.clear();
        let mut kernel = Raw {
            stat: Stat::default(),
            status: None,
            io: None,
            disk: None,
            fds: 0,
        };
        let mut kernel_seen = false;
        let mut all_io = true;
        let mut alive = std::collections::HashSet::with_capacity(pids.len());
        for pid in pids {
            let Some(stat) = procfs::stat(pid) else {
                continue;
            };
            alive.insert(pid);
            if stat.kernel_thread() && pid != KERNEL_PID {
                // Folded into kthreadd: its times, faults and threads.
                kernel.stat.utime += stat.utime;
                kernel.stat.stime += stat.stime;
                kernel.stat.minflt += stat.minflt;
                kernel.stat.majflt += stat.majflt;
                kernel.stat.num_threads += 1;
                self.kernel_threads.push(pid);
                continue;
            }
            if pid == KERNEL_PID {
                kernel_seen = true;
                let folded = std::mem::take(&mut kernel.stat);
                kernel.stat = Stat {
                    utime: stat.utime + folded.utime,
                    stime: stat.stime + folded.stime,
                    minflt: stat.minflt + folded.minflt,
                    majflt: stat.majflt + folded.majflt,
                    num_threads: stat.num_threads + folded.num_threads,
                    ..stat
                };
                continue;
            }
            let known = match self.known.get_mut(&pid) {
                Some(k) if k.starttime == stat.starttime => k,
                _ => {
                    self.known.insert(pid, known(pid, &stat));
                    self.known.get_mut(&pid).unwrap()
                }
            };
            let ran = (
                stat.utime,
                stat.stime,
                stat.minflt,
                stat.majflt,
                stat.state,
                stat.num_threads,
            );
            let fresh = match &mut known.slow {
                Some(slow) if slow.ran == ran && slow.age < REREAD_EVERY => {
                    slow.age += 1;
                    slow.clone()
                }
                _ => {
                    let slow = read_slow(pid, ran, known);
                    known.slow = Some(slow.clone());
                    slow
                }
            };
            all_io &= fresh.io.is_some() || stat.state == b'Z';
            self.entries.push(Raw {
                stat,
                status: fresh.status,
                io: fresh.io,
                disk: fresh.disk,
                fds: fresh.fds,
            });
        }
        if kernel_seen {
            self.entries.push(kernel);
        }
        self.known.retain(|pid, _| alive.contains(pid));
        self.level = if all_io {
            InfoLevel::Full
        } else {
            InfoLevel::Basic
        };
        Ok(())
    }

    pub fn entries(&self) -> impl Iterator<Item = Entry<'_>> {
        self.entries.iter().map(|raw| Entry { raw, scanner: self })
    }
}

/// Memory, I/O counters and open files, read now.
fn read_slow(pid: u32, ran: (u64, u64, u64, u64, u8, u32), known: &mut Known) -> Slow {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .map(Arc::from);
    let (io, disk) = if known.io_denied {
        (None, None)
    } else {
        match read_io(pid) {
            Ok(counters) => counters,
            Err(denied) => {
                known.io_denied = denied;
                (None, None)
            }
        }
    };
    let fds = if known.fds_denied {
        0
    } else {
        match open_files(pid) {
            Ok(n) => n,
            Err(e) => {
                known.fds_denied = e.kind() == std::io::ErrorKind::PermissionDenied;
                0
            }
        }
    };
    Slow {
        ran,
        age: 0,
        status,
        io,
        disk,
        fds,
    }
}

/// How many files a process has open. Since Linux 6.2 the size of its fd
/// directory says, in one stat; before, the entries are counted.
fn open_files(pid: u32) -> std::io::Result<u32> {
    let path = format!("/proc/{pid}/fd");
    let size = std::fs::metadata(&path)?.len();
    if size > 0 {
        return Ok(size as u32);
    }
    Ok(std::fs::read_dir(&path)?.count() as u32)
}

/// The facts read once: owner, session.
fn known(pid: u32, stat: &Stat) -> Known {
    let session = std::fs::read_to_string(format!("/proc/{pid}/sessionid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&s| s != u32::MAX)
        .unwrap_or(0);
    Known {
        starttime: stat.starttime,
        uid: procfs::uid(pid).map(u32::to_le_bytes),
        session,
        io_denied: false,
        fds_denied: false,
        slow: None,
    }
}

/// The I/O counters: everything read and written through system calls,
/// and what of it reached storage. Err(true) when reading them is not
/// allowed (another user's process, without the right to trace it).
fn read_io(pid: u32) -> Result<(Option<IoCounters>, Option<DiskCounters>), bool> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/io")) {
        Ok(text) => text,
        Err(e) => return Err(e.kind() == std::io::ErrorKind::PermissionDenied),
    };
    let field = |key: &str| {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(key)?
                    .strip_prefix(':')?
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or(0)
    };
    let io = IoCounters {
        read_bytes: field("rchar"),
        write_bytes: field("wchar"),
        other_bytes: 0,
        read_ops: field("syscr"),
        write_ops: field("syscw"),
        other_ops: 0,
    };
    let disk = DiskCounters {
        read_bytes: field("read_bytes"),
        // Written to the page cache, then truncated or deleted before it
        // reached the disk: never written.
        write_bytes: field("write_bytes").saturating_sub(field("cancelled_write_bytes")),
        read_ops: 0,
        write_ops: 0,
        flush_ops: 0,
    };
    Ok((Some(io), Some(disk)))
}

/// Windows' base priority for a nice value and scheduling policy, so the
/// priority classes (`actions::Priority::from_base`) read the same on both.
pub fn base_priority(nice: i64, policy: u32) -> i32 {
    match policy {
        // SCHED_FIFO, SCHED_RR, SCHED_DEADLINE
        1 | 2 | 6 => 24,
        // SCHED_IDLE
        5 => 4,
        _ => match nice {
            15.. => 4,
            5..=14 => 6,
            -4..=4 => 8,
            -9..=-5 => 10,
            _ => 13,
        },
    }
}

pub struct Entry<'a> {
    raw: &'a Raw,
    scanner: &'a ProcessScanner,
}

impl Entry<'_> {
    pub fn pid(&self) -> u32 {
        self.raw.stat.pid
    }

    /// The name to show: the kernel's short name (15 characters at most),
    /// or the program's own when that was cut short.
    pub fn image(&self) -> String {
        let stat = &self.raw.stat;
        if stat.pid == KERNEL_PID {
            return KERNEL_NAME.into();
        }
        if stat.comm.len() >= 15 {
            let argv0 = procfs::argv(stat.pid)
                .into_iter()
                .next()
                .unwrap_or_default();
            let file = argv0
                .rsplit('/')
                .next()
                .unwrap_or("")
                .split(' ')
                .next()
                .unwrap_or("");
            if file.starts_with(&stat.comm) {
                return file.to_string();
            }
            if let Some(exe) = procfs::exe(stat.pid) {
                let file = exe.rsplit('/').next().unwrap_or("");
                if file.starts_with(&stat.comm) {
                    return file.to_string();
                }
            }
        }
        stat.comm.clone()
    }

    /// The owner's uid, as 4 little-endian bytes (`ProcessInfo::user_sid`).
    pub fn user_sid(&self) -> Option<&[u8]> {
        let known = self.scanner.known.get(&self.raw.stat.pid)?;
        known.uid.as_ref().map(|u| &u[..])
    }

    pub fn sample(&self) -> ProcSample {
        let raw = self.raw;
        let stat = &raw.stat;
        let status = raw.status.as_deref().unwrap_or("");
        let kb = |key: &str| procfs::status_kb(status, key).unwrap_or(0);
        let count = |key: &str| {
            procfs::status_value(status, key)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let anon = kb("RssAnon");
        let session = self.scanner.known.get(&stat.pid).map_or(0, |k| k.session);
        ProcSample {
            pid: stat.pid,
            ppid: stat.ppid,
            create_time: stat.create_time(),
            session_id: session,
            base_priority: base_priority(stat.nice, stat.policy),
            threads: stat.num_threads,
            handles: raw.fds,
            kernel_time: stat.kernel_time(),
            user_time: stat.user_time(),
            cycles: 0,
            private_bytes: anon + kb("VmSwap"),
            working_set: kb("VmRSS").max(stat.rss_pages * procfs::page_size()),
            private_working_set: anon,
            peak_working_set: kb("VmHWM"),
            virtual_size: stat.vsize,
            paged_pool: 0,
            nonpaged_pool: 0,
            page_faults: (stat.minflt + stat.majflt) as u32,
            hard_faults: stat.majflt as u32,
            io: raw.io.unwrap_or_default(),
            disk: raw.disk,
            // The main thread's: the kernel keeps no sum for the process.
            context_switches: count("voluntary_ctxt_switches")
                + count("nonvoluntary_ctxt_switches"),
            suspended: matches!(stat.state, b'T' | b't'),
            // Ended, and not yet collected by its parent.
            ending: matches!(stat.state, b'Z' | b'X'),
        }
    }

    pub fn thread_samples(&self) -> impl Iterator<Item = ThreadSample> + '_ {
        let pid = self.raw.stat.pid;
        let tasks: Vec<(u32, u32)> = if pid == KERNEL_PID {
            std::iter::once(KERNEL_PID)
                .chain(self.scanner.kernel_threads.iter().copied())
                .map(|tid| (tid, tid))
                .collect()
        } else {
            procfs::tids(pid)
                .into_iter()
                .map(|tid| (pid, tid))
                .collect()
        };
        tasks
            .into_iter()
            .filter_map(|(pid, tid)| thread_sample(pid, tid))
    }
}

fn thread_sample(pid: u32, tid: u32) -> Option<ThreadSample> {
    let stat = procfs::task_stat(pid, tid)?;
    let status =
        std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/status")).unwrap_or_default();
    let count = |key: &str| {
        procfs::status_value(&status, key)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0)
    };
    // The system call a sleeping thread waits in (needs the right to look
    // into the process); 0 when running or unreadable.
    let wait_reason = match stat.state {
        b'S' | b'D' => std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/syscall"))
            .ok()
            .and_then(|s| s.split_ascii_whitespace().next()?.parse::<u32>().ok())
            .map_or(0, |nr| nr + 1),
        _ => 0,
    };
    Some(ThreadSample {
        tid,
        create_time: stat.create_time(),
        start_address: 0,
        win32_start_address: 0,
        priority: stat.priority as i32,
        base_priority: stat.nice as i32,
        state: u32::from(stat.state),
        wait_reason,
        kernel_time: stat.kernel_time(),
        user_time: stat.user_time(),
        context_switches: count("voluntary_ctxt_switches") + count("nonvoluntary_ctxt_switches"),
        stack_base: 0,
        stack_limit: 0,
        teb: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_self_and_the_kernel() {
        let mut scanner = ProcessScanner::new();
        scanner.refresh().expect("/proc is readable");
        let me = std::process::id();
        let mut found_self = false;
        let mut found_kernel = false;
        let mut count = 0;
        for e in scanner.entries() {
            count += 1;
            let s = e.sample();
            if e.pid() == me {
                found_self = true;
                assert!(s.threads >= 1);
                assert!(s.working_set > 0);
                assert!(s.handles > 0, "own open files are countable");
                assert!(e.user_sid().is_some());
                assert!(e.thread_samples().any(|t| t.tid == me));
                assert!(e.image().starts_with("anabolic"), "{}", e.image());
            }
            if e.pid() == KERNEL_PID {
                found_kernel = true;
                assert_eq!(e.image(), KERNEL_NAME);
                assert!(s.threads > 1, "kernel threads are folded in");
            }
            assert!(
                !procfs::stat(e.pid()).is_some_and(|s| s.kernel_thread() && e.pid() != KERNEL_PID),
                "kernel thread {} listed on its own",
                e.pid()
            );
        }
        assert!(count > 10, "only {count} processes");
        assert!(found_self && found_kernel);
    }

    #[test]
    fn priority_classes_survive_the_round_trip() {
        use crate::actions::Priority;
        assert_eq!(
            Priority::from_base(base_priority(0, 0)),
            Some(Priority::Normal)
        );
        assert_eq!(
            Priority::from_base(base_priority(19, 0)),
            Some(Priority::Idle)
        );
        assert_eq!(
            Priority::from_base(base_priority(10, 0)),
            Some(Priority::BelowNormal)
        );
        assert_eq!(
            Priority::from_base(base_priority(-5, 0)),
            Some(Priority::AboveNormal)
        );
        assert_eq!(
            Priority::from_base(base_priority(-10, 0)),
            Some(Priority::High)
        );
        assert_eq!(
            Priority::from_base(base_priority(0, 2)),
            Some(Priority::Realtime)
        );
    }
}
