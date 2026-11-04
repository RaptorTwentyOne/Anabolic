// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Windows services: each one's state, start type and host process, and
//! starting, stopping and reconfiguring them. All of it goes to the Service
//! Control Manager over RPC and a stop can take half a minute, so it runs on
//! threads of its own, never the UI's.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_DEPENDENT_SERVICES_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST,
    ERROR_SERVICE_MARKED_FOR_DELETE, ERROR_SERVICE_NOT_ACTIVE,
};
use windows::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose,
    LsaNtStatusToWinError, LsaOpenPolicy, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
};
use windows::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, ENUM_SERVICE_STATUS_PROCESSW, ENUM_SERVICE_STATUSW,
    ENUM_SERVICE_TYPE, EnumDependentServicesW, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW,
    QUERY_SERVICE_CONFIGW, QueryServiceConfig2W, QueryServiceConfigW, QueryServiceStatusEx,
    SC_ACTION, SC_ACTION_NONE, SC_ACTION_REBOOT, SC_ACTION_RESTART, SC_ACTION_RUN_COMMAND,
    SC_ACTION_TYPE, SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE,
    SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_PAUSE_CONTINUE,
    SERVICE_ACCEPT_STOP, SERVICE_ACTIVE, SERVICE_AUTO_START, SERVICE_BOOT_START,
    SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
    SERVICE_CONFIG_TRIGGER_INFO, SERVICE_CONTINUE_PENDING, SERVICE_CONTROL_CONTINUE,
    SERVICE_CONTROL_PAUSE, SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO,
    SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_DISABLED, SERVICE_DRIVER, SERVICE_ERROR,
    SERVICE_ERROR_CRITICAL, SERVICE_ERROR_IGNORE, SERVICE_ERROR_NORMAL, SERVICE_ERROR_SEVERE,
    SERVICE_FAILURE_ACTIONS_FLAG, SERVICE_FAILURE_ACTIONSW, SERVICE_NO_CHANGE,
    SERVICE_PAUSE_CONTINUE, SERVICE_PAUSE_PENDING, SERVICE_PAUSED, SERVICE_QUERY_CONFIG,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING,
    SERVICE_START_TYPE, SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
    SERVICE_STOP_PENDING, SERVICE_SYSTEM_START, SERVICE_TRIGGER_INFO, SERVICE_WIN32,
    SERVICE_WIN32_OWN_PROCESS, SERVICE_WIN32_SHARE_PROCESS, StartServiceW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::util::{error_text, from_wide, wide};

use super::*;

/// A per-user service: the template, or (with the instance bit) the copy
/// Windows starts for each signed-in user, named `Template_1a2b3c`.
const SERVICE_USER_SERVICE: u32 = 0x40;
const SERVICE_USERSERVICE_INSTANCE: u32 = 0x80;

impl StartType {
    fn from_raw(start: SERVICE_START_TYPE, delayed: bool) -> Self {
        match start {
            SERVICE_BOOT_START => StartType::Boot,
            SERVICE_SYSTEM_START => StartType::System,
            SERVICE_AUTO_START if delayed => StartType::AutomaticDelayed,
            SERVICE_AUTO_START => StartType::Automatic,
            SERVICE_DISABLED => StartType::Disabled,
            _ => StartType::Manual,
        }
    }

    fn raw(self) -> SERVICE_START_TYPE {
        match self {
            StartType::Boot => SERVICE_BOOT_START,
            StartType::System => SERVICE_SYSTEM_START,
            StartType::Automatic | StartType::AutomaticDelayed => SERVICE_AUTO_START,
            StartType::Manual => SERVICE_DEMAND_START,
            StartType::Disabled => SERVICE_DISABLED,
        }
    }
}

/// Closes the service (or manager) handle on drop.
struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn manager(access: u32) -> Result<Handle, String> {
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) }
        .map(Handle)
        .map_err(|e| error_text(&e))
}

fn open(scm: &Handle, name: &str, access: u32) -> windows::core::Result<Handle> {
    let name = wide(name);
    unsafe { OpenServiceW(scm.0, PCWSTR(name.as_ptr()), access) }.map(Handle)
}

/// A string the SCM handed back in one of its buffers.
fn text(p: PWSTR) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { p.to_string() }.unwrap_or_default()
    }
}

/// A double-nul-terminated list of strings the SCM handed back.
fn strings(p: PWSTR) -> Vec<String> {
    let mut out = Vec::new();
    if p.is_null() {
        return out;
    }
    let mut at = p.0.cast_const();
    loop {
        let one = unsafe { PCWSTR(at).to_string() }.unwrap_or_default();
        if one.is_empty() {
            return out;
        }
        at = unsafe { at.add(one.encode_utf16().count() + 1) };
        out.push(one);
    }
}

/// Runs a "how big must the buffer be" call twice: once to learn the size,
/// once with room for it.
fn sized(
    mut call: impl FnMut(&mut [u8], &mut u32) -> windows::core::Result<()>,
) -> Option<Vec<u64>> {
    let mut needed = 0u32;
    let mut buf: Vec<u64> = vec![0; 64];
    loop {
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), buf.len() * 8) };
        match call(bytes, &mut needed) {
            Ok(()) => return Some(buf),
            Err(_) if needed as usize > buf.len() * 8 => {
                buf = vec![0; (needed as usize).div_ceil(8)];
            }
            Err(_) => return None,
        }
    }
}

fn state_of(raw: SERVICE_STATUS_PROCESS) -> State {
    match raw.dwCurrentState {
        SERVICE_START_PENDING => State::Starting,
        SERVICE_STOP_PENDING => State::Stopping,
        SERVICE_RUNNING => State::Running,
        SERVICE_CONTINUE_PENDING => State::Continuing,
        SERVICE_PAUSE_PENDING => State::Pausing,
        SERVICE_PAUSED => State::Paused,
        _ => State::Stopped,
    }
}

