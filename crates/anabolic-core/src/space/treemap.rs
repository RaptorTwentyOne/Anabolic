// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The treemap: a folder's rectangle split among its children by size
//! (squarified, so the pieces stay close to squares), drawn with the
//! cushion shading WinDirStat made familiar, which shows the nesting
//! without drawing lines.
//!
//! The work is bounded by the picture, not by the drive: a folder smaller
//! than a few pixels is not split, and children too small to show are
//! drawn together as one piece. Laying out and drawing happen on worker
//! threads, in bands of rows; the window only shows the finished image and
//! looks up what is under the mouse in [`Map::hit`], one index per pixel.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{NONE, Tree};

/// A folder narrower or lower than this is not split.
const MIN_SIDE: f32 = 2.0;
/// Children smaller than this many pixels are drawn together.
const MIN_AREA: f64 = 1.5;
/// Rows per band of drawing work.
const BAND: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A folder split among its children (hidden under them).
    Folder,
    File,
    /// A folder too small to split.
    SmallFolder,
    /// A folder's children too small to show one by one.
    Rest,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn w(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn h(&self) -> f32 {
        self.y1 - self.y0
    }

    fn area(&self) -> f64 {
        f64::from(self.w()) * f64::from(self.h())
    }
}

/// One rectangle of the map.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub rect: Rect,
    pub node: u32,
    /// The cell of the folder it is in; NONE for the top one.
    pub parent: u32,
    pub kind: Kind,
    /// For a piece of small files: how many of the folder's children were
    /// drawn on their own before it.
    pub skip: u32,
    surface: [f32; 4],
}

/// How the map is drawn.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// Cushion shading; without it, flat pieces with a darker edge.
    pub cushion: bool,
    /// The height of the top cushion, and how much each level down keeps.
    pub height: f32,
    pub scale: f32,
    /// Light that reaches every pixel, of 1.
    pub ambient: f32,
    /// How much the shading brightens the colours at the top of a cushion.
    pub brightness: f32,
    /// Where nothing is drawn.
    pub background: [u8; 3],
}

impl Default for Style {
    fn default() -> Self {
        Style {
            cushion: true,
            height: 0.38,
            scale: 0.91,
            ambient: 0.15,
            brightness: 1.3,
            background: [16, 18, 22],
        }
    }
}

/// A finished map.
pub struct Map {
    pub width: u32,
    pub height: u32,
    /// The node the map shows.
    pub root: u32,
    pub cells: Vec<Cell>,
    /// The cell under each pixel, row by row; NONE where nothing is drawn.
    pub hit: Vec<u32>,
    /// RGBA, opaque, row by row.
    pub pixels: Vec<[u8; 4]>,
    by_node: HashMap<u32, u32>,
}

impl Map {
    /// The cell under a pixel.
    pub fn cell_at(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let c = self.hit[(y * self.width + x) as usize];
        (c != NONE).then_some(c)
    }

    /// The cell that shows a node: its own, or that of the nearest folder
    /// above it that was drawn.
    pub fn cell_of(&self, tree: &Tree, node: u32) -> Option<u32> {
        let mut at = node;
        while at != NONE {
            if let Some(&c) = self.by_node.get(&at) {
                return Some(c);
            }
            if at == self.root {
                return None;
            }
            at = tree.parent(at);
        }
        None
    }
}

