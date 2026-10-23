// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Programs as the desktop knows them: their .desktop entries (name,
//! description, icon), and their icons from the icon theme, drawn at the
//! size the process list needs.
//!
//! The entries are read once into an index by executable, desktop file id
//! and window class. Icons follow the freedesktop icon theme rules: the
//! user's theme, the themes it inherits, hicolor, then /usr/share/pixmaps.
//! Most themes are SVG only, so SVGs are drawn (resvg) as well as PNGs read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use crate::enrich::Icon;

/// The side of the icons drawn, in pixels.
pub const ICON_SIZE: u32 = 32;

/// What a desktop entry says of a program.
#[derive(Clone, Debug)]
pub struct App {
    /// The desktop file's id: `org.kde.konsole`.
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
}

/// The XDG data folders, the user's first.
pub fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => dirs.extend(home.as_ref().map(|h| h.join(".local/share"))),
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').map(PathBuf::from));
    // Not always in XDG_DATA_DIRS when Anabolic is started from elsewhere.
    for extra in ["/var/lib/flatpak/exports/share", "/var/lib/snapd/desktop"] {
        dirs.push(PathBuf::from(extra));
    }
    if let Some(h) = &home {
        dirs.push(h.join(".local/share/flatpak/exports/share"));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// The `[Desktop Entry]` group's values, unlocalized.
fn parse_entry(text: &str) -> HashMap<&str, &str> {
    let mut values = HashMap::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.entry(key.trim()).or_insert(value.trim());
        }
    }
    values
}

