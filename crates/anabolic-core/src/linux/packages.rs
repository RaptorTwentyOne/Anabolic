// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Which package a file came with: what Linux has in place of a publisher's
//! signature. The package manager's own records are read, nothing is run
//! for pacman and dpkg:
//!
//! - pacman and dpkg list every file of every package. The lists are read
//!   once into an index of path hashes (a few MB), rebuilt when a package
//!   is installed or removed.
//! - rpm keeps a database only its own tool reads, so `rpm -qf` is asked,
//!   once per file.
//! - Flatpak and Snap apps carry their own description: a Flatpak sandbox
//!   has /.flatpak-info, a snap lives under /snap/<name>/<revision>.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// A package, as its manager describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    /// pacman, dpkg, rpm, Flatpak or Snap.
    pub manager: &'static str,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub url: Option<String>,
}

impl Package {
    /// Who the package comes from, in words: "firefox (Arch Linux
    /// package)", "org.mozilla.firefox (Flatpak)". What Windows calls the
    /// company.
    pub fn label(&self) -> String {
        match self.manager {
            "Flatpak" | "Snap" => format!("{} ({})", self.name, self.manager),
            _ => format!("{} ({} package)", self.name, distribution()),
        }
    }
}

/// The distribution's name, from /etc/os-release ("Arch Linux").
pub fn distribution() -> &'static str {
    static NAME: LazyLock<String> = LazyLock::new(|| {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines().find_map(|l| {
                    let value = l.strip_prefix("NAME=")?;
                    Some(value.trim_matches('"').to_string())
                })
            })
            .unwrap_or_else(|| "Linux".into())
    });
    &NAME
}

/// Paths are looked up by a 64-bit hash: a collision among a million
/// files is not something to plan for.
fn hash(path: &str) -> u64 {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// Folders whose files are never programs or libraries: not indexed, to
/// keep the index small.
const SKIPPED: [&str; 8] = [
    "/usr/share/doc/",
    "/usr/share/man/",
    "/usr/share/info/",
    "/usr/share/locale/",
    "/usr/share/icons/",
    "/usr/share/help/",
    "/usr/share/gtk-doc/",
    "/usr/include/",
];

/// Every indexed file's hash, sorted, with its package beside it.
struct Index {
    packages: Vec<Arc<Package>>,
    /// The package's folder in pacman's database, for its file hashes.
    folders: Vec<String>,
    hashes: Vec<u64>,
    owners: Vec<u32>,
}

impl Index {
    fn new(entries: Vec<(Package, String, Vec<String>)>) -> Self {
        let mut pairs: Vec<(u64, u32)> = Vec::new();
        let mut packages = Vec::with_capacity(entries.len());
        let mut folders = Vec::with_capacity(entries.len());
        for (i, (package, folder, files)) in entries.into_iter().enumerate() {
            for file in files {
                if !file.ends_with('/') && !SKIPPED.iter().any(|s| file.starts_with(s)) {
                    pairs.push((hash(&file), i as u32));
                }
            }
            packages.push(Arc::new(package));
            folders.push(folder);
        }
        pairs.sort_unstable();
        let (hashes, owners) = pairs.into_iter().unzip();
        Self {
            packages,
            folders,
            hashes,
            owners,
        }
    }

    fn owner(&self, path: &str) -> Option<usize> {
        let at = self.hashes.binary_search(&hash(path)).ok()?;
        Some(self.owners[at] as usize)
    }
}

/// The text of each `%KEY%` block of a pacman `desc` file.
fn pacman_field<'a>(desc: &'a str, key: &str) -> Option<&'a str> {
    let at = desc.find(&format!("%{key}%\n"))? + key.len() + 3;
    let value = desc[at..].split("\n\n").next()?.trim();
    (!value.is_empty()).then_some(value)
}

const PACMAN: &str = "/var/lib/pacman/local";
const DPKG: &str = "/var/lib/dpkg";

fn pacman_index() -> Index {
    let mut entries = Vec::new();
    for e in std::fs::read_dir(PACMAN).into_iter().flatten().flatten() {
        let folder = e.path();
        let (Ok(desc), Ok(files)) = (
            std::fs::read_to_string(folder.join("desc")),
            std::fs::read_to_string(folder.join("files")),
        ) else {
            continue;
        };
        let Some(name) = pacman_field(&desc, "NAME") else {
            continue;
        };
        let package = Package {
            manager: "pacman",
            name: name.into(),
            version: pacman_field(&desc, "VERSION").unwrap_or_default().into(),
            description: pacman_field(&desc, "DESC").map(str::to_string),
            url: pacman_field(&desc, "URL").map(str::to_string),
        };
        let listed = files
            .split("%FILES%\n")
            .nth(1)
            .unwrap_or_default()
            .split("\n\n")
            .next()
            .unwrap_or_default();
        let paths = listed.lines().map(|l| format!("/{l}")).collect();
        entries.push((package, folder.to_string_lossy().into_owned(), paths));
    }
    Index::new(entries)
}

