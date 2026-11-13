// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The timer's resolution from the kernel (NtQueryTimerResolution), and
//! who asked for it from the kernel's power provider: asked for a rundown,
//! Microsoft-Windows-Kernel-Power logs every outstanding request
//! (SystemTimeResolutionRequestRundown, event 97: the resolution, the
//! process and its image's path), as `powercfg /energy` reads them. Only
//! an administrator may start the trace. No process is opened.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::GUID;

use crate::etw::{Session, user_data};
use crate::timer::{Request, TimerResolution, ms};

const KERNEL_POWER: GUID = GUID::from_u128(0x331c3b3a_2005_44c2_ac5e_77220c37d6b4);
/// The provider's rundown keyword.
const RUNDOWN: u64 = 0x4000;
const REQUEST_RUNDOWN: u16 = 97;

pub fn read() -> Result<TimerResolution, String> {
    let started = Instant::now();
    let (mut coarsest, mut finest, mut current) = (0u32, 0u32, 0u32);
    let status =
        unsafe { crate::ntapi::NtQueryTimerResolution(&mut coarsest, &mut finest, &mut current) };
    if !crate::ntapi::nt_success(status) {
        return Err(crate::ntapi::status_message(status));
    }
    let mut out = TimerResolution {
        current_ms: ms(current),
        finest_ms: ms(finest),
        coarsest_ms: ms(coarsest),
        ..TimerResolution::default()
    };
    match requests() {
        Ok(r) => out.requests = r,
        Err(e) => out.requests_unknown = Some(e),
    }
    out.took = started.elapsed();
    Ok(out)
}

/// One event 97: the resolution asked for (100 ns units), the process,
/// then the length of its image's NT path in characters and the path.
pub(crate) fn parse(d: &[u8], map: &crate::winfile::DeviceMap) -> Option<Request> {
    let units = crate::etw::u32_at(d, 0)?;
    let pid = crate::etw::u32_at(d, 4)?;
    let chars = u16::from_le_bytes(d.get(8..10)?.try_into().ok()?) as usize;
    let text: Vec<u16> = d
        .get(10..10 + chars * 2)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&c| c != 0)
        .collect();
    let nt = String::from_utf16_lossy(&text);
    let path = map.to_dos_path(&nt).or((!nt.is_empty()).then_some(nt));
    let name = path
        .as_deref()
        .and_then(|p| p.rsplit('\\').next())
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("Process {pid}"), str::to_string);
    (units > 0).then_some(Request {
        pid,
        name,
        path,
        ms: ms(units),
    })
}

fn requests() -> Result<Vec<Request>, String> {
    let session = Session::start("Anabolic-Timer", 0, 0).map_err(|e| {
        if e.to_lowercase().contains("access") {
            "Windows tells only an administrator which programs asked".to_string()
        } else {
            e
        }
    })?;
    session.enable_ids(&KERNEL_POWER, 5, RUNDOWN, &[REQUEST_RUNDOWN])?;
    let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let keep = seen.clone();
    let consumer = session.consume(move |r| {
        if r.EventHeader.EventDescriptor.Id == REQUEST_RUNDOWN
            && let Ok(mut seen) = keep.lock()
        {
            seen.push(user_data(r).to_vec());
        }
    })?;
    session.capture_state(&KERNEL_POWER, RUNDOWN)?;
    // The rundown is written at once; give the buffers a moment to reach
    // the consumer, then stop, which flushes the rest.
    std::thread::sleep(Duration::from_millis(400));
    drop(session);
    let _ = consumer.join();
    let map = crate::winfile::DeviceMap::build();
    let raw = seen.lock().map(|s| s.clone()).unwrap_or_default();
    let mut out: Vec<Request> = raw.iter().filter_map(|d| parse(d, &map)).collect();
    out.sort_by(|a, b| a.ms.total_cmp(&b.ms).then_with(|| a.name.cmp(&b.name)));
    out.dedup_by(|a, b| a.pid == b.pid && a.ms == b.ms);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_request() {
        let path = r"\Device\HarddiskVolume9\Games\game.exe";
        let mut d = 10_000u32.to_le_bytes().to_vec();
        d.extend(4242u32.to_le_bytes());
        d.extend((path.len() as u16).to_le_bytes());
        d.extend(path.encode_utf16().flat_map(|u| u.to_le_bytes()));
        d.extend([0, 0]);
        let r = parse(&d, &crate::winfile::DeviceMap::default()).unwrap();
        assert_eq!(r.pid, 4242);
        assert_eq!(r.name, "game.exe");
        assert_eq!(r.ms, 1.0);
        // A release is no request.
        d[..4].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse(&d, &crate::winfile::DeviceMap::default()).is_none());
    }

    #[test]
    fn reads_the_resolution_and_who_asked() {
        let t = read().unwrap();
        assert!(t.finest_ms > 0.0 && t.finest_ms <= t.current_ms);
        assert!(t.current_ms <= t.coarsest_ms);
        println!(
            "now {} (finest {}, default {}) in {:?}",
            crate::timer::words(t.current_ms),
            crate::timer::words(t.finest_ms),
            crate::timer::words(t.coarsest_ms),
            t.took
        );
        for r in &t.requests {
            println!("  {} {} {:?}", crate::timer::words(r.ms), r.name, r.path);
        }
        if let Some(e) = &t.requests_unknown {
            println!("  requests: {e}");
        }
    }
}
