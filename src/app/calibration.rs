//! Temperature correction UI: emissivity, reflected temperature and reference points.

use std::sync::Arc;

use eframe::egui::{self, Color32};

use super::ThermalApp;
use crate::camera::{CameraStatus, ConnState};
use crate::thermal::calibration::{
    CalTable, Calibration, EMISSIVITY_PRESETS, MAX_REFS, MIN_EMISSIVITY,
};
use crate::thermal::frame::ThermalFrame;
use crate::thermal::measure::{Spot, Spots};

/// Live frames averaged for one reference reading (about 2 s at 8.7 fps).
pub(super) const CAPTURE_FRAMES: u32 = 16;
pub(super) const ACTIVE: Color32 = Color32::from_rgb(235, 170, 30);

/// Where a reference is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RefSource {
    Center,
    Spot(u32),
}

impl RefSource {
    fn label(self) -> String {
        match self {
            RefSource::Center => "중앙".into(),
            RefSource::Spot(id) => format!("S{id}"),
        }
    }
}

struct Capture {
    source: RefSource,
    reference_c: f32,
    sum: f64,
    n: u32,
}

pub(super) struct CalState {
    table: Option<Arc<CalTable>>,
    source: RefSource,
    reference_c: f32,
    capture: Option<Capture>,
}

impl Default for CalState {
    fn default() -> Self {
        Self {
            table: None,
            source: RefSource::Center,
            reference_c: 0.0,
            capture: None,
        }
    }
}

impl CalState {
    /// Correction table for frames with this scale, rebuilt only when the scale or settings change.
    fn table_for(
        &mut self,
        kelvin_per_count: Option<f32>,
        cal: &Calibration,
    ) -> Option<Arc<CalTable>> {
        let k = kelvin_per_count?;
        if cal.is_identity() {
            return None;
        }
        if let Some(t) = &self.table {
            if t.matches(k, cal) {
                return Some(t.clone());
            }
        }
        let t = Arc::new(CalTable::new(k, cal));
        self.table = Some(t.clone());
        Some(t)
    }

    #[cfg(test)]
    pub(super) fn capturing(&self) -> Option<u32> {
        self.capture.as_ref().map(|c| c.n)
    }
}

/// Attach the current correction to a frame that is about to be shown.
pub(super) fn attach(app: &mut ThermalApp, f: &mut ThermalFrame) {
    f.calibration = app
        .cal
        .table_for(f.kelvin_per_count, &app.settings.calibration);
}

/// Re-apply the correction to the frame on screen after a settings change.
pub(super) fn reapply(app: &mut ThermalApp) {
    if let Some(mut f) = app.frame.take() {
        attach(app, &mut f);
        app.frame = Some(f);
    }
    app.auto_range.reset();
    app.render();
}

/// Uncorrected camera reading at `src` (3×3 mean), °C.
fn reading(f: &ThermalFrame, src: RefSource, spots: &Spots) -> Option<f32> {
    let (x, y) = match src {
        RefSource::Center => (f.width / 2, f.height / 2),
        RefSource::Spot(id) => {
            let s = spots.items.iter().find(|s| s.id == id)?;
            if s.x >= f.width || s.y >= f.height {
                return None;
            }
            (s.x, s.y)
        }
    };
    f.apparent_c(f.mean_around(x, y, Spot::RADIUS))
}

/// Start averaging a reference reading (only on live, radiometric video).
pub(super) fn start_capture(app: &mut ThermalApp) {
    app.cal.capture = Some(Capture {
        source: app.cal.source,
        reference_c: app.cal.reference_c,
        sum: 0.0,
        n: 0,
    });
}

