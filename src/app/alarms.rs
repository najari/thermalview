//! Temperature alarm UI: banner, rule editor and event log.

use std::collections::VecDeque;

use eframe::egui::{self, Color32};

use super::ThermalApp;
use crate::camera::{CameraStatus, ConnState};
use crate::settings::Settings;
use crate::thermal::alarm::{AlarmConfig, Condition, Metric, Monitor, Source, Suspended};
use crate::thermal::measure::Spots;
use crate::thermal::shapes::Shapes;

const LOG_MAX: usize = 100;
const RED: Color32 = Color32::from_rgb(200, 30, 30);
const AMBER: Color32 = Color32::from_rgb(235, 170, 30);
const GREEN: Color32 = Color32::from_rgb(60, 180, 75);

struct LogEntry {
    when: String,
    text: String,
    color: Option<Color32>,
}

#[derive(Default)]
pub(super) struct AlarmState {
    monitor: Monitor,
    log: VecDeque<LogEntry>,
    /// Monitoring stopped while rules were armed, so its return is worth logging.
    interrupted: bool,
}

impl AlarmState {
    pub(super) fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    fn push(&mut self, text: String, color: Option<Color32>) {
        if self.log.len() == LOG_MAX {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            when: chrono::Local::now().format("%H:%M:%S").to_string(),
            text,
            color,
        });
    }
}

/// Run once per UI pass. `new_live` is the time of a frame that just arrived from the camera.
pub(super) fn update(
    app: &mut ThermalApp,
    ctx: &egui::Context,
    status: &CameraStatus,
    new_live: Option<f64>,
) {
    let removed = app
        .settings
        .alarms
        .prune(&app.settings.spots, &app.settings.shapes);
    if removed > 0 {
        app.notify(format!(
            "삭제된 측정을 쓰던 경보 규칙 {removed}개를 지웠습니다"
        ));
        app.mark_dirty();
    }

    let why = if app.player.is_some() {
        Some(Suspended::Playback)
    } else if status.state != ConnState::Streaming {
        Some(Suspended::NoSignal)
    } else {
        match &app.frame {
            None => Some(Suspended::NoSignal),
            Some(f) if !f.is_radiometric() => Some(Suspended::NotRadiometric),
            Some(_) => None,
        }
    };
    let was = app.alarms.monitor.suspended();
    if was != why {
        match why {
            Some(w) if was.is_none() && app.settings.alarms.is_armed() => {
                app.alarms
                    .push(format!("감시 중단: {}", w.label()), Some(AMBER));
                app.alarms.interrupted = true;
            }
            None if app.alarms.interrupted => {
                app.alarms.push("감시 재개".into(), None);
                app.alarms.interrupted = false;
            }
            _ => {}
        }
        app.alarms.monitor.set_suspended(why);
    }

    let (Some(t), Some(f), Some(st)) = (new_live, &app.frame, &app.stats) else {
        return;
    };
    let s = &app.settings;
    let changes = app
        .alarms
        .monitor
        .update(&s.alarms, t, |src| src.read(f, st, &s.spots, &s.shapes));
    for c in changes {
        let Some(rule) = s.alarms.rules.iter().find(|r| r.id == c.rule_id) else {
            continue;
        };
        let what = rule.describe(&s.shapes);
        if c.raised {
            log::warn!("alarm raised: {what}, value {:.2} C", c.value);
            app.alarms
                .push(format!("발생: {what} ({:.2} °C)", c.value), Some(RED));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Critical,
            ));
        } else {
            log::info!("alarm cleared: {what}, value {:.2} C", c.value);
            app.alarms
                .push(format!("해제: {what} ({:.2} °C)", c.value), Some(GREEN));
        }
    }
}

