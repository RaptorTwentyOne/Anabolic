// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! systemd's services, over D-Bus: the system manager's, and the signed-in
//! user's own (`systemctl --user`), whose names carry a " (user)" mark.
//!
//! Units the manager has loaded come with their state from one call; the
//! installed ones it has not loaded are read from their unit files, so
//! listing never makes systemd load anything. Starting, stopping and
//! changing how a system service starts go through polkit, which asks for
//! a password through the desktop's agent when it has to.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::{CacheProperties, MethodFlags};
use zbus::zvariant::OwnedObjectPath;

use super::*;

const SYSTEMD: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const UNIT: &str = "org.freedesktop.systemd1.Unit";
const SERVICE: &str = "org.freedesktop.systemd1.Service";

/// What marks a unit of the user's own manager in a service's name.
const USER_MARK: &str = " (user)";

/// One row of ListUnitsByPatterns.
type UnitRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

/// One command of ExecStart: path, arguments, and how it last ran.
type ExecCommand = (String, Vec<String>, bool, u64, u64, u64, u64, u32, i32, i32);

/// Which manager a unit belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    System,
    User,
}

impl Scope {
    fn connect(self) -> zbus::Result<Connection> {
        match self {
            Scope::System => Connection::system(),
            Scope::User => Connection::session(),
        }
    }
}

/// The unit name and manager behind a service's name.
fn unit_of(name: &str) -> (&str, Scope) {
    match name.strip_suffix(USER_MARK) {
        Some(unit) => (unit, Scope::User),
        None => (name, Scope::System),
    }
}

fn proxy<'a>(conn: &'a Connection, path: &'a str, interface: &'a str) -> zbus::Result<Proxy<'a>> {
    zbus::blocking::proxy::Builder::new(conn)
        .destination(SYSTEMD)?
        .path(path)?
        .interface(interface)?
        // One read each: no cache of every property, no change signals.
        .cache_properties(CacheProperties::No)
        .build()
}

/// A D-Bus failure in words: polkit's refusals as what they mean.
fn words(e: &zbus::Error) -> String {
    if let zbus::Error::MethodError(name, message, _) = e {
        return match name.as_str() {
            "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => {
                "the system did not allow it (no administrator password was given)".into()
            }
            "org.freedesktop.systemd1.NoSuchUnit" => "there is no such service".into(),
            _ => message
                .clone()
                .unwrap_or_else(|| name.trim_start_matches("org.freedesktop.").to_string()),
        };
    }
    e.to_string()
}

/// systemd's unit file states as start types: enabled units start at boot
/// (or sign-in), disabled ones when asked or needed, masked ones never;
/// static units have nothing to switch (they start only when another unit
/// needs them).
fn start_type(state: &str) -> StartType {
    match state {
        "enabled" | "enabled-runtime" | "linked" | "linked-runtime" => StartType::Automatic,
        "masked" | "masked-runtime" => StartType::Disabled,
        "static" | "generated" | "transient" => StartType::System,
        _ => StartType::Manual,
    }
}

fn state_of(active: &str) -> State {
    match active {
        "active" | "reloading" | "refreshing" => State::Running,
        "activating" => State::Starting,
        "deactivating" => State::Stopping,
        _ => State::Stopped,
    }
}

/// How a failed unit failed, folded into a number (`Service::exit_code`):
/// the exit status itself (1–255), 0x100 plus the signal that killed it,
/// or one of the codes below for failures that have no number.
const FAILED_TIMEOUT: u32 = 0x200;
const FAILED_WATCHDOG: u32 = 0x201;
const FAILED_START_LIMIT: u32 = 0x202;
const FAILED_RESOURCES: u32 = 0x203;
const FAILED_OOM: u32 = 0x204;
const FAILED_OTHER: u32 = 0x2FF;

fn failure_code(result: &str, status: i32) -> u32 {
    match result {
        "exit-code" if status > 0 => status as u32,
        "signal" | "core-dump" if status > 0 => 0x100 | status as u32,
        "timeout" => FAILED_TIMEOUT,
        "watchdog" => FAILED_WATCHDOG,
        "start-limit-hit" => FAILED_START_LIMIT,
        "resources" => FAILED_RESOURCES,
        "oom-kill" => FAILED_OOM,
        _ => FAILED_OTHER,
    }
}

