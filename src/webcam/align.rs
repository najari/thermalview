//! Automatic alignment: finds the size and place of the webcam picture on the thermal image.
//!
//! Both cameras see the same outlines: a warm body in front of a cooler wall is an edge in the
//! thermal image and, usually, in the webcam picture too, though with a different strength. So
//! the search compares edge maps. For each candidate size and place of the webcam picture it
//! takes the correlation between the thermal edges and the webcam edges lying on them. All sizes
//! and places are tried on small images; the best few candidates are then refined on finer ones.
//! Pure code, unit-tested.

use super::blend::Alignment;

/// Result of a search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Found {
    pub align: Alignment,
    /// Correlation of the two edge maps there, -1..=1. See [`MIN_SCORE`].
    pub score: f32,
}

impl Found {
    /// Whether the match is good enough to use.
    pub fn trusted(&self) -> bool {
        self.score >= MIN_SCORE
    }
}

/// Below this correlation the found place is not trusted: the scene has too few clear
/// outlines, or the two pictures show different things.
pub const MIN_SCORE: f32 = 0.25;

/// Width of the thermal image in the first, full search.
const COARSE_WIDTH: usize = 40;
/// Ratio between neighbouring sizes in the first search.
const SCALE_STEP: f32 = 1.04;
/// The webcam picture must cover at least this share of the thermal image.
const MIN_COVER: f32 = 0.5;
/// Candidates of the first search that are refined.
const KEEP: usize = 6;
/// Places tried around a candidate when refining, in pixels each way.
const REFINE_REACH: i32 = 3;

/// Grayscale image.
struct Img {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

impl Img {
    /// Shrink (or enlarge) to `w`×`h` by averaging the source pixels each target pixel covers.
    fn resize(&self, w: usize, h: usize) -> Img {
        let spans = |src: usize, dst: usize| -> Vec<(usize, usize)> {
            (0..dst)
                .map(|i| {
                    let a = i * src / dst;
                    let b = ((i + 1) * src).div_ceil(dst).clamp(a + 1, src);
                    (a, b)
                })
                .collect()
        };
        let (xs, ys) = (spans(self.w, w), spans(self.h, h));
        let mut px = Vec::with_capacity(w * h);
        for &(y0, y1) in &ys {
            for &(x0, x1) in &xs {
                let mut sum = 0.0;
                for y in y0..y1 {
                    sum += self.px[y * self.w + x0..y * self.w + x1]
                        .iter()
                        .sum::<f32>();
                }
                px.push(sum / ((y1 - y0) * (x1 - x0)) as f32);
            }
        }
        Img { w, h, px }
    }

    /// Edge direction field (see [`Edges`]).
    fn edges(&self) -> Edges {
        let (w, h) = (self.w, self.h);
        let (mut gx, mut gy) = (vec![0f32; w * h], vec![0f32; w * h]);
        if w >= 3 && h >= 3 {
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let p = |dx: usize, dy: usize| self.px[(y + dy - 1) * w + (x + dx - 1)];
                    gx[y * w + x] =
                        (p(2, 0) + 2.0 * p(2, 1) + p(2, 2)) - (p(0, 0) + 2.0 * p(0, 1) + p(0, 2));
                    gy[y * w + x] =
                        (p(0, 2) + 2.0 * p(1, 2) + p(2, 2)) - (p(0, 0) + 2.0 * p(1, 0) + p(2, 0));
                }
            }
        }
        let mag: Vec<f32> = gx
            .iter()
            .zip(&gy)
            .map(|(a, b)| (a * a + b * b).sqrt())
            .collect();
        let mean = mag.iter().sum::<f32>() / mag.len().max(1) as f32;
        let (mut c, mut s) = (vec![0f32; w * h], vec![0f32; w * h]);
        if mean > 0.0 {
            for i in 0..w * h {
                if mag[i] > 0.0 {
                    // Length g / (g + mean g), angle doubled.
                    let k = 1.0 / ((mag[i] + mean) * mag[i]);
                    c[i] = (gx[i] * gx[i] - gy[i] * gy[i]) * k;
                    s[i] = 2.0 * gx[i] * gy[i] * k;
                }
            }
        }
        Edges {
            w,
            h,
            c: blur3(&c, w, h),
            s: blur3(&s, w, h),
        }
    }
}

/// 3×3 box blur (the border averages what exists).
fn blur3(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (mut sum, mut n) = (0.0, 0.0);
            for yy in y.saturating_sub(1)..(y + 2).min(h) {
                for xx in x.saturating_sub(1)..(x + 2).min(w) {
                    sum += v[yy * w + xx];
                    n += 1.0;
                }
            }
            out[y * w + x] = sum / n;
        }
    }
    out
}

