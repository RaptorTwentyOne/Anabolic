// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Programs that crashed, hung or were thought to leak, drivers that had to
//! be reset, and stop errors: what the system itself recorded (Windows
//! Error Reporting and the event logs, systemd-coredump on Linux), with the
//! module a fault was in and the exception in words. Nothing is kept by us.

use std::time::Duration;

#[cfg_attr(windows, path = "windows/crashes.rs")]
#[cfg_attr(target_os = "linux", path = "linux/crashes.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Debug, Default)]
pub struct Crash {
    /// When, as a FILETIME.
    pub at: i64,
    pub kind: CrashKind,
    /// The program's file name (chrome.exe), or a Store app's name.
    pub program: String,
    /// Its full path, when known.
    pub path: Option<String>,
    pub version: Option<String>,
    /// A Store app's package (5319275A.WhatsAppDesktop_2.2642.101.0_x64_…).
    pub package: Option<String>,
    /// The module the fault was in: its file name, path and version.
    pub module: Option<String>,
    pub module_path: Option<String>,
    pub module_version: Option<String>,
    /// The exception (Windows) or signal (Linux) it stopped on.
    pub code: Option<u32>,
    /// Where in the module, from its start.
    pub offset: Option<u64>,
    pub pid: Option<u32>,
    /// How long the process had run, in ms.
    pub ran_ms: Option<u64>,
    /// More words: a .NET exception with its stack, the kind of a hang, a
    /// stop error's parameters, the driver that was reset.
    pub detail: Option<String>,
    /// A dump of the process (or of the system) that was kept.
    pub dump: Option<String>,
    /// The folder of the system's report about it.
    pub report: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CrashKind {
    /// It stopped on a fault: an exception nothing handled, a failed check.
    #[default]
    Crash,
    /// It stopped responding and was closed.
    Hang,
    /// It stopped responding for a while, then went on.
    Stall,
    /// Windows thought it was leaking memory (its resource exhaustion
    /// detector: a process whose memory keeps growing).
    Leak,
    /// A driver, mostly the display driver, stopped responding and was
    /// reset, or a device's driver failed and Windows recovered.
    DriverReset,
    /// The whole system stopped (a blue screen).
    StopError,
}

impl CrashKind {
    pub fn label(self) -> &'static str {
        match self {
            CrashKind::Crash => "Crashed",
            CrashKind::Hang => "Hung, closed",
            CrashKind::Stall => "Stopped responding a while",
            CrashKind::Leak => "Thought to leak",
            CrashKind::DriverReset => "Driver reset",
            CrashKind::StopError => "Stop error",
        }
    }

    /// Bad enough to show first: the program or the system went away.
    pub fn serious(self) -> bool {
        matches!(
            self,
            CrashKind::Crash | CrashKind::Hang | CrashKind::StopError | CrashKind::DriverReset
        )
    }
}

#[derive(Debug, Default)]
pub struct Crashes {
    /// Newest first.
    pub crashes: Vec<Crash>,
    /// Why there may be fewer than there were: a log that was not readable,
    /// a source that is missing.
    pub notes: Vec<String>,
    /// The oldest moment the logs reach back to, as a FILETIME.
    pub since: Option<i64>,
    pub took: Duration,
}