fn read_config(scm: &Handle, name: &str) -> Option<Config> {
    let service = open(scm, name, SERVICE_QUERY_CONFIG).ok()?;
    let buf = sized(|b, needed| unsafe {
        QueryServiceConfigW(
            service.0,
            Some(b.as_mut_ptr().cast()),
            b.len() as u32,
            needed,
        )
    })?;
    let raw = unsafe { &*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let dependencies = strings(raw.lpDependencies);
    let config2 = |level| {
        sized(|b, needed| unsafe { QueryServiceConfig2W(service.0, level, Some(b), needed) })
    };
    let description = config2(SERVICE_CONFIG_DESCRIPTION)
        .map(|b| text(unsafe { &*b.as_ptr().cast::<SERVICE_DESCRIPTIONW>() }.lpDescription))
        .unwrap_or_default();
    let delayed = config2(SERVICE_CONFIG_DELAYED_AUTO_START_INFO).is_some_and(|b| {
        unsafe { &*b.as_ptr().cast::<SERVICE_DELAYED_AUTO_START_INFO>() }
            .fDelayedAutostart
            .as_bool()
    });
    let triggered = config2(SERVICE_CONFIG_TRIGGER_INFO)
        .is_some_and(|b| unsafe { &*b.as_ptr().cast::<SERVICE_TRIGGER_INFO>() }.cTriggers > 0);
    Some(Config {
        start: StartType::from_raw(raw.dwStartType, delayed),
        triggered,
        binary: text(raw.lpBinaryPathName),
        account: text(raw.lpServiceStartName),
        description,
        dependencies,
    })
}

/// What a stopped service's `exit_code` (a Win32 error) means.
pub fn exit_text(code: u32) -> String {
    windows::core::Error::from(windows::Win32::Foundation::WIN32_ERROR(code).to_hresult())
        .message()
        .trim()
        .to_string()
}

/// Every Win32 service with its state; configurations come from `configs`,
/// read for the services it does not know yet.
pub(super) fn list(configs: &mut HashMap<Arc<str>, Option<Arc<Config>>>) -> ServiceList {
    let started = Instant::now();
    let scm = match manager(SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE) {
        Ok(scm) => scm,
        Err(error) => {
            return ServiceList {
                error: Some(error),
                ..Default::default()
            };
        }
    };
    let mut count = 0u32;
    let mut resume = 0u32;
    let buf = sized(|b, needed| {
        resume = 0;
        unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32
                    | ENUM_SERVICE_TYPE(SERVICE_USER_SERVICE | SERVICE_USERSERVICE_INSTANCE),
                SERVICE_STATE_ALL,
                Some(b),
                needed,
                &mut count,
                Some(&mut resume),
                PCWSTR::null(),
            )
        }
    });
    let Some(buf) = buf else {
        return ServiceList {
            error: Some(windows::core::Error::from_thread().message()),
            ..Default::default()
        };
    };
    let raw = unsafe {
        std::slice::from_raw_parts(
            buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
            count as usize,
        )
    };
    let mut services = Vec::with_capacity(raw.len());
    for entry in raw {
        let name: Arc<str> = text(entry.lpServiceName).into();
        let status = entry.ServiceStatusProcess;
        let kind = status.dwServiceType.0;
        let config = configs
            .entry(name.clone())
            .or_insert_with(|| read_config(&scm, &name).map(Arc::new))
            .clone();
        services.push(Service {
            display: text(entry.lpDisplayName).into(),
            state: state_of(status),
            pid: status.dwProcessId,
            shared: kind & SERVICE_WIN32_SHARE_PROCESS.0 != 0,
            user_template: kind & SERVICE_USER_SERVICE != 0
                && kind & SERVICE_USERSERVICE_INSTANCE == 0,
            accepts_stop: status.dwControlsAccepted & SERVICE_ACCEPT_STOP != 0,
            accepts_pause: status.dwControlsAccepted & SERVICE_ACCEPT_PAUSE_CONTINUE != 0,
            exit_code: status.dwWin32ExitCode,
            config,
            name,
        });
    }
    ServiceList {
        services,
        error: None,
        took: started.elapsed(),
    }
}

fn status(service: &Handle) -> Option<SERVICE_STATUS_PROCESS> {
    let mut raw = SERVICE_STATUS_PROCESS::default();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut raw as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    let mut needed = 0;
    unsafe { QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) }
        .ok()?;
    Some(raw)
}

