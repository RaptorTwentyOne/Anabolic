//! Static hardware facts: what the CPU is and supports (CPUID, SMBIOS), the
//! installed memory modules (SMBIOS type 17), board and firmware, page files.
//! Gathered once; none of this changes while running.

use core::mem::size_of;

use windows::Win32::System::Power::{CallNtPowerInformation, ProcessorInformation};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RegGetValueW};
use windows::Win32::System::SystemInformation::{
    GetPhysicallyInstalledSystemMemory, GetSystemFirmwareTable, RSMB,
};
use windows::Win32::System::Threading::{IsProcessorFeaturePresent, PF_VIRT_FIRMWARE_ENABLED};
use windows::core::w;

use crate::ntapi;

use super::*;

pub(super) fn smbios() -> Vec<u8> {
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    if size == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize];
    let got = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) };
    buf.truncate(got as usize);
    buf
}

pub(super) fn microcode() -> Option<String> {
    let mut data = [0u8; 8];
    let mut size = data.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            w!("Update Revision"),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let high = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let low = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let revision = if high != 0 { high } else { low };
    (revision != 0).then(|| format!("0x{revision:X}"))
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPowerInformation {
    number: u32,
    max_mhz: u32,
    current_mhz: u32,
    mhz_limit: u32,
    max_idle_state: u32,
    current_idle_state: u32,
}

/// Nominal ("100 %") frequency per logical processor, in MHz.
pub fn nominal_mhz(cpus: usize) -> Vec<u32> {
    let mut info = vec![ProcessorPowerInformation::default(); cpus.max(1)];
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(info.as_mut_ptr().cast()),
            (info.len() * size_of::<ProcessorPowerInformation>()) as u32,
        )
    };
    if status.is_err() {
        return Vec::new();
    }
    info.iter().map(|i| i.max_mhz).collect()
}

/// What Windows counts as installed, from the firmware's memory map.
pub(super) fn installed_memory() -> u64 {
    let mut installed_kb = 0u64;
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut installed_kb) }.is_ok() {
        installed_kb * 1024
    } else {
        0
    }
}

pub(super) fn virtualization_enabled() -> bool {
    unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) }.as_bool()
}

/// Windows always has the SMBIOS tables to read the board from.
pub(super) fn board_fallback() -> Option<BoardInfo> {
    None
}

/// Page files with their size and use.
pub fn page_files() -> Vec<PageFile> {
    let mut buf = vec![0u64; 512];
    let mut returned = 0u32;
    let status = unsafe {
        ntapi::NtQuerySystemInformation(
            ntapi::SystemPageFileInformation,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut returned,
        )
    };
    if !ntapi::nt_success(status) {
        return Vec::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        if at + size_of::<ntapi::SYSTEM_PAGEFILE_INFORMATION>() > bytes.len() {
            break;
        }
        let info = unsafe {
            bytes
                .as_ptr()
                .add(at)
                .cast::<ntapi::SYSTEM_PAGEFILE_INFORMATION>()
                .read_unaligned()
        };
        if info.TotalSize == 0 && info.NextEntryOffset == 0 && out.is_empty() && returned == 0 {
            break;
        }
        let name = info.PageFileName;
        let base = bytes.as_ptr() as usize;
        let ptr = name.Buffer as usize;
        let len = name.Length as usize & !1;
        let path = if !name.Buffer.is_null() && ptr >= base && ptr + len <= base + bytes.len() {
            let units: Vec<u16> = bytes[ptr - base..ptr - base + len]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
                .trim_start_matches(r"\??\")
                .to_string()
        } else {
            String::new()
        };
        const PAGE: u64 = 4096;
        out.push(PageFile {
            path,
            size: info.TotalSize as u64 * PAGE,
            used: info.TotalInUse as u64 * PAGE,
            peak: info.PeakUsage as u64 * PAGE,
        });
        if info.NextEntryOffset == 0 {
            break;
        }
        at += info.NextEntryOffset as usize;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cpu_and_memory() {
        let hw = hardware_info();
        println!("{:#?}", hw.cpu);
        println!("{:#?}", hw.board);
        println!(
            "{} slots, {} modules, installed {} GB, {}",
            hw.memory.slots,
            hw.memory.modules.len(),
            hw.memory.installed >> 30,
            hw.memory.error_correction
        );
        for m in &hw.memory.modules {
            println!(
                "  {} {} {} GB {} {} {} MT/s (cfg {}) {} {} rank {} {} mV",
                m.locator,
                m.bank,
                m.size >> 30,
                m.kind,
                m.form_factor,
                m.speed,
                m.configured_speed,
                m.manufacturer,
                m.part_number,
                m.rank,
                m.voltage_mv
            );
        }
        println!("{:?}", page_files());
        assert!(!hw.cpu.brand.is_empty());
        assert!(hw.cpu.threads >= hw.cpu.cores && hw.cpu.cores > 0);
        assert!(hw.memory.installed > 0);
        let sum: u64 = hw.memory.modules.iter().map(|m| m.size).sum();
        assert!(
            sum == 0 || sum == hw.memory.installed,
            "modules {sum} vs installed {}",
            hw.memory.installed
        );
    }
}
