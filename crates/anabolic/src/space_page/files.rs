// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Choosing a folder to scan, and saving what the page lists as CSV: both
//! on threads of their own, so the window never waits for a dialog or a
//! disk.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anabolic_core::space::{DIR, EXTRA_LINK, ExtTotal, ROOT, Tree};
use anabolic_core::sysinfo::local_time;
use crossbeam_channel::{Receiver, bounded};

use crate::fmt;

/// The desktop's folder picker, on a thread of its own (the dialog runs
/// its own message loop); None when nothing was chosen.
#[cfg(windows)]
pub fn pick_folder(owner: isize) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = bounded(1);
    let _ = std::thread::Builder::new()
        .name("space-pick".into())
        .spawn(move || {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree,
            };
            use windows::Win32::UI::Shell::{
                FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
                SIGDN_FILESYSPATH,
            };
            use windows::core::w;
            let picked = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                (|| -> windows::core::Result<String> {
                    let dialog: IFileOpenDialog =
                        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                    dialog.SetTitle(w!("A folder to see what fills it"))?;
                    dialog
                        .SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
                    let owner = (owner != 0).then_some(HWND(owner as *mut _));
                    dialog.Show(owner)?;
                    let item = dialog.GetResult()?;
                    let path = item.GetDisplayName(SIGDN_FILESYSPATH)?;
                    let text = path.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(path.0.cast()));
                    Ok(text)
                })()
            };
            let _ = tx.send(picked.ok().filter(|p| !p.is_empty()).map(PathBuf::from));
        });
    rx
}

/// The desktop's folder picker: KDE's kdialog, else GNOME's zenity, on a
/// thread of its own.
#[cfg(target_os = "linux")]
pub fn pick_folder(_owner: isize) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = bounded(1);
    let _ = std::thread::Builder::new()
        .name("space-pick".into())
        .spawn(move || {
            use std::process::Command;
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            let title = "A folder to see what fills it";
            let ask = |program: &str, args: &[&str]| {
                let out = Command::new(program).args(args).output().ok()?;
                let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
                Some((out.status.success() && !path.is_empty()).then_some(path))
            };
            let picked = ask(
                "kdialog",
                &["--title", title, "--getexistingdirectory", &home],
            )
            .or_else(|| {
                ask(
                    "zenity",
                    &[
                        "--file-selection",
                        "--directory",
                        &format!("--title={title}"),
                    ],
                )
            })
            .flatten();
            let _ = tx.send(picked.map(PathBuf::from));
        });
    rx
}

/// What a CSV is made of: a title row, then rows.
pub type Table = (Vec<&'static str>, Vec<Vec<String>>);

fn date(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    local_time(secs * 10_000_000 + 116_444_736_000_000_000).unwrap_or_default()
}

/// Every folder of at least a megabyte, folder by folder from the top: a
/// drive's whole tree would be millions of rows nobody opens.
pub fn folders(tree: &Tree) -> Table {
    const MIN: u64 = 1 << 20;
    const MOST: usize = 200_000;
    let mut rows = Vec::new();
    let mut stack = vec![ROOT];
    while let Some(n) = stack.pop() {
        if rows.len() >= MOST {
            break;
        }
        let node = tree.node(n);
        rows.push(vec![
            tree.path_of(n).to_string_lossy().into_owned(),
            node.size.to_string(),
            node.alloc.to_string(),
            node.files.to_string(),
            date(node.modified),
        ]);
        // Reversed so the largest is written first.
        for c in tree.children(n).rev() {
            if tree.flags(c) & DIR != 0 && tree.size(c) >= MIN && !tree.is_gone(c) {
                stack.push(c);
            }
        }
    }
    (vec!["Folder", "Size", "On disk", "Files", "Modified"], rows)
}

pub fn types(tree: &Tree, exts: &[ExtTotal]) -> Table {
    let rows = exts
        .iter()
        .map(|e| {
            vec![
                match tree.ext_name(e.ext) {
                    "" => String::new(),
                    x => format!(".{x}"),
                },
                e.size.to_string(),
                e.alloc.to_string(),
                e.files.to_string(),
            ]
        })
        .collect();
    (vec!["Extension", "Size", "On disk", "Files"], rows)
}

pub fn largest(tree: &Tree, files: &[u32]) -> Table {
    let rows = files
        .iter()
        .filter(|&&n| !tree.is_gone(n) && tree.flags(n) & EXTRA_LINK == 0)
        .map(|&n| {
            let node = tree.node(n);
            vec![
                tree.path_of(n).to_string_lossy().into_owned(),
                node.size.to_string(),
                node.alloc.to_string(),
                date(node.modified),
            ]
        })
        .collect();
    (vec!["File", "Size", "On disk", "Modified"], rows)
}

pub fn clean(tree: &Tree, found: &[anabolic_core::space::clean::Suggestion]) -> Table {
    let mut rows = Vec::new();
    for s in found {
        for &n in &s.nodes {
            rows.push(vec![
                s.title.to_string(),
                s.level.label().to_string(),
                tree.path_of(n).to_string_lossy().into_owned(),
                tree.size(n).to_string(),
            ]);
        }
    }
    (vec!["Clean-up", "How safe", "Path", "Size"], rows)
}

pub fn dupes(tree: &Tree, groups: &[anabolic_core::space::dupes::Group]) -> Table {
    let mut rows = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        for &n in group.nodes.iter().filter(|&&n| !tree.is_gone(n)) {
            rows.push(vec![
                (g + 1).to_string(),
                tree.path_of(n).to_string_lossy().into_owned(),
                group.size.to_string(),
            ]);
        }
    }
    (vec!["Set", "File", "Size"], rows)
}

/// Writes `make`'s table as CSV into Anabolic's exports folder, on a
/// thread of its own; answers the file written.
pub fn save<F>(name: &'static str, tree: Arc<Tree>, make: F) -> Receiver<Result<PathBuf, String>>
where
    F: FnOnce(&Tree) -> Table + Send + 'static,
{
    let (tx, rx) = bounded(1);
    let _ = std::thread::Builder::new()
        .name("space-export".into())
        .spawn(move || {
            let run = || -> Result<PathBuf, String> {
                let (titles, rows) = make(&tree);
                let dir = crate::data_dir()
                    .ok_or("no folder for Anabolic's files")?
                    .join("exports");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let stamp: String = fmt::clock(SystemTime::now(), false)
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                let path = dir.join(format!("space-{name}-{stamp}.csv"));
                let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
                let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                let mut out = std::io::BufWriter::new(file);
                let line = |cells: &mut dyn Iterator<Item = String>| {
                    cells.collect::<Vec<_>>().join(",") + "\r\n"
                };
                out.write_all(line(&mut titles.iter().map(|t| quote(t))).as_bytes())
                    .map_err(|e| e.to_string())?;
                for row in rows {
                    out.write_all(line(&mut row.iter().map(|c| quote(c))).as_bytes())
                        .map_err(|e| e.to_string())?;
                }
                out.flush().map_err(|e| e.to_string())?;
                Ok(path)
            };
            let _ = tx.send(run());
        });
    rx
}
