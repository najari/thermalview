//! Temperature correction on top of the Lepton's TLinear output (DEVELOPMENT.md, 단계 5).
//!
//! TLinear is the *apparent* temperature: what an ideal blackbody (ε = 1) at that temperature
//! would read. Two corrections turn it into object temperature, in this order:
//!
//! 1. Reference correction (sensor): `apparent' = gain × apparent + offset`, fitted from one or
//!    two references of known temperature.
//! 2. Emissivity: with in-band (8–14 µm) radiance `L`,
//!    `L(apparent') = ε·L(object) + (1 − ε)·L(reflected)`, solved for the object temperature.
//!
//! Raw counts are never changed; snapshots and recordings keep the camera's values.

use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const K0: f64 = 273.15;
/// Lepton 3.5 spectral band, µm.
const BAND_UM: (f64, f64) = (8.0, 14.0);
/// First and second radiation constants for spectral radiance (W·µm⁴·m⁻²·sr⁻¹, µm·K).
const C1: f64 = 1.191_042_97e8;
const C2: f64 = 14_387.769;
/// Radiance table range and step, K.
const T_MIN: f64 = 100.0;
const T_MAX: f64 = 2500.0;
const T_STEP: f64 = 0.5;

pub const MIN_EMISSIVITY: f32 = 0.10;
pub const MAX_REFS: usize = 2;
/// Two references must be at least this far apart (measured, °C) to fit a gain.
pub const MIN_REF_SPAN_C: f32 = 2.0;
pub const GAIN_LIMITS: (f32, f32) = (0.5, 2.0);
/// A single reference may not move the reading by more than this; larger means a mistake.
pub const MAX_REF_OFFSET_C: f32 = 20.0;

/// In-band blackbody radiance tabulated over temperature.
struct Band {
    radiance: Vec<f64>,
}

impl Band {
    fn build() -> Self {
        let n = ((T_MAX - T_MIN) / T_STEP).round() as usize + 1;
        let radiance = (0..n)
            .map(|i| band_radiance(T_MIN + i as f64 * T_STEP))
            .collect();
        Self { radiance }
    }

    fn radiance(&self, t_k: f64) -> f64 {
        let last = self.radiance.len() - 1;
        let pos = ((t_k - T_MIN) / T_STEP).clamp(0.0, last as f64);
        let i = (pos.floor() as usize).min(last - 1);
        let frac = pos - i as f64;
        self.radiance[i] + (self.radiance[i + 1] - self.radiance[i]) * frac
    }

    /// Inverse of [`Self::radiance`], clamped to the table range.
    fn temperature(&self, l: f64) -> f64 {
        let r = &self.radiance;
        if l.is_nan() || l <= r[0] {
            return T_MIN;
        }
        if l >= r[r.len() - 1] {
            return T_MAX;
        }
        let i = r.partition_point(|&x| x < l).max(1);
        let frac = (l - r[i - 1]) / (r[i] - r[i - 1]);
        T_MIN + (i as f64 - 1.0 + frac) * T_STEP
    }
}

/// Planck spectral radiance integrated over the band (Simpson's rule).
fn band_radiance(t_k: f64) -> f64 {
    const N: usize = 120;
    let (a, b) = BAND_UM;
    let h = (b - a) / N as f64;
    let planck = |lam: f64| C1 / (lam.powi(5) * (C2 / (lam * t_k)).exp_m1());
    let mut sum = planck(a) + planck(b);
    for i in 1..N {
        let w = if i % 2 == 1 { 4.0 } else { 2.0 };
        sum += w * planck(a + i as f64 * h);
    }
    sum * h / 3.0
}

fn band() -> &'static Band {
    static BAND: OnceLock<Band> = OnceLock::new();
    BAND.get_or_init(Band::build)
}

