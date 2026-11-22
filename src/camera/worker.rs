//! Background camera worker: connects, streams, watches for stalls, re-applies settings.
//!
//! Every USB call (stream control and Lepton commands) happens on this one thread, which keeps
//! them serialized without a shared lock (DEVELOPMENT.md §4.8). The UI talks to it through a
//! command channel and reads a shared status snapshot plus a "latest frame" slot.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use super::lepton::{self, ShutterMode};
use super::uvc::{self, DeviceInfo, Handle, UvcError};
use super::{HEIGHT, PID, VID, WIDTH};
use crate::thermal::frame::{decode_le_u16, ThermalFrame};

/// No frame for this long → the stream is considered stalled.
const STALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Limit for the first frame after a stream start (the Lepton may still be booting).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
/// Lepton status poll + shutter-mode re-apply (2 CCI commands; see the session loop).
const STATUS_PERIOD: Duration = Duration::from_secs(5);
const FPS_PERIOD: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnState {
    #[default]
    Starting,
    /// Device not found (unplugged, or driver not WinUSB).
    Searching,
    /// Found but could not open: another program has it.
    Busy,
    Streaming,
    /// Streaming, but frames stopped; recovery in progress.
    Stalled,
    /// Recovery by restarting/reopening failed repeatedly: ask the user to replug.
    NeedsReplug,
}