/// An Exec line's words: spaces separate them, except inside double
/// quotes, where a backslash escapes the next character.
fn exec_words(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut escaped, mut started) = (false, false, false);
    for c in exec.chars() {
        match c {
            _ if escaped => {
                word.push(c);
                escaped = false;
            }
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            _ => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

/// The program an Exec line runs, without `env VAR=…`; for `flatpak run`
/// or a shell, nothing (the app id names it instead).
fn exec_program(exec: &str) -> Option<String> {
    let words = exec_words(exec);
    let mut words = words.iter();
    let mut program = words.next()?;
    if program == "env" || program.ends_with("/env") {
        program = words.find(|w| !w.contains('=') && !w.starts_with('-'))?;
    }
    let name = program.rsplit('/').next()?;
    (!matches!(name, "flatpak" | "sh" | "bash" | "snap") && !name.is_empty())
        .then(|| program.clone())
}

/// Folders programs of many apps live in: an app's own folder says which
/// app it is (/opt/google/chrome), these do not.
fn shared_dir(dir: &Path) -> bool {
    matches!(
        dir.to_str(),
        Some(
            "/usr/bin"
                | "/bin"
                | "/usr/sbin"
                | "/sbin"
                | "/usr/local/bin"
                | "/usr/lib"
                | "/usr/libexec"
        )
    )
}

/// Desktop entries by every key a process may be matched with.
#[derive(Default)]
struct Entries {
    by_key: HashMap<String, Arc<App>>,
    /// An app's own folder (its program's, links followed) -> the app.
    by_dir: HashMap<PathBuf, Arc<App>>,
}

impl Entries {
    fn load() -> Self {
        let mut entries = Self::default();
        let mut ids = std::collections::HashSet::new();
        for dir in data_dirs() {
            let apps = dir.join("applications");
            let mut files: Vec<PathBuf> = Vec::new();
            for e in std::fs::read_dir(&apps).into_iter().flatten().flatten() {
                let path = e.path();
                if path.is_dir() {
                    files.extend(
                        std::fs::read_dir(&path)
                            .into_iter()
                            .flatten()
                            .flatten()
                            .map(|e| e.path()),
                    );
                } else {
                    files.push(path);
                }
            }
            files.sort();
            for file in files {
                if file.extension().is_none_or(|e| e != "desktop") {
                    continue;
                }
                // The id: the path under applications/, '/' made '-'.
                let id = file
                    .strip_prefix(&apps)
                    .unwrap_or(&file)
                    .with_extension("")
                    .to_string_lossy()
                    .replace('/', "-");
                // The first folder's entry of an id wins (the user's).
                if !ids.insert(id.clone()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&file) else {
                    continue;
                };
                entries.add(id, &parse_entry(&text));
            }
        }
        entries
    }

    fn add(&mut self, id: String, values: &HashMap<&str, &str>) {
        if values.get("Type").is_some_and(|t| *t != "Application") {
            return;
        }
        let Some(name) = values.get("Name") else {
            return;
        };
        let hidden =
            values.get("NoDisplay") == Some(&"true") || values.get("Hidden") == Some(&"true");
        let app = Arc::new(App {
            id: id.clone(),
            name: name.to_string(),
            icon: values.get("Icon").map(|i| i.to_string()),
        });
        let mut keys = vec![id.to_lowercase()];
        // org.kde.konsole is konsole's.
        if let Some(last) = id.rsplit('.').next().filter(|l| *l != id) {
            keys.push(last.to_lowercase());
        }
        if let Some(flatpak) = values.get("X-Flatpak") {
            keys.push(flatpak.to_lowercase());
        }
        if let Some(class) = values.get("StartupWMClass") {
            keys.push(class.to_lowercase());
        }
        for exec in [values.get("TryExec"), values.get("Exec")]
            .into_iter()
            .flatten()
        {
            let Some(program) = exec_program(exec) else {
                continue;
            };
            if let Some(file) = program.rsplit('/').next() {
                keys.push(file.to_lowercase());
            }
            let real = if program.starts_with('/') {
                std::fs::canonicalize(&program).ok()
            } else {
                None
            };
            if let Some(dir) = real.as_deref().and_then(Path::parent)
                && !shared_dir(dir)
            {
                self.by_dir
                    .entry(dir.to_path_buf())
                    .or_insert_with(|| app.clone());
            }
        }
        for key in keys {
            match self.by_key.get(&key) {
                // A visible entry beats one kept out of menus.
                Some(_) if hidden => {}
                Some(old) if old.id.to_lowercase() == key => {}
                _ => {
                    self.by_key.insert(key, app.clone());
                }
            }
        }
    }
}

static ENTRIES: LazyLock<Entries> = LazyLock::new(Entries::load);

/// The desktop entry of a process: by its Flatpak app id, its executable,
/// the name it was started as, the kernel's name for it, or its app's own
/// folder.
pub fn find(
    exe: Option<&str>,
    argv0: Option<&str>,
    comm: &str,
    flatpak: Option<&str>,
) -> Option<Arc<App>> {
    let entries = &*ENTRIES;
    let file = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
    let keys = flatpak
        .map(str::to_lowercase)
        .into_iter()
        .chain(exe.map(file))
        .chain(argv0.map(|a| file(a.split(' ').next().unwrap_or(a))))
        .chain(std::iter::once(comm.to_lowercase()));
    for key in keys {
        if let Some(app) = entries.by_key.get(&key) {
            return Some(app.clone());
        }
    }
    let dir = Path::new(exe?).parent()?;
    entries.by_dir.get(dir).cloned()
}

/// A theme's folders with icons, from its index.theme: (folder, nominal
/// size, scalable range).
struct Theme {
    roots: Vec<PathBuf>,
    dirs: Vec<(String, u32, u32, u32)>,
    inherits: Vec<String>,
}

fn load_theme(name: &str, bases: &[PathBuf]) -> Option<Theme> {
    let roots: Vec<PathBuf> = bases
        .iter()
        .map(|b| b.join(name))
        .filter(|r| r.is_dir())
        .collect();
    let index = roots
        .iter()
        .find_map(|r| std::fs::read_to_string(r.join("index.theme")).ok())?;
    let mut dirs = Vec::new();
    let mut inherits = Vec::new();
    let mut group = String::new();
    let mut current: Option<(String, u32, u32, u32, bool)> = None;
    let push = |c: Option<(String, u32, u32, u32, bool)>, dirs: &mut Vec<_>| {
        if let Some((dir, size, min, max, scalable)) = c {
            let (min, max) = if scalable { (min, max) } else { (size, size) };
            dirs.push((dir, size, min, max));
        }
    };
    for line in index.lines() {
        let line = line.trim();
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            push(current.take(), &mut dirs);
            group = g.to_string();
            if group != "Icon Theme" {
                current = Some((group.clone(), 0, 0, u32::MAX, false));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let number = || value.trim().parse::<u32>().unwrap_or(0);
        match (group.as_str(), key.trim(), current.as_mut()) {
            ("Icon Theme", "Inherits", _) => {
                inherits = value.split(',').map(|t| t.trim().to_string()).collect();
            }
            (_, "Size", Some(c)) => c.1 = number(),
            (_, "MinSize", Some(c)) => c.2 = number(),
            (_, "MaxSize", Some(c)) => c.3 = number(),
            (_, "Type", Some(c)) => c.4 = value.trim() == "Scalable",
            _ => {}
        }
    }
    push(current.take(), &mut dirs);
    // Application icons only: other contexts (actions, places) are not
    // what a program is drawn with.
    dirs.retain(|(dir, ..)| dir.contains("apps") || dir.contains("applications"));
    Some(Theme {
        roots,
        dirs,
        inherits,
    })
}

/// The icon theme the desktop uses: KDE's from kdeglobals, GNOME's from
/// its settings, else hicolor.
fn user_theme() -> String {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    if let Ok(kde) = std::fs::read_to_string(config.join("kdeglobals")) {
        let mut in_icons = false;
        for line in kde.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_icons = line == "[Icons]";
            } else if in_icons && let Some(theme) = line.strip_prefix("Theme=") {
                return theme.trim().to_string();
            }
        }
    }
    let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("GNOME"));
    if gnome
        && let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "icon-theme"])
            .output()
    {
        let theme = String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_matches('\'')
            .to_string();
        if !theme.is_empty() {
            return theme;
        }
    }
    "hicolor".into()
}

