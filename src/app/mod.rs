//! eframe application: image view, control panel, status bar.

mod graphs;
mod playback;
mod view;

use std::time::{Duration, Instant};

use eframe::egui;

use crate::camera::lepton::ShutterMode;
use crate::camera::{Camera, CameraStatus, Command, ConnState};
use crate::settings::Settings;
use crate::storage::csv_log::{self, CsvLog};
use crate::storage::sequence::{self, SeqHeader, SeqWriter};
use crate::storage::snapshot::{self, ShapeReading, SnapshotMeta, SpotReading};
use crate::thermal::frame::{FrameStats, ThermalFrame};
use crate::thermal::history::{History, Sample};
use crate::thermal::palette::{self, Palette};
use crate::thermal::range::{AutoRange, RangeMode};
use crate::thermal::shapes::ShapeKind;

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

pub struct ThermalApp {
    camera: Camera,
    settings: Settings,
    settings_dirty: Option<Instant>,

    frame: Option<ThermalFrame>,
    stats: Option<FrameStats>,
    rgba: Vec<u8>,
    texture: Option<egui::TextureHandle>,
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
    /// Pixel where the current Box/Line drag started, and where it is now.
    drag_from: Option<(usize, usize)>,
    drag_to: Option<(usize, usize)>,

    /// Radiometric sequence recording (live only).
    seq: Option<SeqWriter>,
    /// When set, the image comes from a recorded sequence instead of the camera.
    player: Option<playback::Player>,
    show_open_dialog: bool,
}

