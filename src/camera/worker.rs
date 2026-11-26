//! Background camera worker: picks the camera, connects, streams, watches for stalls,
//! re-applies settings.
//!
//! Every USB call (stream control and camera commands) happens on this one thread, which keeps
//! them serialized without a shared lock (DEVELOPMENT.md §4.8). The UI talks to it through a
//! command channel and reads a shared status snapshot plus a short queue of received frames.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use super::lepton::{self, ShutterMode};
use super::p3;
use super::uvc::{self, DeviceInfo, Handle, UvcError};
use super::{Config, Kind, Source, HEIGHT, PID, VID, WIDTH};
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
    /// The camera of the current (or last) connection.
    pub kind: Option<Kind>,
    pub device: Option<DeviceInfo>,
    pub part_number: Option<String>,
    pub lepton_sw: Option<String>,
    /// Camera firmware where it is not in the USB strings (P3).
    pub firmware: Option<String>,
    pub kelvin_per_count: Option<f32>,
    /// Scene emissivity configured inside the Lepton (read once per connection).
    pub camera_emissivity: Option<f32>,
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

impl CameraStatus {
    /// Forget what the previous camera reported; a new connection of `kind` begins.
    fn begin(&mut self, kind: Option<Kind>) {
        self.kind = kind;
        self.part_number = None;
        self.lepton_sw = None;
        self.firmware = None;
        self.kelvin_per_count = None;
        self.camera_emissivity = None;
        self.fpa_c = None;
        self.shutter_mode = None;
        self.since_ffc_s = None;
        self.last_error = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Run a flat-field correction. For the shutterless Lepton the UI must have confirmed the
    /// lens is covered; the P3 closes its own shutter.
    RunFfc,
    SetShutterMode(ShutterMode),
    /// P3 measurement range.
    SetGain(p3::Gain),
}

/// Frames kept for the UI thread (about 1.8 s of Lepton video). eframe runs a hidden window only
/// about ten times a second, close to the frame rate, so a single slot would lose frames while
/// minimized.
const QUEUE_MAX: usize = 16;

struct Shared {
    /// Received frames not yet taken, oldest first.
    queue: Mutex<VecDeque<ThermalFrame>>,
    status: Mutex<CameraStatus>,
    frames: AtomicU64,
    /// Milliseconds since `epoch` of the last good frame.
    last_frame_ms: AtomicU64,
    epoch: Instant,
    stop: AtomicBool,
    /// The wanted [`Source`]; a session ends when it no longer allows its camera.
    source: AtomicU8,
    repaint: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn new(source: Source, repaint: Box<dyn Fn() + Send + Sync>) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            status: Mutex::new(CameraStatus::default()),
            frames: AtomicU64::new(0),
            last_frame_ms: AtomicU64::new(0),
            epoch: Instant::now(),
            stop: AtomicBool::new(false),
            source: AtomicU8::new(source.to_u8()),
            repaint,
        }
    }
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
    fn status(&self, f: impl FnOnce(&mut CameraStatus)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
        (self.repaint)();
    }
    fn source(&self) -> Source {
        Source::from_u8(self.source.load(Ordering::Relaxed))
    }
    /// Keep going with a camera of `kind`: not stopping, and still the wanted source.
    fn wants(&self, kind: Kind) -> bool {
        !self.stop.load(Ordering::SeqCst) && self.source().allows(kind)
    }
    /// Queue a received frame for the UI.
    fn push_frame(&self, frame: ThermalFrame) {
        if let Ok(mut q) = self.queue.lock() {
            if q.len() == QUEUE_MAX {
                q.pop_front();
            }
            q.push_back(frame);
        }
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.last_frame_ms.store(self.now_ms(), Ordering::Relaxed);
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
    /// Start the worker. `config` is applied on every connect and kept enforced; `repaint` is
    /// called whenever a new frame or status is available.
    pub fn start(config: Config, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared::new(config.source, Box::new(repaint)));
        let (tx, rx) = mpsc::channel();
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("camera".into())
            .spawn(move || worker(s, rx, config))
            .ok();
        Self { shared, tx, thread }
    }

    /// A camera with no worker thread and no USB access, for UI tests.
    #[cfg(test)]
    pub(crate) fn disconnected() -> Self {
        let shared = Arc::new(Shared::new(Source::Auto, Box::new(|| {})));
        let (tx, _rx) = mpsc::channel();
        Self {
            shared,
            tx,
            thread: None,
        }
    }

    /// Queue a frame as if the camera had sent it (tests of the disconnected camera).
    #[cfg(test)]
    pub(crate) fn inject(&self, f: ThermalFrame) {
        if let Ok(mut q) = self.shared.queue.lock() {
            q.push_back(f);
        }
    }

    /// Take the oldest frame not yet taken. Call until None to see every frame.
    pub fn take_frame(&self) -> Option<ThermalFrame> {
        self.shared
            .queue
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
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

    /// Look for another kind of camera. A running connection to a camera the new source does
    /// not allow is closed.
    pub fn set_source(&self, source: Source) {
        self.shared.source.store(source.to_u8(), Ordering::Relaxed);
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

/// Outcome of trying one kind of camera.
enum Attempt {
    /// Not connected: try the next kind.
    NotFound,
    /// Found; the session (or the failure to open) is over. `healthy`: it streamed frames.
    Done { healthy: bool },
}

fn worker(shared: Arc<Shared>, rx: Receiver<Command>, mut config: Config) {
    let ctx = match uvc::Context::new() {
        Ok(c) => c,
        Err(e) => {
            shared.status(|s| s.last_error = Some(format!("libusb 초기화 실패: {e}")));
            return;
        }
    };
    // Created on first use: only needed when the P3 may be wanted.
    let mut usb: Option<rusb::Context> = None;
    let mut failed_sessions = 0u32;

    while !shared.stop.load(Ordering::SeqCst) {
        let source = shared.source();
        let mut attempt = Attempt::NotFound;
        if source.allows(Kind::PureThermal) {
            attempt = try_purethermal(&ctx, &shared, &rx, &mut config);
        }
        if matches!(attempt, Attempt::NotFound) && source.allows(Kind::P3) {
            if usb.is_none() {
                match rusb::Context::new() {
                    Ok(c) => usb = Some(c),
                    Err(e) => {
                        shared.status(|s| s.last_error = Some(format!("libusb 초기화 실패: {e}")))
                    }
                }
            }
            if let Some(usb) = &usb {
                attempt = try_p3(usb, &shared, &rx, &mut config);
            }
        }
        match attempt {
            Attempt::NotFound => shared.status(|s| {
                if s.state != ConnState::NeedsReplug {
                    s.state = ConnState::Searching;
                }
                s.fps = 0.0;
            }),
            Attempt::Done { healthy } => {
                failed_sessions = if healthy { 0 } else { failed_sessions + 1 };
                if failed_sessions >= 3 {
                    shared.status(|s| s.state = ConnState::NeedsReplug);
                }
            }
        }
        // Drain commands that arrived while disconnected, but remember setting changes.
        while let Ok(cmd) = rx.try_recv() {
            remember(&mut config, cmd);
        }
        sleep_unless_stopped(&shared, RETRY_DELAY);
    }
}

/// Keep a setting command for the next connection.
fn remember(config: &mut Config, cmd: Command) {
    match cmd {
        Command::SetShutterMode(m) => config.shutter_mode = m,
        Command::SetGain(g) => config.gain = g,
        Command::RunFfc => {}
    }
}

fn busy(shared: &Shared, e: String) {
    shared.status(|s| {
        // After stuck transfers this process itself still holds the device.
        if s.state != ConnState::NeedsReplug {
            s.state = ConnState::Busy;
        }
        s.last_error = Some(e);
    });
}

fn try_purethermal(
    ctx: &uvc::Context,
    shared: &Arc<Shared>,
    rx: &Receiver<Command>,
    config: &mut Config,
) -> Attempt {
    match open(ctx, shared) {
        Ok((handle, info)) => Attempt::Done {
            healthy: session(shared, rx, config, handle, info),
        },
        Err(OpenError::NotFound) => Attempt::NotFound,
        Err(OpenError::Busy(e)) => {
            busy(shared, e.to_string());
            Attempt::Done { healthy: true }
        }
        Err(OpenError::Other(e)) => {
            shared.status(|s| s.last_error = Some(e));
            Attempt::Done { healthy: true }
        }
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
        s.push_frame(ThermalFrame {
            width: WIDTH,
            height: HEIGHT,
            raw: decode_le_u16(f.data),
            kelvin_per_count: scale,
            sequence: f.sequence,
            captured: SystemTime::now(),
            calibration: None,
        });
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

/// Frame-rate reporting and stall detection shared by both cameras.
struct Watch {
    start_frames: u64,
    streaming: bool,
    fps_mark: (Instant, u64),
}

impl Watch {
    fn new(shared: &Shared) -> Self {
        shared
            .last_frame_ms
            .store(shared.now_ms(), Ordering::SeqCst);
        let frames = shared.frames.load(Ordering::Relaxed);
        Self {
            start_frames: frames,
            streaming: false,
            fps_mark: (Instant::now(), frames),
        }
    }

    /// Update the state and frame rate. Returns false when the stream has stalled.
    fn check(&mut self, shared: &Shared) -> bool {
        let idle = shared.now_ms() - shared.last_frame_ms.load(Ordering::Relaxed);
        let got_frame = self.streamed(shared);
        if got_frame && !self.streaming {
            self.streaming = true;
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
            return false;
        }
        // Frame rate from the frame counter (no USB traffic), refreshed every second.
        if self.fps_mark.0.elapsed() >= FPS_PERIOD {
            let now_frames = shared.frames.load(Ordering::Relaxed);
            let fps =
                (now_frames - self.fps_mark.1) as f32 / self.fps_mark.0.elapsed().as_secs_f32();
            self.fps_mark = (Instant::now(), now_frames);
            shared.status(|s| {
                s.fps = fps;
                s.frames = now_frames;
            });
        }
        true
    }

    fn streamed(&self, shared: &Shared) -> bool {
        shared.frames.load(Ordering::Relaxed) > self.start_frames
    }
}

/// Run one connected PureThermal session. Returns true if it streamed frames before ending.
fn session(
    shared: &Arc<Shared>,
    rx: &Receiver<Command>,
    config: &mut Config,
    mut h: Handle,
    info: DeviceInfo,
) -> bool {
    log::info!(
        "connected: {} {} ({})",
        info.manufacturer,
        info.product,
        info.serial
    );
    shared.status(|s| s.begin(Some(Kind::PureThermal)));
    let scale = lepton::tlinear_scale(&h).ok().flatten();
    let part = lepton::part_number(&h).ok();
    let sw = lepton::software_version(&h).ok();
    // Connect-time only, outside the polling budget (§4.10).
    let cam_eps = match lepton::scene_emissivity(&h) {
        Ok(e) => {
            log::info!("Lepton scene emissivity {e:.3}");
            Some(e)
        }
        Err(e) => {
            log::warn!("could not read RAD flux parameters: {e}");
            None
        }
    };
    if let Err(e) = enforce_mode(&h, config.shutter_mode) {
        log::warn!("could not apply shutter mode: {e}");
    }
    shared.status(|s| {
        s.kelvin_per_count = scale;
        s.camera_emissivity = cam_eps;
        s.part_number = part;
        s.lepton_sw = sw;
    });

    let mut watch = Watch::new(shared);
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

    let mut last_status = Instant::now() - STATUS_PERIOD;
    while shared.wants(Kind::PureThermal) {
        // Commands from the UI.
        while let Ok(cmd) = rx.try_recv() {
            let result = match cmd {
                Command::RunFfc => lepton::run_ffc(&h).map(|_| log::info!("FFC run")),
                Command::SetShutterMode(m) => {
                    config.shutter_mode = m;
                    lepton::set_shutter_mode(&h, m).map(|_| log::info!("shutter mode -> {m:?}"))
                }
                Command::SetGain(_) => {
                    remember(config, cmd);
                    Ok(())
                }
            };
            if let Err(e) = result {
                shared.status(|s| s.last_error = Some(format!("명령 실패 ({cmd:?}): {e}")));
            }
        }

        // Stall detection. Recovery closes and reopens the device, the teardown path that
        // examples/churn exercises. A Lepton still booting gets a longer first-frame limit.
        if !watch.check(shared) {
            break;
        }

        // Periodic Lepton poll. Every CCI command makes the PureThermal drop video frames
        // (measured: 4 commands/s cost ~0.6 fps), so this is kept to 2 commands every 5 s.
        // The same read re-applies the shutter mode if the Lepton reset itself to AUTO.
        // A failing control transfer means the device is gone or wedged.
        if last_status.elapsed() >= STATUS_PERIOD {
            last_status = Instant::now();
            match poll_status(&h) {
                Ok(p) => {
                    let desired = config.shutter_mode;
                    let reverted = p.mode != desired as i32;
                    shared.status(|s| {
                        s.fpa_c = Some(p.fpa_c);
                        s.shutter_mode = ShutterMode::from_i32(p.mode);
                        s.since_ffc_s = Some(p.since_ffc_ms as f32 / 1000.0);
                    });
                    if reverted {
                        match lepton::set_shutter_mode(&h, desired) {
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

    let streamed = watch.streamed(shared);
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

fn try_p3(
    usb: &rusb::Context,
    shared: &Arc<Shared>,
    rx: &Receiver<Command>,
    config: &mut Config,
) -> Attempt {
    match p3::open(usb) {
        Ok(dev) => Attempt::Done {
            healthy: p3_session(shared, rx, config, dev),
        },
        Err(p3::OpenError::NotFound) => Attempt::NotFound,
        Err(p3::OpenError::Busy(e)) => {
            busy(shared, format!("P3: {e}"));
            Attempt::Done { healthy: true }
        }
        Err(p3::OpenError::Other(e)) => {
            shared.status(|s| s.last_error = Some(e));
            Attempt::Done { healthy: true }
        }
    }
}

/// Run one connected P3 session. Returns true if it streamed frames (or was asked to stop
/// before it could).
fn p3_session(
    shared: &Arc<Shared>,
    rx: &Receiver<Command>,
    config: &mut Config,
    mut dev: p3::Device,
) -> bool {
    let info = match dev.info() {
        Ok(i) => i,
        Err(e) => {
            shared.status(|s| s.last_error = Some(format!("P3 정보 읽기 실패: {e}")));
            return false;
        }
    };
    log::info!(
        "connected: P3 {} fw {} sn {}",
        info.model,
        info.firmware,
        info.serial
    );
    shared.status(|s| {
        s.begin(Some(Kind::P3));
        s.device = Some(DeviceInfo {
            vid: p3::VID,
            pid: p3::PID,
            manufacturer: "Thermal Master".into(),
            product: if info.model.is_empty() {
                "P3".into()
            } else {
                info.model.clone()
            },
            serial: info.serial.clone(),
        });
        s.firmware = Some(info.firmware.clone());
        s.kelvin_per_count = Some(p3::KELVIN_PER_COUNT);
        if s.state != ConnState::NeedsReplug {
            s.state = ConnState::Starting;
        }
    });

    let mut wait = |d: Duration| {
        sleep_unless_stopped(shared, d);
        shared.wants(Kind::P3)
    };
    match dev.start(&mut wait) {
        Ok(true) => {}
        Ok(false) => return true, // asked to stop while starting
        Err(e) => {
            shared.status(|s| s.last_error = Some(format!("P3 스트리밍 시작 실패: {e}")));
            return false;
        }
    }
    if let Err(e) = dev.set_gain(config.gain) {
        log::warn!("P3: setting the gain failed: {e}");
    }

    let mut watch = Watch::new(shared);
    let mut asm = p3::Assembler::default();
    let mut buf = vec![0u8; p3::CHUNK];
    let mut seq = 0u32;
    while shared.wants(Kind::P3) {
        while let Ok(cmd) = rx.try_recv() {
            let result = match cmd {
                Command::RunFfc => dev.shutter().map(|_| log::info!("P3 shutter")),
                Command::SetGain(g) => {
                    config.gain = g;
                    dev.set_gain(g).map(|_| log::info!("P3 gain -> {g:?}"))
                }
                Command::SetShutterMode(_) => {
                    remember(config, cmd);
                    Ok(())
                }
            };
            if let Err(e) = result {
                shared.status(|s| s.last_error = Some(format!("명령 실패 ({cmd:?}): {e}")));
            }
        }

        match dev.read(&mut buf) {
            Ok(n) => {
                if let Some(raw) = asm.push(&buf[..n]) {
                    seq = seq.wrapping_add(1);
                    shared.push_frame(ThermalFrame {
                        width: p3::WIDTH,
                        height: p3::HEIGHT,
                        raw,
                        kelvin_per_count: Some(p3::KELVIN_PER_COUNT),
                        sequence: seq,
                        captured: SystemTime::now(),
                        calibration: None,
                    });
                }
            }
            Err(e) => {
                log::warn!("P3 read failed, treating device as disconnected: {e}");
                shared.status(|s| s.last_error = Some(format!("장치 응답 없음: {e}")));
                break;
            }
        }

        if !watch.check(shared) {
            break;
        }
    }
    log::info!("P3 session ended: {seq} frames, {} dropped", asm.dropped);
    let streamed = watch.streamed(shared) || !shared.wants(Kind::P3);
    drop(dev);
    shared.status(|s| s.fps = 0.0);
    streamed
}

fn sleep_unless_stopped(shared: &Shared, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end && !shared.stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
}
