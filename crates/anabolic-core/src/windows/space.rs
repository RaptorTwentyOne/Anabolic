// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Disk space on Windows: the drives to offer, and a scan that reads the
//! NTFS master file table when it may and folders one by one otherwise
//! (FAT and exFAT drives, network shares, or Anabolic without
//! administrator rights).

use std::mem::offset_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use windows::Win32::Foundation::ERROR_NO_MORE_FILES;
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetDiskFreeSpaceExW, GetDriveTypeW,
    GetFileInformationByHandle, GetFileInformationByHandleEx, GetLogicalDrives,
    GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, OPEN_EXISTING,
};
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE,
};
use windows::core::PCWSTR;

use crate::space::{
    Builder, CANCELLED, DIR, Entry, HIDDEN, LINK, Listing, Method, Progress, SYSTEM, Target, Tree,
    walk,
};
use crate::util::{OwnedHandle, from_wide, wide};

#[path = "mft.rs"]
mod mft;

const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE: u32 = 0x400;

/// Keeps a card reader without a card from asking for one.
struct QuietErrors(THREAD_ERROR_MODE);

impl QuietErrors {
    fn new() -> QuietErrors {
        let mut old = THREAD_ERROR_MODE(0);
        let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old)) };
        QuietErrors(old)
    }
}

impl Drop for QuietErrors {
    fn drop(&mut self) {
        let _ = unsafe { SetThreadErrorMode(self.0, None) };
    }
}

/// The drives with a letter. Asks each drive, which can take seconds for a
/// network share that is gone: call it off the window's thread.
pub fn targets() -> Vec<Target> {
    let _quiet = QuietErrors::new();
    let elevated = crate::hardening::is_elevated();
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", char::from(b'A' + i as u8));
        let root_w = wide(&root);
        let kind = unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) };
        if kind == DRIVE_CDROM {
            continue;
        }
        let Some((label, fs)) = volume_info(&root_w) else {
            continue;
        };
        let (mut total, mut free) = (0u64, 0u64);
        let _ = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(root_w.as_ptr()),
                None,
                Some(&mut total),
                Some(&mut free),
            )
        };
        let label = if label.is_empty() && kind == DRIVE_REMOTE {
            "Network".into()
        } else {
            label
        };
        out.push(Target {
            path: PathBuf::from(&root),
            label,
            fast: fs == "NTFS" && elevated && kind != DRIVE_REMOTE,
            fs,
            total,
            free,
        });
    }
    out
}

/// (label, file system) of a volume root.
fn volume_info(root: &[u16]) -> Option<(String, String)> {
    let mut label = [0u16; 261];
    let mut fs = [0u16; 64];
    unsafe {
        GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            Some(&mut label),
            None,
            None,
            None,
            Some(&mut fs),
        )
    }
    .ok()?;
    Some((from_wide(&label), from_wide(&fs)))
}

/// The volume a path is on: its root ("C:\" or a mount folder) and its
/// device name for opening it whole (`\\?\Volume{...}`).
fn volume_of(path: &Path) -> Option<(PathBuf, String)> {
    let path_w = wide(&path.to_string_lossy());
    let mut root = [0u16; 1024];
    unsafe { GetVolumePathNameW(PCWSTR(path_w.as_ptr()), &mut root) }.ok()?;
    let mut name = [0u16; 64];
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut name) }.ok()?;
    let device = from_wide(&name);
    Some((
        PathBuf::from(from_wide(&root)),
        device.trim_end_matches('\\').to_string(),
    ))
}

/// The master file table record of a folder.
fn record_of(path: &Path) -> Option<u64> {
    let h = open_dir(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(h.raw(), &mut info) }.ok()?;
    Some(
        ((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)) & 0xFFFF_FFFF_FFFF,
    )
}

