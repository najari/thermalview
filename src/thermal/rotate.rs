//! Rotating the thermal image in 90° steps, applied to live frames as they arrive so that the
//! view, measurements, recordings and snapshots all use the turned image.

use serde::{Deserialize, Serialize};

use super::frame::ThermalFrame;

/// Clockwise rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub const ALL: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];

    pub fn label(self) -> &'static str {
        match self {
            Rotation::R0 => "회전 없음",
            Rotation::R90 => "시계 방향 90°",
            Rotation::R180 => "180°",
            Rotation::R270 => "반시계 방향 90°",
        }
    }

    fn quarters(self) -> usize {
        self as usize
    }

    fn from_quarters(q: usize) -> Self {
        Self::ALL[q % 4]
    }

    /// The rotation that turns an image already rotated by `from` into one rotated by `self`.
    pub fn since(self, from: Rotation) -> Rotation {
        Self::from_quarters(self.quarters() + 4 - from.quarters())
    }

    /// Size of a w × h image after rotating.
    pub fn size(self, (w, h): (usize, usize)) -> (usize, usize) {
        match self {
            Rotation::R0 | Rotation::R180 => (w, h),
            Rotation::R90 | Rotation::R270 => (h, w),
        }
    }

    /// Where pixel (x, y) of a w × h image ends up.
    pub fn point(self, (x, y): (usize, usize), (w, h): (usize, usize)) -> (usize, usize) {
        let (x, y) = (x.min(w.saturating_sub(1)), y.min(h.saturating_sub(1)));
        match self {
            Rotation::R0 => (x, y),
            Rotation::R90 => (h - 1 - y, x),
            Rotation::R180 => (w - 1 - x, h - 1 - y),
            Rotation::R270 => (y, w - 1 - x),
        }
    }

    /// The frame turned by this rotation (everything but the pixels and size is kept).
    pub fn apply(self, f: ThermalFrame) -> ThermalFrame {
        if self == Rotation::R0 || f.raw.len() != f.width * f.height {
            return f;
        }
        let size = (f.width, f.height);
        let (nw, nh) = self.size(size);
        let mut raw = vec![0u16; f.raw.len()];
        for y in 0..f.height {
            for x in 0..f.width {
                let (nx, ny) = self.point((x, y), size);
                raw[ny * nw + nx] = f.raw[y * f.width + x];
            }
        }
        ThermalFrame {
            width: nw,
            height: nh,
            raw,
            ..f
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3 × 2 image:
    /// 1 2 3
    /// 4 5 6
    fn frame() -> ThermalFrame {
        ThermalFrame::new(3, 2, vec![1, 2, 3, 4, 5, 6], Some(0.01))
    }

    #[test]
    fn rotates_pixels_clockwise() {
        let cases = [
            (Rotation::R0, (3, 2), vec![1, 2, 3, 4, 5, 6]),
            // 4 1
            // 5 2
            // 6 3
            (Rotation::R90, (2, 3), vec![4, 1, 5, 2, 6, 3]),
            (Rotation::R180, (3, 2), vec![6, 5, 4, 3, 2, 1]),
            // 3 6
            // 2 5
            // 1 4
            (Rotation::R270, (2, 3), vec![3, 6, 2, 5, 1, 4]),
        ];
        for (r, size, raw) in cases {
            let f = r.apply(frame());
            assert_eq!(((f.width, f.height), f.raw), (size, raw), "{r:?}");
            assert_eq!(f.kelvin_per_count, Some(0.01));
        }
    }

    #[test]
    fn points_follow_their_pixels_and_steps_compose() {
        let f = frame();
        for r in Rotation::ALL {
            let turned = r.apply(f.clone());
            for y in 0..2 {
                for x in 0..3 {
                    let (nx, ny) = r.point((x, y), (3, 2));
                    assert_eq!(turned.at(nx, ny), f.at(x, y), "{r:?} ({x},{y})");
                }
            }
            for from in Rotation::ALL {
                // Rotating to `from`, then by `r.since(from)`, equals rotating by `r`.
                let two = r.since(from).apply(from.apply(f.clone()));
                assert_eq!(two.raw, turned.raw, "{from:?} → {r:?}");
            }
        }
    }
}
