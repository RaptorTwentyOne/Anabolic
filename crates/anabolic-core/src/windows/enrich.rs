//! Slow, per-process facts (path, command line, owner, icon, version info) are
//! gathered here, off both the UI and the collector thread. Results stream to
//! the UI over a channel; nothing ever waits for them.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, PSID, TOKEN_ELEVATION,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenIntegrityLevel, TokenUser,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
    IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN,
};
use windows::Win32::System::Threading::{
    IsWow64Process2, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

use crate::hardening::{ThreadPriority, set_current_thread_priority};
use crate::model::ProcKey;
use crate::ntapi;
use crate::util::OwnedHandle;
use crate::winfile::{self, DeviceMap, FileInfo};

use super::*;

impl Integrity {
    fn from_rid(rid: u32) -> Self {
        match rid {
            0..0x1000 => Integrity::Untrusted,
            0x1000..0x2000 => Integrity::Low,
            0x2000..0x2100 => Integrity::Medium,
            0x2100..0x3000 => Integrity::MediumPlus,
            0x3000..0x4000 => Integrity::High,
            0x4000..0x5000 => Integrity::System,
            _ => Integrity::Protected,
        }
    }
}

#[derive(Default)]
struct Caches {
    users: HashMap<Vec<u8>, String>,
    files: HashMap<String, Arc<FileInfo>>,
    icons: HashMap<String, Option<Arc<Icon>>>,
    devices: DeviceMap,
}

static NEXT_ICON_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn worker(requests: Receiver<EnrichRequest>, results: Sender<Enriched>) {
    set_current_thread_priority(ThreadPriority::BelowNormal);
    // Shell icon extraction wants COM on this thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    let mut caches = Caches {
        devices: DeviceMap::build(),
        ..Default::default()
    };
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

/// The account name of an owner SID: `DOMAIN\user`.
pub fn user_name(sid: &[u8]) -> Option<String> {
    winfile::lookup_account(sid)
}

/// The details of one process, read on the calling thread with caches of
/// its own (the MCP server asks for any process, not the ones on screen).
pub fn details_of(key: ProcKey, sid: Option<Arc<[u8]>>) -> ProcessDetails {
    let mut caches = Caches {
        devices: DeviceMap::build(),
        ..Default::default()
    };
    enrich(
        &EnrichRequest {
            key,
            sid,
            nt_path: None,
        },
        &mut caches,
    )
}

fn enrich(request: &EnrichRequest, caches: &mut Caches) -> ProcessDetails {
    let pid = request.key.pid;
    let mut d = ProcessDetails::default();

    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => Some(OwnedHandle(h)),
        Err(e) => {
            d.open_error = Some(crate::util::error_text(&e));
            None
        }
    };

    d.path = if pid == 4 {
        std::env::var("SystemRoot")
            .ok()
            .map(|root| format!(r"{root}\System32\ntoskrnl.exe"))
    } else {
        request
            .nt_path
            .as_deref()
            .and_then(|p| caches.devices.to_dos_path(p))
            .or_else(|| handle.as_ref().and_then(|h| win32_image_path(h.raw())))
            .or_else(|| winfile::nt_image_path(pid).and_then(|p| caches.devices.to_dos_path(&p)))
    };

    if let Some(h) = &handle {
        d.command_line = command_line(h.raw());
        d.arch = arch(h.raw());
        d.critical = is_critical(h.raw());
        token_info(
            h.raw(),
            &mut d,
            request.sid.is_none().then_some(&mut caches.users),
        );
    }
    if let Some(sid) = &request.sid {
        d.user = Some(account_name(sid, &mut caches.users));
    }

    if let Some(path) = d.path.clone() {
        let cache_key = path.to_lowercase();
        let info = caches
            .files
            .entry(cache_key.clone())
            .or_insert_with(|| Arc::new(winfile::file_info(&path)))
            .clone();
        d.description = info.description.clone();
        d.company = info.company.clone();
        d.version = info.version.clone();
        d.icon = caches
            .icons
            .entry(cache_key)
            .or_insert_with(|| {
                winfile::extract_icon(&path, 32).map(|(width, height, rgba)| {
                    Arc::new(Icon {
                        id: NEXT_ICON_ID.fetch_add(1, Ordering::Relaxed),
                        width,
                        height,
                        rgba,
                    })
                })
            })
            .clone();
    }
    if pid == 4 && d.description.is_none() {
        d.description = Some("NT Kernel & System".into());
    }
    d
}

