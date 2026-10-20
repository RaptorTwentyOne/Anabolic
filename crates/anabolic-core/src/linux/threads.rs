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

/// Frames past this are not followed.
const MAX_FRAMES: usize = 64;
/// How far above the stack pointer the fallback looks for return addresses.
const SCAN_BYTES: usize = 32 * 1024;

/// The thread's call stack, innermost first: where it waits inside the
/// kernel (/proc, which needs cap_sys_admin), then its own frames. Those
/// are read the way a debugger does: the thread alone is held still for
/// the moment it takes to copy its registers and stack (ptrace, which
/// needs cap_sys_ptrace for a process Anabolic did not start), and the
/// frame pointers followed. Code built without frame pointers leaves only
/// a guess, from scanning the stack for return addresses: those frames
/// are marked `?`.
pub fn call_stack(key: ProcKey, tid: u32, symbols: &mut Symbolizer) -> Result<StackReport, String> {
    let started = Instant::now();
    if key.pid == std::process::id() {
        return Err("Anabolic does not read its own threads' stacks".into());
    }
    if key.create_time != 0
        && procfs::stat(key.pid).is_none_or(|s| s.create_time() != key.create_time)
    {
        return Err("the process has exited".into());
    }
    let thread = procfs::task_stat(key.pid, tid).ok_or("the thread has ended")?;
    let mut problems = Vec::new();
    // Read before the thread is held: holding it takes it out of its wait.
    let mut frames = match kernel_frames(key.pid, tid) {
        Ok(frames) => frames,
        Err(e) => {
            problems.push(e);
            Vec::new()
        }
    };
    let mut held = std::time::Duration::ZERO;
    if thread.kernel_thread() {
        // All of a kernel thread is the kernel part.
    } else if thread.state == b'D' {
        problems.push("its own frames are out of reach while it waits inside the kernel".into());
    } else {
        match user_frames(key.pid, tid, symbols) {
            Ok((user, took)) => {
                frames.extend(user);
                held = took;
            }
            Err(e) => problems.push(e),
        }
    }
    if frames.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(StackReport {
        key,
        tid,
        frames,
        wow64: false,
        held,
        took: started.elapsed(),
    })
}

/// Where the thread is inside the kernel, as /proc shows it.
fn kernel_frames(pid: u32, tid: u32) -> Result<Vec<Frame>, String> {
    let text =
        std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stack")).map_err(|e| {
            match e.kind() {
                std::io::ErrorKind::PermissionDenied => {
                    "its kernel stack needs cap_sys_admin".to_string()
                }
                _ => crate::util::error_text(&e),
            }
        })?;
    // "[<0>] futex_wait_queue+0x60/0x90"
    Ok(text
        .lines()
        .filter_map(|line| {
            let symbol = line.split_once("] ")?.1.trim();
            let symbol = symbol.split_once('/').map_or(symbol, |(at, _)| at);
            Some(Frame {
                address: 0,
                symbol: Some(format!("kernel!{symbol}")),
            })
        })
        .collect())
}

/// The thread's own frames, and how long it was held for them.
#[cfg(target_arch = "x86_64")]
fn user_frames(
    pid: u32,
    tid: u32,
    symbols: &mut Symbolizer,
) -> Result<(Vec<Frame>, std::time::Duration), String> {
    let maps = crate::ptrace::maps(pid).map_err(|e| crate::util::error_text(&e))?;
    let (walked, held) = crate::ptrace::hold(&[tid], |held| {
        let regs = held[0].1.regs().map_err(|e| crate::util::error_text(&e))?;
        Ok::<_, String>(walk(pid, regs.rip, regs.rsp, regs.rbp, &maps))
    })?;
    let (addresses, exact) = walked?;
    // Named once the thread runs again.
    let modules = crate::modules::process_modules(pid).unwrap_or_default();
    let frames = addresses
        .into_iter()
        .enumerate()
        .map(|(i, address)| {
            let name = symbols.name(&modules, address);
            // The first is where it is: always exact.
            let symbol = if exact || i == 0 {
                name
            } else {
                Some(format!(
                    "? {}",
                    name.unwrap_or_else(|| format!("0x{address:X}"))
                ))
            };
            Frame { address, symbol }
        })
        .collect();
    Ok((frames, held))
}

