// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! systemd unit files as text: the settings of a service that Anabolic
//! shows and changes, read the way systemd reads them (the unit's own
//! file, then its drop-ins), a new service's file, and changes written into
//! a unit file or a drop-in with the rest of it left as it was.

use std::time::Duration;

use crate::services::{Account, FailureAction, Recovery, ServiceSpec};

/// The settings of a service unit that Anabolic shows and changes.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Settings {
    pub description: String,
    /// The first command, as written (its prefixes kept).
    pub exec_start: String,
    pub user: String,
    pub requires: Vec<String>,
    pub after: Vec<String>,
    pub restart: String,
    pub restart_sec: String,
    /// It says when to start ([Install]): it can be enabled.
    pub install: bool,
}

/// The assignments of a unit file as (section, key, value), in order:
/// comments and blank lines dropped, lines continued with a backslash
/// joined.
pub fn entries(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut pending: Option<String> = None;
    for raw in text.lines() {
        let line = match pending.take() {
            Some(start) => format!("{start} {}", raw.trim()),
            None => {
                let line = raw.trim();
                if line.starts_with(['#', ';']) {
                    continue;
                }
                line.to_string()
            }
        };
        if let Some(start) = line.strip_suffix('\\') {
            pending = Some(start.trim_end().to_string());
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.push((
                section.clone(),
                key.trim().to_string(),
                value.trim().to_string(),
            ));
        }
    }
    out
}

/// Adds a list setting's words, or empties the list for an empty one.
fn extend(list: &mut Vec<String>, value: &str) {
    if value.is_empty() {
        list.clear();
        return;
    }
    for word in value.split_ascii_whitespace() {
        if !list.iter().any(|w| w == word) {
            list.push(word.to_string());
        }
    }
}

impl Settings {
    /// Reads one more file over what was read before: the unit's own file,
    /// then each drop-in. Later ones win; an empty assignment empties a
    /// list (and ExecStart).
    pub fn read(&mut self, text: &str) {
        for (section, key, value) in entries(text) {
            match (section.as_str(), key.as_str()) {
                ("Unit", "Description") => self.description = value,
                ("Unit", "Requires") => extend(&mut self.requires, &value),
                ("Unit", "After") => extend(&mut self.after, &value),
                ("Service", "ExecStart") if value.is_empty() => self.exec_start.clear(),
                ("Service", "ExecStart") if self.exec_start.is_empty() => self.exec_start = value,
                ("Service", "User") => self.user = value,
                ("Service", "Restart") => self.restart = value,
                ("Service", "RestartSec") => self.restart_sec = value,
                ("Install", "WantedBy" | "RequiredBy" | "UpheldBy" | "Alias" | "Also") => {
                    self.install |= !value.is_empty()
                }
                _ => {}
            }
        }
    }

    /// What systemd does when it fails: it answers every failure the same
    /// way, after RestartSec (100 ms unless set).
    pub fn recovery(&self) -> Recovery {
        let restart = self.restart.trim();
        let action = if restart.is_empty() || restart == "no" {
            FailureAction::Nothing
        } else {
            FailureAction::Restart
        };
        Recovery {
            actions: [action; 3],
            delay: span(&self.restart_sec).unwrap_or(Duration::from_millis(100)),
            reset_days: None,
            program: String::new(),
            also: restart == "always",
        }
    }

    /// Who it runs as, in a system manager (root unless User says).
    pub fn account(&self) -> Account {
        match self.user.trim() {
            "" | "root" => Account::System,
            name => Account::User(name.to_string()),
        }
    }
}

/// A systemd time span ("5", "5s", "500ms", "1min 30s") as a duration; a
/// bare number is seconds.
pub fn span(text: &str) -> Option<Duration> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut total = 0.0f64;
    let mut rest = text;
    while !rest.is_empty() {
        rest = rest.trim_start();
        let number_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if number_end == 0 {
            return None;
        }
        let number: f64 = rest[..number_end].parse().ok()?;
        rest = rest[number_end..].trim_start();
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let seconds = match &rest[..unit_end] {
            "" | "s" | "sec" | "second" | "seconds" => 1.0,
            "ms" | "msec" => 0.001,
            "us" | "usec" => 0.000_001,
            "m" | "min" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86400.0,
            _ => return None,
        };
        total += number * seconds;
        rest = &rest[unit_end..];
    }
    Some(Duration::from_secs_f64(total))
}

