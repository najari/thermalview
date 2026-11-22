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
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.spots.clear();
    }

    pub fn push(&mut self, s: Sample, spot_values: &[(u32, f32)]) {
        self.samples.push_back(s);
        for &(id, v) in spot_values {
            self.spots.entry(id).or_default().push_back((s.t, v));
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
    }

    /// Forget the series of spots that no longer exist.
    pub fn retain_spots(&mut self, ids: &[u32]) {
        self.spots.retain(|id, _| ids.contains(id));
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
    pub fn series(&self, window: f64, pick: impl Fn(&Sample) -> f32) -> Vec<[f64; 2]> {
        let Some(now) = self.latest_t() else {
            return Vec::new();
        };
        self.samples
            .iter()
            .filter(|s| now - s.t <= window)
            .map(|s| [s.t - now, pick(s) as f64])
            .collect()
    }

    pub fn spot_points(&self, id: u32, window: f64) -> Vec<[f64; 2]> {
        let (Some(now), Some(series)) = (self.latest_t(), self.spots.get(&id)) else {
            return Vec::new();
        };
        series
            .iter()
            .filter(|(t, _)| now - t <= window)
            .map(|(t, v)| [t - now, *v as f64])
            .collect()
    }
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
        h.push(s(0.0, 1.0), &[(1, 5.0)]);
        h.push(s(5.0, 2.0), &[(2, 6.0)]);
        h.push(s(12.0, 3.0), &[(2, 7.0)]);
        assert_eq!(h.samples().len(), 2);
        assert_eq!(
            h.spot_series().map(|(id, _)| id).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn series_is_relative_to_now() {
        let mut h = History::new(100.0);
        for i in 0..5 {
            h.push(s(i as f64, i as f32), &[(1, 10.0 + i as f32)]);
        }
        assert_eq!(
            h.series(2.0, |s| s.max),
            vec![[-2.0, 2.0], [-1.0, 3.0], [0.0, 4.0]]
        );
        assert_eq!(h.spot_points(1, 1.0), vec![[-1.0, 13.0], [0.0, 14.0]]);
    }
}
