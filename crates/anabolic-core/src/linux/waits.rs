//! Why a program does not respond, on Linux.
//!
//! A Wayland compositor tells no one which windows stopped answering, so
//! `hung_windows` has nothing to say.
//!
//! `wait_chains` reads where each thread sleeps inside the kernel (its
//! wchan, readable for one's own processes, and with cap_sys_ptrace for
//! all) and follows what can be followed: a file lock to the process that
//! holds it (/proc/locks), a pipe or a local socket to the program at its
//! other end, a wait for a child to that child, process to process, until a
//! thread that runs, a wait that leads nowhere, or a loop: a deadlock.
//! Threads that poll for events or sleep are idle and not followed, and
//! neither are futex waits (locks and conditions inside the program),
//! whose other side the kernel does not name.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::model::ProcKey;
use crate::procfs;

use super::*;

/// A Wayland compositor tells no one which windows stopped answering.
pub fn hung_windows() -> Vec<HungWindow> {
    Vec::new()
}

/// How far a chain is followed from process to process.
const MAX_DEPTH: usize = 16;

/// What a thread's state and wchan say it does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sleep {
    Running,
    /// Polling for events, sleeping, waiting for a signal or for work.
    Idle,
    Futex,
    Lock,
    PipeRead,
    PipeWrite,
    Socket,
    Child,
    Kernel,
    Stopped,
}

fn classify(state: u8, wchan: &str) -> Sleep {
    match state {
        b'R' => Sleep::Running,
        b'D' => Sleep::Kernel,
        b'T' | b't' => Sleep::Stopped,
        _ => match wchan {
            w if w.contains("futex") => Sleep::Futex,
            w if w.starts_with("locks_")
                || w.contains("flock")
                || w.contains("posix_lock")
                || w.contains("fcntl_setlk") =>
            {
                Sleep::Lock
            }
            "pipe_wait_readable" | "pipe_read" => Sleep::PipeRead,
            "pipe_wait_writable" | "pipe_write" => Sleep::PipeWrite,
            w if w.starts_with("unix_stream") || w.starts_with("unix_dgram") => Sleep::Socket,
            "do_wait" | "kernel_wait4" | "kernel_waitid" => Sleep::Child,
            _ => Sleep::Idle,
        },
    }
}

/// One thread as it is now.
struct Thread {
    state: u8,
    wchan: String,
    switches: u32,
    /// The file descriptor its system call is on, when that can be read
    /// (its own children, or with cap_sys_ptrace).
    fd: Option<u32>,
}

fn thread(pid: u32, tid: u32) -> Option<Thread> {
    let stat = procfs::task_stat(pid, tid)?;
    let base = format!("/proc/{pid}/task/{tid}");
    let wchan = std::fs::read_to_string(format!("{base}/wchan")).unwrap_or_default();
    let status = std::fs::read_to_string(format!("{base}/status")).unwrap_or_default();
    let count = |key: &str| {
        procfs::status_value(&status, key)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0)
    };
    // "0" while it runs.
    let wchan = wchan.trim();
    Some(Thread {
        state: stat.state,
        wchan: if wchan == "0" { "" } else { wchan }.to_string(),
        switches: count("voluntary_ctxt_switches") + count("nonvoluntary_ctxt_switches"),
        fd: syscall_fd(&base),
    })
}

/// The file a blocking system call works on: its first argument for the
/// calls that take one (read, write, recv, send, flock, fcntl).
#[cfg(target_arch = "x86_64")]
fn syscall_fd(base: &str) -> Option<u32> {
    let text = std::fs::read_to_string(format!("{base}/syscall")).ok()?;
    let mut fields = text.split_ascii_whitespace();
    let nr: u32 = fields.next()?.parse().ok()?;
    // read, write, pread64, pwrite64, readv, writev, sendto, recvfrom,
    // sendmsg, recvmsg, fcntl, flock.
    if ![0, 1, 17, 18, 19, 20, 44, 45, 46, 47, 72, 73].contains(&nr) {
        return None;
    }
    u32::from_str_radix(fields.next()?.trim_start_matches("0x"), 16).ok()
}

#[cfg(not(target_arch = "x86_64"))]
fn syscall_fd(_base: &str) -> Option<u32> {
    None
}

/// One line of /proc/locks.
struct LockLine {
    pid: u32,
    /// "major:minor:inode", as the kernel names the file.
    file: String,
    waiting: bool,
}

fn locks() -> Vec<LockLine> {
    let text = std::fs::read_to_string("/proc/locks").unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            // "3: POSIX ADVISORY WRITE 3810 103:09:42488 0 EOF", or with
            // "->" before the kind for a process waiting for it.
            let mut fields = line.split_ascii_whitespace().skip(1).peekable();
            let waiting = fields.next_if_eq(&"->").is_some();
            let mut fields = fields.skip(3);
            // OFD locks belong to no process (-1).
            let pid = fields.next()?.parse::<u32>().ok()?;
            Some(LockLine {
                pid,
                file: fields.next()?.to_string(),
                waiting,
            })
        })
        .collect()
}

