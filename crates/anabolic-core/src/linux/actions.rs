// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Process control on Linux: signals through pidfds (never to a recycled
//! PID), priorities and affinity set on every thread (Linux keeps them per
//! thread), and the desktop's file manager and browser for the rest.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::hardening::has_capability;
use crate::util::{error_text, open_process};

use super::*;

/// Nothing to set up on the action thread.
pub(super) fn thread_init() {}

impl Priority {
    /// The nice value a class stands for; None for Realtime, which is a
    /// scheduling policy instead.
    fn nice(self) -> Option<i32> {
        match self {
            Priority::Idle => Some(19),
            Priority::BelowNormal => Some(10),
            Priority::Normal => Some(0),
            Priority::AboveNormal => Some(-5),
            Priority::High => Some(-10),
            Priority::Realtime => None,
        }
    }
}

/// Carries out an action on the calling thread (the action thread, or the
/// MCP server's): the outcome, or None for one that has nothing to report.
pub fn perform(action: Action) -> Option<Outcome> {
    let result = match action {
        Action::Quiet(action) => {
            return perform(*action).filter(|o| matches!(o, Outcome::Error(_)));
        }
        Action::Silent(action) => {
            perform(*action);
            return None;
        }
        Action::SignIn(on) => return Some(crate::signin::perform(on)),
        Action::Firewall { path, block } => {
            let file = std::path::Path::new(&path)
                .file_name()
                .map_or(path.clone(), |f| f.to_string_lossy().into_owned());
            let what = if block { "block" } else { "allow" };
            return Some(Outcome::Error(format!(
                "Could not {what} {file}: Linux firewalls filter by address, port and user, not by \
                 program"
            )));
        }
        Action::QueryFirewall(_) => return Some(Outcome::StillBlocked(Vec::new())),
        Action::CloseConnection {
            pid,
            process,
            remote,
            remote_port,
            local_port,
        } => crate::sockets::close_tcp(pid, remote, remote_port, local_port)
            .map(|_| format!("Closed {process}'s connection to {remote}:{remote_port}"))
            .map_err(|e| {
                format!("Could not close {process}'s connection to {remote}:{remote_port}: {e}")
            }),
        Action::GamePower { on: true, .. } => {
            return Some(match crate::power::for_game() {
                Ok(previous) => Outcome::PowerForGame(previous),
                Err(e) => {
                    Outcome::Error(format!("Game mode could not switch the power profile: {e}"))
                }
            });
        }
        Action::GamePower {
            on: false,
            previous,
        } => {
            let previous = previous?;
            return Some(match crate::power::restore(&previous) {
                Ok(name) => Outcome::PowerRestored(name),
                Err(e) => Outcome::Error(format!("Could not put the power profile back: {e}")),
            });
        }
        Action::Copy(text) => arboard::Clipboard::new()
            .and_then(|mut c| c.set_text(text))
            .map(|_| String::new())
            .map_err(|e| format!("Could not copy: {e}")),
        Action::RelaunchElevated => {
            return Some(match relaunch_elevated() {
                Ok(()) => Outcome::Relaunched,
                Err(e) => Outcome::Error(e),
            });
        }
        Action::Terminate(targets) => return Some(terminate(&targets)),
        // A program that needs root asks for it itself (pkexec, sudo in a
        // terminal): one started as root here would find no desktop.
        Action::Run { command, .. } => run(&command),
        Action::BrowseProgram => return Some(Outcome::Browsed(None)),
        // Run on their own thread by the runner.
        Action::Dump { .. } => return None,
        Action::Suspend(t) => signal(&t, libc::SIGSTOP).map(|_| format!("Suspended {}", t.label())),
        Action::Resume(t) => signal(&t, libc::SIGCONT).map(|_| format!("Resumed {}", t.label())),
        Action::SetPriority(t, priority) => set_priority(&t, priority)
            .map(|_| format!("{} priority set to {}", t.label(), priority.label()))
            .map_err(|e| format!("Could not change priority of {}: {e}", t.label())),
        Action::SetEfficiencyMode(t, on) => set_efficiency_mode(&t, on),
        Action::QueryAffinity(t) => {
            return Some(match affinity(t.key.pid) {
                Ok(process_mask) => Outcome::Affinity {
                    target: t,
                    process_mask,
                    system_mask: system_mask(),
                },
                Err(e) => Outcome::Error(format!("Could not read affinity of {}: {e}", t.label())),
            });
        }
        Action::SetAffinity(t, mask) => set_affinity(&t, mask)
            .map(|_| format!("{} affinity set to {} CPU(s)", t.label(), mask.count_ones()))
            .map_err(|e| format!("Could not set affinity of {}: {e}", t.label())),
        Action::OpenFileLocation(path) => {
            if file_manager("ShowItems", &path).is_err() {
                let dir = std::path::Path::new(&path)
                    .parent()
                    .map_or(path.clone(), |p| p.to_string_lossy().into_owned());
                let _ = open(&dir);
            }
            return None;
        }
        Action::FileProperties(path) => file_manager("ShowItemProperties", &path)
            .map(|_| String::new())
            .map_err(|e| format!("Could not show properties of {path}: {e}")),
        Action::Thread { pid, tid, op } => crate::threads::apply(pid, tid, op),
        Action::CloseHandles { process, pid, .. } | Action::CloseHandle { process, pid, .. } => {
            Err(format!(
                "Could not close files in {process} ({pid}): Linux does not let one program \
                 close another's"
            ))
        }
        Action::OpenUrl(url) => {
            let _ = open(&url);
            return None;
        }
        Action::SearchOnline(query) => {
            let _ = open(&format!(
                "https://www.google.com/search?q={}",
                url_encode(&query)
            ));
            return None;
        }
    };
    Some(match result {
        Ok(msg) if msg.is_empty() => return None,
        Ok(msg) => Outcome::Info(msg),
        Err(msg) => Outcome::Error(msg),
    })
}

