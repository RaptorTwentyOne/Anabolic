// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! What fills a drive or folder, read the way the Disk space page reads it.
//! A scan is kept for a while, so asking next about a folder inside it
//! does not read the drive again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anabolic_core::space::{self, DIR, EXTRA_LINK, Phase, Progress, ROOT, Tree};
use anabolic_core::sysinfo::local_time;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::handler::{AnabolicMcp, fail, ok, progress};
use crate::settings::McpKind;

/// How long a scan is kept for further questions.
const KEEP: Duration = Duration::from_secs(600);

/// The last scan, kept for further questions about it.
pub struct Kept {
    tree: Arc<Tree>,
    at: Instant,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct SpaceArgs {
    /// The drive or folder ("C:\\", "D:\\Games", "/home/you"). Default: the
    /// system drive on Windows, the home folder on Linux.
    pub path: Option<String>,
    /// Folder levels to return below it (1 to 4, default 2).
    pub depth: Option<u32>,
    /// The largest entries of each folder (1 to 50, default 8); the rest of
    /// a folder is summed into one line.
    pub top: Option<u32>,
    /// How many of the largest files under it to list (0 to 200, default
    /// 20).
    pub largest: Option<u32>,
    /// How many file types to list, the most room first (0 to 50, default
    /// 10).
    pub types: Option<u32>,
    /// True reads it again instead of using a scan of it (or of a folder
    /// above it) from the last ten minutes.
    pub rescan: bool,
}

const DESCRIPTION: &str = "Lists what fills a drive or folder, as Anabolic's Disk space page reads \
it: its folders as a tree (the largest entries of each level with their size and share, the rest \
summed into one line), the largest files under it and the file types taking the most room. Use \
it when a drive is full or to find what to clean up; get_disks gives the drives' size and \
activity, not their contents. On Windows an NTFS drive is read from its master file table when \
Anabolic runs as administrator (seconds for millions of files, and the whole drive even for a \
folder of it), otherwise folder by folder, which takes longer; progress is reported while it \
reads. A scan is kept for ten minutes, so asking next about a folder inside it is instant \
(rescan=true reads again). It only reads: nothing is deleted, the user deletes from the page.";

/// The folder asked about, as a path to scan.
fn target(asked: Option<&str>) -> PathBuf {
    if let Some(p) = asked.map(str::trim).filter(|p| !p.is_empty()) {
        let p = if cfg!(windows) && p.len() == 2 && p.ends_with(':') {
            format!("{p}\\")
        } else {
            p.to_string()
        };
        return PathBuf::from(p);
    }
    if cfg!(windows) {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        PathBuf::from(format!("{drive}\\"))
    } else {
        std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
    }
}

/// The node of `path` in a tree scanned at or above it.
fn find(tree: &Tree, path: &Path) -> Option<u32> {
    let sep = std::path::MAIN_SEPARATOR;
    let norm = |p: &Path| {
        let s = p.to_string_lossy().trim_end_matches(sep).to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let (base, asked) = (norm(&tree.path), norm(path));
    if asked == base {
        return Some(ROOT);
    }
    let rest = asked.strip_prefix(&format!("{base}{sep}"))?;
    let mut at = ROOT;
    for part in rest.split(sep).filter(|p| !p.is_empty()) {
        at = tree.children(at).find(|&c| {
            let name = tree.name(c);
            if cfg!(windows) {
                name.to_lowercase() == part
            } else {
                name == part
            }
        })?;
    }
    Some(at)
}

/// Every file under `root`.
fn files_under(tree: &Tree, root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if tree.is_dir(n) {
            stack.extend(tree.children(n));
        } else if tree.flags(n) & EXTRA_LINK == 0 {
            out.push(n);
        }
    }
    out
}

fn modified(secs: i64) -> Value {
    if secs <= 0 {
        return Value::Null;
    }
    local_time(secs * 10_000_000 + 116_444_736_000_000_000).map_or(Value::Null, Value::from)
}

fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 * 1000.0 / whole as f64).round() / 10.0
    }
}