fn dpkg_index() -> Index {
    // Name -> (version, description, homepage), for installed packages.
    let status = std::fs::read_to_string(format!("{DPKG}/status")).unwrap_or_default();
    let mut known: HashMap<&str, (&str, Option<&str>, Option<&str>)> = HashMap::new();
    for stanza in status.split("\n\n") {
        let field = |key: &str| {
            stanza
                .lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(':').map(str::trim))
        };
        let (Some(name), Some(state)) = (field("Package"), field("Status")) else {
            continue;
        };
        if state.ends_with("installed") {
            known.insert(
                name,
                (
                    field("Version").unwrap_or_default(),
                    field("Description"),
                    field("Homepage"),
                ),
            );
        }
    }
    let mut entries = Vec::new();
    for e in std::fs::read_dir(format!("{DPKG}/info"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = file.strip_suffix(".list") else {
            continue;
        };
        // libc6:amd64.list
        let name = stem.split(':').next().unwrap_or(stem);
        let Ok(list) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let (version, description, url) = known.get(name).copied().unwrap_or_default();
        let package = Package {
            manager: "dpkg",
            name: name.into(),
            version: version.into(),
            description: description.map(str::to_string),
            url: url.map(str::to_string),
        };
        entries.push((
            package,
            String::new(),
            list.lines().map(str::to_string).collect(),
        ));
    }
    Index::new(entries)
}

/// The index of the package manager this system has, rebuilt when its
/// database changes (checked at most every half minute).
struct Cached {
    index: Arc<Index>,
    built_from: Option<SystemTime>,
    checked: Instant,
}

static CACHE: LazyLock<Mutex<Option<Cached>>> = LazyLock::new(|| Mutex::new(None));

/// pacman's or dpkg's database, and its last change.
fn database() -> Option<(&'static str, Option<SystemTime>)> {
    [PACMAN, DPKG]
        .into_iter()
        .find(|d| std::path::Path::new(d).is_dir())
        .map(|d| {
            let changed = std::fs::metadata(if d == DPKG {
                format!("{d}/status")
            } else {
                d.to_string()
            })
            .and_then(|m| m.modified())
            .ok();
            (d, changed)
        })
}

fn index() -> Option<Arc<Index>> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_mut()
        && c.checked.elapsed() < Duration::from_secs(30)
    {
        return Some(c.index.clone());
    }
    let (which, changed) = database()?;
    if let Some(c) = cache.as_mut()
        && c.built_from == changed
    {
        c.checked = Instant::now();
        return Some(c.index.clone());
    }
    let index = Arc::new(if which == PACMAN {
        pacman_index()
    } else {
        dpkg_index()
    });
    *cache = Some(Cached {
        index: index.clone(),
        built_from: changed,
        checked: Instant::now(),
    });
    Some(index)
}

/// rpm's answers, kept: asking costs a process each time.
static RPM: LazyLock<Mutex<HashMap<String, Option<Arc<Package>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn rpm_owner(path: &str) -> Option<Arc<Package>> {
    let mut cache = RPM.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(known) = cache.get(path) {
        return known.clone();
    }
    let answer = std::process::Command::new("rpm")
        .args([
            "-qf",
            "--queryformat",
            "%{NAME}\\t%{VERSION}-%{RELEASE}\\t%{SUMMARY}\\t%{URL}",
            path,
        ])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            let text = String::from_utf8(out.stdout).ok()?;
            let mut f = text.split('\t');
            let (name, version) = (f.next()?, f.next()?);
            let text = |s: Option<&str>| {
                s.filter(|s| !s.is_empty() && *s != "(none)")
                    .map(str::to_string)
            };
            Some(Arc::new(Package {
                manager: "rpm",
                name: name.into(),
                version: version.into(),
                description: text(f.next()),
                url: text(f.next()),
            }))
        });
    cache.insert(path.to_string(), answer.clone());
    answer
}

/// The package that put this file in place, from the system's package
/// manager; None for a file of no package (built or copied by hand).
pub fn owner(path: &str) -> Option<Arc<Package>> {
    if let Some(snap) = snap(path) {
        return Some(Arc::new(snap));
    }
    let Some(index) = index() else {
        return std::path::Path::new("/usr/bin/rpm")
            .exists()
            .then(|| rpm_owner(path))
            .flatten();
    };
    let found = index.owner(path).or_else(|| {
        // Lists name files where the package put them; the path asked
        // about may lead there through a link (/bin is /usr/bin).
        let real = std::fs::canonicalize(path).ok()?;
        index.owner(real.to_str()?)
    })?;
    Some(index.packages[found].clone())
}

/// What the UI shows for a file in place of Windows' version resource:
/// (description, company, version).
pub fn describe(path: &str) -> (Option<String>, Option<String>, Option<String>) {
    match owner(path) {
        Some(p) => (
            p.description.clone(),
            Some(p.label()),
            Some(p.version.clone()),
        ),
        None => (None, None, None),
    }
}

