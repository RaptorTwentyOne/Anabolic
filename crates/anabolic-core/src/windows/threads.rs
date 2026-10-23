// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! One thread of another process: its name, its call stack, and what can
//! be done to it. Every function here opens the thread (and for the stack,
//! the process for reading), so the UI runs them only when asked.

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::System::Diagnostics::Debug::{
    ADDRESS64, AddrModeFlat, CONTEXT, CONTEXT_FULL_AMD64, GetThreadContext, IMAGEHLP_MODULEW64,
    MAX_SYM_NAME, STACKFRAME64, SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS, SYMOPT_FAIL_CRITICAL_ERRORS,
    SYMOPT_NO_PROMPTS, SYMOPT_UNDNAME, StackWalk64, SymCleanup, SymFromAddrW,
    SymFunctionTableAccess64, SymGetModuleBase64, SymGetModuleInfoW64, SymInitializeW,
    SymSetOptions, WOW64_CONTEXT, WOW64_CONTEXT_FULL, Wow64GetThreadContext,
};
use windows::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386,
};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, GetProcessIdOfThread, GetThreadDescription, IsWow64Process, OpenThread,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, ResumeThread, SetThreadPriority, SuspendThread,
    THREAD_ACCESS_RIGHTS, THREAD_GET_CONTEXT, THREAD_PRIORITY, THREAD_QUERY_LIMITED_INFORMATION,
    THREAD_SET_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME, THREAD_TERMINATE, TerminateThread,
};
use windows::core::{BOOL, PCWSTR};

use crate::model::ProcKey;
use crate::modules::{Symbolizer, process_modules};
use crate::util::{OwnedHandle, error_text, open_process};

use super::*;

/// Thread priority levels as SetThreadPriority takes them, with names.
pub const PRIORITIES: [(i32, &str); 7] = [
    (15, "Time critical"),
    (2, "Highest"),
    (1, "Above normal"),
    (0, "Normal"),
    (-1, "Below normal"),
    (-2, "Lowest"),
    (-15, "Idle"),
];

/// Opens `tid` and makes sure it still belongs to `pid`: thread IDs are
/// reused as quickly as process IDs.
fn open_thread(pid: u32, tid: u32, access: THREAD_ACCESS_RIGHTS) -> Result<OwnedHandle, String> {
    let h = unsafe { OpenThread(access | THREAD_QUERY_LIMITED_INFORMATION, false, tid) }
        .map(OwnedHandle)
        .map_err(|e| error_text(&e))?;
    if unsafe { GetProcessIdOfThread(h.raw()) } != pid {
        return Err("the thread has exited".into());
    }
    Ok(h)
}

/// Does `op` to a thread. Returns what happened, for the user.
pub fn apply(pid: u32, tid: u32, op: ThreadOp) -> Result<String, String> {
    if pid == std::process::id() {
        return Err("Anabolic does not act on its own threads".into());
    }
    match op {
        ThreadOp::Suspend => {
            let h = open_thread(pid, tid, THREAD_SUSPEND_RESUME)?;
            match unsafe { SuspendThread(h.raw()) } {
                u32::MAX => Err(windows::core::Error::from_thread().message()),
                0 => Ok(format!("Suspended thread {tid}")),
                n => Ok(format!(
                    "Suspended thread {tid} (it was already suspended {n} times)"
                )),
            }
        }
        ThreadOp::Resume => {
            let h = open_thread(pid, tid, THREAD_SUSPEND_RESUME)?;
            match unsafe { ResumeThread(h.raw()) } {
                u32::MAX => Err(windows::core::Error::from_thread().message()),
                0 => Ok(format!("Thread {tid} was not suspended")),
                1 => Ok(format!("Resumed thread {tid}")),
                n => Ok(format!(
                    "Thread {tid} is still suspended {} more times",
                    n - 1
                )),
            }
        }
        ThreadOp::Terminate => {
            let h = open_thread(pid, tid, THREAD_TERMINATE)?;
            unsafe { TerminateThread(h.raw(), 1) }.map_err(|e| error_text(&e))?;
            Ok(format!("Ended thread {tid}"))
        }
        ThreadOp::Priority(level) => {
            let h = open_thread(pid, tid, THREAD_SET_LIMITED_INFORMATION)?;
            unsafe { SetThreadPriority(h.raw(), THREAD_PRIORITY(level)) }
                .map_err(|e| error_text(&e))?;
            let name = PRIORITIES
                .iter()
                .find(|(l, _)| *l == level)
                .map_or("custom", |(_, n)| n);
            Ok(format!("Thread {tid} priority set to {name}"))
        }
    }
}

