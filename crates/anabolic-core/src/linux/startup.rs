//! What starts with the session on Linux:
//!
//! - autostart entries (.desktop files in ~/.config/autostart and the
//!   system's /etc/xdg/autostart), which every desktop starts at sign-in;
//! - systemd user units enabled for the session, the user's own and those
//!   enabled for every user (/etc/systemd/user);
//! - Plasma's sign-in and sign-out scripts.
//!
//! Turned off the way the desktop's own settings do it: a system entry by a
//! user copy that says Hidden=true (which Anabolic removes again to turn it
//! on), a user entry by Hidden in place, a unit by disabling it, or masking
//! it for this user when it is enabled for everyone. Nothing else is ever
//! deleted.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::CacheProperties;

use super::*;

/// Nothing to set up on the startup thread.
pub(super) fn thread_init() {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// ~/.config/autostart: this user only.
    AutostartUser,
    /// /etc/xdg/autostart: every user's.
    AutostartSystem,
    /// A systemd user unit this user enabled.
    UnitUser,
    /// A systemd user unit enabled for every user.
    UnitGlobal,
    /// A script Plasma runs as the session starts.
    PlasmaLogin,
    /// A script Plasma runs as the session ends.
    PlasmaLogout,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::AutostartUser => "Autostart",
            Source::AutostartSystem => "Autostart, all users",
            Source::UnitUser => "systemd user unit",
            Source::UnitGlobal => "systemd user unit, all users",
            Source::PlasmaLogin => "Plasma, at sign-in",
            Source::PlasmaLogout => "Plasma, at sign-out",
        }
    }

    /// Run by systemd rather than by the desktop.
    pub fn is_task(self) -> bool {
        matches!(self, Source::UnitUser | Source::UnitGlobal)
    }

    /// Set up for every user. (Turning one off here turns it off for this
    /// user only, which needs no administrator.)
    pub fn machine_wide(self) -> bool {
        matches!(self, Source::AutostartSystem | Source::UnitGlobal)
    }
}

/// Where the entries are, from the XDG variables (and tests' own).
struct Dirs {
    /// ~/.config
    config_home: PathBuf,
    /// $XDG_CONFIG_DIRS, default /etc/xdg.
    config_dirs: Vec<PathBuf>,
    /// The desktops this session is (XDG_CURRENT_DESKTOP).
    desktops: Vec<String>,
}

impl Dirs {
    fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let config_dirs = std::env::var("XDG_CONFIG_DIRS")
            .ok()
            .filter(|d| !d.is_empty())
            .map(|d| d.split(':').map(PathBuf::from).collect())
            .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
        let desktops = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .collect();
        Self {
            config_home,
            config_dirs,
            desktops,
        }
    }

    fn user_autostart(&self) -> PathBuf {
        self.config_home.join("autostart")
    }

    fn user_units(&self) -> PathBuf {
        self.config_home.join("systemd/user")
    }
}

/// A desktop entry's [Desktop Entry] group, keys in order.
fn desktop_entry(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    out
}

fn is_true(value: Option<&String>) -> bool {
    value.is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn is_false(value: Option<&String>) -> bool {
    value.is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

/// The arguments of an Exec line, quoting undone and field codes (%f, %U…)
/// dropped, as the desktop runs it without files to open.
pub fn exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args.into_iter()
        .filter(|a| !(a.len() == 2 && a.starts_with('%') && a != "%%"))
        .map(|a| a.replace("%%", "%"))
        .collect()
}

/// The program a command starts, through PATH when it names no folder,
/// symbolic links followed (it is matched to running processes by path).
fn program_of(command: &[String]) -> Option<(String, bool)> {
    // `env VAR=value program …` starts the program.
    let mut args = command.iter();
    let mut first = args.next()?;
    if first == "env" || first.ends_with("/env") {
        first = args.find(|a| !a.contains('=') && !a.starts_with('-'))?;
    }
    let found = if first.contains('/') {
        Some(PathBuf::from(first)).filter(|p| p.exists())
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(first))
                .find(|p| p.is_file())
        })
    };
    Some(match found {
        Some(path) => (
            std::fs::canonicalize(&path)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned(),
            false,
        ),
        None => (first.clone(), true),
    })
}

