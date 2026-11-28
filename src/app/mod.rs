//! eframe application: image view, control panel, status bar.

mod alarms;
mod calibration;
mod graphs;
mod playback;
mod remote;
mod video;
mod view;
mod webcam;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;

use crate::camera::lepton::ShutterMode;
use crate::camera::{Camera, CameraStatus, Command, ConnState, Kind, Source};
use crate::settings::{Layout, Settings};
use crate::storage::csv_log::{self, CsvLog};
use crate::storage::sequence::{self, SeqHeader, SeqWriter};
use crate::storage::snapshot::{
    self, DeltaReading, ShapeReading, SnapshotMeta, SpotReading, WebcamInfo,
};
use crate::thermal::alarm::{Metric, Source as Reading};
use crate::thermal::delta::{Delta, Deltas};
use crate::thermal::filter::Temporal;
use crate::thermal::frame::{FrameStats, ThermalFrame};
use crate::thermal::history::{History, Sample};
use crate::thermal::isotherm::Band;
use crate::thermal::palette::{self, Palette};
use crate::thermal::range::{self, AutoRange, RangeMode};
use crate::thermal::rotate::Rotation;
use crate::thermal::shapes::{Grab, Shape, ShapeKind};
use crate::thermal::upscale::{self, Sharpen, Upscaler};
use crate::video::Mp4Writer;

/// AI upscales (external Upscayl processes) that may run at once.
const MAX_AI_JOBS: usize = 2;

/// Why the image does not follow the camera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The user froze the live image.
    Frozen,
    /// A saved snapshot is open; its base file name.
    Snapshot(String),
}

/// A series of snapshots taken at a fixed interval.
struct Timelapse {
    next: Instant,
    shots: u32,
    /// The frame of the last picture: the same one is not saved twice.
    last: Option<(u32, SystemTime)>,
}

/// Which list the open dialog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenTab {
    Recordings,
    Snapshots,
}

type FileList = Vec<(PathBuf, u64, SystemTime)>;

/// What a left click / drag on the image does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Spot,
    Box,
    Line,
}

impl Tool {
    fn shape_kind(self) -> Option<ShapeKind> {
        match self {
            Tool::Spot => None,
            Tool::Box => Some(ShapeKind::Box),
            Tool::Line => Some(ShapeKind::Line),
        }
    }
}

/// A pointer drag in progress on the image.
#[derive(Debug, Clone, Copy)]
enum DragOp {
    /// Drawing a new Box/Line.
    Draw {
        kind: ShapeKind,
        from: (usize, usize),
        to: (usize, usize),
    },
    MoveSpot {
        id: u32,
    },
    /// Moving or resizing an existing shape, relative to where the drag began.
    EditShape {
        original: Shape,
        grab: Grab,
        origin: (usize, usize),
    },
}

pub struct ThermalApp {
    camera: Camera,
    settings: Settings,
    settings_dirty: Option<Instant>,

    frame: Option<ThermalFrame>,
    stats: Option<FrameStats>,
    rgba: Vec<u8>,
    texture: Option<egui::TextureHandle>,
    /// `rgba` changed since the texture was last uploaded.
    texture_dirty: bool,
    /// The 4× upscaled image of the current frame (empty when upscaling is off).
    rgba_up: Vec<u8>,
    upscaler: Option<Upscaler>,
    /// Installed Upscayl, for AI copies of snapshots.
    upscayl: Option<crate::upscayl::Upscayl>,
    /// Running AI upscales: each reports the written file or an error.
    ai_jobs: Vec<mpsc::Receiver<Result<PathBuf, String>>>,
    /// The color table in use, with the palette and direction it was built for.
    lut: ((Palette, bool), [[u8; 3]; 256]),
    auto_range: AutoRange,
    /// Range used for the current image, in display units.
    display_range: (f32, f32),
    /// Share of the pixels inside the isotherm band (when it is on).
    iso_share: Option<f32>,
    /// Working memory of the robust automatic range.
    scratch: Vec<u16>,
    filter: Temporal,
    /// Time of the frame last given to the noise filter.
    filter_t: Option<f64>,
    /// When set, the image is held and camera frames are dropped.
    hold: Option<Hold>,
    zoom: view::Zoom,

    confirm_ffc: bool,
    /// "Delete all measurements" is waiting for its confirmation.
    confirm_clear: bool,
    /// The save folder as typed in the settings panel, until applied.
    dir_edit: Option<String>,
    message: Option<(String, Instant)>,
    /// Counts the passes of [`Self::tick`], and the pass `message` is from.
    pass: u64,
    message_pass: u64,

    started: Instant,
    history: History,
    graphs: graphs::GraphState,
    csv: Option<CsvLog>,
    /// Frame time of the last CSV row, for logging at an interval.
    csv_last: Option<f64>,
    timelapse: Option<Timelapse>,
    last_fpa: Option<f32>,
    /// The two measurements picked for a new difference.
    delta_pick: (Reading, Reading),

    tool: Tool,
    drag: Option<DragOp>,

    /// Radiometric sequence recording (live only).
    seq: Option<SeqWriter>,
    /// When set, the image comes from a recorded sequence instead of the camera.
    player: Option<playback::Player>,
    show_open_dialog: bool,
    open_tab: OpenTab,
    /// Recordings listed in the open dialog: (path, bytes, last change), newest first.
    open_list: Option<FileList>,
    /// Snapshots listed in the open dialog, as their 16-bit images.
    open_snaps: Option<FileList>,
    /// The camera error already shown in the status line.
    seen_error: Option<String>,
    /// MP4 recording of the view (live or playback).
    video: Option<Mp4Writer>,

    alarms: alarms::AlarmState,
    cal: calibration::CalState,
    remote: remote::RemoteState,
    webcam: webcam::WebcamState,
}