/// Names programs gave their threads (SetThreadDescription), where set.
pub fn names(pid: u32, tids: &[u32]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for &tid in tids {
        let Ok(h) = open_thread(pid, tid, THREAD_ACCESS_RIGHTS(0)) else {
            continue;
        };
        let Ok(text) = (unsafe { GetThreadDescription(h.raw()) }) else {
            continue;
        };
        let name = unsafe { text.to_string() }.unwrap_or_default();
        unsafe {
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        }
        if !name.is_empty() {
            out.insert(tid, name);
        }
    }
    out
}

const MAX_FRAMES: usize = 64;

unsafe extern "system" fn table_access(process: HANDLE, base: u64) -> *mut core::ffi::c_void {
    unsafe { SymFunctionTableAccess64(process, base) }
}

unsafe extern "system" fn module_base(process: HANDLE, address: u64) -> u64 {
    unsafe { SymGetModuleBase64(process, address) }
}

/// Ends the symbol session on drop.
struct Symbols(HANDLE);

impl Drop for Symbols {
    fn drop(&mut self) {
        unsafe {
            let _ = SymCleanup(self.0);
        }
    }
}

/// Resumes the thread on drop, so no early return leaves it suspended.
struct Held<'a>(&'a OwnedHandle);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        unsafe {
            ResumeThread(self.0.raw());
        }
    }
}