impl ThermalApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_korean_font(&cc.egui_ctx);
        let settings = Settings::load();
        let ctx = cc.egui_ctx.clone();
        let camera = Camera::start(settings.shutter_mode, move || ctx.request_repaint());
        let lut = (settings.palette, settings.palette.lut());
        Self {
            camera,
            settings,
            settings_dirty: None,
            frame: None,
            stats: None,
            rgba: Vec::new(),
            texture: None,
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
            drag_from: None,
            drag_to: None,
            seq: None,
            player: None,
            show_open_dialog: false,
        }
    }

    fn toggle_seq(&mut self, status: &CameraStatus) {
        if let Some(w) = self.seq.take() {
            match w.finish() {
                Ok((path, n)) => self.notify(format!("녹화 종료: {n}프레임 → {}", path.display())),
                Err(e) => self.notify(format!("녹화 파일 저장 실패: {e}")),
            }
            return;
        }
        let Some(f) = &self.frame else {
            return;
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
                self.notify(format!("녹화 시작: {}", w.path().display()));
                self.seq = Some(w);
            }
            Err(e) => self.notify(format!("녹화 파일을 만들 수 없습니다: {e}")),
        }
    }

    fn open_sequence(&mut self, path: &std::path::Path) {
        match playback::Player::open(path) {
            Ok(p) => {
                if let Some(w) = self.seq.take() {
                    let _ = w.finish();
                }
                self.history.clear();
                self.auto_range.reset();
                self.notify(format!("재생: {}", p.file_name()));
                self.player = Some(p);
            }
            Err(e) => self.notify(format!("열 수 없습니다: {e}")),
        }
    }

    fn close_player(&mut self) {
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
            mean: value_of_mean(f, st.mean),
            center: f.value_around(f.width / 2, f.height / 2, 1),
        };
        let ids: Vec<u32> = spots.iter().map(|(id, _)| *id).collect();
        self.history.retain_spots(&ids);
        self.history.push(sample, &spots);
        if let Some(log) = &mut self.csv {
            let row = csv_log::Row {
                sequence: f.sequence,
                max: sample.max,
                min: sample.min,
                mean: sample.mean,
                center: sample.center,
                fpa_c: self.last_fpa,
                spots: &spots,
            };
            if let Err(e) = log.write(row) {
                let msg = format!("CSV 기록 중단: {e}");
                self.csv = None;
                self.notify(msg);
            }
        }
    }

    fn toggle_csv(&mut self) {
        if let Some(log) = self.csv.take() {
            match log.finish() {
                Ok((path, rows)) => {
                    self.notify(format!("기록 종료: {rows}행 → {}", path.display()))
                }
                Err(e) => self.notify(format!("CSV 저장 실패: {e}")),
            }
            return;
        }
        let unit = match self.frame.as_ref().map(|f| f.is_radiometric()) {
            Some(false) => "cnt",
            _ => "C",
        };
        let ids: Vec<u32> = self.settings.spots.items.iter().map(|s| s.id).collect();
        match CsvLog::start(&self.settings.snapshot_dir(), &ids, unit) {
            Ok(log) => {
                self.notify(format!("기록 시작: {}", log.path().display()));
                self.csv = Some(log);
            }
            Err(e) => self.notify(format!("CSV 파일을 만들 수 없습니다: {e}")),
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

    /// Pull the newest frame (from the camera, or the player during playback) and re-render.
    fn update_frame(&mut self, ctx: &egui::Context) {
        let live = self.camera.take_frame();
        let (f, t) = if let Some(p) = &mut self.player {
            // Live frames are dropped during playback; the camera stays connected.
            match p.tick() {
                Ok(Some((t_ms, f))) => (f, t_ms as f64 / 1000.0),
                Ok(None) => return,
                Err(e) => {
                    let msg = format!("재생 오류: {e}");
                    self.close_player();
                    self.notify(msg);
                    return;
                }
            }
        } else {
            let Some(f) = live else {
                return;
            };
            if let Some(w) = &mut self.seq {
                w.push(&f);
            }
            (f, self.started.elapsed().as_secs_f64())
        };
        self.stats = Some(f.stats());
        self.frame = Some(f);
        self.render(ctx);
        self.record(t);
    }

    /// Recolor the current frame (after a new frame or a palette/range change).
    fn render(&mut self, ctx: &egui::Context) {
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
        let image = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &self.rgba);
        match &mut self.texture {
            Some(t) => t.set(image, egui::TextureOptions::NEAREST),
            None => {
                self.texture =
                    Some(ctx.load_texture("thermal", image, egui::TextureOptions::NEAREST))
            }
        }
    }

    fn take_snapshot(&mut self, status: &CameraStatus) {
        let (Some(f), Some(st)) = (&self.frame, &self.stats) else {
            self.notify("저장할 영상이 없습니다");
            return;
        };
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
            mean: f.value(st.mean.round() as u16),
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
        };
        let dir = self.settings.snapshot_dir();
        match snapshot::save(&dir, f, &self.rgba, &meta) {
            Ok(files) => self.notify(format!("저장됨: {}", files.color_png.display())),
            Err(e) => self.notify(format!("스냅샷 저장 실패: {e}")),
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
                ui.label("프레임");
                ui.label(format!("{:.1} fps", status.fps));
                ui.end_row();
                if let Some(t) = status.fpa_c {
                    ui.label("센서(FPA)");
                    ui.label(format!("{t:.2} °C"));
                    ui.end_row();
                }
                ui.label("온도 단위");
                ui.label(match status.kelvin_per_count {
                    Some(k) => format!("TLinear {k} K"),
                    None => "원시값(온도 아님)".into(),
                });
                ui.end_row();
            });
            if status.state == ConnState::Busy {
                ui.label("GetThermal 등 카메라를 쓰는 다른 프로그램을 닫아 주세요.");
            }
            if status.state == ConnState::Searching {
                ui.label("PureThermal이 연결돼 있고 WinUSB 드라이버가 설치됐는지 확인하세요.");
            }

            ui.separator();
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
            if changed {
                self.mark_dirty();
                let ctx = ui.ctx().clone();
                self.render(&ctx);
            }

            ui.separator();
            ui.heading("측정");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tool, Tool::Spot, "⊙ 점");
                ui.selectable_value(&mut self.tool, Tool::Box, "▭ 영역");
                ui.selectable_value(&mut self.tool, Tool::Line, "╱ 선");
            });
            ui.label(match self.tool {
                Tool::Spot => "영상 클릭: 측정점 추가 · 우클릭: 삭제",
                Tool::Box => "드래그: 영역 그리기 · 우클릭: 삭제",
                Tool::Line => "드래그: 선 그리기 · 우클릭: 삭제",
            });
            if let (Some(f), Some(st)) = (&self.frame, &self.stats) {
                egui::Grid::new("stats").num_columns(2).show(ui, |ui| {
                    ui.label("최고");
                    ui.label(format!("{:.2} {unit}", f.value(st.max)));
                    ui.end_row();
                    ui.label("최저");
                    ui.label(format!("{:.2} {unit}", f.value(st.min)));
                    ui.end_row();
                    ui.label("평균");
                    ui.label(format!("{:.2} {unit}", value_of_mean(f, st.mean)));
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
        });
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

impl eframe::App for ThermalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.update_frame(&ctx);
        let status = self.camera.status();
        self.last_fpa = status.fpa_c;

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("ThermalView");
                ui.separator();
                if ui
                    .add_enabled(self.frame.is_some(), egui::Button::new("📷 스냅샷"))
                    .clicked()
                {
                    self.take_snapshot(&status);
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
                    .on_hover_text("모든 프레임의 온도 원본을 .tvseq 파일로 녹화 (약 1.2 GB/시간)")
                    .clicked()
                {
                    self.toggle_seq(&status);
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

        egui::CentralPanel::default().show(ui, |ui| {
            if view::image_view(ui, self) {
                self.mark_dirty();
            }
        });

        self.ffc_dialog(&ctx);
        self.open_dialog(&ctx);
        self.save_settings_if_due(false);
        // Keep status fresh even when no frames arrive (disconnected / stalled).
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn on_exit(&mut self) {
        self.save_settings_if_due(true);
        if let Some(log) = self.csv.take() {
            let _ = log.finish();
        }
        if let Some(w) = self.seq.take() {
            let _ = w.finish();
        }
    }
}

fn value_of_mean(f: &ThermalFrame, mean_raw: f32) -> f32 {
    match f.kelvin_per_count {
        Some(k) => mean_raw * k - 273.15,
        None => mean_raw,
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
