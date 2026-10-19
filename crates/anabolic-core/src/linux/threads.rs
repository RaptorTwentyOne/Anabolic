//! One thread of another process on Linux: its name, its stack, and what
//! can be done to it. Linux keeps a priority (nice) per thread, and a
//! thread can be ended only with its whole process.

use std::collections::HashMap;
use std::time::Instant;

use crate::model::ProcKey;
use crate::modules::Symbolizer;
use crate::procfs;

use super::*;

/// Thread priorities as nice values, with names: lower is more urgent.
pub const PRIORITIES: [(i32, &str); 7] = [
    (-20, "Highest"),
    (-10, "High"),
    (-5, "Above normal"),
    (0, "Normal"),
    (5, "Below normal"),
    (10, "Low"),
    (19, "Lowest"),
];

pub fn apply(pid: u32, tid: u32, op: ThreadOp) -> Result<String, String> {
    if pid == std::process::id() {
        return Err("Anabolic does not act on its own threads".into());
    }
    // The thread must still belong to the process asked about.
    if !std::path::Path::new(&format!("/proc/{pid}/task/{tid}")).exists() {
        return Err(format!(
            "thread {tid} is not one of process {pid}'s any more"
        ));
    }
    let signal_thread = |signal: i32| {
        let done = unsafe {
            libc::syscall(
                libc::SYS_tgkill,
                pid as libc::pid_t,
                tid as libc::pid_t,
                signal,
            )
        };
        if done == 0 {
            Ok(())
        } else {
            Err(crate::util::error_text(&std::io::Error::last_os_error()))
        }
    };
    match op {
        // A stop signal stops the whole process; Linux has no way to hold
        // one thread from outside short of debugging it.
        ThreadOp::Suspend => Err(
            "Linux cannot suspend one thread of a process from outside; suspend the process".into(),
        ),
        ThreadOp::Resume => signal_thread(libc::SIGCONT).map(|_| format!("Resumed thread {tid}")),
        ThreadOp::Terminate => {
            Err("Linux ends a thread only with its process: a thread cannot be ended alone".into())
        }
        ThreadOp::Priority(level) => {
            let done = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, level) };
            if done != 0 {
                let e = std::io::Error::last_os_error();
                return Err(match e.kind() {
                    std::io::ErrorKind::PermissionDenied if level < 0 => format!(
                        "{}: raising a priority needs cap_sys_nice",
                        crate::util::error_text(&e)
                    ),
                    _ => crate::util::error_text(&e),
                });
            }
            let name = PRIORITIES
                .iter()
                .find(|(l, _)| *l == level)
                .map_or("custom", |(_, n)| n);
            Ok(format!("Thread {tid} priority set to {name}"))
        }
    }
}

/// The names threads carry (what the program called them, `comm`).
pub fn names(pid: u32, tids: &[u32]) -> HashMap<u32, String> {
    tids.iter()
        .filter_map(|&tid| {
            let name = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm")).ok()?;
            Some((tid, name.trim().to_string()))
        })
        .collect()
}

/// A process' threads with their CPU time in 100 ns units, the busiest
/// first.
pub fn by_cpu_time(pid: u32) -> Result<Vec<(u32, i64)>, String> {
    let tids = procfs::tids(pid);
    if tids.is_empty() {
        return Err("the thread list could not be read (the process has exited)".into());
    }
    let mut threads: Vec<(u32, i64)> = tids
        .into_iter()
        .filter_map(|tid| {
            let s = procfs::task_stat(pid, tid)?;
            Some((tid, (s.kernel_time() + s.user_time()) as i64))
        })
        .collect();
    threads.sort_by_key(|t| std::cmp::Reverse(t.1));
    Ok(threads)
}

/// The thread's stack inside the kernel: where it waits. Reading it needs
/// cap_sys_admin; user-mode frames need a debugger attached, which
/// Anabolic does not do.
pub fn call_stack(
    key: ProcKey,
    tid: u32,
    _fallback: &mut Symbolizer,
) -> Result<StackReport, String> {
    let started = Instant::now();
    if key.pid == std::process::id() {
        return Err("Anabolic does not read its own threads' stacks".into());
    }
    let text = std::fs::read_to_string(format!("/proc/{}/task/{tid}/stack", key.pid)).map_err(
        |e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                "reading a thread's kernel stack needs cap_sys_admin".to_string()
            }
            _ => crate::util::error_text(&e),
        },
    )?;
    // "[<0>] futex_wait_queue+0x60/0x90"
    let frames = text
        .lines()
        .filter_map(|line| {
            let symbol = line.split_once("] ")?.1.trim();
            Some(Frame {
                address: 0,
                symbol: Some(format!("kernel!{symbol}")),
            })
        })
        .collect();
    Ok(StackReport {
        key,
        tid,
        frames,
        wow64: false,
        held: std::time::Duration::ZERO,
        took: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_times_own_threads() {
        let me = std::process::id();
        let threads = by_cpu_time(me).unwrap();
        assert!(threads.iter().any(|(tid, _)| *tid == me));
        let names = names(me, &[me]);
        assert!(!names[&me].is_empty());
    }
}
