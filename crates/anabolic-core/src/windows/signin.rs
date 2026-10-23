// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Starting at sign-in on Windows: a Task Scheduler task for an
//! administrator (elevated, an installed copy), the Run key otherwise.

use std::path::Path;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::TaskScheduler::{
    ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_ENUM_HIDDEN,
    TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::core::{BSTR, PCWSTR};

use super::SignIn;
use crate::util::{error_text, from_wide, wide};

/// The Task Scheduler folder of Anabolic's tasks.
const FOLDER: &str = "Anabolic";
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Anabolic";

pub(super) fn state(exe: &Path) -> SignIn {
    if !crate::hardening::is_elevated() {
        return if run_value().is_some() {
            SignIn::On
        } else {
            SignIn::Off
        };
    }
    if !crate::update::installed_copy(exe) {
        return SignIn::Unavailable(
            "Only an installed Anabolic starts at sign-in on an administrator's account: it \
             starts elevated without asking, so its file has to be where only administrators can \
             change it. The installer puts it in Program Files."
                .into(),
        );
    }
    match task_folder().map(|f| f.is_some_and(|f| has_task(&f, &task_name()))) {
        Ok(true) => SignIn::On,
        Ok(false) => SignIn::Off,
        Err(e) => SignIn::Unavailable(format!("Task Scheduler cannot be read: {e}")),
    }
}

pub(super) fn set(exe: &Path, on: bool) -> Result<(), String> {
    if !crate::hardening::is_elevated() {
        return set_run_value(exe, on);
    }
    if let SignIn::Unavailable(why) = state(exe) {
        return Err(why);
    }
    let failed = |e: windows::core::Error| format!("Task Scheduler refused: {}", error_text(&e));
    let service = scheduler().map_err(failed)?;
    unsafe {
        let root = service.GetFolder(&BSTR::from("\\")).map_err(failed)?;
        if on {
            let folder = match service.GetFolder(&BSTR::from(format!("\\{FOLDER}"))) {
                Ok(folder) => folder,
                Err(_) => root
                    .CreateFolder(&BSTR::from(FOLDER), &VARIANT::default())
                    .map_err(failed)?,
            };
            let empty = VARIANT::default();
            folder
                .RegisterTask(
                    &BSTR::from(task_name()),
                    &BSTR::from(task_xml(exe)),
                    TASK_CREATE_OR_UPDATE.0,
                    &empty,
                    &empty,
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &empty,
                )
                .map_err(failed)?;
        } else if let Ok(folder) = service.GetFolder(&BSTR::from(format!("\\{FOLDER}"))) {
            if has_task(&folder, &task_name()) {
                folder
                    .DeleteTask(&BSTR::from(task_name()), 0)
                    .map_err(failed)?;
            }
            // The folder goes with its last task.
            if task_count(&folder) == 0 {
                let _ = root.DeleteFolder(&BSTR::from(FOLDER), 0);
            }
        }
    }
    Ok(())
}

pub(super) fn forget_everyone() -> Result<(), String> {
    let failed = |e: windows::core::Error| format!("Task Scheduler refused: {}", error_text(&e));
    let service = scheduler().map_err(failed)?;
    unsafe {
        let Ok(folder) = service.GetFolder(&BSTR::from(format!("\\{FOLDER}"))) else {
            return Ok(());
        };
        if let Ok(tasks) = folder.GetTasks(TASK_ENUM_HIDDEN.0) {
            let names: Vec<BSTR> = (1..=tasks.Count().unwrap_or(0))
                .filter_map(|i| tasks.get_Item(&variant_i32(i)).ok()?.Name().ok())
                .collect();
            for name in names {
                folder.DeleteTask(&name, 0).map_err(failed)?;
            }
        }
        let root = service.GetFolder(&BSTR::from("\\")).map_err(failed)?;
        root.DeleteFolder(&BSTR::from(FOLDER), 0).map_err(failed)
    }
}

/// One task per user who asked for it, by name.
fn task_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| crate::util::user_sid());
    format!("Start at sign-in ({user})")
}

