//! Taking over Ctrl+Shift+Esc. Windows runs the "debugger" registered for an
//! image under Image File Execution Options instead of the image itself, so
//! pointing taskmgr.exe's at anabolic-launch.exe makes every way of opening
//! Task Manager open Anabolic. The launcher brings a running instance to the
//! front (see `instance`) or starts one through a scheduled task that runs
//! elevated, which avoids a UAC prompt on every Ctrl+Shift+Esc.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

use crate::util::{from_wide, wide};

pub const TASK_NAME: &str = "Anabolic Elevated Start";
pub const LAUNCHER: &str = "anabolic-launch.exe";
const APP: &str = "anabolic.exe";
const IFEO: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn ifeo_key(image: &str) -> Vec<u16> {
    wide(&format!(r"{IFEO}\{image}"))
}

pub fn app_path() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.with_file_name(APP))
}

pub fn launcher_path() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.with_file_name(LAUNCHER))
}

fn debugger(image: &str) -> Option<String> {
    let key = ifeo_key(image);
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            w!("Debugger"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then(|| from_wide(&buf))
}

fn set_debugger(image: &str, command: Option<&str>) -> Result<(), String> {
    let key = ifeo_key(image);
    let status = match command {
        Some(command) => {
            let value = wide(command);
            unsafe {
                RegSetKeyValueW(
                    HKEY_LOCAL_MACHINE,
                    PCWSTR(key.as_ptr()),
                    w!("Debugger"),
                    REG_SZ.0,
                    Some(value.as_ptr().cast()),
                    (value.len() * 2) as u32,
                )
            }
        }
        None => unsafe {
            RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), w!("Debugger"))
        },
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(windows::core::Error::from(status.to_hresult()).message())
    }
}

/// What Ctrl+Shift+Esc starts at the moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registration {
    /// taskmgr.exe starts our launcher.
    pub replaced: bool,
    /// The anabolic.exe the launcher starts: the second quoted path of its
    /// command line.
    pub app: Option<String>,
}

/// A registry read: do it off the UI thread.
pub fn registration() -> Registration {
    match debugger("taskmgr.exe") {
        Some(command) if command.to_lowercase().contains(LAUNCHER) => Registration {
            replaced: true,
            app: command.split('"').nth(3).map(str::to_string),
        },
        _ => Registration::default(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let output = Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if text.is_empty() {
            format!("schtasks failed ({})", output.status)
        } else {
            text
        })
    }
}

/// A task with no trigger that starts `exe` with the user's full rights.
fn create_task(name: &str, exe: &Path) -> Result<(), String> {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Anabolic elevated without a UAC prompt (Ctrl+Shift+Esc).</Description>
  </RegistrationInfo>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
    </Exec>
  </Actions>
</Task>
"#,
        xml_escape(&exe.display().to_string())
    );
    let path = std::env::temp_dir().join(format!("anabolic-task-{}.xml", std::process::id()));
    // schtasks wants UTF-16 with a byte order mark.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let result = schtasks(&[
        "/create",
        "/tn",
        name,
        "/xml",
        &path.display().to_string(),
        "/f",
    ]);
    let _ = std::fs::remove_file(&path);
    result
}

fn delete_task(name: &str) -> Result<(), String> {
    schtasks(&["/delete", "/tn", name, "/f"])
}

/// Puts the launcher next to anabolic.exe, or under %LOCALAPPDATA% when that
/// folder is not writable. An identical copy already there is reused.
fn install_launcher(bytes: &[u8]) -> Result<PathBuf, String> {
    let local =
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Anabolic").join(LAUNCHER));
    for path in [launcher_path(), local].into_iter().flatten() {
        if std::fs::read(&path).is_ok_and(|existing| existing == bytes) {
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&path, bytes).is_ok() {
            return Ok(path);
        }
    }
    Err(format!("could not write {LAUNCHER}"))
}

fn replace_image(image: &str, task: &str, on: bool, launcher: &[u8]) -> Result<(), String> {
    if on {
        if launcher.is_empty() {
            return Err(format!("this build carries no {LAUNCHER}"));
        }
        let app = app_path()
            .filter(|p| p.exists())
            .ok_or(format!("{APP} not found"))?;
        let launcher = install_launcher(launcher)?;
        create_task(task, &app)?;
        // The launcher gets anabolic.exe's path first, then whatever Windows appends.
        set_debugger(
            image,
            Some(&format!("\"{}\" \"{}\"", launcher.display(), app.display())),
        )
    } else {
        // Only undo our own registration.
        if debugger(image).is_some_and(|d| d.to_lowercase().contains(LAUNCHER)) {
            set_debugger(image, None)?;
        }
        let _ = delete_task(task);
        Ok(())
    }
}

/// Makes Ctrl+Shift+Esc (and anything else that starts taskmgr.exe) open
/// Anabolic, or undoes that. Needs administrator rights.
pub fn set_replaced(on: bool, launcher: &[u8]) -> Result<(), String> {
    replace_image("taskmgr.exe", TASK_NAME, on, launcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the registry and task plumbing on a made-up image name, so
    /// the real taskmgr.exe registration is never touched. Needs elevation.
    #[test]
    fn registers_and_unregisters_a_dummy_image() {
        if !crate::hardening::is_elevated() {
            eprintln!("not elevated, skipping");
            return;
        }
        let image = "anabolic-selftest-dummy.exe";
        let task = "Anabolic Selftest";
        let launcher = format!("\"C:\\nowhere\\{LAUNCHER}\"");
        set_debugger(image, Some(&launcher)).unwrap();
        assert_eq!(debugger(image).as_deref(), Some(launcher.as_str()));
        create_task(task, Path::new(r"C:\nowhere\anabolic.exe")).unwrap();
        assert!(schtasks(&["/query", "/tn", task]).is_ok());

        replace_image(image, task, false, &[]).unwrap();
        assert_eq!(debugger(image), None);
        assert!(schtasks(&["/query", "/tn", task]).is_err());
        // Clean up the empty key the test left behind.
        let key = ifeo_key(image);
        unsafe {
            let _ = windows::Win32::System::Registry::RegDeleteKeyW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(key.as_ptr()),
            );
        }
    }
}
