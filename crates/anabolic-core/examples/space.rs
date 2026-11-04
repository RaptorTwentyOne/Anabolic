// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! Scans a drive or folder and draws its treemap, printing how long each
//! step took: `cargo run --release -p anabolic-core --example space -- C:\ [WxH]`.
//! With SPACE_PPM=<file> the map is written as a PPM image.

use std::path::PathBuf;
use std::time::Instant;

use anabolic_core::space::{self, Progress, ROOT, treemap};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let (w, h) = args
        .next()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((2560u32, 1400u32));

    let progress = Progress::default();
    let started = Instant::now();
    let done = std::sync::atomic::AtomicBool::new(false);
    let tree = std::thread::scope(|s| {
        s.spawn(|| {
            let mut read_at = None;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if read_at.is_none() && progress.phase() == space::Phase::Building {
                    read_at = Some(started.elapsed());
                    println!("reading took {:.2?}", started.elapsed());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        let tree = space::scan(&path, &progress);
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        tree
    });
    let tree = match tree {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let root = tree.node(ROOT);
    println!(
        "{} by {} in {:.2?}: {} nodes, {} files, {:.2} GB ({:.2} GB on disk), {} unreadable",
        tree.path.display(),
        tree.method.label(),
        tree.took,
        tree.len(),
        root.files,
        root.size as f64 / 1e9,
        root.alloc as f64 / 1e9,
        tree.unreadable,
    );
    println!(
        "drive: {:.2} GB, {:.2} GB free, so {:.2} GB used",
        tree.total as f64 / 1e9,
        tree.free as f64 / 1e9,
        (tree.total - tree.free) as f64 / 1e9
    );
    if std::env::var_os("SPACE_DRILL").is_some() {
        // The files whose space on disk is furthest above their size.
        let mut odd: Vec<u32> = (0..tree.len() as u32)
            .filter(|&i| !tree.node(i).is_dir())
            .collect();
        odd.sort_unstable_by_key(|&i| {
            let n = tree.node(i);
            std::cmp::Reverse(n.alloc.saturating_sub(n.size))
        });
        for &i in odd.iter().take(15) {
            let n = tree.node(i);
            println!(
                "  odd: size {:.3} GB alloc {:.3} GB flags {:#x} {}",
                n.size as f64 / 1e9,
                n.alloc as f64 / 1e9,
                n.flags,
                tree.path_of(i).display()
            );
        }
    }
    if let Some(note) = &tree.note {
        println!("note: {note}");
    }
    for c in root.children().take(12) {
        let n = tree.node(c);
        println!(
            "  {:>10.2} GB  {:>10.2} GB on disk  {}",
            n.size as f64 / 1e9,
            n.alloc as f64 / 1e9,
            tree.name(c)
        );
    }

    let started = Instant::now();
    let found = space::clean::suggest(&tree);
    println!(
        "{} clean-up suggestions in {:.2?}",
        found.len(),
        started.elapsed()
    );
    for c in &found {
        println!(
            "  {:>10.2} GB  {:<15} {} ({} places, first {})",
            c.size as f64 / 1e9,
            c.level.label(),
            c.title,
            c.nodes.len(),
            tree.path_of(c.nodes[0]).display()
        );
    }

    let started = Instant::now();
    let ext = tree.by_extension();
    println!("{} extensions in {:.2?}", ext.len(), started.elapsed());

    let style = treemap::Style::default();
    let palette: [[u8; 3]; 6] = [
        [70, 130, 200],
        [200, 120, 60],
        [90, 170, 100],
        [180, 80, 160],
        [200, 180, 70],
        [80, 170, 170],
    ];
    let color = |c: &treemap::Cell| -> [u8; 3] {
        match c.kind {
            treemap::Kind::File => palette[tree.node(c.node).ext as usize % 6],
            _ => [110, 110, 110],
        }
    };
    for round in 0..3 {
        let started = Instant::now();
        let map = treemap::render(&tree, ROOT, w, h, &style, &color, &|| false).unwrap();
        println!(
            "treemap {w}x{h} round {round}: {} cells in {:.2?}",
            map.cells.len(),
            started.elapsed()
        );
        if round == 0
            && let Some(file) = std::env::var_os("SPACE_PPM")
        {
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in &map.pixels {
                out.extend_from_slice(&p[..3]);
            }
            std::fs::write(file, out).unwrap();
        }
    }
}
