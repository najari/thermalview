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
use crate::settings::Settings;
use crate::storage::csv_log::{self, CsvLog};
use crate::storage::sequence::{self, SeqHeader, SeqWriter};
use crate::storage::snapshot::{self, ShapeReading, SnapshotMeta, SpotReading, WebcamInfo};
use crate::thermal::frame::{FrameStats, ThermalFrame};
use crate::thermal::history::{History, Sample};
use crate::thermal::palette::{self, Palette};
use crate::thermal::range::{AutoRange, RangeMode};
use crate::thermal::rotate::Rotation;
use crate::thermal::shapes::{Grab, Shape, ShapeKind};
use crate::thermal::upscale::{self, Sharpen, Upscaler};
use crate::video::Mp4Writer;

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
    lut: (Palette, [[u8; 3]; 256]),
    auto_range: AutoRange,
    /// Range used for the current image, in display units.
    display_range: (f32, f32),

    confirm_ffc: bool,
    message: Option<(String, Instant)>,

    started: Instant,
    history: History,
    graphs: graphs::GraphState,
    csv: Option<CsvLog>,
    last_fpa: Option<f32>,

    tool: Tool,
    drag: Option<DragOp>,

    /// Radiometric sequence recording (live only).
    seq: Option<SeqWriter>,
    /// When set, the image comes from a recorded sequence instead of the camera.
    player: Option<playback::Player>,
    show_open_dialog: bool,
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
        let settings = Settings::load();
        let ctx = cc.egui_ctx.clone();
        let config = crate::camera::Config {
            source: settings.camera,
            shutter_mode: settings.shutter_mode,
        };
        let camera = Camera::start(config, move || ctx.request_repaint());
        Self::with_camera(settings, camera)
    }

    fn with_camera(settings: Settings, camera: Camera) -> Self {
        let lut = (settings.palette, settings.palette.lut());
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
            confirm_ffc: false,
            message: None,
            started: Instant::now(),
            history: History::default(),
            graphs: graphs::GraphState::default(),
            csv: None,
            last_fpa: None,
            tool: Tool::Spot,
            drag: None,
            seq: None,
            player: None,
            show_open_dialog: false,
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
                if let Some(w) = self.seq.take() {
                    let _ = w.finish();
                }
                // One MP4 holds one source.
                let video = self.video.is_some().then(|| self.stop_video());
                self.history.clear();
                self.auto_range.reset();
                let mut msg = format!("재생: {}", p.file_name());
                if let Some(r) = video {
                    msg.push_str(match r {
                        Ok(_) => " · MP4 녹화를 끝냈습니다",
                        Err(_) => " · MP4 저장 실패",
                    });
                }
                self.notify(msg);
                self.player = Some(p);
            }
            Err(e) => self.notify(format!("열 수 없습니다: {e}")),
        }
    }

    fn close_player(&mut self) {
        if self.video.is_some() {
            let _ = self.stop_video();
        }
        self.player = None;
        self.history.clear();
        self.auto_range.reset();
        self.started = Instant::now();
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_open_dialog {
            return;
        }
        let dir = self.settings.snapshot_dir();
        let files = sequence::list(&dir);
        let mut chosen = None;
        let mut open = true;
        egui::Window::new("녹화 파일 열기")
            .open(&mut open)
            .collapsible(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(format!("폴더: {}", dir.display()));
                ui.separator();
                if files.is_empty() {
                    ui.label("녹화 파일(.tvseq)이 없습니다. 실시간 화면에서 '⏺ 녹화'로 만드세요.");
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for (path, bytes) in &files {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                            let text = format!(
                                "{}  ({:.1} MB)",
                                name.unwrap_or_default(),
                                *bytes as f64 / 1e6
                            );
                            if ui.button(text).clicked() {
                                chosen = Some(path.clone());
                            }
                        }
                    });
            });
        if let Some(p) = chosen {
            self.open_sequence(&p);
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
        self.history.push(sample, &spots);
        if let Some(log) = &mut self.csv {
            let shapes: Vec<_> = self
                .settings
                .shapes
                .items
                .iter()
                .filter_map(|s| s.stats(f).map(|st| (s.id, st)))
                .collect();
            let row = csv_log::Row {
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
        if self.frame.is_none() {
            return Err("no frame yet".into());
        }
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

    fn notify(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
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
        let (mut f, t) = if let Some(p) = &mut self.player {
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
            let f = self.settings.rotation.apply(self.camera.take_frame()?);
            if let Some(w) = &mut self.seq {
                w.push(&f);
            }
            // Time of arrival, not of processing: queued frames are handled in a burst.
            let age = SystemTime::now()
                .duration_since(f.captured)
                .unwrap_or_default();
            let t = self.started.elapsed().saturating_sub(age);
            (f, t.as_secs_f64())
        };
        calibration::attach(self, &mut f);
        self.stats = Some(f.stats());
        self.frame = Some(f);
        self.render();
        self.push_video(t);
        self.record(t);
        is_live.then_some(t)
    }

    /// Recolor the current frame (after a new frame or a palette/range change). The texture is
    /// uploaded later by [`Self::upload_texture`], only when the window is drawn: texture
    /// updates made while it is hidden would pile up undelivered.
    fn render(&mut self) {
        let (Some(f), Some(st)) = (&self.frame, &self.stats) else {
            return;
        };
        if self.lut.0 != self.settings.palette {
            self.lut = (self.settings.palette, self.settings.palette.lut());
        }
        self.display_range = match self.settings.range {
            RangeMode::Auto => self.auto_range.update(f.value(st.min), f.value(st.max)),
            RangeMode::Manual { lo, hi } => (lo.min(hi), hi.max(lo)),
        };
        let lo = f.raw_from_value(self.display_range.0);
        let hi = f.raw_from_value(self.display_range.1);
        palette::colorize(&f.raw, lo, hi, &self.lut.1, &mut self.rgba);
        let up = self.settings.upscale;
        if up.enabled {
            let u = match &mut self.upscaler {
                Some(u) if u.sharpen() == up.sharpen => u,
                slot => slot.insert(Upscaler::new(up.sharpen)),
            };
            let field = u.upscale(&f.raw, f.width, f.height);
            palette::colorize_f32(&field, lo, hi, &self.lut.1, &mut self.rgba_up);
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
            captured_local: chrono::Local::now().to_rfc3339(),
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
        match snapshot::save(&dir, f, &self.rgba, upscaled, picture, &meta) {
            Ok(files) => {
                self.notify(format!("저장됨: {}", files.color_png.display()));
                if let Some((u, input)) = ai {
                    self.start_ai_upscale(u, input, files.color_png.clone());
                }
                Ok(files.color_png)
            }
            Err(e) => {
                self.notify(format!("스냅샷 저장 실패: {e}"));
                Err(format!("snapshot failed: {e}"))
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, status: &CameraStatus) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("카메라");
            let color = match status.state {
                ConnState::Streaming => egui::Color32::from_rgb(60, 180, 75),
                ConnState::Busy | ConnState::NeedsReplug => egui::Color32::from_rgb(220, 60, 60),
                _ => egui::Color32::from_rgb(230, 160, 0),
            };
            ui.colored_label(color, format!("● {}", status.state.label()));
            let mut source = self.settings.camera;
            egui::ComboBox::from_label("카메라 종류")
                .selected_text(source.label())
                .show_ui(ui, |ui| {
                    for s in Source::ALL {
                        ui.selectable_value(&mut source, s, s.label());
                    }
                })
                .response
                .on_hover_text("자동은 PureThermal을 먼저 찾고, 없으면 Thermal Master P3를 찾습니다");
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
            });
            if status.state == ConnState::Busy {
                ui.label("GetThermal, Thermal Master 프로그램 등 카메라를 쓰는 다른 프로그램을 닫아 주세요.");
            }
            if status.state == ConnState::Searching {
                ui.label(match source {
                    Source::Auto => "PureThermal 또는 P3가 연결돼 있고 WinUSB 드라이버가 설치됐는지 확인하세요.",
                    Source::PureThermal => "PureThermal이 연결돼 있고 WinUSB 드라이버가 설치됐는지 확인하세요.",
                    Source::P3 => "P3가 연결돼 있고 WinUSB 드라이버가 설치됐는지 확인하세요.",
                });
            }

            ui.separator();
            // The connected camera, or the one asked for before anything connects.
            let kind = status.kind.or(match source {
                Source::P3 => Some(Kind::P3),
                _ => None,
            });
            if kind == Some(Kind::P3) {
                self.p3_controls(ui, status);
            } else {
                self.lepton_controls(ui, status);
            }

            ui.separator();
            ui.heading("표시");
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

            let unit = self.frame.as_ref().map(|f| f.unit()).unwrap_or("°C");
            let mut manual = matches!(self.settings.range, RangeMode::Manual { .. });
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut manual, false, "자동 범위").changed();
                changed |= ui.radio_value(&mut manual, true, "수동 범위").changed();
            });
            match (&mut self.settings.range, manual) {
                (RangeMode::Auto, true) => {
                    self.settings.range = RangeMode::Manual {
                        lo: self.display_range.0.floor(),
                        hi: self.display_range.1.ceil(),
                    }
                }
                (RangeMode::Manual { .. }, false) => {
                    self.settings.range = RangeMode::Auto;
                    self.auto_range.reset();
                }
                _ => {}
            }
            if let RangeMode::Manual { lo, hi } = &mut self.settings.range {
                ui.horizontal(|ui| {
                    ui.label("최저");
                    changed |= ui
                        .add(
                            egui::DragValue::new(lo)
                                .speed(0.1)
                                .suffix(format!(" {unit}")),
                        )
                        .changed();
                    ui.label("최고");
                    changed |= ui
                        .add(
                            egui::DragValue::new(hi)
                                .speed(0.1)
                                .suffix(format!(" {unit}")),
                        )
                        .changed();
                });
            }
            changed |= ui
                .checkbox(&mut self.settings.show_extremes, "최고·최저 위치 표시")
                .changed();
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

            ui.separator();
            webcam::controls(ui, self);

            ui.separator();
            ui.heading("측정");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tool, Tool::Spot, "⊙ 점");
                ui.selectable_value(&mut self.tool, Tool::Box, "▭ 영역");
                ui.selectable_value(&mut self.tool, Tool::Line, "╱ 선");
            });
            ui.label(match self.tool {
                Tool::Spot => "클릭: 점 추가 · 끌기: 측정 이동 · 우클릭: 삭제",
                Tool::Box => "끌기: 영역 그리기, 테두리는 크기 조절 · 우클릭: 삭제",
                Tool::Line => "끌기: 선 그리기, 끝점은 조절 · 우클릭: 삭제",
            });
            if self.frame.as_ref().is_some_and(|f| f.calibration.is_some()) {
                ui.colored_label(
                    calibration::ACTIVE,
                    format!("보정 적용: {}", self.settings.calibration.summary()),
                );
            }
            if let (Some(f), Some(st)) = (&self.frame, &self.stats) {
                egui::Grid::new("stats").num_columns(2).show(ui, |ui| {
                    ui.label("최고");
                    ui.label(format!("{:.2} {unit}", f.value(st.max)));
                    ui.end_row();
                    ui.label("최저");
                    ui.label(format!("{:.2} {unit}", f.value(st.min)));
                    ui.end_row();
                    ui.label("평균");
                    ui.label(format!("{:.2} {unit}", f.value_f(st.mean)));
                    ui.end_row();
                    ui.label("중앙");
                    ui.label(format!(
                        "{:.2} {unit}",
                        f.value_around(f.width / 2, f.height / 2, 1)
                    ));
                    ui.end_row();
                });
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
            let any =
                !self.settings.spots.items.is_empty() || !self.settings.shapes.items.is_empty();
            if any && ui.button("측정 모두 삭제").clicked() {
                self.settings.spots.clear();
                self.settings.shapes.clear();
                self.mark_dirty();
            }

            ui.separator();
            calibration::controls(ui, self, status);

            ui.separator();
            alarms::controls(ui, self);

            ui.separator();
            remote::controls(ui, self);
        });
    }

    /// Why the rotation cannot change now: a recording keeps one image size and fixed
    /// measurement positions, and played-back frames were already turned when recorded.
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
        ui.heading("Lepton 보정(FFC)");
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
        ui.heading("P3 설정");
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
                self.notify("FFC 실행됨. 약 1초 후 렌즈를 여세요.");
            }
        }
    }
}