/// The themes to look in, in order, and where.
struct Icons {
    themes: Vec<Theme>,
    pixmaps: Vec<PathBuf>,
    drawn: HashMap<String, Option<Arc<Icon>>>,
}

impl Icons {
    fn load() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let mut bases = vec![home.join(".local/share/icons"), home.join(".icons")];
        bases.extend(data_dirs().into_iter().map(|d| d.join("icons")));
        let mut themes = Vec::new();
        let mut queue = vec![user_theme()];
        let mut seen = std::collections::HashSet::new();
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(theme) = load_theme(&name, &bases) {
                // Inherited themes come after this one, in their order.
                queue.extend(theme.inherits.iter().rev().cloned());
                themes.push(theme);
            }
        }
        if !seen.contains("hicolor")
            && let Some(theme) = load_theme("hicolor", &bases)
        {
            themes.push(theme);
        }
        let pixmaps = data_dirs().into_iter().map(|d| d.join("pixmaps")).collect();
        Self {
            themes,
            pixmaps,
            drawn: HashMap::new(),
        }
    }

    /// The file for an icon name: in the first theme that has it, the
    /// size nearest ICON_SIZE (a scalable one that covers it best).
    fn file(&self, name: &str) -> Option<PathBuf> {
        for theme in &self.themes {
            let mut best: Option<(u32, PathBuf)> = None;
            for (dir, size, min, max) in &theme.dirs {
                let distance = if (*min..=*max).contains(&ICON_SIZE) {
                    0
                } else {
                    size.abs_diff(ICON_SIZE)
                };
                if best.as_ref().is_some_and(|(d, _)| *d <= distance) {
                    continue;
                }
                for root in &theme.roots {
                    let found = ["png", "svg"]
                        .iter()
                        .map(|ext| root.join(dir).join(format!("{name}.{ext}")))
                        .find(|p| p.is_file());
                    if let Some(found) = found {
                        best = Some((distance, found));
                        break;
                    }
                }
            }
            if let Some((_, file)) = best {
                return Some(file);
            }
        }
        self.pixmaps.iter().find_map(|dir| {
            ["png", "svg", "xpm"]
                .iter()
                .map(|ext| dir.join(format!("{name}.{ext}")))
                .find(|p| p.is_file())
        })
    }
}

