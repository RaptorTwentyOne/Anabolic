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
use crate::topology::{Topology, topology};

#[derive(Clone, Debug, Default)]
pub struct CpuInfo {
    pub brand: String,
    pub vendor: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub socket: String,
    pub sockets: usize,
    pub cores: usize,
    pub threads: usize,
    pub numa_nodes: usize,
    /// Nominal frequency as Windows reports it (what 100 % performance means).
    pub base_mhz: u32,
    /// SMBIOS: highest supported speed and the external (bus) clock.
    pub max_mhz: u32,
    pub bus_mhz: u32,
    pub voltage: Option<f32>,
    /// (name, size per instance, instances)
    pub caches: Vec<(String, u32, usize)>,
    pub features: Vec<&'static str>,
    pub virtualization_enabled: bool,
    pub hypervisor_present: bool,
    pub microcode: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryModule {
    pub locator: String,
    pub bank: String,
    pub size: u64,
    pub kind: &'static str,
    pub form_factor: &'static str,
    /// MT/s
    pub speed: u32,
    pub configured_speed: u32,
    pub manufacturer: String,
    pub part_number: String,
    pub serial: String,
    pub rank: u8,
    pub voltage_mv: u16,
}

#[derive(Clone, Debug, Default)]
pub struct PageFile {
    pub path: String,
    pub size: u64,
    pub used: u64,
    pub peak: u64,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryInfo {
    /// What the firmware reports as installed.
    pub installed: u64,
    pub slots: usize,
    pub max_capacity: u64,
    pub error_correction: &'static str,
    pub modules: Vec<MemoryModule>,
}

#[derive(Clone, Debug, Default)]
pub struct BoardInfo {
    pub manufacturer: String,
    pub product: String,
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
}

#[derive(Clone, Debug, Default)]
pub struct HardwareInfo {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub board: BoardInfo,
    pub topology: Topology,
}

/// One SMBIOS structure: its formatted bytes and its strings.
struct Structure<'a> {
    kind: u8,
    data: &'a [u8],
    strings: Vec<&'a [u8]>,
}

impl Structure<'_> {
    fn u8(&self, at: usize) -> Option<u8> {
        self.data.get(at).copied()
    }
    fn u16(&self, at: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            self.data.get(at..at + 2)?.try_into().ok()?,
        ))
    }
    fn u32(&self, at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            self.data.get(at..at + 4)?.try_into().ok()?,
        ))
    }
    fn u64(&self, at: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            self.data.get(at..at + 8)?.try_into().ok()?,
        ))
    }
    /// Strings are referenced by 1-based index; 0 means none.
    fn string(&self, at: usize) -> String {
        match self.u8(at) {
            Some(n) if n > 0 => self
                .strings
                .get(n as usize - 1)
                .map(|s| String::from_utf8_lossy(s).trim().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
}

fn smbios() -> Vec<u8> {
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    if size == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize];
    let got = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) };
    buf.truncate(got as usize);
    buf
}

fn structures(raw: &[u8]) -> Vec<Structure<'_>> {
    // RawSMBIOSData: 4 version bytes, u32 length, then the table.
    let Some(len) = raw
        .get(4..8)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    else {
        return Vec::new();
    };
    let table = &raw[8..(8 + len).min(raw.len())];
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= table.len() {
        let (kind, length) = (table[at], table[at + 1] as usize);
        if length < 4 || at + length > table.len() {
            break;
        }
        let data = &table[at..at + length];
        // Strings follow, each NUL-terminated; the set ends with an extra NUL.
        let mut strings = Vec::new();
        let mut cursor = at + length;
        loop {
            let end = table[cursor..]
                .iter()
                .position(|&b| b == 0)
                .map_or(table.len(), |n| cursor + n);
            if end == cursor {
                cursor += 1;
                break;
            }
            strings.push(&table[cursor..end]);
            cursor = end + 1;
            if cursor >= table.len() {
                break;
            }
        }
        if strings.is_empty() {
            // An empty string set is still terminated by two NULs.
            cursor = (at + length + 2).min(table.len());
        } else if table.get(cursor - 1) == Some(&0) && cursor < table.len() && table[cursor] == 0 {
            cursor += 1;
        }
        out.push(Structure {
            kind,
            data,
            strings,
        });
        if kind == 127 {
            break;
        }
        at = cursor;
    }
    out
}

fn memory_type(code: u8) -> &'static str {
    match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x20 => "HBM",
        0x21 => "HBM2",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        0x24 => "HBM3",
        _ => "",
    }
}

fn form_factor(code: u8) -> &'static str {
    match code {
        0x09 => "DIMM",
        0x0D => "SODIMM",
        0x0F => "FB-DIMM",
        0x10 => "Die",
        0x11 => "CAMM",
        _ => "",
    }
}

fn error_correction(code: u8) -> &'static str {
    match code {
        3 => "None",
        4 => "Parity",
        5 => "Single-bit ECC",
        6 => "Multi-bit ECC",
        7 => "CRC",
        _ => "Unknown",
    }
}

fn cpuid(leaf: u32, sub: u32) -> [u32; 4] {
    let r = core::arch::x86_64::__cpuid_count(leaf, sub);
    [r.eax, r.ebx, r.ecx, r.edx]
}