/// A folder and, `depth` levels down, its largest entries.
fn branch(tree: &Tree, n: u32, name: &str, depth: u32, top: usize) -> Value {
    let node = tree.node(n);
    let parent = if node.parent == space::NONE || n == ROOT {
        node.size
    } else {
        tree.size(node.parent)
    };
    let mut out = json!({
        "name": name,
        "size_bytes": node.size,
        "on_disk_bytes": node.alloc,
        "share_percent": share(node.size, parent),
    });
    if node.flags & DIR != 0 {
        out["files"] = json!(node.files);
        if depth > 0 && node.count > 0 {
            let kids: Vec<u32> = tree.children(n).filter(|&c| tree.size(c) > 0).collect();
            let mut children: Vec<Value> = kids
                .iter()
                .take(top)
                .map(|&c| branch(tree, c, tree.name(c), depth - 1, top))
                .collect();
            if kids.len() > top {
                let rest: u64 = kids[top..].iter().map(|&c| tree.size(c)).sum();
                children.push(json!({
                    "name": format!("({} more)", kids.len() - top),
                    "size_bytes": rest,
                    "share_percent": share(rest, node.size),
                }));
            }
            out["children"] = json!(children);
        }
    }
    out
}

impl AnabolicMcp {
    /// A kept scan that covers `path`, unless a new one is wanted.
    fn kept(&self, path: &Path) -> Option<(Arc<Tree>, u32, Duration)> {
        let kept = self.shared.space.lock().ok()?;
        let k = kept.as_ref()?;
        let age = k.at.elapsed();
        if age > KEEP {
            return None;
        }
        let node = find(&k.tree, path)?;
        Some((k.tree.clone(), node, age))
    }

    async fn scan_space(
        &self,
        path: &Path,
        context: &RequestContext<RoleServer>,
    ) -> Result<Arc<Tree>, String> {
        let watch = Arc::new(Progress::default());
        let (p, at) = (watch.clone(), path.to_path_buf());
        let work = tokio::task::spawn_blocking(move || space::scan(&at, &p));
        while !work.is_finished() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if context.ct.is_cancelled() {
                watch.cancel();
            }
            let table = watch.table_total.load(Ordering::Relaxed);
            if watch.phase() == Phase::Building {
                progress(context, 1.0, Some(1.0), "Putting the tree together").await;
            } else if table > 0 {
                let read = watch.table_read.load(Ordering::Relaxed);
                progress(
                    context,
                    read as f64,
                    Some(table as f64),
                    "Reading the master file table",
                )
                .await;
            } else {
                let files = watch.files.load(Ordering::Relaxed);
                progress(context, files as f64, None, format!("{files} files read")).await;
            }
        }
        let tree = Arc::new(work.await.map_err(|e| format!("The scan stopped: {e}"))??);
        if let Ok(mut kept) = self.shared.space.lock() {
            *kept = Some(Kept {
                tree: tree.clone(),
                at: Instant::now(),
            });
        }
        // Let go of it once it is too old to be asked about.
        let shared = self.shared.clone();
        let mine = Arc::downgrade(&tree);
        tokio::spawn(async move {
            tokio::time::sleep(KEEP).await;
            if let Ok(mut kept) = shared.space.lock()
                && kept
                    .as_ref()
                    .is_some_and(|k| mine.upgrade().is_some_and(|t| Arc::ptr_eq(&t, &k.tree)))
            {
                *kept = None;
            }
        });
        Ok(tree)
    }
}

