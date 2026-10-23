// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Small static facts and conversions the UI needs.

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetTickCount64};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::core::w;

use crate::util::{filetime_to_i64, from_wide};

/// Marketing name of the first processor, e.g. "AMD Ryzen 9 9950X 16-Core Processor".
pub fn cpu_name() -> Option<String> {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then(|| from_wide(&buf).trim().to_string())
}

/// How long Windows has been running.
pub fn uptime() -> std::time::Duration {
    std::time::Duration::from_millis(unsafe { GetTickCount64() })
}

pub fn now_filetime() -> i64 {
    filetime_to_i64(unsafe { GetSystemTimeAsFileTime() })
}

/// A FILETIME (UTC, 100 ns since 1601) as local "YYYY-MM-DD HH:MM:SS".
pub fn local_time(filetime: i64) -> Option<String> {
    if filetime <= 0 {
        return None;
    }
    let ft = FILETIME {
        dwLowDateTime: filetime as u32,
        dwHighDateTime: (filetime >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    ))
}

pub fn thread_state_name(state: u32) -> &'static str {
    match state {
        0 => "Initialized",
        1 => "Ready",
        2 => "Running",
        3 => "Standby",
        4 => "Terminated",
        5 => "Waiting",
        6 => "Transition",
        7 => "Deferred ready",
        8 => "Gate wait",
        9 => "Waiting (swap)",
        _ => "?",
    }
}

pub fn wait_reason_name(reason: u32) -> &'static str {
    const NAMES: [&str; 43] = [
        "Executive",
        "FreePage",
        "PageIn",
        "PoolAllocation",
        "DelayExecution",
        "Suspended",
        "UserRequest",
        "WrExecutive",
        "WrFreePage",
        "WrPageIn",
        "WrPoolAllocation",
        "WrDelayExecution",
        "WrSuspended",
        "WrUserRequest",
        "WrSpare0",
        "WrQueue",
        "WrLpcReceive",
        "WrLpcReply",
        "WrVirtualMemory",
        "WrPageOut",
        "WrRendezvous",
        "WrKeyedEvent",
        "WrTerminated",
        "WrProcessInSwap",
        "WrCpuRateControl",
        "WrCalloutStack",
        "WrKernel",
        "WrResource",
        "WrPushLock",
        "WrMutex",
        "WrQuantumEnd",
        "WrDispatchInt",
        "WrPreempted",
        "WrYieldExecution",
        "WrFastMutex",
        "WrGuardedMutex",
        "WrRundown",
        "WrAlertByThreadId",
        "WrDeferredPreempt",
        "WrPhysicalFault",
        "WrIoRing",
        "WrMdlCache",
        "WrRcu",
    ];
    NAMES.get(reason as usize).copied().unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_name_and_time() {
        assert!(cpu_name().is_some_and(|n| !n.is_empty()));
        let now = local_time(now_filetime()).unwrap();
        assert_eq!(now.len(), 19, "{now}");
    }
}