/// Edges as a field of double-angle vectors: per pixel (cos 2θ, sin 2θ) of the gradient angle
/// θ, with length g / (g + mean g). Doubling the angle makes an edge from warm to cold and one
/// from cold to warm alike (a warm body can look darker or brighter than the wall behind it).
/// The dot product of two such vectors is +1 for parallel edges, −1 for crossing ones and 0 on
/// average for unrelated texture, so only outlines both cameras see add up. The length
/// compression keeps one very strong outline (a hot spot) from outweighing all others, and the
/// 3×3 blur lets outlines a pixel apart still meet.
struct Edges {
    w: usize,
    h: usize,
    c: Vec<f32>,
    s: Vec<f32>,
}

/// How well the edges of `t` and of `e` (placed with its top-left corner at (ox, oy) of `t`)
/// agree over the part they share: Σ a·b / √(Σ|a|² Σ|b|²), −1..=1. `None` when they share too
/// little or one side has no edges there.
fn correlation(t: &Edges, e: &Edges, ox: i32, oy: i32) -> Option<f32> {
    let x0 = ox.max(0) as usize;
    let y0 = oy.max(0) as usize;
    let x1 = (ox + e.w as i32).min(t.w as i32);
    let y1 = (oy + e.h as i32).min(t.h as i32);
    if x1 <= x0 as i32 || y1 <= y0 as i32 {
        return None;
    }
    let (x1, y1) = (x1 as usize, y1 as usize);
    let n = (x1 - x0) * (y1 - y0);
    if (n as f32) < MIN_COVER * (t.w * t.h) as f32 {
        return None;
    }
    let (mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64);
    let ex0 = (x0 as i32 - ox) as usize;
    let len = x1 - x0;
    for y in y0..y1 {
        let ey = (y as i32 - oy) as usize;
        let (ti, ei) = (y * t.w + x0, ey * e.w + ex0);
        let (ac, as_) = (&t.c[ti..ti + len], &t.s[ti..ti + len]);
        let (bc, bs) = (&e.c[ei..ei + len], &e.s[ei..ei + len]);
        let (mut raa, mut rbb, mut rab) = (0f32, 0f32, 0f32);
        for i in 0..len {
            raa += ac[i] * ac[i] + as_[i] * as_[i];
            rbb += bc[i] * bc[i] + bs[i] * bs[i];
            rab += ac[i] * bc[i] + as_[i] * bs[i];
        }
        saa += raa as f64;
        sbb += rbb as f64;
        sab += rab as f64;
    }
    let tiny = 1e-6 * n as f64;
    if saa <= tiny || sbb <= tiny {
        return None;
    }
    Some((sab / (saa * sbb).sqrt()) as f32)
}

/// The thermal edges at one resolution.
struct Level {
    edges: Edges,
}

/// The webcam edges for one size on a level.
struct Canvas {
    edges: Edges,
}

/// The search over one pair of pictures.
struct Search<'a> {
    webcam: &'a Img,
    /// Webcam height / width.
    aspect: f32,
}