#[cfg(not(target_arch = "x86_64"))]
fn user_frames(
    _pid: u32,
    _tid: u32,
    _symbols: &mut Symbolizer,
) -> Result<(Vec<Frame>, std::time::Duration), String> {
    Err("its own frames are read on x86-64 only".into())
}

/// Return addresses from the instruction pointer outwards, and whether
/// they came from the frame pointer chain (exact) rather than a scan.
#[cfg(target_arch = "x86_64")]
fn walk(
    pid: u32,
    rip: u64,
    rsp: u64,
    rbp: u64,
    maps: &[crate::ptrace::Mapping],
) -> (Vec<u64>, bool) {
    use crate::ptrace::{read_memory, read_u64};
    let code = |address: u64| {
        maps.iter()
            .any(|m| m.exec && m.contains(address) && (m.file_backed() || m.path == "[vdso]"))
    };
    let Some(stack) = maps.iter().find(|m| m.contains(rsp)) else {
        return (vec![rip], true);
    };
    let mut chain = vec![rip];
    let mut bp = rbp;
    while chain.len() < MAX_FRAMES && bp >= rsp && bp.is_multiple_of(8) && stack.contains(bp + 8) {
        let (Some(next), Some(ret)) = (read_u64(pid, bp), read_u64(pid, bp + 8)) else {
            break;
        };
        if !code(ret) {
            break;
        }
        chain.push(ret);
        // Frames go up the stack; anything else is not a frame pointer.
        if next <= bp {
            break;
        }
        bp = next;
    }
    if chain.len() >= 3 {
        return (chain, true);
    }
    // No usable chain: values on the stack that point just past a call.
    let mut bytes = vec![0u8; SCAN_BYTES.min((stack.end - rsp) as usize)];
    let len = read_memory(pid, rsp, &mut bytes);
    let mut found = vec![rip];
    for word in bytes[..len].as_chunks::<8>().0 {
        if found.len() >= MAX_FRAMES {
            break;
        }
        let value = u64::from_ne_bytes(*word);
        if value > 7 && code(value) && after_call(pid, value) {
            found.push(value);
        }
    }
    (found, false)
}

/// Whether the bytes before `address` end in a call instruction: what
/// makes a value on the stack a return address rather than a pointer.
#[cfg(target_arch = "x86_64")]
fn after_call(pid: u32, address: u64) -> bool {
    let mut b = [0u8; 7];
    if crate::ptrace::read_memory(pid, address - 7, &mut b) != 7 {
        return false;
    }
    // b[i] is the byte at address - 7 + i; the opcode's forms by length.
    b[2] == 0xE8 // call rel32
        || (b[1] == 0xFF && (b[2] == 0x15 || b[2] & 0xF8 == 0x90)) // [rip+d32], [reg+d32]
        || (b[3] == 0xFF && b[4] == 0x54) // [base+index+d8]
        || (b[4] == 0xFF && (b[5] & 0xF8 == 0x50 || b[5] == 0x14)) // [reg+d8], [base+index]
        || (b[5] == 0xFF && (b[6] & 0xF8 == 0xD0 || b[6] & 0xF8 == 0x10)) // reg, [reg]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_childs_stack() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let key = ProcKey {
            pid,
            create_time: procfs::stat(pid).unwrap().create_time(),
        };
        let report = call_stack(key, pid, &mut Symbolizer::default());
        let _ = child.kill();
        let _ = child.wait();
        let report = report.expect("a child's stack can be read");
        let user: Vec<&str> = report
            .frames
            .iter()
            .filter(|f| f.address != 0)
            .filter_map(|f| f.symbol.as_deref())
            .collect();
        // sleep waits in the C library.
        assert!(user.iter().any(|s| s.contains("libc")), "{user:?}");
        assert!(report.held < std::time::Duration::from_millis(100));
    }

    #[test]
    fn names_and_times_own_threads() {
        let me = std::process::id();
        let threads = by_cpu_time(me).unwrap();
        assert!(threads.iter().any(|(tid, _)| *tid == me));
        let names = names(me, &[me]);
        assert!(!names[&me].is_empty());
    }
}
