// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Memory dumps on Linux: an ELF core file, as the kernel writes when a
//! program crashes and as gdb and lldb read (`gdb <program> <file>`).
//!
//! Every thread is held still (ptrace) while its registers and the memory
//! are copied, so the file is one consistent moment; there is no way to
//! clone a process the way Windows does, so the process stays held for the
//! whole write. A full dump copies every readable mapping; a small one the
//! stacks and the program's data (globals, small heaps), leaving the code,
//! which the debugger reads from the program's own files.

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Instant;

use crate::model::ProcKey;
use crate::procfs;
use crate::ptrace::{self, Mapping};

use super::*;

/// A name for the file: `name-pid-YYYYMMDD-HHMMSS.core`.
pub fn file_name(name: &str, pid: u32, kind: DumpKind) -> String {
    let stamp = crate::sysinfo::local_time(crate::sysinfo::now_filetime())
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    let small = if kind == DumpKind::Small {
        "-small"
    } else {
        ""
    };
    format!("{name}-{pid}-{stamp}{small}.core")
}

/// The keyrings: their memory holds the session's passwords and keys.
const SIGN_IN_SECRETS: [&str; 5] = [
    "gnome-keyring-daemon",
    "kwalletd5",
    "kwalletd6",
    "ssh-agent",
    "gpg-agent",
];

/// Whether this process holds sign-in secrets (a keyring).
pub fn holds_secrets(name: &str) -> bool {
    SIGN_IN_SECRETS.contains(&name)
}

/// Whether a process of this name is not dumped: the keyrings unless
/// `secrets` (the user's setting) allows it.
pub fn refused(name: &str, secrets: bool) -> bool {
    !secrets && holds_secrets(name)
}

const PAGE: u64 = 4096;
/// A small dump leaves out writable memory larger than this (big heaps),
/// unless a thread's stack is in it.
const SMALL_LIMIT: u64 = 16 * 1024 * 1024;

const PT_LOAD: u32 = 1;
const PT_NOTE: u32 = 4;
const NT_PRSTATUS: u32 = 1;
const NT_PRFPREG: u32 = 2;
const NT_PRPSINFO: u32 = 3;
const NT_AUXV: u32 = 6;
const NT_FILE: u32 = 0x4649_4C45;