/// Banner under the toolbar. Shown while an alarm is active, until a past alarm is acknowledged,
/// and whenever armed rules are not being evaluated.
pub(super) fn banner(ui: &mut egui::Ui, app: &mut ThermalApp) {
    let m = &app.alarms.monitor;
    let cfg = &app.settings.alarms;
    let active: Vec<String> = cfg
        .rules
        .iter()
        .filter(|r| m.is_active(r.id))
        .map(|r| {
            let now = m.value(r.id).unwrap_or(f32::NAN);
            format!("{} (현재 {now:.1} °C)", r.describe(&app.settings.shapes))
        })
        .collect();
    let unacked = m.unacknowledged();
    let (fill, text, msg) = if !active.is_empty() {
        let mut msg = format!("⚠ 경보: {}", active[..active.len().min(3)].join(", "));
        if active.len() > 3 {
            msg.push_str(&format!(" 외 {}건", active.len() - 3));
        }
        (RED, Color32::WHITE, msg)
    } else if unacked > 0 {
        let msg = format!("⚠ 경보 {unacked}건이 발생했다가 해제되었습니다");
        (AMBER, Color32::BLACK, msg)
    } else if let (true, Some(why)) = (cfg.is_armed(), m.suspended()) {
        let msg = format!("경보 감시 중단: {}", why.label());
        (AMBER, Color32::BLACK, msg)
    } else {
        return;
    };

    let mut acknowledge = false;
    egui::Panel::top("alarm_banner")
        .frame(egui::Frame::new().fill(fill).inner_margin(6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(text, egui::RichText::new(msg).strong());
                if unacked > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        acknowledge = ui.button("확인").clicked();
                    });
                }
            });
        });
    if acknowledge {
        app.alarms.monitor.acknowledge();
    }
}

fn source_options(spots: &Spots, shapes: &Shapes) -> Vec<Source> {
    let mut out: Vec<Source> = Metric::ALL.iter().map(|m| Source::Frame(*m)).collect();
    out.push(Source::Center);
    out.extend(spots.items.iter().map(|s| Source::Spot(s.id)));
    for s in &shapes.items {
        out.extend(Metric::ALL.iter().map(|m| Source::Shape(s.id, *m)));
    }
    out
}