impl Search<'_> {
    /// Webcam picture size on a level for a scale (picture width / thermal width).
    fn size(&self, lv: &Level, scale: f32) -> (usize, usize) {
        let cw = ((scale * lv.edges.w as f32).round() as usize).max(3);
        let ch = ((cw as f32 * self.aspect).round() as usize).max(3);
        (cw, ch)
    }

    fn canvas(&self, lv: &Level, scale: f32) -> Canvas {
        let (cw, ch) = self.size(lv, scale);
        Canvas {
            edges: self.webcam.resize(cw, ch).edges(),
        }
    }

    /// Alignment for a canvas whose top-left corner is at (ox, oy) on the level; `None` if
    /// the sliders could not show it.
    fn alignment(lv: &Level, c: &Canvas, ox: i32, oy: i32) -> Option<Alignment> {
        let (w, h) = (lv.edges.w as f32, lv.edges.h as f32);
        let a = Alignment {
            scale: c.edges.w as f32 / w,
            dx: (ox as f32 + c.edges.w as f32 / 2.0) / w - 0.5,
            dy: (oy as f32 + c.edges.h as f32 / 2.0) / h - 0.5,
        };
        let ok = Alignment::SCALE.contains(&a.scale)
            && Alignment::SHIFT.contains(&a.dx)
            && Alignment::SHIFT.contains(&a.dy);
        ok.then_some(a)
    }

    /// Top-left corner of the canvas on the level for an alignment.
    fn corner(lv: &Level, c: &Canvas, a: &Alignment) -> (i32, i32) {
        let (w, h) = (lv.edges.w as f32, lv.edges.h as f32);
        (
            ((a.dx + 0.5) * w - c.edges.w as f32 / 2.0).round() as i32,
            ((a.dy + 0.5) * h - c.edges.h as f32 / 2.0).round() as i32,
        )
    }

    /// Every size and place on the level; the best matches, each different from the others.
    fn coarse(&self, lv: &Level) -> Vec<Found> {
        let mut all = Vec::new();
        let (lo, hi) = (*Alignment::SCALE.start(), *Alignment::SCALE.end());
        let mut scale = lo;
        while scale <= hi * 1.0001 {
            let c = self.canvas(lv, scale);
            // Corners that keep the center shift inside Alignment::SHIFT.
            let (cw, ch) = (c.edges.w as i32, c.edges.h as i32);
            let (w, h) = (lv.edges.w as i32, lv.edges.h as i32);
            for oy in (-ch / 2)..=(h - ch / 2) {
                for ox in (-cw / 2)..=(w - cw / 2) {
                    let Some(align) = Self::alignment(lv, &c, ox, oy) else {
                        continue;
                    };
                    if let Some(score) = correlation(&lv.edges, &c.edges, ox, oy) {
                        all.push(Found { align, score });
                    }
                }
            }
            scale *= SCALE_STEP;
        }
        all.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut keep: Vec<Found> = Vec::new();
        for f in all {
            let near = keep.iter().any(|k| {
                let r = f.align.scale / k.align.scale;
                (0.9..1.1).contains(&r)
                    && (f.align.dx - k.align.dx).abs() < 0.08
                    && (f.align.dy - k.align.dy).abs() < 0.08
            });
            if !near {
                keep.push(f);
                if keep.len() == KEEP {
                    break;
                }
            }
        }
        keep
    }

    /// The best size and place near `start` on a finer level. Sizes are tried in steps of
    /// `step` (relative).
    fn refine(&self, lv: &Level, start: Found, step: f32) -> Found {
        let mut best: Option<Found> = None;
        for k in -3..=3 {
            let scale = (start.align.scale * (1.0 + k as f32 * step))
                .clamp(*Alignment::SCALE.start(), *Alignment::SCALE.end());
            let c = self.canvas(lv, scale);
            let (cx, cy) = Self::corner(lv, &c, &start.align);
            for oy in cy - REFINE_REACH..=cy + REFINE_REACH {
                for ox in cx - REFINE_REACH..=cx + REFINE_REACH {
                    let Some(align) = Self::alignment(lv, &c, ox, oy) else {
                        continue;
                    };
                    let Some(score) = correlation(&lv.edges, &c.edges, ox, oy) else {
                        continue;
                    };
                    if best.is_none_or(|b| score > b.score) {
                        best = Some(Found { align, score });
                    }
                }
            }
        }
        best.unwrap_or(start)
    }
}

/// Find where the webcam picture (`rgba`, `pw`×`ph`) lies on the thermal image (`thermal`,
/// `tw`×`th`, any unit that rises with temperature). `None` if the pictures are too small or
/// featureless to compare. The result may still be poor: check [`Found::trusted`].
pub fn find(
    thermal: &[f32],
    tw: usize,
    th: usize,
    rgba: &[u8],
    pw: usize,
    ph: usize,
) -> Option<Found> {
    let (thermal, webcam) = prepare(thermal, tw, th, rgba, pw, ph)?;
    let mut levels = Vec::new();
    let mut w = COARSE_WIDTH.min(tw);
    loop {
        let h = ((th * w) as f32 / tw as f32).round().max(3.0) as usize;
        levels.push(Level {
            edges: thermal.resize(w, h).edges(),
        });
        if w == tw {
            break;
        }
        w = (w * 2).min(tw);
    }
    let search = Search {
        webcam: &webcam,
        aspect: ph as f32 / pw as f32,
    };
    search
        .coarse(&levels[0])
        .into_iter()
        .map(|mut f| {
            let mut step = 0.02;
            for lv in &levels[1..] {
                f = search.refine(lv, f, step);
                step /= 2.0;
            }
            f
        })
        .max_by(|a, b| a.score.total_cmp(&b.score))
}

/// The score [`find`] would give `align` on the full-size thermal image. `None` as in [`find`],
/// or when the webcam picture covers too little of the thermal image there.
pub fn score_at(
    thermal: &[f32],
    tw: usize,
    th: usize,
    rgba: &[u8],
    pw: usize,
    ph: usize,
    align: &Alignment,
) -> Option<f32> {
    let (thermal, webcam) = prepare(thermal, tw, th, rgba, pw, ph)?;
    let lv = Level {
        edges: thermal.edges(),
    };
    let search = Search {
        webcam: &webcam,
        aspect: ph as f32 / pw as f32,
    };
    let c = search.canvas(&lv, align.scale);
    let (ox, oy) = Search::corner(&lv, &c, align);
    correlation(&lv.edges, &c.edges, ox, oy)
}