/// One note: "CORE", its type and contents, each padded to 4 bytes.
fn note(out: &mut Vec<u8>, kind: u32, desc: &[u8]) {
    out.extend_from_slice(&5u32.to_le_bytes());
    out.extend_from_slice(&(desc.len() as u32).to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(b"CORE\0\0\0\0");
    out.extend_from_slice(desc);
    out.resize(out.len().next_multiple_of(4), 0);
}

fn bytes_of<T>(value: &T) -> &[u8] {
    // SAFETY: T is plain old data from the C library (registers).
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

/// The mappings, by start, whose pages the program changed (copied from
/// their file on write): its data, and the tables the loader wrote and
/// then made read-only, where a debugger finds the libraries.
fn changed(pid: u32) -> HashSet<u64> {
    let smaps = std::fs::read_to_string(format!("/proc/{pid}/smaps")).unwrap_or_default();
    let mut out = HashSet::new();
    let mut current = None;
    for line in smaps.lines() {
        let Some(first) = line.split_ascii_whitespace().next() else {
            continue;
        };
        if !first.ends_with(':') {
            // "start-end perms offset dev inode path": a new mapping.
            current = first
                .split_once('-')
                .and_then(|(s, _)| u64::from_str_radix(s, 16).ok());
        } else if first == "Anonymous:"
            && let Some(start) = current
            && line.split_ascii_whitespace().nth(1) != Some("0")
        {
            out.insert(start);
        }
    }
    out
}

/// What is copied of a mapping: all of it, its first page (an ELF file's
/// header, for the debugger to find the program), or nothing.
fn kept(m: &Mapping, kind: DumpKind, stacks: &[u64], changed: &HashSet<u64>) -> u64 {
    let size = m.end - m.start;
    if !m.read || m.path == "[vvar]" || m.path == "[vsyscall]" {
        return 0;
    }
    if kind == DumpKind::Full || stacks.iter().any(|&sp| m.contains(sp)) {
        return size;
    }
    if (m.write || changed.contains(&m.start)) && size <= SMALL_LIMIT {
        return size;
    }
    if m.file_backed() && m.offset == 0 {
        return PAGE.min(size);
    }
    0
}

/// Copies `len` bytes of the process' memory from `start` to `out`; pages
/// that cannot be read (guard pages) are written as zeros.
fn copy(pid: u32, start: u64, len: u64, out: &mut impl Write) -> std::io::Result<()> {
    let mut buf = vec![0u8; 1024 * 1024];
    let mut at = start;
    let end = start + len;
    while at < end {
        let want = ((end - at) as usize).min(buf.len());
        let got = ptrace::read_memory(pid, at, &mut buf[..want]);
        if got > 0 {
            out.write_all(&buf[..got])?;
            at += got as u64;
            continue;
        }
        // Not readable: a page of zeros, then on.
        let skip = (PAGE - at % PAGE).min(end - at) as usize;
        out.write_all(&vec![0u8; skip])?;
        at += skip as u64;
    }
    Ok(())
}

/// Writes a core file of `key` into `dir`, which is created if needed.
/// The keyrings only with `secrets`.
pub fn write(
    key: ProcKey,
    name: &str,
    kind: DumpKind,
    dir: &Path,
    secrets: bool,
) -> Result<DumpReport, String> {
    let started = Instant::now();
    if refused(name, secrets) {
        return Err(format!(
            "{name} holds this session's passwords and keys: it is dumped only when Settings \
             allow it"
        ));
    }
    if key.pid == std::process::id() {
        return Err("Anabolic does not dump itself".into());
    }
    let pid = key.pid;
    let stat = procfs::stat(pid)
        .filter(|s| key.create_time == 0 || s.create_time() == key.create_time)
        .ok_or("it has exited")?;
    if stat.kernel_thread() {
        return Err("a kernel thread has no memory of its own to dump".into());
    }
    let maps = ptrace::maps(pid).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            "another user's process: dumping it needs cap_sys_ptrace".to_string()
        }
        _ => crate::util::error_text(&e),
    })?;
    let auxv = std::fs::read(format!("/proc/{pid}/auxv")).unwrap_or_default();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file_name(name, pid, kind));
    // The main thread first: debuggers take the first as the current one.
    let mut tids = procfs::tids(pid);
    tids.sort_by_key(|&t| (t != pid, t));

    let (written, _held) = ptrace::hold(&tids, |held| -> Result<u64, String> {
        let regs: Vec<_> = held
            .iter()
            .filter_map(|(tid, h)| Some((*tid, h.regs().ok()?, h.fpregs().ok())))
            .collect();
        let stacks: Vec<u64> = regs.iter().map(|(_, r, _)| r.rsp).collect();
        let changed = changed(pid);
        let notes = notes(&stat, &regs, &auxv, &maps);
        let segments: Vec<(&Mapping, u64)> = maps
            .iter()
            .filter(|m| m.read || m.file_backed())
            .map(|m| (m, kept(m, kind, &stacks, &changed)))
            .collect();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            // Memory holds whatever the program had: its owner's alone.
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = BufWriter::with_capacity(1024 * 1024, file);
        let written = write_core(pid, &notes, &segments, &mut out)
            .and_then(|n| out.flush().map(|_| n))
            .map_err(|e| format!("{}: {e}", path.display()));
        if written.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        written
    })?;
    Ok(DumpReport {
        path,
        bytes: written?,
        took: started.elapsed(),
        cloned: false,
    })
}

type Regs = (
    u32,
    libc::user_regs_struct,
    Option<libc::user_fpregs_struct>,
);

/// The notes: each thread's registers, the process' description, its
/// auxiliary vector and the files it has mapped.
fn notes(stat: &procfs::Stat, regs: &[Regs], auxv: &[u8], maps: &[Mapping]) -> Vec<u8> {
    let mut out = Vec::new();
    for (tid, r, fp) in regs {
        // struct elf_prstatus: signal info, pids, times, registers.
        let mut status = vec![0u8; 336];
        status[32..36].copy_from_slice(&tid.to_le_bytes());
        status[36..40].copy_from_slice(&stat.ppid.to_le_bytes());
        status[112..112 + size_of::<libc::user_regs_struct>()].copy_from_slice(bytes_of(r));
        status[328..332].copy_from_slice(&u32::from(fp.is_some()).to_le_bytes());
        note(&mut out, NT_PRSTATUS, &status);
        if let Some(fp) = fp {
            note(&mut out, NT_PRFPREG, bytes_of(fp));
        }
    }
    // struct elf_prpsinfo: state, ids, name and arguments.
    let mut info = vec![0u8; 136];
    info[1] = stat.state;
    info[3] = stat.nice as i8 as u8;
    let ids = std::fs::read_to_string(format!("/proc/{}/status", stat.pid)).unwrap_or_default();
    let first = |key: &str| {
        procfs::status_value(&ids, key)
            .and_then(|v| v.split_ascii_whitespace().next()?.parse::<u32>().ok())
            .unwrap_or(0)
    };
    info[16..20].copy_from_slice(&first("Uid").to_le_bytes());
    info[20..24].copy_from_slice(&first("Gid").to_le_bytes());
    info[24..28].copy_from_slice(&stat.pid.to_le_bytes());
    info[28..32].copy_from_slice(&stat.ppid.to_le_bytes());
    let fname = stat.comm.as_bytes();
    info[40..40 + fname.len().min(15)].copy_from_slice(&fname[..fname.len().min(15)]);
    let args = procfs::cmdline(stat.pid).unwrap_or_default();
    let args = args.as_bytes();
    info[56..56 + args.len().min(79)].copy_from_slice(&args[..args.len().min(79)]);
    note(&mut out, NT_PRPSINFO, &info);
    if !auxv.is_empty() {
        note(&mut out, NT_AUXV, auxv);
    }
    // NT_FILE: count, page size, (start, end, offset in pages), names.
    let files: Vec<&Mapping> = maps.iter().filter(|m| m.file_backed()).collect();
    let mut table = Vec::new();
    table.extend_from_slice(&(files.len() as u64).to_le_bytes());
    table.extend_from_slice(&PAGE.to_le_bytes());
    for m in &files {
        table.extend_from_slice(&m.start.to_le_bytes());
        table.extend_from_slice(&m.end.to_le_bytes());
        table.extend_from_slice(&(m.offset / PAGE).to_le_bytes());
    }
    for m in &files {
        table.extend_from_slice(m.path.trim_end_matches(" (deleted)").as_bytes());
        table.push(0);
    }
    note(&mut out, NT_FILE, &table);
    out
}

