// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The profiler on Windows: a kernel trace session of its own with the
//! sampled profile and a stack for each sample, and the modules' loads
//! (with the ones already loaded, from the session's rundown) to place the
//! addresses. The process is never opened: its stacks are walked by the
//! kernel, and the names come from the modules' files through dbghelp, from
//! the symbols on this machine or the exports. No symbol server is asked.

use core::ffi::c_void;
use core::mem::size_of;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::{
    MAX_SYM_NAME, SYMBOL_INFOW, SYMFLAG_EXPORT, SYMOPT_DEFERRED_LOADS, SYMOPT_FAIL_CRITICAL_ERRORS,
    SYMOPT_NO_PROMPTS, SYMOPT_UNDNAME, SymCleanup, SymFromAddrW, SymInitializeW, SymLoadModuleExW,
    SymSetOptions,
};
use windows::Win32::System::Diagnostics::Etw::{
    EVENT_TRACE_FLAG_IMAGE_LOAD, EVENT_TRACE_FLAG_PROCESS, EVENT_TRACE_FLAG_PROFILE,
    EVENT_TRACE_SYSTEM_LOGGER_MODE, ImageLoadGuid, PerfInfoGuid, ProcessGuid,
};
use windows::core::{GUID, PCWSTR};

use crate::etw::{Buffers, Session, pointer_at, pointer_size, u32_at, u64_at, user_data};
use crate::profile::{Frame, Profile, Sample};
use crate::util::wide;
use crate::winfile::DeviceMap;

/// The kernel's stack walk events.
const STACK_WALK: GUID = GUID::from_u128(0xdef2fe46_7bd6_4b80_bd94_f57fe20d0ce3);
const OPCODE_STACK: u8 = 32;
const OPCODE_SAMPLED_PROFILE: u8 = 46;
const OPCODE_IMAGE_LOAD: u8 = 10;
const OPCODE_IMAGE_RUNDOWN: u8 = 3;
const OPCODE_PROCESS_END: u8 = 2;
/// Enough for a minute of a busy process; the rest is left out.
const MAX_SAMPLES: usize = 600_000;
const KERNEL_SPACE: u64 = 0xFFFF_8000_0000_0000;

#[derive(Clone)]
struct Image {
    base: u64,
    size: u64,
    /// The NT path the kernel names it by.
    path: String,
}

#[derive(Default)]
struct Capture {
    /// Each sample's stack by (its time, thread): the kernel's part comes
    /// first, the user part once the thread is back in user mode.
    parts: HashMap<(u64, u32), Vec<u64>>,
    order: Vec<(u64, u32)>,
    images: Vec<Image>,
    ended: bool,
}

