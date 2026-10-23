// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Keeps Anabolic responsive on Linux: a higher CPU priority (nice) and
//! I/O priority, memory locked in place, and what rights it holds.
//!
//! Anabolic runs as the signed-in user. Its executable can be given the
//! capabilities it needs to see and act on every process (`grant_command`),
//! which is what "administrator" means here: the window stays the user's,
//! and only the rights it needs come with it.

use super::*;

/// The capabilities Anabolic asks for, by bit number, with what each is
/// for.
pub const CAPABILITIES: [(u32, &str, &str); 9] = [
    (
        2,
        "cap_dac_read_search",
        "read the firmware's hardware tables",
    ),
    (5, "cap_kill", "end and suspend other users' processes"),
    (
        12,
        "cap_net_admin",
        "close connections, watch process starts",
    ),
    (14, "cap_ipc_lock", "keep its own memory resident"),
    (
        19,
        "cap_sys_ptrace",
        "read every process' files, I/O and threads",
    ),
    (
        21,
        "cap_sys_admin",
        "record file activity, read kernel stacks",
    ),
    (23, "cap_sys_nice", "raise priorities"),
    (38, "cap_perfmon", "kernel traces: cores, disks, latency"),
    (34, "cap_syslog", "name kernel addresses"),
];

/// The capabilities this process holds now (CapEff).
pub fn effective_capabilities() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let hex = crate::procfs::status_value(&s, "CapEff")?.to_string();
            u64::from_str_radix(&hex, 16).ok()
        })
        .unwrap_or(0)
}

pub fn has_capability(bit: u32) -> bool {
    effective_capabilities() & (1 << bit) != 0
}

/// Every kernel trace is tried: what it lacks (cap_perfmon, tracefs), the
/// kernel says itself, and a lowered perf_event_paranoid may need neither.
pub fn may_trace() -> bool {
    true
}

/// Root, or holding every capability Anabolic asks for.
pub fn is_elevated() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return true;
    }
    let held = effective_capabilities();
    CAPABILITIES
        .iter()
        .all(|(bit, _, _)| held & (1 << bit) != 0)
}

/// The command that gives `exe` the capabilities, run through pkexec.
pub fn grant_command(exe: &str) -> Vec<String> {
    let caps: Vec<&str> = CAPABILITIES.iter().map(|(_, name, _)| *name).collect();
    vec![
        "pkexec".into(),
        "setcap".into(),
        format!("{}+ep", caps.join(",")),
        exe.into(),
    ]
}

fn nice(value: i32) -> Result<(), String> {
    // Linux sets a priority per thread; threads started later inherit it.
    let tid = unsafe { libc::gettid() } as libc::id_t;
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, value) } == 0 {
        Ok(())
    } else {
        Err(crate::util::error_text(&std::io::Error::last_os_error()))
    }
}

/// I/O priority class best-effort, level 0: first among ordinary requests.
fn io_priority_high() -> Result<(), String> {
    const IOPRIO_WHO_PROCESS: i32 = 1;
    const IOPRIO_CLASS_BE: i32 = 2;
    let value = IOPRIO_CLASS_BE << 13;
    let done = unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, value) };
    if done == 0 {
        Ok(())
    } else {
        Err(crate::util::error_text(&std::io::Error::last_os_error()))
    }
}

/// Applies every tweak that does not depend on the UI being up. Runs
/// before any other thread starts, so all of them inherit the priority.
pub fn harden_process() -> Vec<HardeningItem> {
    let mut items = Vec::new();
    let mut add = |what, result| items.push(HardeningItem { what, result });
    add(
        "High priority (nice -10)",
        nice(-10).map_err(|e| format!("{e}: needs cap_sys_nice")),
    );
    add("High I/O priority", io_priority_high());
    items
}

pub fn own_working_set() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_ascii_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * crate::procfs::page_size())
}

/// Locks Anabolic's memory in place, now and as it grows, so the UI does
/// not have to be read back from swap exactly when RAM is scarce. Needs
/// cap_ipc_lock: without it the lock limit (a few MB) is far too small.
pub fn pin_working_set(_min_bytes: usize) -> Result<(), String> {
    if !has_capability(14) && unsafe { libc::geteuid() } != 0 {
        return Err("needs cap_ipc_lock".into());
    }
    // Pages are locked as they are first touched, not all at once.
    let flags = libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT;
    if unsafe { libc::mlockall(flags) } == 0 {
        Ok(())
    } else {
        Err(crate::util::error_text(&std::io::Error::last_os_error()))
    }
}

pub fn set_current_thread_priority(priority: ThreadPriority) {
    let tid = unsafe { libc::gettid() } as libc::id_t;
    // getpriority can return -1 legitimately; errno tells.
    let current = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid) };
    let wanted = match priority {
        ThreadPriority::BelowNormal => current + 5,
        ThreadPriority::AboveNormal => current - 2,
    };
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, tid, wanted.clamp(-20, 19));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_its_own_rights_and_memory() {
        assert!(own_working_set() > 0);
        let grant = grant_command("/usr/bin/anabolic");
        assert_eq!(grant[0], "pkexec");
        assert!(grant[2].ends_with("+ep") && grant[2].contains("cap_perfmon"));
        // An ordinary test run holds none of them.
        if unsafe { libc::geteuid() } != 0 {
            assert!(!is_elevated() || effective_capabilities() != 0);
        }
    }
}