/// Waits for a pending control to settle, up to `limit`.
fn settle(service: &Handle, want: State, limit: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let Some(now) = status(service) else {
            return Err("its state could not be read".into());
        };
        let state = state_of(now);
        if state == want {
            return Ok(());
        }
        // Gave up starting: say why if it said.
        if want == State::Running && state == State::Stopped {
            return Err(stopped_because(now));
        }
        if started.elapsed() > limit {
            return Err(format!(
                "it is still {} after {} s",
                state.label().to_lowercase(),
                limit.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn stopped_because(raw: SERVICE_STATUS_PROCESS) -> String {
    match raw.dwWin32ExitCode {
        0 => "it stopped again at once".into(),
        // ERROR_SERVICE_SPECIFIC_ERROR: its own code.
        1066 => format!(
            "it stopped again with its own error code {}",
            raw.dwServiceSpecificExitCode
        ),
        code => format!(
            "it stopped again: {}",
            error_text(&windows::core::Error::from(
                windows::Win32::Foundation::WIN32_ERROR(code).to_hresult()
            ))
        ),
    }
}

/// The running services that depend on this one, in the order to stop them.
fn running_dependents(scm: &Handle, name: &str) -> Vec<String> {
    let Ok(service) = open(scm, name, 0x8 /* SERVICE_ENUMERATE_DEPENDENTS */) else {
        return Vec::new();
    };
    let mut count = 0u32;
    let buf = sized(|b, needed| unsafe {
        EnumDependentServicesW(
            service.0,
            SERVICE_ACTIVE,
            Some(b.as_mut_ptr().cast()),
            b.len() as u32,
            needed,
            &mut count,
        )
    });
    let Some(buf) = buf else {
        return Vec::new();
    };
    let raw = unsafe {
        std::slice::from_raw_parts(buf.as_ptr().cast::<ENUM_SERVICE_STATUSW>(), count as usize)
    };
    raw.iter().map(|s| text(s.lpServiceName)).collect()
}

const STOP_LIMIT: Duration = Duration::from_secs(30);
const START_LIMIT: Duration = Duration::from_secs(30);

fn stop_one(scm: &Handle, name: &str) -> windows::core::Result<()> {
    let service = open(scm, name, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let mut raw = SERVICE_STATUS::default();
    match unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut raw) } {
        Ok(()) => {}
        // Stopped already: fine.
        Err(e) if e.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return Ok(()),
        Err(e) => return Err(e),
    }
    settle(&service, State::Stopped, STOP_LIMIT)
        .map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e))
}

fn start_one(scm: &Handle, name: &str) -> Result<(), String> {
    let service =
        open(scm, name, SERVICE_START | SERVICE_QUERY_STATUS).map_err(|e| error_text(&e))?;
    unsafe { StartServiceW(service.0, None) }.map_err(|e| error_text(&e))?;
    settle(&service, State::Running, START_LIMIT)
}

/// Stops `name`, its running dependents first when allowed. Returns the
/// dependents it stopped, or the error and the dependents in the way.
fn stop(scm: &Handle, name: &str, dependents: bool) -> Result<Vec<String>, (String, Vec<String>)> {
    let others = running_dependents(scm, name);
    if !others.is_empty() && !dependents {
        return Err((
            format!(
                "{} other service{} depend{} on it",
                others.len(),
                if others.len() == 1 { "" } else { "s" },
                if others.len() == 1 { "s" } else { "" }
            ),
            others,
        ));
    }
    for other in &others {
        stop_one(scm, other).map_err(|e| {
            (
                format!("{other} did not stop: {}", error_text(&e)),
                Vec::new(),
            )
        })?;
    }
    match stop_one(scm, name) {
        Ok(()) => Ok(others),
        Err(e) if e.code() == ERROR_DEPENDENT_SERVICES_RUNNING.to_hresult() => {
            let others = running_dependents(scm, name);
            Err((error_text(&e), others))
        }
        Err(e) => Err((error_text(&e), Vec::new())),
    }
}

fn set_start(scm: &Handle, name: &str, start: StartType) -> Result<(), String> {
    let service = open(scm, name, SERVICE_CHANGE_CONFIG).map_err(|e| error_text(&e))?;
    unsafe {
        ChangeServiceConfigW(
            service.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            start.raw(),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    }
    .map_err(|e| error_text(&e))?;
    if matches!(start, StartType::Automatic | StartType::AutomaticDelayed) {
        let info = SERVICE_DELAYED_AUTO_START_INFO {
            fDelayedAutostart: (start == StartType::AutomaticDelayed).into(),
        };
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                Some((&info as *const SERVICE_DELAYED_AUTO_START_INFO).cast()),
            )
        }
        .map_err(|e| error_text(&e))?;
    }
    Ok(())
}

fn pause(scm: &Handle, name: &str, resume: bool) -> Result<(), String> {
    let service = open(scm, name, SERVICE_PAUSE_CONTINUE | SERVICE_QUERY_STATUS)
        .map_err(|e| error_text(&e))?;
    let mut raw = SERVICE_STATUS::default();
    let control = if resume {
        SERVICE_CONTROL_CONTINUE
    } else {
        SERVICE_CONTROL_PAUSE
    };
    unsafe { ControlService(service.0, control, &mut raw) }.map_err(|e| error_text(&e))?;
    let want = if resume {
        State::Running
    } else {
        State::Paused
    };
    settle(&service, want, STOP_LIMIT)
}

