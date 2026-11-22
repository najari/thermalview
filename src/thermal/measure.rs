//! Measurement tools. MVP: spot meters (3×3 mean).

use serde::{Deserialize, Serialize};

use super::frame::ThermalFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spot {
    pub id: u32,
    pub x: usize,
    pub y: usize,
}

impl Spot {
    /// Half-size of the averaging window (1 → 3×3).
    pub const RADIUS: usize = 1;

    pub fn value(&self, f: &ThermalFrame) -> Option<f32> {
        (self.x < f.width && self.y < f.height)
            .then(|| f.value_around(self.x, self.y, Self::RADIUS))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spots {
    pub items: Vec<Spot>,
    next_id: u32,
}

impl Spots {
    pub const MAX: usize = 16;

    pub fn add(&mut self, x: usize, y: usize) -> Option<u32> {
        if self.items.len() >= Self::MAX {
            return None;
        }
        self.next_id += 1;
        self.items.push(Spot {
            id: self.next_id,
            x,
            y,
        });
        Some(self.next_id)
    }

    pub fn remove(&mut self, id: u32) {
        self.items.retain(|s| s.id != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 0;
    }

    /// Spot within `max_dist` pixels of (x, y), nearest first.
    pub fn hit(&self, x: f32, y: f32, max_dist: f32) -> Option<u32> {
        self.items
            .iter()
            .map(|s| {
                let dx = s.x as f32 + 0.5 - x;
                let dy = s.y as f32 + 0.5 - y;
                (s.id, dx * dx + dy * dy)
            })
            .filter(|(_, d2)| *d2 <= max_dist * max_dist)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_hit() {
        let mut s = Spots::default();
        let a = s.add(10, 10).unwrap();
        let b = s.add(50, 50).unwrap();
        assert_eq!(s.hit(11.0, 10.0, 3.0), Some(a));
        assert_eq!(s.hit(30.0, 30.0, 3.0), None);
        s.remove(a);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].id, b);
    }

    #[test]
    fn spot_value_is_window_mean() {
        let f = ThermalFrame::new(3, 3, (0..9).map(|v| 27315 + v * 100).collect(), Some(0.01));
        let s = Spot { id: 1, x: 1, y: 1 };
        assert!((s.value(&f).unwrap() - 4.0).abs() < 1e-3);
        assert_eq!(Spot { id: 2, x: 5, y: 0 }.value(&f), None);
    }
}
