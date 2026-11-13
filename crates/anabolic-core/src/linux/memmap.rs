// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! A process' memory from /proc/PID/smaps: each mapping with what is in RAM
//! (Rss) and of that what only this process uses (Private_Clean and
//! Private_Dirty). The kernel commits lazily, so "committed" here is what
//! is in RAM or swapped out. [heap] and the threads' stacks are named as
//! such; a file mapped executable is a program or library. Another user's
//! process needs root.

use std::time::Instant;

use crate::memmap::{Allocation, Kind, MemoryMap};
use crate::model::ProcKey;

pub fn read(key: ProcKey) -> Result<MemoryMap, String> {
    let started = Instant::now();
    let text = std::fs::read_to_string(format!("/proc/{}/smaps", key.pid))
        .map_err(|e| format!("/proc/{}/smaps could not be read: {e}", key.pid))?;
    Ok(MemoryMap {
        key,
        allocations: parse(&text),
        at: crate::sysinfo::now_filetime(),
        took: started.elapsed(),
    })
}

fn parse(text: &str) -> Vec<Allocation> {
    let mut out: Vec<Allocation> = Vec::new();
    let mut swapped = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(first) = parts.next() else { continue };
        if let Some((start, end)) = first.split_once('-')
            && let (Ok(start), Ok(end)) =
                (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16))
        {
            if let Some(last) = out.last_mut() {
                last.committed += swapped;
            }
            swapped = 0;
            let perms = parts.next().unwrap_or("----");
            let path = parts.skip(3).collect::<Vec<_>>().join(" ");
            let kind = if path == "[heap]" {
                Kind::Heap
            } else if path.starts_with("[stack") {
                Kind::Stack
            } else if path.starts_with('[') {
                // [vdso], [vvar]: the kernel's own pages in the process.
                Kind::Image
            } else if path.is_empty() {
                if perms.ends_with('s') {
                    Kind::Shareable
                } else {
                    Kind::PrivateData
                }
            } else if perms.contains('x')
                || out
                    .last()
                    .is_some_and(|l| l.kind == Kind::Image && l.detail == path)
            {
                Kind::Image
            } else {
                Kind::MappedFile
            };
            let protection = match &perms[..perms.len().min(3)] {
                "r--" => "Read",
                "rw-" => "Read/Write",
                "r-x" => "Execute/Read",
                "rwx" => "Execute/Read/Write",
                "---" => "No access",
                "--x" => "Execute",
                _ => "Other",
            };
            out.push(Allocation {
                base: start,
                size: end - start,
                kind,
                protection: protection.into(),
                detail: match kind {
                    Kind::Stack if path == "[stack]" => "The main thread's stack".into(),
                    Kind::Heap => "The process' heap ([heap])".into(),
                    _ => path,
                },
                blocks: 1,
                ..Allocation::default()
            });
            continue;
        }
        let Some(last) = out.last_mut() else { continue };
        let kb = parts
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024;
        match first {
            "Rss:" => {
                last.working_set = kb;
                last.committed += kb;
            }
            "Private_Clean:" | "Private_Dirty:" => last.private_ws += kb,
            "Swap:" => swapped = kb,
            _ => {}
        }
    }
    if let Some(last) = out.last_mut() {
        last.committed += swapped;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_smaps() {
        let list = parse(
            "55d0c0000000-55d0c0021000 r-xp 00000000 08:01 123 /usr/bin/foo\n\
             Size:                132 kB\nRss:                  64 kB\n\
             Private_Clean:        8 kB\nPrivate_Dirty:        0 kB\nSwap:                 0 kB\n\
             55d0c1000000-55d0c1400000 rw-p 00000000 00:00 0 [heap]\n\
             Rss:                1024 kB\nPrivate_Dirty:     1000 kB\nSwap:               100 kB\n\
             7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0 [stack]\n\
             Rss:                  12 kB\n\
             7f0000000000-7f0000100000 rw-s 00000000 00:01 9 \n\
             Rss:                   4 kB\n",
        );
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].kind, Kind::Image);
        assert_eq!(list[0].detail, "/usr/bin/foo");
        assert_eq!(list[0].working_set, 64 * 1024);
        assert_eq!(list[0].private_ws, 8 * 1024);
        assert_eq!(list[1].kind, Kind::Heap);
        assert_eq!(list[1].committed, (1024 + 100) * 1024);
        assert_eq!(list[2].kind, Kind::Stack);
        assert_eq!(list[3].kind, Kind::Shareable);
        assert_eq!(list[1].protection, "Read/Write");
    }
}