impl ThermalApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_korean_font(&cc.egui_ctx);
        let (settings, notice) = Settings::load();
        let ctx = cc.egui_ctx.clone();
        let config = crate::camera::Config {
            source: settings.camera,
            shutter_mode: settings.shutter_mode,
        };
        let camera = Camera::start(config, move || ctx.request_repaint());
        let mut app = Self::with_camera(settings, camera);
        if let Some(n) = notice {
            log::warn!("{n}");
            app.notify(n);
        }
        app
    }

    fn with_camera(settings: Settings, camera: Camera) -> Self {
        let key = (settings.palette, settings.palette_invert);
        let lut = (key, key.0.lut_of(key.1));
        Self {
            camera,
            settings,
            settings_dirty: None,
            frame: None,
            stats: None,
            rgba: Vec::new(),
            texture: None,
            texture_dirty: false,
            rgba_up: Vec::new(),
            upscaler: None,
            upscayl: crate::upscayl::find(),
            ai_jobs: Vec::new(),
            lut,
            auto_range: AutoRange::default(),
            display_range: (20.0, 40.0),
            iso_share: None,
            scratch: Vec::new(),
            filter: Temporal::default(),
            filter_t: None,
            hold: None,
            zoom: view::Zoom::default(),
            confirm_ffc: false,
            confirm_clear: false,
            dir_edit: None,
            message: None,
            pass: 0,
            message_pass: 0,
            started: Instant::now(),
            history: History::default(),
            graphs: graphs::GraphState::default(),
            csv: None,
            csv_last: None,
            timelapse: None,
            last_fpa: None,
            delta_pick: (Reading::Frame(Metric::Max), Reading::Frame(Metric::Min)),
            tool: Tool::Spot,
            drag: None,
            seq: None,
            player: None,
            show_open_dialog: false,
            open_tab: OpenTab::Recordings,
            open_list: None,
            open_snaps: None,
            seen_error: None,
            video: None,
            alarms: alarms::AlarmState::default(),
            cal: calibration::CalState::default(),
            remote: remote::RemoteState::default(),
            webcam: webcam::WebcamState::default(),
        }
    }

    fn toggle_seq(&mut self, status: &CameraStatus) {
        let _ = if self.seq.is_some() {
            self.stop_seq().map(drop)
        } else {
            self.start_seq(status).map(drop)
        };
    }

    /// Start recording the live video. The user is notified of the outcome; the error text is
    /// for remote clients.
    fn start_seq(&mut self, status: &CameraStatus) -> Result<PathBuf, String> {
        if self.seq.is_some() {
            return Err("already recording".into());
        }
        if self.player.is_some() {
            return Err("a recording is being played back; recording needs live video".into());
        }
        self.check_not_held()?;
        let Some(f) = &self.frame else {
            return Err("no frame yet".into());
        };
        let header = SeqHeader {
            width: f.width,
            height: f.height,
            kelvin_per_count: f.kelvin_per_count,
            started_local: chrono::Local::now().to_rfc3339(),
            device: status.device.as_ref().map(|d| d.product.clone()),
        };
        match SeqWriter::start(&self.settings.snapshot_dir(), header) {
            Ok(w) => {
                let path = w.path().to_path_buf();
                self.notify(format!("녹화 시작: {}", path.display()));
                self.seq = Some(w);
                Ok(path)
            }
            Err(e) => {
                self.notify(format!("녹화 파일을 만들 수 없습니다: {e}"));
                Err(format!("cannot create the recording: {e}"))
            }
        }
    }

    /// Finish the recording; returns its path and frame count.
    fn stop_seq(&mut self) -> Result<(PathBuf, u64), String> {
        let w = self.seq.take().ok_or("not recording")?;
        match w.finish() {
            Ok((path, n)) => {
                self.notify(format!("녹화 종료: {n}프레임 → {}", path.display()));
                Ok((path, n))
            }
            Err(e) => {
                self.notify(format!("녹화 파일 저장 실패: {e}"));
                Err(format!("saving the recording failed: {e}"))
            }
        }
    }

    fn open_sequence(&mut self, path: &std::path::Path) {
        match playback::Player::open(path) {
            Ok(p) => {
                // Recording needs live video, and one MP4 holds one source. Both tell the
                // user how they ended.
                if self.seq.is_some() {
                    let _ = self.stop_seq();
                }
                if self.video.is_some() {
                    let _ = self.stop_video();
                }
                self.hold = None;
                self.history.clear();
                self.auto_range.reset();
                self.notify(format!("재생: {}", p.file_name()));
                self.player = Some(p);
            }
            Err(e) => self.notify(format!("열 수 없습니다: {e}")),
        }
    }

    /// Show a saved snapshot as a held image, to measure it again. The measurements, the
    /// temperature correction and the palette are the current ones.
    fn open_snapshot(&mut self, path: &std::path::Path) {
        let loaded = match snapshot::load(path) {
            Ok(l) => l,
            Err(e) => {
                self.notify(format!("열 수 없습니다: {e:#}"));
                return;
            }
        };
        // Recordings hold live video (or one played file): end them, with their messages.
        if self.csv.is_some() {
            let _ = self.stop_csv();
        }
        if self.seq.is_some() {
            let _ = self.stop_seq();
        }
        if self.video.is_some() {
            let _ = self.stop_video();
        }
        self.player = None;
        self.history.clear();
        self.auto_range.reset();
        self.filter.reset();
        self.zoom = view::Zoom::default();
        let mut f = loaded.frame;
        calibration::attach(self, &mut f);
        self.stats = Some(f.stats());
        self.frame = Some(f);
        self.notify(format!("스냅샷 열기: {}", loaded.name));
        webcam::release(self);
        self.hold = Some(Hold::Snapshot(loaded.name));
        self.render();
    }

    /// CSV, recording and MP4 need frames that keep coming.
    fn check_not_held(&self) -> Result<(), String> {
        match self.hold {
            Some(_) => Err("the image is frozen; send FREEZE OFF first".into()),
            None => Ok(()),
        }
    }

    /// Hold the current image, or go back to the camera. While held, camera frames are
    /// dropped. The error text is for remote clients.
    pub(super) fn set_hold(&mut self, on: bool) -> Result<(), String> {
        if !on {
            match self.hold.take() {
                // A file is not live video: show nothing until the camera sends a frame.
                Some(Hold::Snapshot(_)) => {
                    self.frame = None;
                    self.stats = None;
                    self.history.clear();
                    self.auto_range.reset();
                    self.started = Instant::now();
                    self.notify("실시간 영상으로 돌아갑니다");
                }
                Some(Hold::Frozen) => self.notify("화면 정지를 풀었습니다"),
                None => {}
            }
            webcam::release(self);
            self.filter.reset();
            return Ok(());
        }
        if self.hold.is_some() {
            return Ok(());
        }
        if self.player.is_some() {
            return Err("a recording is being played back; pause it instead".into());
        }
        if self.csv.is_some() || self.seq.is_some() || self.video.is_some() {
            return Err("stop CSV, REC and MP4 first: frames are dropped while frozen".into());
        }
        if self.frame.is_none() {
            return Err("no frame yet".into());
        }
        webcam::hold(self);
        self.hold = Some(Hold::Frozen);
        self.drag = None;
        self.notify("화면 정지: 카메라 영상은 정지를 풀 때까지 버립니다");
        Ok(())
    }

    /// Whether the freeze button does anything now.
    fn can_hold(&self) -> bool {
        self.hold.is_some()
            || (self.frame.is_some()
                && self.player.is_none()
                && self.csv.is_none()
                && self.seq.is_none()
                && self.video.is_none())
    }

    fn close_player(&mut self) {
        if self.video.is_some() {
            let _ = self.stop_video();
        }
        self.player = None;
        self.hold = None;
        self.filter.reset();
        // The last played frame is not live video: show nothing until the camera sends one.
        self.frame = None;
        self.stats = None;
        self.history.clear();
        self.auto_range.reset();
        self.started = Instant::now();
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_open_dialog {
            self.open_list = None;
            self.open_snaps = None;
            return;
        }
        let dir = self.settings.snapshot_dir();
        let mut tab = self.open_tab;
        // Read the folder when the dialog opens, not on every pass.
        let files = match tab {
            OpenTab::Recordings => self
                .open_list
                .get_or_insert_with(|| sequence::list(&dir))
                .clone(),
            OpenTab::Snapshots => self
                .open_snaps
                .get_or_insert_with(|| snapshot::list(&dir))
                .clone(),
        };
        let mut chosen = None;
        let mut refresh = false;
        let mut open = true;
        egui::Window::new("열기")
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut tab, OpenTab::Recordings, "녹화 (.tvseq)");
                    ui.selectable_value(&mut tab, OpenTab::Snapshots, "스냅샷")
                        .on_hover_text("저장한 스냅샷을 다시 열어 측정합니다");
                    ui.separator();
                    refresh = ui
                        .small_button("⟳")
                        .on_hover_text("목록 새로 고침")
                        .clicked();
                });
                ui.weak(format!("폴더: {}", dir.display()));
                ui.weak("다른 폴더의 파일은 이 창에 끌어다 놓으면 열립니다.");
                ui.separator();
                if files.is_empty() {
                    ui.label(match tab {
                        OpenTab::Recordings => {
                            "녹화 파일(.tvseq)이 없습니다. 실시간 화면에서 '● 녹화'로 만드세요."
                        }
                        OpenTab::Snapshots => "스냅샷이 없습니다. '📷 스냅샷'으로 만드세요.",
                    });
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        egui::Grid::new(("open_files", tab == OpenTab::Snapshots))
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                for (path, bytes, modified) in &files {
                                    let name = path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    // A snapshot is several files: show their common name.
                                    let name = name.trim_end_matches("_raw16.png");
                                    if ui.button(name).clicked() {
                                        chosen = Some(path.clone());
                                    }
                                    let when = chrono::DateTime::<chrono::Local>::from(*modified);
                                    ui.label(when.format("%Y-%m-%d %H:%M").to_string());
                                    ui.label(match tab {
                                        OpenTab::Recordings => {
                                            format!("{:.1} MB", *bytes as f64 / 1e6)
                                        }
                                        OpenTab::Snapshots => String::new(),
                                    });
                                    ui.end_row();
                                }
                            });
                    });
            });
        if refresh {
            self.open_list = None;
            self.open_snaps = None;
        }
        self.open_tab = tab;
        if let Some(p) = chosen {
            match tab {
                OpenTab::Recordings => self.open_sequence(&p),
                OpenTab::Snapshots => self.open_snapshot(&p),
            }
            open = false;
        }
        self.show_open_dialog = open;
    }

    /// Current spot readings as (id, value).
    fn spot_values(&self, f: &ThermalFrame) -> Vec<(u32, f32)> {
        self.settings
            .spots
            .items
            .iter()
            .filter_map(|s| s.value(f).map(|v| (s.id, v)))
            .collect()
    }

    /// Append the newest frame (taken at `t` seconds) to the time series and, if recording,
    /// to the CSV log.
    fn record(&mut self, t: f64) {
        let (Some(f), Some(st)) = (&self.frame, &self.stats) else {
            return;
        };
        let spots = self.spot_values(f);
        let sample = Sample {
            t,
            max: f.value(st.max),
            min: f.value(st.min),
            mean: f.value_f(st.mean),
            center: f.value_around(f.width / 2, f.height / 2, 1),
        };
        let ids: Vec<u32> = spots.iter().map(|(id, _)| *id).collect();
        self.history.retain_spots(&ids);
        let shapes: Vec<_> = self
            .settings
            .shapes
            .items
            .iter()
            .filter_map(|s| s.stats(f).map(|st| (s.id, st)))
            .collect();
        let ids: Vec<u32> = shapes.iter().map(|(id, _)| *id).collect();
        self.history.retain_shapes(&ids);
        let plotted: Vec<_> = shapes
            .iter()
            .map(|(id, st)| (*id, [st.max, st.mean]))
            .collect();
        self.history.push(sample, &spots, &plotted);
        // One row per frame, or one every `csv_interval_s` seconds of frame time.
        let every = self.settings.csv_interval_s as f64;
        let due = match self.csv_last {
            Some(last) => t < last || t - last >= every,
            None => true,
        };
        if let Some(log) = self.csv.as_mut().filter(|_| due) {
            // Keep the rhythm: the next row is due one interval after this one was.
            self.csv_last = Some(match self.csv_last {
                Some(last) if t >= last && t - last < 2.0 * every => last + every,
                _ => t,
            });
            let row = csv_log::Row {
                // Played-back frames carry no arrival time.
                captured: self.player.is_none().then_some(f.captured),
                sequence: f.sequence,
                max: sample.max,
                min: sample.min,
                mean: sample.mean,
                center: sample.center,
                fpa_c: self.last_fpa,
                spots: &spots,
                shapes: &shapes,
            };
            if let Err(e) = log.write(row) {
                let msg = format!("CSV 기록 중단: {e}");
                self.csv = None;
                self.notify(msg);
            }
        }
    }

    fn toggle_csv(&mut self) {
        let _ = if self.csv.is_some() {
            self.stop_csv().map(drop)
        } else {
            self.start_csv().map(drop)
        };
    }

    /// Finish the CSV log; returns its path and row count.
    fn stop_csv(&mut self) -> Result<(PathBuf, u64), String> {
        let log = self.csv.take().ok_or("not logging")?;
        match log.finish() {
            Ok((path, rows)) => {
                self.notify(format!("기록 종료: {rows}행 → {}", path.display()));
                Ok((path, rows))
            }
            Err(e) => {
                self.notify(format!("CSV 저장 실패: {e}"));
                Err(format!("saving the CSV failed: {e}"))
            }
        }
    }

    /// Start logging every frame to CSV. The user is notified of the outcome.
    fn start_csv(&mut self) -> Result<PathBuf, String> {
        if self.csv.is_some() {
            return Err("already logging".into());
        }
        self.check_not_held()?;
        if self.frame.is_none() {
            return Err("no frame yet".into());
        }
        self.csv_last = None;
        let unit = match self.frame.as_ref().map(|f| f.is_radiometric()) {
            Some(false) => "cnt",
            _ => "C",
        };
        let ids: Vec<u32> = self.settings.spots.items.iter().map(|s| s.id).collect();
        let shapes: Vec<(u32, String)> = self
            .settings
            .shapes
            .items
            .iter()
            .map(|s| (s.id, s.label()))
            .collect();
        match CsvLog::start(&self.settings.snapshot_dir(), &ids, &shapes, unit) {
            Ok(log) => {
                let path = log.path().to_path_buf();
                self.notify(format!("기록 시작: {}", path.display()));
                self.csv = Some(log);
                Ok(path)
            }
            Err(e) => {
                self.notify(format!("CSV 파일을 만들 수 없습니다: {e}"));
                Err(format!("cannot create the CSV: {e}"))
            }
        }
    }

    fn mark_dirty(&mut self) {
        self.settings_dirty.get_or_insert_with(Instant::now);
    }

    /// Show a message in the status line. Messages of the same pass (a snapshot saved and its
    /// AI copy started, recordings ended because the camera changed) are shown together
    /// instead of the last one hiding the others.
    fn notify(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        match &mut self.message {
            Some((shown, _))
                if self.message_pass == self.pass && shown.len() < 500 && !shown.contains(&msg) =>
            {
                shown.push_str(" · ");
                shown.push_str(&msg);
            }
            _ => {
                self.message = Some((msg, Instant::now()));
                self.message_pass = self.pass;
            }
        }
    }

    fn save_settings_if_due(&mut self, force: bool) {
        if let Some(t) = self.settings_dirty {
            if force || t.elapsed() > Duration::from_secs(1) {
                if let Err(e) = self.settings.save() {
                    log::warn!("saving settings failed: {e}");
                }
                self.settings_dirty = None;
            }
        }
    }

    /// Pull the next frame (the oldest queued camera frame, or the player's during playback) and
    /// re-render. Returns the frame time if a new *live* frame arrived.
    fn update_frame(&mut self) -> Option<f64> {
        let is_live = self.player.is_none();
        let (f, t) = if let Some(p) = &mut self.player {
            // Live frames are dropped during playback; the camera stays connected.
            while self.camera.take_frame().is_some() {}
            match p.tick() {
                Ok(Some((t_ms, f))) => (f, t_ms as f64 / 1000.0),
                Ok(None) => return None,
                Err(e) => {
                    let msg = format!("재생 오류: {e}");
                    self.close_player();
                    self.notify(msg);
                    return None;
                }
            }
        } else {
            if self.hold.is_some() {
                // The image is held: the camera stays connected, its frames are dropped.
                while self.camera.take_frame().is_some() {}
                return None;
            }
            let f = self.settings.rotation.apply(self.camera.take_frame()?);
            self.end_recordings_on_resize(&f);
            if self.seq.as_mut().is_some_and(|w| !w.push(&f)) {
                // The writer stopped on an error (disk full…); finishing reports it.
                let _ = self.stop_seq();
            }
            // Time of arrival, not of processing: queued frames are handled in a burst.
            let age = SystemTime::now()
                .duration_since(f.captured)
                .unwrap_or_default();
            let t = self.started.elapsed().saturating_sub(age);
            (f, t.as_secs_f64())
        };
        // The noise filter averages neighbours in time: not across a gap (a shutter
        // correction, a stall) or a jump in a recording. The recording above keeps the
        // camera's own frames.
        if self
            .filter_t
            .is_some_and(|last| !(-0.5..=1.0).contains(&(t - last)))
        {
            self.filter.reset();
        }
        self.filter_t = Some(t);
        let mut f = self.filter.apply(f, self.settings.noise_filter);
        calibration::attach(self, &mut f);
        self.stats = Some(f.stats());
        self.frame = Some(f);
        self.render();
        self.push_video(t);
        self.record(t);
        is_live.then_some(t)
    }

    /// One CSV, recording or MP4 holds one image size and one camera. When a live frame of
    /// another size arrives (another camera took over), end them with a message instead of
    /// silently recording nothing, or mixing two cameras in one file.
    fn end_recordings_on_resize(&mut self, new: &ThermalFrame) {
        let resized = self
            .frame
            .as_ref()
            .is_some_and(|old| (old.width, old.height) != (new.width, new.height));
        if !resized || (self.csv.is_none() && self.seq.is_none() && self.video.is_none()) {
            return;
        }
        self.notify("영상 크기가 바뀌어(카메라 변경) 기록을 끝냅니다");
        if self.csv.is_some() {
            let _ = self.stop_csv();
        }
        if self.seq.is_some() {
            let _ = self.stop_seq();
        }
        if self.video.is_some() {
            let _ = self.stop_video();
        }
    }

    /// Recolor the current frame (after a new frame or a palette/range change). The texture is
    /// uploaded later by [`Self::upload_texture`], only when the window is drawn: texture
    /// updates made while it is hidden would pile up undelivered.
    fn render(&mut self) {
        let (Some(f), Some(st)) = (&self.frame, &self.stats) else {
            return;
        };
        let key = (self.settings.palette, self.settings.palette_invert);
        if self.lut.0 != key {
            self.lut = (key, key.0.lut_of(key.1));
        }
        self.display_range = match self.settings.range {
            RangeMode::Auto => self.auto_range.update(f.value(st.min), f.value(st.max)),
            RangeMode::Robust => {
                let (lo, hi) = range::percentiles(&f.raw, range::ROBUST_TAIL, &mut self.scratch)
                    .unwrap_or((st.min, st.max));
                self.auto_range.update(f.value(lo), f.value(hi))
            }
            RangeMode::Manual { lo, hi } => (lo.min(hi), hi.max(lo)),
        };
        let lo = f.raw_from_value(self.display_range.0);
        let hi = f.raw_from_value(self.display_range.1);
        palette::colorize(&f.raw, lo, hi, &self.lut.1, &mut self.rgba);
        let iso = self.settings.isotherm;
        let band = iso.raw_band(f);
        self.iso_share = band.map(|(a, b)| {
            let painted = palette::paint_band(&f.raw, a, b, iso.color, &mut self.rgba);
            painted as f32 / f.raw.len().max(1) as f32
        });
        let up = self.settings.upscale;
        if up.enabled {
            let u = match &mut self.upscaler {
                Some(u) if u.sharpen() == up.sharpen => u,
                slot => slot.insert(Upscaler::new(up.sharpen)),
            };
            let field = u.upscale(&f.raw, f.width, f.height);
            palette::colorize_f32(&field, lo, hi, &self.lut.1, &mut self.rgba_up);
            if let Some((a, b)) = band {
                palette::paint_band_f32(&field, a, b, iso.color, &mut self.rgba_up);
            }
        } else {
            self.rgba_up.clear();
            self.upscaler = None;
        }
        self.texture_dirty = true;
    }

    fn upload_texture(&mut self, ctx: &egui::Context) {
        let Some(f) = &self.frame else {
            return;
        };
        if !self.texture_dirty || self.rgba.len() != f.width * f.height * 4 {
            return;
        }
        self.texture_dirty = false;
        // The upscaled image when there is one (smooth), else the sensor pixels (sharp squares).
        let (n, filter) = (upscale::FACTOR, egui::TextureOptions::LINEAR);
        let (image, options) = if self.rgba_up.len() == self.rgba.len() * n * n {
            let size = [f.width * n, f.height * n];
            (
                egui::ColorImage::from_rgba_unmultiplied(size, &self.rgba_up),
                filter,
            )
        } else {
            let size = [f.width, f.height];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &self.rgba);
            (image, egui::TextureOptions::NEAREST)
        };
        match &mut self.texture {
            Some(t) => t.set(image, options),
            None => self.texture = Some(ctx.load_texture("thermal", image, options)),
        }
    }

    /// Save a snapshot of the current frame; returns the color PNG path.
    fn take_snapshot(&mut self, status: &CameraStatus) -> Result<PathBuf, String> {
        let (Some(f), Some(st)) = (&self.frame, &self.stats) else {
            self.notify("저장할 영상이 없습니다");
            return Err("no frame yet".into());
        };
        let cam = webcam::layer(self);
        let n = upscale::FACTOR;
        let upscaled = (self.rgba_up.len() == f.width * f.height * 4 * n * n)
            .then_some(self.rgba_up.as_slice());
        let ai = self
            .upscayl
            .clone()
            .filter(|_| self.settings.upscale.ai)
            .map(|u| {
                let input = crate::upscayl::Input {
                    raw: f.raw.clone(),
                    width: f.width,
                    height: f.height,
                    lo: f.raw_from_value(self.display_range.0),
                    hi: f.raw_from_value(self.display_range.1),
                    lut: self.lut.1,
                };
                (u, input)
            });
        let meta = SnapshotMeta {
            app: "thermalview",
            version: env!("CARGO_PKG_VERSION"),
            // When the frame arrived, which is not now if the camera has stopped since.
            // Played-back frames carry no arrival time.
            captured_local: if f.captured == SystemTime::UNIX_EPOCH {
                chrono::Local::now()
            } else {
                chrono::DateTime::<chrono::Local>::from(f.captured)
            }
            .to_rfc3339(),
            width: f.width,
            height: f.height,
            kelvin_per_count: f.kelvin_per_count,
            unit: f.unit(),
            min: f.value(st.min),
            max: f.value(st.max),
            mean: f.value_f(st.mean),
            palette: self.settings.palette.label().to_string(),
            range: self.display_range,
            spots: self
                .settings
                .spots
                .items
                .iter()
                .filter_map(|s| {
                    s.value(f).map(|value| SpotReading {
                        id: s.id,
                        x: s.x,
                        y: s.y,
                        value,
                    })
                })
                .collect(),
            shapes: self
                .settings
                .shapes
                .items
                .iter()
                .filter_map(|s| {
                    s.stats(f).map(|st| ShapeReading {
                        label: s.label(),
                        kind: format!("{:?}", s.kind),
                        a: s.a,
                        b: s.b,
                        min: st.min,
                        max: st.max,
                        mean: st.mean,
                    })
                })
                .collect(),
            deltas: self
                .settings
                .deltas
                .items
                .iter()
                .filter_map(|d| {
                    let s = &self.settings;
                    d.read(f, st, &s.spots, &s.shapes)
                        .map(|value| DeltaReading {
                            label: d.label(&s.shapes),
                            value,
                        })
                })
                .collect(),
            noise_filter: Some(self.settings.noise_filter).filter(|n| *n > 1),
            device: status.device.as_ref().map(|d| d.product.clone()),
            lepton_part: status.part_number.clone(),
            fpa_c: status.fpa_c,
            shutter_mode: status.shutter_mode.map(|m| format!("{m:?}")),
            calibration: f
                .calibration
                .is_some()
                .then(|| self.settings.calibration.clone()),
            webcam: cam.as_ref().map(|(pic, overlay)| WebcamInfo {
                device: self.settings.webcam.device_name.clone(),
                width: pic.width,
                height: pic.height,
                overlay: *overlay,
            }),
            upscale: upscaled.is_some().then_some(self.settings.upscale),
            ai_model: ai.as_ref().map(|(u, _)| u.model.clone()),
        };
        let dir = self.settings.snapshot_dir();
        let picture = cam.as_ref().map(|(pic, _)| pic.as_ref());
        let temps = self.settings.snapshot_temps_csv;
        match snapshot::save(&dir, f, &self.rgba, upscaled, picture, temps, &meta) {
            Ok(files) => {
                self.notify(format!("저장됨: {}", files.color_png.display()));
                if let Some((u, input)) = ai {
                    // Each copy is a separate process on the GPU: a burst of snapshots (a
                    // remote client in a loop) must not stack them up.
                    if self.ai_jobs.len() < MAX_AI_JOBS {
                        self.start_ai_upscale(u, input, files.color_png.clone());
                    } else {
                        self.notify("AI 업스케일이 밀려 있어 이 스냅샷은 건너뜁니다");
                    }
                }
                Ok(files.color_png)
            }
            Err(e) => {
                self.notify(format!("스냅샷 저장 실패: {e}"));
                Err(format!("snapshot failed: {e}"))
            }
        }
    }

    /// The settings panel: foldable sections, each with a one-line summary in its header so
    /// a folded section still tells its state.
    fn controls(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let summary = format!("{} · {:.1} fps", status.state.label(), status.fps);
            self.section(
                ui,
                |l| &mut l.camera,
                "카메라",
                &summary,
                |app, ui| app.camera_controls(ui, status),
            );

            // The connected camera, or the one asked for before anything connects. Under
            // "자동" with nothing connected there is no camera to show controls for.
            let kind = status.kind.or(match self.settings.camera {
                Source::P3 => Some(Kind::P3),
                Source::PureThermal => Some(Kind::PureThermal),
                Source::Auto => None,
            });
            match kind {
                Some(Kind::P3) => {
                    self.section(
                        ui,
                        |l| &mut l.device,
                        "P3 설정",
                        "",
                        |app, ui| app.p3_controls(ui, status),
                    );
                }
                Some(Kind::PureThermal) => {
                    let mode = self.settings.shutter_mode.label();
                    self.section(
                        ui,
                        |l| &mut l.device,
                        "Lepton 보정(FFC)",
                        mode,
                        |app, ui| app.lepton_controls(ui, status),
                    );
                }
                None => {}
            }

            let mut summary = format!(
                "{} · {}",
                self.settings.palette.label(),
                match self.settings.range {
                    RangeMode::Auto | RangeMode::Robust => "자동 범위",
                    RangeMode::Manual { .. } => "수동 범위",
                }
            );
            if self.settings.isotherm.enabled {
                summary.push_str(" · 등온선");
            }
            self.section(
                ui,
                |l| &mut l.display,
                "표시",
                &summary,
                |app, ui| app.display_controls(ui),
            );

            let (spots, shapes) = (
                self.settings.spots.items.len(),
                self.settings.shapes.items.len(),
            );
            let summary = match (spots, shapes) {
                (0, 0) => "없음".to_owned(),
                (s, 0) => format!("점 {s}"),
                (0, b) => format!("영역·선 {b}"),
                (s, b) => format!("점 {s} · 영역·선 {b}"),
            };
            self.section(
                ui,
                |l| &mut l.measure,
                "측정",
                &summary,
                |app, ui| app.measure_controls(ui),
            );

            let summary = webcam::summary(self);
            self.section(
                ui,
                |l| &mut l.webcam,
                "웹캠 합성",
                &summary,
                |app, ui| webcam::controls(ui, app),
            );

            let summary = if self.settings.calibration.is_identity() {
                "없음".to_owned()
            } else {
                self.settings.calibration.summary()
            };
            self.section(
                ui,
                |l| &mut l.calibration,
                "온도 보정",
                &summary,
                |app, ui| calibration::controls(ui, app, status),
            );

            let summary = alarms::summary(self);
            self.section(
                ui,
                |l| &mut l.alarms,
                "온도 경보",
                &summary,
                |app, ui| alarms::controls(ui, app),
            );

            let summary = self
                .settings
                .snapshot_dir()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.section(
                ui,
                |l| &mut l.storage,
                "저장",
                &summary,
                |app, ui| app.storage_controls(ui, status),
            );

            let summary = remote::summary(self);
            self.section(
                ui,
                |l| &mut l.remote,
                "원격 제어",
                &summary,
                |app, ui| remote::controls(ui, app),
            );
        });
    }

    /// Where snapshots, recordings, MP4 and CSV files go, and what is written when.
    fn storage_controls(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        self.saving_options(ui, status);
        ui.separator();
        ui.label("스냅샷, 녹화, MP4, CSV 파일을 저장하는 폴더입니다.");
        let current = self.settings.snapshot_dir();
        let text = self
            .dir_edit
            .get_or_insert_with(|| current.display().to_string());
        ui.add(
            egui::TextEdit::singleline(text)
                .desired_width(f32::INFINITY)
                .hint_text(r"예: D:\Thermal"),
        );
        let typed = PathBuf::from(text.trim());
        let changed = !text.trim().is_empty() && typed != current;
        let custom = self.settings.snapshot_dir.is_some();
        let (mut apply, mut reset) = (false, false);
        ui.horizontal(|ui| {
            apply = ui
                .add_enabled(changed, egui::Button::new("적용"))
                .on_hover_text("폴더가 없으면 만듭니다")
                .clicked();
            reset = ui
                .add_enabled(custom, egui::Button::new("기본 폴더로"))
                .on_hover_text(r"사진\ThermalView")
                .clicked();
            if ui.button("📁 열기").clicked() {
                let _ = std::fs::create_dir_all(&current);
                let _ = std::process::Command::new("explorer").arg(&current).spawn();
            }
        });
        ui.weak("기록 중인 파일은 시작한 폴더에 끝까지 저장됩니다.");
        if apply {
            if !typed.is_absolute() {
                self.notify(r"드라이브부터 시작하는 전체 경로를 입력하세요(예: D:\Thermal)");
            } else {
                match std::fs::create_dir_all(&typed) {
                    Ok(()) => {
                        self.notify(format!("저장 폴더: {}", typed.display()));
                        self.settings.snapshot_dir = Some(typed);
                        self.dir_edit = None;
                        self.open_list = None;
                        self.mark_dirty();
                    }
                    Err(e) => self.notify(format!("폴더를 만들 수 없습니다: {e}")),
                }
            }
        }
        if reset {
            self.settings.snapshot_dir = None;
            self.dir_edit = None;
            self.open_list = None;
            self.mark_dirty();
            let dir = self.settings.snapshot_dir();
            self.notify(format!("저장 폴더: {}", dir.display()));
        }
    }

    /// CSV interval, the pixel table of snapshots and the interval snapshots.
    fn saving_options(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        let mut changed = false;
        let mut every = self.settings.csv_interval_s;
        ui.horizontal(|ui| {
            ui.label("CSV 기록 간격");
            egui::ComboBox::from_id_salt("csv_interval")
                .selected_text(interval_label(every))
                .show_ui(ui, |ui| {
                    for s in [0.0, 1.0, 5.0, 10.0, 60.0, 600.0] {
                        ui.selectable_value(&mut every, s, interval_label(s));
                    }
                })
                .response
                .on_hover_text(
                    "CSV 기록에 한 행을 쓰는 간격입니다. 오래 감시할 때 파일이 작아집니다",
                );
        });
        if every != self.settings.csv_interval_s {
            self.settings.csv_interval_s = every;
            changed = true;
        }
        changed |= ui
            .checkbox(
                &mut self.settings.snapshot_temps_csv,
                "스냅샷에 온도 표 CSV 추가",
            )
            .on_hover_text(
                "스냅샷마다 모든 화소의 온도를 _temps.csv로 함께 저장합니다(영상의 행마다 한 줄). \
                 엑셀 등에서 분석할 때 씁니다",
            )
            .changed();
        let running = self.timelapse.as_ref().map(|t| t.shots);
        let mut toggle = false;
        ui.horizontal(|ui| {
            ui.label("간격 스냅샷");
            ui.add_enabled_ui(running.is_none(), |ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.settings.timelapse_s)
                            .range(1..=3600)
                            .suffix(" 초마다"),
                    )
                    .changed();
            });
            let text = match running {
                Some(n) => format!("■ 정지 ({n}장)"),
                None => "시작".to_owned(),
            };
            toggle = ui
                .add_enabled(
                    running.is_some() || self.frame.is_some(),
                    egui::Button::new(text),
                )
                .on_hover_text(
                    "정한 간격마다 스냅샷을 저장합니다. 새 영상이 없으면(정지, 카메라 끊김) \
                     찍지 않습니다",
                )
                .clicked();
        });
        if toggle {
            self.toggle_timelapse(status);
        }
        if changed {
            self.mark_dirty();
        }
    }

    fn toggle_timelapse(&mut self, status: &CameraStatus) {
        match self.timelapse.take() {
            Some(t) => self.notify(format!("간격 스냅샷 종료: {}장", t.shots)),
            None => {
                self.timelapse = Some(Timelapse {
                    next: Instant::now(),
                    shots: 0,
                    last: None,
                });
                self.run_timelapse(status);
            }
        }
    }

    /// Take the next picture of the interval series when it is due and the image is new.
    fn run_timelapse(&mut self, status: &CameraStatus) {
        let (Some(t), Some(f)) = (&self.timelapse, &self.frame) else {
            return;
        };
        let id = (f.sequence, f.captured);
        if Instant::now() < t.next || t.last == Some(id) {
            return;
        }
        let saved = self.take_snapshot(status).is_ok();
        let every = Duration::from_secs(self.settings.timelapse_s.max(1) as u64);
        match &mut self.timelapse {
            Some(t) if saved => {
                t.shots += 1;
                t.last = Some(id);
                t.next = Instant::now() + every;
            }
            // The reason (disk full, folder gone) is already in the status line.
            _ => {
                self.timelapse = None;
                self.notify("간격 스냅샷을 멈췄습니다");
            }
        }
    }

    /// Put the color image (the upscaled one when shown) on the clipboard.
    fn copy_image(&mut self, ctx: &egui::Context) {
        let Some(f) = &self.frame else {
            return;
        };
        let n = upscale::FACTOR;
        let (size, pixels) = if self.rgba_up.len() == f.width * f.height * 4 * n * n {
            ([f.width * n, f.height * n], &self.rgba_up)
        } else if self.rgba.len() == f.width * f.height * 4 {
            ([f.width, f.height], &self.rgba)
        } else {
            return;
        };
        ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(size, pixels));
        self.notify(format!(
            "영상을 클립보드로 복사했습니다 ({}×{})",
            size[0], size[1]
        ));
    }

    /// Differences whose Spot, Box or Line was deleted go with it.
    fn prune_deltas(&mut self) {
        let s = &mut self.settings;
        if s.deltas.prune(&s.spots, &s.shapes) > 0 {
            self.mark_dirty();
        }
    }

    /// One foldable section of the settings panel. Which sections are open is a setting.
    fn section(
        &mut self,
        ui: &mut egui::Ui,
        pick: fn(&mut Layout) -> &mut bool,
        title: &str,
        summary: &str,
        body: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let open = *pick(&mut self.settings.layout);
        if section_header(ui, title, summary, open) {
            *pick(&mut self.settings.layout) = !open;
            self.mark_dirty();
        }
        if open {
            ui.push_id(title, |ui| {
                ui.add_space(2.0);
                body(self, ui);
            });
        }
        ui.add_space(6.0);
    }

    fn camera_controls(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        {
            let mut source = self.settings.camera;
            // Another camera means another image size: not in the middle of a file.
            let recording = self.csv.is_some() || self.seq.is_some() || self.video.is_some();
            ui.add_enabled_ui(!recording, |ui| {
                egui::ComboBox::from_label("카메라 종류")
                    .selected_text(source.label())
                    .show_ui(ui, |ui| {
                        for s in Source::ALL {
                            ui.selectable_value(&mut source, s, s.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "자동은 PureThermal을 먼저 찾고, 없으면 Thermal Master P3를 찾습니다",
                    )
                    .on_disabled_hover_text("CSV·녹화·MP4 기록 중에는 바꿀 수 없습니다");
            });
            if source != self.settings.camera {
                self.settings.camera = source;
                self.camera.set_source(source);
                self.mark_dirty();
            }
            egui::Grid::new("cam_info").num_columns(2).show(ui, |ui| {
                if let Some(d) = &status.device {
                    ui.label("장치");
                    ui.label(&d.product);
                    ui.end_row();
                }
                if let Some(p) = &status.part_number {
                    ui.label("Lepton");
                    ui.label(format!(
                        "{p} / {}",
                        status.lepton_sw.as_deref().unwrap_or("?")
                    ));
                    ui.end_row();
                }
                if let Some(fw) = &status.firmware {
                    ui.label("펌웨어");
                    ui.label(fw);
                    ui.end_row();
                }
                ui.label("프레임");
                ui.label(format!("{:.1} fps", status.fps));
                ui.end_row();
                if let Some(t) = status.fpa_c {
                    ui.label("센서(FPA)");
                    ui.label(format!("{t:.2} °C"));
                    ui.end_row();
                }
                ui.label("온도 단위");
                ui.label(match (status.kelvin_per_count, status.kind) {
                    (Some(k), Some(Kind::P3)) => format!("{k} K"),
                    (Some(k), _) => format!("TLinear {k} K"),
                    (None, _) => "원시값(온도 아님)".into(),
                });
                ui.end_row();
                ui.label("받은 프레임");
                ui.label(status.frames.to_string());
                ui.end_row();
                ui.label("정지 복구")
                    .on_hover_text("영상이 멈춰 카메라를 다시 연 횟수");
                ui.label(format!("{}회", status.stalls));
                ui.end_row();
            });
            if status.state == ConnState::Busy {
                ui.label(
                    "GetThermal, Thermal Master 프로그램 등 카메라를 쓰는 다른 프로그램을 \
                     닫아 주세요.",
                );
            }
            if status.state == ConnState::Searching {
                ui.label(match source {
                    Source::Auto => {
                        "PureThermal 또는 P3가 연결돼 있고 WinUSB 드라이버가 설치됐는지 \
                         확인하세요."
                    }
                    Source::PureThermal => {
                        "PureThermal이 연결돼 있고 WinUSB 드라이버가 설치됐는지 확인하세요."
                    }
                    Source::P3 => "P3가 연결돼 있고 WinUSB 드라이버가 설치됐는지 확인하세요.",
                });
            }
        }
    }

    fn display_controls(&mut self, ui: &mut egui::Ui) {
        {
            let mut pal = self.settings.palette;
            egui::ComboBox::from_label("팔레트")
                .selected_text(pal.label())
                .show_ui(ui, |ui| {
                    for p in Palette::ALL {
                        ui.selectable_value(&mut pal, p, p.label());
                    }
                });
            let mut changed = pal != self.settings.palette;
            self.settings.palette = pal;
            changed |= ui
                .checkbox(&mut self.settings.palette_invert, "팔레트 뒤집기")
                .on_hover_text("색 순서를 거꾸로 씁니다(뜨거운 쪽이 어두운 색)")
                .changed();

            let unit = self.frame.as_ref().map(|f| f.unit()).unwrap_or("°C");
            let was = self.settings.range;
            let mut manual = matches!(was, RangeMode::Manual { .. });
            let mut robust = was == RangeMode::Robust;
            ui.horizontal(|ui| {
                ui.radio_value(&mut manual, false, "자동 범위");
                ui.radio_value(&mut manual, true, "수동 범위");
            });
            if !manual {
                ui.checkbox(&mut robust, "극단값 제외 (1~99 %)")
                    .on_hover_text(
                        "가장 차갑고 뜨거운 화소 1 %씩을 빼고 색 범위를 잡습니다. 뜨거운 점 \
                         하나 때문에 나머지가 한 색으로 보일 때 켜세요",
                    );
            }
            match (was, manual, robust) {
                (RangeMode::Manual { .. }, true, _) => {}
                (_, true, _) => {
                    self.settings.range = RangeMode::Manual {
                        lo: self.display_range.0.floor(),
                        hi: self.display_range.1.ceil(),
                    }
                }
                (_, false, true) => self.settings.range = RangeMode::Robust,
                (_, false, false) => self.settings.range = RangeMode::Auto,
            }
            if self.settings.range != was {
                changed = true;
                if !manual {
                    self.auto_range.reset();
                }
            }
            if let RangeMode::Manual { lo, hi } = &mut self.settings.range {
                fn range_field<'a>(v: &'a mut f32, unit: &str) -> egui::DragValue<'a> {
                    egui::DragValue::new(v)
                        .speed(0.1)
                        .fixed_decimals(1)
                        .range(-300.0..=70_000.0)
                        .suffix(format!(" {unit}"))
                }
                ui.horizontal(|ui| {
                    ui.label("최저");
                    let lo_changed = ui.add(range_field(lo, unit)).changed();
                    ui.label("최고");
                    let hi_changed = ui.add(range_field(hi, unit)).changed();
                    // Kept in order: the one being edited pushes the other along.
                    if lo_changed && *lo >= *hi {
                        *hi = *lo + 0.1;
                    }
                    if hi_changed && *hi <= *lo {
                        *lo = *hi - 0.1;
                    }
                    changed |= lo_changed || hi_changed;
                });
                let fit = ui
                    .button("지금 화면에 맞춤")
                    .on_hover_text("지금 영상의 최저·최고 온도로 범위를 맞춥니다")
                    .clicked();
                if let (true, Some(f), Some(st)) = (fit, &self.frame, &self.stats) {
                    *lo = f.value(st.min).floor();
                    *hi = f.value(st.max).ceil().max(*lo + 1.0);
                    changed = true;
                }
            }
            changed |= self.isotherm_controls(ui, unit);
            changed |= ui
                .checkbox(&mut self.settings.show_extremes, "최고·최저 위치 표시")
                .changed();
            let mut strength = self.settings.noise_filter;
            egui::ComboBox::from_label("잡음 필터")
                .selected_text(filter_label(strength))
                .show_ui(ui, |ui| {
                    for s in Temporal::STRENGTHS {
                        ui.selectable_value(&mut strength, s, filter_label(s));
                    }
                })
                .response
                .on_hover_text(
                    "화소마다 최근 프레임을 평균해 잡음을 줄입니다. 1도 미만의 차이가 잘 \
                     보이지만 온도 변화를 조금 늦게 따라갑니다. 측정·CSV·경보에도 적용되고, \
                     녹화(.tvseq)에는 필터 전 원본이 저장됩니다",
                );
            if strength != self.settings.noise_filter {
                self.settings.noise_filter = strength;
                self.mark_dirty();
            }
            let can_rotate = self.rotation_lock().is_none();
            let mut rot = self.settings.rotation;
            ui.add_enabled_ui(can_rotate, |ui| {
                egui::ComboBox::from_label("영상 회전")
                    .selected_text(rot.label())
                    .show_ui(ui, |ui| {
                        for r in Rotation::ALL {
                            ui.selectable_value(&mut rot, r, r.label());
                        }
                    })
                    .response
                    .on_disabled_hover_text(self.rotation_lock().map_or("", |l| l.0));
            });
            if rot != self.settings.rotation {
                let _ = self.set_rotation(rot);
            }
            changed |= ui
                .checkbox(&mut self.settings.upscale.enabled, "4배 업스케일")
                .on_hover_text(
                    "센서 흐림을 되돌리는 선명화(위너 필터) 뒤 Lanczos 보간으로 4배 확대해 보여 줍니다. \
                     MP4와 스냅샷(_x4.png)에도 들어갑니다. 측정값은 계속 센서 화소 기준입니다.",
                )
                .changed();
            if self.settings.upscale.enabled {
                let mut s = self.settings.upscale.sharpen;
                egui::ComboBox::from_id_salt("upscale_sharpen")
                    .selected_text(s.label())
                    .show_ui(ui, |ui| {
                        for v in Sharpen::ALL {
                            ui.selectable_value(&mut s, v, v.label());
                        }
                    })
                    .response
                    .on_hover_text("강하게는 미세한 부분이 또렷해지지만 센서 잡음도 함께 커집니다");
                changed |= s != self.settings.upscale.sharpen;
                self.settings.upscale.sharpen = s;
            }
            match &self.upscayl {
                Some(u) => {
                    let hint = format!(
                        "스냅샷마다 Upscayl({})로 4배 확대한 _ai_x4.png를 추가로 만듭니다. 1~2초 걸립니다.",
                        u.model
                    );
                    if ui
                        .checkbox(&mut self.settings.upscale.ai, "스냅샷에 AI 업스케일 추가")
                        .on_hover_text(hint)
                        .changed()
                    {
                        self.mark_dirty();
                    }
                }
                None => {
                    ui.add_enabled(
                        false,
                        egui::Checkbox::new(&mut false, "스냅샷에 AI 업스케일 추가"),
                    )
                    .on_disabled_hover_text(
                        "Upscayl이 설치돼 있지 않습니다. 설치(winget install Upscayl.Upscayl) 후 앱을 다시 시작하세요.",
                    );
                }
            }
            if changed {
                self.mark_dirty();
                self.render();
            }
        }
    }

    /// Isotherm: the band, its temperatures and color. Returns true when the image must be
    /// recolored.
    fn isotherm_controls(&mut self, ui: &mut egui::Ui, unit: &str) -> bool {
        let share = self.iso_share;
        let iso = &mut self.settings.isotherm;
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut iso.enabled, "등온선")
                .on_hover_text(
                    "정한 온도 이상·이하·구간의 화소를 한 색으로 칠합니다. 스냅샷과 MP4에도 \
                     들어가고 측정값은 바뀌지 않습니다 (I)",
                )
                .changed();
            if !iso.enabled {
                return;
            }
            egui::ComboBox::from_id_salt("iso_band")
                .width(52.0)
                .selected_text(iso.band.label())
                .show_ui(ui, |ui| {
                    for b in Band::ALL {
                        changed |= ui.selectable_value(&mut iso.band, b, b.label()).changed();
                    }
                });
            changed |= ui
                .color_edit_button_srgb(&mut iso.color)
                .on_hover_text("칠하는 색")
                .changed();
        });
        if !iso.enabled {
            return changed;
        }
        fn edge<'a>(v: &'a mut f32, unit: &str) -> egui::DragValue<'a> {
            egui::DragValue::new(v)
                .speed(0.1)
                .fixed_decimals(1)
                .range(-300.0..=70_000.0)
                .suffix(format!(" {unit}"))
        }
        ui.horizontal(|ui| {
            match iso.band {
                Band::Above => changed |= ui.add(edge(&mut iso.lo, unit)).changed(),
                Band::Below => changed |= ui.add(edge(&mut iso.hi, unit)).changed(),
                Band::Between => {
                    let lo_changed = ui.add(edge(&mut iso.lo, unit)).changed();
                    ui.label("~");
                    let hi_changed = ui.add(edge(&mut iso.hi, unit)).changed();
                    // Kept in order: the one being edited pushes the other along.
                    if lo_changed && iso.lo >= iso.hi {
                        iso.hi = iso.lo + 0.1;
                    }
                    if hi_changed && iso.hi <= iso.lo {
                        iso.lo = iso.hi - 0.1;
                    }
                    changed |= lo_changed || hi_changed;
                }
            }
            if iso.band != Band::Between {
                ui.label(iso.band.label());
            }
            if let Some(share) = share {
                ui.weak(format!("화소 {:.1} %", share * 100.0));
            }
        });
        changed
    }

    /// The list of measurements with their readings. The tools and the whole-image readings
    /// are in the bar above the image.
    fn measure_controls(&mut self, ui: &mut egui::Ui) {
        {
            let unit = self.frame.as_ref().map(|f| f.unit()).unwrap_or("°C");
            if self.frame.as_ref().is_some_and(|f| f.calibration.is_some()) {
                ui.colored_label(
                    calibration::ACTIVE,
                    format!("보정 적용: {}", self.settings.calibration.summary()),
                );
            }
            let mut remove = None;
            for s in &self.settings.spots.items {
                let v = self
                    .frame
                    .as_ref()
                    .and_then(|f| s.value(f))
                    .map(|v| format!("{v:.2} {unit}"))
                    .unwrap_or_else(|| "-".into());
                ui.horizontal(|ui| {
                    ui.label(format!("S{} ({}, {})", s.id, s.x, s.y));
                    ui.strong(v);
                    if ui.small_button("✕").clicked() {
                        remove = Some(s.id);
                    }
                });
            }
            if let Some(id) = remove {
                self.settings.spots.remove(id);
                self.mark_dirty();
            }
            let mut remove_shape = None;
            for s in &self.settings.shapes.items {
                let st = self.frame.as_ref().and_then(|f| s.stats(f));
                ui.horizontal(|ui| {
                    ui.colored_label(view::SHAPE_COLOR, s.label());
                    match st {
                        Some(st) => ui.label(format!(
                            "↑{:.1} ↓{:.1} ~{:.1} {unit}",
                            st.max, st.min, st.mean
                        )),
                        None => ui.label("-"),
                    };
                    if ui.small_button("✕").clicked() {
                        remove_shape = Some(s.id);
                    }
                });
            }
            if let Some(id) = remove_shape {
                self.settings.shapes.remove(id);
                self.mark_dirty();
            }
            self.delta_controls(ui, unit);
            let count = self.settings.spots.items.len() + self.settings.shapes.items.len();
            if count == 0 {
                self.confirm_clear = false;
            }
            if self.confirm_clear {
                // Deleting them all also deletes the alarm rules that watch them: ask first.
                ui.horizontal(|ui| {
                    ui.label(format!("측정 {count}개를 모두 지울까요?"));
                    if ui.button("지우기").clicked() {
                        self.settings.spots.clear();
                        self.settings.shapes.clear();
                        self.confirm_clear = false;
                        self.mark_dirty();
                    }
                    if ui.button("취소").clicked() {
                        self.confirm_clear = false;
                    }
                });
            } else if count > 0 {
                if ui
                    .button("측정 모두 삭제…")
                    .on_hover_text("이 측정을 쓰는 경보 규칙도 함께 지워집니다")
                    .clicked()
                {
                    self.confirm_clear = true;
                }
            } else {
                ui.weak(
                    "영상 위 도구 막대에서 점·영역·선을 고른 뒤 영상을 클릭하거나 끌어 \
                     측정을 추가하세요.",
                );
            }
        }
    }

    /// Differences between two measurements: the list, and two pickers to add one.
    fn delta_controls(&mut self, ui: &mut egui::Ui, unit: &str) {
        let s = &self.settings;
        let mut remove = None;
        for (i, d) in s.deltas.items.iter().enumerate() {
            let value = match (&self.frame, &self.stats) {
                (Some(f), Some(st)) => d.read(f, st, &s.spots, &s.shapes),
                _ => None,
            };
            ui.horizontal(|ui| {
                ui.label(format!("Δ {}", d.label(&s.shapes)));
                ui.strong(value.map_or("-".into(), |v| format!("{v:+.2} {unit}")));
                if ui.small_button("✕").clicked() {
                    remove = Some(i);
                }
            });
        }
        let options = alarms::source_options(&s.spots, &s.shapes);
        let (mut a, mut b) = self.delta_pick;
        // A picked measurement that was deleted falls back to the first choice.
        for pick in [&mut a, &mut b] {
            if !options.contains(pick) {
                *pick = options[0];
            }
        }
        let full = s.deltas.items.len() >= Deltas::MAX;
        let mut add = false;
        ui.horizontal(|ui| {
            ui.label("차");
            for (salt, pick) in [("delta_a", &mut a), ("delta_b", &mut b)] {
                if salt == "delta_b" {
                    ui.label("−");
                }
                egui::ComboBox::from_id_salt(salt)
                    .width(78.0)
                    .selected_text(pick.label(&s.shapes))
                    .show_ui(ui, |ui| {
                        for o in &options {
                            ui.selectable_value(pick, *o, o.label(&s.shapes));
                        }
                    });
            }
            add = ui
                .add_enabled(!full && a != b, egui::Button::new("추가"))
                .on_hover_text("두 측정값의 차(앞 − 뒤)를 목록에 계속 표시합니다")
                .on_disabled_hover_text(if full {
                    "차는 4개까지 둘 수 있습니다"
                } else {
                    "서로 다른 두 측정을 고르세요"
                })
                .clicked();
        });
        self.delta_pick = (a, b);
        if let Some(i) = remove {
            self.settings.deltas.items.remove(i);
            self.mark_dirty();
        }
        if add && self.settings.deltas.add(Delta { a, b }) {
            self.mark_dirty();
        }
    }

    /// Make the AI-upscaled copy of a saved snapshot on a helper thread (it takes a second or
    /// two); [`Self::poll_ai_jobs`] reports the result.
    fn start_ai_upscale(
        &mut self,
        u: crate::upscayl::Upscayl,
        input: crate::upscayl::Input,
        color_png: PathBuf,
    ) {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("upscayl".into())
            .spawn(move || {
                let out = crate::upscayl::output_for(&color_png);
                let r = crate::upscayl::make(&u, &input, &out).map(|_| out);
                let _ = tx.send(r.map_err(|e| format!("{e:#}")));
            });
        match spawned {
            Ok(_) => {
                self.ai_jobs.push(rx);
                self.notify("AI 업스케일 중…");
            }
            Err(e) => self.notify(format!("AI 업스케일을 시작할 수 없습니다: {e}")),
        }
    }

    fn poll_ai_jobs(&mut self) {
        let mut done = Vec::new();
        self.ai_jobs.retain(|rx| match rx.try_recv() {
            Ok(r) => {
                done.push(r);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for r in done {
            self.notify(match r {
                Ok(p) => format!("AI 업스케일 저장됨: {}", p.display()),
                Err(e) => format!("AI 업스케일 실패: {e}"),
            });
        }
    }

    /// Why the rotation cannot change now: a recording keeps one image size and fixed
    /// measurement positions, and played-back frames were already turned when recorded.
    /// Returns (message for the window, message for remote clients).
    fn rotation_lock(&self) -> Option<(&'static str, &'static str)> {
        if self.player.is_some() {
            Some((
                "재생 중에는 바꿀 수 없습니다(녹화할 때의 방향으로 저장돼 있음)",
                "a recording is being played back; rotation applies to live video",
            ))
        } else if self.csv.is_some() || self.seq.is_some() || self.video.is_some() {
            Some((
                "CSV·녹화·MP4 기록 중에는 바꿀 수 없습니다",
                "rotation cannot change while CSV, REC or MP4 runs",
            ))
        } else {
            None
        }
    }

    /// Turn the live image. The current frame and every Spot, Box and Line move with it, so
    /// measurements stay on the same scene points.
    pub(super) fn set_rotation(&mut self, new: Rotation) -> Result<(), &'static str> {
        if let Some((_, why)) = self.rotation_lock() {
            return Err(why);
        }
        let step = new.since(self.settings.rotation);
        if let Some(f) = self.frame.take() {
            let size = (f.width, f.height);
            for s in &mut self.settings.spots.items {
                (s.x, s.y) = step.point((s.x, s.y), size);
            }
            for s in &mut self.settings.shapes.items {
                s.a = step.point(s.a, size);
                s.b = step.point(s.b, size);
            }
            let f = step.apply(f);
            self.stats = Some(f.stats());
            self.frame = Some(f);
        }
        self.drag = None;
        self.settings.rotation = new;
        self.mark_dirty();
        self.render();
        Ok(())
    }

    /// Shutter mode and FFC of the shutterless Lepton.
    fn lepton_controls(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        let mut mode = self.settings.shutter_mode;
        egui::ComboBox::from_label("셔터 모드")
            .selected_text(mode.label())
            .show_ui(ui, |ui| {
                for m in [
                    ShutterMode::Manual,
                    ShutterMode::Auto,
                    ShutterMode::External,
                ] {
                    ui.selectable_value(&mut mode, m, m.label());
                }
            });
        if mode != self.settings.shutter_mode {
            self.settings.shutter_mode = mode;
            self.camera.send(Command::SetShutterMode(mode));
            self.mark_dirty();
        }
        if mode == ShutterMode::Auto {
            ui.colored_label(
                egui::Color32::from_rgb(230, 120, 0),
                "셔터가 없는 모듈에서 AUTO는 장면이 잔상으로 남습니다.",
            );
        }
        if let (Some(m), true) = (status.shutter_mode, status.state == ConnState::Streaming) {
            if m != mode {
                ui.label(format!("카메라 현재값: {} (재적용 대기)", m.label()));
            }
        }
        if let Some(s) = status.since_ffc_s {
            ui.label(format!("마지막 FFC 이후 {}", fmt_duration(s)));
        }
        let can_ffc = status.state == ConnState::Streaming;
        if ui
            .add_enabled(can_ffc, egui::Button::new("FFC 실행…"))
            .on_hover_text("렌즈를 완전히 가린 상태에서 실행하세요")
            .clicked()
        {
            self.confirm_ffc = true;
        }
    }

    /// Shutter of the Thermal Master P3. Its gain (range) is left to the camera: switching it
    /// disturbs the stream and the low-gain readings are not understood (DEVELOPMENT.md, stage 9).
    fn p3_controls(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        ui.label("측정 범위는 카메라 기본값(고감도, 약 −20~150 °C)을 씁니다.");
        // The P3 has a real shutter: no need to cover the lens (unlike the Lepton, rule 4).
        let streaming = status.state == ConnState::Streaming;
        if ui
            .add_enabled(streaming, egui::Button::new("셔터 보정"))
            .on_hover_text("카메라가 셔터를 닫고 보정합니다. 약 90초마다 자동으로도 합니다.")
            .clicked()
        {
            self.camera.send(Command::RunFfc);
            self.notify("P3 셔터 보정을 실행했습니다.");
        }
    }

    fn ffc_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_ffc {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("ffc_confirm")).show(ctx, |ui| {
            ui.heading("FFC 실행");
            ui.label("이 Lepton에는 셔터가 없습니다.");
            ui.label("렌즈를 균일한 물체(손바닥, 두꺼운 종이 등)로 완전히 가렸습니까?");
            ui.label("가리지 않고 실행하면 지금 장면이 잔상으로 남습니다.");
            ui.add_space(8.0);
            let mut result = None;
            ui.horizontal(|ui| {
                if ui.button("가렸음 - FFC 실행").clicked() {
                    result = Some(true);
                }
                if ui.button("취소").clicked() {
                    result = Some(false);
                }
            });
            result
        });
        let choice = modal.inner.or(modal.should_close().then_some(false));
        if let Some(run) = choice {
            self.confirm_ffc = false;
            if run {
                self.camera.send(Command::RunFfc);
                self.notify("FFC를 요청했습니다. 약 1초 뒤 렌즈를 여세요.");
            }
        }
    }
}

