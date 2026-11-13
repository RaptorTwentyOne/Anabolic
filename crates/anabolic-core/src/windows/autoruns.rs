// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What starts by itself on Windows, from the places Autoruns reads: the
//! registry's Run keys, Active Setup, Explorer's extensions (resolved
//! through their CLSIDs), Winlogon's values, the services and drivers set
//! to start, every scheduled task, Image File Execution Options debuggers,
//! the .exe association and the command prompt's AutoRun, AppInit and
//! AppCert DLLs, Known DLLs, Session Manager's BootExecute, LSA's packages,
//! print monitors and network providers. Each file is checked for a
//! signature (on a few threads at once) and named from its version
//! resource. Nothing is changed.

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
};

use crate::autoruns::{Autorun, Autoruns, Category};
use crate::filefacts::Signature;
use crate::reg::{Value, open, root_name, subkeys, text, value, values};

fn system_root() -> String {
    std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())
}

/// `%VAR%` replaced by the variable's value.
fn expand(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) => {
                let name = &after[..j];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Where Windows looks for a program named without a folder.
fn find(name: &str) -> Option<String> {
    let root = system_root();
    let mut dirs = vec![format!(r"{root}\System32"), root.clone()];
    dirs.extend(
        std::env::var("PATH")
            .unwrap_or_default()
            .split(';')
            .filter(|d| !d.is_empty())
            .map(str::to_string),
    );
    dirs.into_iter()
        .map(|d| format!(r"{}\{name}", d.trim_end_matches('\\')))
        .find(|p| std::path::Path::new(p).exists())
}

/// Endings of files that run or are loaded.
const RUNS: [&str; 12] = [
    ".exe", ".dll", ".sys", ".ocx", ".cpl", ".scr", ".com", ".bat", ".cmd", ".ps1", ".vbs", ".js",
];

/// The file a command or a registry path names: environment variables
/// expanded, `\SystemRoot\` and `\??\` taken off; a whole value that is a
/// file as it stands, else the quoted part or the text up to a program's
/// ending; a bare name looked for where Windows would.
fn resolve(command: &str) -> Option<String> {
    let c = expand(command.trim());
    let c = c.trim().trim_start_matches(r"\??\").trim_end_matches(',');
    let c = if let Some(rest) = c.strip_prefix(r"\SystemRoot\") {
        format!(r"{}\{rest}", system_root())
    } else if c.to_lowercase().starts_with(r"system32\") {
        format!(r"{}\{c}", system_root())
    } else {
        c.to_string()
    };
    if c.is_empty() {
        return None;
    }
    // A quoted file later in a line: `if exist "x.bat" "x.bat"`.
    let quoted = c
        .split('"')
        .skip(1)
        .step_by(2)
        .find(|q| RUNS.iter().any(|e| q.to_lowercase().ends_with(e)))
        .map(str::to_string);
    let path = if let Some(rest) = c.strip_prefix('"') {
        rest.split('"').next()?.to_string()
    } else if let Some(q) = quoted {
        q
    } else if std::path::Path::new(&c).is_file() {
        c.clone()
    } else {
        let lower = c.to_lowercase();
        let end = RUNS
            .iter()
            .filter_map(|e| {
                lower
                    .match_indices(e)
                    .map(|(i, _)| i + e.len())
                    .find(|&end| lower[end..].is_empty() || lower[end..].starts_with([' ', ',']))
            })
            .min();
        match end {
            Some(end) => c[..end].to_string(),
            None => c.split_whitespace().next()?.to_string(),
        }
    };
    let path = path.trim().to_string();
    if path.contains('\\') {
        return Some(path);
    }
    // A bare name: only one with an ending names a file.
    let lower = path.to_lowercase();
    if RUNS.iter().any(|e| lower.ends_with(e)) {
        return find(&path).or(Some(format!(r"{}\System32\{path}", system_root())));
    }
    None
}

/// What a CLSID loads: its InprocServer32 (or LocalServer32) file.
fn clsid_file(clsid: &str) -> Option<(String, Option<String>)> {
    let base = format!(r"CLSID\{clsid}");
    let name = open(HKEY_CLASSES_ROOT, &base, false).and_then(|k| text(&k, ""));
    for server in ["InprocServer32", "LocalServer32"] {
        if let Some(k) = open(HKEY_CLASSES_ROOT, &format!(r"{base}\{server}"), false)
            && let Some(file) = text(&k, "")
        {
            return Some((file, name));
        }
    }
    None
}

struct Reader {
    out: Vec<Autorun>,
    errors: Vec<String>,
}

impl Reader {
    fn push(&mut self, mut a: Autorun) {
        if a.path.is_none() {
            a.path = resolve(&a.command);
        }
        self.out.push(a);
    }

    /// Every value of a key as an entry (Run keys and the like).
    fn key_values(&mut self, category: Category, root: HKEY, path: &str, wow32: bool) {
        let Some(key) = open(root, path, wow32) else {
            return;
        };
        let location = format!(
            r"{}\{path}{}",
            root_name(root),
            if wow32 { " (32-bit)" } else { "" }
        );
        for (name, v) in values(&key) {
            let command = v.text();
            if command.trim().is_empty() {
                continue;
            }
            let name = if name.is_empty() {
                "(default)".into()
            } else {
                name
            };
            self.push(Autorun::new(category, &location, &name, &command));
        }
    }

    /// Subkeys naming CLSIDs (in their name or default value): each one's
    /// file.
    fn clsid_keys(&mut self, category: Category, root: HKEY, path: &str, wow32: bool) {
        let Some(key) = open(root, path, wow32) else {
            return;
        };
        let location = format!(r"{}\{path}", root_name(root));
        for sub in subkeys(&key) {
            let clsid = if sub.starts_with('{') {
                sub.clone()
            } else {
                match open(root, &format!(r"{path}\{sub}"), wow32).and_then(|k| text(&k, "")) {
                    Some(c) if c.starts_with('{') => c,
                    _ => continue,
                }
            };
            let Some((file, name)) = clsid_file(&clsid) else {
                continue;
            };
            let mut a = Autorun::new(category, &location, &name.unwrap_or(sub), &file);
            a.note = None;
            self.push(a);
        }
    }

    fn logon(&mut self) {
        for (root, path) in [
            (
                HKEY_CURRENT_USER,
                r"Software\Microsoft\Windows\CurrentVersion\Run",
            ),
            (
                HKEY_CURRENT_USER,
                r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
            ),
            (
                HKEY_CURRENT_USER,
                r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer\Run",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\Run",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer\Run",
            ),
        ] {
            self.key_values(Category::Logon, root, path, false);
        }
        for path in [
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
        ] {
            self.key_values(Category::Logon, HKEY_LOCAL_MACHINE, path, true);
        }
        // Active Setup: run once per user at sign-in.
        let active = r"Software\Microsoft\Active Setup\Installed Components";
        if let Some(key) = open(HKEY_LOCAL_MACHINE, active, false) {
            for sub in subkeys(&key) {
                let Some(k) = open(HKEY_LOCAL_MACHINE, &format!(r"{active}\{sub}"), false) else {
                    continue;
                };
                let Some(stub) = text(&k, "StubPath") else {
                    continue;
                };
                let name = text(&k, "").unwrap_or(sub);
                self.push(Autorun::new(
                    Category::Logon,
                    &format!(r"HKLM\{active}"),
                    &name,
                    &stub,
                ));
            }
        }
        // The Startup folders, as the Startup page reads them.
        for item in crate::startup::list().items {
            if !matches!(
                item.source,
                crate::startup::Source::FolderUser | crate::startup::Source::FolderCommon
            ) {
                continue;
            }
            let mut a = Autorun::new(Category::Logon, &item.location, &item.name, &item.command);
            a.path = item.program.clone();
            a.enabled = item.enabled;
            self.push(a);
        }
    }

    fn explorer(&mut self) {
        let cv = r"Software\Microsoft\Windows\CurrentVersion";
        for (root, wow32) in [
            (HKEY_LOCAL_MACHINE, false),
            (HKEY_LOCAL_MACHINE, true),
            (HKEY_CURRENT_USER, false),
        ] {
            // name = CLSID values.
            let path = format!(r"{cv}\ShellServiceObjectDelayLoad");
            if let Some(key) = open(root, &path, wow32) {
                for (name, v) in values(&key) {
                    if let Some((file, _)) = clsid_file(&v.text()) {
                        self.push(Autorun::new(
                            Category::Explorer,
                            &format!(r"{}\{path}", root_name(root)),
                            &name,
                            &file,
                        ));
                    }
                }
            }
            self.clsid_keys(
                Category::Explorer,
                root,
                &format!(r"{cv}\Explorer\ShellIconOverlayIdentifiers"),
                wow32,
            );
            self.clsid_keys(
                Category::Explorer,
                root,
                &format!(r"{cv}\Explorer\Browser Helper Objects"),
                wow32,
            );
        }
        for class in [
            "*",
            "Directory",
            r"Directory\Background",
            "Folder",
            "Drive",
            "AllFilesystemObjects",
        ] {
            self.clsid_keys(
                Category::Explorer,
                HKEY_CLASSES_ROOT,
                &format!(r"{class}\shellex\ContextMenuHandlers"),
                false,
            );
        }
    }

    fn winlogon(&mut self) {
        let path = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
        let defaults: [(&str, &[&str]); 5] = [
            ("Shell", &["explorer.exe"]),
            (
                "Userinit",
                &[
                    r"c:\windows\system32\userinit.exe,",
                    r"c:\windows\system32\userinit.exe",
                ],
            ),
            ("Taskman", &[]),
            ("AppSetup", &[]),
            ("VmApplet", &["systempropertiesperformance.exe /pagefile"]),
        ];
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let Some(key) = open(root, path, false) else {
                continue;
            };
            for (name, usual) in defaults {
                let Some(v) = text(&key, name) else {
                    continue;
                };
                let mut a = Autorun::new(
                    Category::Winlogon,
                    &format!(r"{}\{path}", root_name(root)),
                    name,
                    &v,
                );
                let lower = v.trim().to_lowercase();
                if !usual.iter().any(|u| *u == lower) || root == HKEY_CURRENT_USER {
                    a.note = Some(format!("Not Windows' usual {name}"));
                }
                self.push(a);
            }
        }
        if let Some(key) = open(HKEY_LOCAL_MACHINE, r"SYSTEM\Setup", false)
            && let Some(cmd) = text(&key, "CmdLine")
        {
            let mut a = Autorun::new(Category::Winlogon, r"HKLM\SYSTEM\Setup", "CmdLine", &cmd);
            a.note = Some("Runs at the next start, before anyone signs in".into());
            self.push(a);
        }
    }

    fn services(&mut self) {
        let base = r"SYSTEM\CurrentControlSet\Services";
        let Some(key) = open(HKEY_LOCAL_MACHINE, base, false) else {
            self.errors.push("The services could not be read".into());
            return;
        };
        for name in subkeys(&key) {
            let Some(k) = open(HKEY_LOCAL_MACHINE, &format!(r"{base}\{name}"), false) else {
                continue;
            };
            let kind = match value(&k, "Type") {
                Some(Value::Number(n)) => n,
                _ => continue,
            };
            let start = match value(&k, "Start") {
                Some(Value::Number(n)) => n,
                _ => continue,
            };
            let Some(image) = text(&k, "ImagePath") else {
                continue;
            };
            let driver = kind & 0x3 != 0;
            let category = if driver {
                // Boot, system and automatic drivers.
                if start > 2 {
                    continue;
                }
                Category::Drivers
            } else {
                // Automatic services.
                if start != 2 || kind & 0x30 == 0 {
                    continue;
                }
                Category::Services
            };
            let display = text(&k, "DisplayName")
                .filter(|d| !d.starts_with('@'))
                .unwrap_or_else(|| name.clone());
            let mut a = Autorun::new(category, &format!(r"HKLM\{base}\{name}"), &display, &image);
            // A shared service host runs a DLL: that is the service.
            if image.to_lowercase().contains("svchost.exe")
                && let Some(dll) = open(
                    HKEY_LOCAL_MACHINE,
                    &format!(r"{base}\{name}\Parameters"),
                    false,
                )
                .and_then(|p| text(&p, "ServiceDll"))
            {
                a.path = resolve(&dll);
            }
            if driver && !image.contains('\\') {
                a.path = Some(format!(r"{}\System32\drivers\{image}", system_root()));
            }
            self.push(a);
        }
    }

    fn tasks(&mut self) {
        match crate::startup::all_tasks() {
            Ok(tasks) => {
                for t in tasks {
                    let mut a = Autorun::new(Category::Tasks, &t.path, &t.name, &t.command);
                    a.enabled = t.enabled;
                    if !t.when.is_empty() {
                        a.location = format!("{} ({})", t.path, t.when.join(", "));
                    }
                    self.push(a);
                }
            }
            Err(e) => self.errors.push(format!("Scheduled tasks: {e}")),
        }
    }

    fn hijacks(&mut self) {
        let ifeo = r"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options";
        for wow32 in [false, true] {
            let Some(key) = open(HKEY_LOCAL_MACHINE, ifeo, wow32) else {
                continue;
            };
            for program in subkeys(&key) {
                let Some(k) = open(HKEY_LOCAL_MACHINE, &format!(r"{ifeo}\{program}"), wow32) else {
                    continue;
                };
                if let Some(debugger) = text(&k, "Debugger") {
                    let mut a = Autorun::new(
                        Category::Hijacks,
                        &format!(r"HKLM\{ifeo}\{program}"),
                        &program,
                        &debugger,
                    );
                    a.note = Some(format!("Runs instead of {program}"));
                    self.push(a);
                }
            }
        }
        let silent = r"Software\Microsoft\Windows NT\CurrentVersion\SilentProcessExit";
        if let Some(key) = open(HKEY_LOCAL_MACHINE, silent, false) {
            for program in subkeys(&key) {
                if let Some(k) = open(HKEY_LOCAL_MACHINE, &format!(r"{silent}\{program}"), false)
                    && let Some(monitor) = text(&k, "MonitorProcess")
                {
                    let mut a = Autorun::new(
                        Category::Hijacks,
                        &format!(r"HKLM\{silent}\{program}"),
                        &program,
                        &monitor,
                    );
                    a.note = Some(format!("Runs whenever {program} exits"));
                    self.push(a);
                }
            }
        }
        // How .exe files open: "%1" %* unless something sits in between.
        if let Some(k) = open(HKEY_CLASSES_ROOT, r"exefile\shell\open\command", false)
            && let Some(cmd) = text(&k, "")
            && cmd.trim() != "\"%1\" %*"
        {
            let mut a = Autorun::new(
                Category::Hijacks,
                r"HKCR\exefile\shell\open\command",
                ".exe files",
                &cmd,
            );
            a.note = Some("Every program starts through this".into());
            self.push(a);
        }
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let path = r"Software\Microsoft\Command Processor";
            if let Some(k) = open(root, path, false)
                && let Some(cmd) = text(&k, "AutoRun")
            {
                let mut a = Autorun::new(
                    Category::Hijacks,
                    &format!(r"{}\{path}", root_name(root)),
                    "AutoRun",
                    &cmd,
                );
                a.note = Some("Runs in every command prompt".into());
                self.push(a);
            }
        }
    }

    fn appinit(&mut self) {
        let path = r"Software\Microsoft\Windows NT\CurrentVersion\Windows";
        for wow32 in [false, true] {
            if let Some(k) = open(HKEY_LOCAL_MACHINE, path, wow32)
                && let Some(dlls) = text(&k, "AppInit_DLLs")
            {
                let on = matches!(value(&k, "LoadAppInit_DLLs"), Some(Value::Number(1)));
                for dll in dlls.split([',', ' ']).filter(|d| !d.is_empty()) {
                    let mut a = Autorun::new(
                        Category::AppInit,
                        &format!(r"HKLM\{path}"),
                        "AppInit_DLLs",
                        dll,
                    );
                    a.enabled = on;
                    a.note = Some("Loaded into every program that loads user32.dll".into());
                    self.push(a);
                }
            }
        }
        let certs = r"SYSTEM\CurrentControlSet\Control\Session Manager\AppCertDlls";
        if open(HKEY_LOCAL_MACHINE, certs, false).is_some() {
            let before = self.out.len();
            self.key_values(Category::AppInit, HKEY_LOCAL_MACHINE, certs, false);
            for a in &mut self.out[before..] {
                a.note = Some("Loaded into every program that starts another".into());
            }
        }
    }

    fn known_dlls(&mut self) {
        let path = r"SYSTEM\CurrentControlSet\Control\Session Manager\KnownDLLs";
        let Some(k) = open(HKEY_LOCAL_MACHINE, path, false) else {
            return;
        };
        let dir = format!(r"{}\System32", system_root());
        for (name, v) in values(&k) {
            let dll = v.text();
            if !dll.to_lowercase().ends_with(".dll") {
                continue;
            }
            let mut a = Autorun::new(Category::KnownDlls, &format!(r"HKLM\{path}"), &name, &dll);
            a.path = Some(format!(r"{dir}\{dll}"));
            self.push(a);
        }
    }

    fn boot_execute(&mut self) {
        let path = r"SYSTEM\CurrentControlSet\Control\Session Manager";
        let Some(k) = open(HKEY_LOCAL_MACHINE, path, false) else {
            return;
        };
        for name in ["BootExecute", "SetupExecute", "Execute", "S0InitialCommand"] {
            let list = match value(&k, name) {
                Some(Value::List(l)) => l,
                Some(Value::Text(t)) if !t.trim().is_empty() => vec![t],
                _ => continue,
            };
            for cmd in list {
                let mut a =
                    Autorun::new(Category::BootExecute, &format!(r"HKLM\{path}"), name, &cmd);
                let words: Vec<&str> = cmd.split_whitespace().collect();
                let program = words.get(1).or(words.first()).copied().unwrap_or("");
                a.path = Some(format!(r"{}\System32\{program}.exe", system_root()));
                if cmd.trim().to_lowercase() != "autocheck autochk *" {
                    a.note = Some("Not Windows' usual check of the disks".into());
                }
                self.push(a);
            }
        }
    }

    fn lsa(&mut self) {
        let path = r"SYSTEM\CurrentControlSet\Control\Lsa";
        let dir = format!(r"{}\System32", system_root());
        let add = |r: &mut Reader, location: &str, name: &str, packages: Vec<String>| {
            for p in packages {
                let p = p.trim().trim_matches('"');
                if p.is_empty() {
                    continue;
                }
                let file = if p.to_lowercase().ends_with(".dll") {
                    p.to_string()
                } else {
                    format!("{p}.dll")
                };
                let mut a = Autorun::new(Category::Lsa, location, name, p);
                a.path = Some(format!(r"{dir}\{file}"));
                r.push(a);
            }
        };
        for (sub, names) in [
            (
                "",
                &[
                    "Authentication Packages",
                    "Notification Packages",
                    "Security Packages",
                ][..],
            ),
            (r"\OSConfig", &["Security Packages"][..]),
        ] {
            let Some(k) = open(HKEY_LOCAL_MACHINE, &format!("{path}{sub}"), false) else {
                continue;
            };
            for name in names {
                let list = match value(&k, name) {
                    Some(Value::List(l)) => l,
                    Some(Value::Text(t)) => vec![t],
                    _ => continue,
                };
                add(self, &format!(r"HKLM\{path}{sub}"), name, list);
            }
        }
        let providers = r"SYSTEM\CurrentControlSet\Control\SecurityProviders";
        if let Some(k) = open(HKEY_LOCAL_MACHINE, providers, false)
            && let Some(list) = text(&k, "SecurityProviders")
        {
            add(
                self,
                &format!(r"HKLM\{providers}"),
                "SecurityProviders",
                list.split(',').map(str::to_string).collect(),
            );
        }
    }

    fn print_monitors(&mut self) {
        let path = r"SYSTEM\CurrentControlSet\Control\Print\Monitors";
        let Some(k) = open(HKEY_LOCAL_MACHINE, path, false) else {
            return;
        };
        for monitor in subkeys(&k) {
            if let Some(m) = open(HKEY_LOCAL_MACHINE, &format!(r"{path}\{monitor}"), false)
                && let Some(driver) = text(&m, "Driver")
            {
                let mut a = Autorun::new(
                    Category::PrintMonitors,
                    &format!(r"HKLM\{path}\{monitor}"),
                    &monitor,
                    &driver,
                );
                if !driver.contains('\\') {
                    a.path = Some(format!(r"{}\System32\{driver}", system_root()));
                }
                self.push(a);
            }
        }
    }

    fn network_providers(&mut self) {
        let order = r"SYSTEM\CurrentControlSet\Control\NetworkProvider\Order";
        let Some(list) =
            open(HKEY_LOCAL_MACHINE, order, false).and_then(|k| text(&k, "ProviderOrder"))
        else {
            return;
        };
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let path = format!(r"SYSTEM\CurrentControlSet\Services\{name}\NetworkProvider");
            if let Some(k) = open(HKEY_LOCAL_MACHINE, &path, false)
                && let Some(dll) = text(&k, "ProviderPath")
            {
                let display = text(&k, "Name").unwrap_or_else(|| name.to_string());
                self.push(Autorun::new(
                    Category::NetworkProviders,
                    &format!(r"HKLM\{path}"),
                    &display,
                    &dll,
                ));
            }
        }
    }
}