fn win32_image_path(process: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn command_line(process: HANDLE) -> Option<String> {
    let mut buf: Vec<u64> = vec![0; 512];
    for _ in 0..3 {
        let mut needed = 0u32;
        let bytes = buf.len() * 8;
        let status = unsafe {
            ntapi::NtQueryInformationProcess(
                process,
                ntapi::ProcessCommandLineInformation,
                buf.as_mut_ptr().cast(),
                bytes as u32,
                &mut needed,
            )
        };
        if ntapi::nt_success(status) {
            // SAFETY: the kernel wrote a UNICODE_STRING followed by its characters.
            let us = unsafe { &*buf.as_ptr().cast::<ntapi::UNICODE_STRING>() };
            let base = buf.as_ptr() as usize;
            let ptr = us.Buffer as usize;
            let len = us.Length as usize;
            if us.Buffer.is_null() || ptr < base || ptr + len > base + bytes {
                return None;
            }
            let chars = unsafe { std::slice::from_raw_parts(us.Buffer, len / 2) };
            return Some(String::from_utf16_lossy(chars));
        }
        let grow = matches!(
            status,
            ntapi::STATUS_INFO_LENGTH_MISMATCH
                | ntapi::STATUS_BUFFER_OVERFLOW
                | ntapi::STATUS_BUFFER_TOO_SMALL
        );
        if !grow || needed as usize <= bytes || needed > 1 << 20 {
            return None;
        }
        buf.resize((needed as usize).div_ceil(8), 0);
    }
    None
}

fn arch(process: HANDLE) -> Option<Arch> {
    let mut process_machine = IMAGE_FILE_MACHINE::default();
    let mut native_machine = IMAGE_FILE_MACHINE::default();
    unsafe { IsWow64Process2(process, &mut process_machine, Some(&mut native_machine)) }.ok()?;
    let machine = if process_machine == IMAGE_FILE_MACHINE_UNKNOWN {
        native_machine
    } else {
        process_machine
    };
    Some(match machine {
        IMAGE_FILE_MACHINE_AMD64 => Arch::X64,
        IMAGE_FILE_MACHINE_I386 => Arch::X86,
        IMAGE_FILE_MACHINE_ARM64 => Arch::Arm64,
        other => Arch::Other(other.0),
    })
}

fn is_critical(process: HANDLE) -> bool {
    let mut value = 0u32;
    let status = unsafe {
        ntapi::NtQueryInformationProcess(
            process,
            ntapi::ProcessBreakOnTermination,
            (&raw mut value).cast(),
            4,
            std::ptr::null_mut(),
        )
    };
    ntapi::nt_success(status) && value != 0
}

/// Elevation, integrity level and (when the scan did not provide a SID) owner.
fn token_info(
    process: HANDLE,
    d: &mut ProcessDetails,
    users: Option<&mut HashMap<Vec<u8>, String>>,
) {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.is_err() {
        return;
    }
    let token = OwnedHandle(token);
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    let mut query = |class, buf: &mut [u64; 64]| unsafe {
        GetTokenInformation(
            token.raw(),
            class,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut len,
        )
        .is_ok()
    };

    if query(TokenElevation, &mut buf) {
        let elevation = unsafe { &*buf.as_ptr().cast::<TOKEN_ELEVATION>() };
        d.elevated = Some(elevation.TokenIsElevated != 0);
    }
    if query(TokenIntegrityLevel, &mut buf) {
        let label = unsafe { &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
        let sid = label.Label.Sid;
        unsafe {
            let count = *GetSidSubAuthorityCount(sid);
            if count > 0 {
                d.integrity = Some(Integrity::from_rid(*GetSidSubAuthority(
                    sid,
                    count as u32 - 1,
                )));
            }
        }
    }
    if let Some(users) = users
        && query(TokenUser, &mut buf)
    {
        let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
        if let Some(sid) = sid_bytes(user.User.Sid) {
            d.user = Some(account_name(&sid, users));
        }
    }
}

fn sid_bytes(sid: PSID) -> Option<Vec<u8>> {
    if sid.0.is_null() {
        return None;
    }
    let count = unsafe { *GetSidSubAuthorityCount(sid) } as usize;
    let len = 8 + 4 * count;
    Some(unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), len) }.to_vec())
}

fn account_name(sid: &[u8], cache: &mut HashMap<Vec<u8>, String>) -> String {
    if let Some(name) = cache.get(sid) {
        return name.clone();
    }
    let name = winfile::lookup_account(sid).unwrap_or_else(|| winfile::sid_to_string(sid));
    cache.insert(sid.to_vec(), name.clone());
    name
}
