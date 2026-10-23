// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What a Linux program file is: the package that vouches for it (and
//! whether it is still as the package installed it), its SHA-256, and how
//! it was built, from its ELF headers.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::Instant;

use sha2::{Digest, Sha256};

use super::*;

pub fn file_facts(path: &str) -> Result<FileFacts, String> {
    let started = Instant::now();
    let meta = std::fs::metadata(path).map_err(|e| crate::util::error_text(&e))?;
    let modified = meta.modified().ok().and_then(|t| {
        let unix = t.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(crate::util::filetime_from_unix(unix.as_secs_f64()))
    });
    let sha256 = sha256(path);
    let elf = File::open(path).ok().and_then(|f| elf_facts(&f));
    Ok(FileFacts {
        path: Arc::from(path),
        size: meta.len(),
        modified,
        signature: package_signature(path, sha256.as_deref()),
        sha256,
        pe: None,
        elf,
        took: started.elapsed(),
    })
}

pub fn sha256_of(mut data: impl Read) -> Option<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = data.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Linux files carry no signature of their own: the package manager's
/// record of them is what vouches for them.
pub fn signature(path: &str) -> Signature {
    let hash = sha256(path);
    package_signature(path, hash.as_deref())
}

fn package_signature(path: &str, hash: Option<&str>) -> Signature {
    match crate::packages::owner(path) {
        Some(p) => Signature::Package {
            manager: p.manager,
            name: p.name.clone(),
            version: p.version.clone(),
            verified: hash.and_then(|h| crate::packages::verify(path, &p, h)),
        },
        None => Signature::Unsigned,
    }
}

fn machine_name(machine: u16) -> &'static str {
    match machine {
        3 => "x86",
        8 => "MIPS",
        20 => "PowerPC",
        21 => "PowerPC64",
        40 => "ARM",
        62 => "x86-64",
        183 => "ARM64",
        243 => "RISC-V",
        258 => "LoongArch",
        _ => "unknown machine",
    }
}

/// A little-endian ELF file, 32 or 64 bits: the parts of it read here.
struct Elf<'a> {
    file: &'a File,
    wide: bool,
}

impl Elf<'_> {
    fn bytes(&self, offset: u64, len: u64) -> Option<Vec<u8>> {
        // Header tables and string tables; nothing large is read whole.
        if len > 64 << 20 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        self.file.read_exact_at(&mut buf, offset).ok()?;
        Some(buf)
    }

    /// A field that is 4 bytes in 32-bit files and 8 in 64-bit ones.
    fn word(&self, d: &[u8], at32: usize, at64: usize) -> Option<u64> {
        if self.wide {
            Some(u64::from_le_bytes(d.get(at64..at64 + 8)?.try_into().ok()?))
        } else {
            Some(u32::from_le_bytes(d.get(at32..at32 + 4)?.try_into().ok()?) as u64)
        }
    }
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const PT_GNU_STACK: u32 = 0x6474_e551;
const PT_GNU_RELRO: u32 = 0x6474_e552;
const PF_X: u32 = 1;

const DT_SONAME: u64 = 14;
const DT_BIND_NOW: u64 = 24;
const DT_FLAGS: u64 = 30;
const DT_FLAGS_1: u64 = 0x6fff_fffb;
const DF_BIND_NOW: u64 = 0x8;
const DF_1_NOW: u64 = 0x1;
const DF_1_PIE: u64 = 0x0800_0000;

const NT_GNU_BUILD_ID: u32 = 3;
const NT_GNU_PROPERTY_TYPE_0: u32 = 5;
const GNU_PROPERTY_X86_FEATURE_1_AND: u32 = 0xc000_0002;

/// The notes of one PT_NOTE segment: (type, name, description).
fn notes(d: &[u8], align: usize) -> Vec<(u32, &[u8], &[u8])> {
    let pad = |n: usize| n.div_ceil(align) * align;
    let mut out = Vec::new();
    let mut at = 0;
    while let (Some(namesz), Some(descsz), Some(kind)) =
        (u32_at(d, at), u32_at(d, at + 4), u32_at(d, at + 8))
    {
        // Name and description each start aligned, counted from the
        // segment's start.
        let name_at = at + 12;
        let desc_at = pad(name_at + namesz as usize);
        let (Some(name), Some(desc)) = (
            d.get(name_at..name_at + namesz as usize),
            d.get(desc_at..desc_at + descsz as usize),
        ) else {
            break;
        };
        out.push((kind, name, desc));
        at = pad(desc_at + descsz as usize);
    }
    out
}

