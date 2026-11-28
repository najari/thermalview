//! Difference between two measurements (S1 − S2, a box maximum minus the frame mean…).

use serde::{Deserialize, Serialize};

use super::alarm::Source;
use super::frame::{FrameStats, ThermalFrame};
use super::measure::Spots;
use super::shapes::Shapes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub a: Source,
    pub b: Source,
}

impl Delta {
    /// "S1 − S2".
    pub fn label(&self, shapes: &Shapes) -> String {
        format!("{} − {}", self.a.label(shapes), self.b.label(shapes))
    }

    /// a − b, or `None` when either measurement has no value.
    pub fn read(
        &self,
        f: &ThermalFrame,
        st: &FrameStats,
        spots: &Spots,
        shapes: &Shapes,
    ) -> Option<f32> {
        Some(self.a.read(f, st, spots, shapes)? - self.b.read(f, st, spots, shapes)?)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deltas {
    pub items: Vec<Delta>,
}

impl Deltas {
    pub const MAX: usize = 4;

    /// False when the list is full or already has this pair.
    pub fn add(&mut self, d: Delta) -> bool {
        if self.items.len() >= Self::MAX || self.items.contains(&d) || d.a == d.b {
            return false;
        }
        self.items.push(d);
        true
    }

    /// Drop the differences whose measurement was deleted (ids are reused, so a stale one
    /// could start reading another spot). Returns how many were removed.
    pub fn prune(&mut self, spots: &Spots, shapes: &Shapes) -> usize {
        let before = self.items.len();
        self.items
            .retain(|d| d.a.exists(spots, shapes) && d.b.exists(spots, shapes));
        before - self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thermal::alarm::Metric;

    #[test]
    fn reads_the_difference_and_follows_deletions() {
        // 0.01 K per count; pixel (0, 0) is 10 K warmer than the rest.
        let mut raw = vec![30000u16; 16 * 16];
        raw[0] = 31000;
        let f = ThermalFrame::new(16, 16, raw, Some(0.01));
        let st = f.stats();
        let mut spots = Spots::default();
        let s1 = spots.add(8, 8).unwrap();
        let shapes = Shapes::default();

        let mut deltas = Deltas::default();
        let d = Delta {
            a: Source::Frame(Metric::Max),
            b: Source::Spot(s1),
        };
        assert!(deltas.add(d));
        assert!(!deltas.add(d), "no duplicates");
        assert!(!deltas.add(Delta { a: d.a, b: d.a }), "a − a says nothing");
        let v = d.read(&f, &st, &spots, &shapes).unwrap();
        assert!((v - 10.0).abs() < 1e-3, "{v}");
        assert_eq!(d.label(&shapes), "화면 최고 − S1");

        spots.remove(s1);
        assert_eq!(d.read(&f, &st, &spots, &shapes), None);
        assert_eq!(deltas.prune(&spots, &shapes), 1);
        assert!(deltas.items.is_empty());
    }

    #[test]
    fn the_list_is_limited() {
        let mut deltas = Deltas::default();
        for id in 1..=10 {
            deltas.add(Delta {
                a: Source::Center,
                b: Source::Spot(id),
            });
        }
        assert_eq!(deltas.items.len(), Deltas::MAX);
    }
}