/// Fills in each entry's file: whether it is there, what it is, who
/// signed it. Each file once, on a few threads.
fn describe(entries: &mut [Autorun]) {
    let mut files: Vec<String> = entries
        .iter()
        .filter_map(|a| a.path.clone())
        .map(|p| p.trim_matches('"').to_string())
        .collect();
    files.sort_by_key(|p| p.to_lowercase());
    files.dedup_by_key(|p| p.to_lowercase());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let chunk = files.len().div_ceil(threads).max(1);
    let facts: HashMap<String, Option<(Signature, crate::winfile::FileInfo)>> =
        std::thread::scope(|s| {
            let handles: Vec<_> = files
                .chunks(chunk)
                .map(|part| {
                    s.spawn(move || {
                        part.iter()
                            .map(|p| {
                                let found = std::path::Path::new(p).exists().then(|| {
                                    (crate::filefacts::signature(p), crate::winfile::file_info(p))
                                });
                                (p.to_lowercase(), found)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
    for a in entries {
        let Some(path) = a.path.as_ref().map(|p| p.trim_matches('"').to_string()) else {
            continue;
        };
        match facts.get(&path.to_lowercase()) {
            Some(Some((signature, info))) => {
                a.system = matches!(
                    signature,
                    Signature::Signed { signer: Some(s), .. } if s.starts_with("Microsoft")
                );
                a.signature = Some(signature.clone());
                a.description = info.description.clone();
                a.company = info.company.clone();
            }
            // Windows lists Known DLLs for other processors too (the ARM
            // emulation's): one that is not there is no one's doing.
            _ if a.category == Category::KnownDlls => a.system = true,
            _ => a.missing = true,
        }
        a.path = Some(path);
    }
}

pub fn read() -> Autoruns {
    let started = Instant::now();
    // The task scheduler and shortcuts are COM; this thread may not have
    // it yet (a second call only says so).
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }
    let mut r = Reader {
        out: Vec::new(),
        errors: Vec::new(),
    };
    r.logon();
    r.explorer();
    r.winlogon();
    r.services();
    r.tasks();
    r.hijacks();
    r.appinit();
    r.known_dlls();
    r.boot_execute();
    r.lsa();
    r.print_monitors();
    r.network_providers();
    describe(&mut r.out);
    r.out.sort_by(|a, b| {
        (a.category, a.name.to_lowercase()).cmp(&(b.category, b.name.to_lowercase()))
    });
    Autoruns {
        entries: r.out,
        errors: r.errors,
        took: started.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_places_paths_are_written() {
        let root = system_root();
        assert_eq!(
            resolve(r"\SystemRoot\System32\drivers\acpi.sys")
                .unwrap()
                .to_lowercase(),
            format!(r"{root}\System32\drivers\acpi.sys").to_lowercase()
        );
        assert_eq!(
            resolve(r"system32\drivers\x.sys").unwrap().to_lowercase(),
            format!(r"{root}\system32\drivers\x.sys").to_lowercase()
        );
        assert_eq!(
            resolve(r#""C:\Program Files\App\app.exe" --tray"#).unwrap(),
            r"C:\Program Files\App\app.exe"
        );
        assert_eq!(
            resolve("explorer.exe").unwrap().to_lowercase(),
            format!(r"{root}\explorer.exe").to_lowercase()
        );
        assert_eq!(
            resolve(r"C:\Program Files\7-Zip\7-zip.dll").unwrap(),
            r"C:\Program Files\7-Zip\7-zip.dll"
        );
        assert_eq!(
            resolve(r"%SystemRoot%\system32\regsvr32.exe /s /n /i:/UserInstall x.dll")
                .unwrap()
                .to_lowercase(),
            format!(r"{root}\system32\regsvr32.exe").to_lowercase()
        );
        assert_eq!(resolve("U"), None);
        assert_eq!(
            resolve(r#"if exist "C:\x\hook.bat" "C:\x\hook.bat""#).unwrap(),
            r"C:\x\hook.bat"
        );
    }

    #[test]
    #[ignore = "reads this machine's autostart places and checks signatures"]
    fn reads_this_machines_autoruns() {
        let a = read();
        let mut by: HashMap<Category, (usize, usize)> = HashMap::new();
        for e in &a.entries {
            let c = by.entry(e.category).or_default();
            c.0 += 1;
            if !e.system {
                c.1 += 1;
            }
            if e.suspicious() && !e.system {
                println!(
                    "{:?} {} | {} | {:?} | {}",
                    e.category,
                    e.name,
                    e.signer(),
                    e.path,
                    e.note.as_deref().unwrap_or("")
                );
            }
        }
        let mut by: Vec<_> = by.into_iter().collect();
        by.sort();
        println!("{by:?}");
        println!(
            "{} entries in {:?}; errors {:?}",
            a.entries.len(),
            a.took,
            a.errors
        );
        assert!(a.entries.iter().any(|e| e.category == Category::Services));
    }
}