/// What a failed service's `exit_code` means.
pub fn exit_text(code: u32) -> String {
    match code {
        1..=0xFF => format!("its program ended with exit status {code}"),
        0x101..=0x1FF => {
            let signal = (code & 0xFF) as i32;
            let name = unsafe { libc::strsignal(signal) };
            let name = if name.is_null() {
                String::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned()
            };
            format!("its program was ended by signal {signal} ({name})")
        }
        FAILED_TIMEOUT => "it did not start or stop in time".into(),
        FAILED_WATCHDOG => "it stopped answering its watchdog".into(),
        FAILED_START_LIMIT => "it failed to start too often in a row".into(),
        FAILED_RESOURCES => "what it needs to run could not be set up".into(),
        FAILED_OOM => "the kernel ended it for lack of memory".into(),
        _ => "it failed".into(),
    }
}

/// The current user's account name, for the units of their own manager.
fn user_name() -> String {
    static NAME: LazyLock<String> = LazyLock::new(|| {
        let uid = unsafe { libc::getuid() };
        crate::enrich::user_name(&uid.to_le_bytes()).unwrap_or_else(|| uid.to_string())
    });
    NAME.clone()
}

/// The keys of a unit file that matter here, later ones winning (as
/// systemd reads them), drop-ins not included.
#[derive(Default)]
struct UnitFile {
    description: String,
    documentation: Vec<String>,
    exec_start: String,
    user: String,
    requires: Vec<String>,
    wants: Vec<String>,
    /// It says when to start ([Install]): it can be enabled.
    install: bool,
}

fn parse_unit_file(text: &str) -> UnitFile {
    let mut file = UnitFile::default();
    let mut section = "";
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let list = || value.split_ascii_whitespace().map(str::to_string);
        match (section, key.trim()) {
            ("Install", "WantedBy" | "RequiredBy" | "UpheldBy" | "Alias" | "Also") => {
                file.install |= !value.is_empty()
            }
            ("Install", _) => {}
            (_, "Description") => file.description = value.to_string(),
            (_, "Documentation") => file.documentation.extend(list()),
            // The first command; prefixes (@, -, :, +, !) say how it runs.
            (_, "ExecStart") if file.exec_start.is_empty() => {
                file.exec_start = value
                    .trim_start_matches(['@', '-', ':', '+', '!'])
                    .to_string()
            }
            (_, "User") => file.user = value.to_string(),
            (_, "Requires") => file.requires.extend(list()),
            (_, "Wants") => file.wants.extend(list()),
            _ => {}
        }
    }
    file
}

/// Unit files read before, by path, with when they were last written:
/// listing reads only the ones that changed.
type Parsed = HashMap<PathBuf, (SystemTime, Arc<UnitFile>)>;
static PARSED: LazyLock<Mutex<Parsed>> = LazyLock::new(Mutex::default);

fn parsed(path: &Path) -> Arc<UnitFile> {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut cache = PARSED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, file)) = cache.get(path)
        && *at == modified
    {
        return file.clone();
    }
    let file = Arc::new(parse_unit_file(
        &std::fs::read_to_string(path).unwrap_or_default(),
    ));
    cache.insert(path.to_path_buf(), (modified, file.clone()));
    file
}

/// A directory under the user's home, from its XDG variable or default.
fn xdg(variable: &str, default: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(default)))
}

/// Where a manager finds unit files, the first winning over the later ones.
fn search_path(scope: Scope) -> Vec<PathBuf> {
    match scope {
        Scope::System => [
            "/etc/systemd/system",
            "/run/systemd/system",
            "/usr/local/lib/systemd/system",
            "/usr/lib/systemd/system",
        ]
        .iter()
        .map(PathBuf::from)
        .collect(),
        Scope::User => {
            let mut dirs: Vec<PathBuf> = Vec::new();
            dirs.extend(xdg("XDG_CONFIG_HOME", ".config").map(|d| d.join("systemd/user")));
            dirs.push("/etc/systemd/user".into());
            dirs.extend(
                std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("systemd/user")),
            );
            dirs.extend(xdg("XDG_DATA_HOME", ".local/share").map(|d| d.join("systemd/user")));
            dirs.push("/usr/local/lib/systemd/user".into());
            dirs.push("/usr/lib/systemd/user".into());
            dirs
        }
    }
}

