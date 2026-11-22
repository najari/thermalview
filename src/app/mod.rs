//! eframe application: image view, control panel, status bar.

mod view;

use std::time::{Duration, Instant};

use eframe::egui;

use crate::camera::lepton::ShutterMode;
use crate::camera::{Camera, CameraStatus, Command, ConnState};
use crate::settings::Settings;
use crate::storage::snapshot::{self, SnapshotMeta, SpotReading};
use crate::thermal::frame::{FrameStats, ThermalFrame};
use crate::thermal::palette::{self, Palette};
use crate::thermal::range::{AutoRange, RangeMode};

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

    /// Pull the newest frame from the camera and re-render the texture.
    fn update_frame(&mut self, ctx: &egui::Context) {
        if let Some(f) = self.camera.take_frame() {
            self.stats = Some(f.stats());
            self.frame = Some(f);
            self.render(ctx);
        }
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
            ui.label("영상 클릭: 측정점 추가 · 우클릭: 삭제");
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
            if !self.settings.spots.items.is_empty() && ui.button("측정점 모두 삭제").clicked()
            {
                self.settings.spots.clear();
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
                if ui.button("📁 저장 폴더").clicked() {
                    let dir = self.settings.snapshot_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                }
            });
        });

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

        egui::CentralPanel::default().show(ui, |ui| {
            if view::image_view(ui, self) {
                self.mark_dirty();
            }
        });

        self.ffc_dialog(&ctx);
        self.save_settings_if_due(false);
        // Keep status fresh even when no frames arrive (disconnected / stalled).
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn on_exit(&mut self) {
        self.save_settings_if_due(true);
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