/// One recorded reference.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RefPoint {
    /// Apparent temperature the camera read, before any correction, °C.
    pub measured_c: f32,
    /// Apparent temperature it should have read: the reference temperature passed through the
    /// emissivity model in force when it was recorded, °C.
    pub expected_c: f32,
    /// Reference temperature the user entered, °C.
    pub reference_c: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RefError {
    Full,
    TooClose,
    Gain(f32),
    Offset(f32),
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefError::Full => write!(f, "기준점은 {MAX_REFS}개까지 기록할 수 있습니다"),
            RefError::TooClose => write!(
                f,
                "두 기준점의 측정값이 {MIN_REF_SPAN_C} °C 이상 달라야 합니다"
            ),
            RefError::Gain(g) => write!(
                f,
                "계산된 기울기 {g:.3}이(가) {}~{} 밖입니다. 기준 온도를 확인하세요",
                GAIN_LIMITS.0, GAIN_LIMITS.1
            ),
            RefError::Offset(o) => write!(
                f,
                "측정값과 기준의 차이({o:+.1} °C)가 ±{MAX_REF_OFFSET_C} °C를 넘습니다. 기준 온도를 확인하세요"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    pub emissivity: f32,
    /// Temperature of the surroundings reflected by the object, °C.
    pub reflected_c: f32,
    pub refs: Vec<RefPoint>,
}

impl Default for Calibration {
    fn default() -> Self {
        Self {
            emissivity: 1.0,
            reflected_c: 20.0,
            refs: Vec::new(),
        }
    }
}

/// Typical emissivities offered in the UI.
pub const EMISSIVITY_PRESETS: [(&str, f32); 6] = [
    ("흑체 (1.00)", 1.00),
    ("사람 피부 (0.98)", 0.98),
    ("물·얼음 (0.96)", 0.96),
    ("검은 절연 테이프 (0.95)", 0.95),
    ("무광 페인트 (0.94)", 0.94),
    ("나무 (0.90)", 0.90),
];

impl Calibration {
    pub fn is_identity(&self) -> bool {
        !self.uses_emissivity() && self.refs.is_empty()
    }

    pub fn uses_emissivity(&self) -> bool {
        self.emissivity < 1.0
    }

    fn eps(&self) -> f64 {
        self.emissivity.clamp(MIN_EMISSIVITY, 1.0) as f64
    }

    /// Reference correction as (gain, offset): apparent' = gain × apparent + offset.
    pub fn linear(&self) -> (f32, f32) {
        match self.refs.as_slice() {
            [] => (1.0, 0.0),
            [p] => (1.0, p.expected_c - p.measured_c),
            [p, q, ..] => {
                let gain = (q.expected_c - p.expected_c) / (q.measured_c - p.measured_c);
                (gain, p.expected_c - gain * p.measured_c)
            }
        }
    }

    /// Object temperature for a camera reading (apparent temperature), both °C.
    pub fn object_c(&self, apparent_c: f32) -> f32 {
        let (gain, offset) = self.linear();
        let a = gain * apparent_c + offset;
        if !self.uses_emissivity() {
            return a;
        }
        let b = band();
        let e = self.eps();
        let lm = b.radiance(a as f64 + K0);
        let lr = b.radiance(self.reflected_c as f64 + K0);
        (b.temperature((lm - (1.0 - e) * lr) / e) - K0) as f32
    }

    /// Apparent temperature an ideal sensor would report for an object at `object_c`
    /// (the emissivity model only, no reference correction).
    pub fn apparent_c(&self, object_c: f32) -> f32 {
        if !self.uses_emissivity() {
            return object_c;
        }
        let b = band();
        let e = self.eps();
        let lo = b.radiance(object_c as f64 + K0);
        let lr = b.radiance(self.reflected_c as f64 + K0);
        (b.temperature(e * lo + (1.0 - e) * lr) - K0) as f32
    }

    /// Record a reference: the camera read `measured_c` (uncorrected) while looking at an object
    /// of known temperature `reference_c`.
    pub fn add_ref(&mut self, measured_c: f32, reference_c: f32) -> Result<(), RefError> {
        if self.refs.len() >= MAX_REFS {
            return Err(RefError::Full);
        }
        let p = RefPoint {
            measured_c,
            expected_c: self.apparent_c(reference_c),
            reference_c,
        };
        let diff = p.expected_c - p.measured_c;
        if diff.abs() > MAX_REF_OFFSET_C {
            return Err(RefError::Offset(diff));
        }
        if let Some(q) = self.refs.first() {
            if (p.measured_c - q.measured_c).abs() < MIN_REF_SPAN_C {
                return Err(RefError::TooClose);
            }
            let gain = (p.expected_c - q.expected_c) / (p.measured_c - q.measured_c);
            if !(GAIN_LIMITS.0..=GAIN_LIMITS.1).contains(&gain) {
                return Err(RefError::Gain(gain));
            }
        }
        self.refs.push(p);
        Ok(())
    }

    /// Make values read from a settings file safe to use, with the limits the UI enforces: a
    /// hand-edited or damaged file could otherwise give two references a zero span (division
    /// by zero, every temperature NaN). Returns true if anything was changed.
    pub fn sanitize(&mut self) -> bool {
        let before = self.clone();
        if !self.emissivity.is_finite() {
            self.emissivity = 1.0;
        }
        self.emissivity = self.emissivity.clamp(MIN_EMISSIVITY, 1.0);
        if !self.reflected_c.is_finite() {
            self.reflected_c = 20.0;
        }
        self.refs.truncate(MAX_REFS);
        self.refs.retain(|p| {
            p.measured_c.is_finite()
                && p.expected_c.is_finite()
                && (p.expected_c - p.measured_c).abs() <= MAX_REF_OFFSET_C
        });
        if let [p, q] = self.refs.as_slice() {
            let span = q.measured_c - p.measured_c;
            let gain = (q.expected_c - p.expected_c) / span;
            if span.abs() < MIN_REF_SPAN_C || !(GAIN_LIMITS.0..=GAIN_LIMITS.1).contains(&gain) {
                self.refs.truncate(1);
            }
        }
        *self != before
    }

    /// One-line description for the UI and logs.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.uses_emissivity() {
            parts.push(format!(
                "ε {:.2}, 반사 {:.1} °C",
                self.emissivity, self.reflected_c
            ));
        }
        match self.refs.len() {
            0 => {}
            1 => parts.push(format!("기준 1점 ({:+.2} °C)", self.linear().1)),
            n => {
                let (g, o) = self.linear();
                parts.push(format!("기준 {n}점 (×{g:.3} {o:+.2} °C)"));
            }
        }
        parts.join(", ")
    }
}

