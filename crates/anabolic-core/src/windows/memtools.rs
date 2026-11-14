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

/// Below this free on the dump's drive, no dump is started; more where
/// there is more RAM (a 128 GB machine's kernel dump took 8.4 GB).
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
    let needed = ROOM_NEEDED.max(crate::memory::MemorySampler::new().sample().total / 8);
    if unsafe { GetDiskFreeSpaceExW(PCWSTR(wdir.as_ptr()), Some(&mut free), None, None) }.is_ok()
        && free < needed
    {
        return Err(format!(
            "{} has {} GB free: a kernel dump on this PC can take up to {} GB",
            dir.display(),
            free >> 30,
            needed >> 30
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntapi::SYSTEM_MEMORY_LIST_INFORMATION;

    const MB: usize = 1 << 20;

    fn lists() -> SYSTEM_MEMORY_LIST_INFORMATION {
        enable_privilege(w!("SeProfileSingleProcessPrivilege")).unwrap();
        let mut lists = SYSTEM_MEMORY_LIST_INFORMATION::default();
        let mut returned = 0;
        let status = unsafe {
            ntapi::NtQuerySystemInformation(
                ntapi::SystemMemoryListInformation,
                (&raw mut lists).cast(),
                std::mem::size_of::<SYSTEM_MEMORY_LIST_INFORMATION>() as u32,
                &mut returned,
            )
        };
        assert!(
            ntapi::nt_success(status),
            "{}",
            ntapi::status_message(status)
        );
        lists
    }

    fn show(when: &str, l: &SYSTEM_MEMORY_LIST_INFORMATION) {
        let by: Vec<String> = l
            .PageCountByPriority
            .iter()
            .map(|p| format!("{}", p * 4096 / MB))
            .collect();
        println!(
            "{when:<38} free {:>7} MB  modified {:>6} MB  standby {:>7} MB  by priority 0-7 [{}]",
            (l.FreePageCount + l.ZeroPageCount) * 4096 / MB,
            l.ModifiedPageCount * 4096 / MB,
            l.PageCountByPriority.iter().sum::<usize>() * 4096 / MB,
            by.join(" ")
        );
    }

    /// Live: gives this machine's memory back, gentlest first. Not the
    /// working sets unless ANABOLIC_PURGE_ALL is set: what programs changed
    /// then goes to the page file, 26 GB of writes in 15 seconds once.
    /// Administrator only, and never in a normal run:
    /// `cargo test -p anabolic-core gives_memory_back -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn gives_memory_back() {
        let all = std::env::var_os("ANABOLIC_PURGE_ALL").is_some();
        let mut memory = crate::memory::MemorySampler::new();
        show("before", &lists());
        for what in Purge::ALL {
            if what == Purge::WorkingSets && !all {
                continue;
            }
            let before = memory.sample();
            let t = Instant::now();
            purge(what).unwrap_or_else(|e| panic!("{}: {e}", what.label()));
            let took = t.elapsed();
            show(&format!("{} ({took:.1?})", what.label()), &lists());
            let said = crate::memtools::effect(what, &before, &memory.sample());
            println!("{:<38} says: {}", "", said.unwrap_or_default());
        }
    }

    /// Live: writes a kernel dump of this machine into ANABOLIC_DUMP_DIR
    /// (or the temp folder) and leaves it there. Administrator only:
    /// `cargo test -p anabolic-core writes_a_kernel_dump -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn writes_a_kernel_dump() {
        let dir = std::env::var_os("ANABOLIC_DUMP_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("anabolic-kernel-dump"));
        let dump = kernel_dump(&dir, false).unwrap();
        println!(
            "{} : {} MB in {:.1?}",
            dump.path.display(),
            dump.bytes >> 20,
            dump.took
        );
        assert!(dump.bytes > 16 << 20);
        let mut head = [0u8; 8];
        use std::io::Read;
        std::fs::File::open(&dump.path)
            .unwrap()
            .read_exact(&mut head)
            .unwrap();
        // A 64-bit kernel dump begins PAGEDU64.
        assert_eq!(&head, b"PAGEDU64", "{head:?}");
    }
}