/// Rule editor and event log, in the control panel.
pub(super) fn controls(ui: &mut egui::Ui, app: &mut ThermalApp) {
    ui.heading("온도 경보");
    let suggested = match (&app.frame, &app.stats) {
        (Some(f), Some(st)) if f.is_radiometric() => (f.value(st.max) + 5.0).ceil(),
        _ => 60.0,
    };
    let mut changed = false;
    {
        let Settings {
            alarms: cfg,
            spots,
            shapes,
            ..
        } = &mut app.settings;
        let monitor = &app.alarms.monitor;
        changed |= ui.checkbox(&mut cfg.enabled, "경보 사용").changed();
        let options = source_options(spots, shapes);
        let mut remove = None;
        for r in &mut cfg.rules {
            ui.push_id(r.id, |ui| {
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut r.enabled, "").changed();
                    egui::ComboBox::from_id_salt("source")
                        .width(92.0)
                        .selected_text(r.source.label(shapes))
                        .show_ui(ui, |ui| {
                            for s in &options {
                                changed |= ui
                                    .selectable_value(&mut r.source, *s, s.label(shapes))
                                    .changed();
                            }
                        });
                    egui::ComboBox::from_id_salt("condition")
                        .width(52.0)
                        .selected_text(r.condition.label())
                        .show_ui(ui, |ui| {
                            for c in [Condition::Above, Condition::Below] {
                                changed |= ui
                                    .selectable_value(&mut r.condition, c, c.label())
                                    .changed();
                            }
                        });
                });
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut r.threshold)
                                .speed(0.1)
                                .range(-40.0..=600.0)
                                .suffix(" °C"),
                        )
                        .changed();
                    match monitor.value(r.id) {
                        Some(v) if monitor.is_active(r.id) => {
                            ui.colored_label(RED, format!("경보 {v:.1} °C"));
                        }
                        Some(v) if monitor.is_pending(r.id) => {
                            ui.colored_label(AMBER, format!("확인 중 {v:.1} °C"));
                        }
                        Some(v) => {
                            ui.label(format!("정상 {v:.1} °C"));
                        }
                        None => {
                            ui.label("-");
                        }
                    }
                    if ui.small_button("✕").clicked() {
                        remove = Some(r.id);
                    }
                });
            });
        }
        if let Some(id) = remove {
            cfg.remove(id);
            changed = true;
        }
        if ui
            .add_enabled(
                cfg.rules.len() < AlarmConfig::MAX_RULES,
                egui::Button::new("규칙 추가"),
            )
            .clicked()
        {
            cfg.add(Source::Frame(Metric::Max), Condition::Above, suggested);
            changed = true;
        }
        egui::Grid::new("alarm_timing")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("유지 시간");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut cfg.hold_s)
                            .speed(0.1)
                            .range(0.0..=60.0)
                            .suffix(" 초"),
                    )
                    .on_hover_text("조건이 이 시간 동안 이어져야 경보가 발생합니다")
                    .changed();
                ui.end_row();
                ui.label("해제 폭");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut cfg.hysteresis)
                            .speed(0.05)
                            .range(0.0..=20.0)
                            .suffix(" °C"),
                    )
                    .on_hover_text("기준값에서 이만큼 되돌아와야 경보가 해제됩니다")
                    .changed();
                ui.end_row();
            });
    }
    egui::CollapsingHeader::new(format!("경보 기록 ({})", app.alarms.log.len()))
        .id_salt("alarm_log")
        .show(ui, |ui| {
            if app.alarms.log.is_empty() {
                ui.label("기록이 없습니다.");
            }
            for e in app.alarms.log.iter().rev() {
                let line = format!("{} {}", e.when, e.text);
                match e.color {
                    Some(c) => ui.colored_label(c, line),
                    None => ui.label(line),
                };
            }
        });
    if changed {
        app.mark_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::thermal::frame::ThermalFrame;

    /// About 26.85 °C everywhere, with one rule at 20 °C.
    fn rig(kelvin_per_count: Option<f32>) -> (ThermalApp, egui::Context) {
        let mut settings = Settings::default();
        settings
            .alarms
            .add(Source::Frame(Metric::Max), Condition::Above, 20.0);
        let mut app = ThermalApp::with_camera(settings, Camera::disconnected());
        let f = ThermalFrame::new(160, 120, vec![30000; 160 * 120], kelvin_per_count);
        app.stats = Some(f.stats());
        app.frame = Some(f);
        (app, egui::Context::default())
    }

    fn streaming() -> CameraStatus {
        CameraStatus {
            state: ConnState::Streaming,
            ..Default::default()
        }
    }

    #[test]
    fn raises_only_on_new_live_frames_and_stops_without_signal() {
        let (mut app, ctx) = rig(Some(0.01));
        let id = app.settings.alarms.rules[0].id;
        update(&mut app, &ctx, &streaming(), Some(0.0));
        // The same frame shown again carries no new information.
        update(&mut app, &ctx, &streaming(), None);
        assert!(!app.alarms.monitor.is_active(id));
        update(&mut app, &ctx, &streaming(), Some(1.0));
        assert!(app.alarms.monitor.is_active(id));
        assert_eq!(app.alarms.log.len(), 1);

        update(&mut app, &ctx, &CameraStatus::default(), None);
        assert_eq!(app.alarms.monitor.suspended(), Some(Suspended::NoSignal));
        assert!(!app.alarms.monitor.is_active(id));
        assert!(app.alarms.log.back().unwrap().text.starts_with("감시 중단"));
        // The raised alarm must still be waiting for the user.
        assert_eq!(app.alarms.monitor.unacknowledged(), 1);

        update(&mut app, &ctx, &streaming(), Some(2.0));
        assert_eq!(app.alarms.log.back().unwrap().text, "감시 재개");
        assert!(!app.alarms.monitor.is_active(id));
    }

    #[test]
    fn uncalibrated_counts_never_raise() {
        let (mut app, ctx) = rig(None);
        let id = app.settings.alarms.rules[0].id;
        for t in 0..3 {
            update(&mut app, &ctx, &streaming(), Some(t as f64));
        }
        assert_eq!(
            app.alarms.monitor.suspended(),
            Some(Suspended::NotRadiometric)
        );
        assert!(!app.alarms.monitor.is_active(id));
    }

    #[test]
    fn deleting_a_measurement_removes_its_rule() {
        let (mut app, ctx) = rig(Some(0.01));
        let spot = app.settings.spots.add(10, 10).unwrap();
        app.settings
            .alarms
            .add(Source::Spot(spot), Condition::Above, 20.0);
        update(&mut app, &ctx, &streaming(), Some(0.0));
        assert_eq!(app.settings.alarms.rules.len(), 2);
        app.settings.spots.clear();
        // After a clear the next new spot gets the same id: the rule must not follow it.
        update(&mut app, &ctx, &streaming(), Some(0.1));
        assert_eq!(app.settings.spots.add(90, 90), Some(spot));
        assert_eq!(app.settings.alarms.rules.len(), 1);
        assert!(app.settings_dirty.is_some());
    }
}