/// When a file was last written, as a FILETIME.
fn written(path: &Path) -> Option<i64> {
    let at = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    let unix = at.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(crate::util::filetime_from_unix(unix.as_secs_f64()))
}

/// What Anabolic writes to turn a system entry off for this user.
const OVERRIDE: &str = "[Desktop Entry]\n# Written by Anabolic: turns the system's entry of the same name off for this user.\nX-Anabolic-Override=true\nHidden=true\n";

/// Every autostart entry that applies to this desktop, the user's copies
/// winning over the system's.
fn autostart(dirs: &Dirs, out: &mut Vec<StartupItem>) {
    // file name -> (path, source), the first found winning.
    let mut found: BTreeMap<String, Vec<(PathBuf, Source)>> = BTreeMap::new();
    let user = dirs.user_autostart();
    let system: Vec<PathBuf> = dirs
        .config_dirs
        .iter()
        .map(|d| d.join("autostart"))
        .collect();
    for (dir, source) in std::iter::once((user, Source::AutostartUser))
        .chain(system.into_iter().map(|d| (d, Source::AutostartSystem)))
    {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".desktop") {
                found.entry(name).or_default().push((entry.path(), source));
            }
        }
    }
    for (file, places) in found {
        let (path, source) = &places[0];
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let entry = desktop_entry(&text);
        // A system entry stays the system's when the user has a copy of it
        // (the desktop's settings switch one off that way). A copy that
        // only switches it off says nothing else: the system's file does.
        let system = places.iter().find(|(_, s)| *s == Source::AutostartSystem);
        let only_switch = is_true(entry.get("X-Anabolic-Override")) || !entry.contains_key("Exec");
        let (shown, shown_source, shown_entry) = match system {
            Some((system_path, _)) if *source == Source::AutostartUser && only_switch => {
                let text = std::fs::read_to_string(system_path).unwrap_or_default();
                (
                    system_path.clone(),
                    Source::AutostartSystem,
                    desktop_entry(&text),
                )
            }
            Some(_) => (path.clone(), Source::AutostartSystem, entry.clone()),
            None => (path.clone(), *source, entry.clone()),
        };
        // Meant for other desktops: not started in this one.
        let listed = |key: &str| {
            shown_entry.get(key).map(|v| {
                v.split(';').any(|d| {
                    dirs.desktops
                        .iter()
                        .any(|mine| mine.eq_ignore_ascii_case(d))
                })
            })
        };
        if listed("OnlyShowIn") == Some(false) || listed("NotShowIn") == Some(true) {
            continue;
        }
        let off = is_true(entry.get("Hidden")) || is_false(entry.get("X-GNOME-Autostart-enabled"));
        let exec = shown_entry.get("Exec").cloned().unwrap_or_default();
        let args = exec_args(&exec);
        let program = program_of(&args);
        let try_missing = shown_entry
            .get("TryExec")
            .and_then(|t| program_of(std::slice::from_ref(t)))
            .is_some_and(|(_, missing)| missing);
        out.push(StartupItem {
            name: shown_entry
                .get("Name")
                .cloned()
                .unwrap_or_else(|| file.trim_end_matches(".desktop").to_string()),
            source: shown_source,
            location: shown.to_string_lossy().into_owned(),
            command: args.join(" "),
            program: program.as_ref().map(|(p, _)| p.clone()),
            enabled: !off,
            disabled_at: if off { written(path) } else { None },
            publisher: None,
            description: shown_entry
                .get("Comment")
                .or_else(|| shown_entry.get("GenericName"))
                .cloned(),
            system: shown_source == Source::AutostartSystem,
            missing: try_missing || program.as_ref().is_some_and(|(_, missing)| *missing),
        });
    }
}

