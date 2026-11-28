//! Difference image: how much each pixel has changed since a reference frame.

use super::frame::ThermalFrame;

/// A frame kept as "before".
#[derive(Debug, Clone)]
pub struct Reference {
    raw: Vec<u16>,
    size: (usize, usize),
    scale: Option<f32>,
}

/// The extremes of a difference image, in display units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffStats {
    /// Largest drop (negative when something got colder).
    pub min: f32,
    /// Largest rise.
    pub max: f32,
    pub min_pos: (usize, usize),
    pub max_pos: (usize, usize),
}

impl DiffStats {
    /// The larger of the rise and the drop.
    pub fn reach(&self) -> f32 {
        self.max.abs().max(self.min.abs())
    }
}

impl Reference {
    pub fn of(f: &ThermalFrame) -> Self {
        Self {
            raw: f.raw.clone(),
            size: (f.width, f.height),
            scale: f.kelvin_per_count,
        }
    }

    /// Whether `f` can be compared with it: the same image size and temperature scale.
    pub fn matches(&self, f: &ThermalFrame) -> bool {
        self.size == (f.width, f.height)
            && self.scale == f.kelvin_per_count
            && self.raw.len() == f.raw.len()
    }

    /// `f` minus the reference for every pixel, as display values (so with `f`'s temperature
    /// correction applied to both). `None` when the two cannot be compared.
    pub fn subtract(&self, f: &ThermalFrame, out: &mut Vec<f32>) -> Option<DiffStats> {
        out.clear();
        if !self.matches(f) || f.raw.is_empty() {
            return None;
        }
        let mut st = DiffStats {
            min: f32::MAX,
            max: f32::MIN,
            min_pos: (0, 0),
            max_pos: (0, 0),
        };
        out.reserve(f.raw.len());
        for (i, (&now, &before)) in f.raw.iter().zip(&self.raw).enumerate() {
            // Equal counts are exactly "no change", whatever the conversion rounds to.
            let d = if now == before {
                0.0
            } else {
                f.value(now) - f.value(before)
            };
            if d < st.min {
                st.min = d;
                st.min_pos = (i % f.width, i / f.width);
            }
            if d > st.max {
                st.max = d;
                st.max_pos = (i % f.width, i / f.width);
            }
            out.push(d);
        }
        Some(st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differences_and_their_extremes() {
        let before = ThermalFrame::new(3, 2, vec![30000; 6], Some(0.01));
        let reference = Reference::of(&before);
        // +2 K at (1, 0), −0.5 K at (2, 1).
        let now = ThermalFrame::new(
            3,
            2,
            vec![30000, 30200, 30000, 30000, 30000, 29950],
            Some(0.01),
        );
        let mut d = Vec::new();
        let st = reference.subtract(&now, &mut d).unwrap();
        assert_eq!(d.len(), 6);
        assert_eq!(d[0], 0.0);
        assert!((d[1] - 2.0).abs() < 1e-3 && (d[5] + 0.5).abs() < 1e-3);
        assert_eq!((st.max_pos, st.min_pos), ((1, 0), (2, 1)));
        assert!((st.max - 2.0).abs() < 1e-3 && (st.min + 0.5).abs() < 1e-3);
        assert!((st.reach() - 2.0).abs() < 1e-3);

        // The same frame: nothing changed.
        let st = reference.subtract(&before, &mut d).unwrap();
        assert_eq!((st.min, st.max), (0.0, 0.0));
    }

    #[test]
    fn other_sizes_and_scales_cannot_be_compared() {
        let reference = Reference::of(&ThermalFrame::new(2, 2, vec![30000; 4], Some(0.01)));
        let mut d = vec![1.0];
        let wide = ThermalFrame::new(4, 1, vec![30000; 4], Some(0.01));
        assert!(!reference.matches(&wide));
        assert_eq!(reference.subtract(&wide, &mut d), None);
        assert!(d.is_empty(), "no stale values left behind");
        let counts = ThermalFrame::new(2, 2, vec![30000; 4], None);
        assert!(!reference.matches(&counts));
        let other_scale = ThermalFrame::new(2, 2, vec![3000; 4], Some(0.1));
        assert!(!reference.matches(&other_scale));
    }

    #[test]
    fn the_correction_applies_to_both_frames() {
        use crate::thermal::calibration::{CalTable, Calibration};
        use std::sync::Arc;
        let reference = Reference::of(&ThermalFrame::new(1, 1, vec![30000], Some(0.01)));
        let mut now = ThermalFrame::new(1, 1, vec![30100], Some(0.01));
        let mut d = Vec::new();
        let plain = reference.subtract(&now, &mut d).unwrap().max;
        // A low emissivity stretches differences from the reflected temperature.
        let cal = Calibration {
            emissivity: 0.5,
            ..Default::default()
        };
        now.calibration = Some(Arc::new(CalTable::new(0.01, &cal)));
        let corrected = reference.subtract(&now, &mut d).unwrap().max;
        assert!((plain - 1.0).abs() < 1e-3, "{plain}");
        assert!(corrected > plain * 1.3, "{corrected} vs {plain}");
    }
}