#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// `module!name+0x..` from dbghelp (exports, or symbols when a symbol
/// path is set); None when it knows nothing.
fn dbghelp_name(process: HANDLE, address: u64) -> Option<String> {
    let mut module = IMAGEHLP_MODULEW64 {
        SizeOfStruct: size_of::<IMAGEHLP_MODULEW64>() as u32,
        ..Default::default()
    };
    unsafe { SymGetModuleInfoW64(process, address, &mut module) }.ok()?;
    // 32-bit modules come with their extension; show both kinds alike.
    let mut module_name = crate::util::from_wide(&module.ModuleName);
    for ext in [".dll", ".exe", ".DLL", ".EXE"] {
        if let Some(stem) = module_name.strip_suffix(ext) {
            module_name = stem.to_string();
        }
    }
    let offset = address - module.BaseOfImage;
    // SYMBOL_INFOW ends in a name buffer of MaxNameLen characters.
    let mut buf = vec![0u64; (size_of::<SYMBOL_INFOW>() + MAX_SYM_NAME as usize * 2).div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<SYMBOL_INFOW>();
    unsafe {
        (*info).SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
        (*info).MaxNameLen = MAX_SYM_NAME;
    }
    let mut displacement = 0u64;
    // Without symbols dbghelp names the nearest export before the address,
    // however far: beyond a page it is some private function instead.
    const SYMFLAG_EXPORT: u32 = 0x200;
    if unsafe { SymFromAddrW(process, address, Some(&mut displacement), info) }.is_err()
        || (unsafe { (*info).Flags }.0 & SYMFLAG_EXPORT != 0 && displacement > 0x1000)
    {
        return Some(format!("{module_name}+0x{offset:X}"));
    }
    let name = unsafe {
        let len = ((*info).NameLen as usize).min(MAX_SYM_NAME as usize);
        String::from_utf16_lossy(std::slice::from_raw_parts((*info).Name.as_ptr(), len))
    };
    Some(match displacement {
        0 => format!("{module_name}!{name}"),
        d => format!("{module_name}!{name}+0x{d:X}"),
    })
}

/// The user-mode call stack of one thread. The thread is suspended while
/// its stack is read (a few milliseconds), then resumed.
/// A process' threads from the system's thread list (the process is not
/// opened): ids with their CPU time in 100 ns units, the busiest first.
pub fn by_cpu_time(pid: u32) -> Result<Vec<(u32, i64)>, String> {
    let mut scanner = crate::scan::ProcessScanner::new();
    scanner
        .refresh()
        .map_err(|status| format!("the thread list could not be read ({status:#x})"))?;
    let mut threads: Vec<(u32, i64)> = scanner
        .entries()
        .find(|e| e.pid() == pid)
        .map(|p| {
            p.threads()
                .map(|t| {
                    let cpu = t.info.KernelTime + t.info.UserTime;
                    (t.info.ClientId.UniqueThread as u32, cpu)
                })
                .collect()
        })
        .unwrap_or_default();
    threads.sort_by_key(|t| std::cmp::Reverse(t.1));
    Ok(threads)
}

pub fn call_stack(
    key: ProcKey,
    tid: u32,
    fallback: &mut Symbolizer,
) -> Result<StackReport, String> {
    let started = Instant::now();
    if key.pid == std::process::id() {
        return Err("Anabolic does not read its own threads' stacks".into());
    }
    let process = open_process(key, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)
        .map_err(|e| error_text(&e))?;
    let thread = open_thread(key.pid, tid, THREAD_GET_CONTEXT | THREAD_SUSPEND_RESUME)?;
    if tid == unsafe { GetCurrentThreadId() } {
        return Err("that is the calling thread".into());
    }
    let mut wow64 = BOOL(0);
    let _ = unsafe { IsWow64Process(process.raw(), &mut wow64) };
    let wow64 = wow64.as_bool();

    unsafe {
        SymSetOptions(
            SYMOPT_DEFERRED_LOADS
                | SYMOPT_UNDNAME
                | SYMOPT_FAIL_CRITICAL_ERRORS
                | SYMOPT_NO_PROMPTS,
        );
        SymInitializeW(process.raw(), PCWSTR::null(), true).map_err(|e| error_text(&e))?;
    }
    let _symbols = Symbols(process.raw());

    let mut addresses = Vec::new();
    let held_since = Instant::now();
    {
        if unsafe { SuspendThread(thread.raw()) } == u32::MAX {
            return Err(windows::core::Error::from_thread().message());
        }
        let _held = Held(&thread);
        let flat = |offset: u64| ADDRESS64 {
            Offset: offset,
            Segment: 0,
            Mode: AddrModeFlat,
        };
        let mut frame = STACKFRAME64::default();
        let mut x64 = Box::new(AlignedContext(unsafe { std::mem::zeroed() }));
        let mut x86: Box<WOW64_CONTEXT> = Box::new(unsafe { std::mem::zeroed() });
        let (machine, context): (u32, *mut core::ffi::c_void) = if wow64 {
            x86.ContextFlags = WOW64_CONTEXT_FULL;
            unsafe { Wow64GetThreadContext(thread.raw(), &mut *x86) }
                .map_err(|e| error_text(&e))?;
            frame.AddrPC = flat(x86.Eip as u64);
            frame.AddrFrame = flat(x86.Ebp as u64);
            frame.AddrStack = flat(x86.Esp as u64);
            (
                IMAGE_FILE_MACHINE_I386.0 as u32,
                (&mut *x86 as *mut WOW64_CONTEXT).cast(),
            )
        } else {
            x64.0.ContextFlags = CONTEXT_FULL_AMD64;
            unsafe { GetThreadContext(thread.raw(), &mut x64.0) }.map_err(|e| error_text(&e))?;
            frame.AddrPC = flat(x64.0.Rip);
            frame.AddrFrame = flat(x64.0.Rbp);
            frame.AddrStack = flat(x64.0.Rsp);
            (
                IMAGE_FILE_MACHINE_AMD64.0 as u32,
                (&mut x64.0 as *mut CONTEXT).cast(),
            )
        };
        let mut last = (0u64, 0u64);
        while addresses.len() < MAX_FRAMES {
            let ok = unsafe {
                StackWalk64(
                    machine,
                    process.raw(),
                    thread.raw(),
                    &mut frame,
                    context,
                    None,
                    Some(table_access),
                    Some(module_base),
                    None,
                )
            };
            let pc = frame.AddrPC.Offset;
            let at = (pc, frame.AddrStack.Offset);
            if !ok.as_bool() || pc == 0 || at == last {
                break;
            }
            last = at;
            addresses.push(pc);
        }
    }
    let held = held_since.elapsed();

    // Named after the thread runs again.
    let modules = process_modules(key.pid).unwrap_or_default();
    let frames = addresses
        .into_iter()
        .map(|address| Frame {
            address,
            symbol: dbghelp_name(process.raw(), address)
                .or_else(|| fallback.name(&modules, address)),
        })
        .collect();
    Ok(StackReport {
        key,
        tid,
        frames,
        wow64,
        held,
        took: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child that sleeps: its main thread's stack goes through ntdll.
    #[test]
    fn reads_a_sleeping_childs_stack() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .spawn()
            .expect("spawn cmd");
        std::thread::sleep(Duration::from_millis(500));
        let mut scanner = crate::scan::ProcessScanner::new();
        scanner.refresh().unwrap();
        let entry = scanner
            .entries()
            .find(|e| e.pid() == child.id())
            .expect("child listed");
        let key = ProcKey {
            pid: child.id(),
            create_time: entry.spi.CreateTime,
        };
        let tid = entry
            .threads()
            .next()
            .map(|t| t.info.ClientId.UniqueThread as u32)
            .expect("a thread");
        let report = call_stack(key, tid, &mut Symbolizer::default()).expect("stack");
        assert!(!report.frames.is_empty());
        let named = report
            .frames
            .iter()
            .filter_map(|f| f.symbol.as_deref())
            .any(|s| s.to_lowercase().starts_with("ntdll"));
        assert!(named, "{:#?}", report.frames);
        let names = names(child.id(), &[tid]);
        assert!(names.len() <= 1);
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