/// A duration as systemd reads it.
pub fn span_text(duration: Duration) -> String {
    if duration.subsec_millis() != 0 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

/// Restart= for what a failure should do.
pub fn restart_value(recovery: &Recovery) -> &'static str {
    match (recovery.actions[0], recovery.also) {
        (FailureAction::Restart, true) => "always",
        (FailureAction::Restart, false) => "on-failure",
        _ => "no",
    }
}

/// A dependency as a unit name: a bare name is a service's.
pub fn unit_of_dependency(name: &str) -> String {
    let name = name.trim();
    if name.contains('.') {
        name.to_string()
    } else {
        format!("{name}.service")
    }
}

fn dependencies(spec: &ServiceSpec) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for unit in spec
        .dependencies
        .iter()
        .filter(|d| !d.trim().is_empty())
        .map(|d| unit_of_dependency(d))
    {
        if !out.contains(&unit) {
            out.push(unit);
        }
    }
    out
}

/// A service unit's name that Anabolic writes files and runs commands
/// for: letters, digits and `:_.@-`, not starting with `-` or `.`, no `..`.
pub fn valid_unit(unit: &str) -> bool {
    let Some(base) = unit.strip_suffix(".service") else {
        return false;
    };
    !base.is_empty()
        && unit.len() <= 255
        && !base.starts_with(['-', '.'])
        && !unit.contains("..")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '@' | '-'))
}

/// The account a system unit names in User=, if not root.
fn user_of(account: &Account) -> Option<String> {
    match account {
        Account::User(name) if !matches!(name.trim(), "" | "root") => Some(name.trim().to_string()),
        _ => None,
    }
}

/// A new service's unit file. `system`: for the system manager (which
/// runs it as User=, at boot), not the user's own (at sign-in).
pub fn render(spec: &ServiceSpec, unit: &str, system: bool) -> String {
    let display = match spec.display.trim() {
        "" => unit.trim_end_matches(".service"),
        shown => shown,
    };
    let mut out = format!("[Unit]\nDescription={display}\n");
    let needs = dependencies(spec);
    if !needs.is_empty() {
        out.push_str(&format!("Requires={0}\nAfter={0}\n", needs.join(" ")));
    }
    out.push_str(&format!("\n[Service]\nExecStart={}\n", spec.program.trim()));
    if system && let Some(user) = user_of(&spec.account) {
        out.push_str(&format!("User={user}\n"));
    }
    let restart = restart_value(&spec.recovery);
    if restart != "no" {
        out.push_str(&format!(
            "Restart={restart}\nRestartSec={}\n",
            span_text(spec.recovery.delay)
        ));
    }
    out.push_str(&format!(
        "\n[Install]\nWantedBy={}\n",
        if system {
            "multi-user.target"
        } else {
            "default.target"
        }
    ));
    out
}

/// One setting to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub section: &'static str,
    pub key: &'static str,
    /// Its words; none removes it (in a drop-in: sets it to nothing).
    pub values: Vec<String>,
    /// In a drop-in, an empty assignment first: it clears what the unit's
    /// own file says (a list, or ExecStart).
    pub reset: bool,
    /// Keeps the words the text has for it already, adding these.
    pub merge: bool,
}

impl Setting {
    fn new(section: &'static str, key: &'static str, values: Vec<String>) -> Self {
        Self {
            section,
            key,
            values,
            reset: false,
            merge: false,
        }
    }
}