pub fn profile(
    pid: u32,
    seconds: f32,
    stop: &(dyn Fn() -> bool + Sync),
) -> Result<Profile, String> {
    if !crate::hardening::is_elevated() {
        return Err("Windows samples code for administrators only".into());
    }
    if pid == 0 || pid == 4 {
        return Err("The idle process and the kernel's own threads are not profiled".into());
    }
    // The kernel's profiler and its stacks need this switched on.
    let _ = crate::hardening::enable_privilege(windows::core::w!("SeSystemProfilePrivilege"));
    let flags =
        EVENT_TRACE_FLAG_PROFILE.0 | EVENT_TRACE_FLAG_IMAGE_LOAD.0 | EVENT_TRACE_FLAG_PROCESS.0;
    static INSTANCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let instance = INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let session = Session::start_instance(
        "Anabolic-Profile",
        instance,
        EVENT_TRACE_SYSTEM_LOGGER_MODE,
        flags,
        Buffers {
            kb: 1024,
            min: 32,
            max: 256,
        },
    )?;
    session.stacks(&[(PerfInfoGuid, OPCODE_SAMPLED_PROFILE)])?;
    let capture = Arc::new(Mutex::new(Capture::default()));
    let sink = capture.clone();
    let consumer = session.consume(move |record| {
        let h = &record.EventHeader;
        let opcode = h.EventDescriptor.Opcode;
        let d = user_data(record);
        let p = pointer_size(record);
        let Ok(mut c) = sink.lock() else { return };
        if h.ProviderId == STACK_WALK && opcode == OPCODE_STACK {
            if u32_at(d, 8) != Some(pid) || c.order.len() >= MAX_SAMPLES {
                return;
            }
            let (Some(at), Some(tid)) = (u64_at(d, 0), u32_at(d, 12)) else {
                return;
            };
            let frames: Vec<u64> = (16..d.len())
                .step_by(p)
                .filter_map(|o| pointer_at(d, o, p))
                .filter(|&a| a != 0)
                .collect();
            let key = (at, tid);
            match c.parts.get_mut(&key) {
                Some(part) => part.extend(frames),
                None => {
                    c.parts.insert(key, frames);
                    c.order.push(key);
                }
            }
        } else if h.ProviderId == ImageLoadGuid
            && (opcode == OPCODE_IMAGE_LOAD || opcode == OPCODE_IMAGE_RUNDOWN)
        {
            // ImageBase, ImageSize, ProcessId, ... FileName at 3 pointers
            // and 32 bytes in.
            let owner = u32_at(d, 2 * p);
            if owner != Some(pid) && owner != Some(0) {
                return;
            }
            let (Some(base), Some(size)) = (pointer_at(d, 0, p), pointer_at(d, p, p)) else {
                return;
            };
            let name: Vec<u16> = d
                .get(3 * p + 32..)
                .unwrap_or(&[])
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| u16::from_le_bytes(b))
                .take_while(|&c| c != 0)
                .collect();
            c.images.push(Image {
                base,
                size,
                path: String::from_utf16_lossy(&name),
            });
        } else if h.ProviderId == ProcessGuid
            && opcode == OPCODE_PROCESS_END
            && u32_at(d, p) == Some(pid)
        {
            c.ended = true;
        }
    })?;
    let started = Instant::now();
    let wanted = Duration::from_secs_f32(seconds.clamp(0.5, 120.0));
    while started.elapsed() < wanted && !stop() {
        std::thread::sleep(Duration::from_millis(50));
        if capture.lock().is_ok_and(|c| c.ended) {
            break;
        }
    }
    let took = started.elapsed();
    let (lost_events, lost_buffers) = session.lost();
    // Stopping the session ends the consumer once the last buffers are in.
    drop(session);
    let _ = consumer.join();
    let capture = std::mem::take(&mut *capture.lock().map_err(|_| "the capture broke")?);
    let mut notes = Vec::new();
    if capture.ended {
        notes.push("The process exited while it was sampled.".to_string());
    }
    if capture.order.len() >= MAX_SAMPLES {
        notes.push(format!("Only the first {MAX_SAMPLES} samples were kept."));
    }
    if lost_events > 0 || lost_buffers > 0 {
        notes.push(format!(
            "The system dropped {lost_events} events: some samples are missing."
        ));
    }
    let (frames, samples) = symbolize(&capture);
    if samples.is_empty() {
        notes.push(
            "No samples: the process used no processor time while it was sampled.".to_string(),
        );
    }
    Ok(Profile::build(pid, took, frames, &samples, notes))
}

/// A dbghelp session on module files alone, ended on drop.
struct Symbols(HANDLE);

impl Drop for Symbols {
    fn drop(&mut self) {
        let _ = unsafe { SymCleanup(self.0) };
    }
}

