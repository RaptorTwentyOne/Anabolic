// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Limits as job objects, one per group. A job never ends its processes: it
//! is made without "kill on close", and a lifted one only stops limiting.
//! Its limits outlive its last handle while its processes run, and nothing
//! can open it again then (its name, had it one, goes with the handle):
//! they are lifted while Anabolic still holds it, as it quits or panics.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Threading::{PROCESS_SET_QUOTA, PROCESS_TERMINATE};
use windows::core::{BOOL, PCWSTR};

use super::{Applied, Limits};
use crate::actions::Target;
use crate::util::{error_text, open_process};

/// The jobs made this run, by group, as raw handles.
static JOBS: Mutex<Option<HashMap<String, isize>>> = Mutex::new(None);

/// Limits can be set here.
pub fn available() -> Result<(), String> {
    Ok(())
}

/// The group's job, made on first use.
fn job(group: &str) -> windows::core::Result<HANDLE> {
    let mut jobs = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    let jobs = jobs.get_or_insert_with(HashMap::new);
    if let Some(&raw) = jobs.get(group) {
        return Ok(HANDLE(raw as *mut _));
    }
    // Without a name: another Anabolic (a test copy) keeps its own.
    let handle = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
    jobs.insert(group.to_string(), handle.0 as isize);
    Ok(handle)
}

fn set(job: HANDLE, limits: Limits) -> windows::core::Result<()> {
    let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    if let Some(pct) = limits.cpu_pct {
        cpu.ControlFlags =
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        // Hundredths of a percent of all processors' time.
        cpu.Anonymous.CpuRate = ((pct * 100.0).round() as u32).clamp(1, 10_000);
    }
    let mut memory = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    if let Some(bytes) = limits.memory {
        memory.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY;
        memory.JobMemoryLimit = bytes as usize;
    }
    unsafe {
        let capped = SetInformationJobObject(
            job,
            JobObjectCpuRateControlInformation,
            (&raw const cpu).cast(),
            size_of_val(&cpu) as u32,
        );
        // Turning off a cap the job never had is refused, and harmless.
        if limits.cpu_pct.is_some() {
            capped?;
        }
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const memory).cast(),
            size_of_val(&memory) as u32,
        )
    }
}

/// Puts `targets` in the group's job and holds it to `limits`.
pub fn limit(group: &str, targets: &[Target], limits: Limits) -> Applied {
    let mut applied = Applied {
        group: group.to_string(),
        limits,
        before: None,
        joined: Vec::new(),
        failed: Vec::new(),
        error: None,
    };
    let job = match job(group).and_then(|job| set(job, limits).map(|_| job)) {
        Ok(job) => job,
        Err(e) => {
            applied.error = Some(error_text(&e));
            return applied;
        }
    };
    for t in targets {
        let joined = open_process(t.key, PROCESS_SET_QUOTA | PROCESS_TERMINATE).and_then(|h| {
            let mut inside = BOOL(0);
            unsafe {
                if IsProcessInJob(h.raw(), Some(job), &mut inside).is_ok() && inside.as_bool() {
                    return Ok(());
                }
                AssignProcessToJobObject(job, h.raw())
            }
        });
        match joined {
            Ok(()) => applied.joined.push(t.key),
            Err(e) => applied.failed.push((t.clone(), error_text(&e))),
        }
    }
    applied
}

/// Stops limiting the group. Its processes stay in the job, which holds
/// them to nothing any more.
pub fn lift(group: &str) -> Applied {
    let mut applied = Applied {
        group: group.to_string(),
        limits: Limits::default(),
        before: None,
        joined: Vec::new(),
        failed: Vec::new(),
        error: None,
    };
    let held = JOBS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|jobs| jobs.remove(group));
    if let Some(raw) = held {
        let job = HANDLE(raw as *mut _);
        if let Err(e) = set(job, Limits::default()) {
            applied.error = Some(error_text(&e));
        }
        unsafe {
            let _ = CloseHandle(job);
        }
    }
    applied
}

