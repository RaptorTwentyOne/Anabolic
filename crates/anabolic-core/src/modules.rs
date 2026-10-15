//! Loaded modules of a process (user mode) and of the kernel (drivers), and
//! turning code addresses into `module!export+offset` without symbol files:
//! the nearest export from the image's own export table on disk.

use std::collections::HashMap;
use std::sync::Arc;

use windows::Win32::Foundation::{ERROR_BAD_LENGTH, ERROR_PARTIAL_COPY};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};

use crate::ntapi;
use crate::util::{OwnedHandle, from_wide};
use crate::winfile::DeviceMap;

#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub base: u64,
    pub size: u64,
    pub name: Arc<str>,
    pub path: Arc<str>,
}

/// Modules mapped into a process, sorted by base address. Opens the process
/// for reading (Toolhelp does), so only on request.
pub fn process_modules(pid: u32) -> Result<Vec<ModuleInfo>, String> {
    // The snapshot fails transiently while the loader is busy.
    let mut last_error = String::new();
    for _ in 0..5 {
        match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) } {
            Ok(h) => {
                let snapshot = OwnedHandle(h);
                let mut entry = MODULEENTRY32W {
                    dwSize: size_of::<MODULEENTRY32W>() as u32,
                    ..Default::default()
                };
                let mut out = Vec::new();
                let mut ok = unsafe { Module32FirstW(snapshot.raw(), &mut entry) }.is_ok();
                while ok {
                    out.push(ModuleInfo {
                        base: entry.modBaseAddr as u64,
                        size: entry.modBaseSize as u64,
                        name: Arc::from(from_wide(&entry.szModule)),
                        path: Arc::from(from_wide(&entry.szExePath)),
                    });
                    ok = unsafe { Module32NextW(snapshot.raw(), &mut entry) }.is_ok();
                }
                out.sort_by_key(|m| m.base);
                return Ok(out);
            }
            Err(e)
                if e.code() == ERROR_BAD_LENGTH.to_hresult()
                    || e.code() == ERROR_PARTIAL_COPY.to_hresult() =>
            {
                last_error = crate::util::error_text(&e);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(crate::util::error_text(&e)),
        }
    }
    Err(last_error)
}

/// Kernel image and drivers, sorted by base. Needs SeDebugPrivilege for the
/// addresses (they read as zero otherwise).
pub fn kernel_modules(devices: &DeviceMap) -> Vec<ModuleInfo> {
    let mut buf = vec![0u64; 64 * 1024];
    let mut returned = 0u32;
    let status = unsafe {
        ntapi::NtQuerySystemInformation(
            ntapi::SystemModuleInformation,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut returned,
        )
    };
    if !ntapi::nt_success(status) {
        return Vec::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
    let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    // RTL_PROCESS_MODULE_INFORMATION is 296 bytes, starting at offset 8.
    const ENTRY: usize = 296;
    let mut out: Vec<ModuleInfo> = (0..count)
        .filter_map(|i| {
            let e = bytes.get(8 + i * ENTRY..8 + (i + 1) * ENTRY)?;
            let base = u64::from_le_bytes(e[16..24].try_into().unwrap());
            let size = u32::from_le_bytes(e[24..28].try_into().unwrap()) as u64;
            let name_offset = u16::from_le_bytes(e[38..40].try_into().unwrap()) as usize;
            let full = &e[40..296];
            let end = full.iter().position(|&b| b == 0).unwrap_or(full.len());
            let nt_path = String::from_utf8_lossy(&full[..end]).to_string();
            let name = String::from_utf8_lossy(&full[name_offset.min(end)..end]).to_string();
            let path = devices.to_dos_path(&nt_path).unwrap_or(nt_path);
            (base != 0).then(|| ModuleInfo {
                base,
                size,
                name: Arc::from(name),
                path: Arc::from(path),
            })
        })
        .collect();
    out.sort_by_key(|m| m.base);
    out
}

/// The module containing `address`, by binary search over sorted modules.
pub fn module_at(modules: &[ModuleInfo], address: u64) -> Option<&ModuleInfo> {
    let i = modules
        .partition_point(|m| m.base <= address)
        .checked_sub(1)?;
    let m = &modules[i];
    (address < m.base + m.size).then_some(m)
}

/// Exported names of a PE image on disk, by RVA (sorted).
type Exports = Arc<Vec<(u32, Arc<str>)>>;

fn read_exports(path: &str) -> Exports {
    let Ok(data) = std::fs::read(path) else {
        return Arc::default();
    };
    Arc::new(parse_exports(&data).unwrap_or_default())
}

fn parse_exports(d: &[u8]) -> Option<Vec<(u32, Arc<str>)>> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?));
    if d.get(0..2)? != b"MZ" {
        return None;
    }
    let nt = u32_at(0x3C)? as usize;
    if d.get(nt..nt + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(nt + 6)? as usize;
    let optional_size = u16_at(nt + 20)? as usize;
    let optional = nt + 24;
    let magic = u16_at(optional)?;
    // Data directory 0 (exports) sits at a different offset in PE32 and PE32+.
    let directory = optional + if magic == 0x20B { 112 } else { 96 };
    let (export_rva, _) = (u32_at(directory)?, u32_at(directory + 4)?);
    if export_rva == 0 {
        return Some(Vec::new());
    }
    let section_table = optional + optional_size;
    let to_offset = |rva: u32| -> Option<usize> {
        (0..sections).find_map(|i| {
            let s = section_table + i * 40;
            let (vsize, vaddr, raw_size, raw_ptr) = (
                u32_at(s + 8)?,
                u32_at(s + 12)?,
                u32_at(s + 16)?,
                u32_at(s + 20)?,
            );
            let span = vsize.max(raw_size);
            (rva >= vaddr && rva < vaddr + span).then(|| (rva - vaddr + raw_ptr) as usize)
        })
    };
    let dir = to_offset(export_rva)?;
    let names = u32_at(dir + 24)? as usize;
    let functions = to_offset(u32_at(dir + 28)?)?;
    let name_table = to_offset(u32_at(dir + 32)?)?;
    let ordinals = to_offset(u32_at(dir + 36)?)?;
    let mut out: Vec<(u32, Arc<str>)> = Vec::with_capacity(names);
    for i in 0..names.min(100_000) {
        let name_offset = to_offset(u32_at(name_table + i * 4)?)?;
        let ordinal = u16_at(ordinals + i * 2)? as usize;
        let rva = u32_at(functions + ordinal * 4)?;
        let end = d[name_offset..]
            .iter()
            .position(|&b| b == 0)
            .map_or(d.len(), |n| name_offset + n);
        let name = String::from_utf8_lossy(&d[name_offset..end]);
        out.push((rva, Arc::from(name.as_ref())));
    }
    // Aliases share an address (NtX/ZwX); keep the conventional name.
    out.sort_by_key(|(rva, name)| (*rva, name.starts_with("Zw")));
    out.dedup_by_key(|(rva, _)| *rva);
    Some(out)
}

