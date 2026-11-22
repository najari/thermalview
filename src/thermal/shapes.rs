//! Area (Box) and Line measurements.

use serde::{Deserialize, Serialize};

use super::frame::ThermalFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeKind {
    Box,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    pub id: u32,
    pub kind: ShapeKind,
    pub a: (usize, usize),
    pub b: (usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeStats {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub min_pos: (usize, usize),
    pub max_pos: (usize, usize),
    pub pixels: usize,
}

impl Shape {
    pub fn label(&self) -> String {
        match self.kind {
            ShapeKind::Box => format!("B{}", self.id),
            ShapeKind::Line => format!("L{}", self.id),
        }
    }

    /// Pixels covered: every pixel of the rectangle, or the Bresenham line from a to b.
    pub fn pixels(&self, w: usize, h: usize) -> Vec<(usize, usize)> {
        let clamp =
            |(x, y): (usize, usize)| (x.min(w.saturating_sub(1)), y.min(h.saturating_sub(1)));
        let (a, b) = (clamp(self.a), clamp(self.b));
        match self.kind {
            ShapeKind::Box => {
                let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
                let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
                (y0..=y1)
                    .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
                    .collect()
            }
            ShapeKind::Line => bresenham(a, b),
        }
    }

    pub fn stats(&self, f: &ThermalFrame) -> Option<ShapeStats> {
        let px = self.pixels(f.width, f.height);
        let first = *px.first()?;
        let mut s = ShapeStats {
            min: f32::MAX,
            max: f32::MIN,
            mean: 0.0,
            min_pos: first,
            max_pos: first,
            pixels: px.len(),
        };
        let mut sum = 0.0f64;
        for (x, y) in px {
            let v = f.value(f.at(x, y));
            sum += v as f64;
            if v < s.min {
                s.min = v;
                s.min_pos = (x, y);
            }
            if v > s.max {
                s.max = v;
                s.max_pos = (x, y);
            }
        }
        s.mean = (sum / s.pixels as f64) as f32;
        Some(s)
    }

    /// Values along a line (distance in pixels from `a`, value). Empty for boxes.
    pub fn profile(&self, f: &ThermalFrame) -> Vec<[f64; 2]> {
        if self.kind != ShapeKind::Line {
            return Vec::new();
        }
        let px = self.pixels(f.width, f.height);
        let Some(&(ax, ay)) = px.first() else {
            return Vec::new();
        };
        px.iter()
            .map(|&(x, y)| {
                let d = ((x as f64 - ax as f64).powi(2) + (y as f64 - ay as f64).powi(2)).sqrt();
                [d, f.value(f.at(x, y)) as f64]
            })
            .collect()
    }

    /// Distance (pixels) from point (x, y) to the shape outline / segment.
    pub fn distance(&self, x: f32, y: f32) -> f32 {
        let (ax, ay) = (self.a.0 as f32 + 0.5, self.a.1 as f32 + 0.5);
        let (bx, by) = (self.b.0 as f32 + 0.5, self.b.1 as f32 + 0.5);
        match self.kind {
            ShapeKind::Line => seg_dist(x, y, ax, ay, bx, by),
            ShapeKind::Box => {
                let (x0, x1) = (ax.min(bx), ax.max(bx));
                let (y0, y1) = (ay.min(by), ay.max(by));
                if (x0..=x1).contains(&x) && (y0..=y1).contains(&y) {
                    0.0
                } else {
                    let dx = (x0 - x).max(x - x1).max(0.0);
                    let dy = (y0 - y).max(y - y1).max(0.0);
                    (dx * dx + dy * dy).sqrt()
                }
            }
        }
    }
}

fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

fn bresenham(a: (usize, usize), b: (usize, usize)) -> Vec<(usize, usize)> {
    let (mut x0, mut y0) = (a.0 as i64, a.1 as i64);
    let (x1, y1) = (b.0 as i64, b.1 as i64);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut out = Vec::new();
    loop {
        out.push((x0 as usize, y0 as usize));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Shapes {
    pub items: Vec<Shape>,
    next_id: u32,
}

impl Shapes {
    pub const MAX: usize = 8;

    pub fn add(&mut self, kind: ShapeKind, a: (usize, usize), b: (usize, usize)) -> Option<u32> {
        if self.items.len() >= Self::MAX {
            return None;
        }
        self.next_id += 1;
        self.items.push(Shape {
            id: self.next_id,
            kind,
            a,
            b,
        });
        Some(self.next_id)
    }

    pub fn remove(&mut self, id: u32) {
        self.items.retain(|s| s.id != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 0;
    }

    /// Nearest shape within `max_dist` pixels of (x, y).
    pub fn hit(&self, x: f32, y: f32, max_dist: f32) -> Option<u32> {
        self.items
            .iter()
            .map(|s| (s.id, s.distance(x, y)))
            .filter(|(_, d)| *d <= max_dist)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> ThermalFrame {
        // 4×3, value(x, y) = 10*y + x °C
        let raw = (0..3)
            .flat_map(|y| {
                (0..4).map(move |x| (((10 * y + x) as f32 + 273.15) * 100.0).round() as u16)
            })
            .collect();
        ThermalFrame::new(4, 3, raw, Some(0.01))
    }

    #[test]
    fn box_stats() {
        let f = ramp();
        let s = Shape {
            id: 1,
            kind: ShapeKind::Box,
            a: (2, 2),
            b: (1, 1),
        };
        let st = s.stats(&f).unwrap();
        assert_eq!(st.pixels, 4);
        assert!((st.min - 11.0).abs() < 1e-3 && st.min_pos == (1, 1));
        assert!((st.max - 22.0).abs() < 1e-3 && st.max_pos == (2, 2));
        assert!((st.mean - 16.5).abs() < 1e-3);
    }

    #[test]
    fn line_profile_and_clamping() {
        let f = ramp();
        let s = Shape {
            id: 1,
            kind: ShapeKind::Line,
            a: (0, 0),
            b: (9, 0), // clamped to x = 3
        };
        let p = s.profile(&f);
        assert_eq!(p.len(), 4);
        assert_eq!(p[3][0], 3.0);
        assert!((p[3][1] - 3.0).abs() < 1e-3);
    }

    #[test]
    fn bresenham_diagonal() {
        assert_eq!(bresenham((0, 0), (2, 2)), vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(bresenham((2, 0), (0, 1)).len(), 3);
    }

    #[test]
    fn hit_test() {
        let mut s = Shapes::default();
        let b = s.add(ShapeKind::Box, (10, 10), (20, 20)).unwrap();
        let l = s.add(ShapeKind::Line, (0, 50), (100, 50)).unwrap();
        assert_eq!(s.hit(15.0, 15.0, 2.0), Some(b));
        assert_eq!(s.hit(40.0, 51.0, 2.0), Some(l));
        assert_eq!(s.hit(40.0, 40.0, 2.0), None);
    }
}
