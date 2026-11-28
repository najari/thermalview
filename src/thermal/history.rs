//! Time series of frame statistics and spot values, kept for a bounded time window.

use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Seconds since the history started.
    pub t: f64,
    pub max: f32,
    pub min: f32,
    pub mean: f32,
    pub center: f32,
}

#[derive(Debug, Clone)]
pub struct History {
    /// Oldest samples are dropped once they are older than this (seconds).
    pub keep_secs: f64,
    samples: VecDeque<Sample>,
    /// Per-spot series keyed by spot id: (t, value).
    spots: BTreeMap<u32, VecDeque<(f64, f32)>>,
    /// Per Box/Line series keyed by shape id: (t, [maximum, mean]).
    shapes: BTreeMap<u32, VecDeque<(f64, [f32; 2])>>,
}

/// Which statistic of a Box or Line to plot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeValue {
    Max = 0,
    Mean = 1,
}

impl Default for History {
    fn default() -> Self {
        Self::new(30.0 * 60.0)
    }
}

impl History {
    pub fn new(keep_secs: f64) -> Self {
        Self {
            keep_secs,
            samples: VecDeque::new(),
            spots: BTreeMap::new(),
            shapes: BTreeMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.spots.clear();
        self.shapes.clear();
    }

    /// Add one frame: its statistics, the spot values as (id, value) and the Box/Line values
    /// as (id, [maximum, mean]).
    pub fn push(
        &mut self,
        s: Sample,
        spot_values: &[(u32, f32)],
        shape_values: &[(u32, [f32; 2])],
    ) {
        // Time went back (seeking back or replaying a recording): the series only make sense
        // in time order, so start over.
        if self.latest_t().is_some_and(|last| s.t < last) {
            self.clear();
        }
        self.samples.push_back(s);
        for &(id, v) in spot_values {
            self.spots.entry(id).or_default().push_back((s.t, v));
        }
        for &(id, v) in shape_values {
            self.shapes.entry(id).or_default().push_back((s.t, v));
        }
        let cutoff = s.t - self.keep_secs;
        while self.samples.front().is_some_and(|f| f.t < cutoff) {
            self.samples.pop_front();
        }
        self.spots.retain(|_, series| {
            while series.front().is_some_and(|(t, _)| *t < cutoff) {
                series.pop_front();
            }
            !series.is_empty()
        });
        self.shapes.retain(|_, series| {
            while series.front().is_some_and(|(t, _)| *t < cutoff) {
                series.pop_front();
            }
            !series.is_empty()
        });
    }

    /// Forget the series of spots that no longer exist.
    pub fn retain_spots(&mut self, ids: &[u32]) {
        self.spots.retain(|id, _| ids.contains(id));
    }

    /// Forget the series of Boxes and Lines that no longer exist.
    pub fn retain_shapes(&mut self, ids: &[u32]) {
        self.shapes.retain(|id, _| ids.contains(id));
    }

    pub fn shape_points(&self, id: u32, window: f64, value: ShapeValue) -> Vec<[f64; 2]> {
        let (Some(now), Some(series)) = (self.latest_t(), self.shapes.get(&id)) else {
            return Vec::new();
        };
        let start = series.partition_point(|(t, _)| now - t > window);
        let points = series.range(start..);
        thin(
            points.len(),
            points.map(|(t, v)| [t - now, v[value as usize] as f64]),
        )
    }

    pub fn samples(&self) -> &VecDeque<Sample> {
        &self.samples
    }

    pub fn spot_series(&self) -> impl Iterator<Item = (u32, &VecDeque<(f64, f32)>)> {
        self.spots.iter().map(|(id, s)| (*id, s))
    }

    pub fn latest_t(&self) -> Option<f64> {
        self.samples.back().map(|s| s.t)
    }

    /// Points of one statistic within the last `window` seconds, as plot coordinates
    /// (x = seconds relative to the newest sample, so "now" is 0).
    ///
    /// A long window holds far more samples than a plot is wide (30 minutes at 25 frames a
    /// second is 45,000), so the result is thinned to about [`MAX_POINTS`].
    pub fn series(&self, window: f64, pick: impl Fn(&Sample) -> f32) -> Vec<[f64; 2]> {
        let Some(now) = self.latest_t() else {
            return Vec::new();
        };
        // Samples are in time order: skip the ones before the window without visiting them.
        let start = self.samples.partition_point(|s| now - s.t > window);
        let points = self.samples.range(start..);
        thin(points.len(), points.map(|s| [s.t - now, pick(s) as f64]))
    }

    pub fn spot_points(&self, id: u32, window: f64) -> Vec<[f64; 2]> {
        let (Some(now), Some(series)) = (self.latest_t(), self.spots.get(&id)) else {
            return Vec::new();
        };
        let start = series.partition_point(|(t, _)| now - t > window);
        let points = series.range(start..);
        thin(points.len(), points.map(|(t, v)| [t - now, *v as f64]))
    }
}

/// Most points one plotted line gets.
pub const MAX_POINTS: usize = 2000;

/// At most about [`MAX_POINTS`] of `n` points: each run of neighbours is reduced to its lowest
/// and highest value, in time order, so a short peak stays visible.
fn thin(n: usize, points: impl Iterator<Item = [f64; 2]>) -> Vec<[f64; 2]> {
    if n <= MAX_POINTS {
        return points.collect();
    }
    let per = n.div_ceil(MAX_POINTS / 2);
    let mut out = Vec::with_capacity(MAX_POINTS + 2);
    let mut flush = |lo: [f64; 2], hi: [f64; 2]| {
        let (first, second) = if lo[0] <= hi[0] { (lo, hi) } else { (hi, lo) };
        out.push(first);
        if second != first {
            out.push(second);
        }
    };
    let mut run: Option<([f64; 2], [f64; 2], usize)> = None;
    let mut newest = None;
    for p in points {
        newest = Some(p);
        let (lo, hi, count) = run.get_or_insert((p, p, 0));
        if p[1] < lo[1] {
            *lo = p;
        }
        if p[1] > hi[1] {
            *hi = p;
        }
        *count += 1;
        if *count == per {
            flush(*lo, *hi);
            run = None;
        }
    }
    if let Some((lo, hi, _)) = run {
        flush(lo, hi);
    }
    // The line must still end at "now".
    if let Some(p) = newest.filter(|p| out.last() != Some(p)) {
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, v: f32) -> Sample {
        Sample {
            t,
            max: v,
            min: v,
            mean: v,
            center: v,
        }
    }

    #[test]
    fn drops_old_samples_and_empty_spots() {
        let mut h = History::new(10.0);
        h.push(s(0.0, 1.0), &[(1, 5.0)], &[(4, [9.0, 8.0])]);
        h.push(s(5.0, 2.0), &[(2, 6.0)], &[(5, [9.5, 8.5])]);
        h.push(s(12.0, 3.0), &[(2, 7.0)], &[(5, [9.75, 8.75])]);
        assert_eq!(h.samples().len(), 2);
        assert_eq!(
            h.spot_series().map(|(id, _)| id).collect::<Vec<_>>(),
            vec![2]
        );
        // The Box that was deleted before the last frames is gone with its old samples.
        assert!(h.shape_points(4, 100.0, ShapeValue::Max).is_empty());
        assert_eq!(
            h.shape_points(5, 100.0, ShapeValue::Mean),
            vec![[-7.0, 8.5], [0.0, 8.75]]
        );
        assert_eq!(h.shape_points(5, 1.0, ShapeValue::Max), vec![[0.0, 9.75]]);
        h.retain_shapes(&[]);
        assert!(h.shape_points(5, 100.0, ShapeValue::Max).is_empty());
    }

    #[test]
    fn series_is_relative_to_now() {
        let mut h = History::new(100.0);
        for i in 0..5 {
            h.push(s(i as f64, i as f32), &[(1, 10.0 + i as f32)], &[]);
        }
        assert_eq!(
            h.series(2.0, |s| s.max),
            vec![[-2.0, 2.0], [-1.0, 3.0], [0.0, 4.0]]
        );
        assert_eq!(h.spot_points(1, 1.0), vec![[-1.0, 13.0], [0.0, 14.0]]);
    }

    #[test]
    fn time_going_back_starts_over() {
        let mut h = History::new(100.0);
        for i in 0..5 {
            h.push(s(i as f64, 1.0), &[(1, 2.0)], &[(3, [5.0, 4.0])]);
        }
        // Seeking back in a recording.
        h.push(s(2.0, 9.0), &[(1, 8.0)], &[(3, [7.0, 6.0])]);
        assert_eq!(h.series(100.0, |s| s.max), vec![[0.0, 9.0]]);
        assert_eq!(h.spot_points(1, 100.0), vec![[0.0, 8.0]]);
        assert_eq!(h.shape_points(3, 100.0, ShapeValue::Max), vec![[0.0, 7.0]]);
    }

    #[test]
    fn long_series_are_thinned_but_keep_peaks() {
        let mut h = History::new(4000.0);
        let n = 45_000;
        for i in 0..n {
            // Flat, with one sample-wide peak and one dip.
            let v = match i {
                20_000 => 90.0,
                30_001 => -5.0,
                _ => 20.0,
            };
            h.push(s(i as f64 * 0.04, v), &[(1, v)], &[]);
        }
        for pts in [h.series(4000.0, |s| s.max), h.spot_points(1, 4000.0)] {
            assert!(pts.len() <= MAX_POINTS + 2, "{}", pts.len());
            assert!(pts.windows(2).all(|w| w[0][0] <= w[1][0]), "time order");
            assert!(pts.iter().any(|p| p[1] == 90.0) && pts.iter().any(|p| p[1] == -5.0));
            assert_eq!(pts.last().unwrap()[0], 0.0, "the newest sample is kept");
        }
        // A short window is not touched.
        assert_eq!(h.series(4.01, |s| s.max).len(), 101);
    }
}
