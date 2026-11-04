// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Files with the same contents. Only files of the same size can be the
//! same, which the tree says for nothing; of those, the start, middle and
//! end are compared first, and only files alike there are read whole. The
//! reading is done by a few threads at the lowest priority the system has
//! for disk, memory and processor, so it never slows what the user does.
//!
//! Left out: files stored in the cloud and not on the disk (OneDrive's
//! placeholders: reading one would download it), links, and second names
//! of one file (hard links), which take no room twice.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::{DIR, EXTRA_LINK, LINK, META, Tree};

/// Files with the same contents, largest wasted room first.
#[derive(Clone, Debug)]
pub struct Group {
    pub size: u64,
    pub nodes: Vec<u32>,
}

impl Group {
    /// What deleting all but one would free.
    pub fn wasted(&self) -> u64 {
        self.size * (self.nodes.len() as u64).saturating_sub(1)
    }
}

/// How far a search is, read by the window while it runs.
#[derive(Default)]
pub struct Progress {
    /// Bytes to read, and read so far.
    pub total: AtomicU64,
    pub read: AtomicU64,
    /// Files that share a size with another.
    pub candidates: AtomicUsize,
    cancel: AtomicBool,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A file to compare: its node, where it is, and how large.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub node: u32,
    pub path: PathBuf,
    pub size: u64,
}

/// The files that share a size with another, at least `min` bytes. Quick:
/// the tree alone. The search itself works from these, so it does not keep
/// the tree while it reads.
pub fn candidates(tree: &Tree, min: u64) -> Vec<Candidate> {
    let mut by_size: HashMap<u64, Vec<u32>> = HashMap::new();
    for i in 0..tree.len() as u32 {
        let n = tree.node(i);
        let skip = DIR | EXTRA_LINK | LINK | META;
        // Nothing allocated: in the cloud, or all holes.
        if n.flags & skip != 0 || n.size < min.max(1) || n.alloc == 0 || tree.is_gone(i) {
            continue;
        }
        by_size.entry(n.size).or_default().push(i);
    }
    by_size
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .map(|node| Candidate {
            node,
            path: tree.path_of(node),
            size: tree.size(node),
        })
        .collect()
}

/// How much is read of each end and the middle at first.
const SAMPLE: usize = 64 << 10;
const BUFFER: usize = 1 << 20;

/// A 128-bit fingerprint from two keyed hashes.
struct Print(std::hash::DefaultHasher, std::hash::DefaultHasher);

impl Print {
    fn new(keys: &(RandomState, RandomState)) -> Print {
        Print(keys.0.build_hasher(), keys.1.build_hasher())
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
        self.1.write(bytes);
    }

    fn finish(&self) -> u128 {
        (u128::from(self.0.finish()) << 64) | u128::from(self.1.finish())
    }
}

fn open(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Read front to back: the cache reads ahead and lets go behind.
        const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
        options.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    }
    options.open(path)
}

/// Which file a handle is: two names of one file are one file.
#[cfg(windows)]
fn identity(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[cfg(target_os = "linux")]
fn identity(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().ok()?;
    Some((m.dev(), m.ino()))
}

/// The start, middle and end of a file.
fn sample(
    c: &Candidate,
    keys: &(RandomState, RandomState),
    p: &Progress,
) -> Option<(u128, (u64, u64))> {
    let mut file = open(&c.path).ok()?;
    let id = identity(&file)?;
    let mut print = Print::new(keys);
    let mut buf = vec![0u8; SAMPLE];
    let mut at = |offset: u64| -> Option<()> {
        file.seek(SeekFrom::Start(offset)).ok()?;
        let n = read_up_to(&mut file, &mut buf).ok()?;
        print.write(&buf[..n]);
        p.read.fetch_add(n as u64, Ordering::Relaxed);
        Some(())
    };
    at(0)?;
    if c.size > SAMPLE as u64 {
        at(c.size / 2)?;
        at(c.size.saturating_sub(SAMPLE as u64))?;
    }
    Some((print.finish(), id))
}

/// The whole file.
fn whole(c: &Candidate, keys: &(RandomState, RandomState), p: &Progress) -> Option<u128> {
    let mut file = open(&c.path).ok()?;
    let mut print = Print::new(keys);
    let mut buf = vec![0u8; BUFFER];
    let mut seen = 0u64;
    loop {
        if p.cancelled() {
            return None;
        }
        let n = read_up_to(&mut file, &mut buf).ok()?;
        if n == 0 {
            break;
        }
        print.write(&buf[..n]);
        seen += n as u64;
        p.read.fetch_add(n as u64, Ordering::Relaxed);
    }
    // A file that changed size since the scan is not what was compared.
    (seen == c.size).then(|| print.finish())
}

fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Runs `work` over `items` on a few background threads; None where it
/// answered None.
fn each<T: Sync, R: Send>(
    items: &[T],
    p: &Progress,
    work: impl Fn(&T) -> Option<R> + Sync,
) -> Vec<Option<R>> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                crate::hardening::set_current_thread_priority(
                    crate::hardening::ThreadPriority::Background,
                );
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() || p.cancelled() {
                        break;
                    }
                    if let Some(r) = work(&items[i]) {
                        out.lock().unwrap().push((i, r));
                    }
                }
            });
        }
    });
    let mut all: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in out.into_inner().unwrap() {
        all[i] = Some(r);
    }
    all
}