/// Raw count → corrected °C for every possible count, built when the calibration changes and
/// shared by frames.
pub struct CalTable {
    kelvin_per_count: f32,
    cal: Calibration,
    lut: Box<[f32]>,
}

impl fmt::Debug for CalTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalTable")
            .field("kelvin_per_count", &self.kelvin_per_count)
            .field("cal", &self.cal)
            .finish_non_exhaustive()
    }
}

impl CalTable {
    pub fn new(kelvin_per_count: f32, cal: &Calibration) -> Self {
        let lut = (0..=u16::MAX)
            .map(|r| cal.object_c(r as f32 * kelvin_per_count - K0 as f32))
            .collect();
        Self {
            kelvin_per_count,
            cal: cal.clone(),
            lut,
        }
    }

    pub fn matches(&self, kelvin_per_count: f32, cal: &Calibration) -> bool {
        self.kelvin_per_count == kelvin_per_count && &self.cal == cal
    }

    pub fn value(&self, raw: u16) -> f32 {
        self.lut[raw as usize]
    }

    /// Corrected value of a fractional raw count (e.g. a window mean), interpolated.
    pub fn value_f(&self, raw: f32) -> f32 {
        let pos = raw.clamp(0.0, u16::MAX as f32);
        let i = (pos.floor() as usize).min(self.lut.len() - 2);
        let frac = pos - i as f32;
        self.lut[i] + (self.lut[i + 1] - self.lut[i]) * frac
    }