impl ThermalApp {
    /// Frame processing, recording, alarms and remote commands. Runs on every pass, and also
    /// while the window is minimized or not being drawn, when eframe skips `ui`.
    fn tick(&mut self, ctx: &egui::Context) {
        self.pass += 1;
        let status = self.camera.status();
        self.last_fpa = status.fpa_c;
        // A new camera error is a message like any other; the worker keeps the text until the
        // next successful start, which would otherwise leave it in the status line for good.
        if status.last_error != self.seen_error {
            self.seen_error = status.last_error.clone();
            if let Some(e) = &status.last_error {
                self.notify(e.clone());
            }
        }
        // Every queued frame, oldest first, so recordings and alarms see them all.
        loop {
            let new_live = self.update_frame();
            calibration::update(self, &status, new_live);
            alarms::update(self, ctx, &status, new_live);
            if new_live.is_none() {
                break;
            }
        }
        self.prune_deltas();
        self.run_timelapse(&status);
        remote::sync(self, ctx);
        remote::process(self, ctx, &status);
        webcam::sync(self, ctx);
        self.poll_ai_jobs();
        self.save_settings_if_due(false);
        if self.player.as_ref().is_some_and(|p| p.is_playing()) {
            // Playback is paced here, not in `ui`: a hidden window must keep playing too
            // (a recording being exported to MP4).
            ctx.request_repaint_after(Duration::from_millis(10));
        } else {
            // Keep status fresh even when no frames arrive (disconnected / stalled).
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

/// Fill of a toolbar button whose recording is running.
const RUNNING: egui::Color32 = egui::Color32::from_rgb(200, 45, 45);

/// Toolbar button for something that runs until stopped. Idle: a red dot and `idle`. Running:
/// a red button with `running` (what it has recorded so far).
fn run_button(
    ui: &mut egui::Ui,
    enabled: bool,
    idle: &str,
    running: Option<String>,
) -> egui::Response {
    use egui::{text::LayoutJob, Color32, FontId, TextFormat};
    let font = egui::TextStyle::Button.resolve(ui.style());
    let part = |color| TextFormat::simple(FontId::new(font.size, font.family.clone()), color);
    let mut job = LayoutJob::default();
    let button = match running {
        Some(text) => {
            job.append(&format!("■ {text}"), 0.0, part(Color32::WHITE));
            egui::Button::new(job).fill(RUNNING)
        }
        None => {
            job.append("●", 0.0, part(RUNNING));
            job.append(idle, 5.0, part(ui.visuals().text_color()));
            egui::Button::new(job)
        }
    };
    ui.add_enabled(enabled, button)
}

/// One whole-image reading in the bar above the image. Fixed width, so the values next to it
/// do not shift as digits change.
fn reading(ui: &mut egui::Ui, name: &str, value: f32, unit: &str, color: Option<egui::Color32>) {
    let size = egui::vec2(112.0, ui.spacing().interact_size.y);
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(size, layout, |ui| {
        ui.set_min_width(size.x);
        ui.spacing_mut().item_spacing.x = 5.0;
        let name = egui::RichText::new(name);
        ui.label(match color {
            Some(c) => name.color(c).strong(),
            None => name.weak(),
        });
        ui.label(egui::RichText::new(format!("{value:.2} {unit}")).strong());
    });
}

impl ThermalApp {
    fn toolbar(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        ui.horizontal_wrapped(|ui| {
            let layout = &mut self.settings.layout;
            let mut changed = ui
                .toggle_value(&mut layout.panel, "⚙ 설정")
                .on_hover_text("설정 패널 보이기·숨기기 (P)")
                .changed();
            changed |= ui
                .toggle_value(&mut layout.graphs, "📈 그래프")
                .on_hover_text("그래프 패널 보이기·숨기기 (G)")
                .changed();
            if changed {
                self.mark_dirty();
            }
            ui.separator();

            if ui
                .add_enabled(self.frame.is_some(), egui::Button::new("📷 스냅샷"))
                .on_hover_text("지금 영상을 컬러 PNG, 온도 원본(16비트 PNG), JSON으로 저장 (S)")
                .clicked()
            {
                let _ = self.take_snapshot(status);
            }
            if ui
                .add_enabled(self.frame.is_some(), egui::Button::new("📋"))
                .on_hover_text("보이는 컬러 영상을 클립보드로 복사")
                .clicked()
            {
                self.copy_image(ui.ctx());
            }
            let mut held = self.hold.is_some();
            if ui
                .add_enabled(self.can_hold(), egui::Button::selectable(held, "⏸ 정지"))
                .on_hover_text(
                    "지금 영상을 붙잡아 두고 측정합니다. 다시 누르면 실시간으로 돌아갑니다 (Space)",
                )
                .on_disabled_hover_text(if self.player.is_some() {
                    "재생 중에는 재생의 일시정지를 쓰세요"
                } else {
                    "영상이 있고 녹화·MP4·CSV 기록 중이 아닐 때 정지할 수 있습니다"
                })
                .clicked()
            {
                held = !held;
                let _ = self.set_hold(held);
            }
            if let Some(shots) = self.timelapse.as_ref().map(|t| t.shots) {
                if run_button(ui, true, "", Some(format!("간격 스냅샷 {shots}장")))
                    .on_hover_text("간격 스냅샷을 멈춥니다")
                    .clicked()
                {
                    self.toggle_timelapse(status);
                }
            }
            ui.separator();

            let running = self.seq.as_ref().map(|w| {
                format!(
                    "녹화 {:.0}초 · {:.0} MB",
                    w.elapsed_secs(),
                    w.bytes() as f64 / 1e6
                )
            });
            if run_button(ui, self.can_record(), "녹화", running)
                .on_hover_text(
                    "모든 프레임의 온도 원본을 .tvseq 파일로 무손실 녹화. 나중에 다시 열어 \
                     측정할 수 있습니다 (R)",
                )
                .on_disabled_hover_text(
                    "실시간 영상이 있을 때만 녹화할 수 있습니다(재생·화면 정지 중에는 안 됨)",
                )
                .clicked()
            {
                self.toggle_seq(status);
            }
            let running = self
                .video
                .as_ref()
                .map(|w| format!("MP4 {:.0}초", w.elapsed_secs()));
            if run_button(ui, self.can_video(), "MP4", running)
                .on_hover_text(
                    "보이는 영상(팔레트, 측정 표시, 색 막대)을 MP4 동영상으로 녹화. 재생 중이면 \
                     재생 화면을 녹화 (V)",
                )
                .clicked()
            {
                self.toggle_video();
            }
            let running = self.csv.as_ref().map(|log| format!("CSV {}행", log.rows()));
            if run_button(ui, self.can_csv(), "CSV 기록", running)
                .on_hover_text("프레임마다 최고·최저·평균·중앙·측정값을 CSV(엑셀) 파일로 기록 (C)")
                .clicked()
            {
                self.toggle_csv();
            }
            ui.separator();

            if ui
                .button("📂 열기…")
                .on_hover_text(
                    "녹화 파일(.tvseq)을 재생하거나 저장한 스냅샷을 다시 엽니다 (Ctrl+O). \
                     파일을 이 창에 끌어다 놓아도 열립니다",
                )
                .clicked()
            {
                self.show_open_dialog = true;
            }
            if ui
                .button("📁 저장 폴더")
                .on_hover_text(self.settings.snapshot_dir().display().to_string())
                .clicked()
            {
                let dir = self.settings.snapshot_dir();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            }
        });
    }

    /// There is an image and it follows the camera or a playback: something to record.
    fn has_moving_image(&self) -> bool {
        self.frame.is_some() && self.hold.is_none()
    }

    fn can_record(&self) -> bool {
        self.player.is_none() && (self.has_moving_image() || self.seq.is_some())
    }

    fn can_video(&self) -> bool {
        self.has_moving_image() || self.video.is_some()
    }

    fn can_csv(&self) -> bool {
        self.has_moving_image() || self.csv.is_some()
    }

    /// Bar above the image: the measuring tool and the whole-image readings.
    fn measure_bar(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        ui.horizontal_wrapped(|ui| {
            for (tool, text, hint) in [
                (
                    Tool::Spot,
                    "⊙ 점",
                    "클릭: 점 추가 · 끌기: 측정 옮기기 · 우클릭: 삭제 (1)",
                ),
                (
                    Tool::Box,
                    "▭ 영역",
                    "끌기: 영역 그리기 · 테두리 끌기: 크기 조절 · 우클릭: 삭제 (2)",
                ),
                (
                    Tool::Line,
                    "╱ 선",
                    "끌기: 선 그리기 · 끝점 끌기: 조절 · 우클릭: 삭제 (3)",
                ),
            ] {
                ui.selectable_value(&mut self.tool, tool, text)
                    .on_hover_text(hint);
            }
            ui.separator();
            let (Some(f), Some(st)) = (&self.frame, &self.stats) else {
                return;
            };
            let unit = f.unit();
            let center = f.value_around(f.width / 2, f.height / 2, 1);
            reading(ui, "최고", f.value(st.max), unit, Some(view::MAX_COLOR));
            reading(ui, "최저", f.value(st.min), unit, Some(view::MIN_COLOR));
            reading(ui, "평균", f.value_f(st.mean), unit, None);
            reading(ui, "중앙", center, unit, None);
            let amber = egui::Color32::from_rgb(215, 120, 0);
            let mut release = false;
            match &self.hold {
                Some(hold) => {
                    ui.colored_label(
                        amber,
                        match hold {
                            Hold::Frozen => "⏸ 화면 정지".to_owned(),
                            Hold::Snapshot(name) => format!("🖼 스냅샷 {name}"),
                        },
                    );
                    release = ui
                        .small_button("▶ 실시간으로")
                        .on_hover_text("카메라 영상으로 돌아갑니다 (Space)")
                        .clicked();
                }
                // The image stays when the camera stops: say that it is not live.
                None if self.player.is_none() && status.state != ConnState::Streaming => {
                    ui.colored_label(
                        amber,
                        format!("⚠ {} · 마지막으로 받은 영상", status.state.label()),
                    );
                }
                None => {}
            }
            if self.zoom.factor > 1.0 {
                ui.label(format!("🔍 {:.1}×", self.zoom.factor));
                if ui
                    .small_button("맞춤")
                    .on_hover_text("전체 영상이 보이게 되돌립니다 (0)")
                    .clicked()
                {
                    self.zoom = view::Zoom::default();
                }
            }
            if release {
                let _ = self.set_hold(false);
            }
        });
    }

    /// What the image area shows before the first frame: what the camera is doing and what
    /// the user can do about it.
    fn empty_view(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        let (title, hint) = match status.state {
            ConnState::Busy => (
                "다른 프로그램이 카메라를 쓰고 있습니다",
                "GetThermal, Thermal Master 프로그램 등 카메라를 쓰는 프로그램을 닫아 주세요.",
            ),
            ConnState::NeedsReplug => (
                "카메라가 응답하지 않습니다",
                "USB 케이블을 뽑았다가 다시 꽂아 주세요.",
            ),
            ConnState::Searching => (
                "카메라를 찾는 중…",
                match self.settings.camera {
                    Source::Auto => {
                        "PureThermal 또는 Thermal Master P3를 USB에 연결하세요. 연결했는데도 \
                         찾지 못하면 WinUSB 드라이버가 설치됐는지 확인하세요."
                    }
                    Source::PureThermal => {
                        "PureThermal을 USB에 연결하세요. 연결했는데도 찾지 못하면 WinUSB \
                         드라이버가 설치됐는지 확인하세요."
                    }
                    Source::P3 => {
                        "Thermal Master P3를 USB에 연결하세요. 연결했는데도 찾지 못하면 WinUSB \
                         드라이버가 설치됐는지 확인하세요."
                    }
                },
            ),
            _ => ("영상을 기다리는 중…", "카메라가 켜지는 데 몇 초 걸립니다."),
        };
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.3).max(8.0));
            ui.colored_label(
                state_color(&status.state),
                egui::RichText::new("●").size(22.0),
            );
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(hint);
            if let Some(e) = &status.last_error {
                ui.weak(e);
            }
            ui.add_space(12.0);
            if ui
                .button("📂 녹화·스냅샷 열기…")
                .on_hover_text("카메라 없이도 녹화해 둔 영상이나 스냅샷을 열어 측정할 수 있습니다")
                .clicked()
            {
                self.show_open_dialog = true;
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        let msg = match &self.message {
            Some((m, t)) if t.elapsed() < Duration::from_secs(8) => Some(m.clone()),
            // While the camera is not delivering, the reason stays visible.
            _ if status.state != ConnState::Streaming => status.last_error.clone(),
            _ => None,
        };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The camera at a glance, also when the settings panel is hidden or scrolled.
                let mut parts = vec![status.state.label().to_owned()];
                if let Some(d) = &status.device {
                    parts.push(d.product.clone());
                }
                if status.state == ConnState::Streaming {
                    if let Some(f) = self.frame.as_ref().filter(|_| self.player.is_none()) {
                        parts.push(format!("{}×{}", f.width, f.height));
                    }
                    parts.push(format!("{:.1} fps", status.fps));
                }
                let counters = format!(
                    "받은 프레임 {} · 정지 복구 {}회 · 셔터 모드 재적용 {}회",
                    status.frames, status.stalls, status.reapplied
                );
                ui.label(parts.join(" · ")).on_hover_text(counters);
                ui.colored_label(state_color(&status.state), "●");
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(msg.unwrap_or_default()).truncate());
                });
            });
        });
    }

    /// Keyboard shortcuts. Off while a text or number field has the keyboard, or a dialog is
    /// open.
    fn shortcuts(&mut self, ctx: &egui::Context, status: &CameraStatus) {
        use egui::{Key, Modifiers};
        if ctx.egui_wants_keyboard_input() || self.confirm_ffc || self.show_open_dialog {
            return;
        }
        let hit = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let key = |k: Key| hit(Modifiers::NONE, k);

        if hit(Modifiers::COMMAND, Key::O) {
            self.show_open_dialog = true;
        }
        if key(Key::S) && self.frame.is_some() {
            let _ = self.take_snapshot(status);
        }
        if key(Key::R) && self.can_record() {
            self.toggle_seq(status);
        }
        if key(Key::V) && self.can_video() {
            self.toggle_video();
        }
        if key(Key::C) && self.can_csv() {
            self.toggle_csv();
        }
        for (k, tool) in [
            (Key::Num1, Tool::Spot),
            (Key::Num2, Tool::Box),
            (Key::Num3, Tool::Line),
        ] {
            if key(k) {
                self.tool = tool;
            }
        }
        if key(Key::Num0) {
            self.zoom = view::Zoom::default();
        }
        if key(Key::I) {
            self.settings.isotherm.enabled = !self.settings.isotherm.enabled;
            self.mark_dirty();
            self.render();
        }
        // Space pauses a playback (below); on live video it holds the image.
        if self.player.is_none() && self.can_hold() && key(Key::Space) {
            let _ = self.set_hold(self.hold.is_none());
        }
        let layout = &mut self.settings.layout;
        let mut changed = false;
        if key(Key::P) {
            layout.panel = !layout.panel;
            changed = true;
        }
        if key(Key::G) {
            layout.graphs = !layout.graphs;
            changed = true;
        }
        if changed {
            self.mark_dirty();
        }

        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if key(Key::F11) || (fullscreen && key(Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        } else if key(Key::Escape) {
            // Give up the shape being drawn or moved.
            self.drag = None;
        }

        let mut failed = None;
        if let Some(p) = &mut self.player {
            let r = if key(Key::Space) {
                p.toggle_play()
            } else if key(Key::ArrowLeft) {
                p.step(false)
            } else if key(Key::ArrowRight) {
                p.step(true)
            } else {
                Ok(())
            };
            failed = r.err();
        }
        if let Some(e) = failed {
            self.close_player();
            self.notify(format!("재생 오류: {e}"));
        }
    }

    /// A recording dropped on the window opens like one picked in the dialog.
    fn open_dropped_file(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let Some(path) = dropped.first().map(|f| f.path().to_path_buf()) else {
            return;
        };
        let is = |ext: &str| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        };
        if is("tvseq") {
            self.show_open_dialog = false;
            self.open_sequence(&path);
        } else if is("png") || is("json") || is("csv") {
            // Any file of a snapshot stands for the snapshot.
            self.show_open_dialog = false;
            self.open_snapshot(&path);
        } else {
            self.notify("녹화 파일(.tvseq)과 스냅샷 파일만 열 수 있습니다");
        }
    }
}

