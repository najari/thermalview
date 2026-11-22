//! Color-scale range: automatic (follows the frame, smoothed) or manual (fixed by the user).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RangeMode {
    Auto,
    /// Fixed display values (°C when radiometric).
    Manual {
        lo: f32,
        hi: f32,
    },
}

impl Default for RangeMode {
    fn default() -> Self {
        RangeMode::Auto
    }
}

/// Smooths the automatic range so the colors don't flicker frame to frame.
#[derive(Debug, Clone, Default)]
pub struct AutoRange {
    state: Option<(f32, f32)>,
}

impl AutoRange {
    /// Minimum span in display units so a flat scene isn't amplified into noise.
    pub const MIN_SPAN: f32 = 2.0;
    const ALPHA: f32 = 0.3;

    pub fn reset(&mut self) {
        self.state = None;
    }

    /// Feed this frame's min/max (display units) and get the smoothed range.
    pub fn update(&mut self, min: f32, max: f32) -> (f32, f32) {
        let (lo, hi) = match self.state {
            None => (min, max),
            Some((lo, hi)) => (lo + (min - lo) * Self::ALPHA, hi + (max - hi) * Self::ALPHA),
        };
        self.state = Some((lo, hi));
        widen(lo, hi, Self::MIN_SPAN)
    }
}

/// Make sure `hi - lo >= min_span`, widening around the center.
pub fn widen(lo: f32, hi: f32, min_span: f32) -> (f32, f32) {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    if hi - lo >= min_span {
        (lo, hi)
    } else {
        let c = (lo + hi) / 2.0;
        (c - min_span / 2.0, c + min_span / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_update_is_exact_then_smooths() {
        let mut a = AutoRange::default();
        assert_eq!(a.update(20.0, 30.0), (20.0, 30.0));
        let (lo, hi) = a.update(10.0, 30.0);
        assert!((lo - 17.0).abs() < 1e-4);
        assert_eq!(hi, 30.0);
    }

    #[test]
    fn flat_scene_gets_min_span() {
        assert_eq!(widen(25.0, 25.5, 2.0), (24.25, 26.25));
        assert_eq!(widen(30.0, 20.0, 2.0), (20.0, 30.0));
    }
}
