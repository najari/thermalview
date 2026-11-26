//! 256-entry color lookup tables built from gradient stops.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Palette {
    #[default]
    Ironbow,
    Rainbow,
    WhiteHot,
    BlackHot,
    Lava,
}

impl Palette {
    pub const ALL: [Palette; 5] = [
        Palette::Ironbow,
        Palette::Rainbow,
        Palette::WhiteHot,
        Palette::BlackHot,
        Palette::Lava,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Palette::Ironbow => "Ironbow",
            Palette::Rainbow => "Rainbow",
            Palette::WhiteHot => "White hot",
            Palette::BlackHot => "Black hot",
            Palette::Lava => "Lava",
        }
    }

    fn stops(self) -> &'static [(f32, [u8; 3])] {
        match self {
            Palette::Ironbow => &[
                (0.00, [0, 0, 10]),
                (0.15, [32, 0, 110]),
                (0.35, [140, 0, 150]),
                (0.55, [220, 50, 60]),
                (0.75, [250, 140, 0]),
                (0.90, [255, 220, 50]),
                (1.00, [255, 255, 230]),
            ],
            Palette::Rainbow => &[
                (0.00, [0, 0, 130]),
                (0.20, [0, 60, 255]),
                (0.40, [0, 220, 220]),
                (0.60, [60, 230, 0]),
                (0.80, [255, 210, 0]),
                (1.00, [230, 0, 0]),
            ],
            Palette::WhiteHot => &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
            Palette::BlackHot => &[(0.0, [255, 255, 255]), (1.0, [0, 0, 0])],
            Palette::Lava => &[
                (0.00, [0, 0, 0]),
                (0.30, [90, 0, 0]),
                (0.60, [220, 40, 0]),
                (0.85, [255, 170, 0]),
                (1.00, [255, 255, 200]),
            ],
        }
    }

    /// Build the 256-entry RGB table.
    pub fn lut(self) -> [[u8; 3]; 256] {
        let stops = self.stops();
        let mut lut = [[0u8; 3]; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            let t = i as f32 / 255.0;
            let k = stops
                .windows(2)
                .position(|w| t <= w[1].0)
                .unwrap_or(stops.len() - 2);
            let (t0, c0) = stops[k];
            let (t1, c1) = stops[k + 1];
            let f = if t1 > t0 {
                ((t - t0) / (t1 - t0)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            for ch in 0..3 {
                entry[ch] = (c0[ch] as f32 + (c1[ch] as f32 - c0[ch] as f32) * f).round() as u8;
            }
        }
        lut
    }
}

/// Map raw values to RGBA through `lut`, with `lo..=hi` (raw units) spread over the table.
pub fn colorize(raw: &[u16], lo: u16, hi: u16, lut: &[[u8; 3]; 256], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(raw.len() * 4);
    let span = (hi.saturating_sub(lo)).max(1) as f32;
    for &v in raw {
        let t = (v.saturating_sub(lo) as f32 / span).min(1.0);
        let c = lut[(t * 255.0).round() as usize];
        out.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
}

/// [`colorize`] for fractional raw values (an upscaled image), with the same mapping.
pub fn colorize_f32(raw: &[f32], lo: u16, hi: u16, lut: &[[u8; 3]; 256], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(raw.len() * 4);
    let span = (hi.saturating_sub(lo)).max(1) as f32;
    let lo = lo as f32;
    for &v in raw {
        let t = ((v - lo) / span).clamp(0.0, 1.0);
        let c = lut[(t * 255.0).round() as usize];
        out.extend_from_slice(&[c[0], c[1], c[2], 255]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_colorize_matches_integer_colorize() {
        let lut = Palette::Ironbow.lut();
        let raw = [0u16, 100, 137, 150, 199, 200, 999];
        let (mut a, mut b) = (Vec::new(), Vec::new());
        colorize(&raw, 100, 200, &lut, &mut a);
        let f: Vec<f32> = raw.iter().map(|&v| v as f32).collect();
        colorize_f32(&f, 100, 200, &lut, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn lut_endpoints_match_stops() {
        for p in Palette::ALL {
            let lut = p.lut();
            let s = p.stops();
            assert_eq!(lut[0], s[0].1, "{p:?} start");
            assert_eq!(lut[255], s[s.len() - 1].1, "{p:?} end");
        }
    }

    #[test]
    fn colorize_clamps_out_of_range() {
        let lut = Palette::WhiteHot.lut();
        let mut out = Vec::new();
        colorize(&[0, 100, 150, 200, 999], 100, 200, &lut, &mut out);
        let px: Vec<u8> = out.chunks(4).map(|c| c[0]).collect();
        assert_eq!(px, vec![0, 0, 128, 255, 255]);
    }
}
