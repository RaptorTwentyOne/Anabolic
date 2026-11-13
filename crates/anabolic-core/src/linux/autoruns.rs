// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What starts by itself on Linux: the systemd units that are enabled (the
//! system's and the user's) and their timers, cron's tables, the desktop's
//! autostart entries, /etc/ld.so.preload (libraries loaded into every
//! program), rc.local and the shell's start files, and the kernel modules
//! loaded at boot. A file the system's packages own is vouched for by the
//! package manager.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use crate::autoruns::{Autorun, Autoruns, Category};
use crate::filefacts::Signature;

fn lines(path: &str) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn files_in(dir: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The program a command line runs: its first word, found on PATH when
/// bare.
fn program(command: &str) -> Option<String> {
    let first = command.split_whitespace().find(|w| !w.contains('='))?;
    let first = first.trim_matches(|c| c == '"' || c == '\'');
    if first.starts_with('/') {
        return Some(first.to_string());
    }
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| format!("{d}/{first}"))
        .find(|p| Path::new(p).exists())
}

fn systemd(out: &mut Vec<Autorun>, errors: &mut Vec<String>, user: bool) {
    let mut cmd = Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    let listed = cmd
        .args([
            "list-unit-files",
            "--state=enabled",
            "--no-legend",
            "--no-pager",
        ])
        .output();
    let Ok(listed) = listed else {
        errors.push("systemctl could not be run".into());
        return;
    };
    for line in String::from_utf8_lossy(&listed.stdout).lines() {
        let Some(unit) = line.split_whitespace().next() else {
            continue;
        };
        let timer = unit.ends_with(".timer");
        if !unit.ends_with(".service") && !timer {
            continue;
        }
        let mut show = Command::new("systemctl");
        if user {
            show.arg("--user");
        }
        let shown = show
            .args([
                "show",
                unit,
                "-p",
                "ExecStart",
                "-p",
                "FragmentPath",
                "-p",
                "Description",
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let field = |name: &str| {
            shown
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{name}=")))
                .unwrap_or("")
                .to_string()
        };
        // ExecStart={ path=/usr/bin/x ; argv[]=/usr/bin/x -a ; ... }
        let exec = field("ExecStart");
        let command = exec
            .split("argv[]=")
            .nth(1)
            .map(|a| a.split(" ;").next().unwrap_or(a).trim().to_string())
            .unwrap_or_default();
        let location = field("FragmentPath");
        let category = if timer {
            Category::Cron
        } else {
            Category::Services
        };
        let mut a = Autorun::new(category, &location, unit, &command);
        a.description = Some(field("Description")).filter(|d| !d.is_empty());
        a.path = program(&command).or_else(|| (!location.is_empty()).then(|| location.clone()));
        if user {
            a.location = format!("{location} (yours)");
        }
        out.push(a);
    }
}

fn cron(out: &mut Vec<Autorun>) {
    let mut tables = vec!["/etc/crontab".to_string()];
    tables.extend(files_in("/etc/cron.d"));
    tables.extend(files_in("/var/spool/cron/crontabs"));
    tables.extend(files_in("/var/spool/cron"));
    for table in tables {
        if Path::new(&table).is_dir() {
            continue;
        }
        let system = table.starts_with("/etc/");
        for line in lines(&table) {
            // Variable settings are not jobs.
            if line
                .split_whitespace()
                .next()
                .is_some_and(|w| w.contains('='))
            {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let (when, rest) = if line.starts_with('@') {
                (words[0].to_string(), words[1..].to_vec())
            } else if words.len() > 5 {
                (words[..5].join(" "), words[5..].to_vec())
            } else {
                continue;
            };
            // System tables name the user before the command.
            let command = if system && rest.len() > 1 {
                rest[1..].join(" ")
            } else {
                rest.join(" ")
            };
            let mut a = Autorun::new(Category::Cron, &table, &when, &command);
            a.path = program(&command);
            out.push(a);
        }
    }
    for dir in [
        "/etc/cron.hourly",
        "/etc/cron.daily",
        "/etc/cron.weekly",
        "/etc/cron.monthly",
    ] {
        for file in files_in(dir) {
            let name = file.rsplit('/').next().unwrap_or(&file).to_string();
            let mut a = Autorun::new(Category::Cron, dir, &name, &file);
            a.path = Some(file);
            out.push(a);
        }
    }
}

fn autostart(out: &mut Vec<Autorun>) {
    let home = std::env::var("HOME").unwrap_or_default();
    for dir in [
        format!("{home}/.config/autostart"),
        "/etc/xdg/autostart".to_string(),
    ] {
        for file in files_in(&dir) {
            if !file.ends_with(".desktop") {
                continue;
            }
            let entries = lines(&file);
            let get = |key: &str| {
                entries
                    .iter()
                    .find_map(|l| l.strip_prefix(&format!("{key}=")))
                    .map(str::to_string)
            };
            let command = get("Exec").unwrap_or_default();
            let name = get("Name").unwrap_or_else(|| file.rsplit('/').next().unwrap_or("").into());
            let mut a = Autorun::new(Category::Logon, &file, &name, &command);
            a.enabled = get("Hidden").as_deref() != Some("true")
                && get("X-GNOME-Autostart-enabled").as_deref() != Some("false");
            a.path = program(&command);
            out.push(a);
        }
    }
}

fn shell(out: &mut Vec<Autorun>) {
    for lib in lines("/etc/ld.so.preload") {
        let mut a = Autorun::new(Category::Shell, "/etc/ld.so.preload", &lib, &lib);
        a.path = Some(lib.clone());
        a.note = Some("Loaded into every program".into());
        out.push(a);
    }
    if Path::new("/etc/rc.local").exists() {
        let mut a = Autorun::new(Category::Shell, "/etc", "rc.local", "/etc/rc.local");
        a.path = Some("/etc/rc.local".into());
        out.push(a);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let mut starts: Vec<String> = files_in("/etc/profile.d");
    for f in [
        ".profile",
        ".bash_profile",
        ".bashrc",
        ".zshrc",
        ".xprofile",
        ".xinitrc",
    ] {
        starts.push(format!("{home}/{f}"));
    }
    for file in starts.into_iter().filter(|f| Path::new(f).is_file()) {
        let name = file.rsplit('/').next().unwrap_or(&file).to_string();
        let mut a = Autorun::new(Category::Shell, &file, &name, &file);
        a.path = Some(file.clone());
        out.push(a);
    }
}

fn modules(out: &mut Vec<Autorun>) {
    let mut confs = vec!["/etc/modules".to_string()];
    confs.extend(files_in("/etc/modules-load.d"));
    for conf in confs {
        for module in lines(&conf) {
            let mut a = Autorun::new(Category::Modules, &conf, &module, &module);
            a.path = None;
            out.push(a);
        }
    }
}

pub fn read() -> Autoruns {
    let started = Instant::now();
    let mut out = Vec::new();
    let mut errors = Vec::new();
    systemd(&mut out, &mut errors, false);
    systemd(&mut out, &mut errors, true);
    cron(&mut out);
    autostart(&mut out);
    shell(&mut out);
    modules(&mut out);
    for a in &mut out {
        let Some(path) = a.path.clone() else {
            continue;
        };
        if !Path::new(&path).exists() {
            a.missing = true;
            continue;
        }
        let signature = crate::filefacts::signature(&path);
        a.system = matches!(signature, Signature::Package { .. });
        a.signature = Some(signature);
    }
    out.sort_by(|a, b| {
        (a.category, a.name.to_lowercase()).cmp(&(b.category, b.name.to_lowercase()))
    });
    Autoruns {
        entries: out,
        errors,
        took: started.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_commands_program() {
        assert_eq!(
            program("/usr/bin/foo --bar").as_deref(),
            Some("/usr/bin/foo")
        );
        assert_eq!(program("FOO=1 /opt/x/run").as_deref(), Some("/opt/x/run"));
    }
}
