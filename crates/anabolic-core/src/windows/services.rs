//! Windows services: each one's state, start type and host process, and
//! starting, stopping and reconfiguring them. All of it goes to the Service
//! Control Manager over RPC and a stop can take half a minute, so it runs on
//! threads of its own, never the UI's.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_DEPENDENT_SERVICES_RUNNING, ERROR_SERVICE_NOT_ACTIVE};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    ENUM_SERVICE_STATUS_PROCESSW, ENUM_SERVICE_STATUSW, ENUM_SERVICE_TYPE, EnumDependentServicesW,
    EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
    QueryServiceConfig2W, QueryServiceConfigW, QueryServiceStatusEx, SC_ENUM_PROCESS_INFO,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO,
    SERVICE_ACCEPT_PAUSE_CONTINUE, SERVICE_ACCEPT_STOP, SERVICE_ACTIVE, SERVICE_AUTO_START,
    SERVICE_BOOT_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_CONFIG_TRIGGER_INFO, SERVICE_CONTINUE_PENDING,
    SERVICE_CONTROL_CONTINUE, SERVICE_CONTROL_PAUSE, SERVICE_CONTROL_STOP,
    SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_DISABLED,
    SERVICE_ERROR, SERVICE_NO_CHANGE, SERVICE_PAUSE_CONTINUE, SERVICE_PAUSE_PENDING,
    SERVICE_PAUSED, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
    SERVICE_START_PENDING, SERVICE_START_TYPE, SERVICE_STATE_ALL, SERVICE_STATUS,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOP_PENDING, SERVICE_SYSTEM_START,
    SERVICE_TRIGGER_INFO, SERVICE_WIN32, SERVICE_WIN32_SHARE_PROCESS, StartServiceW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::util::{error_text, wide};

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
    // A double-nul-terminated list.
    let mut dependencies = Vec::new();
    if !raw.lpDependencies.is_null() {
        let mut at = raw.lpDependencies.0.cast_const();
        loop {
            let one = unsafe { PCWSTR(at).to_string() }.unwrap_or_default();
            if one.is_empty() {
                break;
            }
            at = unsafe { at.add(one.encode_utf16().count() + 1) };
            dependencies.push(one);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
