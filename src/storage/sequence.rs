//! Radiometric sequence files (`.tvseq`): every frame's raw counts, for later playback/analysis.
//!
//! Layout (little-endian):
//! - 8-byte magic: `TVSEQ\0\x02\0` (compressed) or `TVSEQ\0\x01\0` (older, uncompressed)
//! - u32 header length + UTF-8 JSON [`SeqHeader`]
//! - records: u64 milliseconds since recording start, u32 sensor sequence, then
//!   - v2: u32 payload length + lossless compressed frame (see [`encode_frame`])
//!   - v1: width×height u16 raw counts
//!
//! The reader scans the record headers once on open, so any frame is one seek away and a record
//! cut short by a crash is simply ignored. Writing happens on a background thread.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use crate::thermal::frame::ThermalFrame;

const MAGIC_V1: &[u8; 8] = b"TVSEQ\0\x01\0";
const MAGIC_V2: &[u8; 8] = b"TVSEQ\0\x02\0";
pub const EXTENSION: &str = "tvseq";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SeqHeader {
    pub width: usize,
    pub height: usize,
    pub kelvin_per_count: Option<f32>,
    pub started_local: String,
    pub device: Option<String>,
}

fn zigzag(v: i16) -> u16 {
    ((v << 1) ^ (v >> 15)) as u16
}

fn unzigzag(v: u16) -> i16 {
    ((v >> 1) as i16) ^ -((v & 1) as i16)
}

/// Lossless frame compression. Each pixel is predicted from its left neighbor (first column: the
/// pixel above), residuals are zigzag-coded, low and high bytes are stored as separate planes
/// (the high plane is almost all zeros), and the result is deflated.
pub fn encode_frame(raw: &[u16], width: usize) -> std::io::Result<Vec<u8>> {
    let mut planes = vec![0u8; raw.len() * 2];
    let (low, high) = planes.split_at_mut(raw.len());
    for (i, &v) in raw.iter().enumerate() {
        let pred = if i == 0 {
            0
        } else if i % width == 0 {
            raw[i - width]
        } else {
            raw[i - 1]
        };
        let z = zigzag(v.wrapping_sub(pred) as i16);
        low[i] = z as u8;
        high[i] = (z >> 8) as u8;
    }
    let mut enc = DeflateEncoder::new(Vec::with_capacity(raw.len()), Compression::fast());
    enc.write_all(&planes)?;
    enc.finish()
}

/// Inverse of [`encode_frame`] for a frame of `pixels` pixels.
pub fn decode_frame(data: &[u8], pixels: usize, width: usize) -> std::io::Result<Vec<u16>> {
    let mut planes = vec![0u8; pixels * 2];
    DeflateDecoder::new(data).read_exact(&mut planes)?;
    let (low, high) = planes.split_at(pixels);
    let mut raw = vec![0u16; pixels];
    for i in 0..pixels {
        let z = u16::from_le_bytes([low[i], high[i]]);
        let pred = if i == 0 {
            0
        } else if i % width == 0 {
            raw[i - width]
        } else {
            raw[i - 1]
        };
        raw[i] = pred.wrapping_add(unzigzag(z) as u16);
    }
    Ok(raw)
}

/// Background writer. Frames are sent over a channel; the file is finalized by [`Self::finish`].
pub struct SeqWriter {
    path: PathBuf,
    tx: Option<Sender<(u64, ThermalFrame)>>,
    thread: Option<JoinHandle<anyhow::Result<u64>>>,
    start: Instant,
    sent: u64,
    bytes: Arc<AtomicU64>,
}