/// A path Windows takes at any length.
fn long(path: &Path) -> Vec<u16> {
    let text = path.as_os_str();
    let mut out: Vec<u16> = Vec::with_capacity(text.len() + 8);
    let s = text.to_string_lossy();
    if !s.starts_with(r"\\?\") {
        if let Some(unc) = s.strip_prefix(r"\\") {
            out.extend(r"\\?\UNC\".encode_utf16());
            out.extend(unc.encode_utf16());
            out.push(0);
            return out;
        }
        out.extend(r"\\?\".encode_utf16());
    }
    out.extend(text.encode_wide());
    out.push(0);
    out
}

fn open_dir(path: &Path) -> std::io::Result<OwnedHandle> {
    let path = long(path);
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map(OwnedHandle)
    .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

fn filetime_to_unix(ft: i64) -> i64 {
    if ft <= 0 {
        0
    } else {
        (ft - 116_444_736_000_000_000) / 10_000_000
    }
}

fn flags_of(attrs: u32) -> u16 {
    let mut flags = 0;
    if attrs & ATTR_DIRECTORY != 0 {
        flags |= DIR;
    }
    if attrs & ATTR_HIDDEN != 0 {
        flags |= HIDDEN;
    }
    if attrs & ATTR_SYSTEM != 0 {
        flags |= SYSTEM;
    }
    if attrs & ATTR_REPARSE != 0 {
        flags |= LINK;
    }
    flags
}

/// One folder's entries, 64 KB of them per call.
fn list(path: &Path) -> std::io::Result<Listing> {
    let h = open_dir(path)?;
    let mut buf = vec![0u64; 8192];
    let bytes = buf.len() * 8;
    let mut class = FileFullDirectoryRestartInfo;
    let mut out = Listing::default();
    let name_at = offset_of!(FILE_FULL_DIR_INFO, FileName);
    loop {
        if let Err(e) = unsafe {
            GetFileInformationByHandleEx(h.raw(), class, buf.as_mut_ptr().cast(), bytes as u32)
        } {
            if e.code() == ERROR_NO_MORE_FILES.to_hresult() {
                break;
            }
            return Err(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF));
        }
        class = FileFullDirectoryInfo;
        let raw: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), bytes) };
        let mut at = 0usize;
        loop {
            let field = |off: usize, n: usize| &raw[at + off..at + off + n];
            let u32_of = |off| u32::from_le_bytes(field(off, 4).try_into().unwrap());
            let i64_of = |off| i64::from_le_bytes(field(off, 8).try_into().unwrap());
            let next = u32_of(offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset)) as usize;
            let name_len = u32_of(offset_of!(FILE_FULL_DIR_INFO, FileNameLength)) as usize;
            if at + name_at + name_len > raw.len() {
                break;
            }
            let units: Vec<u16> = raw[at + name_at..at + name_at + name_len]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let name = String::from_utf16_lossy(&units);
            if name != "." && name != ".." {
                let attrs = u32_of(offset_of!(FILE_FULL_DIR_INFO, FileAttributes));
                let flags = flags_of(attrs);
                let dir = flags & DIR != 0;
                out.push(
                    &name,
                    Entry {
                        size: if dir {
                            0
                        } else {
                            i64_of(offset_of!(FILE_FULL_DIR_INFO, EndOfFile)) as u64
                        },
                        alloc: if dir {
                            0
                        } else {
                            i64_of(offset_of!(FILE_FULL_DIR_INFO, AllocationSize)) as u64
                        },
                        modified: filetime_to_unix(i64_of(offset_of!(
                            FILE_FULL_DIR_INFO,
                            LastWriteTime
                        ))),
                        flags,
                        link: None,
                    },
                );
            }
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    Ok(out)
}