/// Carries out one control and words the outcome.
pub(super) fn control(
    name: &str,
    display: &str,
    op: ServiceOp,
) -> (Result<String, String>, Vec<String>) {
    let scm = match manager(SC_MANAGER_CONNECT) {
        Ok(scm) => scm,
        Err(e) => {
            return (
                Err(format!("Could not reach the service manager: {e}")),
                Vec::new(),
            );
        }
    };
    let also = |others: &[String]| {
        if others.is_empty() {
            String::new()
        } else {
            format!(" (and {})", others.join(", "))
        }
    };
    match op {
        ServiceOp::Start => (
            start_one(&scm, name)
                .map(|_| format!("Started {display}"))
                .map_err(|e| format!("Could not start {display}: {e}")),
            Vec::new(),
        ),
        ServiceOp::Stop { dependents } => match stop(&scm, name, dependents) {
            Ok(others) => (
                Ok(format!("Stopped {display}{}", also(&others))),
                Vec::new(),
            ),
            Err((e, others)) => (Err(format!("Could not stop {display}: {e}")), others),
        },
        ServiceOp::Restart { dependents } => match stop(&scm, name, dependents) {
            Ok(others) => {
                if let Err(e) = start_one(&scm, name) {
                    return (
                        Err(format!("Stopped {display} but could not start it: {e}")),
                        Vec::new(),
                    );
                }
                // Started again in the reverse of the order they stopped in.
                let failed: Vec<&String> = others
                    .iter()
                    .rev()
                    .filter(|o| start_one(&scm, o).is_err())
                    .collect();
                if failed.is_empty() {
                    (
                        Ok(format!("Restarted {display}{}", also(&others))),
                        Vec::new(),
                    )
                } else {
                    (
                        Err(format!(
                            "Restarted {display}, but these did not start again: {}",
                            failed
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                        Vec::new(),
                    )
                }
            }
            Err((e, others)) => (Err(format!("Could not restart {display}: {e}")), others),
        },
        ServiceOp::Pause => (
            pause(&scm, name, false)
                .map(|_| format!("Paused {display}"))
                .map_err(|e| format!("Could not pause {display}: {e}")),
            Vec::new(),
        ),
        ServiceOp::Continue => (
            pause(&scm, name, true)
                .map(|_| format!("Resumed {display}"))
                .map_err(|e| format!("Could not resume {display}: {e}")),
            Vec::new(),
        ),
        ServiceOp::SetStart(start) => (
            set_start(&scm, name, start)
                .map(|_| format!("{display} now starts: {}", start.label().to_lowercase()))
                .map_err(|e| format!("Could not change how {display} starts: {e}")),
            Vec::new(),
        ),
    }
}

// Creating, changing and deleting services.

/// The standard right to delete an object.
const DELETE: u32 = 0x0001_0000;
/// A failure count that never starts again (INFINITE).
const NEVER: u32 = u32::MAX;
const DAY: u32 = 24 * 60 * 60;

const OWN_KEEP: &str = "It is one of Windows' own services (its program is in the Windows \
                        folder): without it Windows may not start or update. Set it to Disabled \
                        instead.";
const OWN_FIXED: &str =
    "It is one of Windows' own services: its program and account stay as Windows set them.";
const PER_USER_KEEP: &str =
    "A per-user service: Windows makes and removes its copies for each user itself.";

impl ErrorControl {
    fn from_raw(raw: SERVICE_ERROR) -> Self {
        match raw {
            SERVICE_ERROR_IGNORE => ErrorControl::Ignore,
            SERVICE_ERROR_SEVERE => ErrorControl::Severe,
            SERVICE_ERROR_CRITICAL => ErrorControl::Critical,
            _ => ErrorControl::Normal,
        }
    }

    fn raw(self) -> SERVICE_ERROR {
        match self {
            ErrorControl::Ignore => SERVICE_ERROR_IGNORE,
            ErrorControl::Normal => SERVICE_ERROR_NORMAL,
            ErrorControl::Severe => SERVICE_ERROR_SEVERE,
            ErrorControl::Critical => SERVICE_ERROR_CRITICAL,
        }
    }
}

impl FailureAction {
    fn from_raw(raw: SC_ACTION_TYPE) -> Self {
        match raw {
            SC_ACTION_RESTART => FailureAction::Restart,
            SC_ACTION_RUN_COMMAND => FailureAction::RunProgram,
            SC_ACTION_REBOOT => FailureAction::Reboot,
            _ => FailureAction::Nothing,
        }
    }

    fn raw(self) -> SC_ACTION_TYPE {
        match self {
            FailureAction::Nothing => SC_ACTION_NONE,
            FailureAction::Restart => SC_ACTION_RESTART,
            FailureAction::RunProgram => SC_ACTION_RUN_COMMAND,
            FailureAction::Reboot => SC_ACTION_REBOOT,
        }
    }
}

impl Account {
    fn from_raw(name: &str) -> Self {
        let name = name.trim();
        match name.to_ascii_lowercase().as_str() {
            "" | "localsystem" | r".\localsystem" | r"nt authority\system" => Account::System,
            r"nt authority\localservice" | r"nt authority\local service" => Account::LocalService,
            r"nt authority\networkservice" | r"nt authority\network service" => {
                Account::NetworkService
            }
            _ => Account::User(name.to_string()),
        }
    }

    /// The name the service manager takes.
    fn raw(&self) -> &str {
        match self {
            Account::System => "LocalSystem",
            Account::LocalService => r"NT AUTHORITY\LocalService",
            Account::NetworkService => r"NT AUTHORITY\NetworkService",
            Account::User(name) => name.trim(),
        }
    }
}

/// A double-nul-terminated list for the service manager; empty, it clears
/// the list.
fn multi(list: &[String]) -> Vec<u16> {
    let mut out = Vec::new();
    for one in list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        out.extend(one.encode_utf16());
        out.push(0);
    }
    out.push(0);
    if out.len() == 1 {
        out.push(0);
    }
    out
}

fn pcwstr(text: &Option<Vec<u16>>) -> PCWSTR {
    text.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()))
}

pub(super) fn canonical(name: &str, _own: bool) -> String {
    name.trim().to_string()
}

pub(super) fn check_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give it a name".into());
    }
    if name.chars().count() > 256 {
        return Err("A service's name has at most 256 characters".into());
    }
    if name.contains(['/', '\\']) {
        return Err("A service's name cannot hold / or \\".into());
    }
    Ok(())
}

/// The Windows folder.
fn windows_dir() -> String {
    std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".into())
}

/// `text` with its %VARIABLES% filled in.
fn expand(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let source = wide(text);
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
    if needed == 0 {
        return text.to_string();
    }
    let mut buf = vec![0u16; needed as usize];
    let got = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut buf)) };
    if got == 0 || got as usize > buf.len() {
        return text.to_string();
    }
    from_wide(&buf)
}

/// The file a service's command line runs, as a whole path: variables
/// filled in, `\SystemRoot\` and `\??\` taken apart, and a path relative to
/// the Windows folder (as drivers have) put under it.
fn program_file(binary: &str) -> Option<String> {
    let binary = expand(binary.trim());
    let path = match binary.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?.to_string(),
        None => {
            let lower = binary.to_ascii_lowercase();
            let end = lower
                .find(".exe")
                .map(|i| i + 4)
                .or_else(|| binary.find(' '))
                .unwrap_or(binary.len());
            binary[..end].trim().to_string()
        }
    };
    if path.is_empty() {
        return None;
    }
    let starts = |prefix: &str| {
        path.get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    };
    let dir = windows_dir();
    Some(if starts(r"\SystemRoot\") {
        format!(
            r"{}\{}",
            dir.trim_end_matches('\\'),
            &path[r"\SystemRoot\".len()..]
        )
    } else if let Some(rest) = path.strip_prefix(r"\??\") {
        rest.to_string()
    } else if !path.contains(':') && !path.starts_with('\\') {
        format!(r"{}\{path}", dir.trim_end_matches('\\'))
    } else {
        path
    })
}

/// One of Windows' own services: its program is in the Windows folder.
fn windows_own(binary: &str) -> bool {
    let Some(file) = program_file(binary) else {
        return false;
    };
    let dir = windows_dir().trim_end_matches('\\').to_ascii_lowercase();
    file.to_ascii_lowercase().starts_with(&format!("{dir}\\"))
}