impl SeqWriter {
    pub fn start(dir: &Path, header: SeqHeader) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let name = chrono::Local::now()
            .format("thermal_seq_%Y%m%d_%H%M%S")
            .to_string();
        let path = dir.join(format!("{name}.{EXTENSION}"));
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&path)?);
        out.write_all(MAGIC_V2)?;
        let json = serde_json::to_vec(&header)?;
        out.write_all(&(json.len() as u32).to_le_bytes())?;
        out.write_all(&json)?;
        let bytes = Arc::new(AtomicU64::new((12 + json.len()) as u64));

        let (tx, rx) = mpsc::channel::<(u64, ThermalFrame)>();
        let (w, h) = (header.width, header.height);
        let written = bytes.clone();
        let thread = std::thread::Builder::new()
            .name("seq-writer".into())
            .spawn(move || -> anyhow::Result<u64> {
                let mut n = 0u64;
                for (t_ms, f) in rx {
                    if f.width != w || f.height != h {
                        continue;
                    }
                    let payload = encode_frame(&f.raw, w)?;
                    out.write_all(&t_ms.to_le_bytes())?;
                    out.write_all(&f.sequence.to_le_bytes())?;
                    out.write_all(&(payload.len() as u32).to_le_bytes())?;
                    out.write_all(&payload)?;
                    written.fetch_add(16 + payload.len() as u64, Ordering::Relaxed);
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
            bytes,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn frames(&self) -> u64 {
        self.sent
    }

    /// Bytes written so far (compressed size).
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
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

#[derive(Debug, Clone, Copy)]
struct Entry {
    /// File offset of the frame data.
    offset: u64,
    /// Bytes of frame data.
    len: u32,
    t_ms: u64,
    sequence: u32,
}

/// Random-access reader for both format versions.
pub struct SeqReader {
    path: PathBuf,
    file: BufReader<File>,
    pub header: SeqHeader,
    compressed: bool,
    index: Vec<Entry>,
}

impl SeqReader {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic)?;
        let compressed = match &magic {
            m if m == MAGIC_V2 => true,
            m if m == MAGIC_V1 => false,
            _ => anyhow::bail!("not a ThermalView sequence file"),
        };
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

        // Scan the record headers. A record that does not fit in the file (crash) ends the scan.
        let total = file.get_ref().metadata()?.len();
        let raw_len = (header.width * header.height * 2) as u64;
        let head_len: u64 = if compressed { 16 } else { 12 };
        let mut pos = 12 + len as u64;
        let mut index = Vec::new();
        let mut head = [0u8; 16];
        while pos + head_len <= total {
            file.seek(SeekFrom::Start(pos))?;
            file.read_exact(&mut head[..head_len as usize])?;
            let data_len = if compressed {
                u32::from_le_bytes(head[12..16].try_into()?) as u64
            } else {
                raw_len
            };
            // Deflate can only expand incompressible data slightly.
            if data_len > raw_len + raw_len / 8 + 1024 || pos + head_len + data_len > total {
                break;
            }
            index.push(Entry {
                offset: pos + head_len,
                len: data_len as u32,
                t_ms: u64::from_le_bytes(head[0..8].try_into()?),
                sequence: u32::from_le_bytes(head[8..12].try_into()?),
            });
            pos += head_len + data_len;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            header,
            compressed,
            index,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Read frame `i`: (milliseconds since start, frame).
    pub fn frame(&mut self, i: usize) -> anyhow::Result<(u64, ThermalFrame)> {
        let e = *self
            .index
            .get(i)
            .ok_or_else(|| anyhow::anyhow!("frame {i} out of range"))?;
        self.file.seek(SeekFrom::Start(e.offset))?;
        let mut data = vec![0u8; e.len as usize];
        self.file.read_exact(&mut data)?;
        let (w, h) = (self.header.width, self.header.height);
        let raw = if self.compressed {
            decode_frame(&data, w * h, w)?
        } else {
            crate::thermal::frame::decode_le_u16(&data)
        };
        let mut f = ThermalFrame::new(w, h, raw, self.header.kelvin_per_count);
        f.sequence = e.sequence;
        f.captured = SystemTime::UNIX_EPOCH;
        Ok((e.t_ms, f))
    }

    /// Index of the last frame at or before `t_ms`.
    pub fn index_at(&self, t_ms: u64) -> usize {
        self.index
            .partition_point(|e| e.t_ms <= t_ms)
            .saturating_sub(1)
    }

    /// Timestamp of frame `i` in milliseconds (0 if out of range).
    pub fn timestamp(&self, i: usize) -> u64 {
        self.index.get(i).map_or(0, |e| e.t_ms)
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

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("thermalview_seq_{tag}_{}", std::process::id()))
    }

    fn header() -> SeqHeader {
        SeqHeader {
            width: 3,
            height: 2,
            kelvin_per_count: Some(0.01),
            started_local: "now".into(),
            device: None,
        }
    }

    #[test]
    fn codec_is_lossless_on_extremes() {
        let raw: Vec<u16> = vec![
            0, 65535, 1, 32768, 32767, 65535, 0, 0, 12345, 54321, 2, 65534,
        ];
        let enc = encode_frame(&raw, 4).unwrap();
        assert_eq!(decode_frame(&enc, raw.len(), 4).unwrap(), raw);
        for v in [i16::MIN, -1, 0, 1, i16::MAX] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn smooth_frames_compress_well() {
        let raw: Vec<u16> = (0..120)
            .flat_map(|y| (0..160).map(move |x| 30000 + (x + y) as u16 / 4))
            .collect();
        let enc = encode_frame(&raw, 160).unwrap();
        assert!(enc.len() < raw.len() / 4, "{} bytes", enc.len());
        assert_eq!(decode_frame(&enc, raw.len(), 160).unwrap(), raw);
    }

    #[test]
    fn roundtrip_and_truncated_tail() {
        let dir = temp_dir("rt");
        let h = header();
        let mut w = SeqWriter::start(&dir, h.clone()).unwrap();
        for k in 0..5u16 {
            let raw = (0..6).map(|i| 30000 + k * 10 + i).collect();
            let mut f = ThermalFrame::new(3, 2, raw, Some(0.01));
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
        assert_eq!(r.header, h);
        assert_eq!(r.len(), 5);
        let (_, f) = r.frame(3).unwrap();
        assert_eq!(f.sequence, 103);
        assert_eq!(f.raw, vec![30030, 30031, 30032, 30033, 30034, 30035]);
        assert!(r.frame(5).is_err());
        assert_eq!(r.index_at(u64::MAX), 4);
        assert_eq!(list(&dir).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_uncompressed_v1_files() {
        let dir = temp_dir("v1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.tvseq");
        let json = serde_json::to_vec(&header()).unwrap();
        let mut b = MAGIC_V1.to_vec();
        b.extend((json.len() as u32).to_le_bytes());
        b.extend(&json);
        for k in 0..3u64 {
            b.extend((k * 100).to_le_bytes());
            b.extend((7 + k as u32).to_le_bytes());
            for i in 0..6u16 {
                b.extend((29000 + k as u16 + i).to_le_bytes());
            }
        }
        b.extend([9, 9]); // truncated next record
        std::fs::write(&path, b).unwrap();

        let mut r = SeqReader::open(&path).unwrap();
        assert_eq!(r.len(), 3);
        let (t, f) = r.frame(2).unwrap();
        assert_eq!((t, f.sequence), (200, 9));
        assert_eq!(f.raw[5], 29007);
        assert_eq!(r.index_at(150), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
