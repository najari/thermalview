//! MP4 (H.264) recording of the colored view, encoded by the encoder built into Windows.
//!
//! The UI thread sends one [`Scene`] per shown frame over a channel. A writer thread draws it
//! ([`overlay`]), converts it to NV12 and feeds the encoder ([`mf`]). The file is only playable
//! once [`Mp4Writer::finish`] (or drop) has finalized it.

pub(crate) mod mf;
pub mod overlay;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use overlay::Scene;

pub const EXTENSION: &str = "mp4";
/// Rate written to the file header. Each frame carries its real time.
const NOMINAL_FPS: u32 = 9;
const FRAME_100NS: i64 = 10_000_000 / NOMINAL_FPS as i64;
const BITRATE: u32 = 2_000_000;

/// Frames that may wait for the encoder. A scene with the 4× image is about 3 MB, so this
/// bounds the memory at roughly 100 MB when the encoder cannot keep up; further frames are
/// skipped (and counted) instead of piling up.
const BACKLOG: usize = 32;

/// Longest wait for the writer to encode its backlog and finalize the file. Normally well under
/// a second (32 frames at about 9 ms each, then the index). Once, a Media Foundation
/// `WriteSample` call never returned (2026-11-28, not reproduced); without a limit, stopping
/// the recording or closing the app would then hang forever.
const FINISH_TIMEOUT: Duration = Duration::from_secs(15);

/// Maps frame times (seconds on any clock) to MP4 sample times (100 ns units). The file starts
/// at 0 and its times only go forward. When the clock jumps back (seeking back in a playback),
/// the next frame comes one nominal frame after the last and the timeline follows the clock
/// from there, so later frames keep their real spacing.
#[derive(Debug, Default)]
struct Timeline {
    /// Clock time that maps to sample time 0.
    origin: Option<f64>,
    last: Option<i64>,
}

impl Timeline {
    fn next(&mut self, t: f64) -> i64 {
        let origin = *self.origin.get_or_insert(t);
        let mut time = ((t - origin) * 1e7).round() as i64;
        if let Some(last) = self.last.filter(|last| time <= *last) {
            time = last + FRAME_100NS;
            self.origin = Some(t - time as f64 / 1e7);
        }
        self.last = Some(time);
        time
    }
}

pub struct Mp4Writer {
    path: PathBuf,
    tx: Option<SyncSender<(f64, Scene)>>,
    thread: Option<JoinHandle<()>>,
    /// The writer's result, sent as its last act.
    done: Receiver<anyhow::Result<u64>>,
    size: (usize, usize),
    start: Instant,
    sent: u64,
    skipped: u64,
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