impl eframe::App for ThermalApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let status = self.camera.status();

        self.shortcuts(&ctx, &status);
        self.open_dropped_file(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &status));
        alarms::banner(ui, self);

        let mut close_player = false;
        let mut playback_error = None;
        if let Some(p) = &mut self.player {
            egui::Panel::bottom("playback").show(ui, |ui| match p.ui(ui) {
                Ok(playback::PlayerAction::Close) => close_player = true,
                Ok(playback::PlayerAction::None) => {}
                Err(e) => {
                    log::warn!("playback error: {e}");
                    close_player = true;
                    playback_error = Some(format!("재생 오류: {e}"));
                }
            });
        }
        if close_player {
            self.close_player();
        }
        if let Some(e) = playback_error {
            self.notify(e);
        }

        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui, &status));

        if self.settings.layout.panel {
            egui::Panel::left("controls")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| self.controls(ui, &status));
        }

        if self.settings.layout.graphs {
            egui::Panel::bottom("graphs")
                .resizable(true)
                .default_size(240.0)
                .show(ui, |ui| graphs::show(ui, self));
        }

        // After the panels: their controls may have recolored the image.
        self.upload_texture(&ctx);
        webcam::upload_texture(self, &ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            if self.frame.is_none() {
                self.empty_view(ui, &status);
                return;
            }
            self.measure_bar(ui, &status);
            ui.add_space(2.0);
            if view::image_view(ui, self) {
                self.mark_dirty();
            }
        });

        self.ffc_dialog(&ctx);
        self.open_dialog(&ctx);
    }

    fn on_exit(&mut self) {
        self.remote.stop();
        self.webcam.stop();
        self.save_settings_if_due(true);
        if let Some(log) = self.csv.take() {
            let _ = log.finish();
        }
        if let Some(w) = self.seq.take() {
            let _ = w.finish();
        }
        if let Some(w) = self.video.take() {
            let _ = w.finish();
        }
    }
}