    /// Smallest raw count whose corrected value is at least `v`.
    pub fn raw_from_value(&self, v: f32) -> u16 {
        self.lut.partition_point(|&x| x < v).min(u16::MAX as usize) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(emissivity: f32, reflected_c: f32) -> Calibration {
        Calibration {
            emissivity,
            reflected_c,
            refs: Vec::new(),
        }
    }

    #[test]
    fn band_table_inverts() {
        let b = band();
        for t in [150.0, 253.15, 300.0, 373.15, 673.15, 1500.0] {
            assert!((b.temperature(b.radiance(t)) - t).abs() < 1e-3, "{t}");
        }
        assert!(b.radiance(300.0) < b.radiance(300.5));
        assert_eq!(b.temperature(-1.0), T_MIN);
        assert_eq!(b.temperature(f64::MAX), T_MAX);
    }

    #[test]
    fn default_is_identity() {
        let c = Calibration::default();
        assert!(c.is_identity());
        assert_eq!(c.object_c(36.6), 36.6);
        assert_eq!(c.apparent_c(36.6), 36.6);
    }

    #[test]
    fn emissivity_roundtrip_and_direction() {
        let c = cal(0.9, 20.0);
        let apparent = c.apparent_c(80.0);
        // A hot object with ε < 1 looks colder than it is, but warmer than its surroundings.
        assert!(apparent < 80.0 && apparent > 70.0, "{apparent}");
        assert!((c.object_c(apparent) - 80.0).abs() < 0.01);
        // A cold object looks warmer.
        assert!(c.apparent_c(0.0) > 0.0);
        // At the reflected temperature emissivity does not matter.
        assert!((c.object_c(20.0) - 20.0).abs() < 0.01);
    }

    #[test]
    fn one_point_offset() {
        let mut c = Calibration::default();
        c.add_ref(30.0, 31.0).unwrap();
        assert_eq!(c.linear(), (1.0, 1.0));
        assert!((c.object_c(30.0) - 31.0).abs() < 1e-4);
        assert!(!c.is_identity());
    }

    #[test]
    fn two_point_gain_and_offset() {
        let mut c = Calibration::default();
        c.add_ref(0.5, 0.0).unwrap();
        c.add_ref(99.0, 100.0).unwrap();
        assert!((c.object_c(0.5) - 0.0).abs() < 1e-3);
        assert!((c.object_c(99.0) - 100.0).abs() < 1e-3);
        let (g, _) = c.linear();
        assert!((g - 100.0 / 98.5).abs() < 1e-4);
        assert_eq!(c.add_ref(50.0, 50.0), Err(RefError::Full));
    }

    #[test]
    fn sanitize_keeps_good_values_and_drops_unusable_ones() {
        let mut c = Calibration::default();
        assert!(!c.sanitize());
        c.add_ref(30.0, 31.0).unwrap();
        c.add_ref(60.0, 62.0).unwrap();
        c.emissivity = 0.95;
        assert!(!c.sanitize(), "valid settings stay as they are");
        // A hand-edited file: two references with the same reading, emissivity not a number.
        c.refs[1].measured_c = c.refs[0].measured_c;
        c.emissivity = f32::NAN;
        assert!(c.sanitize());
        assert_eq!(c.refs.len(), 1);
        assert_eq!(c.emissivity, 1.0);
        assert!(c.object_c(30.0).is_finite());
    }

    #[test]
    fn reference_uses_current_emissivity() {
        // The sensor reads 0.7 °C low on tape (ε 0.95) at 50 °C in a 20 °C room.
        let mut c = cal(0.95, 20.0);
        let measured = c.apparent_c(50.0) - 0.7;
        c.add_ref(measured, 50.0).unwrap();
        assert!((c.object_c(measured) - 50.0).abs() < 0.01);
    }

    #[test]
    fn rejects_bad_references() {
        let mut c = Calibration::default();
        assert!(matches!(c.add_ref(20.0, 45.0), Err(RefError::Offset(_))));
        c.add_ref(20.0, 20.5).unwrap();
        assert_eq!(c.add_ref(21.0, 21.5), Err(RefError::TooClose));
        assert!(matches!(c.add_ref(30.0, 15.0), Err(RefError::Gain(_))));
        assert_eq!(c.refs.len(), 1);
    }

    #[test]
    fn table_matches_formula_and_inverts() {
        let mut c = cal(0.95, 22.0);
        c.add_ref(30.0, 30.4).unwrap();
        let t = CalTable::new(0.01, &c);
        let raw = 30_000u16; // 26.85 °C apparent
        let expected = c.object_c(raw as f32 * 0.01 - 273.15);
        assert!((t.value(raw) - expected).abs() < 1e-4);
        assert!(
            (t.value_f(raw as f32 + 0.5) - (t.value(raw) + t.value(raw + 1)) / 2.0).abs() < 1e-4
        );
        assert!(t.raw_from_value(t.value(raw)).abs_diff(raw) <= 1);
        assert!(t.matches(0.01, &c));
        assert!(!t.matches(0.1, &c));
    }

    #[test]
    fn settings_fill_defaults() {
        let c: Calibration = serde_json::from_str(r#"{"emissivity":0.9}"#).unwrap();
        assert_eq!(c.emissivity, 0.9);
        assert_eq!(c.reflected_c, 20.0);
        assert!(c.refs.is_empty());
    }
}