        let (tx, rx) = mpsc::sync_channel::<(f64, Scene)>(BACKLOG);
        let (ready_tx, ready_rx) = mpsc::sync_channel::<anyhow::Result<()>>(1);
        let (done_tx, done) = mpsc::sync_channel::<anyhow::Result<u64>>(1);
        let file = path.clone();
        let thread = std::thread::Builder::new()
            .name("mp4-writer".into())
            .spawn(move || {
                let _ = done_tx.send(write(file, rx, ready_tx, width, height, cw, ch));
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                path,
                tx: Some(tx),
                thread: Some(thread),
                done,
                size: (width, height),
                start: Instant::now(),
                sent: 0,
                skipped: 0,
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

    /// Frames left out because the encoder was behind (see [`BACKLOG`]).
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Queue a frame shown at `t` seconds. Returns false when the writer has stopped after an
    /// error; [`Self::finish`] then reports it. A frame the encoder has no room for is skipped
    /// and counted; that is not an error.
    pub fn push(&mut self, t: f64, scene: Scene) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        match tx.try_send((t, scene)) {
            Ok(()) => self.sent += 1,
            Err(TrySendError::Full(_)) => self.skipped += 1,
            Err(TrySendError::Disconnected(_)) => return false,
        }
        true
    }

    /// Finalize the file. Returns (path, frames written). With no frames there is no file.
    pub fn finish(mut self) -> anyhow::Result<(PathBuf, u64)> {
        let n = self.end(FINISH_TIMEOUT)?;
        Ok((self.path.clone(), n))
    }

    /// End the writer loop and wait up to `limit` for its result. A writer stuck past that is
    /// left behind (detached): the file is then not playable, but the caller goes on.
    fn end(&mut self, limit: Duration) -> anyhow::Result<u64> {
        self.tx = None;
        let Some(thread) = self.thread.take() else {
            return Ok(0);
        };
        match self.done.recv_timeout(limit) {
            Ok(result) => {
                let _ = thread.join();
                result
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                anyhow::bail!("MP4 writer panicked")
            }
            Err(RecvTimeoutError::Timeout) => {
                log::error!("MP4 writer did not finish within {limit:?}; left running");
                anyhow::bail!(
                    "the encoder did not finish within {} s; the file is incomplete",
                    limit.as_secs()
                )
            }
        }
    }
}

impl Drop for Mp4Writer {
    fn drop(&mut self) {
        let _ = self.end(FINISH_TIMEOUT);
    }
}

/// The writer thread: create the encoder (reporting success or failure on `ready`), encode every
/// scene until the channel closes, then finalize. Returns the number of frames written.
fn write(
    file: PathBuf,
    rx: Receiver<(f64, Scene)>,
    ready: SyncSender<anyhow::Result<()>>,
    width: usize,
    height: usize,
    cw: usize,
    ch: usize,
) -> anyhow::Result<u64> {
    let mut enc = match mf::H264Writer::create(&file, cw as u32, ch as u32, NOMINAL_FPS, BITRATE) {
        Ok(enc) => {
            let _ = ready.send(Ok(()));
            enc
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = ready.send(Err(e));
            anyhow::bail!(msg);
        }
    };
    let text = overlay::Text::load();
    let mut nv12 = Vec::new();
    let mut timeline = Timeline::default();
    let mut n = 0u64;
    for (t, scene) in rx {
        if (scene.width, scene.height) != (width, height) {
            continue;
        }
        let time = timeline.next(t);
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
            signed: false,
            band: None,
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
    fn timeline_keeps_real_spacing_after_a_jump_back() {
        let mut tl = Timeline::default();
        let ms = |v: i64| v / 10_000;
        assert_eq!(tl.next(100.0), 0);
        assert_eq!(ms(tl.next(100.04)), 40);
        assert_eq!(ms(tl.next(100.08)), 80);
        // Seeking back 50 s: one nominal frame later, then real spacing again (not +1/9 s
        // for every frame until the clock catches up).
        let back = tl.next(50.0);
        assert_eq!(back, 800_000 + FRAME_100NS);
        assert_eq!(ms(tl.next(50.04) - back), 40);
        assert_eq!(ms(tl.next(50.08) - back), 80);
        // The same time again still moves forward.
        let again = tl.next(50.08);
        assert!(again > back + 800_000);
    }

    #[test]
    fn a_stuck_writer_does_not_block_finishing() {
        // A writer that never reports back, like a Media Foundation call that never returns.
        let (_tx, rx) = mpsc::sync_channel::<(f64, Scene)>(1);
        let (release, stuck) = mpsc::channel::<()>();
        let (_done_tx, done) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let _rx = rx;
            let _ = stuck.recv();
        });
        let mut w = Mp4Writer {
            path: PathBuf::from("stuck.mp4"),
            tx: None,
            thread: Some(thread),
            done,
            size: (160, 120),
            start: Instant::now(),
            sent: 0,
            skipped: 0,
        };
        let t = Instant::now();
        let err = w.end(Duration::from_millis(100)).unwrap_err().to_string();
        assert!(err.contains("did not finish"), "{err}");
        assert!(t.elapsed() < Duration::from_secs(5));
        // Dropping it afterwards does not wait again.
        drop(w);
        release.send(()).unwrap();
    }

    #[test]
    fn bad_folder_fails_at_start() {
        let file = std::env::temp_dir().join(format!("thermalview_mp4_f_{}", std::process::id()));
        std::fs::write(&file, b"not a folder").unwrap();
        assert!(Mp4Writer::start(&file.join("sub"), 160, 120).is_err());
        std::fs::remove_file(&file).ok();
    }
}