/// At this user's sign-in, with the highest rights the user has, without
/// the limits Task Scheduler puts on tasks by default: no time limit, not
/// held back on battery, and at normal priority, not below it.
fn task_xml(exe: &Path) -> String {
    let sid = xml_text(&crate::util::user_sid());
    let exe = xml_text(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>Anabolic</Author>
    <Description>Starts Anabolic in the notification area at sign-in. Anabolic's Settings turn it off.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>false</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>--tray</Arguments>
    </Exec>
  </Actions>
</Task>"#
    )
}

fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn scheduler() -> windows::core::Result<ITaskService> {
    unsafe {
        // Whatever this thread had already stays: only a first call sets it.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let empty = VARIANT::default();
        service.Connect(&empty, &empty, &empty, &empty)?;
        Ok(service)
    }
}

/// Anabolic's folder, None when there is none yet.
fn task_folder() -> Result<Option<ITaskFolder>, String> {
    let service = scheduler().map_err(|e| error_text(&e))?;
    Ok(unsafe { service.GetFolder(&BSTR::from(format!("\\{FOLDER}"))) }.ok())
}

fn has_task(folder: &ITaskFolder, name: &str) -> bool {
    unsafe { folder.GetTask(&BSTR::from(name)) }.is_ok()
}

fn task_count(folder: &ITaskFolder) -> i32 {
    unsafe { folder.GetTasks(TASK_ENUM_HIDDEN.0).and_then(|t| t.Count()) }.unwrap_or(0)
}

fn variant_i32(value: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_I4;
        (*v.Anonymous.Anonymous).Anonymous.lVal = value;
    }
    v
}

/// The Run key's value, for a standard account.
fn run_value() -> Option<String> {
    let (key, name) = (wide(RUN), wide(RUN_VALUE));
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then(|| from_wide(&buf))
}

fn set_run_value(exe: &Path, on: bool) -> Result<(), String> {
    let (key, name) = (wide(RUN), wide(RUN_VALUE));
    let status = unsafe {
        if on {
            let command = wide(&format!("\"{}\" --tray", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        } else {
            match RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
            ) {
                // Already gone.
                e if e.0 == 2 => return Ok(()),
                e => e,
            }
        }
    };
    status
        .ok()
        .map_err(|e| format!("The Run key refused: {}", error_text(&e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes the task for real and takes it away again: elevated, with
    /// Anabolic installed: `cargo test -p anabolic-core
    /// task_comes_and_goes -- --ignored`.
    #[test]
    #[ignore]
    fn task_comes_and_goes() {
        let exe = std::path::PathBuf::from(std::env::var("ProgramFiles").unwrap())
            .join(r"Anabolic\anabolic.exe");
        assert_eq!(state(&exe), SignIn::Off);
        set(&exe, true).unwrap();
        assert_eq!(state(&exe), SignIn::On);
        // As Task Scheduler keeps it.
        let folder = task_folder().unwrap().unwrap();
        let task = unsafe { folder.GetTask(&BSTR::from(task_name())) }.unwrap();
        println!("{}", unsafe { task.Xml() }.unwrap());
        set(&exe, true).unwrap();
        set(&exe, false).unwrap();
        assert_eq!(state(&exe), SignIn::Off);
        assert!(task_folder().unwrap().is_none(), "the empty folder stays");
        set(&exe, true).unwrap();
        forget_everyone().unwrap();
        assert_eq!(state(&exe), SignIn::Off);
        assert!(task_folder().unwrap().is_none());
    }

    #[test]
    fn task_xml_escapes_what_it_holds() {
        let xml = task_xml(Path::new(r"C:\A & B\<x>\anabolic.exe"));
        assert!(xml.contains(r"<Command>C:\A &amp; B\&lt;x&gt;\anabolic.exe</Command>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
    }

    /// An admin's copy in a folder anyone may change does not offer it.
    #[test]
    fn a_copy_outside_program_files_is_not_started_elevated() {
        if !crate::hardening::is_elevated() {
            return;
        }
        let exe = std::env::temp_dir().join("anabolic.exe");
        assert!(matches!(state(&exe), SignIn::Unavailable(_)));
        assert!(set(&exe, true).is_err());
    }
}
