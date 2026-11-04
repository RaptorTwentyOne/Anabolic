// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Disk space: what fills a drive or a folder, as one tree of every file and
//! folder with their sizes, read once and then laid out as a treemap.
//!
//! The tree is read on worker threads and handed over whole: on Windows from
//! the NTFS master file table when Anabolic may read the volume (a drive of a
//! million files in seconds), otherwise by reading folders on several
//! threads at once. Children are kept largest first and side by side, so a
//! folder's contents are one slice and the treemap and the lists never sort.

#[cfg_attr(windows, path = "windows/space.rs")]
#[cfg_attr(target_os = "linux", path = "linux/space.rs")]
mod imp;
pub use imp::{scan, targets};

pub mod treemap;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

/// The node every tree starts from: the drive or folder that was scanned.
pub const ROOT: u32 = 0;
/// No node.
pub const NONE: u32 = u32::MAX;

// What a node is, beside its sizes.
/// A folder.
pub const DIR: u16 = 1;
pub const HIDDEN: u16 = 2;
pub const SYSTEM: u16 = 4;
/// A symbolic link, junction or other reparse point: not followed.
pub const LINK: u16 = 8;
/// A folder that could not be read; what is under it is missing.
pub const DENIED: u16 = 16;
/// Another name (hard link) of a file whose size is counted elsewhere.
pub const EXTRA_LINK: u16 = 32;
/// NTFS's own files: the master file table, the journal and the like.
pub const META: u16 = 64;
/// Another file system mounted here, which the scan does not enter.
pub const OTHER_FS: u16 = 128;
/// Files the master file table still lists whose folder is gone.
pub const ORPHANS: u16 = 256;
/// Deleted since the scan.
pub const REMOVED: u16 = 512;

/// How a tree was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// From the NTFS master file table, as WizTree does.
    Mft,
    /// Folder by folder.
    Walk,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Mft => "master file table",
            Method::Walk => "folder by folder",
        }
    }
}

/// A drive or mount point that can be scanned.
#[derive(Clone, Debug, Default)]
pub struct Target {
    /// What is scanned: "C:\" or "/home".
    pub path: PathBuf,
    /// The volume's label, or what it is ("Network", "/dev/nvme0n1p2").
    pub label: String,
    /// "NTFS", "ext4"...
    pub fs: String,
    pub total: u64,
    pub free: u64,
    /// Whether the fast reader works here (NTFS, and Anabolic may read the
    /// volume).
    pub fast: bool,
}

/// One file or folder.
#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    /// Bytes of data: a file's length, a folder's everything below it.
    pub size: u64,
    /// Bytes the drive gives it ("size on disk"); small files kept inside
    /// the master file table take none.
    pub alloc: u64,
    /// Last written, Unix seconds (0 = unknown).
    pub modified: i64,
    pub parent: u32,
    /// Children: `first .. first + count`, largest first.
    pub first: u32,
    pub count: u32,
    /// Files below a folder; 1 for a file.
    pub files: u32,
    name_at: u32,
    name_len: u16,
    pub flags: u16,
    /// Index into [`Tree::ext`]; 0 = none.
    pub ext: u32,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.flags & DIR != 0
    }

    pub fn children(&self) -> std::ops::Range<u32> {
        self.first..self.first + self.count
    }
}

/// Everything under the scanned folder.
pub struct Tree {
    pub nodes: Vec<Node>,
    names: String,
    exts: Vec<Box<str>>,
    /// What was scanned, as given.
    pub path: PathBuf,
    pub method: Method,
    /// Why the fast reader was not used, when it could have been.
    pub note: Option<String>,
    pub took: Duration,
    /// Folders that could not be read.
    pub unreadable: u32,
    /// The drive's size and free space.
    pub total: u64,
    pub free: u64,
}

impl Tree {
    pub fn name(&self, i: u32) -> &str {
        let n = &self.nodes[i as usize];
        &self.names[n.name_at as usize..n.name_at as usize + n.name_len as usize]
    }

    /// The extension, lower case, without the dot ("" for none).
    pub fn ext(&self, i: u32) -> &str {
        &self.exts[self.nodes[i as usize].ext as usize]
    }

    pub fn ext_name(&self, ext: u32) -> &str {
        &self.exts[ext as usize]
    }

    pub fn ext_count(&self) -> usize {
        self.exts.len()
    }

