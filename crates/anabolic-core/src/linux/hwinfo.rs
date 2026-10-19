//! What the firmware and the kernel say about the hardware: the SMBIOS
//! tables (readable by root, or with the right to read any file), the
//! board names anyone may read, and the processor's frequencies.

use super::*;

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// The raw SMBIOS structure table; empty without the right to read it.
pub(super) fn smbios() -> Vec<u8> {
    std::fs::read("/sys/firmware/dmi/tables/DMI").unwrap_or_default()
}

/// Linux does not keep the firmware's figure; the modules' sizes stand in.
pub(super) fn installed_memory() -> u64 {
    0
}

/// The kernel lists vmx (Intel) or svm (AMD) only when the firmware left
/// virtualization on.
pub(super) fn virtualization_enabled() -> bool {
    std::fs::read_to_string("/proc/cpuinfo").is_ok_and(|info| {
        info.lines()
            .find(|l| l.starts_with("flags"))
            .is_some_and(|l| l.split_ascii_whitespace().any(|f| f == "vmx" || f == "svm"))
    })
}

pub(super) fn microcode() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    info.lines().find_map(|l| {
        let (key, value) = l.split_once(':')?;
        (key.trim() == "microcode").then(|| value.trim().to_string())
    })
}

/// Board and firmware from /sys/class/dmi/id, which anyone may read: what
/// is left when the tables themselves are not.
pub(super) fn board_fallback() -> Option<BoardInfo> {
    let id = |file: &str| read(&format!("/sys/class/dmi/id/{file}")).unwrap_or_default();
    let board = BoardInfo {
        manufacturer: id("board_vendor"),
        product: id("board_name"),
        bios_vendor: id("bios_vendor"),
        bios_version: id("bios_version"),
        bios_date: id("bios_date"),
    };
    (!board.manufacturer.is_empty()).then_some(board)
}

/// Nominal MHz per processor (what 100 % performance means): the base
/// frequency where the driver gives one (intel_pstate), else the highest.
pub fn nominal_mhz(cpus: usize) -> Vec<u32> {
    (0..cpus)
        .map(|cpu| {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq");
            read(&format!("{base}/base_frequency"))
                .or_else(|| read(&format!("{base}/cpuinfo_max_freq")))
                .and_then(|khz| khz.parse::<u32>().ok())
                .map_or(0, |khz| khz / 1000)
        })
        .collect()
}

/// Swap areas: partitions and files.
pub fn page_files() -> Vec<PageFile> {
    let Ok(text) = std::fs::read_to_string("/proc/swaps") else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            let kb = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
            let used = kb(3);
            Some(PageFile {
                path: f.first()?.replace("\\040", " "),
                size: kb(2),
                used,
                // The kernel keeps no peak.
                peak: used,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_machine() {
        let hw = hardware_info();
        assert!(!hw.cpu.brand.is_empty());
        assert!(hw.cpu.threads > 0 && hw.cpu.cores > 0);
        assert!(hw.cpu.base_mhz > 0);
        assert!(!hw.cpu.caches.is_empty());
        // Without the tables (not root) the board still has a name.
        assert!(!hw.board.manufacturer.is_empty());
    }
}