/// Where the user manager finds unit files, the first winning.
fn unit_dirs(dirs: &Dirs) -> Vec<PathBuf> {
    vec![
        dirs.user_units(),
        PathBuf::from("/etc/systemd/user"),
        PathBuf::from("/usr/local/lib/systemd/user"),
        PathBuf::from("/usr/lib/systemd/user"),
    ]
}

fn unit_file(dirs: &Dirs, unit: &str) -> Option<PathBuf> {
    unit_dirs(dirs)
        .into_iter()
        .map(|d| d.join(unit))
        .find(|p| p.exists() && std::fs::read_link(p).map_or(true, |t| t != Path::new("/dev/null")))
}

/// Masked for this user: a link to /dev/null in its own unit folder.
fn masked(dirs: &Dirs, unit: &str) -> bool {
    std::fs::read_link(dirs.user_units().join(unit)).is_ok_and(|t| t == Path::new("/dev/null"))
}

/// The units linked from the `.wants`/`.requires` folders under `dir`.
fn wanted(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.ends_with(".wants") || name.ends_with(".requires")) {
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
    out.sort();
    out.dedup();
    out
}

/// The specifiers user units use most (%h for the home folder…), filled in.
fn specifiers(text: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let uid = unsafe { libc::getuid() };
    let user = crate::enrich::user_name(&uid.to_le_bytes()).unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(&home),
            Some('t') => out.push_str(&runtime),
            Some('u') => out.push_str(&user),
            Some('U') => out.push_str(&uid.to_string()),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// What a unit file says of itself: its description, what it runs (a
/// service's first command, what a socket listens on, when a timer
/// fires), and whether it can be enabled.
fn unit_facts(path: &Path) -> (String, String, bool) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let (mut description, mut runs, mut install) = (String::new(), String::new(), false);
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
        let value = specifiers(value.trim());
        match (section, key.trim()) {
            ("Unit", "Description") => description = value,
            ("Service", "ExecStart") if runs.is_empty() => {
                runs = value
                    .trim_start_matches(['@', '-', ':', '+', '!'])
                    .to_string()
            }
            ("Socket", "ListenStream" | "ListenDatagram" | "ListenSequentialPacket")
                if runs.is_empty() =>
            {
                runs = format!("listens on {value}")
            }
            ("Timer", "OnCalendar" | "OnBootSec" | "OnStartupSec") if runs.is_empty() => {
                runs = format!("fires: {key}={value}")
            }
            ("Install", "WantedBy" | "RequiredBy" | "Also") => install = true,
            _ => {}
        }
    }
    (description, runs, install)
}

fn unit_item(dirs: &Dirs, unit: &str, source: Source, enabled: bool) -> StartupItem {
    let file = unit_file(dirs, unit);
    let (description, exec, _) = file.as_deref().map(unit_facts).unwrap_or_default();
    // Sockets and timers run no command of their own.
    let args = if unit.ends_with(".service") {
        exec_args(&exec)
    } else {
        Vec::new()
    };
    let program = program_of(&args);
    let mask = dirs.user_units().join(unit);
    StartupItem {
        name: unit.to_string(),
        source,
        location: file
            .as_ref()
            .map_or_else(|| unit.to_string(), |f| f.to_string_lossy().into_owned()),
        command: if args.is_empty() {
            exec
        } else {
            args.join(" ")
        },
        program: program.as_ref().map(|(p, _)| p.clone()),
        enabled,
        disabled_at: (!enabled && masked(dirs, unit))
            .then(|| written(&mask))
            .flatten(),
        publisher: None,
        description: (!description.is_empty()).then_some(description),
        system: source == Source::UnitGlobal,
        missing: file.is_none() || program.as_ref().is_some_and(|(_, missing)| *missing),
    }
}

