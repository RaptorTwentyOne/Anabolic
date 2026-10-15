//! Lightweight history graphs painted straight onto egui's painter. Never draws
//! more points than the graph has pixels.

use std::sync::atomic::{AtomicUsize, Ordering};

use anabolic_core::history::Ring;
use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::theme;

pub struct Series<'a> {
    pub ring: &'a Ring,
    pub color: Color32,
    pub fill: bool,
}

pub struct Graph<'a> {
    pub series: Vec<Series<'a>>,
    /// Fixed top of the scale; `None` scales to the largest visible value.
    pub max: Option<f32>,
    pub grid: bool,
}

impl<'a> Graph<'a> {
    pub fn new(max: Option<f32>) -> Self {
        Self {
            series: Vec::new(),
            max,
            grid: true,
        }
    }

    pub fn series(mut self, ring: &'a Ring, color: Color32, fill: bool) -> Self {
        self.series.push(Series { ring, color, fill });
        self
    }

    pub fn no_grid(mut self) -> Self {
        self.grid = false;
        self
    }

    /// Paints into a rect of the given size and returns its response
    /// together with the scale maximum actually used.
    pub fn show(self, ui: &mut Ui, size: Vec2) -> (egui::Response, f32) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let max = self.paint(ui, rect);
        (response, max)
    }

    pub fn paint(&self, ui: &Ui, rect: Rect) -> f32 {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, theme::BG);

        let visible = self
            .series
            .first()
            .map_or(2, |s| visible_samples(s.ring, rect.width()));
        let max = self.max.unwrap_or_else(|| {
            let m = self
                .series
                .iter()
                .map(|s| tail_max(s.ring, visible))
                .fold(0.0, f32::max);
            nice_ceiling(m)
        });

        if self.grid {
            let grid = Stroke::new(1.0, theme::GRID);
            for i in 1..4 {
                let y = rect.top() + rect.height() * i as f32 / 4.0;
                painter.hline(rect.x_range(), y, grid);
            }
            let columns = (rect.width() / 60.0).floor().max(1.0) as usize;
            for i in 1..columns {
                let x = rect.left() + rect.width() * i as f32 / columns as f32;
                painter.vline(x, rect.y_range(), grid);
            }
        }

        for s in &self.series {
            let points = points(s.ring, rect, visible, max);
            if points.len() < 2 {
                continue;
            }
            if s.fill {
                painter.add(Shape::mesh(area_mesh(
                    &points,
                    rect.bottom(),
                    s.color.gamma_multiply(0.22),
                )));
            }
            painter.add(Shape::line(points, Stroke::new(1.2, s.color)));
        }
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, theme::BORDER),
            StrokeKind::Inside,
        );
        max
    }
}

/// Samples every graph spans when the user picked a fixed window (new
/// samples flow in from the right); 0 fits the span to the history instead.
static WINDOW: AtomicUsize = AtomicUsize::new(0);

pub fn set_window(samples: usize) {
    WINDOW.store(samples, Ordering::Relaxed);
}

/// How many samples a graph `width` wide shows: at most one per pixel and
/// the ring's capacity; then either the fixed window, or while the ring is
/// still filling, what it holds (at least a minute's worth, so the first
/// seconds do not fill the width).
pub fn visible_samples(ring: &Ring, width: f32) -> usize {
    let most = ring.capacity().min(width.max(2.0) as usize);
    match WINDOW.load(Ordering::Relaxed) {
        0 => most.min(ring.len().max(60)),
        window => most.min(window),
    }
    .max(2)
}

fn tail_max(ring: &Ring, n: usize) -> f32 {
    let skip = ring.len().saturating_sub(n);
    ring.iter().skip(skip).fold(0.0, f32::max)
}

/// Newest sample sits on the right edge; older ones scroll left.
fn points(ring: &Ring, rect: Rect, visible: usize, max: f32) -> Vec<Pos2> {
    let n = ring.len().min(visible);
    let skip = ring.len() - n;
    let dx = rect.width() / (visible - 1).max(1) as f32;
    let scale = if max > 0.0 { rect.height() / max } else { 0.0 };
    ring.iter()
        .skip(skip)
        .enumerate()
        .map(|(i, v)| {
            let x = rect.right() - (n - 1 - i) as f32 * dx;
            let y = rect.bottom() - (v.max(0.0) * scale).min(rect.height());
            Pos2::new(x, y)
        })
        .collect()
}

/// The area under a polyline is not convex, so build it from quads.
fn area_mesh(points: &[Pos2], bottom: f32, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(points.len() * 2);
    mesh.reserve_triangles((points.len() - 1) * 2);
    for p in points {
        mesh.colored_vertex(*p, color);
        mesh.colored_vertex(Pos2::new(p.x, bottom), color);
    }
    for i in 0..points.len() as u32 - 1 {
        let (top, base, next_top, next_base) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        mesh.add_triangle(top, base, next_top);
        mesh.add_triangle(next_top, base, next_base);
    }
    mesh
}

/// Rounds a maximum up to 1, 2 or 5 times a power of ten so the scale does
/// not jitter with every sample.
pub fn nice_ceiling(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let base = 10f32.powf(exp);
    for m in [1.0, 2.0, 5.0, 10.0] {
        if v <= m * base {
            return m * base;
        }
    }
    10.0 * base
}

#[cfg(test)]
mod tests {
    use super::{nice_ceiling, set_window, visible_samples};
    use anabolic_core::history::Ring;

    #[test]
    fn fitted_span_grows_fixed_window_stays() {
        let mut ring = Ring::new(600);
        for _ in 0..200 {
            ring.push(1.0);
        }
        // The only test that touches the window; set back before returning.
        set_window(0);
        assert_eq!(visible_samples(&ring, 1000.0), 200);
        set_window(60);
        assert_eq!(visible_samples(&ring, 1000.0), 60);
        // Never more than one sample per pixel or than the ring holds.
        assert_eq!(visible_samples(&ring, 40.0), 40);
        set_window(10_000);
        assert_eq!(visible_samples(&ring, 1000.0), 600);
        set_window(0);
    }

    #[test]
    fn ceilings() {
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(3.0), 5.0);
        assert_eq!(nice_ceiling(12.0), 20.0);
        assert_eq!(nice_ceiling(700.0), 1000.0);
    }
}