/// The ELF header, the program headers (the notes, then one load segment
/// per mapping) and the memory, page-aligned. Returns the file's size.
fn write_core(
    pid: u32,
    notes: &[u8],
    segments: &[(&Mapping, u64)],
    out: &mut impl Write,
) -> std::io::Result<u64> {
    let headers = 64 + 56 * (1 + segments.len() as u64);
    let notes_at = headers;
    let data_at = (notes_at + notes.len() as u64).next_multiple_of(PAGE);

    let mut header = [0u8; 64];
    header[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    header[16..18].copy_from_slice(&4u16.to_le_bytes()); // ET_CORE
    header[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    header[20..24].copy_from_slice(&1u32.to_le_bytes());
    header[32..40].copy_from_slice(&64u64.to_le_bytes()); // program headers
    header[52..54].copy_from_slice(&64u16.to_le_bytes());
    header[54..56].copy_from_slice(&56u16.to_le_bytes());
    header[56..58].copy_from_slice(&(1 + segments.len() as u16).to_le_bytes());
    out.write_all(&header)?;

    let program = |kind: u32, flags: u32, offset: u64, vaddr: u64, filesz: u64, memsz: u64| {
        let mut p = [0u8; 56];
        p[0..4].copy_from_slice(&kind.to_le_bytes());
        p[4..8].copy_from_slice(&flags.to_le_bytes());
        p[8..16].copy_from_slice(&offset.to_le_bytes());
        p[16..24].copy_from_slice(&vaddr.to_le_bytes());
        p[32..40].copy_from_slice(&filesz.to_le_bytes());
        p[40..48].copy_from_slice(&memsz.to_le_bytes());
        p[48..56].copy_from_slice(&(if kind == PT_LOAD { PAGE } else { 4 }).to_le_bytes());
        p
    };
    let size = notes.len() as u64;
    out.write_all(&program(PT_NOTE, 0, notes_at, 0, size, 0))?;
    let mut at = data_at;
    for (m, kept) in segments {
        let flags = u32::from(m.exec) | u32::from(m.write) << 1 | u32::from(m.read) << 2;
        out.write_all(&program(
            PT_LOAD,
            flags,
            at,
            m.start,
            *kept,
            m.end - m.start,
        ))?;
        at += kept;
    }
    out.write_all(notes)?;
    out.write_all(&vec![0u8; (data_at - notes_at - size) as usize])?;
    for (m, kept) in segments {
        if *kept > 0 {
            copy(pid, m.start, *kept, out)?;
        }
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dumps a child both ways and asks gdb, when installed, where it is.
    #[test]
    fn a_debugger_reads_the_dump() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let key = ProcKey {
            pid,
            create_time: procfs::stat(pid).unwrap().create_time(),
        };
        let dir = std::env::temp_dir().join(format!("anabolic-dump-{}", std::process::id()));
        let full = write(key, "sleep", DumpKind::Full, &dir, false);
        let small = write(key, "sleep", DumpKind::Small, &dir, false);
        let _ = child.kill();
        let _ = child.wait();
        let (full, small) = (full.unwrap(), small.unwrap());
        assert!(small.bytes < full.bytes);
        assert_eq!(std::fs::metadata(&full.path).unwrap().len(), full.bytes);
        for report in [&full, &small] {
            let gdb = std::process::Command::new("gdb")
                .args(["-batch", "-nx", "-iex", "set debuginfod enabled off"])
                .args(["-ex", "bt", "/usr/bin/sleep"])
                .env_remove("DEBUGINFOD_URLS")
                .arg(&report.path)
                .output();
            if let Ok(out) = gdb {
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(
                    text.contains("nanosleep"),
                    "{}: {text}",
                    report.path.display()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(refused("kwalletd6", false) && !refused("kwalletd6", true));
    }
}
