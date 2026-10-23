// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Keeps Anabolic itself responsive when the machine is drowning: high CPU
//! priority, no EcoQoS throttling, high I/O priority and a working set the
//! memory manager is not allowed to trim.

use core::ffi::c_void;
use core::mem::size_of;

use windows::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation,
};
use windows::Win32::System::Memory::{
    QUOTA_LIMITS_HARDWS_MAX_DISABLE, QUOTA_LIMITS_HARDWS_MIN_ENABLE, SetProcessWorkingSetSizeEx,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS, MEMORY_PRIORITY_INFORMATION,
    MEMORY_PRIORITY_NORMAL, OpenProcessToken, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    PROCESS_POWER_THROTTLING_STATE, ProcessMemoryPriority, ProcessPowerThrottling,
    SetPriorityClass, SetProcessInformation, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    THREAD_PRIORITY_BELOW_NORMAL,
};
use windows::core::{PCWSTR, w};

use crate::ntapi;
use crate::util::{OwnedHandle, error_text};

use super::*;

/// Kernel traces start only elevated: an unelevated try gets a vaguer
/// answer than "needs administrator rights".
pub fn may_trace() -> bool {
    is_elevated()
}

pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0;
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    }
    .is_ok()
        && elevation.TokenIsElevated != 0
}

/// Enables a privilege the token already holds. Fails when it does not.
pub fn enable_privilege(name: PCWSTR) -> Result<(), String> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .map_err(|e| error_text(&e))?;
    let token = OwnedHandle(token);
    let mut luid = LUID::default();
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid) }
        .map_err(|e| error_text(&e))?;
    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        ..Default::default()
    };
    privileges.Privileges[0].Luid = luid;
    privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
    unsafe { AdjustTokenPrivileges(token.raw(), false, Some(&privileges), 0, None, None) }
        .map_err(|e| error_text(&e))?;
    if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
        return Err("not held by this token".into());
    }
    Ok(())
}

/// Applies every tweak that does not depend on the UI being up.
pub fn harden_process() -> Vec<HardeningItem> {
    let mut items = Vec::new();
    let mut add = |what, result| items.push(HardeningItem { what, result });

    add("SeDebugPrivilege", enable_privilege(w!("SeDebugPrivilege")));
    add(
        "SeIncreaseBasePriorityPrivilege",
        enable_privilege(w!("SeIncreaseBasePriorityPrivilege")),
    );
    add(
        "SeIncreaseWorkingSetPrivilege",
        enable_privilege(w!("SeIncreaseWorkingSetPrivilege")),
    );
    add(
        "SeProfileSingleProcessPrivilege",
        enable_privilege(w!("SeProfileSingleProcessPrivilege")),
    );
    // Kernel trace sessions (context switches for the per-core view).
    add(
        "SeSystemProfilePrivilege",
        enable_privilege(w!("SeSystemProfilePrivilege")),
    );

    let process = unsafe { GetCurrentProcess() };
    add(
        "High priority class",
        unsafe { SetPriorityClass(process, HIGH_PRIORITY_CLASS) }.map_err(|e| error_text(&e)),
    );

    // Opt out of EcoQoS: never get parked on efficiency cores or clocked down.
    let throttling = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    add(
        "Power throttling off",
        unsafe {
            SetProcessInformation(
                process,
                ProcessPowerThrottling,
                (&raw const throttling).cast::<c_void>(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        }
        .map_err(|e| error_text(&e)),
    );

    let memory_priority = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: MEMORY_PRIORITY_NORMAL,
    };
    add(
        "Normal memory priority",
        unsafe {
            SetProcessInformation(
                process,
                ProcessMemoryPriority,
                (&raw const memory_priority).cast::<c_void>(),
                size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            )
        }
        .map_err(|e| error_text(&e)),
    );

    let io_priority = ntapi::IoPriorityHigh;
    let status = unsafe {
        ntapi::NtSetInformationProcess(
            process,
            ntapi::ProcessIoPriority,
            (&raw const io_priority).cast(),
            4,
        )
    };
    add(
        "High I/O priority",
        if ntapi::nt_success(status) {
            Ok(())
        } else {
            Err(ntapi::status_message(status))
        },
    );

    items
}

pub fn own_working_set() -> u64 {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if ok.as_bool() {
        counters.WorkingSetSize as u64
    } else {
        0
    }
}

/// Forbids the memory manager from trimming us below `min_bytes`, so the UI
/// does not have to page itself back in exactly when RAM is scarce.
pub fn pin_working_set(min_bytes: usize) -> Result<(), String> {
    let max = min_bytes.saturating_mul(4).max(512 * 1024 * 1024);
    unsafe {
        SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            min_bytes,
            max,
            QUOTA_LIMITS_HARDWS_MIN_ENABLE | QUOTA_LIMITS_HARDWS_MAX_DISABLE,
        )
    }
    .map_err(|e| error_text(&e))
}

pub fn set_current_thread_priority(priority: ThreadPriority) {
    let priority = match priority {
        ThreadPriority::BelowNormal => THREAD_PRIORITY_BELOW_NORMAL,
        ThreadPriority::AboveNormal => THREAD_PRIORITY_ABOVE_NORMAL,
    };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), priority);
    }
}