/// What a Windows exception code means, in a few words; for a crash's
/// readers, not for the program's developers.
pub fn exception_words(code: u32) -> Option<&'static str> {
    Some(match code {
        0xC000_0005 => "access violation: it used memory that was not its own",
        0xC000_0409 => {
            "it stopped itself (a failed security check, a corrupt stack or a deliberate abort)"
        }
        0xC000_0374 => "its heap was corrupt",
        0xC000_00FD => "stack overflow: it called itself too deep",
        0xC000_0008 => "it used a handle that was not valid",
        0xC000_001D => "an instruction this processor does not have",
        0xC000_0094 => "an integer division by zero",
        0xC000_0096 => "a privileged instruction",
        0xC000_0135 => "a DLL it needs was not found",
        0xC000_0142 => "a DLL it needs failed to start",
        0xC000_0417 => "it passed a C runtime function something it does not take",
        0xC000_041D => "an exception in a callback from the system",
        0xC000_0420 => "an assertion failed",
        0xC000_0602 => "it stopped itself at once (fail fast)",
        0xC06D_007E => "a module it loads late was not found",
        0xC06D_007F => "a function it loads late was not found",
        0xE043_4352 => ".NET: an exception nothing caught",
        0xE06D_7363 => "C++: an exception nothing caught",
        0x8013_1623 => ".NET: it ended itself (Environment.FailFast)",
        0x8000_0003 => "a breakpoint: a debug break left in, often a failed assertion",
        0x4000_0015 => "it aborted",
        0xC000_0006 => "a page of its program could not be read in (a disk or network problem)",
        0xC000_009A => "the system ran out of resources",
        0xC000_0017 => "not enough memory",
        0xC000_012D => "the commit limit was reached: memory and the page file are full",
        _ => return None,
    })
}

/// A stop error's (blue screen's) name, for the common ones.
pub fn stop_words(code: u32) -> Option<&'static str> {
    Some(match code {
        0x0A => "IRQL_NOT_LESS_OR_EQUAL",
        0x19 => "BAD_POOL_HEADER",
        0x1A => "MEMORY_MANAGEMENT",
        0x1E => "KMODE_EXCEPTION_NOT_HANDLED",
        0x3B => "SYSTEM_SERVICE_EXCEPTION",
        0x50 => "PAGE_FAULT_IN_NONPAGED_AREA",
        0x77 => "KERNEL_STACK_INPAGE_ERROR",
        0x7A => "KERNEL_DATA_INPAGE_ERROR",
        0x7E => "SYSTEM_THREAD_EXCEPTION_NOT_HANDLED",
        0x7F => "UNEXPECTED_KERNEL_MODE_TRAP",
        0x9F => "DRIVER_POWER_STATE_FAILURE",
        0xA0 => "INTERNAL_POWER_ERROR",
        0xC2 => "BAD_POOL_CALLER",
        0xD1 => "DRIVER_IRQL_NOT_LESS_OR_EQUAL",
        0xEF => "CRITICAL_PROCESS_DIED",
        0xFC => "ATTEMPTED_EXECUTE_OF_NOEXECUTE_MEMORY",
        0x101 => "CLOCK_WATCHDOG_TIMEOUT",
        0x109 => "CRITICAL_STRUCTURE_CORRUPTION",
        0x116 => "VIDEO_TDR_FAILURE",
        0x119 => "VIDEO_SCHEDULER_INTERNAL_ERROR",
        0x124 => "WHEA_UNCORRECTABLE_ERROR (a hardware error)",
        0x133 => "DPC_WATCHDOG_VIOLATION",
        0x139 => "KERNEL_SECURITY_CHECK_FAILURE",
        0x154 => "UNEXPECTED_STORE_EXCEPTION",
        _ => return None,
    })
}

/// What a live kernel event (a problem Windows recovered from, keeping a
/// dump of the kernel as it went on) was about.
pub fn live_event_words(code: u32) -> Option<&'static str> {
    Some(match code {
        0x117 => "the display driver stopped responding and was reset",
        0x141 => "a graphics engine stopped responding and was reset",
        0x193 => "the graphics kernel took a dump of itself and went on",
        0x1A1 => "a call into the desktop's kernel part hung, and Windows went on",
        0x15F => "a device kept the PC from sleeping (connected standby watchdog)",
        0x144 => "the USB 3 driver found a problem",
        0x156 => "a network socket hung as it was closed",
        0x161 => "a live dump someone asked for (Task Manager, Anabolic)",
        0x1CC => "a kernel lock was held too long",
        _ => return None,
    })
}