/// Resolves addresses to names, caching export tables per image path.
#[derive(Default)]
pub struct Symbolizer {
    exports: HashMap<Arc<str>, Exports>,
}

impl Symbolizer {
    pub fn name(&mut self, modules: &[ModuleInfo], address: u64) -> Option<String> {
        let m = module_at(modules, address)?;
        let offset = address - m.base;
        let exports = self
            .exports
            .entry(m.path.clone())
            .or_insert_with(|| read_exports(&m.path))
            .clone();
        let i = exports.partition_point(|(rva, _)| (*rva as u64) <= offset);
        // Debugger style: the module without its extension.
        let module = m.name.rsplit_once('.').map_or(&*m.name, |(stem, _)| stem);
        Some(match i.checked_sub(1).map(|i| &exports[i]) {
            // Only trust exports reasonably close; otherwise it is some private function.
            Some((rva, export)) if offset - *rva as u64 <= 0x1000 => match offset - *rva as u64 {
                0 => format!("{module}!{export}"),
                delta => format!("{module}!{export}+0x{delta:X}"),
            },
            _ => format!("{module}+0x{offset:X}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_modules_and_exports() {
        let modules = process_modules(std::process::id()).unwrap();
        let ntdll = modules
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("ntdll.dll"))
            .expect("ntdll not listed");
        // An export resolves to its own name.
        let proc_addr = unsafe {
            windows::Win32::System::LibraryLoader::GetProcAddress(
                windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!(
                    "ntdll.dll"
                ))
                .unwrap(),
                windows::core::s!("NtQuerySystemInformation"),
            )
        }
        .unwrap() as usize as u64;
        assert!(proc_addr >= ntdll.base && proc_addr < ntdll.base + ntdll.size);
        let mut symbols = Symbolizer::default();
        assert_eq!(
            symbols.name(&modules, proc_addr).as_deref(),
            Some("ntdll!NtQuerySystemInformation")
        );
        assert!(
            symbols
                .name(&modules, proc_addr + 4)
                .unwrap()
                .ends_with("+0x4")
        );
    }

    #[test]
    fn kernel_drivers_are_listed() {
        let _ = crate::hardening::enable_privilege(windows::core::w!("SeDebugPrivilege"));
        let kernel = kernel_modules(&DeviceMap::build());
        if kernel.is_empty() {
            eprintln!("kernel addresses hidden (not elevated?)");
            return;
        }
        assert!(
            kernel[0].name.to_lowercase().contains("ntoskrnl")
                || kernel
                    .iter()
                    .any(|m| m.name.to_lowercase().contains("ntoskrnl"))
        );
        assert!(kernel.iter().all(|m| m.size > 0));
    }
}
