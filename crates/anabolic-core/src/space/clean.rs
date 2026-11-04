// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What can be cleaned up: places known to hold caches, leftovers and old
//! copies, found in a finished scan (no disk is read again) and said in
//! words: what each is, whether deleting it is safe, and when the system's
//! own tool should do it instead.

use std::collections::{HashMap, HashSet};

use super::{DIR, NONE, ROOT, Tree};

/// How safe it is to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Made again by itself when needed.
    Safe,
    /// Look before deleting: it may hold something wanted.
    Review,
    /// The system's own; its own tool cleans it.
    System,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Safe => "Safe to delete",
            Level::Review => "Look first",
            Level::System => "The system's own",
        }
    }
}

/// One kind of clean-up and where the scan found it.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub title: &'static str,
    pub about: &'static str,
    pub level: Level,
    /// What to do instead of deleting it from here.
    pub how: Option<&'static str>,
    /// The folders or files it covers.
    pub nodes: Vec<u32>,
    /// What is inside the folders goes, not the folders themselves.
    pub contents: bool,
    /// Emptied through the Recycle Bin's own command.
    pub bin: bool,
    pub size: u64,
}

impl Suggestion {
    /// Whether the page offers to delete it.
    pub fn deletable(&self) -> bool {
        self.level != Level::System && !self.nodes.is_empty() && self.title != DOWNLOADS
    }
}

const DOWNLOADS: &str = "Downloads";