/// A Linux signal's name.
pub fn signal_name(signal: u32) -> Option<&'static str> {
    Some(match signal {
        4 => "SIGILL: an illegal instruction",
        5 => "SIGTRAP: a breakpoint",
        6 => "SIGABRT: it aborted",
        7 => "SIGBUS: a bad memory access",
        8 => "SIGFPE: an arithmetic error",
        11 => "SIGSEGV: it used memory that was not its own",
        24 => "SIGXCPU: over its CPU time limit",
        25 => "SIGXFSZ: over its file size limit",
        31 => "SIGSYS: a system call it may not make",
        _ => return None,
    })
}

/// A crash's cause in words: the exception or signal, or its code.
pub fn cause(c: &Crash) -> Option<String> {
    let code = c.code?;
    if cfg!(windows) {
        Some(match exception_words(code) {
            Some(words) => format!("{words} (0x{code:08X})"),
            None => format!("exception 0x{code:08X}"),
        })
    } else {
        Some(
            signal_name(code)
                .map(str::to_string)
                .unwrap_or_else(|| format!("signal {code}")),
        )
    }
}

/// A program's crashes together: how many of each kind, the latest, and
/// the module most of them were in.
#[derive(Clone, Debug, Default)]
pub struct Group {
    pub program: String,
    pub path: Option<String>,
    /// Indexes into the crashes, newest first.
    pub items: Vec<usize>,
    pub latest: i64,
    pub serious: usize,
    /// The module named most often, and how many times.
    pub module: Option<(String, usize)>,
}

/// The crashes by program, the latest first. A driver reset or stop error
/// is its own group, under its driver or the system.
pub fn groups(crashes: &[Crash]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (i, c) in crashes.iter().enumerate() {
        let key = c.program.to_ascii_lowercase();
        let g = match groups
            .iter_mut()
            .position(|g| g.program.to_ascii_lowercase() == key)
        {
            Some(at) => &mut groups[at],
            None => {
                groups.push(Group {
                    program: c.program.clone(),
                    ..Group::default()
                });
                groups.last_mut().unwrap()
            }
        };
        if g.path.is_none() {
            g.path.clone_from(&c.path);
        }
        g.items.push(i);
        g.latest = g.latest.max(c.at);
        g.serious += usize::from(c.kind.serious());
    }
    for g in &mut groups {
        g.items.sort_by_key(|&i| std::cmp::Reverse(crashes[i].at));
        let mut counts: Vec<(String, usize)> = Vec::new();
        for &i in &g.items {
            let Some(m) = crashes[i].module.as_deref() else {
                continue;
            };
            match counts.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(m)) {
                Some((_, n)) => *n += 1,
                None => counts.push((m.to_string(), 1)),
            }
        }
        g.module = counts.into_iter().max_by_key(|(_, n)| *n);
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.latest));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crash(program: &str, at: i64, kind: CrashKind, module: Option<&str>) -> Crash {
        Crash {
            at,
            kind,
            program: program.into(),
            module: module.map(Into::into),
            ..Crash::default()
        }
    }

    #[test]
    fn puts_a_programs_crashes_together() {
        let list = [
            crash("chrome.exe", 30, CrashKind::Crash, Some("ntdll.dll")),
            crash("Code.exe", 50, CrashKind::Hang, None),
            crash("Chrome.exe", 10, CrashKind::Crash, Some("NTDLL.dll")),
            crash("chrome.exe", 20, CrashKind::Leak, Some("chrome.dll")),
        ];
        let g = groups(&list);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].program, "Code.exe");
        let chrome = &g[1];
        assert_eq!(chrome.items, vec![0, 3, 2]);
        assert_eq!(chrome.latest, 30);
        assert_eq!(chrome.serious, 2);
        assert_eq!(chrome.module, Some(("ntdll.dll".to_string(), 2)));
    }

    #[test]
    fn names_exceptions() {
        let c = Crash {
            code: Some(0xC000_0005),
            ..Crash::default()
        };
        if cfg!(windows) {
            assert_eq!(
                cause(&c).unwrap(),
                "access violation: it used memory that was not its own (0xC0000005)"
            );
        }
        assert!(exception_words(0x1234).is_none());
    }
}
