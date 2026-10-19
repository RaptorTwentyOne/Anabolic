//! Per-process facts on Linux: the executable, command line, owner, the
//! machine it is built for, and what rights it runs with.

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
    d
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
}
