//! Radiometric sequence files (`.tvseq`): every frame's raw counts, for later playback/analysis.
//!
//! Layout (little-endian):
//! - 8-byte magic `TVSEQ\0\x01\0`
//! - u32 header length + UTF-8 JSON [`SeqHeader`]
//! - fixed-size records: u64 milliseconds since recording start, u32 sensor sequence,
//!   then width×height u16 raw counts.
//!
//! Records are uncompressed (≈ 38 KB per frame, ≈ 1.2 GB per hour at 8.7 fps) so any frame can be
//! read with one seek. Writing happens on a background thread.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::thermal::frame::ThermalFrame;

const MAGIC: &[u8; 8] = b"TVSEQ\0\x01\0";
pub const EXTENSION: &str = "tvseq";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SeqHeader {
    pub width: usize,
    pub height: usize,
    pub kelvin_per_count: Option<f32>,
    pub started_local: String,
    pub device: Option<String>,
}

fn record_size(h: &SeqHeader) -> u64 {
    12 + (h.width * h.height * 2) as u64
}

/// Background writer. Frames are sent over a channel; the file is finalized by [`Self::finish`].
pub struct SeqWriter {
    path: PathBuf,
    tx: Option<Sender<(u64, ThermalFrame)>>,
    thread: Option<JoinHandle<anyhow::Result<u64>>>,
    start: Instant,
    sent: u64,
    header: SeqHeader,
}