    /// The full path of a node.
    pub fn path_of(&self, i: u32) -> PathBuf {
        let mut parts = Vec::new();
        let mut at = i;
        while at != ROOT && at != NONE {
            parts.push(self.name(at));
            at = self.nodes[at as usize].parent;
        }
        let mut path = self.path.clone();
        for part in parts.iter().rev() {
            path.push(part);
        }
        path
    }

    /// The chain from the root down to `i`, both included.
    pub fn ancestry(&self, i: u32) -> Vec<u32> {
        let mut chain = vec![i];
        let mut at = i;
        while at != ROOT {
            at = self.nodes[at as usize].parent;
            if at == NONE {
                break;
            }
            chain.push(at);
        }
        chain.reverse();
        chain
    }

    /// Sizes per extension, largest first: (extension, size, alloc, files).
    pub fn by_extension(&self) -> Vec<ExtTotal> {
        let mut totals = vec![ExtTotal::default(); self.exts.len()];
        for (i, t) in totals.iter_mut().enumerate() {
            t.ext = i as u32;
        }
        for n in &self.nodes {
            if n.flags & (DIR | EXTRA_LINK | REMOVED) == 0 {
                let t = &mut totals[n.ext as usize];
                t.size += n.size;
                t.alloc += n.alloc;
                t.files += 1;
            }
        }
        totals.retain(|t| t.files > 0);
        totals.sort_unstable_by_key(|t| std::cmp::Reverse(t.size));
        totals
    }

    /// The largest files, largest first.
    pub fn largest_files(&self, limit: usize) -> Vec<u32> {
        let mut files: Vec<u32> = (0..self.nodes.len() as u32)
            .filter(|&i| self.nodes[i as usize].flags & (DIR | EXTRA_LINK | REMOVED) == 0)
            .collect();
        let key = |i: &u32| std::cmp::Reverse(self.nodes[*i as usize].size);
        if files.len() > limit {
            files.select_nth_unstable_by_key(limit, key);
            files.truncate(limit);
        }
        files.sort_unstable_by_key(key);
        files
    }