fn signal(t: &Target, signal: i32) -> Result<(), String> {
    let process = open_process(t.key).map_err(|e| format!("{}: {}", t.label(), error_text(&e)))?;
    process
        .send(signal)
        .map_err(|e| format!("{}: {}", t.label(), denied(&e, "cap_kill")))
}

/// "Operation not permitted", with the capability that would allow it.
fn denied(e: &std::io::Error, capability: &str) -> String {
    match e.raw_os_error() {
        Some(libc::EPERM) | Some(libc::EACCES) if !has_capability(5) => {
            format!(
                "{}: another user's process needs {capability}",
                error_text(e)
            )
        }
        _ => error_text(e),
    }
}

/// How long a program gets to end by itself before it is killed.
const GRACE: Duration = Duration::from_secs(2);

/// Asks every target to end (SIGTERM), then kills those still there after
/// a moment (SIGKILL): a program that answers gets to save and clean up,
/// one that does not is ended anyway.
fn terminate(targets: &[Target]) -> Outcome {
    let mut failures = Vec::new();
    let mut refused = Vec::new();
    let mut asked = Vec::new();
    for t in targets {
        match open_process(t.key).and_then(|p| p.send(libc::SIGTERM).map(|_| p)) {
            Ok(process) => asked.push((t, process)),
            Err(e) => {
                failures.push(format!("{}: {}", t.label(), denied(&e, "cap_kill")));
                refused.push(t.key);
            }
        }
    }
    let deadline = Instant::now() + GRACE;
    let mut done = 0;
    for (t, process) in &asked {
        let left = deadline.saturating_duration_since(Instant::now());
        if process.wait_ended(left.as_millis() as i32) {
            done += 1;
            continue;
        }
        match process.send(libc::SIGKILL) {
            Ok(()) if process.wait_ended(1000) => done += 1,
            Ok(()) => {
                failures.push(format!(
                    "{}: it is stuck inside the kernel and cannot end until that wait finishes",
                    t.label()
                ));
                refused.push(t.key);
            }
            Err(e) => {
                failures.push(format!("{}: {}", t.label(), error_text(&e)));
                refused.push(t.key);
            }
        }
    }
    match (done, failures.is_empty()) {
        (_, false) => Outcome::Refused {
            keys: refused,
            message: format!(
                "Could not terminate {}{}. The details say why.",
                failures.join("; "),
                if done > 0 {
                    format!(" (ended {done} others)")
                } else {
                    String::new()
                }
            ),
        },
        (1, true) => Outcome::Info(format!("Terminated {}", targets[0].label())),
        (n, true) => Outcome::Info(format!("Terminated {n} processes")),
    }
}

/// Every thread of the process: Linux sets priorities and affinity per
/// thread, and a process' are its threads'.
fn threads(t: &Target) -> Result<Vec<u32>, String> {
    // The pidfd proves the PID is still this process while we look.
    let _process = open_process(t.key).map_err(|e| error_text(&e))?;
    let tids = crate::procfs::tids(t.key.pid);
    if tids.is_empty() {
        return Err("it has exited".into());
    }
    Ok(tids)
}

fn set_policy(tid: u32, policy: i32, priority: i32) -> std::io::Result<()> {
    let param = libc::sched_param {
        sched_priority: priority,
    };
    if unsafe { libc::sched_setscheduler(tid as libc::pid_t, policy, &param) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn set_priority(t: &Target, priority: Priority) -> Result<(), String> {
    for tid in threads(t)? {
        let done = match priority.nice() {
            None => set_policy(tid, libc::SCHED_RR, 1),
            Some(nice) => set_policy(tid, libc::SCHED_OTHER, 0).and_then(|_| {
                if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) } == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }),
        };
        if let Err(e) = done {
            // A thread that ended meanwhile is no failure.
            if e.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(match e.raw_os_error() {
                Some(libc::EPERM) | Some(libc::EACCES) => {
                    format!("{}: raising a priority needs cap_sys_nice", error_text(&e))
                }
                _ => error_text(&e),
            });
        }
    }
    Ok(())
}