#[tool_router(router = space_tools, vis = "pub(super)")]
impl AnabolicMcp {
    #[tool(
        description = DESCRIPTION,
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_disk_usage(
        &self,
        Parameters(args): Parameters<SpaceArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(refused) = self.refuse(McpKind::Read) {
            return refused;
        }
        let path = target(args.path.as_deref());
        let depth = args.depth.unwrap_or(2).clamp(1, 4);
        let top = args.top.unwrap_or(8).clamp(1, 50) as usize;
        let largest = args.largest.unwrap_or(20).min(200) as usize;
        let types = args.types.unwrap_or(10).min(50) as usize;

        let kept = if args.rescan { None } else { self.kept(&path) };
        let (tree, node, age) = match kept {
            Some(k) => k,
            None => match self.scan_space(&path, &context).await {
                Ok(tree) => (tree, ROOT, Duration::ZERO),
                Err(e) => return fail(e),
            },
        };
        let root = tree.node(node);
        let shown = if node == ROOT {
            tree.path.to_string_lossy().into_owned()
        } else {
            tree.path_of(node).to_string_lossy().into_owned()
        };

        let files = files_under(&tree, node);
        let mut biggest = files.clone();
        let key = |i: &u32| std::cmp::Reverse(tree.size(*i));
        if biggest.len() > largest {
            biggest.select_nth_unstable_by_key(largest, key);
            biggest.truncate(largest);
        }
        biggest.sort_unstable_by_key(key);
        let mut by_ext: std::collections::HashMap<u32, (u64, u64)> = Default::default();
        for &f in &files {
            let n = tree.node(f);
            let e = by_ext.entry(n.ext).or_default();
            e.0 += n.size;
            e.1 += 1;
        }
        let mut exts: Vec<(u32, (u64, u64))> = by_ext.into_iter().collect();
        exts.sort_unstable_by_key(|(_, (size, _))| std::cmp::Reverse(*size));
        exts.truncate(types);

        let mut out = json!({
            "path": shown,
            "read": tree.method.label(),
            "size_bytes": root.size,
            "on_disk_bytes": root.alloc,
            "files": root.files,
            "drive_size_bytes": tree.total,
            "drive_free_bytes": tree.free,
            "tree": [branch(&tree, node, &shown, depth, top)],
            "largest_files": biggest.iter().map(|&f| json!({
                "name": tree.name(f),
                "path": tree.path_of(f).to_string_lossy(),
                "size_bytes": tree.size(f),
                "modified": modified(tree.node(f).modified),
            })).collect::<Vec<_>>(),
            "types": exts.iter().map(|(e, (size, count))| json!({
                "extension": match tree.ext_name(*e) {
                    "" => "(none)".to_string(),
                    x => format!(".{x}"),
                },
                "size_bytes": size,
                "share_percent": share(*size, root.size),
                "files": count,
            })).collect::<Vec<_>>(),
        });
        if age.is_zero() {
            out["took_ms"] = json!(tree.took.as_millis() as u64);
        } else {
            out["scanned_seconds_ago"] = json!(age.as_secs());
        }
        if tree.unreadable > 0 {
            out["unreadable_folders"] = json!(tree.unreadable);
        }
        if let Some(note) = &tree.note {
            out["note"] = json!(note);
        }
        ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_folder_inside_a_kept_scan() {
        let dir = std::env::temp_dir().join(format!("anabolic-space-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Games/Big")).unwrap();
        std::fs::write(dir.join("Games/Big/data.pak"), vec![0u8; 4000]).unwrap();
        std::fs::write(dir.join("notes.txt"), b"hi").unwrap();
        let tree = space::scan(&dir, &Progress::default()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(find(&tree, &dir), Some(ROOT));
        let big = find(&tree, &dir.join("Games").join("Big")).unwrap();
        assert_eq!(tree.name(big), "Big");
        if cfg!(windows) {
            let upper = PathBuf::from(dir.join("GAMES").to_string_lossy().to_uppercase());
            assert!(find(&tree, &upper).is_some(), "Windows paths ignore case");
        }
        assert!(find(&tree, &dir.join("Missing")).is_none());
        assert!(find(&tree, Path::new("/elsewhere")).is_none());

        let shown = branch(&tree, ROOT, "top", 2, 1);
        assert_eq!(shown["size_bytes"], 4002);
        let kids = shown["children"].as_array().unwrap();
        assert_eq!(kids[0]["name"], "Games");
        assert_eq!(kids[1]["name"], "(1 more)", "the rest summed");
        assert_eq!(kids[1]["size_bytes"], 2);
        assert_eq!(files_under(&tree, ROOT).len(), 2);
    }
}