/// Where a rule looks.
enum Place {
    /// A path from a known folder; `*` stands for any one name, `*.ext` for
    /// any name ending so.
    Path(&'static str),
    /// Every folder with a file of this name in it.
    Tagged(&'static str),
    /// Every folder of this name not inside another of the same name.
    Named(&'static str),
    /// A folder named `.1` inside a folder named `.0`, anywhere.
    Inside(&'static str, &'static str),
    /// In these folders, the folders of an older version of a program
    /// (`RustRover2026.1` beside `RustRover2026.2`).
    Older(&'static [&'static str]),
}

struct Rule {
    title: &'static str,
    about: &'static str,
    level: Level,
    how: Option<&'static str>,
    contents: bool,
    bin: bool,
    places: &'static [Place],
}

const fn rule(
    title: &'static str,
    level: Level,
    contents: bool,
    about: &'static str,
    places: &'static [Place],
) -> Rule {
    Rule {
        title,
        about,
        level,
        how: None,
        contents,
        bin: false,
        places,
    }
}

const fn system(
    title: &'static str,
    about: &'static str,
    how: &'static str,
    places: &'static [Place],
) -> Rule {
    Rule {
        title,
        about,
        level: Level::System,
        how: Some(how),
        contents: false,
        bin: false,
        places,
    }
}

const BUILD: Rule = rule(
    "Build output",
    Level::Safe,
    false,
    "Folders a build tool marked as its cache (a CACHEDIR.TAG file inside, as Rust's target \
     folders have): building again makes them again.",
    &[Place::Tagged("CACHEDIR.TAG")],
);
const NODE_MODULES: Rule = rule(
    "node_modules folders",
    Level::Review,
    false,
    "Packages npm, yarn or pnpm installed for a project (a package.json beside them); \
     installing again (npm install) brings them back. A project you are not working on needs \
     none.",
    &[Place::Named("node_modules")],
);

#[cfg(windows)]
const RULES: &[Rule] = &[
    rule(
        "Temporary files",
        Level::Safe,
        true,
        "Files programs put aside for later and left behind. What a running program still \
         holds open is skipped.",
        &[Place::Path(r"%LOCALAPPDATA%\Temp")],
    ),
    rule(
        "Browser caches",
        Level::Safe,
        true,
        "Copies of web pages and pictures the browser downloads again when needed. Close the \
         browser first: what it holds open is skipped.",
        &[
            Place::Path(r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache"),
            Place::Path(r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Code Cache"),
            Place::Path(r"%LOCALAPPDATA%\Google\Chrome\User Data\*\GPUCache"),
            Place::Path(r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Cache"),
            Place::Path(r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Code Cache"),
            Place::Path(r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\GPUCache"),
            Place::Path(r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data\*\Cache"),
            Place::Path(r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data\*\Code Cache"),
            Place::Path(r"%LOCALAPPDATA%\Vivaldi\User Data\*\Cache"),
            Place::Path(r"%LOCALAPPDATA%\Opera Software\Opera Stable\Cache"),
            Place::Path(r"%LOCALAPPDATA%\Opera Software\Opera GX Stable\Cache"),
            Place::Path(r"%LOCALAPPDATA%\Mozilla\Firefox\Profiles\*\cache2"),
        ],
    ),
    rule(
        "Shader caches",
        Level::Safe,
        true,
        "Shaders the graphics driver compiled and compiles again. A game may stutter for a \
         while afterwards, until it has them again.",
        &[
            Place::Path(r"%LOCALAPPDATA%\NVIDIA\DXCache"),
            Place::Path(r"%LOCALAPPDATA%\NVIDIA\GLCache"),
            Place::Path(r"%LOCALAPPDATA%\AMD\DxCache"),
            Place::Path(r"%LOCALAPPDATA%\AMD\DxcCache"),
            Place::Path(r"%LOCALAPPDATA%\AMD\GLCache"),
            Place::Path(r"%LOCALAPPDATA%\AMD\VkCache"),
            Place::Path(r"%LOCALAPPDATA%\D3DSCache"),
            Place::Path(r"%LOCALAPPDATA%\Intel\ShaderCache"),
            Place::Inside("steamapps", "shadercache"),
        ],
    ),
    rule(
        "Crash reports and dumps",
        Level::Safe,
        true,
        "Memory dumps and error reports of programs that crashed, kept for their makers.",
        &[
            Place::Path(r"%LOCALAPPDATA%\CrashDumps"),
            Place::Path(r"%PROGRAMDATA%\Microsoft\Windows\WER\ReportArchive"),
            Place::Path(r"%PROGRAMDATA%\Microsoft\Windows\WER\ReportQueue"),
        ],
    ),
    rule(
        "Package caches",
        Level::Safe,
        true,
        "Packages a build tool downloaded (npm, pip, Cargo, NuGet, Gradle, Maven, Go); it \
         downloads them again when a project needs them.",
        &[
            Place::Path(r"%LOCALAPPDATA%\npm-cache"),
            Place::Path(r"%LOCALAPPDATA%\pip\Cache"),
            Place::Path(r"%LOCALAPPDATA%\Yarn\Cache"),
            Place::Path(r"%LOCALAPPDATA%\go-build"),
            Place::Path(r"%USERPROFILE%\.cargo\registry\cache"),
            Place::Path(r"%USERPROFILE%\.nuget\packages"),
            Place::Path(r"%USERPROFILE%\.gradle\caches"),
            Place::Path(r"%USERPROFILE%\.m2\repository"),
        ],
    ),
    BUILD,
    NODE_MODULES,
    rule(
        "Older IDE versions",
        Level::Review,
        false,
        "Settings, caches and logs of a JetBrains IDE version older than one installed beside \
         it. The newer version took over its settings when it was first started.",
        &[Place::Older(&[
            r"%LOCALAPPDATA%\JetBrains",
            r"%APPDATA%\JetBrains",
        ])],
    ),
    Rule {
        title: "Recycle Bin",
        about: "What was deleted to the Recycle Bin. Emptying it deletes it for good.",
        level: Level::Review,
        how: None,
        contents: true,
        bin: true,
        places: &[Place::Path(r"%ROOT%\$Recycle.Bin")],
    },
    rule(
        DOWNLOADS,
        Level::Review,
        false,
        "What was downloaded: look through it; installers and archives already used are \
         usually not needed.",
        &[Place::Path(r"%USERPROFILE%\Downloads")],
    ),
    system(
        "Linux and Docker disks",
        "The disks of WSL's Linux systems and of Docker: they grow and never shrink by \
         themselves. Deleting one deletes the system or the containers inside.",
        "Run `wsl --shutdown`, then compact the file (Optimize-VHD in PowerShell, or diskpart's \
         compact vdisk), or prune what Docker no longer uses (docker system prune).",
        &[
            Place::Path(r"%LOCALAPPDATA%\Docker\wsl\*\*.vhdx"),
            Place::Path(r"%LOCALAPPDATA%\Packages\*\LocalState\*.vhdx"),
            Place::Path(r"%LOCALAPPDATA%\wsl\*\*.vhdx"),
        ],
    ),
    system(
        "Windows Update downloads",
        "Updates Windows downloaded, kept until it no longer needs them.",
        "Settings > System > Storage > Temporary files, or Disk Cleanup's \"Clean up system files\".",
        &[Place::Path(r"%SystemRoot%\SoftwareDistribution\Download")],
    ),
    system(
        "Previous Windows installation",
        "The Windows that was there before the last upgrade, kept to go back to it.",
        "Settings > System > Storage > Temporary files > Previous Windows installation(s).",
        &[Place::Path(r"%ROOT%\Windows.old")],
    ),
    system(
        "Windows' temporary files and dumps",
        "Windows' own leftovers and the memory dumps of its crashes.",
        "Settings > System > Storage > Temporary files, or Disk Cleanup.",
        &[
            Place::Path(r"%SystemRoot%\Temp"),
            Place::Path(r"%SystemRoot%\Minidump"),
            Place::Path(r"%SystemRoot%\MEMORY.DMP"),
            Place::Path(r"%SystemRoot%\LiveKernelReports"),
        ],
    ),
    system(
        "Hibernation file",
        "Where Windows keeps memory while hibernating, and for fast startup.",
        "`powercfg /h off` in an administrator terminal removes it, and with it hibernation and \
         fast startup.",
        &[Place::Path(r"%ROOT%\hiberfil.sys")],
    ),
];

#[cfg(target_os = "linux")]
const RULES: &[Rule] = &[
    rule(
        "Caches",
        Level::Safe,
        true,
        "What programs keep to start or load faster (~/.cache), made again when needed.",
        &[Place::Path("$HOME/.cache")],
    ),
    rule(
        "Package caches",
        Level::Safe,
        true,
        "Packages a build tool downloaded (Cargo, npm, Gradle, Maven, NuGet, Go); it downloads \
         them again when a project needs them.",
        &[
            Place::Path("$HOME/.cargo/registry/cache"),
            Place::Path("$HOME/.npm/_cacache"),
            Place::Path("$HOME/.gradle/caches"),
            Place::Path("$HOME/.m2/repository"),
            Place::Path("$HOME/.nuget/packages"),
            Place::Path("$HOME/go/pkg/mod/cache"),
        ],
    ),
    rule(
        "Shader caches",
        Level::Safe,
        true,
        "Shaders Steam's games compiled and compile again; a game may stutter for a while \
         afterwards.",
        &[Place::Inside("steamapps", "shadercache")],
    ),
    BUILD,
    NODE_MODULES,
    rule(
        "Older IDE versions",
        Level::Review,
        false,
        "Settings, caches and logs of a JetBrains IDE version older than one installed beside \
         it.",
        &[Place::Older(&[
            "$HOME/.cache/JetBrains",
            "$HOME/.config/JetBrains",
            "$HOME/.local/share/JetBrains",
        ])],
    ),
    Rule {
        title: "Trash",
        about: "What was moved to the trash. Emptying it deletes it for good.",
        level: Level::Review,
        how: None,
        contents: true,
        bin: true,
        places: &[Place::Path("$HOME/.local/share/Trash")],
    },
    rule(
        DOWNLOADS,
        Level::Review,
        false,
        "What was downloaded: look through it.",
        &[Place::Path("$HOME/Downloads")],
    ),
    system(
        "Downloaded system packages",
        "Packages the package manager downloaded and installed.",
        "`sudo apt clean`, `sudo dnf clean packages` or `sudo paccache -r`, as your system has.",
        &[
            Place::Path("/var/cache/apt/archives"),
            Place::Path("/var/cache/dnf"),
            Place::Path("/var/cache/pacman/pkg"),
        ],
    ),
    system(
        "System journal",
        "systemd's logs.",
        "`sudo journalctl --vacuum-size=200M` keeps the latest 200 MB.",
        &[Place::Path("/var/log/journal")],
    ),
    system(
        "Snaps and Flatpaks",
        "Older revisions of snaps, and Flatpak runtimes nothing uses any more.",
        "`sudo snap set system refresh.retain=2` keeps two revisions; `flatpak uninstall \
         --unused` removes unused runtimes.",
        &[
            Place::Path("/var/lib/snapd/snaps"),
            Place::Path("/var/lib/flatpak"),
            Place::Path("$HOME/.local/share/flatpak"),
        ],
    ),
    system(
        "Docker",
        "Docker's images, containers and volumes.",
        "`docker system prune` removes what no container uses.",
        &[Place::Path("/var/lib/docker")],
    ),
    system(
        "Crash dumps",
        "Core dumps of programs that crashed.",
        "`coredumpctl` lists them; systemd deletes old ones by itself.",
        &[Place::Path("/var/lib/systemd/coredump")],
    ),
];

const SEP: char = std::path::MAIN_SEPARATOR;

/// A path with its known folders filled in; None when one is not known.
fn expand(pattern: &str, root: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern;
    if cfg!(windows) {
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let end = after.find('%')?;
            let name = &after[..end];
            let value = if name == "ROOT" {
                // The drive that was scanned.
                root.get(..2)?.to_string()
            } else {
                std::env::var(name).ok()?
            };
            out.push_str(value.trim_end_matches(SEP));
            rest = &after[end + 1..];
        }
        out.push_str(rest);
    } else {
        let home = std::env::var("HOME").ok()?;
        out = pattern.replace("$HOME", home.trim_end_matches('/'));
    }
    Some(out)
}

fn same(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Whether a name fits one part of a pattern.
fn fits(part: &str, name: &str) -> bool {
    match part.strip_prefix('*') {
        Some("") => true,
        Some(end) => {
            name.len() >= end.len()
                && name.is_char_boundary(name.len() - end.len())
                && same(&name[name.len() - end.len()..], end)
        }
        None => same(part, name),
    }
}

/// The nodes a pattern names in the tree.
fn glob(tree: &Tree, pattern: &str) -> Vec<u32> {
    let root = tree.path.to_string_lossy();
    let root = root.trim_end_matches(SEP);
    let Some(pattern) = expand(pattern, root) else {
        return Vec::new();
    };
    // ASCII only, so the lengths stay and the parts can be cut from the
    // pattern as written.
    let lower = |s: &str| {
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        }
    };
    let (base, asked) = (lower(root), lower(&pattern));
    if asked == base {
        return vec![ROOT];
    }
    let Some(rest) = asked.strip_prefix(&format!("{base}{SEP}")) else {
        return Vec::new();
    };
    // The parts as written, not lowered: `*.vhdx` and names compare alike.
    let parts: Vec<&str> = pattern[pattern.len() - rest.len()..]
        .split(SEP)
        .filter(|p| !p.is_empty())
        .collect();
    let mut at = vec![ROOT];
    for part in parts {
        let mut next = Vec::new();
        for n in at {
            next.extend(tree.children(n).filter(|&c| fits(part, tree.name(c))));
        }
        at = next;
    }
    at
}

/// "RustRover2026.2" as ("RustRover", [2026, 2]).
fn product(name: &str) -> Option<(&str, Vec<u32>)> {
    let digit = name.find(|c: char| c.is_ascii_digit())?;
    let version: Option<Vec<u32>> = name[digit..].split('.').map(|p| p.parse().ok()).collect();
    (digit > 0).then_some((&name[..digit], version?))
}

/// The folders of older versions among the children of `folders`.
fn older(tree: &Tree, folders: &[&str]) -> Vec<u32> {
    let mut out = Vec::new();
    for folder in folders {
        for dir in glob(tree, folder) {
            let mut newest: HashMap<String, Vec<u32>> = HashMap::new();
            let kids: Vec<(u32, String, Vec<u32>)> = tree
                .children(dir)
                .filter(|&c| tree.is_dir(c))
                .filter_map(|c| {
                    let (name, version) = product(tree.name(c))?;
                    Some((c, name.to_lowercase(), version))
                })
                .collect();
            for (_, name, version) in &kids {
                let best = newest.entry(name.clone()).or_default();
                if *version > *best {
                    *best = version.clone();
                }
            }
            out.extend(
                kids.iter()
                    .filter(|(_, name, version)| newest.get(name).is_some_and(|b| version < b))
                    .map(|(c, _, _)| *c),
            );
        }
    }
    out
}

/// Folders found by a file's name, and by their own name (with their
/// parent's).
type Found = (
    HashMap<&'static str, Vec<u32>>,
    HashMap<(&'static str, &'static str), Vec<u32>>,
);

/// One pass over the tree for the rules that look anywhere.
fn anywhere(tree: &Tree) -> Found {
    let mut tagged: HashMap<&'static str, Vec<u32>> = HashMap::new();
    let mut named: HashMap<(&'static str, &'static str), Vec<u32>> = HashMap::new();
    let mut tags = Vec::new();
    let mut names = Vec::new();
    let mut insides = Vec::new();
    for r in RULES {
        for p in r.places {
            match p {
                Place::Tagged(t) => tags.push(*t),
                Place::Named(n) => names.push(*n),
                Place::Inside(a, b) => insides.push((*a, *b)),
                _ => {}
            }
        }
    }
    for i in 0..tree.len() as u32 {
        let dir = tree.flags(i) & DIR != 0;
        let name = tree.name(i);
        if !dir {
            for t in &tags {
                if same(name, t) {
                    tagged.entry(t).or_default().push(tree.parent(i));
                }
            }
            continue;
        }
        for n in &names {
            if same(name, n) {
                // Only the outermost: an inner one is inside it anyway.
                let mut up = tree.parent(i);
                let mut inner = false;
                while up != NONE {
                    if same(tree.name(up), n) {
                        inner = true;
                        break;
                    }
                    up = tree.parent(up);
                }
                // A project's: a package.json beside it. npm's global
                // folder has none, and holds installed tools, not a cache.
                let parent = tree.parent(i);
                let project = parent != NONE
                    && tree
                        .children(parent)
                        .any(|c| !tree.is_dir(c) && same(tree.name(c), "package.json"));
                if !inner && project {
                    named.entry((n, n)).or_default().push(i);
                }
            }
        }
        for (a, b) in &insides {
            let p = tree.parent(i);
            if same(name, b) && p != NONE && same(tree.name(p), a) {
                named.entry((a, b)).or_default().push(i);
            }
        }
    }
    (tagged, named)
}

/// What can be cleaned up in a tree, safest first and the largest first
/// within each level. A place found by two rules is counted by the first.
pub fn suggest(tree: &Tree) -> Vec<Suggestion> {
    let (tagged, named) = anywhere(tree);
    let mut taken: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    for r in RULES {
        let mut nodes = Vec::new();
        for p in r.places {
            match p {
                Place::Path(pattern) => nodes.extend(glob(tree, pattern)),
                Place::Tagged(t) => nodes.extend(tagged.get(t).into_iter().flatten()),
                Place::Named(n) => nodes.extend(named.get(&(*n, *n)).into_iter().flatten()),
                Place::Inside(a, b) => nodes.extend(named.get(&(*a, *b)).into_iter().flatten()),
                Place::Older(folders) => nodes.extend(older(tree, folders)),
            }
        }
        // Not twice, not inside what another rule took, nothing deleted,
        // and nothing the page would refuse to delete.
        nodes.sort_unstable();
        nodes.dedup();
        nodes.retain(|&n| {
            n != NONE
                && tree.size(n) > 0
                && !tree.is_gone(n)
                && !tree.ancestry(n).iter().any(|a| taken.contains(a))
                && (r.level == Level::System
                    || r.bin
                    || (n == ROOT && r.contents)
                    || super::protected(&tree.path_of(n)).is_none())
        });
        // Nor one inside another of the same rule.
        let mine: HashSet<u32> = nodes.iter().copied().collect();
        nodes.retain(|&n| {
            let chain = tree.ancestry(n);
            !chain[..chain.len() - 1].iter().any(|a| mine.contains(a))
        });
        if nodes.is_empty() {
            continue;
        }
        taken.extend(nodes.iter().copied());
        nodes.sort_unstable_by_key(|&n| std::cmp::Reverse(tree.size(n)));
        let size = nodes.iter().map(|&n| tree.size(n)).sum();
        out.push(Suggestion {
            title: r.title,
            about: r.about,
            level: r.level,
            how: r.how,
            nodes,
            contents: r.contents,
            bin: r.bin,
            size,
        });
    }
    out.retain(|s| s.size >= 1 << 20);
    out.sort_by(|a, b| a.level.cmp(&b.level).then(b.size.cmp(&a.size)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::{Progress, scan};

    #[test]
    fn versions_and_patterns() {
        assert_eq!(
            product("RustRover2026.2"),
            Some(("RustRover", vec![2026, 2]))
        );
        assert_eq!(
            product("IntelliJIdea2025.3"),
            Some(("IntelliJIdea", vec![2025, 3]))
        );
        assert_eq!(product("Daemon"), None);
        assert!(fits("*", "anything"));
        assert!(fits("*.vhdx", "ext4.vhdx"));
        assert!(!fits("*.vhdx", "ext4.vhd"));
        assert_eq!(fits("Cache", "cache"), cfg!(windows));
    }

    #[test]
    fn finds_build_output_node_modules_and_older_versions() {
        let dir = std::env::temp_dir().join(format!("anabolic-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mb = vec![0u8; 1 << 20];
        let write = |p: &str, data: &[u8]| {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, data).unwrap();
        };
        write(
            "proj/target/CACHEDIR.TAG",
            b"Signature: 8a477f597d28d172789f06886806bc55",
        );
        write("proj/target/debug/app.exe", &mb);
        write("proj/target/debug/deps/lib.rlib", &mb);
        write("web/node_modules/react/index.js", &mb);
        write("web/node_modules/a/node_modules/b/x.js", &mb);
        write("web/package.json", b"{}");
        write("web/src/app.js", b"x");
        write("npm/node_modules/tool/cli.js", &mb);
        let tree = scan(&dir, &Progress::default()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let found = suggest(&tree);
        let build = found.iter().find(|s| s.title == "Build output").unwrap();
        assert_eq!(build.nodes.len(), 1);
        assert_eq!(tree.name(build.nodes[0]), "target");
        assert!(build.size >= 2 << 20);
        let modules = found
            .iter()
            .find(|s| s.title == "node_modules folders")
            .unwrap();
        assert_eq!(
            modules.nodes.len(),
            1,
            "the inner one is inside the outer one"
        );
        assert_eq!(build.level, Level::Safe);
        assert!(
            found[0].level <= found.last().unwrap().level,
            "safest first"
        );
    }
}