/// The systemd user units that start with the session: the user's own
/// (enabled, or installable and not), and those enabled for everyone.
fn units(dirs: &Dirs, out: &mut Vec<StartupItem>) {
    let mine: HashSet<String> = wanted(&dirs.user_units()).into_iter().collect();
    for unit in &mine {
        out.push(unit_item(dirs, unit, Source::UnitUser, true));
    }
    // The user's own units that could be enabled and are not.
    if let Ok(entries) = std::fs::read_dir(dirs.user_units()) {
        for entry in entries.flatten() {
            let unit = entry.file_name().to_string_lossy().into_owned();
            let is_unit = [".service", ".socket", ".timer"]
                .iter()
                .any(|s| unit.ends_with(s));
            if !is_unit || unit.contains('@') || mine.contains(&unit) || masked(dirs, &unit) {
                continue;
            }
            if unit_facts(&entry.path()).2 {
                out.push(unit_item(dirs, &unit, Source::UnitUser, false));
            }
        }
    }
    for unit in wanted(Path::new("/etc/systemd/user")) {
        if mine.contains(&unit) {
            continue;
        }
        let on = !masked(dirs, &unit);
        out.push(unit_item(dirs, &unit, Source::UnitGlobal, on));
    }
}

/// Plasma's scripts: sourced as the session starts, run as it ends.
fn plasma(dirs: &Dirs, out: &mut Vec<StartupItem>) {
    for (folder, source) in [
        ("env", Source::PlasmaLogin),
        ("shutdown", Source::PlasmaLogout),
    ] {
        let dir = dirs.config_home.join("plasma-workspace").join(folder);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".sh") {
                continue;
            }
            let path = entry.path();
            let real = std::fs::canonicalize(&path).ok();
            out.push(StartupItem {
                name,
                source,
                location: path.to_string_lossy().into_owned(),
                command: format!("sh {}", path.display()),
                program: real.as_ref().map(|r| r.to_string_lossy().into_owned()),
                enabled: true,
                disabled_at: None,
                publisher: None,
                description: None,
                system: false,
                missing: real.is_none(),
            });
        }
    }
}

fn list_in(dirs: &Dirs) -> StartupList {
    let started = Instant::now();
    let mut items = Vec::new();
    autostart(dirs, &mut items);
    units(dirs, &mut items);
    plasma(dirs, &mut items);
    StartupList {
        items,
        errors: Vec::new(),
        took: started.elapsed(),
    }
}

pub fn list() -> StartupList {
    list_in(&Dirs::from_env())
}