/// What differs between two specs, as settings to write. `system`: a
/// system unit, whose account is User=.
pub fn changes(before: &ServiceSpec, after: &ServiceSpec, system: bool) -> Vec<Setting> {
    let mut out = Vec::new();
    let shown = after.display.trim();
    if before.display.trim() != shown && !shown.is_empty() {
        out.push(Setting::new("Unit", "Description", vec![shown.to_string()]));
    }
    if before.program.trim() != after.program.trim() {
        out.push(Setting {
            reset: true,
            ..Setting::new(
                "Service",
                "ExecStart",
                vec![after.program.trim().to_string()],
            )
        });
    }
    if system && user_of(&before.account) != user_of(&after.account) {
        out.push(Setting::new(
            "Service",
            "User",
            user_of(&after.account).into_iter().collect(),
        ));
    }
    let (was, now) = (dependencies(before), dependencies(after));
    if was != now {
        let added: Vec<String> = now.iter().filter(|d| !was.contains(d)).cloned().collect();
        out.push(Setting {
            reset: true,
            ..Setting::new("Unit", "Requires", now)
        });
        // Ordered after what it needs; an order kept from before is
        // harmless.
        if !added.is_empty() {
            out.push(Setting {
                merge: true,
                ..Setting::new("Unit", "After", added)
            });
        }
    }
    let (a, b) = (&before.recovery, &after.recovery);
    if restart_value(a) != restart_value(b) || (restart_value(b) != "no" && a.delay != b.delay) {
        let restart = restart_value(b);
        out.push(Setting::new(
            "Service",
            "Restart",
            vec![restart.to_string()],
        ));
        if restart != "no" {
            out.push(Setting::new(
                "Service",
                "RestartSec",
                vec![span_text(b.delay)],
            ));
        }
    }
    out
}

/// The lines of a section after its header, to the next header.
fn section_span(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let header = format!("[{section}]");
    let start = lines.iter().position(|l| l.trim() == header)? + 1;
    let end = lines[start..]
        .iter()
        .position(|l| l.trim().starts_with('[') && l.trim().ends_with(']'))
        .map_or(lines.len(), |i| start + i);
    Some((start, end))
}