fn read_recovery(service: &Handle) -> Recovery {
    let config2 = |level| {
        sized(|b, needed| unsafe { QueryServiceConfig2W(service.0, level, Some(b), needed) })
    };
    let mut recovery = Recovery {
        reset_days: Some(0),
        ..Default::default()
    };
    if let Some(buf) = config2(SERVICE_CONFIG_FAILURE_ACTIONS) {
        let raw = unsafe { &*buf.as_ptr().cast::<SERVICE_FAILURE_ACTIONSW>() };
        recovery.reset_days = (raw.dwResetPeriod != NEVER).then_some(raw.dwResetPeriod / DAY);
        recovery.program = text(raw.lpCommand);
        let actions: &[SC_ACTION] = if raw.cActions == 0 || raw.lpsaActions.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(raw.lpsaActions, raw.cActions as usize) }
        };
        // Fewer than three: the last one repeats.
        if let Some(last) = actions.last() {
            for (i, action) in recovery.actions.iter_mut().enumerate() {
                *action = FailureAction::from_raw(actions.get(i).unwrap_or(last).Type);
            }
        }
        if let Some(first) = actions.iter().find(|a| a.Type != SC_ACTION_NONE) {
            recovery.delay = Duration::from_millis(first.Delay as u64);
        }
    }
    if let Some(buf) = config2(SERVICE_CONFIG_FAILURE_ACTIONS_FLAG) {
        recovery.also = unsafe { &*buf.as_ptr().cast::<SERVICE_FAILURE_ACTIONS_FLAG>() }
            .fFailureActionsOnNonCrashFailures
            .as_bool();
    }
    recovery
}

pub(super) fn detail(name: &str) -> Result<ServiceDetail, String> {
    let scm = manager(SC_MANAGER_CONNECT)
        .map_err(|e| format!("Could not reach the service manager: {e}"))?;
    let service = open(&scm, name, SERVICE_QUERY_CONFIG).map_err(|e| error_text(&e))?;
    let buf = sized(|b, needed| unsafe {
        QueryServiceConfigW(
            service.0,
            Some(b.as_mut_ptr().cast()),
            b.len() as u32,
            needed,
        )
    })
    .ok_or_else(|| error_text(&windows::core::Error::from_thread()))?;
    let raw = unsafe { &*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let kind = raw.dwServiceType.0;
    if kind & SERVICE_DRIVER.0 != 0 {
        return Err("It is a driver, not a service: Anabolic does not change drivers".into());
    }
    let config2 = |level| {
        sized(|b, needed| unsafe { QueryServiceConfig2W(service.0, level, Some(b), needed) })
    };
    let description = config2(SERVICE_CONFIG_DESCRIPTION)
        .map(|b| text(unsafe { &*b.as_ptr().cast::<SERVICE_DESCRIPTIONW>() }.lpDescription))
        .unwrap_or_default();
    let delayed = config2(SERVICE_CONFIG_DELAYED_AUTO_START_INFO).is_some_and(|b| {
        unsafe { &*b.as_ptr().cast::<SERVICE_DELAYED_AUTO_START_INFO>() }
            .fDelayedAutostart
            .as_bool()
    });
    let program = text(raw.lpBinaryPathName);
    let own = windows_own(&program);
    let per_user = kind & (SERVICE_USER_SERVICE | SERVICE_USERSERVICE_INSTANCE) != 0;
    let spec = ServiceSpec {
        name: name.to_string(),
        display: text(raw.lpDisplayName),
        description,
        program,
        start: StartType::from_raw(raw.dwStartType, delayed),
        account: Account::from_raw(&text(raw.lpServiceStartName)),
        password: None,
        dependencies: strings(raw.lpDependencies),
        error_control: ErrorControl::from_raw(raw.dwErrorControl),
        recovery: read_recovery(&service),
        own: false,
    };
    Ok(ServiceDetail {
        spec,
        keep: if per_user {
            Some(PER_USER_KEEP.into())
        } else if own {
            Some(OWN_KEEP.into())
        } else {
            None
        },
        fixed: (own || per_user).then(|| OWN_FIXED.into()),
        source: format!(r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\{name}"),
        drop_in: false,
    })
}

fn set_description(service: &Handle, description: &str) -> Result<(), String> {
    let mut text = wide(description.trim());
    let info = SERVICE_DESCRIPTIONW {
        lpDescription: PWSTR(text.as_mut_ptr()),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some((&info as *const SERVICE_DESCRIPTIONW).cast()),
        )
    }
    .map_err(|e| error_text(&e))
}

fn set_delayed(service: &Handle, delayed: bool) -> Result<(), String> {
    let info = SERVICE_DELAYED_AUTO_START_INFO {
        fDelayedAutostart: delayed.into(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            Some((&info as *const SERVICE_DELAYED_AUTO_START_INFO).cast()),
        )
    }
    .map_err(|e| error_text(&e))
}

/// Writes a service's failure actions. A "restart the computer" action
/// kept from before needs the shutdown privilege to be written again.
fn set_recovery(service: &Handle, recovery: &Recovery) -> Result<(), String> {
    if recovery.actions.contains(&FailureAction::Reboot) {
        crate::hardening::enable_privilege(windows::core::w!("SeShutdownPrivilege")).map_err(
            |e| format!("its \"restart the computer\" action needs the shutdown privilege: {e}"),
        )?;
    }
    let delay = recovery.delay.as_millis().min(u32::MAX as u128) as u32;
    let mut actions: Vec<SC_ACTION> = recovery
        .actions
        .iter()
        .map(|a| SC_ACTION {
            Type: a.raw(),
            Delay: if *a == FailureAction::Nothing {
                0
            } else {
                delay
            },
        })
        .collect();
    // Nothing at all, as the Services console writes "take no action".
    if recovery
        .actions
        .iter()
        .all(|a| *a == FailureAction::Nothing)
    {
        actions.clear();
    }
    let mut command = wide(recovery.program.trim());
    let info = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: recovery.reset_days.map_or(NEVER, |d| d.saturating_mul(DAY)),
        // The reboot message stays as it is.
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR(command.as_mut_ptr()),
        cActions: actions.len() as u32,
        // Never null, even for none: null would leave the actions as they were.
        lpsaActions: actions.as_mut_ptr(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            Some((&info as *const SERVICE_FAILURE_ACTIONSW).cast()),
        )
    }
    .map_err(|e| error_text(&e))?;
    let flag = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: recovery.also.into(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            Some((&flag as *const SERVICE_FAILURE_ACTIONS_FLAG).cast()),
        )
    }
    .map_err(|e| error_text(&e))
}