/// The units enabled for a manager: linked from a `.wants`, `.requires` or
/// `.upholds` directory where enabling puts them, or under another name
/// (an alias such as display-manager.service).
fn enabled_units(scope: Scope) -> HashSet<String> {
    let dirs: Vec<PathBuf> = match scope {
        Scope::System => vec!["/etc/systemd/system".into(), "/run/systemd/system".into()],
        Scope::User => xdg("XDG_CONFIG_HOME", ".config")
            .map(|d| d.join("systemd/user"))
            .into_iter()
            .chain(std::iter::once(PathBuf::from("/etc/systemd/user")))
            .collect(),
    };
    let mut out = HashSet::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(target) = std::fs::read_link(entry.path())
                && let Some(target) = target.file_name().map(|t| t.to_string_lossy().into_owned())
                && target != name
                && target != "null"
            {
                out.insert(target);
                continue;
            }
            if ![".wants", ".requires", ".upholds"]
                .iter()
                .any(|s| name.ends_with(s))
            {
                continue;
            }
            if let Ok(links) = std::fs::read_dir(entry.path()) {
                out.extend(
                    links
                        .flatten()
                        .map(|l| l.file_name().to_string_lossy().into_owned()),
                );
            }
        }
    }
    out
}

/// An installed service unit: its name, file and unit file state.
struct Installed {
    name: String,
    file: PathBuf,
    state: &'static str,
}

/// Every installed service unit of a manager, worked out from its unit
/// directories the way systemd does it (its own listing reads and judges
/// every unit file, and takes a good part of a second).
fn installed(scope: Scope) -> Vec<Installed> {
    let enabled = enabled_units(scope);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in search_path(scope) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Templates start only as instances, which are listed loaded.
            if !name.ends_with(".service") || name.ends_with("@.service") {
                continue;
            }
            if !seen.insert(name.clone()) {
                continue;
            }
            let mut file = entry.path();
            if let Ok(target) = std::fs::read_link(&file) {
                if target == Path::new("/dev/null") {
                    out.push(Installed {
                        name,
                        file,
                        state: "masked",
                    });
                    continue;
                }
                // An alias: listed under the unit it names.
                if target
                    .file_name()
                    .is_some_and(|t| t.to_string_lossy() != name)
                {
                    continue;
                }
                file = dir.join(target);
            }
            let state = if enabled.contains(&name) {
                "enabled"
            } else if parsed(&file).install {
                "disabled"
            } else {
                "static"
            };
            out.push(Installed { name, file, state });
        }
    }
    out
}

