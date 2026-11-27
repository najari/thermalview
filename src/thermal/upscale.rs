//! 4× upscaling of the temperature image for display and snapshots.
//!
//! After stared/thermal-upscale (MIT): undo the sensor blur with a Wiener deconvolution that
//! assumes a Gaussian point-spread function (σ = 0.81 px, the value fitted there for the
//! Thermal Master P3), then enlarge. That tool enlarges with a super-resolution network; here a
//! Lanczos-3 filter does it, so it runs on every frame without a GPU model. Measurements never
//! use the upscaled image.

use serde::{Deserialize, Serialize};

pub const FACTOR: usize = 4;

/// PSF width assumed by the deconvolution, in sensor pixels.
const PSF_SIGMA: f64 = 0.81;
/// Deconvolution kernel radius (11 × 11 taps).
const RADIUS: usize = 5;
/// Grid used to compute the kernel; large enough that its wrap-around is negligible.
const GRID: usize = 64;

/// How strongly to undo the blur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Sharpen {
    Off,
    /// Noise-to-signal ratio 0.01: about 3.4× gain at the finest detail.
    #[default]
    Medium,
    /// Ratio 0.001 as in thermal-upscale: about 16× gain at the finest detail; shows sensor
    /// noise.
    Strong,
}

impl Sharpen {
    pub const ALL: [Sharpen; 3] = [Sharpen::Off, Sharpen::Medium, Sharpen::Strong];

    pub fn label(self) -> &'static str {
        match self {
            Sharpen::Off => "선명화 없음",
            Sharpen::Medium => "선명화 보통",
            Sharpen::Strong => "선명화 강하게",
        }
    }

    fn noise_ratio(self) -> Option<f64> {
        match self {
            Sharpen::Off => None,
            Sharpen::Medium => Some(1e-2),
            Sharpen::Strong => Some(1e-3),
        }
    }
}

/// Display upscaling. Off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpscaleSettings {
    pub enabled: bool,
    pub sharpen: Sharpen,
    /// Also make an AI-upscaled copy of each snapshot with Upscayl, when it is installed.
    pub ai: bool,
}

/// Spatial Wiener filter for a Gaussian PSF: G = H / (H² + K), H the PSF's transfer function
/// (real and even), sampled on a GRID × GRID torus and cut to (2·RADIUS+1)². Sums to 1 so a
/// flat scene keeps its temperature.
fn wiener_kernel(k: f64) -> Vec<f32> {
    use std::f64::consts::PI;
    // 1-D transfer function of the wrapped, normalized Gaussian; the 2-D one is the product.
    let g: Vec<f64> = (0..GRID)
        .map(|i| {
            let d = i.min(GRID - i) as f64;
            (-d * d / (2.0 * PSF_SIGMA * PSF_SIGMA)).exp()
        })
        .collect();
    let norm: f64 = g.iter().sum();
    let h: Vec<f64> = (0..GRID)
        .map(|u| {
            g.iter()
                .enumerate()
                .map(|(x, v)| v * (2.0 * PI * (u * x) as f64 / GRID as f64).cos())
                .sum::<f64>()
                / norm
        })
        .collect();
    let n = 2 * RADIUS + 1;
    let mut kernel = vec![0f64; n * n];
    // cos tables for the inverse transform at offsets -RADIUS..=RADIUS.
    let cos: Vec<Vec<f64>> = (0..n)
        .map(|o| {
            let x = o as f64 - RADIUS as f64;
            (0..GRID)
                .map(|u| (2.0 * PI * u as f64 * x / GRID as f64).cos())
                .collect()
        })
        .collect();
    for v in 0..GRID {
        for u in 0..GRID {
            let hh = h[u] * h[v];
            let gain = hh / (hh * hh + k);
            for oy in 0..n {
                let cy = gain * cos[oy][v];
                for ox in 0..n {
                    kernel[oy * n + ox] += cy * cos[ox][u];
                }
            }
        }
    }
    let sum: f64 = kernel.iter().sum();
    kernel.iter().map(|&v| (v / sum) as f32).collect()
}

fn lanczos3(x: f32) -> f32 {
    let x = x.abs();
    if x < 1e-6 {
        1.0
    } else if x >= 3.0 {
        0.0
    } else {
        let px = std::f32::consts::PI * x;
        3.0 * px.sin() * (px / 3.0).sin() / (px * px)
    }
}

/// For each output index: first source index and the 6 normalized Lanczos weights.
fn taps(src_len: usize) -> Vec<(isize, [f32; 6])> {
    (0..src_len * FACTOR)
        .map(|o| {
            let x = (o as f32 + 0.5) / FACTOR as f32 - 0.5;
            let i0 = x.floor();
            let mut w = [0f32; 6];
            for (k, wk) in w.iter_mut().enumerate() {
                *wk = lanczos3(x - (i0 + k as f32 - 2.0));
            }
            let s: f32 = w.iter().sum();
            w.iter_mut().for_each(|v| *v /= s);
            (i0 as isize - 2, w)
        })
        .collect()
}

/// Upscales one image size with one sharpening setting; reuses its tables between frames.
pub struct Upscaler {
    sharpen: Sharpen,
    kernel: Vec<f32>,
    size: (usize, usize),
    cols: Vec<(isize, [f32; 6])>,
    rows: Vec<(isize, [f32; 6])>,
}

impl Upscaler {
    pub fn new(sharpen: Sharpen) -> Self {
        Self {
            sharpen,
            kernel: sharpen.noise_ratio().map(wiener_kernel).unwrap_or_default(),
            size: (0, 0),
            cols: Vec::new(),
            rows: Vec::new(),
        }
    }