/// Reads everything under `path` (a drive's root or any folder).
pub fn scan(path: &Path, progress: &Progress) -> Result<Tree, String> {
    let started = Instant::now();
    let _quiet = QuietErrors::new();
    let path = if path.as_os_str().len() == 2 && path.to_string_lossy().ends_with(':') {
        PathBuf::from(format!("{}\\", path.display()))
    } else {
        path.to_path_buf()
    };
    if !path.is_dir() {
        return Err(format!(
            "{} is not a folder that can be read.",
            path.display()
        ));
    }
    let name = path.to_string_lossy().into_owned();
    let volume = volume_of(&path);
    let fs = volume
        .as_ref()
        .and_then(|(root, _)| volume_info(&wide(&root.to_string_lossy())))
        .map(|(_, fs)| fs)
        .unwrap_or_default();
    let (mut total, mut free) = (0u64, 0u64);
    let path_w = wide(&path.to_string_lossy());
    let _ = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(path_w.as_ptr()),
            None,
            Some(&mut total),
            Some(&mut free),
        )
    };

    let mut note = None;
    if fs == "NTFS"
        && let Some((root, device)) = &volume
    {
        if crate::hardening::is_elevated() {
            let record = if root == &path {
                Some(mft::ROOT_RECORD)
            } else {
                record_of(&path)
            };
            if let Some(record) = record {
                match mft::scan(device, record, &name, progress) {
                    Ok(builder) => {
                        return Ok(finish(
                            builder,
                            &path,
                            Method::Mft,
                            None,
                            0,
                            started,
                            progress,
                            total,
                            free,
                        ));
                    }
                    Err(e) if e == CANCELLED => return Err(e),
                    Err(e) => {
                        note = Some(format!(
                            "The master file table could not be read ({e}), so it was read folder by folder."
                        ));
                        progress
                            .files
                            .store(0, std::sync::atomic::Ordering::Relaxed);
                        progress
                            .folders
                            .store(0, std::sync::atomic::Ordering::Relaxed);
                        progress
                            .bytes
                            .store(0, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        } else {
            note = Some(
                "Reading the master file table, which takes seconds, needs Anabolic running as \
                 administrator; without, it was read folder by folder."
                    .into(),
            );
        }
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let root_entry = Entry {
        flags: DIR,
        ..Entry::default()
    };
    let (builder, unreadable) = walk(&path, &name, root_entry, threads, progress, list)?;
    Ok(finish(
        builder,
        &path,
        Method::Walk,
        note,
        unreadable,
        started,
        progress,
        total,
        free,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    builder: Builder,
    path: &Path,
    method: Method,
    note: Option<String>,
    unreadable: u32,
    started: Instant,
    progress: &Progress,
    total: u64,
    free: u64,
) -> Tree {
    let mut tree = Tree::assemble(builder.finish(progress), path, method);
    tree.note = note;
    tree.unreadable = unreadable;
    tree.total = total;
    tree.free = free;
    tree.took = started.elapsed();
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the system drive both ways and compares a folder of it. Needs
    /// an administrator; run with `cargo test -p anabolic-core space -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_table_and_the_folders_agree() {
        let progress = Progress::default();
        let tree = scan(Path::new("C:\\"), &progress).unwrap();
        println!(
            "C: by {} in {:?}: {} nodes, {} files, {:.1} GB",
            tree.method.label(),
            tree.took,
            tree.nodes.len(),
            tree.nodes[0].files,
            tree.nodes[0].size as f64 / 1e9
        );
        assert_eq!(tree.method, Method::Mft, "{:?}", tree.note);
        let folder = Path::new(r"C:\Windows\System32\drivers");
        let by_table = scan(folder, &Progress::default()).unwrap();
        let by_walk = {
            let progress = Progress::default();
            let (b, _) = walk(
                folder,
                "x",
                Entry {
                    flags: DIR,
                    ..Entry::default()
                },
                4,
                &progress,
                list,
            )
            .unwrap();
            Tree::assemble(b.finish(&progress), folder, Method::Walk)
        };
        println!(
            "drivers: table {} files {} bytes in {:?}; folders {} files {} bytes",
            by_table.nodes[0].files,
            by_table.nodes[0].size,
            by_table.took,
            by_walk.nodes[0].files,
            by_walk.nodes[0].size
        );
        assert_eq!(by_table.method, Method::Mft);
        let diff = by_table.nodes[0].size.abs_diff(by_walk.nodes[0].size);
        assert!(diff < by_walk.nodes[0].size / 100, "within 1 %");
    }
}