/// Sets Hidden (and GNOME's switch with it) in a desktop entry's text.
fn with_hidden(text: &str, hidden: bool) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut inside = false;
    let mut written = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if inside && !written {
                out.push_str(&format!("Hidden={hidden}\n"));
                written = true;
            }
            inside = trimmed == "[Desktop Entry]";
        } else if inside {
            let key = trimmed.split_once('=').map(|(k, _)| k.trim());
            if key == Some("Hidden") {
                if !written {
                    out.push_str(&format!("Hidden={hidden}\n"));
                    written = true;
                }
                continue;
            }
            if key == Some("X-GNOME-Autostart-enabled") {
                out.push_str(&format!("X-GNOME-Autostart-enabled={}\n", !hidden));
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if inside && !written {
        out.push_str(&format!("Hidden={hidden}\n"));
    }
    out
}

/// An autostart entry on or off. A system entry gets a user copy that
/// hides it, removed again to turn it on (only if Anabolic wrote it; a
/// copy of the user's own is switched in place).
fn set_autostart(dirs: &Dirs, item: &StartupItem, on: bool) -> Result<(), String> {
    let path = Path::new(&item.location);
    let file = path.file_name().ok_or("the entry has no file")?;
    let user_dir = dirs.user_autostart();
    let user_copy = user_dir.join(file);
    let error = |e: std::io::Error| crate::util::error_text(&e);
    if !user_copy.exists() {
        if on {
            return Ok(());
        }
        std::fs::create_dir_all(&user_dir).map_err(error)?;
        return std::fs::write(&user_copy, OVERRIDE).map_err(error);
    }
    let text = std::fs::read_to_string(&user_copy).map_err(error)?;
    if on && is_true(desktop_entry(&text).get("X-Anabolic-Override")) {
        return std::fs::remove_file(&user_copy).map_err(error);
    }
    std::fs::write(&user_copy, with_hidden(&text, !on)).map_err(error)
}

/// A user unit enabled or disabled, or (enabled for everyone) masked for
/// this user, through the user's own systemd.
fn set_unit(item: &StartupItem, on: bool) -> Result<(), String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    let manager = zbus::blocking::proxy::Builder::<Proxy<'_>>::new(&conn)
        .destination("org.freedesktop.systemd1")
        .and_then(|b| b.path("/org/freedesktop/systemd1"))
        .and_then(|b| b.interface("org.freedesktop.systemd1.Manager"))
        .map(|b| b.cache_properties(CacheProperties::No))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    let files = vec![item.name.as_str()];
    type Changes = Vec<(String, String, String)>;
    let done = match (item.source, on) {
        (Source::UnitGlobal, false) => manager
            .call::<_, _, Changes>("MaskUnitFiles", &(&files, false, true))
            .map(|_| ()),
        (Source::UnitGlobal, true) => manager
            .call::<_, _, Changes>("UnmaskUnitFiles", &(&files, false))
            .map(|_| ()),
        (_, true) => manager
            .call::<_, _, (bool, Changes)>("EnableUnitFiles", &(&files, false, true))
            .map(|_| ()),
        (_, false) => manager
            .call::<_, _, Changes>("DisableUnitFiles", &(&files, false))
            .map(|_| ()),
    };
    done.and_then(|_| manager.call::<_, _, ()>("Reload", &()))
        .map_err(|e| e.to_string())
}

fn set_enabled_in(dirs: &Dirs, item: &StartupItem, on: bool) -> Result<String, String> {
    let result = match item.source {
        Source::AutostartUser | Source::AutostartSystem => set_autostart(dirs, item, on),
        Source::UnitUser | Source::UnitGlobal => set_unit(item, on),
        Source::PlasmaLogin | Source::PlasmaLogout => Err(
            "Plasma has no switch for its scripts: move the file out of the folder to stop it"
                .into(),
        ),
    };
    let what = if on { "on" } else { "off" };
    result
        .map(|_| {
            format!(
                "{} now starts {}",
                item.name,
                if on { "when you sign in" } else { "no more" }
            )
        })
        .map_err(|e| format!("Could not turn {} {what}: {e}", item.name))
}

