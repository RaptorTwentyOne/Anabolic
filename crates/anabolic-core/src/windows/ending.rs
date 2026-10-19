//! Why a process does not go away.
//!
//! Task Manager reports all of these as "access is denied":
//! - It is already ending and Windows cannot finish. With no threads left
//!   it stays listed while something still holds it, and every new attempt
//!   is answered "the process is terminating".
//! - The right to end it is withheld: Windows protects it (a protected
//!   process), or a driver strips that right from every handle opened to it
//!   (anti-cheat and security software do this).
//!
//! [`flags`] costs one query with limited access; the collector runs it for
//! processes that have no threads. [`why_alive`] reads the system handle
//! table and runs when asked.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, STILL_ACTIVE};
use windows::Win32::System::ProcessStatus::{EnumDeviceDrivers, GetDeviceDriverBaseNameW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use crate::handles::with_handle_table;
use crate::model::ProcKey;
use crate::ntapi;
use crate::util::{OwnedHandle, error_text, open_process};

/// What Windows says about a process' state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    /// Ending has begun (it is being deleted).
    pub deleting: bool,
    pub protected: bool,
    pub wow64: bool,
    /// `STILL_ACTIVE` (259) until it has ended.
    pub exit_status: u32,
}

/// PROCESS_EXTENDED_BASIC_INFORMATION: the basic information plus flags.
#[repr(C)]
#[derive(Default)]
struct ExtendedBasic {
    size: usize,
    exit_status: i32,
    peb: usize,
    affinity: usize,
    base_priority: i32,
    pid: usize,
    parent: usize,
    flags: u32,
}

pub fn flags(process: HANDLE) -> Option<Flags> {
    let mut info = ExtendedBasic {
        size: size_of::<ExtendedBasic>(),
        ..Default::default()
    };
    let status = unsafe {
        ntapi::NtQueryInformationProcess(
            process,
            0, // ProcessBasicInformation
            (&mut info as *mut ExtendedBasic).cast(),
            size_of::<ExtendedBasic>() as u32,
            std::ptr::null_mut(),
        )
    };
    ntapi::nt_success(status).then_some(Flags {
        protected: info.flags & 1 != 0,
        wow64: info.flags & 2 != 0,
        deleting: info.flags & 4 != 0,
        exit_status: info.exit_status as u32,
    })
}

/// Whether ending has begun for `pid`. Opens it with limited access and
/// closes the handle at once: a handle kept open would itself hold it.
pub fn is_ending(pid: u32) -> bool {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .ok()
        .map(OwnedHandle)
        .and_then(|h| flags(h.raw()))
        .is_some_and(|f| f.deleting)
}

/// A process holding handles to the one asked about.
#[derive(Clone, Debug)]
pub struct Holder {
    pub pid: u32,
    pub name: Arc<str>,
    pub handles: Vec<u64>,
    /// The rights of all its handles together.
    pub access: u32,
}

