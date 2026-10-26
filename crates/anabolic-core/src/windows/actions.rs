// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Process control. Runs on its own thread: a TerminateProcess against a
//! process wedged in the kernel, or a slow shell call, must never freeze the UI.

use core::ffi::c_void;
use core::mem::size_of;

use windows::Win32::Foundation::{DUPLICATE_CLOSE_SOURCE, DuplicateHandle, HANDLE, HWND};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, GetProcessAffinityMask,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, ProcessPowerThrottling, REALTIME_PRIORITY_CLASS,
    SetPriorityClass, SetProcessAffinityMask, SetProcessInformation, TerminateProcess,
};
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHOP_FILEPATH, SHObjectProperties, SHOpenFolderAndSelectItems,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use crate::ntapi;
use crate::util::{error_text, open_process, wide};

use super::*;

/// Shell calls want COM on the action thread.
pub(super) fn thread_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
}

impl Priority {
    fn class(self) -> PROCESS_CREATION_FLAGS {
        match self {
            Priority::Idle => IDLE_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Priority::High => HIGH_PRIORITY_CLASS,
            Priority::Realtime => REALTIME_PRIORITY_CLASS,
        }
    }
}

/// Carries out an action on the calling thread (the action thread, or the
/// MCP server's): the outcome, or None for one that has nothing to report.
pub fn perform(action: Action) -> Option<Outcome> {
    let result = match action {
        Action::Quiet(action) => {
            return perform(*action).filter(|o| matches!(o, Outcome::Error(_)));
        }
        Action::Silent(action) => {
            perform(*action);
            return None;
        }
        Action::SignIn(on) => return Some(crate::signin::perform(on)),
        Action::Firewall { path, block } => {
            let file = std::path::Path::new(&path)
                .file_name()
                .map_or(path.clone(), |f| f.to_string_lossy().into_owned());
            let done = if block {
                crate::firewall::block(&path)
            } else {
                crate::firewall::unblock(&path)
            };
            return Some(match done {
                Ok(()) => Outcome::Firewall {
                    path,
                    blocked: block,
                },
                Err(e) if block => Outcome::Error(format!("Could not block {file}: {e}")),
                Err(e) => Outcome::Error(format!("Could not allow {file} again: {e}")),
            });
        }
        Action::CloseConnection {
            pid,
            process,
            remote,
            remote_port,
            local_port,
        } => crate::sockets::close_tcp(pid, remote, remote_port, local_port)
            .map(|_| format!("Closed {process}'s connection to {remote}:{remote_port}"))
            .map_err(|e| {
                format!("Could not close {process}'s connection to {remote}:{remote_port}: {e}")
            }),
        Action::QueryFirewall(paths) => {
            return Some(Outcome::StillBlocked(crate::firewall::still_blocked(
                &paths,
            )));
        }
        Action::GamePower { on: true, .. } => {
            return Some(match crate::power::for_game() {
                Ok(previous) => {
                    Outcome::PowerForGame(previous.map(|(id, name)| (format!("{id:?}"), name)))
                }
                Err(e) => Outcome::Error(format!("Game mode could not switch the power plan: {e}")),
            });
        }
        Action::GamePower {
            on: false,
            previous,
        } => {
            let previous = previous.as_deref().and_then(crate::power::parse)?;
            return Some(match crate::power::restore(&previous) {
                Ok(name) => Outcome::PowerRestored(name),
                Err(e) => Outcome::Error(format!("Could not put the power plan back: {e}")),
            });
        }
        Action::Copy(text) => arboard::Clipboard::new()
            .and_then(|mut c| c.set_text(text))
            .map(|_| String::new())
            .map_err(|e| format!("Could not copy: {e}")),
        Action::RelaunchElevated => {
            return Some(match relaunch_elevated() {
                Ok(()) => Outcome::Relaunched,
                Err(e) => Outcome::Error(e),
            });
        }
        Action::Terminate(targets) => return Some(terminate(&targets)),
        Action::Run { command, elevated } => run(&command, elevated),
        Action::BrowseProgram => return Some(Outcome::Browsed(browse_program())),
        // Run on their own thread by the runner.
        Action::Dump { .. } => return None,
        Action::Suspend(t) => nt_call(&t, PROCESS_SUSPEND_RESUME, ntapi::NtSuspendProcess)
            .map(|_| format!("Suspended {}", t.label())),
        Action::Resume(t) => nt_call(&t, PROCESS_SUSPEND_RESUME, ntapi::NtResumeProcess)
            .map(|_| format!("Resumed {}", t.label())),
        Action::SetPriority(t, priority) => open_process(t.key, PROCESS_SET_INFORMATION)
            .and_then(|h| unsafe { SetPriorityClass(h.raw(), priority.class()) })
            .map(|_| format!("{} priority set to {}", t.label(), priority.label()))
            .map_err(|e| {
                format!(
                    "Could not change priority of {}: {}",
                    t.label(),
                    error_text(&e)
                )
            }),
        Action::SetEfficiencyMode(t, on) => set_efficiency_mode(&t, on),
        Action::QueryAffinity(t) => {
            let mut process_mask = 0usize;
            let mut system_mask = 0usize;
            return Some(
                match open_process(t.key, PROCESS_QUERY_LIMITED_INFORMATION).and_then(|h| unsafe {
                    GetProcessAffinityMask(h.raw(), &mut process_mask, &mut system_mask)
                }) {
                    Ok(()) => Outcome::Affinity {
                        target: t,
                        process_mask: process_mask as u64,
                        system_mask: system_mask as u64,
                    },
                    Err(e) => Outcome::Error(format!(
                        "Could not read affinity of {}: {}",
                        t.label(),
                        error_text(&e)
                    )),
                },
            );
        }
        Action::SetAffinity(t, mask) => open_process(t.key, PROCESS_SET_INFORMATION)
            .and_then(|h| unsafe { SetProcessAffinityMask(h.raw(), mask as usize) })
            .map(|_| format!("{} affinity set to {} CPU(s)", t.label(), mask.count_ones()))
            .map_err(|e| {
                format!(
                    "Could not set affinity of {}: {}",
                    t.label(),
                    error_text(&e)
                )
            }),
        Action::OpenFileLocation(path) => open_file_location(&path).map(|_| String::new()),
        Action::FileProperties(path) => {
            let p = wide(&path);
            let ok = unsafe {
                SHObjectProperties(
                    None::<HWND>,
                    SHOP_FILEPATH,
                    PCWSTR(p.as_ptr()),
                    PCWSTR::null(),
                )
            };
            if ok.as_bool() {
                return None;
            } else {
                Err(format!("Could not show properties of {path}"))
            }
        }
        Action::Thread { pid, tid, op } => crate::threads::apply(pid, tid, op),
        Action::CloseHandles {
            pid,
            process,
            handles,
            what,
        } => {
            let failed: Vec<String> = handles
                .iter()
                .filter_map(|&h| {
                    close_remote_handle(pid, h)
                        .err()
                        .map(|e| format!("0x{h:X}: {e}"))
                })
                .collect();
            if failed.is_empty() {
                Ok(format!(
                    "Closed {} handles to {what} in {process} ({pid})",
                    handles.len()
                ))
            } else {
                Err(format!(
                    "Could not close {} of {} handles to {what} in {process} ({pid}): {}",
                    failed.len(),
                    handles.len(),
                    failed.join("; ")
                ))
            }
        }
        Action::CloseHandle {
            pid,
            process,
            handle,
            name,
        } => close_remote_handle(pid, handle)
            .map(|_| format!("Closed handle 0x{handle:X} ({name}) in {process} ({pid})"))
            .map_err(|e| format!("Could not close handle 0x{handle:X} in {process} ({pid}): {e}")),
        Action::OpenUrl(url) => {
            shell_open(&url);
            return None;
        }
        Action::SearchOnline(query) => {
            shell_open(&format!(
                "https://www.google.com/search?q={}",
                url_encode(&query)
            ));
            return None;
        }
    };
    Some(match result {
        Ok(msg) if msg.is_empty() => return None,
        Ok(msg) => Outcome::Info(msg),
        Err(msg) => Outcome::Error(msg),
    })
}

