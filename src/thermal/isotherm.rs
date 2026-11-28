//! Isotherm: every pixel inside a temperature band is shown in one color.

use serde::{Deserialize, Serialize};

use super::frame::ThermalFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Band {
    /// At or above `lo`.
    #[default]
    Above,
    /// At or below `hi`.
    Below,
    /// From `lo` to `hi`.
    Between,
}

impl Band {
    pub const ALL: [Band; 3] = [Band::Above, Band::Below, Band::Between];

    pub fn label(self) -> &'static str {
        match self {
            Band::Above => "이상",
            Band::Below => "이하",
            Band::Between => "구간",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Isotherm {
    pub enabled: bool,
    pub band: Band,
    /// Threshold of [`Band::Above`] and lower edge of [`Band::Between`], in display units.
    pub lo: f32,
    /// Threshold of [`Band::Below`] and upper edge of [`Band::Between`].
    pub hi: f32,
    pub color: [u8; 3],
}

impl Default for Isotherm {
    fn default() -> Self {
        Self {
            enabled: false,
            band: Band::Above,
            lo: 40.0,
            hi: 60.0,
            // A green that none of the palettes but Rainbow uses.
            color: [0, 235, 70],
        }
    }
}

impl Isotherm {
    /// The band in display units; an open end is infinite. `None` when off.
    pub fn limits(&self) -> Option<(f32, f32)> {
        if !self.enabled {
            return None;
        }
        Some(match self.band {
            Band::Above => (self.lo, f32::INFINITY),
            Band::Below => (f32::NEG_INFINITY, self.hi),
            Band::Between => (self.lo.min(self.hi), self.lo.max(self.hi)),
        })
    }

    /// The band as raw counts of `f` (inclusive). `None` when off.
    pub fn raw_band(&self, f: &ThermalFrame) -> Option<(u16, u16)> {
        let (lo, hi) = self.limits()?;
        let raw = |v: f32, open: u16| {
            if v.is_finite() {
                f.raw_from_value(v)
            } else {
                open
            }
        };
        Some((raw(lo, 0), raw(hi, u16::MAX)))
    }

    /// "40.0 °C 이상", for the header of the settings section.
    pub fn summary(&self, unit: &str) -> String {
        match self.band {
            Band::Above => format!("{:.1} {unit} 이상", self.lo),
            Band::Below => format!("{:.1} {unit} 이하", self.hi),
            Band::Between => format!(
                "{:.1}~{:.1} {unit}",
                self.lo.min(self.hi),
                self.lo.max(self.hi)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_in_raw_counts() {
        // 0.01 K per count: 40 °C = 31315, 60 °C = 33315.
        let f = ThermalFrame::new(1, 1, vec![0], Some(0.01));
        let mut iso = Isotherm::default();
        assert_eq!(iso.raw_band(&f), None, "off by default");
        iso.enabled = true;
        assert_eq!(iso.raw_band(&f), Some((31315, u16::MAX)));
        iso.band = Band::Below;
        assert_eq!(iso.raw_band(&f), Some((0, 33315)));
        iso.band = Band::Between;
        assert_eq!(iso.raw_band(&f), Some((31315, 33315)));
        // Edges typed the wrong way round still make a band.
        (iso.lo, iso.hi) = (60.0, 40.0);
        assert_eq!(iso.raw_band(&f), Some((31315, 33315)));
        assert_eq!(iso.summary("°C"), "40.0~60.0 °C");
    }

    #[test]
    fn old_settings_without_it_load() {
        let iso: Isotherm = serde_json::from_str("{}").unwrap();
        assert_eq!(iso, Isotherm::default());
    }
}