/// Run once per pass: feeds a reference capture with new live frames.
pub(super) fn update(app: &mut ThermalApp, status: &CameraStatus, new_live: Option<f64>) {
    let Some(cap) = &mut app.cal.capture else {
        return;
    };
    if app.player.is_some() || status.state != ConnState::Streaming {
        app.cal.capture = None;
        app.notify("기준점 기록을 취소했습니다: 실시간 영상이 아닙니다");
        return;
    }
    let (Some(_), Some(f)) = (new_live, &app.frame) else {
        return;
    };
    match reading(f, cap.source, &app.settings.spots) {
        Some(v) => {
            cap.sum += v as f64;
            cap.n += 1;
        }
        None => {
            app.cal.capture = None;
            app.notify("기준점 기록을 취소했습니다: 측정 위치나 온도값이 없습니다");
            return;
        }
    }
    if cap.n < CAPTURE_FRAMES {
        return;
    }
    let measured = (cap.sum / cap.n as f64) as f32;
    let reference = cap.reference_c;
    app.cal.capture = None;
    match app.settings.calibration.add_ref(measured, reference) {
        Ok(()) => {
            log::info!(
                "calibration reference: measured {measured:.2} C, reference {reference:.2} C"
            );
            app.notify(format!(
                "기준점 기록: 측정 {measured:.2} °C → 기준 {reference:.2} °C"
            ));
            app.mark_dirty();
            reapply(app);
        }
        Err(e) => app.notify(format!("기준점을 기록하지 못했습니다: {e}")),
    }
}