/// The right Windows asks of an account a service logs on as. The built-in
/// and per-service (NT SERVICE\…) accounts have it already.
fn needs_logon_right(account: &str) -> bool {
    let lower = account.trim().to_ascii_lowercase();
    !(lower.starts_with(r"nt authority\") || lower.starts_with(r"nt service\"))
}

fn nt_text(status: windows::Win32::Foundation::NTSTATUS) -> String {
    let code = unsafe { LsaNtStatusToWinError(status) };
    error_text(&windows::core::Error::from(
        windows::Win32::Foundation::WIN32_ERROR(code).to_hresult(),
    ))
}

/// Lets `account` log on as a service, as the Services console does when
/// an account is chosen: without the right the service does not start
/// (error 1069).
fn allow_service_logon(account: &str) -> Result<(), String> {
    let name = account.trim();
    let name = name.strip_prefix(r".\").unwrap_or(name);
    let name = wide(name);
    let (mut sid_len, mut domain_len) = (0u32, 0u32);
    let mut kind = SID_NAME_USE::default();
    let _ = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            None,
            &mut sid_len,
            None,
            &mut domain_len,
            &mut kind,
        )
    };
    if sid_len == 0 {
        return Err("there is no such account".into());
    }
    let mut sid = vec![0u8; sid_len as usize];
    let mut domain = vec![0u16; domain_len.max(1) as usize];
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            Some(PSID(sid.as_mut_ptr().cast())),
            &mut sid_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    }
    .map_err(|e| error_text(&e))?;
    let attributes = LSA_OBJECT_ATTRIBUTES {
        Length: size_of::<LSA_OBJECT_ATTRIBUTES>() as u32,
        ..Default::default()
    };
    let mut policy = LSA_HANDLE::default();
    let status = unsafe {
        LsaOpenPolicy(
            None,
            &attributes,
            (POLICY_CREATE_ACCOUNT | POLICY_LOOKUP_NAMES) as u32,
            &mut policy,
        )
    };
    if status.0 < 0 {
        return Err(nt_text(status));
    }
    let mut right = wide("SeServiceLogonRight");
    let bytes = ((right.len() - 1) * 2) as u16;
    let rights = [LSA_UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes + 2,
        Buffer: PWSTR(right.as_mut_ptr()),
    }];
    let status = unsafe { LsaAddAccountRights(policy, PSID(sid.as_mut_ptr().cast()), &rights) };
    unsafe {
        let _ = LsaClose(policy);
    }
    if status.0 < 0 {
        return Err(nt_text(status));
    }
    Ok(())
}

/// The right to log on as a service, given when the account is set; a
/// failure in words for the outcome, None when it went well.
fn logon_right(account: &Account) -> Option<String> {
    let Account::User(name) = account else {
        return None;
    };
    if !needs_logon_right(name) {
        return None;
    }
    allow_service_logon(name).err().map(|e| {
        format!(
            "{name} could not be given the right to log on as a service ({e}); it does not start \
             until it has it"
        )
    })
}

pub(super) fn create(spec: &ServiceSpec) -> ChangeOutcome {
    let name = canonical(&spec.name, false);
    let display = match spec.display.trim() {
        "" => name.clone(),
        shown => shown.to_string(),
    };
    let scm = match manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE) {
        Ok(scm) => scm,
        Err(e) => return ChangeOutcome::refused(format!("Could not create {display}: {e}")),
    };
    let w_name = wide(&name);
    let w_display = wide(&display);
    let program = wide(spec.program.trim());
    let dependencies = multi(&spec.dependencies);
    let account = wide(spec.account.raw());
    let password = wide(spec.password.as_ref().map_or("", |p| p.0.as_str()));
    let access = SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG | SERVICE_START;
    let service = unsafe {
        CreateServiceW(
            scm.0,
            PCWSTR(w_name.as_ptr()),
            PCWSTR(w_display.as_ptr()),
            access,
            SERVICE_WIN32_OWN_PROCESS,
            spec.start.raw(),
            spec.error_control.raw(),
            PCWSTR(program.as_ptr()),
            PCWSTR::null(),
            None,
            PCWSTR(dependencies.as_ptr()),
            PCWSTR(account.as_ptr()),
            PCWSTR(password.as_ptr()),
        )
    };
    let service = match service {
        Ok(service) => Handle(service),
        Err(e) => {
            return ChangeOutcome::refused(format!(
                "Could not create {display}: {}",
                error_text(&e)
            ));
        }
    };
    let mut missing = Vec::new();
    if !spec.description.trim().is_empty()
        && let Err(e) = set_description(&service, &spec.description)
    {
        missing.push(format!("its description ({e})"));
    }
    if spec.start == StartType::AutomaticDelayed
        && let Err(e) = set_delayed(&service, true)
    {
        missing.push(format!("its delayed start ({e})"));
    }
    if (spec
        .recovery
        .actions
        .iter()
        .any(|a| *a != FailureAction::Nothing)
        || spec.recovery.also)
        && let Err(e) = set_recovery(&service, &spec.recovery)
    {
        missing.push(format!("what it does when it fails ({e})"));
    }
    missing.extend(logon_right(&spec.account));
    if missing.is_empty() {
        ChangeOutcome::done(format!("Created {display}"))
    } else {
        ChangeOutcome::partly(format!(
            "Created {display}, but not all of it: {}",
            missing.join("; ")
        ))
    }
}

