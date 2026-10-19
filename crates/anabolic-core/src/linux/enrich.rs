//! Per-process facts on Linux: the executable, command line, owner, the
//! machine it is built for, what rights it runs with, and what it is: its
//! desktop entry (name, icon) and the package it came with.

use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use crate::hardening::{ThreadPriority, set_current_thread_priority};
use crate::model::ProcKey;
use crate::procfs;

use super::*;

#[derive(Default)]
struct Caches {
    users: HashMap<u32, String>,
}

pub(super) fn worker(requests: Receiver<EnrichRequest>, results: Sender<Enriched>) {
    set_current_thread_priority(ThreadPriority::BelowNormal);
    let mut caches = Caches::default();
    for request in requests {
        let details = enrich(&request, &mut caches);
        if results
            .send(Enriched {
                key: request.key,
                details: Arc::new(details),
            })
            .is_err()
        {
            return;
        }
    }
}

/// The details of one process, read on the calling thread with caches of
/// its own (the MCP server asks for any process, not the ones on screen).
pub fn details_of(key: ProcKey, sid: Option<Arc<[u8]>>) -> ProcessDetails {
    enrich(
        &EnrichRequest {
            key,
            sid,
            nt_path: None,
        },
        &mut Caches::default(),
    )
}

/// The account name of an owner (`ProcessInfo::user_sid`: a uid in 4
/// little-endian bytes).
pub fn user_name(sid: &[u8]) -> Option<String> {
    let uid = u32::from_le_bytes(sid.get(..4)?.try_into().ok()?);
    account(uid)
}

fn account(uid: u32) -> Option<String> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let status =
        unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
    if status != 0 || found.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    Some(name.to_string_lossy().into_owned())
}

/// The machine an ELF file is built for.
fn arch_of(path: &str) -> Option<Arch> {
    use std::io::Read;
    let mut head = [0u8; 20];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..4] != b"\x7fELF" {
        return None;
    }
    let machine = u16::from_le_bytes([head[18], head[19]]);
    Some(match machine {
        62 => Arch::X64,
        3 => Arch::X86,
        183 => Arch::Arm64,
        other => Arch::Other(other),
    })
}

fn enrich(request: &EnrichRequest, caches: &mut Caches) -> ProcessDetails {
    let pid = request.key.pid;
    let mut d = ProcessDetails::default();
    if pid == crate::scan::KERNEL_PID {
        d.description = Some("The Linux kernel's own threads".into());
        d.version = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|v| v.trim().to_string());
        d.user = Some("root".into());
        d.critical = true;
        return d;
    }
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(path) => {
            let path = path.to_string_lossy();
            d.path = Some(path.trim_end_matches(" (deleted)").to_string());
        }
        Err(e) => d.open_error = Some(crate::util::error_text(&e)),
    }
    d.command_line = procfs::cmdline(pid).filter(|c| !c.is_empty());
    d.arch = d
        .path
        .as_deref()
        .and_then(|p| arch_of(&format!("/proc/{pid}/exe")).or_else(|| arch_of(p)));
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let ids = |key: &str| -> Vec<u32> {
        procfs::status_value(&status, key)
            .map(|v| {
                v.split_ascii_whitespace()
                    .filter_map(|n| n.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let uids = ids("Uid");
    if let Some(&uid) = uids.first() {
        d.user = Some(
            caches
                .users
                .entry(uid)
                .or_insert_with(|| account(uid).unwrap_or_else(|| uid.to_string()))
                .clone(),
        );
    }
    // Effectively root, or holding capabilities beyond a user's.
    let effective_root = uids.get(1) == Some(&0);
    let caps = procfs::status_value(&status, "CapEff")
        .and_then(|c| u64::from_str_radix(c, 16).ok())
        .unwrap_or(0);
    d.elevated = Some(effective_root || caps != 0);
    // Ending init ends the system.
    d.critical = pid == 1;
    identify(pid, &mut d);
    d
}

/// What the process is: its desktop entry's name and icon, and the
/// package it came with (the package's description when the desktop has
/// no entry for it). A Flatpak app is its app.
fn identify(pid: u32, d: &mut ProcessDetails) {
    let argv = procfs::argv(pid);
    let argv0 = argv.first().map(String::as_str);
    // Without the right to look into it, the name it was started as is the
    // best guide to what it is: used to look it up, not shown as its path.
    let program = d.path.clone().or_else(|| {
        argv0
            .filter(|a| a.starts_with('/') && std::path::Path::new(a).is_file())
            .map(str::to_string)
    });
    let comm = procfs::stat(pid).map(|s| s.comm).unwrap_or_default();
    let flatpak = crate::packages::flatpak(pid);
    let app = crate::apps::find(
        program.as_deref(),
        argv0,
        &comm,
        flatpak.as_ref().map(|f| f.name.as_str()),
    );
    let package = flatpak
        .map(Arc::new)
        .or_else(|| program.as_deref().and_then(crate::packages::owner));
    d.description = app
        .as_ref()
        .map(|a| a.name.clone())
        .or_else(|| package.as_ref().and_then(|p| p.description.clone()));
    if let Some(p) = &package {
        d.company = Some(p.label());
        d.version = Some(p.version.clone()).filter(|v| !v.is_empty());
    }
    // The entry's icon, else one named after the program, as many are.
    let file = program.as_deref().and_then(|p| p.rsplit('/').next());
    d.icon = app
        .as_ref()
        .and_then(|a| a.icon.as_deref())
        .and_then(crate::apps::icon)
        .or_else(|| file.and_then(crate::apps::icon));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_itself() {
        let me = procfs::stat(std::process::id()).unwrap();
        let key = ProcKey {
            pid: std::process::id(),
            create_time: me.create_time(),
        };
        let d = details_of(key, None);
        assert!(d.path.as_deref().is_some_and(|p| p.contains("anabolic")));
        assert!(d.command_line.is_some());
        assert_eq!(d.arch, Some(Arch::X64));
        assert!(d.user.is_some());
        let uid = unsafe { libc::getuid() };
        assert_eq!(user_name(&uid.to_le_bytes()), d.user);
    }

    /// Every process this user runs, enriched, and what it costs.
    #[test]
    fn describes_the_desktop() {
        let uid = unsafe { libc::getuid() };
        let mine: Vec<ProcKey> = procfs::pids()
            .into_iter()
            .filter(|&pid| procfs::uid(pid) == Some(uid))
            .filter_map(|pid| {
                Some(ProcKey {
                    pid,
                    create_time: procfs::stat(pid)?.create_time(),
                })
            })
            .collect();
        let mut caches = Caches::default();
        let request = |key| EnrichRequest {
            key,
            sid: None,
            nt_path: None,
        };
        let started = std::time::Instant::now();
        let details: Vec<ProcessDetails> = mine
            .iter()
            .map(|&k| enrich(&request(k), &mut caches))
            .collect();
        let first = started.elapsed();
        let started = std::time::Instant::now();
        for &k in &mine {
            enrich(&request(k), &mut caches);
        }
        let again = started.elapsed();
        let named = details.iter().filter(|d| d.description.is_some()).count();
        let packaged = details.iter().filter(|d| d.company.is_some()).count();
        let icons = details.iter().filter(|d| d.icon.is_some()).count();
        eprintln!(
            "{} processes: {named} described, {packaged} packaged, {icons} with icons; \
             first pass {first:?}, then {:?} each",
            mine.len(),
            again / mine.len().max(1) as u32
        );
        assert!(named > 0);
    }
}