/// Dependencies worth naming: other services, sockets and targets.
fn dependencies(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| {
            [".service", ".socket", ".target"]
                .iter()
                .any(|s| n.ends_with(s))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A loaded unit's configuration, read from its manager.
fn loaded_config(conn: &Connection, path: &str, scope: Scope) -> Option<Config> {
    let unit = proxy(conn, path, UNIT).ok()?;
    let service = proxy(conn, path, SERVICE).ok()?;
    let file_state: String = unit.get_property("UnitFileState").unwrap_or_default();
    let triggered: Vec<String> = unit.get_property("TriggeredBy").unwrap_or_default();
    let documentation: Vec<String> = unit.get_property("Documentation").unwrap_or_default();
    let requires: Vec<String> = unit.get_property("Requires").unwrap_or_default();
    let wants: Vec<String> = unit.get_property("Wants").unwrap_or_default();
    let exec: Vec<ExecCommand> = service.get_property("ExecStart").unwrap_or_default();
    let user: String = service.get_property("User").unwrap_or_default();
    Some(Config {
        start: start_type(&file_state),
        triggered: !triggered.is_empty(),
        binary: exec
            .first()
            .map(|(path, argv, ..)| match argv.is_empty() {
                true => path.clone(),
                false => argv.join(" "),
            })
            .unwrap_or_default(),
        account: match (scope, user.is_empty()) {
            (Scope::User, _) => user_name(),
            (Scope::System, true) => "root".into(),
            (Scope::System, false) => user,
        },
        description: documentation.join(", "),
        dependencies: dependencies(requires.into_iter().chain(wants)),
    })
}

/// An installed unit's configuration, read from its file (the manager has
/// not loaded it, and asking it would).
fn file_config(file: &UnitFile, state: &str, scope: Scope) -> Config {
    Config {
        start: start_type(state),
        triggered: false,
        binary: file.exec_start.clone(),
        account: match (scope, file.user.is_empty()) {
            (Scope::User, _) => user_name(),
            (Scope::System, true) => "root".into(),
            (Scope::System, false) => file.user.clone(),
        },
        description: file.documentation.join(", "),
        dependencies: dependencies(file.requires.iter().chain(&file.wants).cloned()),
    }
}

/// Every service unit of one manager.
fn list_scope(
    scope: Scope,
    configs: &mut HashMap<Arc<str>, Option<Arc<Config>>>,
    out: &mut Vec<Service>,
) -> Result<(), String> {
    let conn = scope.connect().map_err(|e| words(&e))?;
    let manager = proxy(&conn, MANAGER_PATH, MANAGER).map_err(|e| words(&e))?;
    let none: Vec<&str> = Vec::new();
    let units: Vec<UnitRow> = manager
        .call("ListUnitsByPatterns", &(&none, vec!["*.service"]))
        .map_err(|e| words(&e))?;
    let mark = |unit: &str| -> Arc<str> {
        match scope {
            Scope::System => Arc::from(unit),
            Scope::User => Arc::from(format!("{unit}{USER_MARK}")),
        }
    };
    let mut listed = HashSet::new();
    for (unit, description, load, active, _sub, _, path, ..) in units {
        // A name something refers to, with no unit behind it.
        if load == "not-found" && active == "inactive" {
            continue;
        }
        listed.insert(unit.clone());
        let name = mark(&unit);
        let state = state_of(&active);
        let service = proxy(&conn, path.as_str(), SERVICE).ok();
        let pid: u32 = if state == State::Stopped {
            0
        } else {
            service
                .as_ref()
                .and_then(|s| s.get_property("MainPID").ok())
                .unwrap_or(0)
        };
        let exit_code = if active == "failed" {
            let result: String = service
                .as_ref()
                .and_then(|s| s.get_property("Result").ok())
                .unwrap_or_default();
            let status: i32 = service
                .as_ref()
                .and_then(|s| s.get_property("ExecMainStatus").ok())
                .unwrap_or(0);
            failure_code(&result, status)
        } else {
            0
        };
        let accepts_stop = proxy(&conn, path.as_str(), UNIT)
            .ok()
            .and_then(|u| u.get_property::<bool>("CanStop").ok())
            .unwrap_or(true);
        let config = configs
            .entry(name.clone())
            .or_insert_with(|| loaded_config(&conn, path.as_str(), scope).map(Arc::new))
            .clone();
        out.push(Service {
            display: Arc::from(if description.is_empty() {
                unit.as_str()
            } else {
                description.as_str()
            }),
            name,
            state,
            pid,
            shared: false,
            user_template: false,
            accepts_stop,
            accepts_pause: false,
            exit_code,
            config,
        });
    }
    for unit in installed(scope) {
        if listed.contains(&unit.name) {
            continue;
        }
        let name = mark(&unit.name);
        let file = parsed(&unit.file);
        let config = configs
            .entry(name.clone())
            .or_insert_with(|| Some(Arc::new(file_config(&file, unit.state, scope))))
            .clone();
        out.push(Service {
            display: Arc::from(if file.description.is_empty() {
                unit.name.as_str()
            } else {
                file.description.as_str()
            }),
            name,
            state: State::Stopped,
            pid: 0,
            shared: false,
            user_template: false,
            accepts_stop: true,
            accepts_pause: false,
            exit_code: 0,
            config,
        });
    }
    Ok(())
}

pub(super) fn list(configs: &mut HashMap<Arc<str>, Option<Arc<Config>>>) -> ServiceList {
    let started = Instant::now();
    let mut services = Vec::new();
    let error = list_scope(Scope::System, configs, &mut services).err();
    // The user's own manager is a nicety: without one (no session bus), the
    // system's services still list.
    let _ = list_scope(Scope::User, configs, &mut services);
    ServiceList {
        services,
        error,
        took: started.elapsed(),
    }
}

/// The unit's object path, loading it if need be (to control it).
fn unit_path(manager: &Proxy<'_>, unit: &str) -> Result<OwnedObjectPath, String> {
    manager
        .call::<_, _, OwnedObjectPath>("LoadUnit", &(unit,))
        .map_err(|e| words(&e))
}

/// Calls a manager method that may need polkit's consent.
fn manage<B, R>(manager: &Proxy<'_>, method: &str, body: &B) -> Result<R, String>
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    manager
        .call_with_flags::<_, B, R>(method, MethodFlags::AllowInteractiveAuth.into(), body)
        .map_err(|e| words(&e))?
        .ok_or_else(|| "systemd did not answer".to_string())
}

const JOB_LIMIT: Duration = Duration::from_secs(30);

/// Runs a start, stop or restart job and waits for it to finish, up to
/// half a minute. Ok when the unit ended up where the job meant it to.
fn run_job(conn: &Connection, unit: &str, method: &str) -> Result<(), String> {
    let manager = proxy(conn, MANAGER_PATH, MANAGER).map_err(|e| words(&e))?;
    let job: OwnedObjectPath = manage(&manager, method, &(unit, "replace"))?;
    let started = Instant::now();
    // A job's object goes away when it is done.
    loop {
        let pending = proxy(conn, job.as_str(), "org.freedesktop.systemd1.Job")
            .and_then(|j| j.get_property::<String>("State"))
            .is_ok();
        if !pending {
            break;
        }
        if started.elapsed() > JOB_LIMIT {
            return Err(format!("it is still busy after {} s", JOB_LIMIT.as_secs()));
        }
        thread::sleep(Duration::from_millis(100));
    }
    let path = unit_path(&manager, unit)?;
    let unit = proxy(conn, path.as_str(), UNIT).map_err(|e| words(&e))?;
    let active: String = unit.get_property("ActiveState").map_err(|e| words(&e))?;
    let wanted_running = method != "StopUnit";
    match (wanted_running, active.as_str()) {
        (false, "inactive" | "failed") | (true, "active" | "activating" | "reloading") => Ok(()),
        // A program that runs and ends (Type=oneshot) ends inactive.
        (true, "inactive") => Ok(()),
        (_, _) => {
            let service = proxy(conn, path.as_str(), SERVICE).map_err(|e| words(&e))?;
            let result: String = service.get_property("Result").unwrap_or_default();
            let status: i32 = service.get_property("ExecMainStatus").unwrap_or(0);
            Err(exit_text(failure_code(&result, status)))
        }
    }
}

/// The active units that would stop with this one: those that require it
/// or are bound to it.
fn running_dependents(conn: &Connection, unit: &str) -> Vec<String> {
    let Ok(manager) = proxy(conn, MANAGER_PATH, MANAGER) else {
        return Vec::new();
    };
    let Ok(path) = unit_path(&manager, unit) else {
        return Vec::new();
    };
    let Ok(unit) = proxy(conn, path.as_str(), UNIT) else {
        return Vec::new();
    };
    let mut names: Vec<String> = ["RequiredBy", "BoundBy"]
        .iter()
        .flat_map(|p| unit.get_property::<Vec<String>>(p).unwrap_or_default())
        .filter(|n| n.ends_with(".service"))
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|name| {
            let Ok(path) = unit_path(&manager, name) else {
                return false;
            };
            proxy(conn, path.as_str(), UNIT)
                .and_then(|u| u.get_property::<String>("ActiveState"))
                .is_ok_and(|a| a != "inactive" && a != "failed")
        })
        .collect()
}

fn set_start(conn: &Connection, unit: &str, start: StartType) -> Result<(), String> {
    let manager = proxy(conn, MANAGER_PATH, MANAGER).map_err(|e| words(&e))?;
    let files = vec![unit];
    type Changes = Vec<(String, String, String)>;
    let path = unit_path(&manager, unit)?;
    let state: String = proxy(conn, path.as_str(), UNIT)
        .and_then(|u| u.get_property("UnitFileState"))
        .unwrap_or_default();
    if state.starts_with("masked") && start != StartType::Disabled {
        let _: Changes = manage(&manager, "UnmaskUnitFiles", &(&files, false))?;
    }
    match start {
        StartType::Automatic => {
            let (install_info, _): (bool, Changes) =
                manage(&manager, "EnableUnitFiles", &(&files, false, true))?;
            if !install_info {
                return Err(
                    "it says nothing about when to start (no [Install] section): it starts only \
                     when something else needs it"
                        .into(),
                );
            }
        }
        StartType::Manual => {
            let _: Changes = manage(&manager, "DisableUnitFiles", &(&files, false))?;
        }
        StartType::Disabled => {
            let _: Changes = manage(&manager, "MaskUnitFiles", &(&files, false, true))?;
        }
        StartType::AutomaticDelayed | StartType::Boot | StartType::System => {
            return Err(format!("systemd has no \"{}\" start", start.label()));
        }
    }
    manage::<_, ()>(&manager, "Reload", &())
}

pub(super) fn control(
    name: &str,
    display: &str,
    op: ServiceOp,
) -> (Result<String, String>, Vec<String>) {
    let (unit, scope) = unit_of(name);
    let conn = match scope.connect() {
        Ok(conn) => conn,
        Err(e) => {
            return (
                Err(format!("Could not reach systemd: {}", words(&e))),
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
            run_job(&conn, unit, "StartUnit")
                .map(|_| format!("Started {display}"))
                .map_err(|e| format!("Could not start {display}: {e}")),
            Vec::new(),
        ),
        ServiceOp::Stop { dependents } | ServiceOp::Restart { dependents } => {
            let others = running_dependents(&conn, unit);
            let restart = matches!(op, ServiceOp::Restart { .. });
            let verb = if restart { "restart" } else { "stop" };
            if !others.is_empty() && !dependents {
                return (
                    Err(format!(
                        "Could not {verb} {display}: {} other service{} depend{} on it",
                        others.len(),
                        if others.len() == 1 { "" } else { "s" },
                        if others.len() == 1 { "s" } else { "" }
                    )),
                    others,
                );
            }
            // systemd takes the dependents along itself, and starts them
            // again after a restart.
            let method = if restart { "RestartUnit" } else { "StopUnit" };
            (
                run_job(&conn, unit, method)
                    .map(|_| {
                        let done = if restart { "Restarted" } else { "Stopped" };
                        format!("{done} {display}{}", also(&others))
                    })
                    .map_err(|e| format!("Could not {verb} {display}: {e}")),
                Vec::new(),
            )
        }
        ServiceOp::Pause | ServiceOp::Continue => (
            Err(format!(
                "Could not pause or resume {display}: systemd services cannot be paused"
            )),
            Vec::new(),
        ),
        ServiceOp::SetStart(start) => {
            let when = match (start, scope) {
                (StartType::Automatic, Scope::System) => "now starts with the computer",
                (StartType::Automatic, Scope::User) => "now starts when you sign in",
                (StartType::Disabled, _) => "is masked now: nothing can start it",
                _ => "now starts only when asked or needed",
            };
            (
                set_start(&conn, unit, start)
                    .map(|_| format!("{display} {when}"))
                    .map_err(|e| format!("Could not change how {display} starts: {e}")),
                Vec::new(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_system_and_user_services() {
        let started = Instant::now();
        let list = list(&mut HashMap::new());
        let took = started.elapsed();
        assert!(list.error.is_none(), "{:?}", list.error);
        // Every systemd machine runs its journal.
        let journal = list
            .services
            .iter()
            .find(|s| &*s.name == "systemd-journald.service")
            .expect("systemd-journald is listed");
        assert_eq!(journal.state, State::Running);
        assert_ne!(journal.pid, 0);
        let config = journal.config.as_ref().expect("its configuration is read");
        assert!(
            config.binary.contains("systemd-journald"),
            "{}",
            config.binary
        );
        assert!(!list.by_pid()[&journal.pid].is_empty());
        let stopped = list
            .services
            .iter()
            .filter(|s| s.state == State::Stopped)
            .count();
        let users = list
            .services
            .iter()
            .filter(|s| s.name.ends_with(USER_MARK))
            .count();
        eprintln!(
            "{} services ({stopped} stopped, {users} of the user's) in {took:?}",
            list.services.len()
        );
        // A second listing reuses the configurations.
        for s in list.services.iter().filter(|s| {
            [
                "sddm",
                "bluetooth",
                "pipewire.service",
                "cups",
                "systemd-networkd.service",
            ]
            .iter()
            .any(|n| s.name.contains(n))
                || s.exit_code != 0
        }) {
            let c = s.config.as_deref();
            eprintln!(
                "  {} | {} | {:?} pid {} | {} | {} | {} | triggered {} | {:?} | exit {}",
                s.name,
                s.display,
                s.state,
                s.pid,
                c.map_or("", |c| c.start.label()),
                c.map_or("", |c| &c.account),
                c.map_or("", |c| &c.binary),
                c.is_some_and(|c| c.triggered),
                c.map(|c| &c.dependencies),
                s.exit_code
            );
        }
        let mut configs = HashMap::new();
        super::list(&mut configs);
        for _ in 0..3 {
            let again = Instant::now();
            super::list(&mut configs);
            eprintln!("again, configurations known: {:?}", again.elapsed());
        }
    }

    #[test]
    fn works_out_unit_file_states_like_systemd() {
        // Compared with the manager's own (slow) listing.
        let conn = Connection::system().unwrap();
        let manager = proxy(&conn, MANAGER_PATH, MANAGER).unwrap();
        let none: Vec<&str> = Vec::new();
        let theirs: Vec<(String, String)> = manager
            .call("ListUnitFilesByPatterns", &(&none, vec!["*.service"]))
            .unwrap();
        let theirs: HashMap<String, String> = theirs
            .into_iter()
            .filter_map(|(path, state)| Some((path.rsplit('/').next()?.to_string(), state)))
            .collect();
        let ours = installed(Scope::System);
        let mut differ = Vec::new();
        for unit in &ours {
            if let Some(state) = theirs.get(&unit.name)
                && start_type(state) != start_type(unit.state)
            {
                differ.push(format!("{}: {} / {}", unit.name, unit.state, state));
            }
        }
        eprintln!("{} installed, differing: {differ:?}", ours.len());
        assert!(differ.len() * 20 <= ours.len(), "{differ:?}");
    }

    #[test]
    fn reads_unit_files() {
        let file = parse_unit_file(
            "[Unit]\nDescription=OpenSSH Daemon\nWants=sshdgenkeys.service\n\
             After=sshdgenkeys.service network.target\n\n[Service]\n\
             ExecStart=-/usr/bin/sshd -D\nExecReload=/bin/kill -HUP $MAINPID\nUser=sshd\n",
        );
        assert_eq!(file.description, "OpenSSH Daemon");
        assert_eq!(file.exec_start, "/usr/bin/sshd -D");
        assert_eq!(file.user, "sshd");
        assert_eq!(file.wants, vec!["sshdgenkeys.service"]);
    }

    #[test]
    fn names_and_failures() {
        assert_eq!(
            unit_of("pipewire.service (user)"),
            ("pipewire.service", Scope::User)
        );
        assert_eq!(unit_of("sshd.service"), ("sshd.service", Scope::System));
        assert_eq!(start_type("enabled"), StartType::Automatic);
        assert_eq!(start_type("disabled"), StartType::Manual);
        assert_eq!(start_type("masked"), StartType::Disabled);
        assert_eq!(start_type("static"), StartType::System);
        assert!(exit_text(failure_code("exit-code", 3)).contains("exit status 3"));
        assert!(exit_text(failure_code("signal", 9)).contains("signal 9"));
        assert!(exit_text(failure_code("timeout", 0)).contains("in time"));
        assert_eq!(failure_code("exit-code", 0), FAILED_OTHER);
    }
}