    pub fn sharpen(&self) -> Sharpen {
        self.sharpen
    }

    /// The deconvolved image at sensor size (a copy when sharpening is off). Used as the input
    /// of the AI upscaler, as thermal-upscale does.
    pub fn deconvolve(&self, src: &[u16], w: usize, h: usize) -> Vec<f32> {
        if self.kernel.is_empty() {
            return src.iter().map(|&v| v as f32).collect();
        }
        let n = 2 * RADIUS + 1;
        // Copy with RADIUS pixels of edge repeated on every side, so the loop needs no bounds.
        let pw = w + 2 * RADIUS;
        let clamp = |i: usize, len: usize| i.saturating_sub(RADIUS).min(len - 1);
        let padded: Vec<f32> = (0..h + 2 * RADIUS)
            .flat_map(|py| {
                let row = &src[clamp(py, h) * w..][..w];
                (0..pw).map(move |px| row[clamp(px, w)] as f32)
            })
            .collect();
        let mut out = vec![0f32; w * h];
        for y in 0..h {
            let line = &mut out[y * w..][..w];
            for ky in 0..n {
                let src_row = &padded[(y + ky) * pw..][..pw];
                for (kx, &kv) in self.kernel[ky * n..][..n].iter().enumerate() {
                    for (d, s) in line.iter_mut().zip(&src_row[kx..kx + w]) {
                        *d += kv * s;
                    }
                }
            }
        }
        out
    }

    /// Raw counts (w × h) → fractional raw counts (FACTOR·w × FACTOR·h).
    pub fn upscale(&mut self, src: &[u16], w: usize, h: usize) -> Vec<f32> {
        debug_assert_eq!(src.len(), w * h);
        if self.size != (w, h) {
            self.size = (w, h);
            self.cols = taps(w);
            self.rows = taps(h);
        }
        let sharp = self.deconvolve(src, w, h);
        let ow = w * FACTOR;
        let at = |i: isize, len: usize| i.clamp(0, len as isize - 1) as usize;
        // Horizontal pass: h × ow.
        let mut mid = vec![0f32; h * ow];
        for y in 0..h {
            let row = &sharp[y * w..y * w + w];
            for (o, (i0, wt)) in self.cols.iter().enumerate() {
                mid[y * ow + o] = (0..6).map(|k| wt[k] * row[at(i0 + k as isize, w)]).sum();
            }
        }
        // Vertical pass: oh × ow.
        let mut out = vec![0f32; h * FACTOR * ow];
        for (oy, (i0, wt)) in self.rows.iter().enumerate() {
            let line = &mut out[oy * ow..oy * ow + ow];
            for (k, wk) in wt.iter().enumerate() {
                let src_row = &mid[at(i0 + k as isize, h) * ow..][..ow];
                for (d, s) in line.iter_mut().zip(src_row) {
                    *d += wk * s;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernels_keep_flat_scenes_and_sharpen_more_when_stronger() {
        let medium = wiener_kernel(1e-2);
        let strong = wiener_kernel(1e-3);
        for k in [&medium, &strong] {
            assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-5);
            // Symmetric.
            let n = 2 * RADIUS + 1;
            for y in 0..n {
                for x in 0..n {
                    assert!((k[y * n + x] - k[x * n + y]).abs() < 1e-6);
                    assert!((k[y * n + x] - k[(n - 1 - y) * n + (n - 1 - x)]).abs() < 1e-6);
                }
            }
        }
        let centre = RADIUS * (2 * RADIUS + 1) + RADIUS;
        assert!(strong[centre] > medium[centre] && medium[centre] > 1.0);
        // The kernel is nearly all inside the 11 × 11 cut: the corner taps are tiny.
        assert!(strong[0].abs() < 0.01 * strong[centre]);
    }

    #[test]
    fn flat_image_stays_flat_and_sizes_are_four_times() {
        for s in Sharpen::ALL {
            let mut u = Upscaler::new(s);
            let out = u.upscale(&[20_000; 8 * 6], 8, 6);
            assert_eq!(out.len(), 32 * 24);
            assert!(out.iter().all(|v| (v - 20_000.0).abs() < 0.05), "{s:?}");
        }
    }

    #[test]
    fn an_edge_is_kept_in_place_and_sharpening_steepens_it() {
        // Left half 1000, right half 2000.
        let (w, h) = (16, 4);
        let src: Vec<u16> = (0..w * h)
            .map(|i| if i % w < w / 2 { 1000 } else { 2000 })
            .collect();
        let slope = |s: Sharpen| {
            let out = Upscaler::new(s).upscale(&src, w, h);
            let row = &out[(h * FACTOR / 2) * w * FACTOR..][..w * FACTOR];
            // Edge between source columns 7 and 8 → output columns 31 and 32.
            assert!(row[31] < 1500.0 && row[32] > 1500.0, "{s:?}");
            assert!(row[0] < 1010.0 && row[w * FACTOR - 1] > 1990.0);
            row[33] - row[30]
        };
        assert!(slope(Sharpen::Strong) > slope(Sharpen::Medium));
        assert!(slope(Sharpen::Medium) > slope(Sharpen::Off));
    }

    #[test]
    fn a_hot_pixel_stays_where_it_was() {
        let (w, h) = (9, 9);
        let mut src = vec![1000u16; w * h];
        src[4 * w + 4] = 3000;
        let out = Upscaler::new(Sharpen::Medium).upscale(&src, w, h);
        let ow = w * FACTOR;
        let (i, _) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        // Source pixel 4 covers output 16..20.
        assert!((16..20).contains(&(i % ow)) && (16..20).contains(&(i / ow)));
    }
}
