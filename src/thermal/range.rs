//! Color-scale range: automatic (follows the frame, smoothed) or manual (fixed by the user).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum RangeMode {
    #[default]
    Auto,
    /// Automatic, without the coldest and hottest [`ROBUST_TAIL`] of the pixels: one hot
    /// point does not flatten the rest of the image into a single color.
    Robust,
    /// Fixed display values (°C when radiometric).
    Manual { lo: f32, hi: f32 },
}

/// Share of the pixels [`RangeMode::Robust`] leaves out at each end.
pub const ROBUST_TAIL: f32 = 0.01;

/// The raw values that leave `tail` of the pixels below and above them. `scratch` is working
/// memory kept between calls.
pub fn percentiles(raw: &[u16], tail: f32, scratch: &mut Vec<u16>) -> Option<(u16, u16)> {
    if raw.is_empty() {
        return None;
    }
    scratch.clear();
    scratch.extend_from_slice(raw);
    let last = raw.len() - 1;
    let k = ((raw.len() as f32 * tail.clamp(0.0, 0.49)).round() as usize).min(last / 2);
    let (_, lo, above) = scratch.select_nth_unstable(k);
    let lo = *lo;
    // The upper one is among the values left above the lower one.
    let hi = match (last - k).checked_sub(k + 1) {
        Some(i) => *above.select_nth_unstable(i).1,
        None => lo,
    };
    Some((lo, hi))
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
    fn percentiles_leave_out_the_tails() {
        let mut scratch = Vec::new();
        // 1000 pixels at 100..=1099, shuffled by a stride, plus nothing else.
        let raw: Vec<u16> = (0..1000u32).map(|i| 100 + (i * 37 % 1000) as u16).collect();
        assert_eq!(percentiles(&raw, 0.01, &mut scratch), Some((110, 1089)));
        assert_eq!(percentiles(&raw, 0.0, &mut scratch), Some((100, 1099)));
        // One hot pixel in a flat image does not set the range.
        let mut flat = vec![500u16; 1000];
        flat[3] = 60000;
        assert_eq!(percentiles(&flat, 0.01, &mut scratch), Some((500, 500)));
        assert_eq!(percentiles(&[7], 0.01, &mut scratch), Some((7, 7)));
        assert_eq!(percentiles(&[7, 9], 0.4, &mut scratch), Some((7, 9)));
        assert_eq!(percentiles(&[], 0.01, &mut scratch), None);
    }

    #[test]
    fn flat_scene_gets_min_span() {
        assert_eq!(widen(25.0, 25.5, 2.0), (24.25, 26.25));
        assert_eq!(widen(30.0, 20.0, 2.0), (20.0, 30.0));
    }
}