/// Groups of files with the same contents among `files`.
pub fn find(files: Vec<Candidate>, p: &Progress) -> Result<Vec<Group>, String> {
    p.candidates.store(files.len(), Ordering::Relaxed);
    let samples: u64 = files.iter().map(|c| c.size.min(3 * SAMPLE as u64)).sum();
    p.total.store(samples, Ordering::Relaxed);
    let keys = (RandomState::new(), RandomState::new());

    // The start, middle and end; one file under two names is one.
    let sampled = each(&files, p, |c| sample(c, &keys, p));
    if p.cancelled() {
        return Err(super::CANCELLED.into());
    }
    let mut seen_ids = std::collections::HashSet::new();
    let mut alike: HashMap<(u64, u128), Vec<usize>> = HashMap::new();
    for (i, s) in sampled.into_iter().enumerate() {
        if let Some((print, id)) = s
            && seen_ids.insert(id)
        {
            alike.entry((files[i].size, print)).or_default().push(i);
        }
    }

    // What is alike there is read whole.
    let to_read: Vec<usize> = alike
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();
    let rest: u64 = to_read.iter().map(|&i| files[i].size).sum();
    p.total.fetch_add(rest, Ordering::Relaxed);
    let read: Vec<&Candidate> = to_read.iter().map(|&i| &files[i]).collect();
    let full = each(&read, p, |c| whole(c, &keys, p));
    if p.cancelled() {
        return Err(super::CANCELLED.into());
    }
    let mut same: HashMap<(u64, u128), Vec<u32>> = HashMap::new();
    for (k, f) in full.into_iter().enumerate() {
        if let Some(print) = f {
            let c = read[k];
            same.entry((c.size, print)).or_default().push(c.node);
        }
    }
    let mut groups: Vec<Group> = same
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|((size, _), mut nodes)| {
            nodes.sort_unstable();
            Group { size, nodes }
        })
        .collect();
    groups.sort_unstable_by_key(|g| std::cmp::Reverse(g.wasted()));
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::{Progress as ScanProgress, scan};

    #[test]
    fn finds_the_same_contents_and_only_them() {
        let dir = std::env::temp_dir().join(format!("anabolic-dupes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut other = big.clone();
        // Same size, same start, middle and end: different in between.
        other[100_000] ^= 1;
        std::fs::write(dir.join("a/one.bin"), &big).unwrap();
        std::fs::write(dir.join("b/two.bin"), &big).unwrap();
        std::fs::write(dir.join("b/three.bin"), &big).unwrap();
        std::fs::write(dir.join("a/almost.bin"), &other).unwrap();
        std::fs::write(dir.join("a/small1.txt"), b"same").unwrap();
        std::fs::write(dir.join("b/small2.txt"), b"same").unwrap();
        // A second name of one.bin is not a copy of it.
        let linked = std::fs::hard_link(dir.join("a/one.bin"), dir.join("b/link.bin")).is_ok();
        let tree = scan(&dir, &ScanProgress::default()).unwrap();
        let files = candidates(&tree, 1000);
        let groups = find(files, &Progress::default()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(groups.len(), 1, "{groups:?}");
        let names: Vec<&str> = groups[0].nodes.iter().map(|&n| tree.name(n)).collect();
        assert!(
            names.contains(&"two.bin") && names.contains(&"three.bin"),
            "{names:?}"
        );
        assert!(!names.contains(&"almost.bin"));
        assert_eq!(
            groups[0].nodes.len(),
            3,
            "one.bin or its link, not both: {names:?} {linked}"
        );
        assert_eq!(groups[0].wasted(), 2 * 300_000);
    }
}