/// Whether the file still is what the package installed: pacman records
/// every file's SHA-256 (in the package's gzipped mtree). None when there
/// is no record to compare with (dpkg keeps MD5s only, rpm is not asked).
pub fn verify(path: &str, package: &Package, sha256: &str) -> Option<bool> {
    if package.manager != "pacman" {
        return None;
    }
    let index = index()?;
    let at = index
        .owner(path)
        .or_else(|| index.owner(std::fs::canonicalize(path).ok()?.to_str()?))?;
    let gz = std::fs::read(format!("{}/mtree", index.folders[at])).ok()?;
    let mtree = String::from_utf8(gunzip(&gz)?).ok()?;
    let real = std::fs::canonicalize(path).ok()?;
    let wanted = format!(".{}", real.to_str()?);
    let line = mtree
        .lines()
        .find(|l| l.split(' ').next() == Some(wanted.as_str()))?;
    let digest = line
        .split(' ')
        .find_map(|f| f.strip_prefix("sha256digest="))?;
    Some(digest.eq_ignore_ascii_case(sha256))
}

/// The body of a gzip file (RFC 1952): its header skipped, the deflate
/// stream inflated.
fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 18 || data[..3] != [0x1f, 0x8b, 8] {
        return None;
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let extra = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        at += 2 + extra;
    }
    for flag in [8u8, 16] {
        // A zero-terminated name, then comment.
        if flags & flag != 0 {
            at += data.get(at..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec_with_limit(data.get(at..)?, 64 << 20).ok()
}

/// A snap's package, from its path: /snap/<name>/<revision>/...
fn snap(path: &str) -> Option<Package> {
    let rest = path.strip_prefix("/snap/")?;
    let mut parts = rest.split('/');
    let (name, revision) = (parts.next()?, parts.next()?);
    let meta = std::fs::read_to_string(format!("/snap/{name}/{revision}/meta/snap.yaml"))
        .unwrap_or_default();
    let field = |key: &str| {
        meta.lines().find_map(|l| {
            let v = l.strip_prefix(key)?.strip_prefix(':')?.trim();
            Some(v.trim_matches(|c| c == '\'' || c == '"').to_string())
        })
    };
    Some(Package {
        manager: "Snap",
        name: name.into(),
        version: field("version").unwrap_or_else(|| revision.into()),
        description: field("summary"),
        url: field("website"),
    })
}

/// A process in a Flatpak sandbox: its app, from /.flatpak-info inside the
/// sandbox (which needs the right to look into the process).
pub fn flatpak(pid: u32) -> Option<Package> {
    let info = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")).ok()?;
    let field = |key: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(str::to_string))
    };
    let id = field("name")?;
    let branch = field("branch").unwrap_or_default();
    // The app's own description of its releases, newest first.
    let release = field("app-path").and_then(|files| {
        let mut text = String::new();
        ["metainfo", "appdata"].iter().find_map(|dir| {
            let mut f = std::fs::File::open(format!("{files}/share/{dir}/{id}.{dir}.xml")).ok()?;
            text.clear();
            f.read_to_string(&mut text).ok()?;
            let at = text.find("<release ")?;
            let tag = &text[at..at + text[at..].find('>')?];
            let v = tag.find("version=\"")? + 9;
            Some(tag[v..v + tag[v..].find('"')?].to_string())
        })
    });
    Some(Package {
        manager: "Flatpak",
        version: release.unwrap_or_else(|| branch.clone()),
        description: None,
        url: None,
        name: id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pacman_fields() {
        let desc = "%NAME%\ncoreutils\n\n%VERSION%\n9.12-2\n\n%DESC%\nThe basic utilities\n\n";
        assert_eq!(pacman_field(desc, "NAME"), Some("coreutils"));
        assert_eq!(pacman_field(desc, "VERSION"), Some("9.12-2"));
        assert_eq!(pacman_field(desc, "URL"), None);
    }

    #[test]
    fn finds_the_owner_of_a_system_program() {
        if database().is_none() {
            return;
        }
        let started = Instant::now();
        let ls = owner("/usr/bin/ls").expect("ls belongs to a package");
        eprintln!(
            "index of {} files built in {:?}",
            index().map_or(0, |i| i.hashes.len()),
            started.elapsed()
        );
        assert!(ls.name.contains("coreutils"), "{}", ls.name);
        assert!(!ls.version.is_empty());
        // Through the /bin link too.
        assert_eq!(
            owner("/bin/ls").map(|p| p.name.clone()),
            Some(ls.name.clone())
        );
        assert!(owner("/tmp/no-such-file").is_none());
        let started = Instant::now();
        for _ in 0..1000 {
            owner("/usr/bin/ls");
        }
        eprintln!("1000 lookups in {:?}", started.elapsed());
    }

    #[test]
    fn verifies_against_pacman() {
        if database().is_none_or(|(d, _)| d != PACMAN) {
            return;
        }
        let ls = owner("/usr/bin/ls").unwrap();
        let hash = crate::filefacts::sha256("/usr/bin/ls").unwrap();
        assert_eq!(verify("/usr/bin/ls", &ls, &hash), Some(true));
        assert_eq!(verify("/usr/bin/ls", &ls, &"0".repeat(64)), Some(false));
    }
}