/// A file's identity as /proc/locks writes it, from an open descriptor.
fn lock_name(pid: u32, fd: u32) -> Option<String> {
    let path = std::ffi::CString::new(format!("/proc/{pid}/fd/{fd}")).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let (major, minor) = (libc::major(st.st_dev), libc::minor(st.st_dev));
    Some(format!("{major:02x}:{minor:02x}:{}", st.st_ino))
}

/// What the analysis learns once and uses for every thread.
struct Lab {
    locks: Vec<LockLine>,
    /// Every open pipe and socket ("pipe:[123]"): the processes holding it
    /// and the descriptor. Read on first need: it means reading the name
    /// of every open file of every process.
    fds: Option<HashMap<String, Vec<(u32, u32)>>>,
    peers: Option<HashMap<u64, u64>>,
}

impl Lab {
    fn fds(&mut self) -> &HashMap<String, Vec<(u32, u32)>> {
        self.fds.get_or_insert_with(|| {
            let mut out: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
            for pid in procfs::pids() {
                let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                    continue;
                };
                for e in dir.flatten() {
                    let Some(fd) = e.file_name().to_str().and_then(|f| f.parse().ok()) else {
                        continue;
                    };
                    let Ok(target) = std::fs::read_link(e.path()) else {
                        continue;
                    };
                    let target = target.to_string_lossy();
                    if target.starts_with("pipe:[") || target.starts_with("socket:[") {
                        out.entry(target.into_owned()).or_default().push((pid, fd));
                    }
                }
            }
            out
        })
    }

    fn peers(&mut self) -> &HashMap<u64, u64> {
        self.peers
            .get_or_insert_with(|| crate::netlink::unix_peers().unwrap_or_default())
    }

    /// The thread of `pid` most worth following: one waiting on something
    /// that leads on, else its main thread.
    fn thread_of(&self, pid: u32) -> u32 {
        procfs::tids(pid)
            .into_iter()
            .find(|&tid| {
                thread(pid, tid).is_some_and(|t| {
                    !matches!(
                        classify(t.state, &t.wchan),
                        Sleep::Running | Sleep::Idle | Sleep::Futex
                    )
                })
            })
            .unwrap_or(pid)
    }

    /// Where a thread's wait leads: the object, and the process at its
    /// other end when known.
    fn next(&mut self, pid: u32, t: &Thread, sleep: Sleep) -> Option<(Link, Option<u32>)> {
        let object = |wait, name: String, status| Link::Object { wait, name, status };
        match sleep {
            Sleep::Running | Sleep::Idle => None,
            Sleep::Futex => Some((object(Wait::Futex, String::new(), Status::Blocked), None)),
            Sleep::Kernel => Some((object(Wait::Kernel, t.wchan.clone(), Status::Blocked), None)),
            Sleep::Stopped => Some((object(Wait::Stopped, String::new(), Status::Blocked), None)),
            Sleep::Lock => {
                // The file it waits for: from its system call, else from
                // the waiting process' line in /proc/locks.
                let file = t.fd.and_then(|fd| lock_name(pid, fd)).or_else(|| {
                    self.locks
                        .iter()
                        .find(|l| l.waiting && l.pid == pid)
                        .map(|l| l.file.clone())
                });
                let holder = file.as_ref().and_then(|file| {
                    self.locks
                        .iter()
                        .find(|l| !l.waiting && &l.file == file && l.pid != pid)
                        .map(|l| l.pid)
                });
                let name =
                    t.fd.and_then(|fd| std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).ok())
                        .map(|p| p.to_string_lossy().into_owned())
                        .or(file)
                        .unwrap_or_default();
                let status = if holder.is_some() {
                    Status::Owned
                } else {
                    Status::Unknown
                };
                Some((object(Wait::FileLock, name, status), holder))
            }
            Sleep::PipeRead | Sleep::PipeWrite => {
                // From the program at the other end: its writer when this
                // one reads, its reader when it writes.
                let wants_writer = sleep == Sleep::PipeRead;
                let pipes = self.ends(pid, t.fd, "pipe:[", Some(!wants_writer));
                for pipe in pipes {
                    let holders = self.fds().get(&pipe).cloned().unwrap_or_default();
                    let other = holders
                        .iter()
                        .find(|&&(p, fd)| p != pid && writes(p, fd) == wants_writer);
                    if let Some(&(other, _)) = other {
                        let name = pipe.trim_start_matches("pipe:[").trim_end_matches(']');
                        return Some((
                            object(Wait::Pipe, format!("pipe {name}"), Status::Owned),
                            Some(other),
                        ));
                    }
                }
                Some((object(Wait::Pipe, String::new(), Status::Unknown), None))
            }
            Sleep::Socket => {
                for socket in self.ends(pid, t.fd, "socket:[", None) {
                    let Some(inode) = socket
                        .trim_start_matches("socket:[")
                        .trim_end_matches(']')
                        .parse::<u64>()
                        .ok()
                    else {
                        continue;
                    };
                    let Some(&peer) = self.peers().get(&inode) else {
                        continue;
                    };
                    let holders = self.fds().get(&format!("socket:[{peer}]")).cloned();
                    if let Some(&(other, _)) = holders.iter().flatten().find(|(p, _)| *p != pid) {
                        return Some((
                            object(Wait::LocalSocket, format!("socket {inode}"), Status::Owned),
                            Some(other),
                        ));
                    }
                }
                Some((
                    object(Wait::LocalSocket, String::new(), Status::Unknown),
                    None,
                ))
            }
            Sleep::Child => {
                let children: Vec<(u32, String)> = procfs::pids()
                    .into_iter()
                    .filter_map(|p| {
                        procfs::stat(p)
                            .filter(|s| s.ppid == pid)
                            .map(|s| (p, s.comm))
                    })
                    .collect();
                let name = match children.as_slice() {
                    [] => String::new(),
                    [(_, comm)] => comm.clone(),
                    [(_, comm), rest @ ..] => format!("{comm} (and {} more)", rest.len()),
                };
                let status = if children.is_empty() {
                    Status::Unknown
                } else {
                    Status::Owned
                };
                Some((
                    object(Wait::Process, name, status),
                    children.first().map(|(p, _)| *p),
                ))
            }
        }
    }

    /// The pipes or sockets (`kind`) a process holds: the one its system
    /// call is on when known, else all of them it holds the right way
    /// (`writing`: for writing, or for reading; None: either).
    fn ends(&self, pid: u32, fd: Option<u32>, kind: &str, writing: Option<bool>) -> Vec<String> {
        let link = |fd: u32| {
            std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
                .ok()
                .map(|t| t.to_string_lossy().into_owned())
        };
        if let Some(target) = fd.and_then(link).filter(|t| t.starts_with(kind)) {
            return vec![target];
        }
        let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return Vec::new();
        };
        let mut out: Vec<String> = dir
            .flatten()
            .filter_map(|e| {
                let fd: u32 = e.file_name().to_str()?.parse().ok()?;
                let target = std::fs::read_link(e.path()).ok()?;
                let target = target.to_string_lossy().into_owned();
                let right_way = writing.is_none_or(|w| writes(pid, fd) == w);
                (target.starts_with(kind) && right_way).then_some(target)
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Follows a thread's wait from link to link. Returns whether the
    /// chain came back to a thread already in it.
    fn follow(
        &mut self,
        pid: u32,
        tid: u32,
        links: &mut Vec<Link>,
        path: &mut Vec<(u32, u32)>,
    ) -> bool {
        if path.contains(&(pid, tid)) {
            return true;
        }
        path.push((pid, tid));
        let Some(t) = thread(pid, tid) else {
            links.push(Link::Thread {
                pid,
                tid,
                status: Status::Error,
                context_switches: 0,
            });
            return false;
        };
        let sleep = classify(t.state, &t.wchan);
        links.push(Link::Thread {
            pid,
            tid,
            status: if sleep == Sleep::Running {
                Status::Running
            } else {
                Status::Blocked
            },
            context_switches: t.switches,
        });
        if path.len() > MAX_DEPTH {
            return false;
        }
        let Some((object, holder)) = self.next(pid, &t, sleep) else {
            return false;
        };
        links.push(object);
        match holder {
            Some(holder) => {
                let tid = self.thread_of(holder);
                self.follow(holder, tid, links, path)
            }
            None => false,
        }
    }
}

/// Whether a process holds this file descriptor for writing.
fn writes(pid: u32, fd: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}"))
        .ok()
        .and_then(|info| {
            let flags = procfs::status_value(&info, "flags")?;
            u32::from_str_radix(flags, 8).ok()
        })
        .is_some_and(|flags| flags & libc::O_ACCMODE as u32 != libc::O_RDONLY as u32)
}

