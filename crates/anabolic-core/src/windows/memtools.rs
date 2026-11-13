// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The memory lists through NtSetSystemInformation's
//! SystemMemoryListInformation (as RAMMap empties them), the file cache
//! through SetSystemFileCacheSize, and a live kernel dump through
//! NtSystemDebugControl's SysDbgGetLiveKernelDump (as Task Manager writes
//! one): the kernel's memory as it is, written while the system runs.

use std::path::Path;
use std::time::Instant;

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, GetDiskFreeSpaceExW,
};
use windows::Win32::System::Memory::SetSystemFileCacheSize;
use windows::core::{PCWSTR, w};

use crate::hardening::enable_privilege;
use crate::memtools::{KernelDump, Purge};
use crate::ntapi;
use crate::util::{OwnedHandle, wide};

/// SystemMemoryListInformation, and its commands.
const SYSTEM_MEMORY_LIST_INFORMATION: u32 = 80;
const MEMORY_EMPTY_WORKING_SETS: u32 = 2;
const MEMORY_FLUSH_MODIFIED_LIST: u32 = 3;
const MEMORY_PURGE_STANDBY_LIST: u32 = 4;
const MEMORY_PURGE_LOW_PRIORITY_STANDBY_LIST: u32 = 5;

/// SysDbgGetLiveKernelDump, and LIVE_SYSTEM_DUMP: the stop code a live
/// dump carries, though nothing stopped.
const SYSDBG_GET_LIVE_KERNEL_DUMP: u32 = 37;
const LIVE_SYSTEM_DUMP: u32 = 0x161;

/// Below this free on the dump's drive, no dump is started.
const ROOM_NEEDED: u64 = 8 << 30;

pub fn purge(what: Purge) -> Result<(), String> {
    if what == Purge::FileCache {
        enable_privilege(w!("SeIncreaseQuotaPrivilege"))?;
        // Both sizes at their largest: the cache's working set is emptied.
        return unsafe { SetSystemFileCacheSize(usize::MAX, usize::MAX, 0) }
            .map_err(|e| format!("the file cache could not be emptied: {}", e.message()));
    }
    enable_privilege(w!("SeProfileSingleProcessPrivilege"))?;
    let command: u32 = match what {
        Purge::Standby => MEMORY_PURGE_STANDBY_LIST,
        Purge::StandbyLowPriority => MEMORY_PURGE_LOW_PRIORITY_STANDBY_LIST,
        Purge::Modified => MEMORY_FLUSH_MODIFIED_LIST,
        Purge::WorkingSets => MEMORY_EMPTY_WORKING_SETS,
        Purge::FileCache => unreachable!(),
    };
    let status = unsafe {
        ntapi::NtSetSystemInformation(
            SYSTEM_MEMORY_LIST_INFORMATION,
            (&raw const command).cast(),
            4,
        )
    };
    if ntapi::nt_success(status) {
        Ok(())
    } else {
        Err(ntapi::status_message(status))
    }
}

/// SYSDBG_LIVEDUMP_CONTROL, version 1.
#[repr(C)]
struct LiveDumpControl {
    version: u32,
    bug_check_code: u32,
    bug_check_param: [usize; 4],
    dump_file: HANDLE,
    cancel_event: HANDLE,
    /// UseDumpStorageStack, CompressMemoryPagesData,
    /// IncludeUserSpaceMemoryPages, AbortIfMemoryPressure, SelectiveDump.
    flags: u32,
    /// HypervisorPages, NonEssentialHypervisorPages.
    add_pages: u32,
}

const _: () = assert!(std::mem::size_of::<LiveDumpControl>() == 64);

const COMPRESS: u32 = 1 << 1;
const USER_PAGES: u32 = 1 << 2;
const ABORT_IF_SHORT: u32 = 1 << 3;

/// Writes a dump of the running kernel into `dir`; with `user_pages`, the
/// processes' memory too (far bigger). It stops if memory runs short.
pub fn kernel_dump(dir: &Path, user_pages: bool) -> Result<KernelDump, String> {
    let started = Instant::now();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut free = 0u64;
    let wdir = wide(&dir.to_string_lossy());
    if unsafe { GetDiskFreeSpaceExW(PCWSTR(wdir.as_ptr()), Some(&mut free), None, None) }.is_ok()
        && free < ROOM_NEEDED
    {
        return Err(format!(
            "{} has {} GB free: a kernel dump can take several",
            dir.display(),
            free >> 30
        ));
    }
    enable_privilege(w!("SeDebugPrivilege"))?;
    let path = crate::memtools::kernel_dump_path(dir);
    let wpath = wide(&path.to_string_lossy());
    let file = unsafe {
        CreateFileW(
            PCWSTR(wpath.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| format!("{}: {}", path.display(), e.message()))?;
    let file = OwnedHandle(file);
    let mut control = LiveDumpControl {
        version: 1,
        bug_check_code: LIVE_SYSTEM_DUMP,
        bug_check_param: [0; 4],
        dump_file: file.raw(),
        cancel_event: HANDLE::default(),
        flags: COMPRESS | ABORT_IF_SHORT | if user_pages { USER_PAGES } else { 0 },
        add_pages: 0,
    };
    let mut returned = 0u32;
    let status = unsafe {
        ntapi::NtSystemDebugControl(
            SYSDBG_GET_LIVE_KERNEL_DUMP,
            (&raw mut control).cast(),
            std::mem::size_of::<LiveDumpControl>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
        )
    };
    drop(file);
    if !ntapi::nt_success(status) {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "Windows did not write the dump: {}",
            ntapi::status_message(status)
        ));
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(KernelDump {
        path,
        bytes,
        took: started.elapsed(),
    })
}
