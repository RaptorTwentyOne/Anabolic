//! Just enough of ELF to name code addresses: the load segments (where a
//! mapped file's addresses come from) and the symbol tables. 64-bit,
//! little-endian files only, as x86-64 and ARM64 Linux run them.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

/// Tables larger than this are not read (a program with all its debug
/// symbols can carry hundreds of MB of them).
const MAX_TABLE: u64 = 256 * 1024 * 1024;

const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_DYNSYM: u32 = 11;
const STT_FUNC: u8 = 2;
const STT_GNU_IFUNC: u8 = 10;

/// A function symbol: its address in the file's own address space, its
/// size (0 when unknown) and its name, demangled.
#[derive(Clone, Debug)]
pub struct Symbol {
    pub value: u64,
    pub size: u64,
    pub name: Arc<str>,
}

/// What is read of one file.
#[derive(Debug, Default)]
pub struct Image {
    /// The address the file's first byte gets in its own address space:
    /// the first load segment's address less its file offset. A mapping
    /// of the file at `base` puts file address `a` at `base + a - this`.
    pub first_vaddr: u64,
    /// Functions by address.
    pub symbols: Vec<Symbol>,
}

impl Image {
    /// The function holding `vaddr` (an address in the file's own space),
    /// and how far into it.
    pub fn function_at(&self, vaddr: u64) -> Option<(&Symbol, u64)> {
        let i = self.symbols.partition_point(|s| s.value <= vaddr);
        let s = &self.symbols[i.checked_sub(1)?];
        let offset = vaddr - s.value;
        // Without a size, only an address close after it is trusted.
        let inside = if s.size > 0 {
            offset < s.size
        } else {
            offset < 0x1000
        };
        inside.then_some((s, offset))
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn read_at(file: &File, offset: u64, len: u64) -> Option<Vec<u8>> {
    if len > MAX_TABLE {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    file.read_exact_at(&mut buf, offset).ok()?;
    Some(buf)
}

/// Reads the load segments and function symbols of an ELF file; None for
/// anything else (a script, a 32-bit file, a file that is gone).
pub fn read(path: &str) -> Option<Image> {
    let file = File::open(path).ok()?;
    let mut header = [0u8; 64];
    file.read_exact_at(&mut header, 0).ok()?;
    // Magic, 64-bit, little-endian.
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let (phoff, shoff) = (u64_at(&header, 32), u64_at(&header, 40));
    let (phentsize, phnum) = (u16_at(&header, 54) as u64, u16_at(&header, 56) as u64);
    let (shentsize, shnum) = (u16_at(&header, 58) as u64, u16_at(&header, 60) as u64);

    let mut image = Image::default();
    if phentsize >= 56 {
        let table = read_at(&file, phoff, phentsize * phnum)?;
        image.first_vaddr = (0..phnum as usize)
            .map(|i| &table[i * phentsize as usize..])
            .filter(|p| u32_at(p, 0) == PT_LOAD)
            .min_by_key(|p| u64_at(p, 8))
            .map_or(0, |p| u64_at(p, 16).wrapping_sub(u64_at(p, 8)));
    }
    if shentsize < 64 || shnum == 0 {
        return Some(image);
    }
    let sections = read_at(&file, shoff, shentsize * shnum)?;
    let section = |i: usize| &sections[i * shentsize as usize..];
    // The full symbol table where it was kept, else the exported ones.
    let table = [SHT_SYMTAB, SHT_DYNSYM]
        .iter()
        .find_map(|&kind| (0..shnum as usize).find(|&i| u32_at(section(i), 4) == kind));
    let Some(table) = table else {
        return Some(image);
    };
    let sym = section(table);
    let strings = section(u32_at(sym, 40) as usize);
    let entries = read_at(&file, u64_at(sym, 24), u64_at(sym, 32))?;
    let names = read_at(&file, u64_at(strings, 24), u64_at(strings, 32))?;
    for e in entries.as_chunks::<24>().0 {
        let kind = e[4] & 0xF;
        let defined = u16_at(e, 6) != 0;
        let value = u64_at(e, 8);
        if !(kind == STT_FUNC || kind == STT_GNU_IFUNC) || !defined || value == 0 {
            continue;
        }
        let start = u32_at(e, 0) as usize;
        let Some(raw) = names.get(start..) else {
            continue;
        };
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name = String::from_utf8_lossy(&raw[..end]);
        image.symbols.push(Symbol {
            value,
            size: u64_at(e, 16),
            name: Arc::from(demangle(&name)),
        });
    }
    image.symbols.sort_by_key(|s| s.value);
    image.symbols.dedup_by_key(|s| s.value);
    Some(image)
}

/// A mangled name readable: Rust's legacy scheme and C++'s nested names
/// become `a::b::c` (C++ without its parameters); anything else stays as
/// it is.
pub fn demangle(name: &str) -> String {
    let Some(rest) = name.strip_prefix("_ZN") else {
        return name.to_string();
    };
    let bytes = rest.as_bytes();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'E' {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // Templates, qualifiers, substitutions: past what this reads.
        let Ok(len) = rest[start..i].parse::<usize>() else {
            return name.to_string();
        };
        let Some(part) = rest.get(i..i + len) else {
            return name.to_string();
        };
        parts.push(part);
        i += len;
    }
    // Rust ends with the hash of the crate's build: h and 16 hex digits.
    if let Some(last) = parts.last()
        && last.len() == 17
        && last.starts_with('h')
        && last[1..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        parts.pop();
    }
    parts
        .iter()
        .map(|p| unescape(p))
        .collect::<Vec<_>>()
        .join("::")
}

/// Rust's escapes in a legacy-mangled name: `$LT$` for <, `$u20$` for a
/// space, `..` for ::.
fn unescape(part: &str) -> String {
    let part = part
        .strip_prefix("_$")
        .map_or(part.to_string(), |p| format!("${p}"));
    let mut out = String::with_capacity(part.len());
    let mut rest = part.as_str();
    while let Some(at) = rest.find(['$', '.']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("..") {
            out.push_str("::");
            rest = after;
            continue;
        }
        if rest.starts_with('.') {
            out.push('.');
            rest = &rest[1..];
            continue;
        }
        let Some(end) = rest[1..].find('$') else {
            break;
        };
        let code = &rest[1..end + 1];
        let decoded = match code {
            "LT" => Some('<'),
            "GT" => Some('>'),
            "RF" => Some('&'),
            "BP" => Some('*'),
            "C" => Some(','),
            _ => code
                .strip_prefix('u')
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 2..];
            }
            None => {
                out.push('$');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demangles_rust_and_cpp() {
        assert_eq!(
            demangle("_ZN3std2rt10lang_start17h1234567890abcdefE"),
            "std::rt::lang_start"
        );
        assert_eq!(
            demangle(
                "_ZN4core3ptr46drop_in_place$LT$alloc..vec..Vec$LT$u8$GT$$GT$17h0123456789abcdefE"
            ),
            "core::ptr::drop_in_place<alloc::vec::Vec<u8>>"
        );
        assert_eq!(demangle("_ZN3foo3barEv"), "foo::bar");
        assert_eq!(demangle("main"), "main");
        assert_eq!(demangle("_ZN3fooI3barE3bazEv"), "_ZN3fooI3barE3bazEv");
    }

    #[test]
    fn reads_its_own_symbols() {
        let exe = std::env::current_exe().unwrap();
        let image = read(&exe.to_string_lossy()).expect("own executable is ELF");
        assert!(!image.symbols.is_empty());
        assert!(image.symbols.windows(2).all(|w| w[0].value < w[1].value));
        // This very function is named.
        let me = reads_its_own_symbols as *const () as u64;
        let modules = crate::modules::process_modules(std::process::id()).unwrap();
        let module = crate::modules::module_at(&modules, me).unwrap();
        let (symbol, _) = image
            .function_at(me - module.base + image.first_vaddr)
            .expect("named");
        assert!(
            symbol.name.contains("reads_its_own_symbols"),
            "{}",
            symbol.name
        );
    }
}
