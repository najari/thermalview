//! Temporal noise filter: each pixel averaged over the last frames.

use super::frame::ThermalFrame;

/// A pixel that moves by more than this starts over at the new value instead of drifting
/// towards it: something moving through the scene leaves no trail.
pub const RESET_K: f32 = 1.0;
/// The same limit for raw counts, which have no temperature scale.
const RESET_COUNTS: f32 = 100.0;

/// Running average per pixel. The first frames are a plain mean; from then on each new frame
/// counts for 1/strength.
#[derive(Debug, Default)]
pub struct Temporal {
    acc: Vec<f32>,
    /// Frames averaged so far, up to the strength.
    count: u32,
    size: (usize, usize),
    scale: Option<f32>,
}

impl Temporal {
    /// Strengths offered to the user; 1 is off.
    pub const STRENGTHS: [u8; 5] = [1, 2, 4, 8, 16];

    /// Forget the past frames: the next one passes unchanged.
    pub fn reset(&mut self) {
        self.count = 0;
    }

    /// The frame with every pixel replaced by its average. `strength` 0 or 1 passes it on
    /// unchanged.
    pub fn apply(&mut self, mut f: ThermalFrame, strength: u8) -> ThermalFrame {
        if strength <= 1 {
            self.count = 0;
            return f;
        }
        let same = self.count > 0
            && self.size == (f.width, f.height)
            && self.scale == f.kelvin_per_count
            && self.acc.len() == f.raw.len();
        if !same {
            self.acc.clear();
            self.acc.extend(f.raw.iter().map(|&v| v as f32));
            self.size = (f.width, f.height);
            self.scale = f.kelvin_per_count;
            self.count = 1;
            return f;
        }
        self.count = (self.count + 1).min(strength as u32);
        let gain = 1.0 / self.count as f32;
        let limit = match f.kelvin_per_count {
            Some(k) if k > 0.0 => RESET_K / k,
            _ => RESET_COUNTS,
        };
        for (acc, v) in self.acc.iter_mut().zip(&mut f.raw) {
            let x = *v as f32;
            if (x - *acc).abs() > limit {
                *acc = x;
            } else {
                *acc += (x - *acc) * gain;
                *v = acc.round() as u16;
            }
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(raw: Vec<u16>) -> ThermalFrame {
        ThermalFrame::new(raw.len(), 1, raw, Some(0.01))
    }

    #[test]
    fn averages_noise_and_follows_slow_change() {
        let mut t = Temporal::default();
        // ±0.2 K around 30000 counts.
        let mut out = Vec::new();
        for i in 0..40 {
            let v = if i % 2 == 0 { 30020 } else { 29980 };
            out.push(t.apply(frame(vec![v]), 8).raw[0]);
        }
        assert_eq!(out[0], 30020, "the first frame passes unchanged");
        assert_eq!(out[1], 30000, "then a plain mean while it fills");
        for v in &out[8..] {
            assert!((29995..=30005).contains(v), "{v}");
        }
        // A slow drift (0.05 K a frame) is followed, with a lag.
        let mut last = 0;
        for i in 0..100u16 {
            last = t.apply(frame(vec![30000 + i * 5]), 8).raw[0];
        }
        assert!((30440..30495).contains(&last), "{last}");
    }

    #[test]
    fn a_jump_is_taken_at_once_only_where_it_happens() {
        let mut t = Temporal::default();
        for _ in 0..10 {
            t.apply(frame(vec![30000, 30000]), 16);
        }
        // +5 K on the first pixel (a hand moving in), +0.1 K on the second.
        let f = t.apply(frame(vec![30500, 30010]), 16);
        assert_eq!(f.raw[0], 30500);
        assert!(f.raw[1] < 30005, "{}", f.raw[1]);
    }

    #[test]
    fn off_and_changes_of_size_or_scale_start_over() {
        let mut t = Temporal::default();
        t.apply(frame(vec![30000, 30000]), 4);
        assert_eq!(t.apply(frame(vec![30040, 30040]), 1).raw, vec![30040; 2]);
        t.apply(frame(vec![30000, 30000]), 4);
        // Another camera: three pixels.
        assert_eq!(t.apply(frame(vec![30040; 3]), 4).raw, vec![30040; 3]);
        // Another scale.
        let mut f = frame(vec![3000; 3]);
        f.kelvin_per_count = Some(0.1);
        assert_eq!(t.apply(f, 4).raw, vec![3000; 3]);
        t.reset();
        let mut f = frame(vec![3004; 3]);
        f.kelvin_per_count = Some(0.1);
        assert_eq!(t.apply(f, 4).raw, vec![3004; 3]);
    }

    #[test]
    fn keeps_the_frame_identity() {
        let mut t = Temporal::default();
        t.apply(frame(vec![30000]), 4);
        let mut f = frame(vec![30010]);
        f.sequence = 77;
        let captured = f.captured;
        let out = t.apply(f, 4);
        assert_eq!((out.sequence, out.captured), (77, captured));
    }
}
