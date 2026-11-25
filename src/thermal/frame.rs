//! Radiometric frame: raw Lepton counts plus the scale needed to turn them into temperatures.

use std::sync::Arc;
use std::time::SystemTime;

use super::calibration::CalTable;

#[derive(Debug, Clone)]
pub struct ThermalFrame {
    pub width: usize,
    pub height: usize,
    /// Row-major raw counts.
    pub raw: Vec<u16>,
    /// Kelvin per count when TLinear is on (0.01 or 0.1); `None` = uncalibrated counts.
    pub kelvin_per_count: Option<f32>,
    pub sequence: u32,
    pub captured: SystemTime,
    /// Temperature correction for display values (radiometric frames only). `raw` is unchanged.
    pub calibration: Option<Arc<CalTable>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    pub min: u16,
    pub max: u16,
    pub min_pos: (usize, usize),
    pub max_pos: (usize, usize),
    pub mean: f32,
}

/// Decode little-endian 16-bit pixels.
pub fn decode_le_u16(bytes: &[u8]) -> Vec<u16> {
    let (pairs, _) = bytes.as_chunks::<2>();
    pairs.iter().map(|p| u16::from_le_bytes(*p)).collect()
}

impl ThermalFrame {
    pub fn new(width: usize, height: usize, raw: Vec<u16>, kelvin_per_count: Option<f32>) -> Self {
        debug_assert_eq!(raw.len(), width * height);
        Self {
            width,
            height,
            raw,
            kelvin_per_count,
            sequence: 0,
            captured: SystemTime::now(),
            calibration: None,
        }
    }

    pub fn is_radiometric(&self) -> bool {
        self.kelvin_per_count.is_some()
    }

    fn table(&self) -> Option<&CalTable> {
        self.kelvin_per_count.and(self.calibration.as_deref())
    }

    /// Display value of a raw count: corrected °C when radiometric, otherwise the count itself.
    pub fn value(&self, raw: u16) -> f32 {
        if let Some(t) = self.table() {
            return t.value(raw);
        }
        self.value_f(raw as f32)
    }

    /// Display value of a fractional raw count, such as a mean over pixels.
    pub fn value_f(&self, raw: f32) -> f32 {
        if let Some(t) = self.table() {
            return t.value_f(raw);
        }
        match self.kelvin_per_count {
            Some(k) => raw * k - 273.15,
            None => raw,
        }
    }

    /// Camera reading in °C before any correction (`None` for raw counts).
    pub fn apparent_c(&self, raw: f32) -> Option<f32> {
        self.kelvin_per_count.map(|k| raw * k - 273.15)
    }

    /// Inverse of [`Self::value`], clamped to the u16 range.
    pub fn raw_from_value(&self, v: f32) -> u16 {
        if let Some(t) = self.table() {
            return t.raw_from_value(v);
        }
        let r = match self.kelvin_per_count {
            Some(k) => (v + 273.15) / k,
            None => v,
        };
        r.round().clamp(0.0, u16::MAX as f32) as u16
    }

    pub fn unit(&self) -> &'static str {
        if self.is_radiometric() {
            "°C"
        } else {
            "cnt"
        }
    }

    pub fn at(&self, x: usize, y: usize) -> u16 {
        self.raw[y * self.width + x]
    }

    pub fn stats(&self) -> FrameStats {
        let mut s = FrameStats {
            min: u16::MAX,
            max: 0,
            min_pos: (0, 0),
            max_pos: (0, 0),
            mean: 0.0,
        };
        let mut sum = 0u64;
        for (i, &v) in self.raw.iter().enumerate() {
            sum += v as u64;
            if v < s.min {
                s.min = v;
                s.min_pos = (i % self.width, i / self.width);
            }
            if v > s.max {
                s.max = v;
                s.max_pos = (i % self.width, i / self.width);
            }
        }
        if !self.raw.is_empty() {
            s.mean = sum as f32 / self.raw.len() as f32;
        }
        s
    }

    /// Mean raw value over the (2r+1)² window around (x, y), clipped to the frame.
    pub fn mean_around(&self, x: usize, y: usize, r: usize) -> f32 {
        let x0 = x.saturating_sub(r);
        let y0 = y.saturating_sub(r);
        let x1 = (x + r).min(self.width - 1);
        let y1 = (y + r).min(self.height - 1);
        let mut sum = 0u64;
        let mut n = 0u64;
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                sum += self.at(xx, yy) as u64;
                n += 1;
            }
        }
        sum as f32 / n as f32
    }

    /// Display value (°C or counts) of the mean around (x, y).
    pub fn value_around(&self, x: usize, y: usize, r: usize) -> f32 {
        self.value_f(self.mean_around(x, y, r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(raw: Vec<u16>, w: usize, h: usize) -> ThermalFrame {
        ThermalFrame::new(w, h, raw, Some(0.01))
    }

    #[test]
    fn decode() {
        assert_eq!(
            decode_le_u16(&[0x34, 0x12, 0xff, 0x00]),
            vec![0x1234, 0x00ff]
        );
    }

    #[test]
    fn celsius_conversion() {
        let f = frame(vec![29315; 4], 2, 2);
        assert!((f.value(29315) - 20.0).abs() < 1e-3);
        assert_eq!(f.raw_from_value(20.0), 29315);
        let counts = ThermalFrame::new(2, 2, vec![5; 4], None);
        assert_eq!(counts.value(5), 5.0);
        assert_eq!(counts.unit(), "cnt");
    }

    #[test]
    fn calibration_applies_to_values_not_raw() {
        use crate::thermal::calibration::Calibration;
        let mut f = frame(vec![29315; 4], 2, 2);
        let mut cal = Calibration::default();
        cal.add_ref(20.0, 21.0).unwrap();
        f.calibration = Some(Arc::new(CalTable::new(0.01, &cal)));
        assert!((f.value(29315) - 21.0).abs() < 1e-3);
        assert!((f.value_around(0, 0, 1) - 21.0).abs() < 1e-3);
        assert_eq!(
            f.apparent_c(29315.0).map(|v| (v * 100.0).round()),
            Some(2000.0)
        );
        assert_eq!(f.raw_from_value(21.0), 29315);
        assert_eq!(f.raw, vec![29315; 4]);
        // Raw counts ignore a table.
        let mut counts = ThermalFrame::new(2, 2, vec![5; 4], None);
        counts.calibration = f.calibration.clone();
        assert_eq!(counts.value(5), 5.0);
    }

    #[test]
    fn stats_find_extremes() {
        let f = frame(vec![5, 9, 1, 7, 3, 2], 3, 2);
        let s = f.stats();
        assert_eq!((s.min, s.min_pos), (1, (2, 0)));
        assert_eq!((s.max, s.max_pos), (9, (1, 0)));
        assert!((s.mean - 27.0 / 6.0).abs() < 1e-5);
    }

    #[test]
    fn mean_around_clips_at_edges() {
        let f = frame((0..9).collect(), 3, 3);
        assert_eq!(f.mean_around(1, 1, 1), 4.0);
        // corner (0,0) with r=1 → pixels 0,1,3,4
        assert_eq!(f.mean_around(0, 0, 1), 2.0);
    }
}