    /// Takes a node out after its file or folder was deleted: its size
    /// leaves every folder above it. The node stays where it is, empty,
    /// childless and flagged, so indexes held elsewhere stay valid; the
    /// treemap and the lists skip it.
    pub fn remove(&mut self, i: u32) {
        let gone = self.nodes[i as usize];
        let mut at = gone.parent;
        while at != NONE {
            let n = &mut self.nodes[at as usize];
            n.size = n.size.saturating_sub(gone.size);
            n.alloc = n.alloc.saturating_sub(gone.alloc);
            n.files = n.files.saturating_sub(gone.files);
            at = n.parent;
        }
        let n = &mut self.nodes[i as usize];
        n.size = 0;
        n.alloc = 0;
        n.files = 0;
        n.count = 0;
        n.flags |= REMOVED;
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtTotal {
    pub ext: u32,
    pub size: u64,
    pub alloc: u64,
    pub files: u64,
}

/// Where a scan is, read by the window while it runs.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub folders: AtomicU64,
    pub bytes: AtomicU64,
    /// Master file table bytes read, of `table_total`.
    pub table_read: AtomicU64,
    pub table_total: AtomicU64,
    phase: AtomicU8,
    cancel: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Reading,
    /// Putting the tree together and sorting it.
    Building,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            0 => Phase::Reading,
            _ => Phase::Building,
        }
    }

    pub(crate) fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    pub(crate) fn add(&self, files: u64, folders: u64, bytes: u64) {
        self.files.fetch_add(files, Ordering::Relaxed);
        self.folders.fetch_add(folders, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// The message a cancelled scan ends with.
pub const CANCELLED: &str = "Stopped.";

/// Collects nodes in any order with their parents, then makes the tree.
pub(crate) struct Builder {
    raw: Vec<Raw>,
    names: String,
    ext_ids: HashMap<Box<str>, u32>,
    exts: Vec<Box<str>>,
    links: HashSet<(u64, u64)>,
}

#[derive(Clone, Copy)]
struct Raw {
    size: u64,
    alloc: u64,
    modified: i64,
    parent: u32,
    name_at: u32,
    name_len: u16,
    flags: u16,
    ext: u32,
}

/// One entry as a folder listing gives it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Entry {
    pub size: u64,
    pub alloc: u64,
    pub modified: i64,
    pub flags: u16,
    /// A file with more than one name: (device, file id), counted once.
    pub link: Option<(u64, u64)>,
}

impl Builder {
    pub fn new(root_name: &str, root: Entry) -> Self {
        let mut b = Builder {
            raw: Vec::with_capacity(1 << 16),
            names: String::with_capacity(1 << 20),
            ext_ids: HashMap::new(),
            exts: vec!["".into()],
            links: HashSet::new(),
        };
        b.push(
            NONE,
            root_name,
            Entry {
                flags: root.flags | DIR,
                ..root
            },
        );
        b
    }

    /// Adds a node under `parent` (NONE while its parent is not known yet).
    pub fn push(&mut self, parent: u32, name: &str, mut e: Entry) -> u32 {
        if let Some(id) = e.link
            && !self.links.insert(id)
        {
            e.size = 0;
            e.alloc = 0;
            e.flags |= EXTRA_LINK;
        }
        let ext = if e.flags & DIR == 0 {
            self.ext_of(name)
        } else {
            0
        };
        let name = &name[..floor_char(name, u16::MAX as usize)];
        let name_at = self.names.len() as u32;
        self.names.push_str(name);
        self.raw.push(Raw {
            size: e.size,
            alloc: e.alloc,
            modified: e.modified,
            parent,
            name_at,
            name_len: name.len() as u16,
            flags: e.flags,
            ext,
        });
        (self.raw.len() - 1) as u32
    }

    // The master file table names a file's folder by its record, which may
    // come later.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn set_parent(&mut self, id: u32, parent: u32) {
        self.raw[id as usize].parent = parent;
    }

    pub fn add_flags(&mut self, id: u32, flags: u16) {
        self.raw[id as usize].flags |= flags;
    }

    fn ext_of(&mut self, name: &str) -> u32 {
        let Some(dot) = name.rfind('.') else {
            return 0;
        };
        let ext = &name[dot + 1..];
        if dot == 0 || ext.is_empty() || ext.len() > 16 {
            return 0;
        }
        let lower: Box<str> = if ext.bytes().any(|b| b.is_ascii_uppercase()) {
            ext.to_ascii_lowercase().into()
        } else {
            ext.into()
        };
        if let Some(&id) = self.ext_ids.get(&lower) {
            return id;
        }
        let id = self.exts.len() as u32;
        self.exts.push(lower.clone());
        self.ext_ids.insert(lower, id);
        id
    }

    /// Sums every folder, sorts each folder's children largest first and
    /// lays the nodes out so siblings sit side by side. Nodes not under the
    /// root (a broken chain of parents) are left out.
    pub fn finish(self, progress: &Progress) -> (Vec<Node>, String, Vec<Box<str>>) {
        progress.set_phase(Phase::Building);
        let raw = self.raw;
        let n = raw.len();
        // Children of each node, unsorted: kids[start[p]..start[p + 1]].
        let mut start = vec![0u32; n + 1];
        for r in &raw[1..] {
            if (r.parent as usize) < n {
                start[r.parent as usize + 1] += 1;
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut kids = vec![0u32; start[n] as usize];
        for (i, r) in raw.iter().enumerate().skip(1) {
            if (r.parent as usize) < n {
                let at = &mut fill[r.parent as usize];
                kids[*at as usize] = i as u32;
                *at += 1;
            }
        }
        drop(fill);

        // Top-down order of what the root reaches, then sums bottom-up.
        let order = |kids: &[u32]| {
            let mut order = Vec::with_capacity(n);
            order.push(ROOT);
            let mut at = 0;
            while at < order.len() {
                let u = order[at] as usize;
                order.extend_from_slice(&kids[start[u] as usize..start[u + 1] as usize]);
                at += 1;
            }
            order
        };
        let top_down = order(&kids);
        let mut size: Vec<u64> = raw.iter().map(|r| r.size).collect();
        let mut alloc: Vec<u64> = raw.iter().map(|r| r.alloc).collect();
        let mut files: Vec<u32> = raw.iter().map(|r| u32::from(r.flags & DIR == 0)).collect();
        for &u in top_down.iter().rev() {
            let p = raw[u as usize].parent;
            if u != ROOT && (p as usize) < n {
                size[p as usize] += size[u as usize];
                alloc[p as usize] += alloc[u as usize];
                files[p as usize] += files[u as usize];
            }
        }
        drop(top_down);

        // Largest first within each folder.
        for p in 0..n {
            let s = &mut kids[start[p] as usize..start[p + 1] as usize];
            if s.len() > 1 {
                s.sort_unstable_by(|a, b| size[*b as usize].cmp(&size[*a as usize]));
            }
        }

        // The final order: breadth first over sorted children, which puts
        // each folder's children next to each other.
        let order = order(&kids);
        let mut new_id = vec![NONE; n];
        for (pos, &u) in order.iter().enumerate() {
            new_id[u as usize] = pos as u32;
        }
        let mut nodes = Vec::with_capacity(order.len());
        for &u in &order {
            let r = raw[u as usize];
            let u = u as usize;
            let count = start[u + 1] - start[u];
            let first = if count > 0 {
                new_id[kids[start[u] as usize] as usize]
            } else {
                0
            };
            nodes.push(Node {
                size: size[u],
                alloc: alloc[u],
                modified: r.modified,
                parent: if u == ROOT as usize {
                    NONE
                } else {
                    new_id[r.parent as usize]
                },
                first,
                count,
                files: files[u],
                name_at: r.name_at,
                name_len: r.name_len,
                flags: r.flags,
                ext: r.ext,
            });
        }
        (nodes, self.names, self.exts)
    }
}

/// The largest index at most `max` that falls on a character boundary.
fn floor_char(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut at = max;
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

impl Tree {
    pub(crate) fn assemble(
        (nodes, names, exts): (Vec<Node>, String, Vec<Box<str>>),
        path: &Path,
        method: Method,
    ) -> Tree {
        Tree {
            nodes,
            names,
            exts,
            path: path.to_path_buf(),
            method,
            note: None,
            took: Duration::ZERO,
            unreadable: 0,
            total: 0,
            free: 0,
        }
    }
}

/// What one folder listing gave: entries with their names in one string.
#[derive(Default)]
pub(crate) struct Listing {
    pub entries: Vec<(u32, u32, Entry)>,
    pub names: String,
}

impl Listing {
    pub fn push(&mut self, name: &str, e: Entry) {
        let at = self.names.len() as u32;
        self.names.push_str(name);
        self.entries.push((at, name.len() as u32, e));
    }
}

/// Reads a folder tree on `threads` threads with `list`, which lists one
/// folder; entries flagged DIR (and not LINK or OTHER_FS) are entered.
pub(crate) fn walk<F>(
    root: &Path,
    root_name: &str,
    root_entry: Entry,
    threads: usize,
    progress: &Progress,
    list: F,
) -> Result<(Builder, u32), String>
where
    F: Fn(&Path) -> std::io::Result<Listing> + Sync,
{
    use crossbeam_channel::unbounded;

    let mut b = Builder::new(root_name, root_entry);
    let (task_tx, task_rx) = unbounded::<(u32, PathBuf)>();
    let (done_tx, done_rx) = unbounded::<(u32, PathBuf, std::io::Result<Listing>)>();
    let mut unreadable = 0u32;
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            let task_rx = task_rx.clone();
            let done_tx = done_tx.clone();
            let list = &list;
            s.spawn(move || {
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::BelowNormal,
                );
                while let Ok((id, path)) = task_rx.recv() {
                    let listed = if progress.cancelled() {
                        Ok(Listing::default())
                    } else {
                        list(&path)
                    };
                    if done_tx.send((id, path, listed)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        drop(task_rx);
        let _ = task_tx.send((ROOT, root.to_path_buf()));
        let mut pending = 1usize;
        while pending > 0 {
            let Ok((dir, path, listed)) = done_rx.recv() else {
                break;
            };
            pending -= 1;
            let listing = match listed {
                Ok(l) => l,
                Err(_) => {
                    unreadable += 1;
                    b.add_flags(dir, DENIED);
                    continue;
                }
            };
            let (mut files, mut folders, mut bytes) = (0, 0, 0);
            for &(at, len, e) in &listing.entries {
                let name = &listing.names[at as usize..(at + len) as usize];
                let id = b.push(dir, name, e);
                if e.flags & DIR != 0 {
                    folders += 1;
                    if e.flags & (LINK | OTHER_FS) == 0 && !progress.cancelled() {
                        pending += 1;
                        let _ = task_tx.send((id, path.join(name)));
                    }
                } else {
                    files += 1;
                    bytes += e.size;
                }
            }
            progress.add(files, folders, bytes);
        }
        drop(task_tx);
    });
    if progress.cancelled() {
        return Err(CANCELLED.into());
    }
    Ok((b, unreadable))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: u64) -> Entry {
        Entry {
            size,
            alloc: size.div_ceil(4096) * 4096,
            ..Entry::default()
        }
    }

    fn dir() -> Entry {
        Entry {
            flags: DIR,
            ..Entry::default()
        }
    }

    /// root/{a/{x 10, y 30}, b 25, c/{z 1}}, pushed children before parents.
    fn sample() -> Tree {
        let mut b = Builder::new("root", dir());
        let x = b.push(NONE, "x.TXT", file(10));
        let y = b.push(NONE, "y.txt", file(30));
        let z = b.push(NONE, "z.bin", file(1));
        let a = b.push(ROOT, "a", dir());
        let c = b.push(ROOT, "c", dir());
        b.push(ROOT, "b.bin", file(25));
        for (kid, parent) in [(x, a), (y, a), (z, c)] {
            b.set_parent(kid, parent);
        }
        // A node whose parent chain never reaches the root is left out.
        let lost = b.push(NONE, "lost", dir());
        b.push(lost, "inside", file(99));
        Tree::assemble(b.finish(&Progress::default()), Path::new("R"), Method::Walk)
    }

    #[test]
    fn sums_sorts_and_keeps_siblings_together() {
        let t = sample();
        assert_eq!(t.nodes.len(), 7, "the lost folder and its file are dropped");
        let root = t.nodes[ROOT as usize];
        assert_eq!(root.size, 66);
        assert_eq!(root.files, 4);
        let kids: Vec<&str> = root.children().map(|i| t.name(i)).collect();
        assert_eq!(kids, ["a", "b.bin", "c"], "largest first");
        let a = root.first;
        let inside: Vec<&str> = t.nodes[a as usize].children().map(|i| t.name(i)).collect();
        assert_eq!(inside, ["y.txt", "x.TXT"]);
        for i in 0..t.nodes.len() as u32 {
            for c in t.nodes[i as usize].children() {
                assert_eq!(t.nodes[c as usize].parent, i);
            }
        }
        assert_eq!(t.path_of(t.nodes[a as usize].first), Path::new("R/a/y.txt"));
        assert_eq!(
            t.ext(t.nodes[a as usize].first + 1),
            "txt",
            "extensions are lower case"
        );
    }

    #[test]
    fn counts_a_file_with_two_names_once() {
        let mut b = Builder::new("root", dir());
        let linked = Entry {
            link: Some((1, 42)),
            ..file(100)
        };
        b.push(ROOT, "one", linked);
        b.push(ROOT, "two", linked);
        let t = Tree::assemble(b.finish(&Progress::default()), Path::new("R"), Method::Walk);
        assert_eq!(t.nodes[ROOT as usize].size, 100);
        assert_eq!(t.nodes[2].flags & EXTRA_LINK, EXTRA_LINK);
    }

    #[test]
    fn removing_takes_the_size_out_of_every_folder_above() {
        let mut t = sample();
        let a = t.nodes[ROOT as usize].first;
        let y = t.nodes[a as usize].first;
        t.remove(y);
        assert_eq!(t.nodes[ROOT as usize].size, 36);
        assert_eq!(t.nodes[ROOT as usize].files, 3);
        assert_eq!(t.nodes[a as usize].size, 10);
        assert_eq!(t.nodes[y as usize].flags & REMOVED, REMOVED);
        assert_eq!(t.name(y), "y.txt", "indexes stay valid");
    }

    #[test]
    fn largest_files_and_extensions() {
        let t = sample();
        let names: Vec<&str> = t.largest_files(2).iter().map(|&i| t.name(i)).collect();
        assert_eq!(names, ["y.txt", "b.bin"]);
        let ext = t.by_extension();
        assert_eq!(t.ext_name(ext[0].ext), "txt");
        assert_eq!(ext[0].size, 40);
        assert_eq!(ext[1].files, 2);
    }

    #[test]
    fn walks_a_real_folder() {
        let dir = std::env::temp_dir().join(format!("anabolic-space-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        std::fs::write(dir.join("a.txt"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("sub/b.bin"), vec![0u8; 5000]).unwrap();
        std::fs::write(dir.join("sub/deeper/c.bin"), vec![0u8; 300]).unwrap();
        let progress = Progress::default();
        let t = scan(&dir, &progress).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(t.nodes[ROOT as usize].size, 6300);
        assert_eq!(t.nodes[ROOT as usize].files, 3);
        let first = t.nodes[ROOT as usize].first;
        assert_eq!(t.name(first), "sub");
        assert_eq!(t.nodes[first as usize].size, 5300);
    }
}
