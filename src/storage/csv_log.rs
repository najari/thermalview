//! CSV measurement log: one row per frame with frame statistics, spot and Box/Line values and
//! the differences between measurements.
//!
//! Columns are fixed when recording starts; measurements added later are not logged and removed
//! ones leave empty cells. Excel-friendly: UTF-8 with BOM, comma separated.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::thermal::delta::Delta;
use crate::thermal::shapes::ShapeStats;

pub struct CsvLog {
    path: PathBuf,
    out: BufWriter<File>,
    spot_ids: Vec<u32>,
    shape_ids: Vec<u32>,
    deltas: Vec<Delta>,
    rows: u64,
    last_flush: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    /// When the frame arrived; `None` (a played-back frame) logs the time of writing. Frames
    /// that waited in the queue are written in a burst, so the writing time would give
    /// several rows the same time.
    pub captured: Option<std::time::SystemTime>,
    pub sequence: u32,
    pub max: f32,
    pub min: f32,
    pub mean: f32,
    pub center: f32,
    pub fpa_c: Option<f32>,
    /// (spot id, value) for the spots currently present.
    pub spots: &'a [(u32, f32)],
    /// (shape id, statistics) for the Boxes and Lines currently present.
    pub shapes: &'a [(u32, ShapeStats)],
    /// Differences between two measurements that have a value.
    pub deltas: &'a [(Delta, f32)],
}

impl CsvLog {
    /// Create `dir/thermal_log_<timestamp>.csv` and write the header. `shapes` and `deltas`
    /// come with their labels.
    pub fn start(
        dir: &Path,
        spot_ids: &[u32],
        shapes: &[(u32, String)],
        deltas: &[(Delta, String)],
        unit: &str,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let name = chrono::Local::now()
            .format("thermal_log_%Y%m%d_%H%M%S.csv")
            .to_string();
        let path = dir.join(name);
        let mut out = BufWriter::new(File::create(&path)?);
        out.write_all("\u{FEFF}".as_bytes())?;
        write!(
            out,
            "time,sequence,max_{unit},min_{unit},mean_{unit},center_{unit},fpa_C"
        )?;
        for id in spot_ids {
            write!(out, ",S{id}_{unit}")?;
        }
        for (_, label) in shapes {
            write!(
                out,
                ",{label}_max_{unit},{label}_min_{unit},{label}_mean_{unit}"
            )?;
        }
        for (_, label) in deltas {
            // "화면 최고 − S1" → "화면최고-S1_C": no spaces, plain minus.
            let name: String = label
                .chars()
                .filter(|c| !c.is_whitespace())
                .map(|c| if c == '−' { '-' } else { c })
                .collect();
            write!(out, ",{name}_{unit}")?;
        }
        writeln!(out)?;
        Ok(Self {
            path,
            out,
            spot_ids: spot_ids.to_vec(),
            shape_ids: shapes.iter().map(|(id, _)| *id).collect(),
            deltas: deltas.iter().map(|(d, _)| *d).collect(),
            rows: 0,
            last_flush: Instant::now(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn write(&mut self, r: Row<'_>) -> anyhow::Result<()> {
        let when = r
            .captured
            .map_or_else(chrono::Local::now, chrono::DateTime::<chrono::Local>::from);
        let now = when.format("%Y-%m-%d %H:%M:%S%.3f");
        write!(
            self.out,
            "{now},{},{:.2},{:.2},{:.2},{:.2},",
            r.sequence, r.max, r.min, r.mean, r.center
        )?;
        if let Some(t) = r.fpa_c {
            write!(self.out, "{t:.2}")?;
        }
        for id in &self.spot_ids {
            write!(self.out, ",")?;
            if let Some((_, v)) = r.spots.iter().find(|(sid, _)| sid == id) {
                write!(self.out, "{v:.2}")?;
            }
        }
        for id in &self.shape_ids {
            match r.shapes.iter().find(|(sid, _)| sid == id) {
                Some((_, s)) => write!(self.out, ",{:.2},{:.2},{:.2}", s.max, s.min, s.mean)?,
                None => write!(self.out, ",,,")?,
            }
        }
        for d in &self.deltas {
            write!(self.out, ",")?;
            if let Some((_, v)) = r.deltas.iter().find(|(x, _)| x == d) {
                write!(self.out, "{v:.2}")?;
            }
        }
        writeln!(self.out)?;
        self.rows += 1;
        if self.last_flush.elapsed().as_secs() >= 1 {
            self.out.flush()?;
            self.last_flush = Instant::now();
        }
        Ok(())
    }

    pub fn finish(mut self) -> anyhow::Result<(PathBuf, u64)> {
        self.out.flush()?;
        Ok((self.path, self.rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_rows() {
        let dir = std::env::temp_dir().join(format!("thermalview_csv_{}", std::process::id()));
        use crate::thermal::alarm::{Metric, Source};
        let shapes = [(1, "B1".to_string()), (2, "L2".to_string())];
        let d1 = Delta {
            a: Source::Frame(Metric::Max),
            b: Source::Spot(3),
        };
        let d2 = Delta {
            a: Source::Center,
            b: Source::Spot(1),
        };
        let deltas = [
            (d1, "화면 최고 − S3".to_string()),
            (d2, "중앙 − S1".to_string()),
        ];
        let mut log = CsvLog::start(&dir, &[1, 3], &shapes, &deltas, "C").unwrap();
        let box_stats = ShapeStats {
            min: 21.0,
            max: 35.5,
            mean: 28.25,
            min_pos: (0, 0),
            max_pos: (1, 1),
            pixels: 4,
        };
        log.write(Row {
            captured: None,
            sequence: 7,
            max: 40.0,
            min: 20.0,
            mean: 30.0,
            center: 25.5,
            fpa_c: Some(31.0),
            spots: &[(3, 22.25)],
            shapes: &[(1, box_stats)], // L2 was removed: its three cells stay empty
            deltas: &[(d1, 17.75)],    // S1 is gone, so is d2
        })
        .unwrap();
        let (path, rows) = log.finish().unwrap();
        assert_eq!(rows, 1);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.trim_start_matches('\u{FEFF}').lines().collect();
        assert_eq!(
            lines[0],
            "time,sequence,max_C,min_C,mean_C,center_C,fpa_C,S1_C,S3_C,\
             B1_max_C,B1_min_C,B1_mean_C,L2_max_C,L2_min_C,L2_mean_C,\
             화면최고-S3_C,중앙-S1_C"
        );
        assert!(lines[1]
            .ends_with(",7,40.00,20.00,30.00,25.50,31.00,,22.25,35.50,21.00,28.25,,,,17.75,"));
        assert_eq!(lines[0].split(',').count(), lines[1].split(',').count());
        std::fs::remove_dir_all(&dir).ok();
    }
}