fn cpu_features() -> Vec<&'static str> {
    let mut out = Vec::new();
    macro_rules! feature {
        ($($name:tt => $label:expr),* $(,)?) => {
            $(if std::arch::is_x86_feature_detected!($name) { out.push($label); })*
        };
    }
    feature! {
        "sse4.2" => "SSE4.2", "avx" => "AVX", "avx2" => "AVX2", "fma" => "FMA3", "f16c" => "F16C",
        "bmi2" => "BMI2", "aes" => "AES-NI", "pclmulqdq" => "CLMUL", "sha" => "SHA", "vaes" => "VAES",
        "rdrand" => "RDRAND", "rdseed" => "RDSEED", "adx" => "ADX", "popcnt" => "POPCNT",
        "avx512f" => "AVX-512F", "avx512bw" => "AVX-512BW", "avx512vl" => "AVX-512VL",
        "avx512vnni" => "AVX-512 VNNI", "avx512bf16" => "AVX-512 BF16", "avx512vp2intersect" => "AVX-512 VP2INTERSECT",
        "gfni" => "GFNI",
    }
    out
}

fn microcode() -> Option<String> {
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

pub fn hardware_info() -> HardwareInfo {
    let topology = topology();
    let mut cpu = CpuInfo::default();

    let [max_leaf, b, c, d] = cpuid(0, 0);
    let vendor: Vec<u8> = [b, d, c].iter().flat_map(|r| r.to_le_bytes()).collect();
    cpu.vendor = String::from_utf8_lossy(&vendor).to_string();
    if max_leaf >= 1 {
        let [eax, _, ecx, _] = cpuid(1, 0);
        let (base_family, base_model) = ((eax >> 8) & 0xF, (eax >> 4) & 0xF);
        let (ext_family, ext_model) = ((eax >> 20) & 0xFF, (eax >> 16) & 0xF);
        cpu.family = if base_family == 0xF {
            base_family + ext_family
        } else {
            base_family
        };
        cpu.model = if base_family == 0x6 || base_family == 0xF {
            (ext_model << 4) | base_model
        } else {
            base_model
        };
        cpu.stepping = eax & 0xF;
        cpu.hypervisor_present = ecx & (1 << 31) != 0;
    }
    if cpuid(0x8000_0000, 0)[0] >= 0x8000_0004 {
        let brand: Vec<u8> = (0x8000_0002..=0x8000_0004u32)
            .flat_map(|leaf| cpuid(leaf, 0))
            .flat_map(|r| r.to_le_bytes())
            .collect();
        cpu.brand = String::from_utf8_lossy(&brand)
            .trim_matches(char::from(0))
            .trim()
            .to_string();
    }
    cpu.features = cpu_features();
    cpu.virtualization_enabled =
        unsafe { IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED) }.as_bool();
    cpu.microcode = microcode();
    cpu.threads = topology.cpus.len();
    cpu.cores = topology.cores;
    let mut nodes: Vec<u32> = topology.cpus.iter().map(|c| c.numa).collect();
    nodes.sort_unstable();
    nodes.dedup();
    cpu.numa_nodes = nodes.len();
    cpu.base_mhz = nominal_mhz(topology.cpus.len())
        .first()
        .copied()
        .unwrap_or(0);
    for (level, name) in [(1u8, "L1 data"), (2, "L2"), (3, "L3")] {
        let caches: Vec<_> = topology
            .caches
            .iter()
            .filter(|c| c.level == level)
            .collect();
        if let Some(first) = caches.first() {
            cpu.caches
                .push((name.to_string(), first.size, caches.len()));
        }
    }

    let mut memory = MemoryInfo::default();
    let mut installed_kb = 0u64;
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut installed_kb) }.is_ok() {
        memory.installed = installed_kb * 1024;
    }
    let mut board = BoardInfo::default();
    let raw = smbios();
    for s in structures(&raw) {
        match s.kind {
            0 => {
                board.bios_vendor = s.string(0x04);
                board.bios_version = s.string(0x05);
                board.bios_date = s.string(0x08);
            }
            2 => {
                board.manufacturer = s.string(0x04);
                board.product = s.string(0x05);
            }
            4 => {
                cpu.sockets += 1;
                if cpu.socket.is_empty() {
                    cpu.socket = s.string(0x04);
                }
                cpu.bus_mhz = s.u16(0x12).unwrap_or(0) as u32;
                cpu.max_mhz = cpu.max_mhz.max(s.u16(0x14).unwrap_or(0) as u32);
                cpu.voltage = s
                    .u8(0x11)
                    .and_then(|v| (v & 0x80 != 0).then(|| (v & 0x7F) as f32 / 10.0));
            }
            16 => {
                memory.error_correction = error_correction(s.u8(0x06).unwrap_or(0));
                memory.max_capacity = match s.u32(0x07) {
                    Some(0x8000_0000) => s.u64(0x0F).unwrap_or(0),
                    Some(kb) => kb as u64 * 1024,
                    None => 0,
                };
            }
            17 => {
                memory.slots += 1;
                let size = match s.u16(0x0C).unwrap_or(0) {
                    0 | 0xFFFF => 0,
                    0x7FFF => (s.u32(0x1C).unwrap_or(0) & 0x7FFF_FFFF) as u64 * 1024 * 1024,
                    v if v & 0x8000 != 0 => (v & 0x7FFF) as u64 * 1024,
                    v => v as u64 * 1024 * 1024,
                };
                if size == 0 {
                    continue;
                }
                memory.modules.push(MemoryModule {
                    locator: s.string(0x10),
                    bank: s.string(0x11),
                    size,
                    kind: memory_type(s.u8(0x12).unwrap_or(0)),
                    form_factor: form_factor(s.u8(0x0E).unwrap_or(0)),
                    speed: s.u16(0x15).unwrap_or(0) as u32,
                    configured_speed: s.u16(0x20).unwrap_or(0) as u32,
                    manufacturer: s.string(0x17),
                    part_number: s.string(0x1A),
                    serial: s.string(0x18),
                    rank: s.u8(0x1B).unwrap_or(0) & 0x0F,
                    voltage_mv: s.u16(0x26).unwrap_or(0),
                });
            }
            _ => {}
        }
    }
    HardwareInfo {
        cpu,
        memory,
        board,
        topology,
    }
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