impl SeqWriter {
    pub fn start(dir: &Path, header: SeqHeader) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let name = chrono::Local::now()
            .format("thermal_seq_%Y%m%d_%H%M%S")
            .to_string();
        let path = dir.join(format!("{name}.{EXTENSION}"));
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&path)?);
        out.write_all(MAGIC)?;
        let json = serde_json::to_vec(&header)?;
        out.write_all(&(json.len() as u32).to_le_bytes())?;
        out.write_all(&json)?;

        let (tx, rx) = mpsc::channel::<(u64, ThermalFrame)>();
        let (w, h) = (header.width, header.height);
        let thread = std::thread::Builder::new()
            .name("seq-writer".into())
            .spawn(move || -> anyhow::Result<u64> {
                let mut n = 0u64;
                let mut buf = Vec::with_capacity(12 + w * h * 2);
                for (t_ms, f) in rx {
                    if f.width != w || f.height != h {
                        continue;
                    }
                    buf.clear();
                    buf.extend_from_slice(&t_ms.to_le_bytes());
                    buf.extend_from_slice(&f.sequence.to_le_bytes());
                    for v in &f.raw {
                        buf.extend_from_slice(&v.to_le_bytes());
                    }
                    out.write_all(&buf)?;
                    n += 1;
                }
                out.flush()?;
                Ok(n)
            })?;
        Ok(Self {
            path,
            tx: Some(tx),
            thread: Some(thread),
            start: Instant::now(),
            sent: 0,
            header,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn frames(&self) -> u64 {
        self.sent
    }

    pub fn bytes(&self) -> u64 {
        self.sent * record_size(&self.header)
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn push(&mut self, f: &ThermalFrame) {
        if let Some(tx) = &self.tx {
            let t_ms = self.start.elapsed().as_millis() as u64;
            if tx.send((t_ms, f.clone())).is_ok() {
                self.sent += 1;
            }
        }
    }

    /// Close the file. Returns (path, frames written).
    pub fn finish(mut self) -> anyhow::Result<(PathBuf, u64)> {
        self.tx = None; // ends the writer loop
        let n = match self.thread.take() {
            Some(t) => t
                .join()
                .map_err(|_| anyhow::anyhow!("sequence writer panicked"))??,
            None => 0,
        };
        Ok((self.path.clone(), n))
    }
}

impl Drop for SeqWriter {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Random-access reader.
pub struct SeqReader {
    path: PathBuf,
    file: BufReader<File>,
    pub header: SeqHeader,
    data_offset: u64,
    frames: usize,
}

impl SeqReader {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        anyhow::ensure!(&magic == MAGIC, "not a ThermalView sequence file");
        let mut len = [0u8; 4];
        file.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        anyhow::ensure!(len < 1 << 20, "corrupt header");
        let mut json = vec![0u8; len];
        file.read_exact(&mut json)?;
        let header: SeqHeader = serde_json::from_slice(&json)?;
        anyhow::ensure!(
            header.width > 0 && header.height > 0 && header.width * header.height <= 1 << 22,
            "bad frame size"
        );
        let data_offset = 12 + len as u64;
        let total = file.get_ref().metadata()?.len();
        // A partially written last record (e.g. after a crash) is ignored.
        let frames = (total.saturating_sub(data_offset) / record_size(&header)) as usize;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            header,
            data_offset,
            frames,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.frames
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    /// Read frame `i`: (milliseconds since start, frame).
    pub fn frame(&mut self, i: usize) -> anyhow::Result<(u64, ThermalFrame)> {
        anyhow::ensure!(i < self.frames, "frame {i} out of range");
        let rs = record_size(&self.header);
        self.file
            .seek(SeekFrom::Start(self.data_offset + i as u64 * rs))?;
        let mut rec = vec![0u8; rs as usize];
        self.file.read_exact(&mut rec)?;
        let t_ms = u64::from_le_bytes(rec[0..8].try_into()?);
        let seq = u32::from_le_bytes(rec[8..12].try_into()?);
        let raw = crate::thermal::frame::decode_le_u16(&rec[12..]);
        let mut f = ThermalFrame::new(
            self.header.width,
            self.header.height,
            raw,
            self.header.kelvin_per_count,
        );
        f.sequence = seq;
        f.captured = SystemTime::UNIX_EPOCH;
        Ok((t_ms, f))
    }

    /// Index of the last frame at or before `t_ms` (binary search over timestamps).
    pub fn index_at(&mut self, t_ms: u64) -> anyhow::Result<usize> {
        let (mut lo, mut hi) = (0usize, self.frames);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.timestamp(mid)? <= t_ms {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    pub fn timestamp(&mut self, i: usize) -> anyhow::Result<u64> {
        let rs = record_size(&self.header);
        self.file
            .seek(SeekFrom::Start(self.data_offset + i as u64 * rs))?;
        let mut t = [0u8; 8];
        self.file.read_exact(&mut t)?;
        Ok(u64::from_le_bytes(t))
    }
}

/// Sequence files in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut v: Vec<(PathBuf, u64, SystemTime)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == EXTENSION))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.len(), m.modified().ok()?))
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.2));
    v.into_iter().map(|(p, n, _)| (p, n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_truncated_tail() {
        let dir = std::env::temp_dir().join(format!("thermalview_seq_{}", std::process::id()));
        let header = SeqHeader {
            width: 3,
            height: 2,
            kelvin_per_count: Some(0.01),
            started_local: "now".into(),
            device: None,
        };
        let mut w = SeqWriter::start(&dir, header.clone()).unwrap();
        for k in 0..5u16 {
            let mut f = ThermalFrame::new(
                3,
                2,
                (0..6).map(|i| 30000 + k * 10 + i).collect(),
                Some(0.01),
            );
            f.sequence = 100 + k as u32;
            w.push(&f);
        }
        let (path, n) = w.finish().unwrap();
        assert_eq!(n, 5);

        // Simulate a crash mid-record: a few stray bytes at the end must be ignored.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();

        let mut r = SeqReader::open(&path).unwrap();
        assert_eq!(r.header, header);
        assert_eq!(r.len(), 5);
        let (_, f) = r.frame(3).unwrap();
        assert_eq!(f.sequence, 103);
        assert_eq!(f.raw, vec![30030, 30031, 30032, 30033, 30034, 30035]);
        assert!(r.frame(5).is_err());
        assert_eq!(r.index_at(u64::MAX).unwrap(), 4);
        assert_eq!(list(&dir).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