/// Turns an entry on or off; worded for a toast.
pub fn set_enabled(item: &StartupItem, on: bool) -> Result<String, String> {
    set_enabled_in(&Dirs::from_env(), item, on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exec_lines() {
        assert_eq!(exec_args("yakuake"), vec!["yakuake"]);
        assert_eq!(
            exec_args(r#"/usr/bin/app --tray %U"#),
            vec!["/usr/bin/app", "--tray"]
        );
        assert_eq!(
            exec_args(r#""/opt/My App/app" "a \"b\"" 100%%"#),
            vec!["/opt/My App/app", r#"a "b""#, "100%"]
        );
        let (program, missing) = program_of(&exec_args("sh -c true")).unwrap();
        assert!(!missing);
        assert!(
            program.starts_with('/') && program.contains("sh"),
            "{program}"
        );
        assert!(
            program_of(&exec_args("no-such-program-anywhere"))
                .unwrap()
                .1
        );
        assert_eq!(
            program_of(&exec_args("env FOO=1 no-such-thing")).unwrap().0,
            "no-such-thing"
        );
    }

    #[test]
    fn switches_hidden_in_place() {
        let text = "[Desktop Entry]\nName=App\nHidden=false\nX-GNOME-Autostart-enabled=true\n\
                    [Desktop Action new]\nName=New\n";
        let off = with_hidden(text, true);
        let entry = desktop_entry(&off);
        assert_eq!(entry["Hidden"], "true");
        assert_eq!(entry["X-GNOME-Autostart-enabled"], "false");
        assert!(off.contains("[Desktop Action new]\nName=New"));
        let added = with_hidden("[Desktop Entry]\nName=App\n", true);
        assert_eq!(desktop_entry(&added)["Hidden"], "true");
    }

    /// Autostart folders in a temporary directory, as XDG would have them.
    fn sandbox(tag: &str) -> (Dirs, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("anabolic-startup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let system = root.join("xdg/autostart");
        std::fs::create_dir_all(&system).unwrap();
        std::fs::create_dir_all(root.join("home/autostart")).unwrap();
        std::fs::write(
            system.join("tool.desktop"),
            "[Desktop Entry]\nName=Tool\nComment=Does things\nExec=sh -c 'sleep 1' %U\n",
        )
        .unwrap();
        std::fs::write(
            system.join("gnome-only.desktop"),
            "[Desktop Entry]\nName=Gnome thing\nExec=sh\nOnlyShowIn=GNOME;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("home/autostart/mine.desktop"),
            "[Desktop Entry]\nName=Mine\nExec=no-such-program-anywhere\n",
        )
        .unwrap();
        let dirs = Dirs {
            config_home: root.join("home"),
            config_dirs: vec![root.join("xdg")],
            desktops: vec!["KDE".into()],
        };
        (dirs, root)
    }

    fn find<'a>(list: &'a StartupList, name: &str) -> Option<&'a StartupItem> {
        list.items.iter().find(|i| i.name == name)
    }

    #[test]
    fn lists_and_switches_autostart_entries() {
        let (dirs, root) = sandbox("switch");
        let list = list_in(&dirs);
        assert!(
            find(&list, "Gnome thing").is_none(),
            "another desktop's entry"
        );
        let tool = find(&list, "Tool").expect("the system entry").clone();
        assert!(tool.enabled && tool.system && !tool.missing);
        assert_eq!(tool.source, Source::AutostartSystem);
        assert_eq!(tool.description.as_deref(), Some("Does things"));
        let mine = find(&list, "Mine").expect("the user's entry").clone();
        assert!(mine.missing && !mine.system);

        // Off: a user copy hides it; the system file stays as it was.
        set_enabled_in(&dirs, &tool, false).unwrap();
        let list = list_in(&dirs);
        let off = find(&list, "Tool").unwrap();
        assert!(!off.enabled && off.system && off.disabled_at.is_some());
        assert!(
            std::fs::read_to_string(root.join("xdg/autostart/tool.desktop"))
                .unwrap()
                .contains("Exec=")
        );
        // On: Anabolic's copy goes again.
        set_enabled_in(&dirs, off, true).unwrap();
        assert!(!root.join("home/autostart/tool.desktop").exists());
        assert!(find(&list_in(&dirs), "Tool").unwrap().enabled);

        // A user entry is switched in place.
        set_enabled_in(&dirs, &mine, false).unwrap();
        assert!(!find(&list_in(&dirs), "Mine").unwrap().enabled);
        set_enabled_in(&dirs, &mine, true).unwrap();
        assert!(find(&list_in(&dirs), "Mine").unwrap().enabled);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lists_this_session() {
        let list = list();
        for i in &list.items {
            eprintln!(
                "  {} | {} | {} | on {} | system {} | missing {} | {} | {:?}",
                i.name,
                i.source.label(),
                i.location,
                i.enabled,
                i.system,
                i.missing,
                i.command,
                i.program
            );
        }
        eprintln!("{} entries in {:?}", list.items.len(), list.took);
        assert!(list.errors.is_empty(), "{:?}", list.errors);
    }
}