pub(super) fn configure(before: &ServiceSpec, after: &ServiceSpec) -> ChangeOutcome {
    let display = match after.display.trim() {
        "" => before.display.clone(),
        shown => shown.to_string(),
    };
    let program_changed = before.program.trim() != after.program.trim();
    let account_changed = before.account != after.account || after.password.is_some();
    if (program_changed || account_changed) && windows_own(&before.program) {
        return ChangeOutcome::refused(format!("Could not change {display}: {OWN_FIXED}"));
    }
    let scm = match manager(SC_MANAGER_CONNECT) {
        Ok(scm) => scm,
        Err(e) => return ChangeOutcome::refused(format!("Could not change {display}: {e}")),
    };
    let recovery_changed = before.recovery != after.recovery;
    let mut access = SERVICE_CHANGE_CONFIG | SERVICE_QUERY_STATUS;
    // Writing a "restart the service" action takes the right to start it.
    if recovery_changed && after.recovery.actions.contains(&FailureAction::Restart) {
        access |= SERVICE_START;
    }
    let service = match open(&scm, &before.name, access) {
        Ok(service) => service,
        Err(e) => {
            return ChangeOutcome::refused(format!(
                "Could not change {display}: {}",
                error_text(&e)
            ));
        }
    };
    let start_changed = before.start.raw() != after.start.raw();
    let error_changed = before.error_control != after.error_control;
    let dependencies_changed = before.dependencies != after.dependencies;
    let display_changed = before.display.trim() != after.display.trim();
    let mut written = false;
    if start_changed
        || error_changed
        || program_changed
        || dependencies_changed
        || account_changed
        || display_changed
    {
        let program = program_changed.then(|| wide(after.program.trim()));
        let dependencies = dependencies_changed.then(|| multi(&after.dependencies));
        let account = account_changed.then(|| wide(after.account.raw()));
        let password =
            account_changed.then(|| wide(after.password.as_ref().map_or("", |p| p.0.as_str())));
        let shown = display_changed.then(|| wide(after.display.trim()));
        let result = unsafe {
            ChangeServiceConfigW(
                service.0,
                ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
                if start_changed {
                    after.start.raw()
                } else {
                    SERVICE_START_TYPE(SERVICE_NO_CHANGE)
                },
                if error_changed {
                    after.error_control.raw()
                } else {
                    SERVICE_ERROR(SERVICE_NO_CHANGE)
                },
                pcwstr(&program),
                PCWSTR::null(),
                None,
                pcwstr(&dependencies),
                pcwstr(&account),
                pcwstr(&password),
                pcwstr(&shown),
            )
        };
        if let Err(e) = result {
            return ChangeOutcome::refused(format!(
                "Could not change {display}: {}",
                error_text(&e)
            ));
        }
        written = true;
    }
    let mut missing = Vec::new();
    let mut step = |what: &str, result: Result<(), String>| match result {
        Ok(()) => written = true,
        Err(e) => missing.push(format!("{what} ({e})")),
    };
    if before.description.trim() != after.description.trim() {
        step(
            "its description",
            set_description(&service, &after.description),
        );
    }
    if before.start != after.start
        && matches!(
            after.start,
            StartType::Automatic | StartType::AutomaticDelayed
        )
    {
        step(
            "its delayed start",
            set_delayed(&service, after.start == StartType::AutomaticDelayed),
        );
    }
    if recovery_changed {
        step(
            "what it does when it fails",
            set_recovery(&service, &after.recovery),
        );
    }
    if account_changed {
        missing.extend(logon_right(&after.account));
    }
    // A running service goes on with what it started with.
    let running = status(&service).is_some_and(|s| state_of(s) != State::Stopped);
    let later = if running && (program_changed || account_changed || dependencies_changed) {
        "; it runs with the new program and account from its next start"
    } else {
        ""
    };
    match (missing.is_empty(), written) {
        (true, _) => ChangeOutcome::done(format!("Changed {display}{later}")),
        (false, true) => ChangeOutcome::partly(format!(
            "Changed {display}, but not all of it: {}",
            missing.join("; ")
        )),
        (false, false) => ChangeOutcome::refused(format!(
            "Could not change {display}: {}",
            missing.join("; ")
        )),
    }
}

