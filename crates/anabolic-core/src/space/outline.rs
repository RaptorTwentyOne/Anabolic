// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A scan's outline: its folders of a megabyte or more and their sizes,
//! kept in a small file to compare a later scan with (what grew, what
//! shrank, what is new, what is gone).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use super::{DIR, ROOT, Tree};

/// Folders smaller than this are left out of an outline.
const MIN: u64 = 1 << 20;
/// At most this many folders: the threshold rises until it fits.
const MOST: usize = 60_000;
/// How deep below the scanned folder.
const DEEPEST: usize = 10;
/// Outlines kept per scanned folder.
const KEEP: usize = 40;
const MAGIC: &str = "anabolic-outline 1";

/// A scan's folders and their sizes.
#[derive(Clone, Debug, Default)]
pub struct Outline {
    /// What was scanned.
    pub path: String,
    /// When, Unix seconds.
    pub at: i64,
    pub size: u64,
    pub files: u64,
    /// (path below the scanned folder, `/` between names; size; files)
    pub folders: Vec<(String, u64, u64)>,
}

/// The outline of a tree.
pub fn outline(tree: &Tree, at: i64) -> Outline {
    let root = tree.node(ROOT);
    let mut min = MIN.max(root.size / 1_000_000);
    loop {
        let mut folders = Vec::new();
        let mut stack: Vec<(u32, String, usize)> = vec![(ROOT, String::new(), 0)];
        while let Some((n, path, depth)) = stack.pop() {
            if depth > 0 {
                let node = tree.node(n);
                folders.push((path.clone(), node.size, u64::from(node.files)));
            }
            if depth >= DEEPEST || folders.len() > MOST {
                continue;
            }
            for c in tree.children(n) {
                if tree.flags(c) & DIR != 0 && tree.size(c) >= min && !tree.is_gone(c) {
                    let child = if path.is_empty() {
                        tree.name(c).to_string()
                    } else {
                        format!("{path}/{}", tree.name(c))
                    };
                    stack.push((c, child, depth + 1));
                }
            }
        }
        if folders.len() <= MOST {
            return Outline {
                path: tree.path.to_string_lossy().into_owned(),
                at,
                size: root.size,
                files: u64::from(root.files),
                folders,
            };
        }
        min *= 4;
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The folder a scanned path's outlines are kept in.
fn folder_for(dir: &Path, path: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let key = if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    };
    // A fixed hash: the name must stay the same from one run to the next.
    let mut h = FixedHasher(0xcbf2_9ce4_8422_2325);
    key.trim_end_matches(['\\', '/']).hash(&mut h);
    dir.join(format!("{:016x}", h.finish()))
}

/// FNV-1a: a hash that is the same in every run.
struct FixedHasher(u64);

impl std::hash::Hasher for FixedHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Keeps an outline under `dir`, and lets go of the oldest beyond the
/// number kept.
pub fn save(dir: &Path, o: &Outline) -> Result<PathBuf, String> {
    let folder = folder_for(dir, &o.path);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let file = folder.join(format!("{}.tsv", o.at));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&file).map_err(|e| e.to_string())?);
    let mut write = || -> std::io::Result<()> {
        writeln!(
            out,
            "{MAGIC}\t{}\t{}\t{}\t{}",
            escape(&o.path),
            o.at,
            o.size,
            o.files
        )?;
        for (path, size, files) in &o.folders {
            writeln!(out, "{size}\t{files}\t{}", escape(path))?;
        }
        out.flush()
    };
    write().map_err(|e| e.to_string())?;
    let mut kept = list(dir, Path::new(&o.path));
    if kept.len() > KEEP {
        for (_, old, _) in kept.drain(KEEP..) {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(file)
}

/// The outlines kept of `path`, newest first: (when, file, size).
pub fn list(dir: &Path, path: &Path) -> Vec<(i64, PathBuf, u64)> {
    let folder = folder_for(dir, &path.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut out: Vec<(i64, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let at: i64 = p.file_stem()?.to_str()?.parse().ok()?;
            // The size from the first line, without reading the rest.
            let file = std::fs::File::open(&p).ok()?;
            let mut first = String::new();
            std::io::BufReader::new(file).read_line(&mut first).ok()?;
            let size = first.trim_end().split('\t').nth(3)?.parse().ok()?;
            Some((at, p, size))
        })
        .collect();
    out.sort_unstable_by_key(|(at, _, _)| std::cmp::Reverse(*at));
    out
}

pub fn load(file: &Path) -> Option<Outline> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next()?.split('\t').collect();
    if head.first() != Some(&MAGIC) || head.len() < 5 {
        return None;
    }
    let folders = lines
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let size = parts.next()?.parse().ok()?;
            let files = parts.next()?.parse().ok()?;
            Some((unescape(parts.next()?), size, files))
        })
        .collect();
    Some(Outline {
        path: unescape(head[1]),
        at: head[2].parse().ok()?,
        size: head[3].parse().ok()?,
        files: head[4].parse().ok()?,
        folders,
    })
}

