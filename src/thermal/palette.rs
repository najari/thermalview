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

    /// The table, or the same table hot end first.
    pub fn lut_of(self, inverted: bool) -> [[u8; 3]; 256] {
        let mut lut = self.lut();
        if inverted {
            lut.reverse();
        }
        lut
    }
}

/// Paint the pixels whose raw value lies in `lo..=hi` in one color, over an image made by
/// [`colorize`] (an isotherm). Returns how many pixels were painted.
pub fn paint_band(raw: &[u16], lo: u16, hi: u16, color: [u8; 3], rgba: &mut [u8]) -> usize {
    let mut painted = 0;
    for (&v, px) in raw.iter().zip(rgba.as_chunks_mut::<4>().0) {
        if (lo..=hi).contains(&v) {
            px[..3].copy_from_slice(&color);
            painted += 1;
        }
    }
    painted
}

/// [`paint_band`] for fractional raw values (an upscaled image).
pub fn paint_band_f32(raw: &[f32], lo: u16, hi: u16, color: [u8; 3], rgba: &mut [u8]) {
    let (lo, hi) = (lo as f32, hi as f32);
    for (&v, px) in raw.iter().zip(rgba.as_chunks_mut::<4>().0) {
        if v >= lo && v <= hi {
            px[..3].copy_from_slice(&color);
        }
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
    fn band_is_painted_over_both_kinds_of_image() {
        let lut = Palette::WhiteHot.lut();
        let raw = [90u16, 100, 150, 200, 210];
        let mut rgba = Vec::new();
        colorize(&raw, 0, 255, &lut, &mut rgba);
        let green = [0, 255, 0];
        assert_eq!(paint_band(&raw, 100, 200, green, &mut rgba), 3);
        let px: Vec<[u8; 4]> = rgba.as_chunks::<4>().0.to_vec();
        assert_eq!(px[0], [90, 90, 90, 255]);
        assert_eq!(px[1], [0, 255, 0, 255]);
        assert_eq!(px[3], [0, 255, 0, 255]);
        assert_eq!(px[4], [210, 210, 210, 255]);

        let f: Vec<f32> = vec![99.4, 100.0, 200.0, 200.6];
        let mut up = vec![7u8; 16];
        paint_band_f32(&f, 100, 200, green, &mut up);
        let px: Vec<[u8; 4]> = up.as_chunks::<4>().0.to_vec();
        assert_eq!(px, [[7; 4], [0, 255, 0, 7], [0, 255, 0, 7], [7; 4]]);
    }

    #[test]
    fn inverted_table_runs_backwards() {
        let (lut, inv) = (Palette::Ironbow.lut(), Palette::Ironbow.lut_of(true));
        assert_eq!(inv[0], lut[255]);
        assert_eq!(inv[200], lut[55]);
        assert_eq!(Palette::Ironbow.lut_of(false), lut);
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