/// Checked inputs as images (the webcam as luma).
fn prepare(
    thermal: &[f32],
    tw: usize,
    th: usize,
    rgba: &[u8],
    pw: usize,
    ph: usize,
) -> Option<(Img, Img)> {
    if tw < 16 || th < 12 || pw < 16 || ph < 12 {
        return None;
    }
    if thermal.len() != tw * th || rgba.len() != pw * ph * 4 {
        return None;
    }
    let thermal = Img {
        w: tw,
        h: th,
        px: thermal.to_vec(),
    };
    let webcam = Img {
        w: pw,
        h: ph,
        px: rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
            .collect(),
    };
    Some((thermal, webcam))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up scene: a few rectangles and a disc of different brightness.
    fn scene(x: f32, y: f32) -> f32 {
        let mut v = 20.0;
        if (0.1..0.35).contains(&x) && (0.2..0.8).contains(&y) {
            v += 60.0;
        }
        if (0.55..0.9).contains(&x) && (0.15..0.45).contains(&y) {
            v += 120.0;
        }
        if (x - 0.65).powi(2) + (y - 0.7).powi(2) < 0.015 {
            v += 90.0;
        }
        if (0.3..0.6).contains(&x) && (0.85..0.95).contains(&y) {
            v += 40.0;
        }
        v
    }

    /// The webcam sees the scene over [0,1]²; the thermal camera sees part of it, as the given
    /// alignment says. Returns (thermal, webcam rgba).
    fn pair(truth: Alignment, tw: usize, th: usize, pw: usize, ph: usize) -> (Vec<f32>, Vec<u8>) {
        let mut rgba = Vec::with_capacity(pw * ph * 4);
        for y in 0..ph {
            for x in 0..pw {
                let v = scene((x as f32 + 0.5) / pw as f32, (y as f32 + 0.5) / ph as f32);
                let v = v.min(255.0) as u8;
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let [x0, y0, x1, y1] = truth.placement((tw, th), (pw, ph));
        let mut thermal = Vec::with_capacity(tw * th);
        for y in 0..th {
            for x in 0..tw {
                let u = (x as f32 + 0.5 - x0) / (x1 - x0);
                let v = (y as f32 + 0.5 - y0) / (y1 - y0);
                // A different response than the webcam: brightness reversed and scaled.
                thermal.push(1000.0 - 3.0 * scene(u, v));
            }
        }
        (thermal, rgba)
    }

    #[test]
    fn finds_a_known_alignment() {
        let truth = Alignment {
            scale: 1.4,
            dx: 0.06,
            dy: -0.04,
        };
        let (tw, th, pw, ph) = (80, 60, 200, 150);
        let (thermal, rgba) = pair(truth, tw, th, pw, ph);
        let f = find(&thermal, tw, th, &rgba, pw, ph).expect("found");
        assert!(f.trusted(), "{f:?}");
        assert!((f.align.scale / truth.scale - 1.0).abs() < 0.03, "{f:?}");
        assert!((f.align.dx - truth.dx).abs() < 0.02, "{f:?}");
        assert!((f.align.dy - truth.dy).abs() < 0.02, "{f:?}");
    }

    #[test]
    fn featureless_or_unrelated_pictures_are_not_trusted() {
        let (tw, th, pw, ph) = (64, 48, 160, 120);
        let flat = vec![300.0; tw * th];
        let rgba = [90u8, 90, 90, 255].repeat(pw * ph);
        assert!(find(&flat, tw, th, &rgba, pw, ph).is_none());

        // Thermal noise against the test scene: nothing in common.
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..tw * th)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                300.0 + (seed >> 16) as f32 / 6553.6
            })
            .collect();
        let (_, scene) = pair(Alignment::default(), tw, th, pw, ph);
        if let Some(f) = find(&noise, tw, th, &scene, pw, ph) {
            assert!(!f.trusted(), "{f:?}");
        }
        assert!(find(&flat, 8, 6, &rgba, pw, ph).is_none(), "too small");
    }

    #[test]
    fn resize_averages() {
        let img = Img {
            w: 4,
            h: 2,
            px: vec![0.0, 2.0, 4.0, 6.0, 0.0, 2.0, 4.0, 6.0],
        };
        let small = img.resize(2, 1);
        assert_eq!(small.px, vec![1.0, 5.0]);
        let big = img.resize(8, 2);
        assert_eq!(big.px[..4], [0.0, 0.0, 2.0, 2.0]);
    }
}