/// Lays out `root` in a `width` x `height` picture and draws it. `color`
/// gives each file's or piece's colour; `stop` is asked now and then, and
/// a true answer abandons the work (a newer picture is wanted).
pub fn render(
    tree: &Tree,
    root: u32,
    width: u32,
    height: u32,
    style: &Style,
    color: &(dyn Fn(&Cell) -> [u8; 3] + Sync),
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Map> {
    let cells = layout(tree, root, width as f32, height as f32, style, stop)?;
    let (pixels, hit) = draw(&cells, width as usize, height as usize, style, color, stop)?;
    let mut by_node = HashMap::with_capacity(cells.len());
    for (i, c) in cells.iter().enumerate() {
        if c.kind != Kind::Rest {
            by_node.insert(c.node, i as u32);
        }
    }
    Some(Map {
        width,
        height,
        root,
        cells,
        hit,
        pixels,
        by_node,
    })
}

/// WinDirStat's cushion: each level adds a ridge over its rectangle.
fn add_ridge(s: &mut [f32; 4], r: Rect, h: f32) {
    let (w, hh) = (r.w(), r.h());
    if w <= 0.0 || hh <= 0.0 {
        return;
    }
    let h4 = 4.0 * h;
    let wf = h4 / w;
    s[2] += wf * (r.x1 + r.x0);
    s[0] -= wf;
    let hf = h4 / hh;
    s[3] += hf * (r.y1 + r.y0);
    s[1] -= hf;
}

fn layout(
    tree: &Tree,
    root: u32,
    width: f32,
    height: f32,
    style: &Style,
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<Cell>> {
    let mut cells = Vec::new();
    let top = Rect {
        x0: 0.0,
        y0: 0.0,
        x1: width,
        y1: height,
    };
    let mut stack = vec![(root, top, [0f32; 4], 0i32, NONE)];
    let mut items: Vec<(u32, f64)> = Vec::new();
    let mut placed: Vec<(u32, Rect)> = Vec::new();
    while let Some((node, rect, mut surface, depth, parent)) = stack.pop() {
        if cells.len() % 4096 == 4095 && stop() {
            return None;
        }
        add_ridge(&mut surface, rect, style.height * style.scale.powi(depth));
        let n = &tree.node(node);
        let split = n.count > 0 && n.size > 0 && rect.w() >= MIN_SIDE && rect.h() >= MIN_SIDE;
        let me = cells.len() as u32;
        cells.push(Cell {
            rect,
            node,
            parent,
            kind: if split {
                Kind::Folder
            } else if n.is_dir() {
                Kind::SmallFolder
            } else {
                Kind::File
            },
            skip: 0,
            surface,
        });
        if !split {
            continue;
        }
        // The children too small to show go into one piece. The scan lays
        // them out largest first, but sizes that changed since (the change
        // journal, a deletion) can leave a small one before larger ones:
        // all are looked at, and those shown are put in order.
        let per_byte = rect.area() / n.size as f64;
        items.clear();
        let mut shown = 0u64;
        for c in n.children() {
            let size = tree.size(c);
            if size == 0 || (size as f64 * per_byte) < MIN_AREA {
                continue;
            }
            items.push((c, size as f64));
            shown += size;
        }
        items.sort_by(|a, b| b.1.total_cmp(&a.1));
        let rest = n.size.saturating_sub(shown);
        let skip = items.len() as u32;
        if rest > 0 {
            items.push((NONE, rest as f64));
        }
        placed.clear();
        squarify(&items, rect, &mut placed);
        let child_height = style.height * style.scale.powi(depth + 1);
        // Pushed in reverse so the largest is laid out first.
        for &(c, r) in placed.iter().rev() {
            if c == NONE {
                let mut s = surface;
                add_ridge(&mut s, r, child_height);
                cells.push(Cell {
                    rect: r,
                    node,
                    parent: me,
                    kind: Kind::Rest,
                    skip,
                    surface: s,
                });
            } else {
                stack.push((c, r, surface, depth + 1, me));
            }
        }
    }
    Some(cells)
}

/// The worst aspect ratio of a row of areas laid along a side of length
/// `side`.
fn worst(sum: f64, max: f64, min: f64, side: f64) -> f64 {
    let s2 = sum * sum;
    let w2 = side * side;
    (w2 * max / s2).max(s2 / (w2 * min))
}

/// Splits `rect` among `items` (largest first) in proportion to their
/// weights, rows along the shorter side growing while the pieces get
/// closer to squares (Bruls, Huizing and van Wijk).
pub fn squarify(items: &[(u32, f64)], rect: Rect, out: &mut Vec<(u32, Rect)>) {
    let total: f64 = items.iter().map(|i| i.1).sum();
    if total <= 0.0 || rect.area() <= 0.0 {
        return;
    }
    let k = rect.area() / total;
    let mut r = rect;
    let mut i = 0;
    while i < items.len() {
        let wide = r.w() >= r.h();
        let side = f64::from(if wide { r.h() } else { r.w() });
        if side <= 0.0 {
            break;
        }
        let (mut sum, mut max, mut min, mut ratio) = (0.0, 0.0f64, f64::INFINITY, f64::INFINITY);
        let mut j = i;
        while j < items.len() {
            let a = items[j].1 * k;
            let (s, hi, lo) = (sum + a, max.max(a), min.min(a));
            let w = worst(s, hi, lo, side);
            if j > i && w > ratio {
                break;
            }
            (sum, max, min, ratio) = (s, hi, lo, w);
            j += 1;
        }
        let last_row = j == items.len();
        let thick = (sum / side) as f32;
        if wide {
            // A column on the left.
            let x1 = if last_row {
                r.x1
            } else {
                (r.x0 + thick).min(r.x1)
            };
            let mut y = r.y0;
            for (t, &(id, weight)) in items[i..j].iter().enumerate() {
                let y1 = if i + t + 1 == j {
                    r.y1
                } else {
                    y + (weight * k / f64::from(thick.max(f32::MIN_POSITIVE))) as f32
                };
                out.push((
                    id,
                    Rect {
                        x0: r.x0,
                        y0: y,
                        x1,
                        y1,
                    },
                ));
                y = y1;
            }
            r.x0 = x1;
        } else {
            // A row along the top.
            let y1 = if last_row {
                r.y1
            } else {
                (r.y0 + thick).min(r.y1)
            };
            let mut x = r.x0;
            for (t, &(id, weight)) in items[i..j].iter().enumerate() {
                let x1 = if i + t + 1 == j {
                    r.x1
                } else {
                    x + (weight * k / f64::from(thick.max(f32::MIN_POSITIVE))) as f32
                };
                out.push((
                    id,
                    Rect {
                        x0: x,
                        y0: r.y0,
                        x1,
                        y1,
                    },
                ));
                x = x1;
            }
            r.y0 = y1;
        }
        i = j;
    }
}

/// Draws every leaf cell; returns the pixels and the cell under each.
fn draw(
    cells: &[Cell],
    w: usize,
    h: usize,
    style: &Style,
    color: &(dyn Fn(&Cell) -> [u8; 3] + Sync),
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<(Vec<[u8; 4]>, Vec<u32>)> {
    let bg = style.background;
    let mut pixels = vec![[bg[0], bg[1], bg[2], 255]; w * h];
    let mut hit = vec![NONE; w * h];
    if w == 0 || h == 0 {
        return Some((pixels, hit));
    }
    // Which leaves touch which band.
    let bands = h.div_ceil(BAND);
    let mut in_band: Vec<Vec<u32>> = vec![Vec::new(); bands];
    for (i, c) in cells.iter().enumerate() {
        if c.kind == Kind::Folder {
            continue;
        }
        let y0 = (c.rect.y0.round().max(0.0) as usize).min(h);
        let y1 = (c.rect.y1.round().max(0.0) as usize).min(h);
        if y0 >= y1 || c.rect.x1.round() <= c.rect.x0.round() {
            continue;
        }
        for band in in_band.iter_mut().take((y1 - 1) / BAND + 1).skip(y0 / BAND) {
            band.push(i as u32);
        }
    }

    let light = {
        let (x, y, z) = (-1.0f32, -1.0f32, 10.0f32);
        let len = (x * x + y * y + z * z).sqrt();
        [x / len, y / len, z / len]
    };
    let work: Vec<_> = pixels
        .chunks_mut(BAND * w)
        .zip(hit.chunks_mut(BAND * w))
        .enumerate()
        .collect();
    let work = Mutex::new(work);
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    std::thread::scope(|s| {
        for _ in 0..threads.min(bands) {
            s.spawn(|| {
                loop {
                    let Some((band, (px, ids))) = work.lock().unwrap().pop() else {
                        break;
                    };
                    if stop() {
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    let top = band * BAND;
                    let rows = px.len() / w;
                    for &i in &in_band[band] {
                        let c = &cells[i as usize];
                        let x0 = (c.rect.x0.round().max(0.0) as usize).min(w);
                        let x1 = (c.rect.x1.round().max(0.0) as usize).min(w);
                        // The piece's own top, for its edge: y0 is where
                        // this band starts drawing it.
                        let edge = c.rect.y0.round().max(0.0) as usize;
                        let y0 = edge.clamp(top, top + rows);
                        let y1 = (c.rect.y1.round().max(0.0) as usize).clamp(top, top + rows);
                        if x0 >= x1 || y0 >= y1 {
                            continue;
                        }
                        let base = color(c).map(f32::from);
                        let sf = c.surface;
                        for y in y0..y1 {
                            let row = (y - top) * w;
                            let ny = -(2.0 * sf[1] * (y as f32 + 0.5) + sf[3]);
                            for x in x0..x1 {
                                let b = if style.cushion {
                                    let nx = -(2.0 * sf[0] * (x as f32 + 0.5) + sf[2]);
                                    let cos = (nx * light[0] + ny * light[1] + light[2])
                                        / (nx * nx + ny * ny + 1.0).sqrt();
                                    (style.ambient + (1.0 - style.ambient) * cos.max(0.0))
                                        * style.brightness
                                } else if x == x0 || y == edge {
                                    0.6
                                } else {
                                    1.0
                                };
                                let p = &mut px[row + x];
                                *p = [
                                    (base[0] * b).min(255.0) as u8,
                                    (base[1] * b).min(255.0) as u8,
                                    (base[2] * b).min(255.0) as u8,
                                    255,
                                ];
                                ids[row + x] = i;
                            }
                        }
                    }
                }
            });
        }
    });
    if stopped.into_inner() {
        return None;
    }
    Some((pixels, hit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::{Builder, DIR, Entry, Method, Progress, ROOT};

    fn overlap(a: Rect, b: Rect) -> f32 {
        let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
        let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
        w * h
    }

    #[test]
    fn squarify_fills_the_rectangle_in_proportion() {
        let items: Vec<(u32, f64)> = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0]
            .iter()
            .enumerate()
            .map(|(i, &w)| (i as u32, w))
            .collect();
        let rect = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 6.0,
            y1: 4.0,
        };
        let mut out = Vec::new();
        squarify(&items, rect, &mut out);
        assert_eq!(out.len(), items.len());
        for (&(id, r), &(_, weight)) in out.iter().zip(&items) {
            assert!((r.area() - weight).abs() < 1e-3, "{id}: {r:?}");
            assert!(r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= 6.0 + 1e-4 && r.y1 <= 4.0 + 1e-4);
        }
        for (i, a) in out.iter().enumerate() {
            for b in &out[i + 1..] {
                assert!(overlap(a.1, b.1) < 1e-3, "{a:?} {b:?}");
            }
        }
    }

    fn sample() -> Tree {
        let mut b = Builder::new(
            "root",
            Entry {
                flags: DIR,
                ..Entry::default()
            },
        );
        let games = b.push(
            ROOT,
            "Games",
            Entry {
                flags: DIR,
                ..Entry::default()
            },
        );
        b.push(
            games,
            "big.pak",
            Entry {
                size: 6000,
                ..Entry::default()
            },
        );
        b.push(
            games,
            "small.pak",
            Entry {
                size: 1000,
                ..Entry::default()
            },
        );
        b.push(
            ROOT,
            "movie.mkv",
            Entry {
                size: 3000,
                ..Entry::default()
            },
        );
        // Many tiny files that only show as one piece together.
        for i in 0..2000 {
            b.push(
                ROOT,
                &format!("t{i}.log"),
                Entry {
                    size: 1,
                    ..Entry::default()
                },
            );
        }
        Tree::assemble(
            b.finish(&Progress::default()),
            std::path::Path::new("R"),
            Method::Walk,
        )
    }

    #[test]
    fn draws_every_pixel_and_finds_what_is_under_it() {
        let tree = sample();
        let style = Style::default();
        let map = render(&tree, ROOT, 160, 90, &style, &|_| [200, 100, 50], &|| false).unwrap();
        assert!(
            map.hit.iter().all(|&c| c != NONE),
            "the whole picture is covered"
        );
        let mut count: HashMap<u32, usize> = HashMap::new();
        for &c in &map.hit {
            *count.entry(map.cells[c as usize].node).or_default() += 1;
        }
        let games = tree.node(ROOT).first;
        let big = tree.node(games).first;
        let movie = games + 1;
        assert!(
            count[&big] > count[&movie],
            "the largest file gets the most pixels"
        );
        let share = count[&big] as f64 / map.hit.len() as f64;
        assert!((share - 6000.0 / 12000.0).abs() < 0.03, "{share}");
        let rest = map.cells.iter().filter(|c| c.kind == Kind::Rest).count();
        assert_eq!(rest, 1, "2000 one-byte files are one piece");
        assert_eq!(
            map.cell_of(&tree, big).map(|c| map.cells[c as usize].node),
            Some(big)
        );
        // A tiny file falls back to its folder's cell.
        let tiny = tree.node(ROOT).children().last().unwrap();
        assert_eq!(
            map.cell_of(&tree, tiny).map(|c| map.cells[c as usize].node),
            Some(ROOT)
        );
        // Cushions: the middle of a piece is brighter than its edge.
        let c = map.cell_of(&tree, big).unwrap();
        let r = map.cells[c as usize].rect;
        let at = |x: f32, y: f32| map.pixels[(y as u32 * 160 + x as u32) as usize][0];
        assert!(at((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0) > at(r.x1 - 1.0, r.y1 - 1.0));
    }

    #[test]
    fn a_child_that_shrank_since_the_scan_hides_none_after_it() {
        // Games was the largest; the change journal shrinks it to a byte
        // and grows the movie, and Games stays first among the children,
        // too small to show.
        let mut tree = sample();
        let games = tree.node(ROOT).first;
        let big = tree.node(games).first;
        let movie = games + 1;
        tree.resize(big, 1, 1);
        tree.resize(big + 1, 0, 0);
        tree.resize(movie, 100_000, 100_000);
        let style = Style::default();
        let map = render(&tree, ROOT, 160, 90, &style, &|_| [200, 100, 50], &|| false).unwrap();
        assert_eq!(
            map.cell_of(&tree, movie)
                .map(|c| map.cells[c as usize].node),
            Some(movie),
            "the movie is a piece of its own, not lost among the small files"
        );
    }

    #[test]
    fn flat_pieces_have_an_edge_only_at_their_own_top() {
        let tree = sample();
        let style = Style {
            cushion: false,
            ..Style::default()
        };
        let map = render(&tree, ROOT, 160, 90, &style, &|_| [200, 100, 50], &|| false).unwrap();
        let big = tree.node(tree.node(ROOT).first).first;
        let r = map.cells[map.cell_of(&tree, big).unwrap() as usize].rect;
        assert!(r.y1 - r.y0 > BAND as f32, "the piece crosses a band");
        let x = ((r.x0 + r.x1) / 2.0) as usize;
        let at = |y: f32| map.pixels[y.round() as usize * 160 + x][0];
        assert!(at(r.y0) < at(r.y0 + 1.0), "its top edge is darker");
        for band in (BAND..90).step_by(BAND) {
            let y = band as f32;
            if y > r.y0.round() + 1.0 && y < r.y1.round() {
                assert_eq!(at(y), at(y - 1.0), "no line where band {band} starts");
            }
        }
    }

    #[test]
    fn stops_when_asked() {
        let tree = sample();
        assert!(
            render(
                &tree,
                ROOT,
                400,
                300,
                &Style::default(),
                &|_| [1, 2, 3],
                &|| true
            )
            .is_none()
        );
    }
}