fn terminate(targets: &[Target]) -> Outcome {
    let mut failures = Vec::new();
    let mut refused = Vec::new();
    let mut done = 0;
    for t in targets {
        match open_process(t.key, PROCESS_TERMINATE)
            .and_then(|h| unsafe { TerminateProcess(h.raw(), 1) })
        {
            Ok(()) => done += 1,
            Err(e) => {
                // "Access is denied" is what Windows also answers for a
                // process that is ending already; say which it is.
                let ending = crate::ending::is_ending(t.key.pid);
                let why = if ending {
                    "it is ending already and Windows cannot finish".to_string()
                } else {
                    error_text(&e)
                };
                failures.push(format!("{}: {why}", t.label()));
                refused.push(t.key);
            }
        }
    }
    match (done, failures.is_empty()) {
        (_, false) => Outcome::Refused {
            keys: refused,
            message: format!(
                "Could not terminate {}{}. The details say why.",
                failures.join("; "),
                if done > 0 {
                    format!(" (ended {done} others)")
                } else {
                    String::new()
                }
            ),
        },
        (1, true) => Outcome::Info(format!("Terminated {}", targets[0].label())),
        (n, true) => Outcome::Info(format!("Terminated {n} processes")),
    }
}

fn nt_call(
    t: &Target,
    access: windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS,
    call: unsafe extern "system" fn(windows::Win32::Foundation::HANDLE) -> ntapi::NTSTATUS,
) -> Result<(), String> {
    let h =
        open_process(t.key, access).map_err(|e| format!("{}: {}", t.label(), error_text(&e)))?;
    let status = unsafe { call(h.raw()) };
    if ntapi::nt_success(status) {
        Ok(())
    } else {
        Err(format!("{}: {}", t.label(), ntapi::status_message(status)))
    }
}