pub(super) fn delete(name: &str, display: &str, stop_first: bool) -> ChangeOutcome {
    let refused =
        |why: String| ChangeOutcome::refused(format!("Could not delete {display}: {why}"));
    match detail(name) {
        Ok(found) => {
            if let Some(keep) = found.keep {
                return refused(keep);
            }
        }
        Err(e) => return refused(e),
    }
    let scm = match manager(SC_MANAGER_CONNECT) {
        Ok(scm) => scm,
        Err(e) => return refused(e),
    };
    let service = match open(&scm, name, DELETE | SERVICE_QUERY_STATUS) {
        Ok(service) => service,
        Err(e) => return refused(error_text(&e)),
    };
    let mut stopped = false;
    if status(&service).is_some_and(|s| state_of(s) != State::Stopped) {
        if !stop_first {
            return refused("it is running; stop it first".into());
        }
        match stop(&scm, name, false) {
            Ok(_) => stopped = true,
            Err((_, others)) if !others.is_empty() => {
                return refused(format!(
                    "these running services need it: {}",
                    others.join(", ")
                ));
            }
            Err((e, _)) => return refused(format!("it did not stop: {e}")),
        }
    }
    if let Err(e) = unsafe { DeleteService(service.0) } {
        let already = e.code() == ERROR_SERVICE_MARKED_FOR_DELETE.to_hresult();
        if !already {
            return if stopped {
                ChangeOutcome::partly(format!(
                    "Stopped {display}, but could not delete it: {}",
                    error_text(&e)
                ))
            } else {
                refused(error_text(&e))
            };
        }
    }
    drop(service);
    // Gone, or only marked: then whoever holds it open keeps it until they
    // let go.
    match open(&scm, name, SERVICE_QUERY_STATUS) {
        Err(e) if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => {
            ChangeOutcome::done(format!("Deleted {display}"))
        }
        _ => ChangeOutcome::done(format!(
            "{display} is marked for deletion: Windows removes it once nothing holds it open (the \
             Services console, for one), or when the computer restarts"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_windows_own_services_from_others() {
        let dir = windows_dir();
        assert!(windows_own(&format!(
            r"{dir}\system32\svchost.exe -k netsvcs -p"
        )));
        assert!(windows_own(r"%SystemRoot%\System32\spoolsv.exe"));
        assert!(windows_own(r"\SystemRoot\System32\drivers\tcpip.sys"));
        assert!(windows_own(r"system32\DRIVERS\disk.sys"));
        assert!(windows_own(&format!(r#""{dir}\System32\lsass.exe""#)));
        assert!(!windows_own(
            r#""C:\Program Files\Mozilla Maintenance Service\maintenanceservice.exe""#
        ));
        assert!(!windows_own(r"C:\ProgramData\App\svc.exe --run"));
        // A folder that only starts like the Windows folder's name.
        assert!(!windows_own(&format!(r"{dir}Apps\svc.exe")));
        assert_eq!(multi(&[]), vec![0, 0]);
        assert_eq!(
            multi(&["a".into(), " ".into(), "+b".into()]),
            vec![97, 0, 43, 98, 0, 0]
        );
        assert_eq!(Account::from_raw("LocalSystem"), Account::System);
        assert_eq!(
            Account::from_raw(r"NT AUTHORITY\LocalService"),
            Account::LocalService
        );
        assert_eq!(
            Account::from_raw(r".\someone"),
            Account::User(r".\someone".into())
        );
    }

    /// Creates, reads back, changes and deletes a throwaway service that is
    /// never started. Needs administrator rights; skips itself without.
    #[test]
    fn creates_changes_and_deletes_a_service() {
        if !crate::hardening::is_elevated() {
            eprintln!("skipped: needs administrator rights");
            return;
        }
        let name = format!("AnabolicTest{}", std::process::id());
        // A program that is not there (outside the Windows folder, or it
        // would count as Windows' own): started by mistake, it fails.
        let absent = format!(
            r"{}\AnabolicTest\absent.exe --never",
            std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())
        );
        let comspec = format!(r"{}\System32\cmd.exe /c exit 0", windows_dir());
        let spec = ServiceSpec {
            name: name.clone(),
            display: "Anabolic test service".into(),
            description: "Made and removed by Anabolic's tests.".into(),
            program: absent,
            start: StartType::Disabled,
            account: Account::LocalService,
            dependencies: vec!["RpcSs".into()],
            error_control: ErrorControl::Ignore,
            recovery: Recovery {
                actions: [
                    FailureAction::Restart,
                    FailureAction::RunProgram,
                    FailureAction::Nothing,
                ],
                delay: Duration::from_secs(120),
                reset_days: Some(2),
                program: comspec.clone(),
                also: true,
            },
            ..Default::default()
        };
        let made = create(&spec);
        // Whatever happens next, the service goes: deleted directly, past
        // any rule of Anabolic's own.
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(scm) = manager(SC_MANAGER_CONNECT)
                    && let Ok(service) = open(&scm, &self.0, DELETE)
                {
                    let _ = unsafe { DeleteService(service.0) };
                }
            }
        }
        let _cleanup = Cleanup(name.clone());
        assert_eq!(made.result, Ok("Created Anabolic test service".into()));
        let read = detail(&name).expect("it reads back");
        assert_eq!(read.keep, None);
        assert_eq!(read.fixed, None);
        let mut expected = spec.clone();
        expected.name = name.clone();
        assert_eq!(read.spec, expected);

        let mut after = read.spec.clone();
        after.display = "Anabolic test service, changed".into();
        after.description = String::new();
        after.start = StartType::Manual;
        after.account = Account::NetworkService;
        after.dependencies.clear();
        after.error_control = ErrorControl::Normal;
        after.recovery = Recovery {
            reset_days: Some(0),
            ..Default::default()
        };
        let changed = configure(&read.spec, &after);
        assert!(changed.result.is_ok(), "{:?}", changed.result);
        let again = detail(&name).expect("it reads back again");
        assert_eq!(again.spec, after);

        let gone = delete(&name, "Anabolic test service", false);
        assert!(gone.written);
        let said = gone.result.expect("it is deleted");
        // A test listing the services at the same moment holds it open for
        // a moment: then it is only marked, and goes once that lets go.
        assert!(
            said.starts_with("Deleted") || said.contains("marked for deletion"),
            "{said}"
        );
        if said.starts_with("Deleted") {
            assert!(detail(&name).is_err(), "it is gone");
        }
    }

    #[test]
    fn will_not_delete_windows_own() {
        // Read only: the refusal comes before anything is opened to delete.
        let refused = delete("RpcSs", "Remote Procedure Call (RPC)", false);
        assert!(!refused.written);
        assert!(
            refused
                .result
                .as_ref()
                .is_err_and(|e| e.contains("Windows' own")),
            "{:?}",
            refused.result
        );
        let read = detail("RpcSs").expect("RpcSs reads");
        assert!(read.keep.is_some() && read.fixed.is_some());
        let mut after = read.spec.clone();
        after.program = r"C:\elsewhere.exe".into();
        let refused = configure(&read.spec, &after);
        assert!(!refused.written, "{:?}", refused.result);
    }

    #[test]
    fn lists_services_with_their_hosts() {
        let mut configs = HashMap::new();
        let list = list(&mut configs);
        assert!(list.error.is_none(), "{:?}", list.error);
        // Every Windows install runs the RPC endpoint mapper in an svchost.
        let rpc = list
            .services
            .iter()
            .find(|s| &*s.name == "RpcEptMapper")
            .expect("RpcEptMapper is listed");
        assert_eq!(rpc.state, State::Running);
        assert_ne!(rpc.pid, 0);
        let config = rpc.config.as_ref().expect("its configuration is readable");
        assert!(config.binary.to_lowercase().contains("svchost"));
        assert!(!config.description.is_empty());
        assert!(!list.by_pid()[&rpc.pid].is_empty());
    }
}
