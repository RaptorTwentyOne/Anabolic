//! Which threads ran on each logical processor, from the kernel's sampled
//! profile (ETW, admin): every millisecond a busy processor reports the
//! thread it is running. Context switch tracing would be exact but costs far
//! more on a busy machine (hundreds of thousands of events a second); the
//! samples answer "who is using this core" just as well. Still not free, so
//! it only runs while someone is looking.
//!
//! Idle processors do not take profile interrupts (tickless idle), so sample
//! counts say who shares the busy time, not how busy a processor was; the
//! collector scales them by the measured utilisation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::Win32::System::Diagnostics::Etw::{
    EVENT_TRACE_FLAG_PROCESS, EVENT_TRACE_FLAG_PROFILE, EVENT_TRACE_FLAG_THREAD,
    EVENT_TRACE_SYSTEM_LOGGER_MODE, PerfInfoGuid, ProcessGuid, ThreadGuid,
};

use crate::etw::{
    self, OPCODE_PROCESS_RUNDOWN, OPCODE_PROCESS_START, Session, process_image, u32_at,
};

const OPCODE_SAMPLED_PROFILE: u8 = 46;
const OPCODE_THREAD_START: u8 = 1;
const OPCODE_THREAD_RUNDOWN: u8 = 3;

#[derive(Default)]
struct Shared {
    /// Per processor: tid -> samples since the last take.
    samples: Vec<HashMap<u32, u32>>,
    /// tid -> pid for threads that started (or were enumerated) since the last
    /// take; short-lived threads are gone before the next process scan.
    owners: HashMap<u32, u32>,
    images: HashMap<u32, Arc<str>>,
}

/// Samples per thread on each processor since the previous `take`, plus the
/// owners of threads seen starting meanwhile.
#[derive(Default, Debug)]
pub struct CoreSamples {
    /// Per processor: (tid, samples); tid 0 is the idle thread.
    pub per_cpu: Vec<Vec<(u32, u32)>>,
    pub owners: HashMap<u32, u32>,
    /// pid -> image name for processes seen starting meanwhile: one that
    /// lives for less than a refresh is never in a process scan.
    pub images: HashMap<u32, Arc<str>>,
}

pub struct CoreTracer {
    shared: Arc<Mutex<Shared>>,
    _session: Session,
}

impl CoreTracer {
    pub fn start(cpus: usize) -> Result<Self, String> {
        let flags =
            EVENT_TRACE_FLAG_PROFILE.0 | EVENT_TRACE_FLAG_THREAD.0 | EVENT_TRACE_FLAG_PROCESS.0;
        let session = Session::start("Anabolic-Cores", EVENT_TRACE_SYSTEM_LOGGER_MODE, flags)?;
        let shared = Arc::new(Mutex::new(Shared {
            samples: vec![HashMap::new(); cpus],
            ..Default::default()
        }));
        let sink = shared.clone();
        session.consume(move |record| {
            let header = &record.EventHeader;
            let opcode = header.EventDescriptor.Opcode;
            let data = etw::user_data(record);
            if opcode == OPCODE_SAMPLED_PROFILE && header.ProviderId == PerfInfoGuid {
                // InstructionPointer (pointer), ThreadId (u32), Count (u16), Reserved.
                let Some(tid) = u32_at(data, 8) else { return };
                let count = data
                    .get(12..14)
                    .map_or(1, |b| u16::from_le_bytes([b[0], b[1]]).max(1))
                    as u32;
                let cpu = unsafe { record.BufferContext.Anonymous.ProcessorIndex } as usize;
                if let Ok(mut shared) = sink.lock()
                    && let Some(slot) = shared.samples.get_mut(cpu)
                {
                    *slot.entry(tid).or_default() += count;
                }
            } else if (opcode == OPCODE_THREAD_START || opcode == OPCODE_THREAD_RUNDOWN)
                && header.ProviderId == ThreadGuid
            {
                // ProcessId, TThreadId, ...
                if let (Some(pid), Some(tid)) = (u32_at(data, 0), u32_at(data, 4))
                    && let Ok(mut shared) = sink.lock()
                {
                    shared.owners.insert(tid, pid);
                }
            } else if (opcode == OPCODE_PROCESS_START || opcode == OPCODE_PROCESS_RUNDOWN)
                && header.ProviderId == ProcessGuid
                && let Some((pid, image)) = process_image(data)
                && let Ok(mut shared) = sink.lock()
            {
                shared.images.insert(pid, Arc::from(image));
            }
        })?;
        Ok(Self {
            shared,
            _session: session,
        })
    }

    pub fn take(&self) -> CoreSamples {
        let Ok(mut shared) = self.shared.lock() else {
            return CoreSamples::default();
        };
        CoreSamples {
            per_cpu: shared
                .samples
                .iter_mut()
                .map(|s| s.drain().collect())
                .collect(),
            owners: std::mem::take(&mut shared.owners),
            images: std::mem::take(&mut shared.images),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Needs an elevated test run; skips itself otherwise.
    #[test]
    fn sees_a_busy_short_lived_thread() {
        if !crate::hardening::is_elevated() {
            eprintln!("not elevated, skipping");
            return;
        }
        let _ = crate::hardening::enable_privilege(windows::core::w!("SeSystemProfilePrivilege"));
        let cpus = crate::cpu::CpuSampler::new().core_count();
        let tracer = CoreTracer::start(cpus).expect("could not start the trace");
        std::thread::sleep(Duration::from_millis(300));
        let _ = tracer.take();
        // The thread is gone before anyone could scan it.
        let busy = std::thread::spawn(|| {
            let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
            let until = Instant::now() + Duration::from_millis(1500);
            let mut x = 0u64;
            while Instant::now() < until {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            }
            std::hint::black_box(x);
            tid
        });
        let tid = busy.join().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let taken = tracer.take();
        let samples: u32 = taken
            .per_cpu
            .iter()
            .flatten()
            .filter(|(t, _)| *t == tid)
            .map(|(_, n)| n)
            .sum();
        assert!(
            samples > 1000 && samples < 2000,
            "busy thread got {samples} samples (~ms)"
        );
        assert_eq!(
            taken.owners.get(&tid),
            Some(&std::process::id()),
            "thread start was not seen"
        );
        // Processes come and go too fast for scans; the trace names them.
        let child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .status()
            .unwrap();
        assert!(child.success());
        std::thread::sleep(Duration::from_millis(1500));
        let taken = tracer.take();
        assert!(
            taken
                .images
                .values()
                .any(|i| i.eq_ignore_ascii_case("cmd.exe")),
            "cmd.exe start was not seen: {:?}",
            taken.images.values().take(5).collect::<Vec<_>>()
        );
    }
}