/// The same thing Task Manager's "Efficiency mode" does: EcoQoS + idle priority.
fn set_efficiency_mode(t: &Target, on: bool) -> Result<String, String> {
    let h = open_process(t.key, PROCESS_SET_INFORMATION)
        .map_err(|e| format!("{}: {}", t.label(), error_text(&e)))?;
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    };
    unsafe {
        SetProcessInformation(
            h.raw(),
            ProcessPowerThrottling,
            (&raw const state).cast::<c_void>(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .and_then(|_| unsafe {
        SetPriorityClass(
            h.raw(),
            if on {
                IDLE_PRIORITY_CLASS
            } else {
                NORMAL_PRIORITY_CLASS
            },
        )
    })
    .map(|_| {
        format!(
            "Efficiency mode {} for {}",
            if on { "on" } else { "off" },
            t.label()
        )
    })
    .map_err(|e| format!("{}: {}", t.label(), error_text(&e)))
}

fn open_file_location(path: &str) -> Result<(), String> {
    let p = wide(path);
    unsafe {
        let pidl = ILCreateFromPathW(PCWSTR(p.as_ptr()));
        if pidl.is_null() {
            return Err(format!("Path not found: {path}"));
        }
        let result = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
        result.map_err(|e| error_text(&e))
    }
}

fn shell_open(target: &str) {
    let t = wide(target);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(t.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// What the Run box would start for this line: a file that exists, with
/// or without ".exe", or what is on the PATH or registered (left to the
/// shell to find).
fn exists(program: &str) -> bool {
    let path = std::path::Path::new(program);
    path.exists() || (path.extension().is_none() && path.with_extension("exe").exists())
}

/// Starts a program the way the Run box does. Without administrator
/// rights asked for, an elevated Anabolic has Explorer start it, so it runs
/// as the signed-in user does; with them, it is started elevated (a UAC
/// prompt unless Anabolic is elevated already).
fn run(command: &str, elevated: bool) -> Result<String, String> {
    let (program, args) = crate::actions::split_command(command, exists);
    if program.is_empty() {
        return Err("Nothing to run".into());
    }
    let shown = std::path::Path::new(&program)
        .file_name()
        .map_or(program.clone(), |f| f.to_string_lossy().into_owned());
    let dir = std::env::var("USERPROFILE").unwrap_or_default();
    let as_user = !elevated && crate::hardening::is_elevated();
    let started = if as_user {
        run_as_user(&program, &args, &dir)
    } else {
        shell_execute(&program, &args, &dir, elevated)
    };
    match started {
        Ok(true) if elevated => Ok(format!("Started {shown} as administrator")),
        Ok(true) => Ok(format!("Started {shown}")),
        // Cancelled at the UAC prompt: nothing to say.
        Ok(false) => Ok(String::new()),
        Err(e) => Err(format!("Could not start {shown}: {e}")),
    }
}

/// ShellExecuteEx, its own error dialogs off: true once started, false
/// when the UAC prompt was cancelled.
fn shell_execute(program: &str, args: &str, dir: &str, elevated: bool) -> Result<bool, String> {
    use windows::Win32::Foundation::{ERROR_CANCELLED, GetLastError};
    use windows::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    let (file, params, directory) = (wide(program), wide(args), wide(dir));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: if elevated { w!("runas") } else { w!("open") },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: PCWSTR(directory.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(true),
        Err(_) if unsafe { GetLastError() } == ERROR_CANCELLED => Ok(false),
        Err(e) => Err(error_text(&e)),
    }
}

/// Starts a program as the signed-in user, from an elevated Anabolic:
/// through the desktop's own shell view, whose ShellExecute runs inside
/// Explorer and so with Explorer's rights.
fn run_as_user(program: &str, args: &str, dir: &str) -> Result<bool, String> {
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, CoCreateInstance, IDispatch, IServiceProvider,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{
        IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows, SID_STopLevelBrowser,
        SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
    };
    use windows::core::{BSTR, Interface};
    let shell = || -> windows::core::Result<IShellDispatch2> {
        unsafe {
            let windows: IShellWindows =
                CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
            let nowhere = VARIANT::default();
            let mut hwnd = 0;
            let desktop = windows.FindWindowSW(
                &nowhere,
                &nowhere,
                SWC_DESKTOP,
                &mut hwnd,
                SWFO_NEEDDISPATCH,
            )?;
            let browser: IShellBrowser = desktop
                .cast::<IServiceProvider>()?
                .QueryService(&SID_STopLevelBrowser)?;
            let view = browser.QueryActiveShellView()?;
            // As IDispatch: only that crosses into Explorer's process.
            let folder: IShellFolderViewDual =
                view.GetItemObject::<IDispatch>(SVGIO_BACKGROUND)?.cast()?;
            folder.Application()?.cast()
        }
    };
    let shell = shell().map_err(|_| {
        "Explorer is not running, and only it can start a program without administrator rights \
         from here. Tick \"Run as administrator\" to start it elevated."
            .to_string()
    })?;
    unsafe {
        shell.ShellExecute(
            &BSTR::from(program),
            &VARIANT::from(args),
            &VARIANT::from(dir),
            &VARIANT::from("open"),
            &VARIANT::from(SW_SHOWNORMAL.0),
        )
    }
    .map(|_| true)
    .map_err(|e| error_text(&e))
}

/// Windows' Open dialog over Anabolic's window, for a program file.
fn browse_program() -> Option<String> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let types = [
            COMDLG_FILTERSPEC {
                pszName: w!("Programs"),
                pszSpec: w!("*.exe;*.com;*.bat;*.cmd;*.lnk;*.msc;*.ps1"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("All files"),
                pszSpec: w!("*.*"),
            },
        ];
        let _ = dialog.SetFileTypes(&types);
        let _ = dialog.SetTitle(w!("Run new task"));
        // Cancelled: an error.
        dialog.Show(crate::instance::own_window()).ok()?;
        let name = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const c_void));
        path
    }
}

/// Starts a new elevated instance of the current executable (UAC prompt).
/// It waits for this one to exit before taking over as the session's
/// Anabolic (`--after`), or it would find us still running and hand over.
fn relaunch_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = wide(&exe.to_string_lossy());
    let args = wide(&format!("--after {}", std::process::id()));
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(exe.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err("elevation was cancelled or failed".into())
    }
}