/// What the threads of `key` wait for. `first` (a thread asked about) is
/// analysed and listed first. `keep_going` hears how many threads are done
/// of how many; when it returns false the analysis stops.
pub fn wait_chains(
    key: ProcKey,
    first: Option<u32>,
    keep_going: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<WaitReport, String> {
    let started = Instant::now();
    let pid = key.pid;
    if procfs::stat(pid).is_none_or(|s| key.create_time != 0 && s.create_time() != key.create_time)
    {
        return Err("the process has exited".into());
    }
    let mut tids = procfs::tids(pid);
    if tids.is_empty() {
        return Err("the process has no threads (it has exited)".into());
    }
    if let Some(first) = first
        && let Some(i) = tids.iter().position(|&t| t == first)
    {
        tids[..=i].rotate_right(1);
    }
    let mut lab = Lab {
        locks: locks(),
        fds: None,
        peers: None,
    };
    let total = tids.len();
    let (mut chains, mut idle, mut failed) = (Vec::new(), 0, 0);
    let mut counted = HashSet::new();
    for (done, &tid) in tids.iter().enumerate() {
        if !keep_going(done, total) {
            return Err("stopped".into());
        }
        let Some(t) = thread(pid, tid) else {
            failed += 1;
            continue;
        };
        let sleep = classify(t.state, &t.wchan);
        let asked = Some(tid) == first;
        if !asked && matches!(sleep, Sleep::Running | Sleep::Idle | Sleep::Futex) {
            idle += 1;
            continue;
        }
        let mut links = Vec::new();
        let cycle = lab.follow(pid, tid, &mut links, &mut Vec::new());
        // A chain seen from another of its threads already: one is enough.
        if cycle && !counted.insert(chain_key(&links)) {
            continue;
        }
        if links.len() > 1 || asked {
            chains.push(Chain { links, cycle });
        }
    }
    let first_tid = |c: &Chain| match c.links.first() {
        Some(Link::Thread { tid, .. }) => Some(*tid),
        _ => None,
    };
    // Deadlocks, then the thread asked about, then the longest chains.
    chains.sort_by_key(|c| {
        (
            !c.cycle,
            first.is_none() || first_tid(c) != first,
            std::cmp::Reverse(c.links.len()),
        )
    });
    Ok(WaitReport {
        key,
        chains,
        threads: total,
        idle,
        failed,
        skipped: 0,
        took: started.elapsed(),
    })
}

/// The threads in a chain, in order, whichever one it was entered from.
fn chain_key(links: &[Link]) -> Vec<(u32, u32)> {
    let mut threads: Vec<(u32, u32)> = links
        .iter()
        .filter_map(|l| match l {
            Link::Thread { pid, tid, .. } => Some((*pid, *tid)),
            Link::Object { .. } => None,
        })
        .collect();
    threads.sort_unstable();
    threads.dedup();
    threads
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    fn key(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: procfs::stat(pid).unwrap().create_time(),
        }
    }

    /// Waits until a child sleeps where `wchan` says, a few seconds at most.
    fn settle(pid: u32, wanted: impl Fn(&str) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            let w = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
            if wanted(&w) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A child waits for a lock this process holds: its chain leads here.
    #[test]
    fn follows_a_file_lock_to_its_holder() {
        let dir = std::env::temp_dir().join(format!("anabolic-waits-{}", std::process::id()));
        std::fs::write(&dir, b"").unwrap();
        let file = std::fs::File::open(&dir).unwrap();
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
        let mut child = Command::new("flock")
            .arg(&dir)
            .arg("true")
            .stdout(Stdio::null())
            .spawn()
            .expect("flock(1) runs");
        let pid = child.id();
        settle(pid, |w| w.starts_with("locks_") || w.contains("flock"));
        let report = wait_chains(key(pid), None, &mut |_, _| true).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&dir);
        let chain = report
            .chains
            .first()
            .expect("the waiting thread has a chain");
        assert!(
            matches!(
                &chain.links[1],
                Link::Object {
                    wait: Wait::FileLock,
                    status: Status::Owned,
                    ..
                }
            ),
            "{chain:?}"
        );
        assert!(
            matches!(chain.links.get(2), Some(Link::Thread { pid, .. }) if *pid == std::process::id()),
            "{chain:?}"
        );
    }

    /// A child reads a pipe this process writes to: its chain leads here.
    #[test]
    fn follows_a_pipe_to_its_writer() {
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        settle(pid, |w| w.starts_with("pipe_"));
        let report = wait_chains(key(pid), None, &mut |_, _| true).unwrap();
        drop(child.stdin.take());
        let _ = child.wait();
        let chain = report.chains.first().expect("cat's thread has a chain");
        assert!(
            matches!(
                &chain.links[1],
                Link::Object {
                    wait: Wait::Pipe,
                    ..
                }
            ),
            "{chain:?}"
        );
        assert!(
            matches!(chain.links.get(2), Some(Link::Thread { pid, .. }) if *pid == std::process::id()),
            "{chain:?}"
        );
    }

    /// A sleeping child is idle: nothing to follow.
    #[test]
    fn a_sleep_is_idle() {
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        let pid = child.id();
        settle(pid, |w| w.contains("nanosleep"));
        let report = wait_chains(key(pid), None, &mut |_, _| true).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert!(report.chains.is_empty(), "{:?}", report.chains);
        assert_eq!(report.idle, 1);
    }
}