#[derive(Clone, Debug)]
pub struct EndReport {
    pub key: ProcKey,
    pub flags: Flags,
    pub threads: u32,
    /// PS_PROTECTION: 0 when Windows does not protect it.
    pub protection: u8,
    /// Whether a handle opened to end it came with the right to; None when
    /// it could not be opened at all (`open_error`).
    pub can_end: Option<bool>,
    pub open_error: Option<String>,
    /// Other processes with handles to it; while they hold them, Windows
    /// cannot remove it.
    pub holders: Vec<Holder>,
    /// Loaded drivers known to withhold the right to end processes.
    pub guards: Vec<(String, &'static str)>,
    pub took: Duration,
}

/// Every process' name (file name only), and how many threads `pid` has.
fn process_names(pid: u32) -> (HashMap<u32, Arc<str>>, u32) {
    let mut scanner = crate::scan::ProcessScanner::new();
    let mut names: HashMap<u32, Arc<str>> = HashMap::new();
    let mut threads = 0;
    if scanner.refresh().is_ok() {
        for e in scanner.entries() {
            if e.pid() == pid {
                threads = e.spi.NumberOfThreads;
            }
            let path = String::from_utf16_lossy(&e.image_name());
            let name = path.rsplit('\\').next().unwrap_or(&path);
            names.insert(e.pid(), Arc::from(name));
        }
    }
    (names, threads)
}

/// The processes with handles to `object` in `table`, most handles first.
fn holders_in(
    table: &[crate::ntapi::SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX],
    object: u64,
    except: &[u32],
    names: &HashMap<u32, Arc<str>>,
) -> Vec<Holder> {
    let mut holders: HashMap<u32, Holder> = HashMap::new();
    for e in table.iter().filter(|e| e.Object as u64 == object) {
        let pid = e.UniqueProcessId as u32;
        if except.contains(&pid) {
            continue;
        }
        let holder = holders.entry(pid).or_insert_with(|| Holder {
            pid,
            name: names.get(&pid).cloned().unwrap_or_else(|| Arc::from("?")),
            handles: Vec::new(),
            access: 0,
        });
        holder.handles.push(e.HandleValue as u64);
        holder.access |= e.GrantedAccess;
    }
    let mut holders: Vec<Holder> = holders.into_values().collect();
    holders.sort_by_key(|h| std::cmp::Reverse(h.handles.len()));
    holders
}

/// Who else holds the kernel object at `object` (an address from the
/// system handle table), `except` left out. Reads the table only.
pub fn object_holders(object: u64, except: &[u32]) -> Result<Vec<Holder>, String> {
    let (names, _) = process_names(0);
    with_handle_table(|table| holders_in(table, object, except, &names)).map(|(h, _)| h)
}

/// Drivers that strip the right to end the processes they guard, by file
/// name prefix.
const GUARDS: [(&str, &str); 11] = [
    ("xhunter", "XIGNCODE3 anti-cheat"),
    ("bedaisy", "BattlEye anti-cheat"),
    ("easyanticheat", "Easy Anti-Cheat"),
    ("eaanticheat", "EA anti-cheat"),
    ("vgk", "Riot Vanguard anti-cheat"),
    ("npptnt", "nProtect GameGuard"),
    ("ace-", "Tencent ACE anti-cheat"),
    ("mhyprot", "miHoYo anti-cheat"),
    ("faceit", "FACEIT anti-cheat"),
    ("klif", "Kaspersky"),
    ("ahnrghlh", "AhnLab"),
];

fn guards() -> Vec<(String, &'static str)> {
    let mut bases = vec![std::ptr::null_mut(); 2048];
    let mut needed = 0u32;
    if unsafe {
        EnumDeviceDrivers(
            bases.as_mut_ptr(),
            (bases.len() * size_of::<usize>()) as u32,
            &mut needed,
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    let n = (needed as usize / size_of::<usize>()).min(bases.len());
    let mut found = Vec::new();
    for &base in &bases[..n] {
        let mut name = [0u16; 260];
        let len = unsafe { GetDeviceDriverBaseNameW(base, &mut name) } as usize;
        let file = String::from_utf16_lossy(&name[..len.min(name.len())]);
        let lower = file.to_lowercase();
        if let Some((_, product)) = GUARDS.iter().find(|(prefix, _)| lower.starts_with(prefix)) {
            found.push((file, *product));
        }
    }
    found
}

/// Signer of a protected process, from its PS_PROTECTION byte.
pub fn protection_label(protection: u8) -> String {
    let kind = match protection & 7 {
        1 => "light ",
        _ => "",
    };
    let signer = match protection >> 4 {
        1 => "Authenticode",
        2 => "CodeGen",
        3 => "Antimalware",
        4 => "LSA",
        5 => "Windows",
        6 => "WinTcb",
        7 => "WinSystem",
        8 => "Store app",
        _ => "unknown signer",
    };
    format!("{kind}protection, {signer}")
}

/// Everything that explains why `key` is still there. Opens it twice: for
/// a limited query and to end it (not used: only to see whether Windows
/// grants that right), plus the system handle table.
pub fn why_alive(key: ProcKey) -> Result<EndReport, String> {
    let started = Instant::now();
    let query = open_process(key, PROCESS_QUERY_LIMITED_INFORMATION).map_err(|e| error_text(&e))?;
    let flags = flags(query.raw()).unwrap_or_default();
    let mut protection = 0u8;
    let _ = unsafe {
        ntapi::NtQueryInformationProcess(
            query.raw(),
            61, // ProcessProtectionInformation
            (&mut protection as *mut u8).cast(),
            1,
            std::ptr::null_mut(),
        )
    };
    let (end, open_error) = match unsafe { OpenProcess(PROCESS_TERMINATE, false, key.pid) } {
        Ok(h) => (Some(OwnedHandle(h)), None),
        Err(e) => (None, Some(error_text(&e))),
    };

    let (names, threads) = process_names(key.pid);

    let own = std::process::id() as usize;
    let ((holders, can_end), _) = with_handle_table(|table| {
        let ours = |h: &OwnedHandle| {
            table
                .iter()
                .find(|e| e.UniqueProcessId == own && e.HandleValue == h.raw().0 as usize)
        };
        let can_end = end
            .as_ref()
            .and_then(ours)
            .map(|e| e.GrantedAccess & PROCESS_TERMINATE.0 != 0);
        let holders = match ours(&query).map(|e| e.Object).filter(|&o| o != 0) {
            Some(object) => holders_in(table, object as u64, &[own as u32], &names),
            None => Vec::new(),
        };
        (holders, can_end)
    })?;

    Ok(EndReport {
        key,
        flags,
        threads,
        protection,
        can_end,
        open_error,
        holders,
        guards: guards(),
        took: started.elapsed(),
    })
}

/// Whether `status` from `flags` means it has not ended.
pub fn still_active(status: u32) -> bool {
    status == STILL_ACTIVE.0 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_process_is_not_ending() {
        let me = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
        let f = flags(me).expect("flags of our own process");
        assert!(!f.deleting);
        assert!(still_active(f.exit_status));
        assert!(!is_ending(std::process::id()));
    }

    #[test]
    fn a_child_has_one_holder_us_excluded() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 30 127.0.0.1 >nul"])
            .spawn()
            .expect("spawn cmd");
        let mut scanner = crate::scan::ProcessScanner::new();
        scanner.refresh().unwrap();
        let create_time = scanner
            .entries()
            .find(|e| e.pid() == child.id())
            .map(|e| e.spi.CreateTime)
            .expect("child listed");
        let report = why_alive(ProcKey {
            pid: child.id(),
            create_time,
        })
        .expect("report");
        assert_eq!(report.can_end, Some(true));
        assert_eq!(report.protection, 0);
        assert!(!report.flags.deleting);
        assert!(report.threads >= 1);
        // We hold the child's handle (std keeps it), but we are excluded.
        assert!(report.holders.iter().all(|h| h.pid != std::process::id()));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