/// Efficiency mode on Linux: the idle scheduling class (it runs when no
/// one else wants the processor) and the idle I/O class.
fn set_efficiency_mode(t: &Target, on: bool) -> Result<String, String> {
    const IOPRIO_WHO_PROCESS: i32 = 1;
    const IOPRIO_CLASS_IDLE: i32 = 3;
    let fail = |e: std::io::Error| format!("{}: {}", t.label(), denied(&e, "cap_sys_nice"));
    for tid in threads(t).map_err(|e| format!("{}: {e}", t.label()))? {
        let done = if on {
            set_policy(tid, libc::SCHED_IDLE, 0)
        } else {
            set_policy(tid, libc::SCHED_OTHER, 0)
        };
        match done {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => continue,
            Err(e) => return Err(fail(e)),
        }
        // Class 0: the I/O priority follows the CPU priority again.
        let value = if on { IOPRIO_CLASS_IDLE << 13 } else { 0 };
        unsafe {
            libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid, value);
        }
    }
    Ok(format!(
        "Efficiency mode {} for {}",
        if on { "on" } else { "off" },
        t.label()
    ))
}

fn affinity(pid: u32) -> Result<u64, String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::sched_getaffinity(pid as libc::pid_t, size_of::<libc::cpu_set_t>(), &mut set)
    } != 0
    {
        return Err(error_text(&std::io::Error::last_os_error()));
    }
    Ok((0..64)
        .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) })
        .fold(0u64, |mask, cpu| mask | 1 << cpu))
}

/// The processors online, as a mask.
fn system_mask() -> u64 {
    let online = std::fs::read_to_string("/sys/devices/system/cpu/online").unwrap_or_default();
    crate::topology::parse_list(online.trim())
        .into_iter()
        .filter(|&cpu| cpu < 64)
        .fold(0u64, |mask, cpu| mask | 1 << cpu)
}

fn set_affinity(t: &Target, mask: u64) -> Result<(), String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for cpu in (0..64).filter(|cpu| mask & (1 << cpu) != 0) {
        unsafe { libc::CPU_SET(cpu, &mut set) };
    }
    for tid in threads(t)? {
        let done = unsafe {
            libc::sched_setaffinity(tid as libc::pid_t, size_of::<libc::cpu_set_t>(), &set)
        };
        if done != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ESRCH) {
                return Err(denied(&e, "cap_sys_nice"));
            }
        }
    }
    Ok(())
}

/// A file:// address for a path.
fn file_url(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Asks the desktop's file manager (Dolphin, Files…) to show a file.
fn file_manager(method: &str, path: &str) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    conn.call_method(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        Some("org.freedesktop.FileManager1"),
        method,
        &(vec![file_url(path)], ""),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Opens a file, folder or web address with the desktop's choice of program.
fn open(target: &str) -> Result<String, String> {
    Command::new("xdg-open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| String::new())
        .map_err(|e| format!("Could not open {target}: {e}"))
}

/// Starts what the line names: a web address, a document or a folder
/// with the desktop's choice of program, anything else as the shell runs
/// it. In a process group of its own, so it outlives Anabolic; waited for
/// on a thread, so it leaves no zombie behind.
fn run(line: &str) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    let line = line.trim();
    if line.is_empty() {
        return Err("Nothing to run".into());
    }
    let path = std::path::Path::new(line);
    let document = path.is_dir()
        || std::fs::metadata(path)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 == 0);
    let address = line.contains("://") || line.starts_with("mailto:");
    let mut command = if document || address {
        let mut c = Command::new("xdg-open");
        c.arg(line);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(line);
        c
    };
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let shown = line.split_whitespace().next().unwrap_or(line).to_string();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Could not start {shown}: {e}"))?;
    // The shell says at once when there is no such program.
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        match child.try_wait() {
            Ok(Some(status)) if status.code() == Some(127) => {
                return Err(format!("Could not start {shown}: there is no such program"));
            }
            Ok(Some(status)) if status.code() == Some(126) => {
                return Err(format!("Could not start {shown}: it cannot be run"));
            }
            Ok(Some(_)) => return Ok(format!("Started {shown}")),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = std::thread::Builder::new()
        .name("anabolic-run".into())
        .spawn(move || child.wait());
    Ok(format!("Started {shown}"))
}

/// Gives Anabolic's executable the capabilities it asks for (pkexec asks
/// for the password), then starts it again: the new instance takes over
/// once this one exits (`--after`).
fn relaunch_elevated() -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&exe).map_err(|e| e.to_string())?;
    // The rights travel with the file: only its owner may run it, or any
    // user of this computer would have them too.
    if meta.uid() != unsafe { libc::getuid() } {
        return Err(format!(
            "{} belongs to another user; Anabolic gives rights only to a copy of its own",
            exe.display()
        ));
    }
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("Could not make {} yours alone: {e}", exe.display()))?;
    let command = crate::hardening::grant_command(&exe.to_string_lossy());
    let status = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("pkexec could not start: {e}"))?;
    match status.code() {
        Some(0) => {}
        // pkexec: the dialog was dismissed, or the password refused.
        Some(126) | Some(127) => return Err("granting the rights was cancelled".into()),
        _ => return Err(format!("setcap failed ({status})")),
    }
    Command::new(&exe)
        .arg("--after")
        .arg(std::process::id().to_string())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Anabolic could not start again: {e}"))
}
