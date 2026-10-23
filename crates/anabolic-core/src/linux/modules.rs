// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The files a process has mapped (its executable and shared libraries),
//! from /proc/<pid>/maps, and the kernel's modules from /proc/modules.

use std::collections::HashMap;
use std::sync::Arc;

use crate::util::DeviceMap;

use super::*;

/// Every file mapped executable into `pid`, lowest address first: the
/// program and its shared libraries. Needs the right to look into it.
pub fn process_modules(pid: u32) -> Result<Vec<ModuleInfo>, String> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps"))
        .map_err(|e| crate::util::error_text(&e))?;
    // path -> (lowest start, highest end, executable)
    let mut spans: HashMap<&str, (u64, u64, bool)> = HashMap::new();
    let mut order = Vec::new();
    for line in maps.lines() {
        // start-end perms offset dev inode path
        let mut fields = line.splitn(6, ' ');
        let (Some(range), Some(perms)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Some(path) = fields.nth(3).map(str::trim_start) else {
            continue;
        };
        if !path.starts_with('/') {
            continue;
        }
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16))
        else {
            continue;
        };
        let span = spans.entry(path).or_insert_with(|| {
            order.push(path);
            (start, end, false)
        });
        span.0 = span.0.min(start);
        span.1 = span.1.max(end);
        span.2 |= perms.contains('x');
    }
    let mut out: Vec<ModuleInfo> = order
        .into_iter()
        .filter_map(|path| {
            let (base, end, exec) = spans[path];
            exec.then(|| ModuleInfo {
                base,
                size: end - base,
                name: Arc::from(path.rsplit('/').next().unwrap_or(path)),
                path: Arc::from(path.trim_end_matches(" (deleted)")),
            })
        })
        .collect();
    out.sort_by_key(|m| m.base);
    Ok(out)
}

/// The kernel's loaded modules (drivers). Their addresses read as zero
/// unless the kernel shows them to this process (kptr_restrict).
pub fn kernel_modules(_devices: &DeviceMap) -> Vec<ModuleInfo> {
    let Ok(text) = std::fs::read_to_string("/proc/modules") else {
        return Vec::new();
    };
    let mut out: Vec<ModuleInfo> = text
        .lines()
        .filter_map(|line| {
            // name size refcount users state address
            let f: Vec<&str> = line.split_ascii_whitespace().collect();
            let name = *f.first()?;
            let size = f.get(1)?.parse().ok()?;
            let base = u64::from_str_radix(f.get(5)?.trim_start_matches("0x"), 16).unwrap_or(0);
            Some(ModuleInfo {
                base,
                size,
                name: Arc::from(name),
                path: Arc::from(format!("/sys/module/{name}")),
            })
        })
        .collect();
    out.sort_by_key(|m| m.base);
    out
}

/// Names code addresses from the symbol tables of the files they lie in:
/// `libc.so.6!__futex_abstimed_wait_common+0x12`, else `libc.so.6+0x8e4f2`.
/// Distributions strip their libraries of everything but the exported
/// functions, so a private function is named after the export before it
/// only when that is close; otherwise by offset.
#[derive(Default)]
pub struct Symbolizer {
    images: HashMap<Arc<str>, Option<Arc<crate::elf::Image>>>,
}

impl Symbolizer {
    pub fn name(&mut self, modules: &[ModuleInfo], address: u64) -> Option<String> {
        let m = module_at(modules, address)?;
        let image = self
            .images
            .entry(m.path.clone())
            .or_insert_with(|| crate::elf::read(&m.path).map(Arc::new))
            .clone();
        let offset = address - m.base;
        let named = image.and_then(|image| {
            let (symbol, into) = image.function_at(offset.wrapping_add(image.first_vaddr))?;
            Some(match into {
                0 => format!("{}!{}", m.name, symbol.name),
                into => format!("{}!{}+0x{into:X}", m.name, symbol.name),
            })
        });
        Some(named.unwrap_or_else(|| format!("{}+0x{offset:X}", m.name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_its_own_functions() {
        let modules = process_modules(std::process::id()).unwrap();
        let me = names_its_own_functions as *const () as u64;
        let name = Symbolizer::default().name(&modules, me + 3).unwrap();
        assert!(name.contains("names_its_own_functions+0x3"), "{name}");
    }

    #[test]
    fn lists_own_program_and_libc() {
        let modules = process_modules(std::process::id()).expect("own maps");
        assert!(modules.iter().any(|m| m.name.contains("libc")));
        assert!(modules.windows(2).all(|w| w[0].base <= w[1].base));
        let first = &modules[0];
        assert!(module_at(&modules, first.base + 1).is_some());
    }
}