impl ConnState {
    pub fn label(&self) -> &'static str {
        match self {
            ConnState::Starting => "시작 중",
            ConnState::Searching => "카메라 찾는 중",
            ConnState::Busy => "다른 프로그램이 사용 중",
            ConnState::Streaming => "수신 중",
            ConnState::Stalled => "영상 멈춤 - 복구 중",
            ConnState::NeedsReplug => "USB를 다시 꽂아 주세요",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CameraStatus {
    pub state: ConnState,
    pub device: Option<DeviceInfo>,
    pub part_number: Option<String>,
    pub lepton_sw: Option<String>,
    pub kelvin_per_count: Option<f32>,
    pub fpa_c: Option<f32>,
    pub shutter_mode: Option<ShutterMode>,
    pub since_ffc_s: Option<f32>,
    pub fps: f32,
    pub frames: u64,
    pub stalls: u32,
    /// Times the shutter mode had to be re-applied (camera reset itself).
    pub reapplied: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Run a flat-field correction. The UI must have confirmed the lens is covered.
    RunFfc,
    SetShutterMode(ShutterMode),
}

struct Shared {
    latest: Mutex<Option<ThermalFrame>>,
    status: Mutex<CameraStatus>,
    frames: AtomicU64,
    /// Milliseconds since `epoch` of the last good frame.
    last_frame_ms: AtomicU64,
    epoch: Instant,
    stop: AtomicBool,
    repaint: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
    fn status(&self, f: impl FnOnce(&mut CameraStatus)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
        (self.repaint)();
    }
}

/// Handle to the camera worker thread. Dropping it stops the worker and closes the device.
pub struct Camera {
    shared: Arc<Shared>,
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Camera {
    /// Start the worker. `desired_mode` is applied on every connect and kept enforced;
    /// `repaint` is called whenever a new frame or status is available.
    pub fn start(desired_mode: ShutterMode, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            status: Mutex::new(CameraStatus::default()),
            frames: AtomicU64::new(0),
            last_frame_ms: AtomicU64::new(0),
            epoch: Instant::now(),
            stop: AtomicBool::new(false),
            repaint: Box::new(repaint),
        });
        let (tx, rx) = mpsc::channel();
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("camera".into())
            .spawn(move || worker(s, rx, desired_mode))
            .ok();
        Self { shared, tx, thread }
    }

    /// Take the newest frame if one arrived since the last call.
    pub fn take_frame(&self) -> Option<ThermalFrame> {
        self.shared.latest.lock().ok().and_then(|mut l| l.take())
    }

    pub fn status(&self) -> CameraStatus {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Exact count of good frames received so far (no USB traffic).
    pub fn total_frames(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn worker(shared: Arc<Shared>, rx: Receiver<Command>, mut desired: ShutterMode) {
    let ctx = match uvc::Context::new() {
        Ok(c) => c,
        Err(e) => {
            shared.status(|s| s.last_error = Some(format!("libusb 초기화 실패: {e}")));
            return;
        }
    };
    let mut failed_sessions = 0u32;

    while !shared.stop.load(Ordering::SeqCst) {
        match open(&ctx, &shared) {
            Ok((handle, info)) => {
                let healthy = session(&shared, &rx, &mut desired, handle, info);
                failed_sessions = if healthy { 0 } else { failed_sessions + 1 };
                if failed_sessions >= 3 {
                    shared.status(|s| s.state = ConnState::NeedsReplug);
                }
            }
            Err(OpenError::NotFound) => {
                shared.status(|s| {
                    if s.state != ConnState::NeedsReplug {
                        s.state = ConnState::Searching;
                    }
                    s.fps = 0.0;
                });
            }
            Err(OpenError::Busy(e)) => shared.status(|s| {
                // After stuck transfers this process itself still holds the device.
                if s.state != ConnState::NeedsReplug {
                    s.state = ConnState::Busy;
                }
                s.last_error = Some(e.to_string());
            }),
            Err(OpenError::Other(e)) => shared.status(|s| s.last_error = Some(e)),
        }
        // Drain commands that arrived while disconnected, but remember a mode change.
        while let Ok(cmd) = rx.try_recv() {
            if let Command::SetShutterMode(m) = cmd {
                desired = m;
            }
        }
        sleep_unless_stopped(&shared, RETRY_DELAY);
    }
}

enum OpenError {
    NotFound,
    Busy(UvcError),
    Other(String),
}

fn open(ctx: &uvc::Context, shared: &Shared) -> Result<(Handle, DeviceInfo), OpenError> {
    let dev = ctx.find_device(VID, PID).map_err(|e| {
        if e.is_not_found() {
            OpenError::NotFound
        } else {
            OpenError::Other(e.to_string())
        }
    })?;
    // USB strings must be read before opening (WinUSB allows one open).
    let info = dev.info().unwrap_or_default();
    let handle = dev.open().map_err(|e| {
        if e.is_access() {
            OpenError::Busy(e)
        } else if e.is_not_found() {
            OpenError::NotFound
        } else {
            OpenError::Other(format!("장치 열기 실패: {e}"))
        }
    })?;
    shared.status(|s| s.device = Some(info.clone()));
    Ok((handle, info))
}

fn start_stream(h: &mut Handle, shared: &Arc<Shared>, scale: Option<f32>) -> Result<(), UvcError> {
    let s = shared.clone();
    shared
        .last_frame_ms
        .store(shared.now_ms(), Ordering::SeqCst);
    h.start_stream(uvc::FORMAT_Y16, WIDTH as u32, HEIGHT as u32, move |f| {
        if f.data.len() != WIDTH * HEIGHT * 2 || f.format != uvc::FORMAT_Y16 {
            return; // partial or foreign frame
        }
        let frame = ThermalFrame {
            width: WIDTH,
            height: HEIGHT,
            raw: decode_le_u16(f.data),
            kelvin_per_count: scale,
            sequence: f.sequence,
            captured: SystemTime::now(),
        };
        if let Ok(mut l) = s.latest.lock() {
            *l = Some(frame);
        }
        s.frames.fetch_add(1, Ordering::Relaxed);
        s.last_frame_ms.store(s.now_ms(), Ordering::Relaxed);
        (s.repaint)();
    })
}

/// Apply `desired` if the camera reports something else. Returns true if it wrote.
fn enforce_mode(h: &Handle, desired: ShutterMode) -> lepton::Result<bool> {
    let obj = lepton::get_shutter_mode_obj(h)?;
    if obj.mode() != desired as i32 {
        lepton::set_shutter_mode(h, desired)?;
        return Ok(true);
    }
    Ok(false)
}

/// Run one connected session. Returns true if it streamed frames before ending.
fn session(
    shared: &Arc<Shared>,
    rx: &Receiver<Command>,
    desired: &mut ShutterMode,
    mut h: Handle,
    info: DeviceInfo,
) -> bool {
    log::info!(
        "connected: {} {} ({})",
        info.manufacturer,
        info.product,
        info.serial
    );
    let scale = lepton::tlinear_scale(&h).ok().flatten();
    let part = lepton::part_number(&h).ok();
    let sw = lepton::software_version(&h).ok();
    if let Err(e) = enforce_mode(&h, *desired) {
        log::warn!("could not apply shutter mode: {e}");
    }
    shared.status(|s| {
        s.kelvin_per_count = scale;
        s.part_number = part;
        s.lepton_sw = sw;
        s.last_error = None;
    });

    let start_frames = shared.frames.load(Ordering::Relaxed);
    if let Err(e) = start_stream(&mut h, shared, scale) {
        shared.status(|s| s.last_error = Some(format!("스트리밍 시작 실패: {e}")));
        return false;
    }
    shared.status(|s| {
        // "Streaming" is reported once a frame has really arrived.
        if s.state != ConnState::NeedsReplug {
            s.state = ConnState::Starting;
        }
    });

    let mut streaming = false;
    let mut last_status = Instant::now() - STATUS_PERIOD;
    let mut fps_mark = (Instant::now(), shared.frames.load(Ordering::Relaxed));

    while !shared.stop.load(Ordering::SeqCst) {
        // Commands from the UI.
        while let Ok(cmd) = rx.try_recv() {
            let result = match cmd {
                Command::RunFfc => lepton::run_ffc(&h).map(|_| log::info!("FFC run")),
                Command::SetShutterMode(m) => {
                    *desired = m;
                    lepton::set_shutter_mode(&h, m).map(|_| log::info!("shutter mode -> {m:?}"))
                }
            };
            if let Err(e) = result {
                shared.status(|s| s.last_error = Some(format!("명령 실패 ({cmd:?}): {e}")));
            }
        }

        // Stall detection. Recovery closes and reopens the device, the teardown path that
        // examples/churn exercises. A Lepton still booting gets a longer first-frame limit.
        let idle = shared.now_ms() - shared.last_frame_ms.load(Ordering::Relaxed);
        let got_frame = shared.frames.load(Ordering::Relaxed) > start_frames;
        if got_frame && !streaming {
            streaming = true;
            shared.status(|s| {
                s.state = ConnState::Streaming;
                s.last_error = None;
            });
        }
        let limit = if got_frame {
            STALL_TIMEOUT
        } else {
            FIRST_FRAME_TIMEOUT
        };
        if idle > limit.as_millis() as u64 {
            log::warn!("no frame for {idle} ms; reopening device");
            shared.status(|s| {
                if s.state != ConnState::NeedsReplug {
                    s.state = ConnState::Stalled;
                }
                s.stalls += 1;
                s.fps = 0.0;
            });
            break;
        }

        // Frame rate from the frame counter (no USB traffic), refreshed every second.
        if fps_mark.0.elapsed() >= FPS_PERIOD {
            let now_frames = shared.frames.load(Ordering::Relaxed);
            let fps = (now_frames - fps_mark.1) as f32 / fps_mark.0.elapsed().as_secs_f32();
            fps_mark = (Instant::now(), now_frames);
            shared.status(|s| {
                s.fps = fps;
                s.frames = now_frames;
            });
        }

        // Periodic Lepton poll. Every CCI command makes the PureThermal drop video frames
        // (measured: 4 commands/s cost ~0.6 fps), so this is kept to 2 commands every 5 s.
        // The same read re-applies the shutter mode if the Lepton reset itself to AUTO.
        // A failing control transfer means the device is gone or wedged.
        if last_status.elapsed() >= STATUS_PERIOD {
            last_status = Instant::now();
            match poll_status(&h) {
                Ok(p) => {
                    let reverted = p.mode != *desired as i32;
                    shared.status(|s| {
                        s.fpa_c = Some(p.fpa_c);
                        s.shutter_mode = ShutterMode::from_i32(p.mode);
                        s.since_ffc_s = Some(p.since_ffc_ms as f32 / 1000.0);
                    });
                    if reverted {
                        match lepton::set_shutter_mode(&h, *desired) {
                            Ok(()) => {
                                log::warn!("shutter mode had reverted; re-applied {desired:?}");
                                shared.status(|s| s.reapplied += 1);
                            }
                            Err(e) => log::warn!("re-applying shutter mode failed: {e}"),
                        }
                    }
                }
                Err(e) => {
                    log::warn!("status poll failed, treating device as disconnected: {e}");
                    shared.status(|s| s.last_error = Some(format!("장치 응답 없음: {e}")));
                    break;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    let streamed = shared.frames.load(Ordering::Relaxed) > start_frames;
    h.stop_stream();
    let stuck = h.has_stuck_transfers();
    drop(h); // closes the device (libuvc keeps the handle open if transfers are stuck)
    shared.status(|s| {
        s.fps = 0.0;
        if stuck {
            s.state = ConnState::NeedsReplug;
            s.last_error =
                Some("USB 전송이 끝나지 않았습니다. 프로그램을 닫고 USB를 다시 꽂아 주세요".into());
        }
    });
    streamed
}

struct Poll {
    fpa_c: f32,
    mode: i32,
    since_ffc_ms: u32,
}

/// Two CCI commands: FPA temperature and the FFC/shutter-mode object.
fn poll_status(h: &Handle) -> lepton::Result<Poll> {
    let fpa_c = lepton::fpa_temp_c(h)?;
    let obj = lepton::get_shutter_mode_obj(h)?;
    Ok(Poll {
        fpa_c,
        mode: obj.mode(),
        since_ffc_ms: obj.elapsed_since_ffc_ms(),
    })
}

fn sleep_unless_stopped(shared: &Shared, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end && !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
}