/// How the file was built, from its program headers, dynamic section,
/// notes and symbol names. None for a file that is not a little-endian ELF.
fn elf_facts(file: &File) -> Option<ElfFacts> {
    let mut head = [0u8; 64];
    file.read_exact_at(&mut head, 0).ok()?;
    if head[..4] != *b"\x7fELF" || head[5] != 1 {
        return None;
    }
    let elf = Elf {
        file,
        wide: head[4] == 2,
    };
    let kind = u16_at(&head, 16)?;
    let mut facts = ElfFacts {
        machine: machine_name(u16_at(&head, 18)?),
        ..Default::default()
    };
    let (phoff, shoff) = (elf.word(&head, 28, 32)?, elf.word(&head, 32, 40)?);
    let at = |a32: usize, a64: usize| if elf.wide { a64 } else { a32 };
    let (phentsize, phnum) = (u16_at(&head, at(42, 54))?, u16_at(&head, at(44, 56))?);
    let (shentsize, shnum, shstrndx) = (
        u16_at(&head, at(46, 58))?,
        u16_at(&head, at(48, 60))?,
        u16_at(&head, at(50, 62))?,
    );

    let mut flags_1 = 0u64;
    let mut soname = false;
    let mut has_interp = false;
    let mut stack_seen = false;
    let phdrs = elf.bytes(phoff, phentsize as u64 * phnum as u64)?;
    for ph in phdrs.chunks_exact(phentsize.max(1) as usize) {
        let kind = u32_at(ph, 0)?;
        let flags = u32_at(ph, at(24, 4))?;
        let (offset, filesz, align) = (
            elf.word(ph, 4, 8)?,
            elf.word(ph, 16, 32)?,
            elf.word(ph, 28, 48)?,
        );
        match kind {
            PT_INTERP => {
                has_interp = true;
                facts.interpreter = elf.bytes(offset, filesz).map(|b| {
                    String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or(&[])).into_owned()
                });
            }
            PT_GNU_STACK => {
                stack_seen = true;
                facts.nx_stack = flags & PF_X == 0;
            }
            PT_GNU_RELRO => facts.relro = true,
            PT_DYNAMIC => {
                let dynamic = elf.bytes(offset, filesz)?;
                let entry = if elf.wide { 16 } else { 8 };
                for d in dynamic.chunks_exact(entry) {
                    let (tag, value) = (elf.word(d, 0, 0)?, elf.word(d, 4, 8)?);
                    match tag {
                        DT_SONAME => soname = true,
                        DT_BIND_NOW => facts.bind_now = true,
                        DT_FLAGS if value & DF_BIND_NOW != 0 => facts.bind_now = true,
                        DT_FLAGS_1 => {
                            flags_1 = value;
                            facts.bind_now |= value & DF_1_NOW != 0;
                        }
                        _ => {}
                    }
                }
            }
            PT_NOTE => {
                let Some(d) = elf.bytes(offset, filesz) else {
                    continue;
                };
                let align = if align == 8 { 8 } else { 4 };
                for (kind, name, desc) in notes(&d, align) {
                    if name != b"GNU\0" {
                        continue;
                    }
                    match kind {
                        NT_GNU_BUILD_ID => {
                            facts.build_id =
                                Some(desc.iter().map(|b| format!("{b:02x}")).collect());
                        }
                        NT_GNU_PROPERTY_TYPE_0 => {
                            let mut p = 0;
                            while let (Some(t), Some(size)) = (u32_at(desc, p), u32_at(desc, p + 4))
                            {
                                if t == GNU_PROPERTY_X86_FEATURE_1_AND {
                                    // Indirect branch tracking and shadow stack.
                                    facts.cet = u32_at(desc, p + 8).is_some_and(|f| f & 3 == 3);
                                }
                                p += 8 + (size as usize).div_ceil(align) * align;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    // Without the header the stack was executable (old toolchains).
    facts.nx_stack &= stack_seen;
    const ET_DYN: u16 = 3;
    facts.pie = kind == ET_DYN && (flags_1 & DF_1_PIE != 0 || (has_interp && !soname));
    facts.library = kind == ET_DYN && !facts.pie;
    facts.static_linked = !has_interp && !facts.library;

    // Section names: a symbol table, and the names of the symbols the
    // program uses (stack canaries, the checked _FORTIFY_SOURCE functions).
    let shdrs = elf
        .bytes(shoff, shentsize as u64 * shnum as u64)
        .unwrap_or_default();
    let sections: Vec<&[u8]> = shdrs.chunks_exact(shentsize.max(1) as usize).collect();
    let section = |i: usize| -> Option<(u32, u64, u64)> {
        let s = sections.get(i)?;
        Some((u32_at(s, 0)?, elf.word(s, 16, 24)?, elf.word(s, 20, 32)?))
    };
    let names = section(shstrndx as usize).and_then(|(_, offset, size)| elf.bytes(offset, size));
    let name_of = |i: usize| -> Option<&str> {
        let (name, ..) = section(i)?;
        let names = names.as_deref()?;
        let rest = names.get(name as usize..)?;
        std::str::from_utf8(&rest[..rest.iter().position(|&b| b == 0)?]).ok()
    };
    let named = |wanted: &str| (0..sections.len()).find(|&i| name_of(i) == Some(wanted));
    facts.stripped = named(".symtab").is_none();
    let symbols = named(".dynstr")
        .or_else(|| named(".strtab"))
        .and_then(section)
        .and_then(|(_, offset, size)| elf.bytes(offset, size))
        .unwrap_or_default();
    for name in symbols.split(|&b| b == 0) {
        if name == b"__stack_chk_fail" || name == b"__stack_chk_guard" {
            facts.stack_protector = true;
        } else if name.starts_with(b"__") && name.ends_with(b"_chk") {
            facts.fortify = true;
        }
    }
    Some(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_sha256sum() {
        assert_eq!(
            sha256_of(&b"abc"[..]).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn reads_notes_aligned_to_eight() {
        // A GNU property note as 64-bit linkers write it: the description
        // starts 8-aligned right after the name.
        let mut note = Vec::new();
        for word in [4u32, 16, NT_GNU_PROPERTY_TYPE_0] {
            note.extend(word.to_le_bytes());
        }
        note.extend(b"GNU\0");
        for word in [GNU_PROPERTY_X86_FEATURE_1_AND, 4, 3, 0] {
            note.extend(word.to_le_bytes());
        }
        let found = notes(&note, 8);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, b"GNU\0");
        assert_eq!(u32_at(found[0].2, 0), Some(GNU_PROPERTY_X86_FEATURE_1_AND));
        assert_eq!(u32_at(found[0].2, 8), Some(3));
    }

    #[test]
    fn describes_a_system_program() {
        let started = Instant::now();
        let facts = file_facts("/usr/bin/ls").expect("ls is readable");
        eprintln!("facts of ls in {:?}", started.elapsed());
        let elf = facts.elf.expect("ls is an ELF file");
        assert_eq!(elf.machine, "x86-64");
        assert!(!elf.library && !elf.static_linked);
        assert!(elf.pie && elf.relro && elf.nx_stack);
        assert!(
            elf.interpreter
                .as_deref()
                .is_some_and(|i| i.contains("ld-linux"))
        );
        assert!(elf.build_id.as_deref().is_some_and(|b| b.len() >= 16));
        assert_eq!(facts.sha256.as_ref().map(String::len), Some(64));
        if let Signature::Package { name, .. } = &facts.signature {
            assert!(name.contains("coreutils"), "{name}");
        }
    }

    #[test]
    fn tells_a_library_from_a_program() {
        let Some(libc) = ["/usr/lib/libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"]
            .iter()
            .find(|p| std::path::Path::new(p).exists())
        else {
            return;
        };
        let elf = elf_facts(&File::open(libc).unwrap()).unwrap();
        assert!(elf.library && !elf.pie, "{elf:?}");
        assert!(elf_facts(&File::open("/etc/os-release").unwrap()).is_none());
    }
}