/// `text` with `settings` written into it and everything else left as it
/// was: a setting's old lines give way to the new one, in their place. In a
/// drop-in (`drop_in`) a reset setting comes after an empty assignment, and
/// a setting with no words is written empty rather than left out.
pub fn apply(text: &str, settings: &[Setting], drop_in: bool) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for setting in settings {
        let (start, end) = match section_span(&lines, setting.section) {
            Some(span) => span,
            // [Unit] first, as systemd's own files have it; others last.
            None if setting.section == "Unit" => {
                let mut head = vec!["[Unit]".to_string()];
                if !lines.is_empty() {
                    head.push(String::new());
                }
                lines.splice(0..0, head);
                (1, 1)
            }
            None => {
                if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(format!("[{}]", setting.section));
                (lines.len(), lines.len())
            }
        };
        // The setting's lines now (a continued line with its rest), and
        // its words.
        let mut found = Vec::new();
        let mut words: Vec<String> = Vec::new();
        let mut i = start;
        while i < end {
            let first = lines[i].trim().to_string();
            let mut next = i + 1;
            if first.starts_with(['#', ';']) {
                i = next;
                continue;
            }
            let mut joined = first;
            while joined.ends_with('\\') && next < end {
                joined.pop();
                joined = format!("{} {}", joined.trim_end(), lines[next].trim());
                next += 1;
            }
            if let Some((key, value)) = joined.split_once('=')
                && key.trim() == setting.key
            {
                found.extend(i..next);
                extend(&mut words, value.trim());
            }
            i = next;
        }
        let at = found.first().copied().unwrap_or_else(|| {
            let mut at = end;
            while at > start && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            at
        });
        for &line in found.iter().rev() {
            lines.remove(line);
        }
        let values = if setting.merge {
            for value in &setting.values {
                if !words.contains(value) {
                    words.push(value.clone());
                }
            }
            words
        } else {
            setting.values.clone()
        };
        let key = setting.key;
        let mut new = Vec::new();
        if drop_in && setting.reset {
            new.push(format!("{key}="));
        }
        if !values.is_empty() {
            new.push(format!("{key}={}", values.join(" ")));
        } else if drop_in && !setting.reset {
            new.push(format!("{key}="));
        }
        lines.splice(at..at, new);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ServiceSpec {
        ServiceSpec {
            name: "backup.service".into(),
            display: "Nightly backup".into(),
            program: "/usr/local/bin/backup --all".into(),
            account: Account::User("backup".into()),
            dependencies: vec!["network-online.target".into(), "postgresql".into()],
            recovery: Recovery {
                actions: [FailureAction::Restart; 3],
                delay: Duration::from_secs(5),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn reads_a_unit_and_its_drop_ins_as_systemd_does() {
        let mut settings = Settings::default();
        settings.read(
            "# made by hand\n[Unit]\nDescription=OpenSSH Daemon\nRequires=a.service\n\
             After=network.target \\\n   a.service\n\n[Service]\nExecStart=-/usr/bin/sshd -D\n\
             ExecStart=/usr/bin/other\nUser=sshd\n; a comment = with an equals sign\n\
             [Install]\nWantedBy=multi-user.target\n",
        );
        assert_eq!(settings.description, "OpenSSH Daemon");
        assert_eq!(settings.exec_start, "-/usr/bin/sshd -D");
        assert_eq!(settings.after, vec!["network.target", "a.service"]);
        assert_eq!(settings.account(), Account::User("sshd".into()));
        assert!(settings.install);
        // A drop-in: ExecStart and Requires cleared and set again.
        settings.read(
            "[Unit]\nRequires=\nRequires=b.service\n[Service]\nExecStart=\n\
             ExecStart=/usr/bin/sshd -D -e\nRestart=always\nRestartSec=1min 30s\n",
        );
        assert_eq!(settings.exec_start, "/usr/bin/sshd -D -e");
        assert_eq!(settings.requires, vec!["b.service"]);
        let recovery = settings.recovery();
        assert_eq!(recovery.actions, [FailureAction::Restart; 3]);
        assert!(recovery.also);
        assert_eq!(recovery.delay, Duration::from_secs(90));
    }

    #[test]
    fn reads_time_spans() {
        assert_eq!(span("5"), Some(Duration::from_secs(5)));
        assert_eq!(span("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(span("2min"), Some(Duration::from_secs(120)));
        assert_eq!(span("1h 1s"), Some(Duration::from_secs(3601)));
        assert_eq!(span("1min30s"), Some(Duration::from_secs(90)));
        assert_eq!(span("infinity"), None);
        assert_eq!(span(""), None);
        assert_eq!(span_text(Duration::from_secs(5)), "5s");
        assert_eq!(span_text(Duration::from_millis(1500)), "1500ms");
    }

    #[test]
    fn writes_a_new_unit_that_reads_back() {
        let text = render(&spec(), "backup.service", true);
        assert_eq!(
            text,
            "[Unit]\nDescription=Nightly backup\n\
             Requires=network-online.target postgresql.service\n\
             After=network-online.target postgresql.service\n\n\
             [Service]\nExecStart=/usr/local/bin/backup --all\nUser=backup\n\
             Restart=on-failure\nRestartSec=5s\n\n\
             [Install]\nWantedBy=multi-user.target\n"
        );
        let mut settings = Settings::default();
        settings.read(&text);
        assert_eq!(settings.description, "Nightly backup");
        assert_eq!(settings.exec_start, "/usr/local/bin/backup --all");
        assert_eq!(settings.account(), Account::User("backup".into()));
        assert_eq!(settings.recovery().delay, Duration::from_secs(5));
        // The user's own: no User=, started at sign-in.
        let own = render(&spec(), "backup.service", false);
        assert!(!own.contains("User="));
        assert!(own.contains("WantedBy=default.target"));
    }

    #[test]
    fn changes_a_unit_file_in_place() {
        let before = spec();
        let mut after = before.clone();
        after.display = "Backup".into();
        after.program = "/usr/local/bin/backup --some".into();
        after.account = Account::System;
        after.dependencies = vec!["postgresql.service".into(), "redis".into()];
        after.recovery.actions = [FailureAction::Nothing; 3];
        let settings = changes(&before, &after, true);
        let file = "[Unit]\nDescription=Nightly backup\n# keep this\n\
                    Requires=network-online.target postgresql.service\n\
                    After=network-online.target postgresql.service\n\n\
                    [Service]\nType=oneshot\nExecStart=/usr/local/bin/backup \\\n  --all\n\
                    ExecStart=/usr/bin/true\nUser=backup\nRestart=on-failure\nRestartSec=5s\n\
                    Environment=A=1\n\n[Install]\nWantedBy=multi-user.target\n";
        assert_eq!(
            apply(file, &settings, false),
            "[Unit]\nDescription=Backup\n# keep this\n\
             Requires=postgresql.service redis.service\n\
             After=network-online.target postgresql.service redis.service\n\n\
             [Service]\nType=oneshot\nExecStart=/usr/local/bin/backup --some\n\
             Restart=no\nRestartSec=5s\nEnvironment=A=1\n\n[Install]\nWantedBy=multi-user.target\n"
        );
    }

    #[test]
    fn writes_a_drop_in_for_a_packages_unit() {
        let before = ServiceSpec {
            display: "OpenSSH Daemon".into(),
            program: "/usr/bin/sshd -D".into(),
            dependencies: vec!["sshdgenkeys.service".into()],
            ..Default::default()
        };
        let mut after = before.clone();
        after.program = "/usr/bin/sshd -D -e".into();
        after.account = Account::User("nobody".into());
        after.dependencies.clear();
        after.recovery.actions = [FailureAction::Restart; 3];
        after.recovery.also = true;
        after.recovery.delay = Duration::from_secs(10);
        let settings = changes(&before, &after, true);
        let written = apply("", &settings, true);
        assert_eq!(
            written,
            "[Unit]\nRequires=\n\n[Service]\nExecStart=\nExecStart=/usr/bin/sshd -D -e\n\
             User=nobody\nRestart=always\nRestartSec=10s\n"
        );
        // Changed again: the drop-in's own lines are replaced, not repeated.
        let mut later = after.clone();
        later.account = Account::System;
        later.dependencies = vec!["a".into()];
        let again = apply(&written, &changes(&after, &later, true), true);
        assert_eq!(
            again,
            "[Unit]\nRequires=\nRequires=a.service\nAfter=a.service\n\n\
             [Service]\nExecStart=\nExecStart=/usr/bin/sshd -D -e\nUser=\n\
             Restart=always\nRestartSec=10s\n"
        );
        let mut settings = Settings::default();
        settings.read("[Service]\nExecStart=/usr/bin/sshd -D\nUser=sshd\n");
        settings.read(&again);
        assert_eq!(settings.exec_start, "/usr/bin/sshd -D -e");
        assert_eq!(settings.account(), Account::System);
        assert_eq!(settings.requires, vec!["a.service"]);
    }

    #[test]
    fn knows_the_names_it_may_write() {
        assert!(valid_unit("backup.service"));
        assert!(valid_unit("getty@tty1.service"));
        assert!(valid_unit("a:b_c-d.e.service"));
        assert!(!valid_unit("backup"));
        assert!(!valid_unit(".service"));
        assert!(!valid_unit("-rf.service"));
        assert!(!valid_unit(".hidden.service"));
        assert!(!valid_unit("a/b.service"));
        assert!(!valid_unit("a..b.service"));
        assert!(!valid_unit("a b.service"));
        assert!(!valid_unit("a;b.service"));
        assert!(!valid_unit("a$(x).service"));
        assert_eq!(unit_of_dependency(" redis "), "redis.service");
        assert_eq!(unit_of_dependency("network.target"), "network.target");
    }

    #[test]
    fn nothing_to_write_when_nothing_changed() {
        assert!(changes(&spec(), &spec(), true).is_empty());
        // A delay that does not count while it does not restart.
        let mut before = spec();
        before.recovery.actions = [FailureAction::Nothing; 3];
        let mut after = before.clone();
        after.recovery.delay = Duration::from_secs(30);
        assert!(changes(&before, &after, true).is_empty());
    }
}
