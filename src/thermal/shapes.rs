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

/// Which endpoint coordinates follow the pointer while a shape is being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grab {
    pub ax: bool,
    pub ay: bool,
    pub bx: bool,
    pub by: bool,
}

impl Grab {
    pub const MOVE: Grab = Grab {
        ax: true,
        ay: true,
        bx: true,
        by: true,
    };
    pub const END_A: Grab = Grab {
        ax: true,
        ay: true,
        bx: false,
        by: false,
    };
    pub const END_B: Grab = Grab {
        ax: false,
        ay: false,
        bx: true,
        by: true,
    };
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
        if f.width == 0 || f.height == 0 {
            return None;
        }
        let mut s = ShapeStats {
            min: f32::MAX,
            max: f32::MIN,
            mean: 0.0,
            min_pos: (0, 0),
            max_pos: (0, 0),
            pixels: 0,
        };
        let mut sum = 0.0f64;
        let mut add = |x: usize, y: usize| {
            let v = f.value(f.at(x, y));
            sum += v as f64;
            s.pixels += 1;
            if v < s.min {
                s.min = v;
                s.min_pos = (x, y);
            }
            if v > s.max {
                s.max = v;
                s.max_pos = (x, y);
            }
        };
        match self.kind {
            // Rows directly: this runs for every frame, and a pixel list of a large box is
            // hundreds of kilobytes.
            ShapeKind::Box => {
                let clamp = |(x, y): (usize, usize)| (x.min(f.width - 1), y.min(f.height - 1));
                let (a, b) = (clamp(self.a), clamp(self.b));
                for y in a.1.min(b.1)..=a.1.max(b.1) {
                    for x in a.0.min(b.0)..=a.0.max(b.0) {
                        add(x, y);
                    }
                }
            }
            ShapeKind::Line => {
                for (x, y) in self.pixels(f.width, f.height) {
                    add(x, y);
                }
            }
        }
        if s.pixels == 0 {
            return None;
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

    /// Box with `a` top-left and `b` bottom-right; lines are unchanged (their direction matters).
    pub fn normalized(&self) -> Shape {
        match self.kind {
            ShapeKind::Line => *self,
            ShapeKind::Box => Shape {
                a: (self.a.0.min(self.b.0), self.a.1.min(self.b.1)),
                b: (self.a.0.max(self.b.0), self.a.1.max(self.b.1)),
                ..*self
            },
        }
    }

    /// What a drag starting at (x, y) would edit. Line: an endpoint, or the whole line. Box: a
    /// corner or edge, or (with `interior`) the whole box when grabbed inside.
    pub fn grab_at(&self, x: f32, y: f32, tol: f32, interior: bool) -> Option<Grab> {
        let s = self.normalized();
        match s.kind {
            ShapeKind::Line => {
                let (ax, ay) = (s.a.0 as f32 + 0.5, s.a.1 as f32 + 0.5);
                let (bx, by) = (s.b.0 as f32 + 0.5, s.b.1 as f32 + 0.5);
                if (x - ax).hypot(y - ay) <= tol {
                    Some(Grab::END_A)
                } else if (x - bx).hypot(y - by) <= tol {
                    Some(Grab::END_B)
                } else if seg_dist(x, y, ax, ay, bx, by) <= tol {
                    Some(Grab::MOVE)
                } else {
                    None
                }
            }
            ShapeKind::Box => {
                // The outline runs along pixel edges.
                let (x0, y0) = (s.a.0 as f32, s.a.1 as f32);
                let (x1, y1) = (s.b.0 as f32 + 1.0, s.b.1 as f32 + 1.0);
                if x < x0 - tol || x > x1 + tol || y < y0 - tol || y > y1 + tol {
                    return None;
                }
                let (left, top) = ((x - x0).abs() <= tol, (y - y0).abs() <= tol);
                let (right, bottom) = ((x - x1).abs() <= tol, (y - y1).abs() <= tol);
                if left || right || top || bottom {
                    Some(Grab {
                        ax: left,
                        ay: top,
                        bx: right && !left,
                        by: bottom && !top,
                    })
                } else {
                    interior.then_some(Grab::MOVE)
                }
            }
        }
    }

    /// This shape after dragging by (dx, dy) pixels, kept inside a w×h frame. A whole-shape move
    /// stops at the frame edge instead of squashing the shape.
    pub fn dragged(&self, grab: Grab, dx: i64, dy: i64, w: usize, h: usize) -> Shape {
        let (max_x, max_y) = (w.saturating_sub(1) as i64, h.saturating_sub(1) as i64);
        // A shape kept from a larger image (another camera, a turned recording) may reach
        // outside this one: bring it in first, or the limits below would cross.
        let inside = |(x, y): (usize, usize)| (x.min(max_x as usize), y.min(max_y as usize));
        let this = Shape {
            a: inside(self.a),
            b: inside(self.b),
            ..*self
        };
        let this = &this;
        let (mut dx, mut dy) = (dx, dy);
        if grab == Grab::MOVE {
            let (x0, x1) = (this.a.0.min(this.b.0) as i64, this.a.0.max(this.b.0) as i64);
            let (y0, y1) = (this.a.1.min(this.b.1) as i64, this.a.1.max(this.b.1) as i64);
            dx = dx.clamp(-x0, max_x - x1);
            dy = dy.clamp(-y0, max_y - y1);
        }
        let shift = |v: usize, on: bool, d: i64, max: i64| {
            if on {
                (v as i64 + d).clamp(0, max) as usize
            } else {
                v
            }
        };
        Shape {
            a: (
                shift(this.a.0, grab.ax, dx, max_x),
                shift(this.a.1, grab.ay, dy, max_y),
            ),
            b: (
                shift(this.b.0, grab.bx, dx, max_x),
                shift(this.b.1, grab.by, dy, max_y),
            ),
            ..*self
        }
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

    /// Topmost (newest) shape a drag at (x, y) would edit, normalized, with what it grabs.
    pub fn grab(&self, x: f32, y: f32, tol: f32, interior: bool) -> Option<(Shape, Grab)> {
        self.items
            .iter()
            .rev()
            .find_map(|s| s.grab_at(x, y, tol, interior).map(|g| (s.normalized(), g)))
    }

    /// Replace the shape with the same id.
    pub fn update(&mut self, shape: Shape) {
        if let Some(s) = self.items.iter_mut().find(|s| s.id == shape.id) {
            *s = shape;
        }
    }

    /// Nearest shape within `max_dist` pixels of (x, y). On a tie (a point inside overlapping
    /// boxes) the newest wins, the same one a drag would grab.
    pub fn hit(&self, x: f32, y: f32, max_dist: f32) -> Option<u32> {
        self.items
            .iter()
            .rev()
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
    fn grabbing_a_box() {
        let b = Shape {
            id: 1,
            kind: ShapeKind::Box,
            a: (30, 40),
            b: (10, 20), // drawn right-to-left
        };
        // top-left corner of the normalized box
        assert_eq!(
            b.grab_at(10.0, 20.0, 1.0, false),
            Some(Grab {
                ax: true,
                ay: true,
                bx: false,
                by: false
            })
        );
        // right edge only (outline at x = 31)
        assert_eq!(
            b.grab_at(31.0, 30.0, 1.0, false),
            Some(Grab {
                ax: false,
                ay: false,
                bx: true,
                by: false
            })
        );
        // inside: only moves when allowed
        assert_eq!(b.grab_at(20.0, 30.0, 1.0, false), None);
        assert_eq!(b.grab_at(20.0, 30.0, 1.0, true), Some(Grab::MOVE));
        assert_eq!(b.grab_at(50.0, 30.0, 1.0, true), None);
    }

    #[test]
    fn dragging_keeps_shapes_in_frame() {
        let b = Shape {
            id: 1,
            kind: ShapeKind::Box,
            a: (10, 10),
            b: (20, 15),
        };
        // a whole-box move stops at the edge and keeps its size
        let m = b.dragged(Grab::MOVE, 500, -500, 160, 120);
        assert_eq!((m.a, m.b), ((149, 0), (159, 5)));
        let l = Shape {
            id: 2,
            kind: ShapeKind::Line,
            a: (5, 5),
            b: (50, 60),
        };
        // moving one end leaves the other alone and clamps
        let e = l.dragged(Grab::END_B, 500, 3, 160, 120);
        assert_eq!((e.a, e.b), ((5, 5), (159, 63)));
    }

    /// A shape drawn on a larger image (P3, 256×192) and then moved on a smaller one (Lepton,
    /// or a turned recording): it comes inside instead of panicking.
    #[test]
    fn dragging_a_shape_larger_than_the_frame() {
        let l = Shape {
            id: 1,
            kind: ShapeKind::Line,
            a: (10, 10),
            b: (250, 180),
        };
        let m = l.dragged(Grab::MOVE, 3, 3, 160, 120);
        assert_eq!((m.a, m.b), ((10, 10), (159, 119)));
        let m = l.dragged(Grab::MOVE, -3, -4, 160, 120);
        assert_eq!((m.a, m.b), ((7, 6), (156, 115)));
        let b = Shape {
            id: 2,
            kind: ShapeKind::Box,
            a: (200, 150),
            b: (255, 191),
        };
        let m = b.dragged(Grab::MOVE, -5, 0, 160, 120);
        assert_eq!((m.a, m.b), ((154, 119), (154, 119)));
        let st = b.stats(&ramp()).unwrap();
        assert_eq!((st.pixels, st.max_pos), (1, (3, 2)));
    }

    #[test]
    fn line_grab_prefers_endpoints() {
        let l = Shape {
            id: 1,
            kind: ShapeKind::Line,
            a: (0, 0),
            b: (100, 0),
        };
        assert_eq!(l.grab_at(0.5, 0.5, 2.0, false), Some(Grab::END_A));
        assert_eq!(l.grab_at(100.0, 1.0, 2.0, false), Some(Grab::END_B));
        assert_eq!(l.grab_at(50.0, 1.0, 2.0, false), Some(Grab::MOVE));
        assert_eq!(l.grab_at(50.0, 9.0, 2.0, false), None);
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