/// The function at an address of a loaded module; None without a name
/// (an export far from the address is some private function instead).
fn function_at(h: HANDLE, address: u64) -> Option<String> {
    let mut buf = vec![0u64; (size_of::<SYMBOL_INFOW>() + MAX_SYM_NAME as usize * 2).div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<SYMBOL_INFOW>();
    unsafe {
        (*info).SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
        (*info).MaxNameLen = MAX_SYM_NAME;
    }
    let mut displacement = 0u64;
    unsafe { SymFromAddrW(h, address, Some(&mut displacement), info) }.ok()?;
    if unsafe { (*info).Flags }.0 & SYMFLAG_EXPORT.0 != 0 && displacement > 0x1000 {
        return None;
    }
    let name = unsafe {
        let len = ((*info).NameLen as usize).min(MAX_SYM_NAME as usize);
        String::from_utf16_lossy(std::slice::from_raw_parts((*info).Name.as_ptr(), len))
    };
    Some(name)
}

/// Names every address and turns the stacks into samples of frames.
fn symbolize(capture: &Capture) -> (Vec<Frame>, Vec<Sample>) {
    let mut images = capture.images.clone();
    images.sort_by_key(|i| i.base);
    images.dedup_by_key(|i| i.base);
    let devices = DeviceMap::build();
    let files: Vec<(String, Arc<str>)> = images
        .iter()
        .map(|i| {
            let dos = devices
                .to_dos_path(&i.path)
                .unwrap_or_else(|| i.path.clone());
            let name: Arc<str> = dos.rsplit(['\\', '/']).next().unwrap_or(&dos).into();
            (dos, name)
        })
        .collect();
    let _lock = crate::threads::DBGHELP
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Any value no other dbghelp session uses: no process is opened.
    let h = HANDLE(0x0A11_0B5E_usize as *mut c_void);
    let symbols = unsafe {
        SymSetOptions(
            SYMOPT_DEFERRED_LOADS
                | SYMOPT_UNDNAME
                | SYMOPT_FAIL_CRITICAL_ERRORS
                | SYMOPT_NO_PROMPTS,
        );
        SymInitializeW(h, PCWSTR::null(), false)
    }
    .ok()
    .map(|()| Symbols(h));
    let mut loaded = vec![false; images.len()];
    let mut frames: Vec<Frame> = Vec::new();
    let mut ids: HashMap<Frame, u32> = HashMap::new();
    let mut by_address: HashMap<u64, u32> = HashMap::new();
    let unknown: Arc<str> = "?".into();
    let mut name_of = |address: u64| -> u32 {
        if let Some(&id) = by_address.get(&address) {
            return id;
        }
        let at = images.partition_point(|i| i.base <= address);
        let image = at
            .checked_sub(1)
            .filter(|&i| address < images[i].base.saturating_add(images[i].size));
        let frame = match image {
            Some(i) => {
                if symbols.is_some() && !loaded[i] {
                    loaded[i] = true;
                    let path = wide(&files[i].0);
                    unsafe {
                        SymLoadModuleExW(
                            h,
                            None,
                            PCWSTR(path.as_ptr()),
                            PCWSTR::null(),
                            images[i].base,
                            images[i].size as u32,
                            None,
                            None,
                        );
                    }
                }
                Frame {
                    module: files[i].1.clone(),
                    function: symbols
                        .as_ref()
                        .and_then(|_| function_at(h, address))
                        .unwrap_or_default()
                        .into(),
                    kernel: address >= KERNEL_SPACE,
                }
            }
            None => Frame {
                module: unknown.clone(),
                function: "".into(),
                kernel: address >= KERNEL_SPACE,
            },
        };
        let id = *ids.entry(frame.clone()).or_insert_with(|| {
            frames.push(frame);
            (frames.len() - 1) as u32
        });
        by_address.insert(address, id);
        id
    };
    let samples = capture
        .order
        .iter()
        .filter_map(|key| {
            let stack = capture.parts.get(key)?;
            let mut out: Vec<u32> = stack.iter().map(|&a| name_of(a)).collect();
            // A function's frames side by side (its own inlined calls,
            // addresses without names in one module) are one call.
            out.dedup();
            Some(Sample {
                tid: key.1,
                frames: out,
            })
        })
        .collect();
    drop(symbols);
    (frames, samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Profiles a thread that spins in a function of this test for two
    /// seconds. Needs an administrator.
    #[test]
    #[ignore]
    fn finds_where_a_busy_thread_spends_its_time() {
        let _turn = crate::etw::kernel_turn();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        #[inline(never)]
        fn spin_here(flag: &std::sync::atomic::AtomicBool) -> u64 {
            let mut x = 0u64;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
            x
        }
        let worker = std::thread::spawn(move || spin_here(&flag));
        let p = profile(std::process::id(), 2.0, &|| false).unwrap();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = worker.join();
        println!(
            "{} samples in {:?}, {} frames, {} nodes, notes {:?}",
            p.samples,
            p.took,
            p.frames.len(),
            p.tree.len(),
            p.notes
        );
        for h in p.functions.iter().take(8) {
            println!(
                "{:5.1} % own {:5.1} % total  {}",
                p.share(h.own),
                p.share(h.total),
                p.frames[h.frame as usize].label()
            );
        }
        for m in p.modules.iter().take(5) {
            println!("{:5.1} %  {}", p.share(m.own), m.module);
        }
        assert!(
            p.samples > 500,
            "a spinning thread is sampled about 1000 times a second"
        );
        let top = &p.frames[p.functions[0].frame as usize];
        assert!(top.module.ends_with(".exe"), "{}", top.label());
    }
}
