// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A process' address space from VirtualQueryEx, block by block, joined
//! into its allocations. What each one is comes from its type (an image, a
//! mapped file or private memory), the file behind it
//! (GetMappedFileNameW), the heaps the process' PEB lists and the threads'
//! stacks (from the system's thread list, so no thread is opened). What is in RAM
//! comes from the process' working set list (QueryWorkingSet), page by
//! page. The process is opened to read, never written to, and nothing runs
//! in it.

use std::time::Instant;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_IMAGE, MEM_MAPPED, MEMORY_BASIC_INFORMATION, VirtualQueryEx,
};
use windows::Win32::System::ProcessStatus::{K32GetMappedFileNameW, K32QueryWorkingSet};
use windows::Win32::System::Threading::{PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

use crate::memmap::{Allocation, Kind, MemoryMap};
use crate::model::ProcKey;
use crate::ntapi;
use crate::util::{error_text, open_process};
use crate::winfile::DeviceMap;

const PAGE: u64 = 4096;
const GRANULARITY: u64 = 0x1_0000;

/// Reads `key`'s memory map.
pub fn read(key: ProcKey) -> Result<MemoryMap, String> {
    let started = Instant::now();
    let stacks = stacks(key.pid);
    let process = open_process(key, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)
        .map_err(|e| format!("the process could not be opened: {}", error_text(&e)))?;
    let h = process.raw();
    let heaps = heaps(h);
    let stacks: Vec<(u32, u64)> = stacks
        .into_iter()
        .filter_map(|(tid, teb)| Some((tid, stack_limit(h, teb)?)))
        .collect();
    let devices = DeviceMap::build();
    let mut allocations: Vec<Allocation> = Vec::new();
    // The most committed bytes under one protection, per allocation.
    let mut protections: Vec<Vec<(u32, u64)>> = Vec::new();
    // Whether an allocation's first page may be read: committed, readable,
    // and no guard page (reading one would clear it, and a thread's stack
    // grows through its guard page).
    let mut readable: Vec<bool> = Vec::new();
    let mut addr: u64 = 0;
    loop {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let got = unsafe {
            VirtualQueryEx(
                h,
                Some(addr as *const _),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if got == 0 {
            break;
        }
        let base = mbi.BaseAddress as u64;
        let size = mbi.RegionSize as u64;
        let next = base.saturating_add(size);
        if mbi.State == MEM_FREE {
            // A free block after an allocation that ends short of the 64 KB
            // granularity: the rest of that 64 KB can never be used.
            if !base.is_multiple_of(GRANULARITY) && !allocations.is_empty() {
                let unusable = (GRANULARITY - base % GRANULARITY).min(size);
                allocations.push(Allocation {
                    base,
                    size: unusable,
                    kind: Kind::Unusable,
                    protection: "None".into(),
                    blocks: 1,
                    ..Allocation::default()
                });
                protections.push(Vec::new());
                readable.push(false);
            }
        } else {
            let alloc_base = mbi.AllocationBase as u64;
            let same = allocations
                .last()
                .is_some_and(|a| a.base == alloc_base && a.kind != Kind::Unusable);
            if !same {
                let kind = if mbi.Type == MEM_IMAGE {
                    Kind::Image
                } else if mbi.Type == MEM_MAPPED {
                    Kind::MappedFile
                } else {
                    Kind::PrivateData
                };
                allocations.push(Allocation {
                    base: alloc_base,
                    kind,
                    ..Allocation::default()
                });
                protections.push(Vec::new());
                let p = mbi.Protect.0;
                readable.push(
                    base == alloc_base
                        && mbi.State == MEM_COMMIT
                        && p & 0x101 == 0
                        && p & 0xEE != 0,
                );
            }
            let a = allocations.last_mut().unwrap();
            a.size += size;
            a.blocks += 1;
            if mbi.State == MEM_COMMIT {
                a.committed += size;
                let p = mbi.Protect.0;
                let list = protections.last_mut().unwrap();
                match list.iter_mut().find(|(q, _)| *q == p) {
                    Some((_, n)) => *n += size,
                    None => list.push((p, size)),
                }
            }
        }
        if next <= addr {
            break;
        }
        addr = next;
    }

    for ((a, prot), &readable) in allocations.iter_mut().zip(&protections).zip(&readable) {
        if a.kind == Kind::Unusable {
            continue;
        }
        a.protection = match prot.iter().max_by_key(|(_, n)| *n) {
            Some((p, _)) => protection(*p),
            None => "Reserved".into(),
        };
        match a.kind {
            Kind::Image | Kind::MappedFile => {
                let mut buf = [0u16; 1024];
                let n = unsafe { K32GetMappedFileNameW(h, a.base as *const _, &mut buf) } as usize;
                if n > 0 {
                    let nt = String::from_utf16_lossy(&buf[..n]);
                    a.detail = devices.to_dos_path(&nt).unwrap_or(nt);
                } else if a.kind == Kind::MappedFile {
                    // Mapped, but not from a file: a section shared with
                    // other processes, or the page file.
                    a.kind = Kind::Shareable;
                }
            }
            Kind::PrivateData => {
                if let Some(i) = heaps.iter().position(|&b| b == a.base) {
                    a.kind = Kind::Heap;
                    a.detail = if i == 0 {
                        "The process' default heap".into()
                    } else {
                        format!("Heap {}", i + 1)
                    };
                } else if readable && let Some(segment) = heap_segment(h, a.base) {
                    // A heap's further segments: they carry its signature.
                    a.kind = Kind::Heap;
                    a.detail = segment.into();
                } else if let Some((tid, _)) = stacks
                    .iter()
                    .find(|(_, limit)| *limit >= a.base && *limit < a.base + a.size)
                {
                    a.kind = Kind::Stack;
                    a.detail = format!("Stack of thread {tid}");
                }
                if prot.iter().any(|(p, _)| executable(*p)) {
                    a.detail = if a.detail.is_empty() {
                        "Executable private memory".into()
                    } else {
                        format!("{}, executable", a.detail)
                    };
                }
            }
            _ => {}
        }
    }
    working_set(h, &mut allocations);
    Ok(MemoryMap {
        key,
        allocations,
        at: crate::sysinfo::now_filetime(),
        took: started.elapsed(),
    })
}

/// The process' threads: (id, where its TEB is), from the system's
/// process list. (Its StackBase and StackLimit there are the kernel's
/// stack, not the thread's own.)
fn stacks(pid: u32) -> Vec<(u32, u64)> {
    let mut scanner = crate::scan::ProcessScanner::new();
    if scanner.refresh().is_err() {
        return Vec::new();
    }
    scanner
        .entries()
        .find(|e| e.pid() == pid)
        .map(|p| {
            p.threads()
                .filter_map(|t| {
                    let teb = t.ext.map(|x| x.TebBase as u64).filter(|&b| b != 0)?;
                    Some((t.info.ClientId.UniqueThread as u32, teb))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether an allocation is a heap's segment, by the signature its header
/// carries at 0x10: 0xFFEEFFEE for the classic heap's segments, 0xDDEEDDEE
/// for a segment heap.
fn heap_segment(h: HANDLE, base: u64) -> Option<&'static str> {
    let mut head = [0u8; 0x18];
    if !read_memory(h, base, &mut head) {
        return None;
    }
    match u32::from_le_bytes(head[0x10..0x14].try_into().ok()?) {
        0xFFEE_FFEE => Some("A segment of a heap"),
        0xDDEE_DDEE => Some("A segment heap"),
        _ => None,
    }
}

/// The lowest address of a thread's stack in use, from its TEB's NT_TIB
/// (StackLimit, after ExceptionList and StackBase).
fn stack_limit(h: HANDLE, teb: u64) -> Option<u64> {
    let mut limit = [0u8; 8];
    read_memory(h, teb + 0x10, &mut limit).then(|| u64::from_le_bytes(limit))
}

fn executable(p: u32) -> bool {
    p & 0xF0 != 0
}

/// A page protection in words: "Read/Write", "Execute/Read", "+Guard".
fn protection(p: u32) -> String {
    let base = match p & 0xFF {
        0x01 => "No access",
        0x02 => "Read",
        0x04 => "Read/Write",
        0x08 => "Copy on write",
        0x10 => "Execute",
        0x20 => "Execute/Read",
        0x40 => "Execute/Read/Write",
        0x80 => "Execute/Copy on write",
        _ => "Other",
    };
    let mut s = base.to_string();
    if p & 0x100 != 0 {
        s.push_str(" +Guard");
    }
    if p & 0x200 != 0 {
        s.push_str(" +No cache");
    }
    if p & 0x400 != 0 {
        s.push_str(" +Write combine");
    }
    s
}

fn read_memory(h: HANDLE, addr: u64, buf: &mut [u8]) -> bool {
    unsafe {
        ReadProcessMemory(
            h,
            addr as *const _,
            buf.as_mut_ptr().cast(),
            buf.len(),
            None,
        )
    }
    .is_ok()
}

/// The process' heaps, by the address each begins at, from its PEB: the
/// 64-bit one, or the 32-bit one of a process under WOW64.
fn heaps(h: HANDLE) -> Vec<u64> {
    let query = |class: u32, out: &mut [u64]| {
        let status = unsafe {
            ntapi::NtQueryInformationProcess(
                h,
                class,
                out.as_mut_ptr().cast(),
                (out.len() * 8) as u32,
                std::ptr::null_mut(),
            )
        };
        ntapi::nt_success(status)
    };
    // ProcessWow64Information (26): the 32-bit PEB, or 0.
    let mut wow = [0u64; 1];
    let peb32 = if query(26, &mut wow) { wow[0] } else { 0 };
    let (peb, count_at, list_at, pointer) = if peb32 != 0 {
        (peb32, 0x88, 0x90, 4)
    } else {
        // ProcessBasicInformation (0): the PEB's address is its second field.
        let mut basic = [0u64; 6];
        if !query(0, &mut basic) {
            return Vec::new();
        }
        (basic[1], 0xE8, 0xF0, 8)
    };
    let mut count = [0u8; 4];
    let mut list = [0u8; 8];
    if !read_memory(h, peb + count_at, &mut count)
        || !read_memory(h, peb + list_at, &mut list[..pointer])
    {
        return Vec::new();
    }
    let count = u32::from_le_bytes(count).min(1024) as usize;
    let list = u64::from_le_bytes(list);
    let mut raw = vec![0u8; count * pointer];
    if count == 0 || !read_memory(h, list, &mut raw) {
        return Vec::new();
    }
    raw.chunks_exact(pointer)
        .map(|c| {
            let mut b = [0u8; 8];
            b[..pointer].copy_from_slice(c);
            u64::from_le_bytes(b)
        })
        .collect()
}

/// Puts each page of the working set on its allocation: what is in RAM,
/// and what only this process uses.
fn working_set(h: HANDLE, allocations: &mut [Allocation]) {
    // The list's room: grows to what it says it needs.
    let mut buf: Vec<u64> = vec![0; 64 * 1024];
    let mut ok = false;
    for _ in 0..4 {
        if unsafe { K32QueryWorkingSet(h, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32) }
            .as_bool()
        {
            ok = true;
            break;
        }
        let entries = buf[0] as usize;
        buf.resize(entries + entries / 8 + 1024, 0);
    }
    if !ok {
        return;
    }
    let count = (buf[0] as usize).min(buf.len() - 1);
    let mut order: Vec<usize> = (0..allocations.len()).collect();
    order.sort_by_key(|&i| allocations[i].base);
    let bases: Vec<u64> = order.iter().map(|&i| allocations[i].base).collect();
    for &entry in &buf[1..=count] {
        let page = entry & !0xFFF;
        let shared = entry & 0x100 != 0;
        let at = match bases.binary_search(&page) {
            Ok(i) => i,
            Err(0) => continue,
            Err(i) => i - 1,
        };
        let a = &mut allocations[order[at]];
        if page >= a.base + a.size {
            continue;
        }
        a.working_set += PAGE;
        if !shared {
            a.private_ws += PAGE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_protections_in_words() {
        assert_eq!(protection(0x04), "Read/Write");
        assert_eq!(protection(0x104), "Read/Write +Guard");
        assert_eq!(protection(0x40), "Execute/Read/Write");
        assert!(executable(0x20));
        assert!(!executable(0x04));
    }

    /// Another process, as in use: a child that waits a few seconds.
    /// (Not the test process itself: the other tests run their threads in
    /// it meanwhile.)
    #[test]
    fn maps_a_childs_memory() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 6 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let key = ProcKey {
            pid: child.id(),
            create_time: 0,
        };
        let map = read(key);
        let _ = child.kill();
        let _ = child.wait();
        let map = map.unwrap();
        let total = map.total();
        assert!(total.committed > 1 << 20, "{total:?}");
        assert!(total.working_set > 0);
        let image = |name: &str| {
            map.allocations
                .iter()
                .any(|a| a.kind == Kind::Image && a.detail.to_lowercase().ends_with(name))
        };
        assert!(image("cmd.exe"), "its program is mapped");
        assert!(image("ntdll.dll"));
        assert!(map.allocations.iter().any(|a| a.kind == Kind::Heap));
        assert!(map.allocations.iter().any(|a| a.kind == Kind::Stack));
        println!(
            "{} allocations, {} committed, {} in RAM, in {:?}",
            map.allocations.len(),
            total.committed,
            total.working_set,
            map.took
        );
    }
}