fn state_color(state: &ConnState) -> egui::Color32 {
    match state {
        ConnState::Streaming => egui::Color32::from_rgb(60, 180, 75),
        ConnState::Busy | ConnState::NeedsReplug => egui::Color32::from_rgb(220, 60, 60),
        _ => egui::Color32::from_rgb(230, 160, 0),
    }
}

/// Header bar of a foldable panel section: a triangle, the title and, on the right, a short
/// summary of the section's state. Returns true when clicked.
fn section_header(ui: &mut egui::Ui, title: &str, summary: &str, open: bool) -> bool {
    use egui::{pos2, vec2, Align, Label, RichText, Sense, Shape, Stroke};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let widgets = &ui.visuals().widgets;
        let look = if resp.hovered() {
            widgets.hovered
        } else {
            widgets.inactive
        };
        ui.painter().rect_filled(rect, 4.0, look.weak_bg_fill);
        let c = pos2(rect.min.x + 13.0, rect.center().y);
        let r = 4.0;
        let triangle = if open {
            vec![
                c + vec2(-r, -r * 0.6),
                c + vec2(r, -r * 0.6),
                c + vec2(0.0, r * 0.8),
            ]
        } else {
            vec![
                c + vec2(-r * 0.6, -r),
                c + vec2(r * 0.8, 0.0),
                c + vec2(-r * 0.6, r),
            ]
        };
        ui.painter().add(Shape::convex_polygon(
            triangle,
            look.fg_stroke.color,
            Stroke::NONE,
        ));
        // Labels that do not take the click from the bar under them.
        let inner = rect.shrink2(vec2(8.0, 0.0));
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        row.add_space(16.0);
        row.add(Label::new(RichText::new(title).strong().size(14.5)).selectable(false));
        if !summary.is_empty() {
            row.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    Label::new(RichText::new(summary).weak().size(12.0))
                        .selectable(false)
                        .truncate(),
                );
            });
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// "모든 프레임", "5초", "10분": a CSV logging interval.
fn interval_label(secs: f32) -> String {
    match secs as u32 {
        0 => "모든 프레임".into(),
        s if s % 60 == 0 => format!("{}분", s / 60),
        s => format!("{s}초"),
    }
}

