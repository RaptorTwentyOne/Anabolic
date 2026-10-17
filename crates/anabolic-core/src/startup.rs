//! Programs that start when the user signs in: the Run keys, the Startup
//! folders, and scheduled tasks triggered at sign-in or boot. Turned on and
//! off the way Task Manager does it (the StartupApproved keys), so the two
//! always agree; tasks through the Task Scheduler. Nothing is deleted.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::Foundation::{ERROR_SUCCESS, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
    STGM_READ,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_BINARY,
    RRF_RT_REG_BINARY, RegCloseKey, RegEnumValueW, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::TaskScheduler::{
    IExecAction, ITaskFolder, ITaskService, TASK_ACTION_EXEC, TASK_ENUM_HIDDEN, TASK_TRIGGER_BOOT,
    TASK_TRIGGER_LOGON, TaskScheduler,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{BSTR, Interface, PCWSTR};

use crate::util::{error_text, from_wide, wide};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// HKCU\...\Run: this user only.
    RunUser,
    /// HKLM\...\Run: every user.
    RunMachine,
    /// HKLM\...\WOW6432Node\...\Run: every user, 32-bit programs.
    RunMachine32,
    /// The user's Startup folder.
    FolderUser,
    /// The Startup folder of all users.
    FolderCommon,
    /// A scheduled task with a sign-in trigger.
    TaskLogon,
    /// A scheduled task with a boot trigger.
    TaskBoot,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::RunUser => "Registry, this user",
            Source::RunMachine => "Registry, all users",
            Source::RunMachine32 => "Registry, all users (32-bit)",
            Source::FolderUser => "Startup folder",
            Source::FolderCommon => "Startup folder, all users",
            Source::TaskLogon => "Task at sign-in",
            Source::TaskBoot => "Task at boot",
        }
    }

    pub fn is_task(self) -> bool {
        matches!(self, Source::TaskLogon | Source::TaskBoot)
    }

    /// Changing it needs administrator rights.
    pub fn machine_wide(self) -> bool {
        !matches!(self, Source::RunUser | Source::FolderUser)
    }

    /// Where Task Manager keeps its on/off switch for this kind of entry.
    fn approved(self) -> Option<(HKEY, &'static str)> {
        Some(match self {
            Source::RunUser => (
                HKEY_CURRENT_USER,
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
            ),
            Source::RunMachine => (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
            ),
            Source::RunMachine32 => (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32",
            ),
            Source::FolderUser => (
                HKEY_CURRENT_USER,
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
            ),
            Source::FolderCommon => (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
            ),
            Source::TaskLogon | Source::TaskBoot => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct StartupItem {
    /// The value name, the file name in the folder, or the task's name.
    pub name: String,
    pub source: Source,
    /// The task's full path (`\Folder\Name`); the file's for a folder item.
    pub location: String,
    /// What runs: a command line.
    pub command: String,
    /// The program that command starts, environment variables expanded.
    pub program: Option<String>,
    pub enabled: bool,
    /// When it was turned off, as a FILETIME (Task Manager records it).
    pub disabled_at: Option<i64>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    /// A task of Windows' own (under \Microsoft\).
    pub windows: bool,
    /// The program is not there any more.
    pub missing: bool,
}

#[derive(Debug, Default)]
pub struct StartupList {
    pub items: Vec<StartupItem>,
    /// Sources that could not be read, and why.
    pub errors: Vec<String>,
    pub took: Duration,
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";

fn expand(text: &str) -> String {
    let w = wide(text);
    let mut buf = vec![0u16; 1024];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return text.to_string();
    }
    from_wide(&buf)
}

/// The program a command line starts: quoted, or up to ".exe", or up to
/// the first space.
pub fn program_of(command: &str) -> Option<String> {
    let command = expand(command.trim());
    let path = if let Some(rest) = command.strip_prefix('"') {
        rest.split('"').next()?.to_string()
    } else {
        let lower = command.to_lowercase();
        match lower.find(".exe") {
            Some(i) => command[..i + 4].to_string(),
            None => command.split(' ').next()?.to_string(),
        }
    };
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    // A bare name ("rundll32.exe ...") lives in System32.
    if !path.contains('\\') {
        let system = expand(r"%SystemRoot%\System32");
        return Some(format!(r"{system}\{path}"));
    }
    Some(path.to_string())
}

/// Task Manager's switch: missing or an even first byte means on; an odd
/// one off, followed by when (a FILETIME).
pub fn parse_approved(data: &[u8]) -> (bool, Option<i64>) {
    match data.first() {
        Some(b) if b & 1 == 1 => {
            let at = data
                .get(4..12)
                .map(|t| i64::from_le_bytes(t.try_into().unwrap()))
                .filter(|&t| t > 0);
            (false, at)
        }
        _ => (true, None),
    }
}

fn approved_state(source: Source, name: &str) -> (bool, Option<i64>) {
    let Some((root, key)) = source.approved() else {
        return (true, None);
    };
    let (key, name) = (wide(key), wide(name));
    let mut data = [0u8; 64];
    let mut size = data.len() as u32;
    let err = unsafe {
        RegGetValueW(
            root,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err != ERROR_SUCCESS {
        return (true, None);
    }
    parse_approved(&data[..size as usize])
}

fn set_approved(source: Source, name: &str, on: bool) -> Result<(), String> {
    let (root, key) = source
        .approved()
        .ok_or_else(|| "not a registry or folder entry".to_string())?;
    let mut data = [0u8; 12];
    if on {
        data[0] = 2;
    } else {
        data[0] = 3;
        let now = unsafe { GetSystemTimeAsFileTime() };
        let at = ((now.dwHighDateTime as i64) << 32) | now.dwLowDateTime as i64;
        data[4..].copy_from_slice(&at.to_le_bytes());
    }
    let (key, name) = (wide(key), wide(name));
    let err = unsafe {
        RegSetKeyValueW(
            root,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_BINARY.0,
            Some(data.as_ptr().cast()),
            data.len() as u32,
        )
    };
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(error_text(&windows::core::Error::from(err.to_hresult())))
    }
}

fn run_key(
    root: HKEY,
    path: &str,
    source: Source,
    out: &mut Vec<StartupItem>,
) -> Result<(), String> {
    let mut key = HKEY::default();
    let path_w = wide(path);
    let err = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(path_w.as_ptr()),
            None,
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if err != ERROR_SUCCESS {
        // No such key is no entries.
        return Ok(());
    }
    let mut index = 0;
    loop {
        let mut name = vec![0u16; 512];
        let mut name_len = name.len() as u32;
        let mut kind = 0u32;
        let mut data = vec![0u8; 8192];
        let mut data_len = data.len() as u32;
        let err = unsafe {
            RegEnumValueW(
                key,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        index += 1;
        if err != ERROR_SUCCESS {
            break;
        }
        // REG_SZ or REG_EXPAND_SZ.
        if kind != 1 && kind != 2 {
            continue;
        }
        let name = from_wide(&name[..name_len as usize]);
        let chars: Vec<u16> = data[..data_len as usize]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let command = from_wide(&chars);
        if name.is_empty() || command.trim().is_empty() {
            continue;
        }
        out.push(item(name, source, path.to_string(), command));
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    Ok(())
}

fn item(name: String, source: Source, location: String, command: String) -> StartupItem {
    let (enabled, disabled_at) = approved_state(source, &name);
    let program = program_of(&command);
    let missing = program
        .as_deref()
        .is_some_and(|p| !std::path::Path::new(p).exists());
    let info = program
        .as_deref()
        .filter(|_| !missing)
        .map(crate::winfile::file_info);
    StartupItem {
        name,
        source,
        location,
        command,
        program,
        enabled,
        disabled_at,
        publisher: info.as_ref().and_then(|i| i.company.clone()),
        description: info.and_then(|i| i.description),
        windows: false,
        missing,
    }
}

/// What a shortcut points at, with its arguments.
fn shortcut_target(path: &str) -> Option<String> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let w = wide(path);
        file.Load(PCWSTR(w.as_ptr()), STGM_READ).ok()?;
        let mut target = vec![0u16; 1024];
        link.GetPath(&mut target, std::ptr::null_mut(), 0).ok()?;
        let mut args = vec![0u16; 2048];
        let _ = link.GetArguments(&mut args);
        let (target, args) = (from_wide(&target), from_wide(&args));
        if target.is_empty() {
            return None;
        }
        Some(if args.is_empty() {
            format!("\"{target}\"")
        } else {
            format!("\"{target}\" {args}")
        })
    }
}

fn folder(dir: &str, source: Source, out: &mut Vec<StartupItem>) {
    let dir = expand(dir);
    let pattern = wide(&format!(r"{dir}\*"));
    let mut data = WIN32_FIND_DATAW::default();
    let Ok(find) = (unsafe { FindFirstFileW(PCWSTR(pattern.as_ptr()), &mut data) }) else {
        return;
    };
    loop {
        let name = from_wide(&data.cFileName);
        let is_dir = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
        if !is_dir && !name.eq_ignore_ascii_case("desktop.ini") {
            let path = format!(r"{dir}\{name}");
            let command = if name.to_lowercase().ends_with(".lnk") {
                shortcut_target(&path).unwrap_or_else(|| format!("\"{path}\""))
            } else {
                format!("\"{path}\"")
            };
            out.push(item(name, source, path, command));
        }
        if unsafe { FindNextFileW(find, &mut data) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(find);
    }
}

fn variant_i32(value: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_I4;
        (*v.Anonymous.Anonymous).Anonymous.lVal = value;
    }
    v
}

fn scheduler() -> windows::core::Result<ITaskService> {
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let empty = VARIANT::default();
        service.Connect(&empty, &empty, &empty, &empty)?;
        Ok(service)
    }
}

/// Every task under `folder` that runs a program at sign-in or boot.
fn tasks(folder: &ITaskFolder, out: &mut Vec<StartupItem>) {
    unsafe {
        let path = folder.Path().map(|p| p.to_string()).unwrap_or_default();
        let windows = path.to_lowercase().starts_with(r"\microsoft");
        if let Ok(list) = folder.GetTasks(TASK_ENUM_HIDDEN.0) {
            for i in 1..=list.Count().unwrap_or(0) {
                let Ok(task) = list.get_Item(&variant_i32(i)) else {
                    continue;
                };
                let Ok(def) = task.Definition() else {
                    continue;
                };
                let mut source = None;
                if let Ok(triggers) = def.Triggers() {
                    let mut count = 0;
                    let _ = triggers.Count(&mut count);
                    for t in 1..=count {
                        let Ok(trigger) = triggers.get_Item(t) else {
                            continue;
                        };
                        let mut kind = Default::default();
                        let _ = trigger.Type(&mut kind);
                        if kind == TASK_TRIGGER_LOGON {
                            source = Some(Source::TaskLogon);
                        } else if kind == TASK_TRIGGER_BOOT && source.is_none() {
                            source = Some(Source::TaskBoot);
                        }
                    }
                }
                let Some(source) = source else {
                    continue;
                };
                // The first program it runs; COM handlers have no command.
                let mut command = None;
                if let Ok(actions) = def.Actions() {
                    let mut count = 0;
                    let _ = actions.Count(&mut count);
                    for a in 1..=count {
                        let Ok(action) = actions.get_Item(a) else {
                            continue;
                        };
                        let mut kind = Default::default();
                        let _ = action.Type(&mut kind);
                        if kind != TASK_ACTION_EXEC {
                            continue;
                        }
                        let Ok(exec) = action.cast::<IExecAction>() else {
                            continue;
                        };
                        let (mut program, mut args) = (BSTR::new(), BSTR::new());
                        let _ = exec.Path(&mut program);
                        let _ = exec.Arguments(&mut args);
                        let program = program.to_string();
                        let args = args.to_string();
                        command = Some(if args.is_empty() {
                            format!("\"{}\"", program.trim_matches('"'))
                        } else {
                            format!("\"{}\" {args}", program.trim_matches('"'))
                        });
                        break;
                    }
                }
                let Some(command) = command else {
                    continue;
                };
                let name = task.Name().map(|n| n.to_string()).unwrap_or_default();
                let location = task.Path().map(|p| p.to_string()).unwrap_or_default();
                let enabled = task.Enabled().is_ok_and(|e| e.as_bool());
                let mut entry = item(name, source, location, command);
                entry.enabled = enabled;
                entry.windows = windows;
                out.push(entry);
            }
        }
        if let Ok(folders) = folder.GetFolders(0) {
            for i in 1..=folders.Count().unwrap_or(0) {
                if let Ok(sub) = folders.get_Item(&variant_i32(i)) {
                    tasks(&sub, out);
                }
            }
        }
    }
}

/// Reads every source; needs COM initialized on the calling thread.
pub fn list() -> StartupList {
    let started = Instant::now();
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for (root, path, source) in [
        (HKEY_CURRENT_USER, RUN, Source::RunUser),
        (HKEY_LOCAL_MACHINE, RUN, Source::RunMachine),
        (HKEY_LOCAL_MACHINE, RUN32, Source::RunMachine32),
    ] {
        if let Err(e) = run_key(root, path, source, &mut items) {
            errors.push(format!("{}: {e}", source.label()));
        }
    }
    folder(
        r"%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup",
        Source::FolderUser,
        &mut items,
    );
    folder(
        r"%ProgramData%\Microsoft\Windows\Start Menu\Programs\Startup",
        Source::FolderCommon,
        &mut items,
    );
    match scheduler().and_then(|s| unsafe { s.GetFolder(&BSTR::from("\\")) }) {
        Ok(root) => tasks(&root, &mut items),
        Err(e) => errors.push(format!("Scheduled tasks: {}", error_text(&e))),
    }
    StartupList {
        items,
        errors,
        took: started.elapsed(),
    }
}

fn set_task_enabled(path: &str, on: bool) -> Result<(), String> {
    unsafe {
        let service = scheduler().map_err(|e| error_text(&e))?;
        let root = service
            .GetFolder(&BSTR::from("\\"))
            .map_err(|e| error_text(&e))?;
        let task = root
            .GetTask(&BSTR::from(path))
            .map_err(|e| error_text(&e))?;
        task.SetEnabled(if on { VARIANT_TRUE } else { VARIANT_FALSE })
            .map_err(|e| error_text(&e))
    }
}

/// Turns an entry on or off; worded for a toast.
pub fn set_enabled(item: &StartupItem, on: bool) -> Result<String, String> {
    let result = if item.source.is_task() {
        set_task_enabled(&item.location, on)
    } else {
        set_approved(item.source, &item.name, on)
    };
    let what = if on { "on" } else { "off" };
    result
        .map(|_| {
            format!(
                "{} now starts {}",
                item.name,
                if on { "with Windows" } else { "no more" }
            )
        })
        .map_err(|e| format!("Could not turn {} {what}: {e}", item.name))
}

#[derive(Debug)]
pub enum StartupEvent {
    List(Arc<StartupList>),
    Done(Result<String, String>),
}

enum Request {
    Refresh,
    Set(Box<StartupItem>, bool),
}

/// Reads and changes the startup entries on a thread of its own (COM, the
/// registry and every program's version resource are slow to reach).
pub struct StartupWatch {
    tx: Sender<Request>,
}

impl StartupWatch {
    pub fn start(notify: impl Fn() + Send + 'static) -> (Self, Receiver<StartupEvent>) {
        let (tx, rx) = unbounded::<Request>();
        let (events, out) = unbounded();
        thread::Builder::new()
            .name("anabolic-startup".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                crate::hardening::set_current_thread_priority(
                    windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
                );
                while let Ok(request) = rx.recv() {
                    let mut refresh = false;
                    for request in std::iter::once(request).chain(rx.try_iter()) {
                        match request {
                            Request::Refresh => refresh = true,
                            Request::Set(item, on) => {
                                let done = set_enabled(&item, on);
                                if events.send(StartupEvent::Done(done)).is_err() {
                                    return;
                                }
                                refresh = true;
                            }
                        }
                    }
                    if refresh && events.send(StartupEvent::List(Arc::new(list()))).is_err() {
                        return;
                    }
                    notify();
                }
            })
            .expect("failed to spawn the startup thread");
        (Self { tx }, out)
    }

    pub fn refresh(&self) {
        let _ = self.tx.send(Request::Refresh);
    }

    pub fn set_enabled(&self, item: StartupItem, on: bool) {
        let _ = self.tx.send(Request::Set(Box::new(item), on));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_task_managers_switch() {
        assert_eq!(parse_approved(&[]), (true, None));
        assert_eq!(
            parse_approved(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            (true, None)
        );
        assert_eq!(parse_approved(&[6, 0, 0, 0]), (true, None));
        let at: i64 = 0x01DB_1234_5678_9ABC;
        let mut off = vec![3, 0, 0, 0];
        off.extend(at.to_le_bytes());
        assert_eq!(parse_approved(&off), (false, Some(at)));
        assert!(!parse_approved(&[7, 0, 0, 0]).0);
    }

    #[test]
    fn finds_the_program_of_a_command() {
        assert_eq!(
            program_of(r#""C:\Program Files\App\app.exe" --tray"#).as_deref(),
            Some(r"C:\Program Files\App\app.exe")
        );
        assert_eq!(
            program_of(r"C:\Tools\tool.exe /min").as_deref(),
            Some(r"C:\Tools\tool.exe")
        );
        let rundll = program_of("rundll32.exe shell32.dll,Control_RunDLL").unwrap();
        assert!(
            rundll.to_lowercase().ends_with(r"\system32\rundll32.exe"),
            "{rundll}"
        );
    }

    #[test]
    fn lists_without_failing() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let list = list();
        assert!(list.errors.is_empty(), "{:?}", list.errors);
        // Windows always has tasks of its own at sign-in.
        assert!(list.items.iter().any(|i| i.windows && i.source.is_task()));
    }
}