/// One folder then and now, in a tree of the differences.
#[derive(Clone, Debug)]
pub struct Diff {
    pub name: String,
    /// None: under a megabyte, or not there.
    pub then: Option<u64>,
    pub now: Option<u64>,
    pub parent: u32,
    /// Children, the largest change first.
    pub children: Vec<u32>,
}

impl Diff {
    pub fn change(&self) -> i64 {
        self.now.unwrap_or(0) as i64 - self.then.unwrap_or(0) as i64
    }
}

/// The differences between two outlines of one folder, as a tree; the
/// first entry is the scanned folder itself.
pub fn compare(then: &Outline, now: &Outline) -> Vec<Diff> {
    let mut out = vec![Diff {
        name: now.path.clone(),
        then: Some(then.size),
        now: Some(now.size),
        parent: u32::MAX,
        children: Vec::new(),
    }];
    let mut at: HashMap<String, u32> = HashMap::new();
    let mut place = |out: &mut Vec<Diff>, path: &str| -> u32 {
        if let Some(&i) = at.get(path) {
            return i;
        }
        // Parents come first: an outline lists a folder before what is in
        // it, and a missing one is made on the way.
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (Some(p.to_string()), n.to_string()),
            None => (None, path.to_string()),
        };
        let parent = parent.map_or(0, |p| {
            if let Some(&i) = at.get(&p) {
                i
            } else {
                // Rare: a parent under the threshold of one outline only.
                let i = out.len() as u32;
                out.push(Diff {
                    name: p.rsplit('/').next().unwrap_or(&p).to_string(),
                    then: None,
                    now: None,
                    parent: 0,
                    children: Vec::new(),
                });
                at.insert(p, i);
                i
            }
        });
        let i = out.len() as u32;
        out.push(Diff {
            name,
            then: None,
            now: None,
            parent,
            children: Vec::new(),
        });
        out[parent as usize].children.push(i);
        at.insert(path.to_string(), i);
        i
    };
    let mut both: Vec<&(String, u64, u64)> = then.folders.iter().chain(&now.folders).collect();
    // Shorter paths first, so parents are placed before their children.
    both.sort_by_key(|(p, _, _)| p.matches('/').count());
    for (path, _, _) in &both {
        place(&mut out, path);
    }
    for (path, size, _) in &then.folders {
        let i = at[path];
        out[i as usize].then = Some(*size);
    }
    for (path, size, _) in &now.folders {
        let i = at[path];
        out[i as usize].now = Some(*size);
    }
    let changes: Vec<i64> = out.iter().map(Diff::change).collect();
    for d in &mut out {
        d.children
            .sort_unstable_by_key(|&c| std::cmp::Reverse(changes[c as usize].unsigned_abs()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(size: u64, folders: &[(&str, u64)]) -> Outline {
        Outline {
            path: "C:\\Data".into(),
            at: 1,
            size,
            files: 0,
            folders: folders
                .iter()
                .map(|(p, s)| (p.to_string(), *s, 1))
                .collect(),
        }
    }

    #[test]
    fn compares_and_keeps_round_trips() {
        let then = o(
            10 << 30,
            &[
                ("Games", 8 << 30),
                ("Games/Old", 3 << 30),
                ("Temp", 2 << 30),
            ],
        );
        let now = o(
            14 << 30,
            &[
                ("Games", 13 << 30),
                ("Games/New", 6 << 30),
                ("Music", 1 << 30),
            ],
        );
        let diff = compare(&then, &now);
        assert_eq!(diff[0].change(), 4 << 30);
        let first = diff[0].children[0];
        assert_eq!(
            diff[first as usize].name, "Games",
            "the largest change first"
        );
        assert_eq!(diff[first as usize].change(), 5 << 30);
        let games = &diff[first as usize];
        let names: Vec<&str> = games
            .children
            .iter()
            .map(|&c| diff[c as usize].name.as_str())
            .collect();
        assert_eq!(names, ["New", "Old"]);
        let temp = diff.iter().find(|d| d.name == "Temp").unwrap();
        assert_eq!(
            (temp.then, temp.now),
            (Some(2 << 30), None),
            "gone, or under a megabyte"
        );

        let dir = std::env::temp_dir().join(format!("anabolic-outline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut tabbed = then.clone();
        tabbed.folders.push(("a\tb\\c".into(), 5, 1));
        let file = save(&dir, &tabbed).unwrap();
        let back = load(&file).unwrap();
        assert_eq!(back.folders, tabbed.folders);
        assert_eq!(back.size, tabbed.size);
        let kept = list(&dir, Path::new("c:\\data\\"));
        assert_eq!(
            kept.len(),
            usize::from(cfg!(windows)),
            "the same folder, written otherwise"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
