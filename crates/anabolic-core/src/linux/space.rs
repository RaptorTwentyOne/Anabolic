// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Disk space on Linux: the mounted file systems to offer, and a scan that
//! reads folders on several threads. It stays on the file system it starts
//! on (another one mounted inside is shown, not entered), and a file with
//! several hard links is counted once.

use std::collections::HashSet;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::space::{
    CANCELLED, DIR, Entry, HIDDEN, LINK, Listing, Method, OTHER_FS, Progress, Target, Tree, walk,
};

/// File systems that are packages or images, not places to clean up.
const SKIP: &[&str] = &["squashfs", "iso9660", "udf", "erofs"];

/// "\040" in mountinfo is a space.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (size, free) of the file system holding `path`.
fn space(path: &Path) -> Option<(u64, u64)> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let unit = st.f_frsize as u64;
    Some((st.f_blocks as u64 * unit, st.f_bavail as u64 * unit))
}

/// The home folder, then each file system on a block device at its first
/// mount point.
pub fn targets() -> Vec<Target> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && home.is_dir()
    {
        let (total, free) = space(&home).unwrap_or((0, 0));
        out.push(Target {
            path: home,
            label: "Home".into(),
            fs: String::new(),
            total,
            free,
            fast: false,
        });
    }
    let table = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut seen = HashSet::new();
    for line in table.lines() {
        // id parent major:minor root mount-point options ... - type source super
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        let (Some(number), Some(point)) = (fields.get(2), fields.get(4)) else {
            continue;
        };
        let fs = fields.get(dash + 1).copied().unwrap_or("");
        if SKIP.contains(&fs) || !seen.insert(number.to_string()) {
            continue;
        }
        let Some(device) = std::fs::canonicalize(format!("/sys/dev/block/{number}"))
            .ok()
            .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
        else {
            continue;
        };
        let point = PathBuf::from(unescape(point));
        let (total, free) = space(&point).unwrap_or((0, 0));
        out.push(Target {
            path: point,
            label: device,
            fs: fs.to_string(),
            total,
            free,
            fast: false,
        });
    }
    out
}

/// One folder's entries; a folder on another device than `root_dev` is
/// marked, not entered.
fn list(root_dev: u64) -> impl Fn(&Path) -> std::io::Result<Listing> + Sync {
    move |path: &Path| {
        let mut out = Listing::default();
        for entry in std::fs::read_dir(path)? {
            let Ok(entry) = entry else {
                continue;
            };
            // Does not follow a symbolic link.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let kind = meta.file_type();
            let mut flags = 0;
            if kind.is_dir() {
                flags |= DIR;
                if meta.dev() != root_dev {
                    flags |= OTHER_FS;
                }
            }
            if kind.is_symlink() {
                flags |= LINK;
            }
            if name.starts_with('.') {
                flags |= HIDDEN;
            }
            let file = !kind.is_dir();
            out.push(
                &name,
                Entry {
                    size: if file { meta.len() } else { 0 },
                    alloc: if file { meta.blocks() * 512 } else { 0 },
                    modified: meta.mtime(),
                    flags,
                    link: (file && meta.nlink() > 1).then(|| (meta.dev(), meta.ino())),
                },
            );
        }
        Ok(out)
    }
}

/// Reads everything under `path`.
pub fn scan(path: &Path, progress: &Progress) -> Result<Tree, String> {
    let started = Instant::now();
    let meta =
        std::fs::metadata(path).map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder.", path.display()));
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let root = Entry {
        flags: DIR,
        modified: meta.mtime(),
        ..Entry::default()
    };
    let name = path.to_string_lossy().into_owned();
    let (builder, unreadable) = walk(path, &name, root, threads, progress, list(meta.dev()))?;
    if progress.cancelled() {
        return Err(CANCELLED.into());
    }
    let mut tree = Tree::assemble(builder.finish(progress), path, Method::Walk);
    tree.unreadable = unreadable;
    (tree.total, tree.free) = space(path).unwrap_or((0, 0));
    tree.took = started.elapsed();
    Ok(tree)
}