fn close_remote_handle(pid: u32, handle: u64) -> Result<(), String> {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_DUP_HANDLE};
    let target = crate::util::OwnedHandle(
        unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }.map_err(|e| error_text(&e))?,
    );
    unsafe {
        DuplicateHandle(
            target.raw(),
            HANDLE(handle as *mut core::ffi::c_void),
            HANDLE::default(),
            std::ptr::null_mut(),
            0,
            false,
            DUPLICATE_CLOSE_SOURCE,
        )
    }
    .map_err(|e| error_text(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a windowless script that writes the groups of the token it got,
    /// and answers whether that token was elevated (High integrity).
    fn started_elevated(elevated: bool) -> bool {
        let dir = std::env::temp_dir().join(format!("anabolic-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("groups-{elevated}.txt"));
        let _ = std::fs::remove_file(&out);
        let script = dir.join(format!("groups-{elevated}.vbs"));
        std::fs::write(
            &script,
            format!(
                "CreateObject(\"WScript.Shell\").Run \"cmd /c %SystemRoot%\\System32\\whoami.exe /groups > \"\"{}\"\"\", 0, True",
                out.display()
            ),
        )
        .unwrap();
        thread_init();
        let line = format!("wscript.exe //B \"{}\"", script.display());
        run(&line, elevated).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let groups = loop {
            if let Ok(text) = std::fs::read_to_string(&out)
                && text.contains("S-1-16-")
            {
                break text;
            }
            assert!(
                std::time::Instant::now() < until,
                "the script wrote nothing"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        let _ = std::fs::remove_dir_all(&dir);
        // S-1-16-12288: High Mandatory Level.
        groups.contains("S-1-16-12288")
    }

    #[test]
    #[ignore = "needs an elevated test run and Explorer; runs a windowless script"]
    fn runs_with_and_without_admin_rights_from_an_elevated_copy() {
        assert!(crate::hardening::is_elevated(), "run the tests elevated");
        assert!(
            !started_elevated(false),
            "unticked should start it as the user"
        );
        assert!(started_elevated(true));
        // Asked of Windows directly, a missing program is an error, not a
        // dialog. (Through Explorer, Explorer says so itself.)
        let e = run("anabolic-no-such-program-here.exe", true).unwrap_err();
        assert!(
            e.starts_with("Could not start anabolic-no-such-program-here.exe"),
            "{e}"
        );
    }
}
