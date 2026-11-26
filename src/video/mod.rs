//! MP4 (H.264) recording of the colored view, encoded by the encoder built into Windows.
//!
//! The UI thread sends one [`Scene`] per shown frame over a channel. A writer thread draws it
//! ([`overlay`]), converts it to NV12 and feeds the encoder ([`mf`]). The file is only playable
//! once [`Mp4Writer::finish`] (or drop) has finalized it.

pub(crate) mod mf;
pub mod overlay;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

use overlay::Scene;

pub const EXTENSION: &str = "mp4";
/// Rate written to the file header. Each frame carries its real time.
const NOMINAL_FPS: u32 = 9;
const FRAME_100NS: i64 = 10_000_000 / NOMINAL_FPS as i64;
const BITRATE: u32 = 2_000_000;

pub struct Mp4Writer {
    path: PathBuf,
    tx: Option<Sender<(f64, Scene)>>,
    thread: Option<JoinHandle<anyhow::Result<u64>>>,
    size: (usize, usize),
    start: Instant,
    sent: u64,
}

impl Mp4Writer {
    /// Create a timestamped MP4 in `dir` for thermal images of `width`×`height` pixels.
    pub fn start(dir: &Path, width: usize, height: usize) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let name = chrono::Local::now()
            .format("thermal_video_%Y%m%d_%H%M%S")
            .to_string();
        let path = dir.join(format!("{name}.{EXTENSION}"));
        let (cw, ch) = overlay::canvas_size(width, height);

        let (tx, rx) = mpsc::channel::<(f64, Scene)>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<anyhow::Result<()>>(1);
        let file = path.clone();
        let thread = std::thread::Builder::new()
            .name("mp4-writer".into())
            .spawn(move || -> anyhow::Result<u64> {
                let mut enc =
                    match mf::H264Writer::create(&file, cw as u32, ch as u32, NOMINAL_FPS, BITRATE)
                    {
                        Ok(enc) => {
                            let _ = ready_tx.send(Ok(()));
                            enc
                        }
                        Err(e) => {
                            let msg = format!("{e:#}");
                            let _ = ready_tx.send(Err(e));
                            anyhow::bail!(msg);
                        }
                    };
                let text = overlay::Text::load();
                let mut nv12 = Vec::new();
                let mut first = None;
                let mut last: Option<i64> = None;
                let mut n = 0u64;
                for (t, scene) in rx {
                    if (scene.width, scene.height) != (width, height) {
                        continue;
                    }
                    let t0 = *first.get_or_insert(t);
                    let mut time = ((t - t0) * 1e7).round() as i64;
                    // Times only go forward, also after seeking back in a playback.
                    if let Some(l) = last.filter(|l| time <= *l) {
                        time = l + FRAME_100NS;
                    }
                    last = Some(time);
                    let cv = overlay::compose(&scene, &text);
                    overlay::to_nv12(&cv, &mut nv12);
                    enc.write(&nv12, time, FRAME_100NS)?;
                    n += 1;
                }
                if n == 0 {
                    // Media Foundation cannot finalize an MP4 without frames.
                    drop(enc);
                    let _ = std::fs::remove_file(&file);
                    return Ok(0);
                }
                enc.finish()?;
                Ok(n)
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                path,
                tx: Some(tx),
                thread: Some(thread),
                size: (width, height),
                start: Instant::now(),
                sent: 0,
            }),
            failed => {
                let _ = thread.join();
                let _ = std::fs::remove_file(&path);
                Err(match failed {
                    Ok(Err(e)) => e,
                    _ => anyhow::anyhow!("MP4 writer thread ended unexpectedly"),
                })
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn frames(&self) -> u64 {
        self.sent
    }

    /// Thermal image size this recording takes.
    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    /// Queue a frame shown at `t` seconds (any clock that only moves forward). Returns false
    /// when the writer has stopped after an error; [`Self::finish`] then reports it.
    pub fn push(&mut self, t: f64, scene: Scene) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        if tx.send((t, scene)).is_err() {
            return false;
        }
        self.sent += 1;
        true
    }

    /// Finalize the file. Returns (path, frames written). With no frames there is no file.
    pub fn finish(mut self) -> anyhow::Result<(PathBuf, u64)> {
        self.tx = None; // ends the writer loop
        let n = match self.thread.take() {
            Some(t) => t
                .join()
                .map_err(|_| anyhow::anyhow!("MP4 writer panicked"))??,
            None => 0,
        };
        Ok((self.path.clone(), n))
    }
}

impl Drop for Mp4Writer {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(shade: u8) -> Scene {
        Scene {
            width: 160,
            height: 120,
            rgba: [shade, 255 - shade, 128, 255].repeat(160 * 120),
            lut: (0..=255u8).map(|v| [v, 0, 255 - v]).collect(),
            range: (20.0, 40.0),
            unit: "°C",
            caption: "test".into(),
            marks: vec![overlay::Mark::Spot {
                at: (80, 60),
                text: "S1 30.0°C".into(),
            }],
            webcam: None,
            upscaled: None,
        }
    }

    /// Whether `needle` occurs in `data`.
    fn has(data: &[u8], needle: &[u8]) -> bool {
        data.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn writes_a_finalized_mp4() {
        let dir = std::env::temp_dir().join(format!("thermalview_mp4_{}", std::process::id()));
        let mut w = Mp4Writer::start(&dir, 160, 120).expect("Media Foundation H.264 encoder");
        for i in 0..27u32 {
            assert!(w.push(i as f64 / 9.0, scene((i * 9) as u8)));
        }
        // A frame of another size is skipped, not written.
        let mut odd = scene(0);
        odd.width = 80;
        w.push(4.0, odd);
        let (path, n) = w.finish().unwrap();
        assert_eq!(n, 27);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[4..8], b"ftyp");
        // The index (moov) is only written by Finalize; avc1 = H.264 track.
        assert!(has(&data, b"moov") && has(&data, b"avc1"));

        let empty = Mp4Writer::start(&dir.join("empty"), 160, 120).unwrap();
        let (path, n) = empty.finish().unwrap();
        assert_eq!(n, 0);
        assert!(!path.exists(), "no file without frames");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bad_folder_fails_at_start() {
        let file = std::env::temp_dir().join(format!("thermalview_mp4_f_{}", std::process::id()));
        std::fs::write(&file, b"not a folder").unwrap();
        assert!(Mp4Writer::start(&file.join("sub"), 160, 120).is_err());
        std::fs::remove_file(&file).ok();
    }
}
