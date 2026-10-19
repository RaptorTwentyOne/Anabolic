//! Holding threads still for a moment, the way a debugger does, to read
//! their registers, and reading another process' memory.
//!
//! Every hold happens on a thread of its own that ends with it: the kernel
//! lets go of a thread's tracees when that thread exits, so a stop that
//! never comes (a thread stuck in the kernel) cannot leave a process
//! stopped behind.

use std::io;
use std::time::{Duration, Instant};

/// How long a thread gets to stop before the hold is given up.
const STOP_TIMEOUT: Duration = Duration::from_millis(500);

/// A mapping of a process, as /proc/<pid>/maps lists it.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub start: u64,
    pub end: u64,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    /// Where in the file it begins.
    pub offset: u64,
    pub inode: u64,
    /// A file, `[stack]`, `[heap]`, `[vdso]`, or empty for anonymous memory.
    pub path: String,
}

impl Mapping {
    pub fn contains(&self, address: u64) -> bool {
        (self.start..self.end).contains(&address)
    }

    pub fn file_backed(&self) -> bool {
        self.inode != 0 && self.path.starts_with('/')
    }
}

pub fn maps(pid: u32) -> io::Result<Vec<Mapping>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/maps"))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            // start-end perms offset dev inode path
            let mut f = line.splitn(6, ' ');
            let (start, end) = f.next()?.split_once('-')?;
            let perms = f.next()?.as_bytes();
            let offset = u64::from_str_radix(f.next()?, 16).ok()?;
            let _dev = f.next()?;
            let inode = f.next()?.parse().ok()?;
            Some(Mapping {
                start: u64::from_str_radix(start, 16).ok()?,
                end: u64::from_str_radix(end, 16).ok()?,
                read: perms.first() == Some(&b'r'),
                write: perms.get(1) == Some(&b'w'),
                exec: perms.get(2) == Some(&b'x'),
                offset,
                inode,
                path: f.next().unwrap_or("").trim_start().to_string(),
            })
        })
        .collect())
}

/// Reads `buf.len()` bytes of `pid`'s memory at `address`; how many could
/// be read (a page that is not there ends it).
pub fn read_memory(pid: u32, address: u64, buf: &mut [u8]) -> usize {
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let n = unsafe { libc::process_vm_readv(pid as libc::pid_t, &local, 1, &remote, 1, 0) };
    n.max(0) as usize
}

pub fn read_u64(pid: u32, address: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    (read_memory(pid, address, &mut b) == 8).then(|| u64::from_ne_bytes(b))
}

fn ptrace(request: libc::c_uint, tid: u32, data: usize) -> io::Result<()> {
    let done = unsafe {
        libc::ptrace(
            request,
            tid as libc::pid_t,
            std::ptr::null_mut::<libc::c_void>(),
            data as *mut libc::c_void,
        )
    };
    if done < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// What holding a thread failed on, in words.
fn denied(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EPERM) => {
            "holding another program's thread needs cap_sys_ptrace (and it must not be \
             traced by a debugger already)"
                .into()
        }
        Some(libc::ESRCH) => "the thread has ended".into(),
        _ => crate::util::error_text(e),
    }
}

/// A thread held still: its registers can be read until this is dropped.
pub struct Held {
    tid: u32,
}

impl Held {
    #[cfg(target_arch = "x86_64")]
    pub fn regs(&self) -> io::Result<libc::user_regs_struct> {
        let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        ptrace(libc::PTRACE_GETREGS, self.tid, &raw mut regs as usize)?;
        Ok(regs)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn fpregs(&self) -> io::Result<libc::user_fpregs_struct> {
        let mut regs: libc::user_fpregs_struct = unsafe { std::mem::zeroed() };
        ptrace(libc::PTRACE_GETFPREGS, self.tid, &raw mut regs as usize)?;
        Ok(regs)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = ptrace(libc::PTRACE_DETACH, self.tid, 0);
    }
}

/// Attaches to `tid` without stopping it, then asks it to stop.
fn seize(tid: u32) -> Result<Held, String> {
    ptrace(libc::PTRACE_SEIZE, tid, 0).map_err(|e| denied(&e))?;
    let held = Held { tid };
    ptrace(libc::PTRACE_INTERRUPT, tid, 0).map_err(|e| denied(&e))?;
    Ok(held)
}

/// Waits for a seized thread to stop, up to the deadline.
fn await_stop(tid: u32, deadline: Instant) -> Result<(), String> {
    loop {
        let mut status = 0;
        let got = unsafe {
            libc::waitpid(
                tid as libc::pid_t,
                &mut status,
                libc::__WALL | libc::WNOHANG,
            )
        };
        if got == tid as libc::pid_t {
            if libc::WIFSTOPPED(status) {
                return Ok(());
            }
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                return Err("the thread has ended".into());
            }
        } else if got < 0 {
            return Err(denied(&io::Error::last_os_error()));
        }
        if Instant::now() >= deadline {
            return Err(
                "it did not stop in time: it waits inside the kernel, out of reach until that \
                 wait ends"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Holds the threads `tids` still, runs `f` with them held, and lets them
/// go: `f` gets the held threads (those that stopped) and how long the
/// first has been held when it returns. Runs on a thread of its own (see
/// the module's notes).
pub fn hold<R: Send>(
    tids: &[u32],
    f: impl FnOnce(&[(u32, Held)]) -> R + Send,
) -> Result<(R, Duration), String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let mut held = Vec::with_capacity(tids.len());
                let mut first_error = None;
                for &tid in tids {
                    match seize(tid) {
                        Ok(h) => held.push((tid, h)),
                        Err(e) => {
                            first_error.get_or_insert(e);
                        }
                    }
                }
                let started = Instant::now();
                let deadline = started + STOP_TIMEOUT;
                let mut stopped = Vec::with_capacity(held.len());
                for (tid, h) in held {
                    match await_stop(tid, deadline) {
                        Ok(()) => stopped.push((tid, h)),
                        Err(e) => {
                            // Still attached and running: ending this
                            // thread lets it go (no detach is possible
                            // before it stops).
                            std::mem::forget(h);
                            first_error.get_or_insert(e);
                        }
                    }
                }
                if stopped.is_empty() {
                    return Err(first_error.unwrap_or_else(|| "no thread to hold".into()));
                }
                let result = f(&stopped);
                let took = started.elapsed();
                drop(stopped);
                Ok((result, took))
            })
            .join()
            .unwrap_or_else(|_| Err("reading the thread failed".into()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_a_child_and_reads_its_registers() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(100));
        let result = hold(&[pid], |held| held[0].1.regs().map(|r| r.rip));
        let _ = child.kill();
        let _ = child.wait();
        let (rip, took) = result.expect("a child can be held");
        assert!(rip.expect("registers") != 0);
        assert!(took < Duration::from_millis(500));
        let maps = maps(std::process::id()).unwrap();
        assert!(maps.iter().any(|m| m.exec && m.file_backed()));
        let x = 0x1234_5678_9abc_def0u64;
        assert_eq!(read_u64(std::process::id(), &raw const x as u64), Some(x));
    }
}
