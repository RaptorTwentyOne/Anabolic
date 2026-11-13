// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Everything that starts by itself, in the manner of Autoruns: not only
//! the Run keys and Startup folders but services and drivers, every
//! scheduled task, what Explorer and Winlogon load, programs a debugger
//! setting hijacks, DLLs loaded into every program, what runs before
//! Windows is up, and the packages the security system loads; on Linux,
//! systemd units, cron, autostart entries, the preload list and the
//! modules loaded at boot. Each with the file it runs and who signed it.
//! It only reads.

use std::time::Duration;

use crate::filefacts::Signature;

#[cfg_attr(windows, path = "windows/autoruns.rs")]
#[cfg_attr(target_os = "linux", path = "linux/autoruns.rs")]
mod imp;
pub use imp::read;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// Run keys, Startup folders, Active Setup: at sign-in.
    Logon,
    /// What Explorer loads: shell extensions, overlays, browser helpers.
    Explorer,
    /// Winlogon's shell and its sign-in programs.
    Winlogon,
    Services,
    Drivers,
    Tasks,
    /// Programs started instead of others: debugger settings, file
    /// associations, the command prompt's AutoRun.
    Hijacks,
    /// DLLs loaded into every program (AppInit, AppCert).
    AppInit,
    KnownDlls,
    /// Before Windows is up: BootExecute and its kin.
    BootExecute,
    /// Packages the security system (LSA) loads.
    Lsa,
    PrintMonitors,
    NetworkProviders,
    /// Linux: shell start files, rc.local, the preload list.
    Shell,
    /// Linux: kernel modules loaded at boot.
    Modules,
    /// Linux: cron and timers.
    Cron,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Logon => "Sign-in",
            Category::Explorer => "Explorer",
            Category::Winlogon => "Winlogon",
            Category::Services => "Services",
            Category::Drivers => "Drivers",
            Category::Tasks => "Scheduled tasks",
            Category::Hijacks => "Hijacks",
            Category::AppInit => "AppInit DLLs",
            Category::KnownDlls => "Known DLLs",
            Category::BootExecute => "Boot execute",
            Category::Lsa => "Security packages",
            Category::PrintMonitors => "Print monitors",
            Category::NetworkProviders => "Network providers",
            Category::Shell => "Shell and preload",
            Category::Modules => "Kernel modules",
            Category::Cron => "Cron and timers",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Autorun {
    pub category: Category,
    /// Where it is set: a registry key, a folder, a task's path, a file.
    pub location: String,
    pub name: String,
    /// As it is written there.
    pub command: String,
    /// The file it runs or loads.
    pub path: Option<String>,
    pub description: Option<String>,
    pub company: Option<String>,
    pub signature: Option<Signature>,
    /// Its file is not there.
    pub missing: bool,
    pub enabled: bool,
    /// Part of the system: signed by Microsoft (Windows), or put there by
    /// the system's packages (Linux).
    pub system: bool,
    /// What is odd about it: a setting not at Windows' default.
    pub note: Option<String>,
}

impl Autorun {
    pub fn new(category: Category, location: &str, name: &str, command: &str) -> Autorun {
        Autorun {
            category,
            location: location.to_string(),
            name: name.to_string(),
            command: command.to_string(),
            path: None,
            description: None,
            company: None,
            signature: None,
            missing: false,
            enabled: true,
            system: false,
            note: None,
        }
    }

    /// Worth a look: no valid signature, a missing file, or a note.
    pub fn suspicious(&self) -> bool {
        self.missing
            || self.note.is_some()
            || matches!(
                self.signature,
                Some(Signature::Unsigned) | Some(Signature::Invalid(_))
            )
    }

    /// Who vouches for it, in a few words.
    pub fn signer(&self) -> String {
        match &self.signature {
            Some(Signature::Signed { signer, .. }) => {
                signer.clone().unwrap_or_else(|| "Signed".to_string())
            }
            Some(Signature::Unsigned) => "Not signed".into(),
            Some(Signature::Invalid(why)) => format!("Signature not valid: {why}"),
            Some(Signature::Package { manager, name, .. }) => format!("{manager}: {name}"),
            None if self.missing => "File not found".into(),
            None => String::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Autoruns {
    pub entries: Vec<Autorun>,
    /// Places that could not be read, and why.
    pub errors: Vec<String>,
    pub took: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_is_suspicious() {
        let mut a = Autorun::new(Category::Logon, "HKCU\\Run", "x", "x.exe");
        a.signature = Some(Signature::Signed {
            signer: Some("Microsoft Windows".into()),
            catalog: true,
        });
        assert!(!a.suspicious());
        assert_eq!(a.signer(), "Microsoft Windows");
        a.signature = Some(Signature::Unsigned);
        assert!(a.suspicious());
        a.signature = None;
        a.missing = true;
        assert_eq!(a.signer(), "File not found");
    }
}