impl ThermalApp {
    /// Frame processing, recording, alarms and remote commands. Runs on every pass, and also
    /// while the window is minimized or not being drawn, when eframe skips `ui`.
    fn tick(&mut self, ctx: &egui::Context) {
        let status = self.camera.status();
        self.last_fpa = status.fpa_c;
        // Every queued frame, oldest first, so recordings and alarms see them all.
        loop {
            let new_live = self.update_frame();
            calibration::update(self, &status, new_live);
            alarms::update(self, ctx, &status, new_live);
            if new_live.is_none() {
                break;
            }
        }
        remote::sync(self, ctx);
        remote::process(self, &status);
        webcam::sync(self, ctx);
        self.poll_ai_jobs();
        self.save_settings_if_due(false);
        // Keep status fresh even when no frames arrive (disconnected / stalled).
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl eframe::App for ThermalApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let status = self.camera.status();

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("ThermalView");
                ui.separator();
                if ui
                    .add_enabled(self.frame.is_some(), egui::Button::new("📷 스냅샷"))
                    .clicked()
                {
                    let _ = self.take_snapshot(&status);
                }
                let rec_label = match &self.csv {
                    Some(log) => format!("⏹ 기록 중지 ({}행)", log.rows()),
                    None => "⏺ CSV 기록".to_string(),
                };
                if ui
                    .add_enabled(
                        self.frame.is_some() || self.csv.is_some(),
                        egui::Button::new(rec_label).selected(self.csv.is_some()),
                    )
                    .on_hover_text("프레임마다 최고·최저·평균·중앙·측정점 값을 CSV로 기록")
                    .clicked()
                {
                    self.toggle_csv();
                }
                ui.separator();
                let seq_label = match &self.seq {
                    Some(w) => format!(
                        "⏹ 녹화 중지 ({:.0}초, {:.0} MB)",
                        w.elapsed_secs(),
                        w.bytes() as f64 / 1e6
                    ),
                    None => "⏺ 녹화".to_string(),
                };
                let can_record =
                    self.player.is_none() && (self.frame.is_some() || self.seq.is_some());
                if ui
                    .add_enabled(
                        can_record,
                        egui::Button::new(seq_label).selected(self.seq.is_some()),
                    )
                    .on_hover_text(
                        "모든 프레임의 온도 원본을 .tvseq 파일로 무손실 녹화 (약 0.6 GB/시간)",
                    )
                    .clicked()
                {
                    self.toggle_seq(&status);
                }
                let mp4_label = match &self.video {
                    Some(w) => format!("⏹ MP4 중지 ({:.0}초)", w.elapsed_secs()),
                    None => "⏺ MP4".to_string(),
                };
                if ui
                    .add_enabled(
                        self.frame.is_some() || self.video.is_some(),
                        egui::Button::new(mp4_label).selected(self.video.is_some()),
                    )
                    .on_hover_text(
                        "보이는 영상(팔레트, 측정 표시, 색 막대)을 MP4 동영상으로 녹화. \n                         재생 중이면 재생 화면을 녹화",
                    )
                    .clicked()
                {
                    self.toggle_video();
                }
                if ui.button("▶ 재생…").clicked() {
                    self.show_open_dialog = true;
                }
                if ui.button("📁 저장 폴더").clicked() {
                    let dir = self.settings.snapshot_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                }
                ui.separator();
                ui.toggle_value(&mut self.graphs.visible, "📈 그래프");
            });
        });
        alarms::banner(ui, self);

        let mut close_player = false;
        if let Some(p) = &mut self.player {
            egui::Panel::bottom("playback").show(ui, |ui| match p.ui(ui) {
                Ok(playback::PlayerAction::Close) => close_player = true,
                Ok(playback::PlayerAction::None) => {}
                Err(e) => {
                    log::warn!("playback error: {e}");
                    close_player = true;
                }
            });
        }
        if close_player {
            self.close_player();
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let msg = match &self.message {
                    Some((m, t)) if t.elapsed() < Duration::from_secs(8) => Some(m.clone()),
                    _ => status.last_error.clone(),
                };
                ui.label(msg.unwrap_or_default());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "프레임 {} · 정지 복구 {} · 셔터 재적용 {}",
                        status.frames, status.stalls, status.reapplied
                    ));
                });
            });
        });

        egui::Panel::left("controls")
            .resizable(true)
            .default_size(290.0)
            .show(ui, |ui| self.controls(ui, &status));

        if self.graphs.visible {
            egui::Panel::bottom("graphs")
                .resizable(true)
                .default_size(240.0)
                .show(ui, |ui| graphs::show(ui, self));
        }

        // After the panels: their controls may have recolored the image.
        self.upload_texture(&ctx);
        webcam::upload_texture(self, &ctx);
        egui::CentralPanel::default().show(ui, |ui| {
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
}