fn filter_label(strength: u8) -> String {
    match strength {
        0 | 1 => "끔".into(),
        n => format!("{n}프레임 평균"),
    }
}

fn fmt_duration(s: f32) -> String {
    let s = s as u64;
    if s < 60 {
        format!("{s}초")
    } else if s < 3600 {
        format!("{}분 {}초", s / 60, s % 60)
    } else {
        format!("{}시간 {}분", s / 3600, (s % 3600) / 60)
    }
}

/// egui's default fonts have no Hangul; add Malgun Gothic as a fallback when it exists.
fn install_korean_font(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let path = std::path::Path::new(&windir)
        .join("Fonts")
        .join("malgun.ttf");
    match std::fs::read(&path) {
        Ok(bytes) => ctx.add_font(FontInsert::new(
            "malgun",
            egui::FontData::from_owned(bytes),
            vec![
                InsertFontFamily {
                    family: egui::FontFamily::Proportional,
                    priority: FontPriority::Lowest,
                },
                InsertFontFamily {
                    family: egui::FontFamily::Monospace,
                    priority: FontPriority::Lowest,
                },
            ],
        )),
        Err(e) => log::warn!("Korean font not loaded ({}): {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hidden window gets few passes: one pass must process every frame that queued up.
    #[test]
    fn one_pass_takes_every_queued_frame() {
        let dir = std::env::temp_dir().join(format!("thermalview_queue_{}", std::process::id()));
        let settings = Settings {
            snapshot_dir: Some(dir.clone()),
            ..Default::default()
        };
        let mut app = ThermalApp::with_camera(settings, Camera::disconnected());
        let ctx = egui::Context::default();
        let frame = |seq| {
            let mut f = ThermalFrame::new(160, 120, vec![30000; 160 * 120], Some(0.01));
            f.sequence = seq;
            f
        };
        app.camera.inject(frame(1));
        app.tick(&ctx);
        app.start_csv().unwrap();
        app.start_seq(&CameraStatus::default()).unwrap();
        for seq in 2..=6 {
            app.camera.inject(frame(seq));
        }
        app.tick(&ctx);
        assert_eq!(app.frame.as_ref().map(|f| f.sequence), Some(6));
        assert!(app.texture_dirty);
        assert_eq!(app.stop_csv().unwrap().1, 5);
        assert_eq!(app.stop_seq().unwrap().1, 5);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn frame_of(w: usize, h: usize, seq: u32) -> ThermalFrame {
        let mut f = ThermalFrame::new(w, h, vec![30000; w * h], Some(0.01));
        f.sequence = seq;
        f
    }

    /// Another camera taking over mid-recording (another image size) ends CSV and recording
    /// with a message, instead of writing nothing while the timers run.
    #[test]
    fn a_frame_of_another_size_ends_the_recordings() {
        let dir = std::env::temp_dir().join(format!("thermalview_resize_{}", std::process::id()));
        let settings = Settings {
            snapshot_dir: Some(dir.clone()),
            ..Default::default()
        };
        let mut app = ThermalApp::with_camera(settings, Camera::disconnected());
        let ctx = egui::Context::default();
        app.camera.inject(frame_of(160, 120, 1));
        app.tick(&ctx);
        app.start_csv().unwrap();
        app.start_seq(&CameraStatus::default()).unwrap();
        app.camera.inject(frame_of(160, 120, 2));
        app.tick(&ctx);
        assert!(app.csv.is_some() && app.seq.is_some());

        app.camera.inject(frame_of(256, 192, 3));
        app.tick(&ctx);
        assert!(app.csv.is_none() && app.seq.is_none());
        let msg = &app.message.as_ref().unwrap().0;
        assert!(msg.contains("영상 크기가 바뀌어"), "{msg}");
        assert!(
            msg.contains("녹화 종료: 1프레임"),
            "both outcomes are shown: {msg}"
        );
        assert_eq!(app.frame.as_ref().map(|f| f.width), Some(256));

        // Leaving a playback: its last frame is not shown as live video.
        app.start_seq(&CameraStatus::default()).unwrap();
        app.camera.inject(frame_of(256, 192, 4));
        app.tick(&ctx);
        let (seq, _) = app.stop_seq().unwrap();
        app.open_sequence(&seq);
        app.tick(&ctx);
        assert!(app.player.is_some() && app.frame.is_some());
        app.close_player();
        assert!(app.frame.is_none() && app.stats.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Draws the toolbar, the settings panel with every section open and folded, the bar above
    /// the image, the empty view and the status line, with and without a frame. No window or
    /// GPU: this catches panics and id clashes in the layout code.
    #[test]
    fn the_whole_window_lays_out() {
        let ctx = egui::Context::default();
        let mut first = ctx.run_ui(egui::RawInput::default(), |_| {});
        first.textures_delta.clear();
        let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        let status = CameraStatus::default();
        let pass = |app: &mut ThermalApp| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 420.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                egui::Panel::top("toolbar").show(ui, |ui| app.toolbar(ui, &status));
                egui::Panel::bottom("status").show(ui, |ui| app.status_bar(ui, &status));
                egui::Panel::left("controls").show(ui, |ui| app.controls(ui, &status));
                egui::Panel::bottom("graphs").show(ui, |ui| graphs::show(ui, app));
                egui::CentralPanel::default().show(ui, |ui| {
                    if app.frame.is_none() {
                        app.empty_view(ui, &status);
                    } else {
                        app.measure_bar(ui, &status);
                        view::image_view(ui, app);
                    }
                });
                app.open_dialog(ui.ctx());
            });
            out.textures_delta.clear();
        };
        pass(&mut app);
        let all = |open: bool| Layout {
            panel: true,
            graphs: true,
            camera: open,
            device: open,
            display: open,
            webcam: open,
            measure: open,
            calibration: open,
            alarms: open,
            storage: open,
            remote: open,
        };
        for (open, source) in [
            (true, Source::Auto),
            (true, Source::P3),
            (true, Source::PureThermal),
            (false, Source::Auto),
        ] {
            app.settings.layout = all(open);
            app.settings.camera = source;
            pass(&mut app);
        }
        // With a frame, measurements, a manual range and the open dialog.
        app.settings.layout = all(true);
        app.camera.inject(frame_of(160, 120, 1));
        app.tick(&ctx);
        app.settings.spots.add(10, 10);
        app.settings.shapes.add(ShapeKind::Box, (5, 5), (300, 300));
        app.settings.range = RangeMode::Manual { lo: 20.0, hi: 30.0 };
        app.confirm_clear = true;
        app.show_open_dialog = true;
        app.upload_texture(&ctx);
        pass(&mut app);
        pass(&mut app);
        assert!(
            app.open_list.is_some(),
            "the folder is listed once, then kept"
        );
        app.show_open_dialog = false;
        pass(&mut app);
        assert!(app.open_list.is_none());

        // A snapshot held on screen, zoomed in, with an isotherm, a difference, an interval
        // series running and the snapshot list open.
        app.settings.isotherm.enabled = true;
        for band in Band::ALL {
            app.settings.isotherm.band = band;
            app.render();
            pass(&mut app);
        }
        app.settings.deltas.add(Delta {
            a: Reading::Frame(Metric::Max),
            b: Reading::Center,
        });
        app.settings.range = RangeMode::Robust;
        app.hold = Some(Hold::Snapshot("thermal_20261128_100000_000".into()));
        app.zoom = view::Zoom::centered(3.0, (80.0, 60.0), 160.0, 120.0);
        app.timelapse = Some(Timelapse {
            next: Instant::now() + Duration::from_secs(60),
            shots: 3,
            last: None,
        });
        app.open_tab = OpenTab::Snapshots;
        app.show_open_dialog = true;
        pass(&mut app);
        assert!(app.open_snaps.is_some() && app.open_list.is_none());
        app.hold = Some(Hold::Frozen);
        app.show_open_dialog = false;
        pass(&mut app);
        assert_eq!(app.zoom.factor, 3.0, "drawing does not change the zoom");
    }

    #[test]
    fn keyboard_shortcuts() {
        let ctx = egui::Context::default();
        let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        let status = CameraStatus::default();
        let press = |app: &mut ThermalApp, key: egui::Key| {
            let raw = egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| app.shortcuts(ui.ctx(), &status));
            out.textures_delta.clear();
        };
        assert!(app.settings.layout.graphs && app.settings.layout.panel);
        press(&mut app, egui::Key::G);
        press(&mut app, egui::Key::P);
        assert!(!app.settings.layout.graphs && !app.settings.layout.panel);
        assert!(app.settings_dirty.is_some(), "the layout is saved");
        press(&mut app, egui::Key::Num2);
        assert_eq!(app.tool, Tool::Box);
        press(&mut app, egui::Key::Num3);
        assert_eq!(app.tool, Tool::Line);
        // Nothing to record without a frame; no panic, no file.
        press(&mut app, egui::Key::R);
        press(&mut app, egui::Key::S);
        assert!(app.seq.is_none());
        // While a dialog is open the keys belong to it.
        app.show_open_dialog = true;
        press(&mut app, egui::Key::Num1);
        assert_eq!(app.tool, Tool::Line);
    }

    /// A P3-sized frame with upscaling on: the view, the MP4 scene and the snapshot get the 4×
    /// image; turning it off removes it everywhere.
    #[test]
    fn upscaling_reaches_view_video_and_snapshot() {
        let dir = std::env::temp_dir().join(format!("thermalview_up_{}", std::process::id()));
        let mut settings = Settings {
            snapshot_dir: Some(dir.clone()),
            ..Default::default()
        };
        settings.upscale.enabled = true;
        let mut app = ThermalApp::with_camera(settings, Camera::disconnected());
        let ctx = egui::Context::default();
        let (w, h) = (256, 192);
        let mut raw = vec![19_000u16; w * h];
        raw[100 * w + 50] = 21_000;
        app.camera
            .inject(ThermalFrame::new(w, h, raw, Some(1.0 / 64.0)));
        app.tick(&ctx);
        assert_eq!(app.rgba_up.len(), w * h * 4 * 16);
        let scene = app.video_scene(0.0).unwrap();
        assert_eq!(scene.upscaled.as_ref().map(Vec::len), Some(w * h * 4 * 16));

        app.take_snapshot(&CameraStatus::default()).unwrap();
        let x4: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with("_x4.png"))
            .collect();
        assert_eq!(x4.len(), 1);
        let img = image::open(&x4[0]).unwrap();
        assert_eq!((img.width(), img.height()), (1024, 768));

        // AI copy through a stand-in Upscayl that copies its input.
        app.upscayl = Some(crate::upscayl::tests::fake(&dir.join("upscayl"), true));
        app.settings.upscale.ai = true;
        let png = app.take_snapshot(&CameraStatus::default()).unwrap();
        let ai = crate::upscayl::output_for(&png);
        let t = Instant::now();
        while app.ai_jobs.len() == 1 && t.elapsed() < Duration::from_secs(10) {
            app.poll_ai_jobs();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(app.ai_jobs.is_empty());
        // The stand-in copies its input, the sharpened sensor image; the temporary file is gone.
        let copied = image::open(&ai).unwrap();
        assert_eq!((copied.width(), copied.height()), (256, 192));
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .to_string_lossy()
                    .ends_with("_in.png")
            })
            .count();
        assert_eq!(leftovers, 0);
        let json = std::fs::read_to_string(png.with_extension("json")).unwrap();
        assert!(json.contains("\"ai_model\": \"upscayl-lite-4x\""), "{json}");

        app.settings.upscale.enabled = false;
        app.render();
        assert!(app.rgba_up.is_empty());
        assert!(app.video_scene(0.0).unwrap().upscaled.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("thermalview_{name}_{}", std::process::id()))
    }

    fn app_in(dir: &std::path::Path) -> (ThermalApp, egui::Context) {
        let settings = Settings {
            snapshot_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let app = ThermalApp::with_camera(settings, Camera::disconnected());
        (app, egui::Context::default())
    }

    fn press(ctx: &egui::Context, app: &mut ThermalApp, key: egui::Key) {
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let status = CameraStatus::default();
        let mut out = ctx.run_ui(raw, |ui| app.shortcuts(ui.ctx(), &status));
        out.textures_delta.clear();
    }

    #[test]
    fn holding_the_image_drops_frames_until_released() {
        let dir = temp("hold");
        let (mut app, ctx) = app_in(&dir);
        let status = CameraStatus::default();
        assert!(app.set_hold(true).is_err(), "nothing to hold yet");
        app.camera.inject(frame_of(160, 120, 1));
        app.tick(&ctx);
        app.start_csv().unwrap();
        assert!(app.set_hold(true).unwrap_err().contains("CSV"));
        app.stop_csv().unwrap();

        app.set_hold(true).unwrap();
        assert!(!app.can_record() && !app.can_video() && !app.can_csv());
        for seq in 2..=4 {
            app.camera.inject(frame_of(160, 120, seq));
        }
        app.tick(&ctx);
        assert_eq!(app.frame.as_ref().map(|f| f.sequence), Some(1));
        // Nothing can be recorded from a still image…
        assert!(app.start_csv().is_err());
        assert!(app.start_seq(&status).is_err());
        assert!(app.start_video().is_err());
        // …but it can be measured and saved.
        app.settings.spots.add(10, 10);
        assert!(app.take_snapshot(&status).is_ok());

        // Space releases it, and holds again.
        press(&ctx, &mut app, egui::Key::Space);
        assert!(app.hold.is_none());
        app.camera.inject(frame_of(160, 120, 5));
        app.tick(&ctx);
        assert_eq!(app.frame.as_ref().map(|f| f.sequence), Some(5));
        press(&ctx, &mut app, egui::Key::Space);
        assert_eq!(app.hold, Some(Hold::Frozen));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_saved_snapshot_opens_as_a_held_image() {
        let dir = temp("reopen");
        let (mut app, ctx) = app_in(&dir);
        let status = CameraStatus::default();
        let mut raw = vec![30000u16; 160 * 120];
        raw[50 * 160 + 20] = 31234;
        app.camera
            .inject(ThermalFrame::new(160, 120, raw.clone(), Some(0.01)));
        app.tick(&ctx);
        let png = app.take_snapshot(&status).unwrap();

        // Later: another camera (another image size) is streaming, a CSV runs, the view is
        // zoomed in.
        app.camera.inject(frame_of(256, 192, 2));
        app.tick(&ctx);
        app.start_csv().unwrap();
        app.zoom = view::Zoom::centered(4.0, (10.0, 10.0), 256.0, 192.0);
        app.open_snapshot(&png);
        let name = png.file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(app.hold, Some(Hold::Snapshot(name)));
        assert!(app.csv.is_none(), "a still image ends the log");
        assert_eq!(app.zoom, view::Zoom::default());
        let f = app.frame.as_ref().unwrap();
        assert_eq!((f.width, f.height, &f.raw), (160, 120, &raw));
        assert_eq!(app.stats.unwrap().max_pos, (20, 50));
        assert_eq!(app.rgba.len(), 160 * 120 * 4, "recolored");

        // Camera frames do not replace it…
        app.camera.inject(frame_of(256, 192, 3));
        app.tick(&ctx);
        assert_eq!(app.frame.as_ref().map(|f| f.width), Some(160));
        // …until the user goes back to live video, which starts from an empty view.
        app.set_hold(false).unwrap();
        assert!(app.frame.is_none());
        app.camera.inject(frame_of(256, 192, 4));
        app.tick(&ctx);
        assert_eq!(app.frame.as_ref().map(|f| f.sequence), Some(4));

        // Not a snapshot: a message, and the view stays as it is.
        app.open_snapshot(&dir.join("nothing.png"));
        assert!(app.hold.is_none());
        let msg = &app.message.as_ref().unwrap().0;
        assert!(msg.starts_with("열 수 없습니다"), "{msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn isotherm_paints_the_band_in_every_image() {
        use crate::thermal::isotherm::Isotherm;
        let (mut app, ctx) = app_in(&temp("iso"));
        // Left half 26.85 °C, right half 46.85 °C.
        let (w, h) = (160, 120);
        let raw: Vec<u16> = (0..w * h)
            .map(|i| if i % w < w / 2 { 30000 } else { 32000 })
            .collect();
        let color = [1, 2, 3];
        app.settings.upscale.enabled = true;
        app.settings.isotherm = Isotherm {
            enabled: true,
            band: Band::Above,
            lo: 40.0,
            hi: 60.0,
            color,
        };
        app.camera.inject(ThermalFrame::new(w, h, raw, Some(0.01)));
        app.tick(&ctx);
        let px = |x: usize, y: usize| &app.rgba[(y * w + x) * 4..][..3];
        assert_eq!(px(120, 60), color);
        assert_ne!(px(40, 60), color);
        assert_eq!(app.iso_share, Some(0.5));
        // The upscaled image, well inside each half.
        let up = |x: usize, y: usize| &app.rgba_up[(y * w * 4 + x) * 4..][..3];
        assert_eq!(up(480, 240), color);
        assert_ne!(up(160, 240), color);
        // The MP4 marks the band beside its color bar.
        let band = app.video_scene(0.0).unwrap().band.unwrap();
        assert_eq!((band.lo, band.color), (40.0, color));
        assert!(band.hi.is_infinite());

        press(&ctx, &mut app, egui::Key::I);
        assert!(!app.settings.isotherm.enabled && app.iso_share.is_none());
        assert_ne!(&app.rgba[(60 * w + 120) * 4..][..3], color);
    }

    #[test]
    fn robust_range_ignores_a_hot_pixel() {
        let (mut app, ctx) = app_in(&temp("robust"));
        let mut raw = vec![30000u16; 160 * 120];
        raw[5] = 40000; // 126.85 °C
        app.camera
            .inject(ThermalFrame::new(160, 120, raw, Some(0.01)));
        app.tick(&ctx);
        assert!(app.display_range.1 > 120.0);
        app.settings.range = RangeMode::Robust;
        app.auto_range.reset();
        app.render();
        let (lo, hi) = app.display_range;
        assert!(lo < 26.85 && hi > 26.85, "{lo} {hi}");
        assert!(hi - lo <= AutoRange::MIN_SPAN + 1e-3, "{lo} {hi}");
    }

    #[test]
    fn csv_rows_follow_the_interval() {
        let dir = temp("interval");
        let (mut app, ctx) = app_in(&dir);
        app.camera.inject(frame_of(160, 120, 1));
        app.tick(&ctx);
        app.settings.csv_interval_s = 1.0;
        app.start_csv().unwrap();
        // Frame times in seconds; the last one is a recording played from an earlier point.
        for t in [0.0, 0.5, 1.0, 1.04, 1.96, 2.0, 1.5] {
            app.record(t);
        }
        let rows = app.stop_csv().unwrap().1;
        assert_eq!(rows, 4, "0.0, 1.0, 2.0 and the jump back");

        app.settings.csv_interval_s = 0.0;
        app.start_csv().unwrap();
        for t in [0.0, 0.04, 0.08] {
            app.record(t);
        }
        assert_eq!(app.stop_csv().unwrap().1, 3, "every frame");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn interval_snapshots_need_a_new_frame() {
        let dir = temp("lapse");
        let (mut app, ctx) = app_in(&dir);
        let status = CameraStatus::default();
        let shots = |app: &ThermalApp| app.timelapse.as_ref().map(|t| t.shots);
        app.camera.inject(frame_of(160, 120, 1));
        app.tick(&ctx);
        app.toggle_timelapse(&status);
        assert_eq!(shots(&app), Some(1), "the first one at once");
        // Due again, but the camera has sent nothing new.
        std::thread::sleep(Duration::from_millis(5));
        app.timelapse.as_mut().unwrap().next = Instant::now();
        app.tick(&ctx);
        assert_eq!(shots(&app), Some(1));
        app.camera.inject(frame_of(160, 120, 2));
        app.tick(&ctx);
        assert_eq!(shots(&app), Some(2));
        // A new frame, but the interval has not passed.
        app.camera.inject(frame_of(160, 120, 3));
        app.tick(&ctx);
        assert_eq!(shots(&app), Some(2));
        app.toggle_timelapse(&status);
        assert!(app.timelapse.is_none());
        let msg = &app.message.as_ref().unwrap().0;
        assert!(msg.contains("2장"), "{msg}");
        assert_eq!(snapshot::list(&dir).len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn noise_filter_smooths_the_view_and_leaves_the_recording_raw() {
        let dir = temp("filter");
        let (mut app, ctx) = app_in(&dir);
        app.settings.noise_filter = 4;
        let flat = |v: u16, seq| {
            let mut f = ThermalFrame::new(160, 120, vec![v; 160 * 120], Some(0.01));
            f.sequence = seq;
            f
        };
        app.camera.inject(flat(30000, 1));
        app.tick(&ctx);
        app.start_seq(&CameraStatus::default()).unwrap();
        app.camera.inject(flat(30040, 2));
        app.camera.inject(flat(30000, 3));
        app.tick(&ctx);
        // Mean of 30000, 30040 and 30000.
        assert_eq!(app.frame.as_ref().unwrap().raw[0], 30013);
        let (path, n) = app.stop_seq().unwrap();
        assert_eq!(n, 2);
        let mut reader = sequence::SeqReader::open(&path).unwrap();
        let first = reader.frame(0).unwrap().1;
        assert_eq!(first.raw[0], 30040, "recorded as the camera sent it");

        // Off: the next frame is shown as it is.
        app.settings.noise_filter = 1;
        app.camera.inject(flat(30040, 4));
        app.tick(&ctx);
        assert_eq!(app.frame.as_ref().unwrap().raw[0], 30040);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn differences_are_saved_and_go_with_their_measurement() {
        let dir = temp("delta");
        let (mut app, ctx) = app_in(&dir);
        let mut raw = vec![30000u16; 160 * 120];
        raw[0] = 30500;
        app.camera
            .inject(ThermalFrame::new(160, 120, raw, Some(0.01)));
        app.tick(&ctx);
        let s1 = app.settings.spots.add(100, 100).unwrap();
        let d = Delta {
            a: Reading::Frame(Metric::Max),
            b: Reading::Spot(s1),
        };
        assert!(app.settings.deltas.add(d));
        app.tick(&ctx);
        assert_eq!(app.settings.deltas.items, vec![d]);
        let png = app.take_snapshot(&CameraStatus::default()).unwrap();
        let json = std::fs::read_to_string(png.with_extension("json")).unwrap();
        assert!(json.contains("화면 최고 − S1"), "{json}");

        app.settings.spots.remove(s1);
        app.tick(&ctx);
        assert!(app.settings.deltas.items.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