/// Settings panel section.
pub(super) fn controls(ui: &mut egui::Ui, app: &mut ThermalApp, status: &CameraStatus) {
    ui.heading("온도 보정");
    if let Some(e) = status.camera_emissivity {
        if (e - 1.0).abs() > 0.001 {
            ui.colored_label(
                ACTIVE,
                format!("카메라 내부 방사율이 {e:.2}로 설정돼 있어 앱 보정과 겹칩니다."),
            );
        }
    }
    let radiometric = app.frame.as_ref().is_none_or(|f| f.is_radiometric());
    if !radiometric {
        ui.label("TLinear가 꺼져 있어 보정을 적용할 수 없습니다.");
    }
    let locked = app.csv.is_some();
    if locked {
        ui.label("CSV 기록 중에는 보정을 바꿀 수 없습니다.");
    }
    if let RefSource::Spot(id) = app.cal.source {
        if !app.settings.spots.items.iter().any(|s| s.id == id) {
            app.cal.source = RefSource::Center;
        }
    }
    let can_capture = !locked
        && radiometric
        && app.player.is_none()
        && status.state == ConnState::Streaming
        && app.settings.calibration.refs.len() < MAX_REFS;

    let before = app.settings.calibration.clone();
    let mut start = false;
    let mut cancel = false;
    ui.add_enabled_ui(!locked, |ui| {
        let ThermalApp {
            settings, cal: st, ..
        } = app;
        let cal = &mut settings.calibration;
        egui::Grid::new("calibration")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("방사율");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut cal.emissivity)
                            .speed(0.005)
                            .range(MIN_EMISSIVITY..=1.0)
                            .fixed_decimals(2),
                    );
                    egui::ComboBox::from_id_salt("emissivity_preset")
                        .width(60.0)
                        .selected_text("예시")
                        .show_ui(ui, |ui| {
                            for (name, e) in EMISSIVITY_PRESETS {
                                if ui.selectable_label(cal.emissivity == e, name).clicked() {
                                    cal.emissivity = e;
                                }
                            }
                        });
                });
                ui.end_row();
                ui.label("반사 온도");
                ui.add_enabled(
                    cal.uses_emissivity(),
                    egui::DragValue::new(&mut cal.reflected_c)
                        .speed(0.1)
                        .range(-50.0..=500.0)
                        .suffix(" °C"),
                )
                .on_hover_text("측정 대상에 비치는 주변 온도. 보통 실내 온도입니다");
                ui.end_row();
            });

        ui.add_space(4.0);
        ui.strong("기준점 보정");
        let mut remove = None;
        for (i, p) in cal.refs.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "기준 {}: 측정 {:.2} → {:.2} °C",
                    i + 1,
                    p.measured_c,
                    p.reference_c
                ));
                if ui.small_button("✕").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            cal.refs.remove(i);
        }
        if !cal.refs.is_empty() {
            let (g, o) = cal.linear();
            ui.label(format!("기울기 {g:.3}, 오프셋 {o:+.2} °C"));
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ref_source")
                .width(56.0)
                .selected_text(st.source.label())
                .show_ui(ui, |ui| {
                    let options = std::iter::once(RefSource::Center)
                        .chain(settings.spots.items.iter().map(|s| RefSource::Spot(s.id)));
                    for s in options {
                        ui.selectable_value(&mut st.source, s, s.label());
                    }
                });
            ui.label("기준");
            ui.add(
                egui::DragValue::new(&mut st.reference_c)
                    .speed(0.1)
                    .range(-40.0..=600.0)
                    .suffix(" °C"),
            );
        });
        match &st.capture {
            Some(c) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("기록 중 {}/{CAPTURE_FRAMES}", c.n));
                    cancel = ui.small_button("취소").clicked();
                });
            }
            None => {
                start = ui
                    .add_enabled(can_capture, egui::Button::new("기준점 기록"))
                    .on_hover_text(
                        "온도를 아는 물체(얼음물, 끓는 물, 접촉 온도계로 잰 표면)를 \
                         기준 위치에 두고 누르세요. 약 2초간 평균합니다. \
                         기준체의 방사율은 위 설정과 같아야 합니다",
                    )
                    .clicked();
            }
        }
        if !cal.is_identity() && ui.button("보정 초기화").clicked() {
            *cal = Calibration::default();
        }
    });
    if cancel {
        app.cal.capture = None;
    }
    if start {
        start_capture(app);
    }
    if app.settings.calibration != before {
        app.mark_dirty();
        reapply(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::settings::Settings;

    fn streaming() -> CameraStatus {
        CameraStatus {
            state: ConnState::Streaming,
            ..Default::default()
        }
    }

    /// 26.85 °C apparent everywhere.
    fn rig() -> (ThermalApp, egui::Context) {
        let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        let f = ThermalFrame::new(160, 120, vec![30000; 160 * 120], Some(0.01));
        app.stats = Some(f.stats());
        app.frame = Some(f);
        (app, egui::Context::default())
    }

    #[test]
    fn capture_averages_live_frames_then_corrects_display() {
        let (mut app, _) = rig();
        app.cal.reference_c = 27.35;
        start_capture(&mut app);
        for i in 0..CAPTURE_FRAMES {
            // Repeated passes without a new frame must not count.
            update(&mut app, &streaming(), None);
            update(&mut app, &streaming(), Some(i as f64));
        }
        assert_eq!(app.cal.capturing(), None);
        let refs = &app.settings.calibration.refs;
        assert_eq!(refs.len(), 1);
        assert!((refs[0].measured_c - 26.85).abs() < 1e-3);
        let f = app.frame.as_ref().unwrap();
        assert!(f.calibration.is_some());
        assert!((f.value(30000) - 27.35).abs() < 1e-3);
        assert!(app.settings_dirty.is_some());
    }

    #[test]
    fn capture_stops_without_live_video_or_its_spot() {
        let (mut app, _) = rig();
        start_capture(&mut app);
        update(&mut app, &CameraStatus::default(), None);
        assert_eq!(app.cal.capturing(), None);

        let id = app.settings.spots.add(10, 10).unwrap();
        app.cal.source = RefSource::Spot(id);
        start_capture(&mut app);
        update(&mut app, &streaming(), Some(0.0));
        assert_eq!(app.cal.capturing(), Some(1));
        app.settings.spots.remove(id);
        update(&mut app, &streaming(), Some(0.1));
        assert_eq!(app.cal.capturing(), None);
        assert!(app.settings.calibration.refs.is_empty());
    }

    #[test]
    fn identity_attaches_no_table() {
        let (mut app, _) = rig();
        let mut f = ThermalFrame::new(2, 2, vec![30000; 4], Some(0.01));
        attach(&mut app, &mut f);
        assert!(f.calibration.is_none());
        app.settings.calibration.emissivity = 0.95;
        attach(&mut app, &mut f);
        let first = f.calibration.clone().unwrap();
        attach(&mut app, &mut f);
        // Unchanged settings reuse the same table.
        assert!(Arc::ptr_eq(&first, f.calibration.as_ref().unwrap()));
    }
}