static ICONS: LazyLock<Mutex<Icons>> = LazyLock::new(|| Mutex::new(Icons::load()));
static NEXT_ICON_ID: AtomicU64 = AtomicU64::new(1);

/// An icon by theme name or file path, drawn at ICON_SIZE; kept, so each is
/// drawn once.
pub fn icon(name: &str) -> Option<Arc<Icon>> {
    let mut icons = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(known) = icons.drawn.get(name) {
        return known.clone();
    }
    let file = if name.starts_with('/') {
        Some(PathBuf::from(name))
    } else {
        icons.file(name)
    };
    let drawn = file.and_then(|f| draw(&f)).map(|(width, height, rgba)| {
        Arc::new(Icon {
            id: NEXT_ICON_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            rgba,
        })
    });
    icons.drawn.insert(name.to_string(), drawn.clone());
    drawn
}

/// An icon file as RGBA8, about ICON_SIZE pixels square.
fn draw(file: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let data = std::fs::read(file).ok()?;
    match file.extension()?.to_str()? {
        "png" => read_png(&data),
        "svg" | "svgz" => draw_svg(&data),
        _ => None,
    }
}

fn read_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let pixels = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some(shrink(w, h, rgba))
}

/// Halves an icon until it is at most twice ICON_SIZE: the texture stays
/// small, and the UI's filtering does the rest.
fn shrink(mut w: u32, mut h: u32, mut rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    while w > ICON_SIZE * 2 && h > ICON_SIZE * 2 && w.is_multiple_of(2) && h.is_multiple_of(2) {
        let (nw, nh) = (w / 2, h / 2);
        let mut out = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let at = |dx: u32, dy: u32| {
                        rgba[(((y * 2 + dy) * w + x * 2 + dx) * 4 + c) as usize] as u32
                    };
                    out[((y * nw + x) * 4 + c) as usize] =
                        ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4) as u8;
                }
            }
        }
        (w, h, rgba) = (nw, nh, out);
    }
    (w, h, rgba)
}

fn draw_svg(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = ICON_SIZE as f32 / size.width().max(size.height());
    let mut pixmap = tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE)?;
    let (dx, dy) = (
        (ICON_SIZE as f32 - size.width() * scale) / 2.0,
        (ICON_SIZE as f32 - size.height() * scale) / 2.0,
    );
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia keeps colors premultiplied by alpha; the UI wants them not.
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Some((ICON_SIZE, ICON_SIZE, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exec_lines() {
        assert_eq!(
            exec_program("/usr/lib/firefox/firefox %u").as_deref(),
            Some("/usr/lib/firefox/firefox")
        );
        assert_eq!(
            exec_program("env GDK_BACKEND=x11 foo --bar").as_deref(),
            Some("foo")
        );
        assert_eq!(
            exec_program("\"/opt/my app/run\" %f").as_deref(),
            Some("/opt/my app/run")
        );
        assert_eq!(exec_program("/usr/bin/flatpak run org.x.Y"), None);
    }

    #[test]
    fn finds_programs_and_their_icons() {
        let started = std::time::Instant::now();
        let Some(konsole) = find(Some("/usr/bin/konsole"), Some("konsole"), "konsole", None) else {
            return;
        };
        eprintln!("entries read in {:?}", started.elapsed());
        assert_eq!(konsole.name, "Konsole");
        let started = std::time::Instant::now();
        let icon = konsole
            .icon
            .as_deref()
            .and_then(icon)
            .expect("Konsole has an icon");
        eprintln!("icon drawn in {:?}", started.elapsed());
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(
            icon.rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
            "the icon is empty"
        );
        if let Some(firefox) = find(Some("/usr/lib/firefox/firefox"), None, "firefox", None) {
            assert!(firefox.name.contains("Firefox"), "{}", firefox.name);
        }
    }
}
