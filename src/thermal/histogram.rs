//! Histogram of a frame's display values.

use super::frame::ThermalFrame;

#[derive(Debug, Clone, PartialEq)]
pub struct Histogram {
    /// Display value at the lower edge of the first bin.
    pub lo: f32,
    pub bin_width: f32,
    pub counts: Vec<u32>,
}

impl Histogram {
    /// Bin the frame between `lo..hi` (display units). Values outside go to the end bins.
    pub fn compute(f: &ThermalFrame, lo: f32, hi: f32, bins: usize) -> Self {
        let bins = bins.max(1);
        let (lo, hi) = if hi > lo { (lo, hi) } else { (lo, lo + 1.0) };
        let bin_width = (hi - lo) / bins as f32;
        let mut counts = vec![0u32; bins];
        for &r in &f.raw {
            let i = ((f.value(r) - lo) / bin_width).floor();
            let i = (i.max(0.0) as usize).min(bins - 1);
            counts[i] += 1;
        }
        Self {
            lo,
            bin_width,
            counts,
        }
    }

    pub fn bin_center(&self, i: usize) -> f32 {
        self.lo + (i as f32 + 0.5) * self.bin_width
    }

    /// Share of pixels (0..1) in each bin.
    pub fn fractions(&self) -> impl Iterator<Item = f32> + '_ {
        let total = self.counts.iter().sum::<u32>().max(1) as f32;
        self.counts.iter().map(move |&c| c as f32 / total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_and_clamps() {
        // values (°C): 0, 1, 2, 3 with 0.01 K/count → raw = (v + 273.15) * 100
        let raw = [0.0f32, 1.0, 2.0, 3.0]
            .iter()
            .map(|v| ((v + 273.15) * 100.0).round() as u16)
            .collect();
        let f = ThermalFrame::new(2, 2, raw, Some(0.01));
        let h = Histogram::compute(&f, 0.5, 2.5, 2);
        // 0 → clamped to bin 0, 1 → bin 0, 2 → bin 1, 3 → clamped to bin 1
        assert_eq!(h.counts, vec![2, 2]);
        assert!((h.bin_center(0) - 1.0).abs() < 1e-5);
        assert!((h.fractions().sum::<f32>() - 1.0).abs() < 1e-5);
    }
}