/// Lifts every limit set this run: Anabolic is quitting, or failing. Not
/// waited for if this thread holds the jobs (it failed on them).
pub fn lift_all() {
    let groups: Vec<String> = match JOBS.try_lock() {
        Ok(jobs) => jobs.as_ref().map(|j| j.keys().cloned().collect()),
        Err(std::sync::TryLockError::Poisoned(e)) => {
            e.into_inner().as_ref().map(|j| j.keys().cloned().collect())
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
    .unwrap_or_default();
    for group in groups {
        lift(&group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcKey;

    /// A process of our own to limit: waits on its standard input.
    fn child() -> std::process::Child {
        std::process::Command::new("cmd.exe")
            .args(["/c", "more"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cmd starts")
    }

    fn key(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: 0,
        }
    }

    #[test]
    fn limits_a_process_and_lifts_the_limit_without_ending_it() {
        let mut c = child();
        let group = format!("test:{}", c.id());
        let target = Target {
            key: key(c.id()),
            name: "cmd.exe".into(),
        };
        let limits = Limits {
            cpu_pct: Some(5.0),
            memory: Some(512 * 1024 * 1024),
        };
        let applied = limit(&group, std::slice::from_ref(&target), limits);
        assert_eq!(applied.error, None);
        assert!(applied.failed.is_empty(), "{:?}", applied.failed);
        assert_eq!(applied.joined, [target.key]);
        // Asked again: already in, not an error.
        let again = limit(&group, std::slice::from_ref(&target), limits);
        assert_eq!(again.joined, [target.key]);
        let lifted = lift(&group);
        assert_eq!(lifted.error, None);
        assert!(lifted.limits.is_empty());
        // Lifting closed the job: the process lives on.
        assert!(c.try_wait().expect("waits").is_none());
        let _ = c.kill();
        let _ = c.wait();
    }

    /// Processor time a process has used, seconds.
    fn cpu_seconds(pid: u32) -> f64 {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{
            GetProcessTimes, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let h = open_process(key(pid), PROCESS_QUERY_LIMITED_INFORMATION).expect("opens");
        let (mut a, mut b, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(h.raw(), &mut a, &mut b, &mut kernel, &mut user) }.expect("times");
        let t =
            |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
        t(kernel) + t(user)
    }

    #[test]
    fn the_cpu_cap_holds_a_busy_process_back() {
        // A loop that never ends, on one thread.
        let mut c = std::process::Command::new("cmd.exe")
            .args(["/c", "for /l %i in (0,0,1) do @rem"])
            .spawn()
            .expect("cmd starts");
        let group = format!("test:{}", c.id());
        let target = Target {
            key: key(c.id()),
            name: "cmd.exe".into(),
        };
        let limits = Limits {
            cpu_pct: Some(1.0),
            memory: None,
        };
        let applied = limit(&group, std::slice::from_ref(&target), limits);
        assert!(
            applied.error.is_none() && applied.failed.is_empty(),
            "{applied:?}"
        );
        let before = cpu_seconds(c.id());
        std::thread::sleep(std::time::Duration::from_secs(2));
        let used = cpu_seconds(c.id()) - before;
        lift(&group);
        let _ = c.kill();
        let _ = c.wait();
        // Unheld it would take the whole 2 s of its core; 1 % of the
        // machine is at most a third of one core on a 32-thread one.
        assert!(used < 1.0, "used {used:.2} s of 2");
    }

    #[test]
    fn a_process_that_is_gone_is_reported() {
        let target = Target {
            key: ProcKey {
                pid: 0x7FFF_FFF0,
                create_time: 1,
            },
            name: "nothing.exe".into(),
        };
        let applied = limit("test:gone", &[target], Limits::default());
        assert!(applied.joined.is_empty());
        assert_eq!(applied.failed.len(), 1);
        lift("test:gone");
    }
}
