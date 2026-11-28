//! Temperature alarm UI: banner, rule editor and event log.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32};

use super::ThermalApp;
use crate::camera::{CameraStatus, ConnState};
use crate::settings::Settings;
use crate::storage::alarm_log::{self, Event};
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
    /// The MP4 being recorded was started by an alarm (and is ended by it). Since when no
    /// alarm has been active, while that recording runs.
    auto_video: Option<Option<Instant>>,
    /// Writing `alarm_log.csv` failed; reported once until a write works again.
    file_failed: bool,
}

impl AlarmState {
    pub(super) fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    /// The running MP4 was started by an alarm.
    pub(super) fn auto_video(&self) -> bool {
        self.auto_video.is_some()
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
    } else if app.hold.is_some() {
        Some(Suspended::Frozen)
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
                write_file(app, Event::Paused, w.label(), None);
            }
            None if app.alarms.interrupted => {
                app.alarms.push("감시 재개".into(), None);
                app.alarms.interrupted = false;
                write_file(app, Event::Resumed, "", None);
            }
            _ => {}
        }
        app.alarms.monitor.set_suspended(why);
    }

    let mut raised = false;
    if let (Some(t), Some(f), Some(st)) = (new_live, &app.frame, &app.stats) {
        let s = &app.settings;
        let changes = app
            .alarms
            .monitor
            .update(&s.alarms, t, |r| r.read(f, st, &s.spots, &s.shapes));
        let mut events = Vec::new();
        for c in changes {
            let Some(rule) = s.alarms.rules.iter().find(|r| r.id == c.rule_id) else {
                continue;
            };
            let what = rule.describe(&s.shapes);
            if c.raised {
                raised = true;
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
            let event = if c.raised {
                Event::Raised
            } else {
                Event::Cleared
            };
            events.push((event, what, c.value));
        }
        for (event, what, value) in events {
            write_file(app, event, &what, Some(value));
        }
    }
    // One picture of the frame that raised it, however many rules tripped together.
    if raised && app.settings.alarms.snapshot_on_raise {
        if let Ok(path) = app.take_snapshot(status) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            app.alarms.push(format!("스냅샷 저장: {name}"), None);
        }
    }
    auto_video(app, raised);
}

/// Start an MP4 when an alarm is raised and nothing is being recorded; end it once no alarm
/// has been active for `record_after_s` seconds. A recording the user started is left alone,
/// and one the user stopped is not restarted until the next alarm.
fn auto_video(app: &mut ThermalApp, raised: bool) {
    if app.alarms.auto_video.is_some() && app.video.is_none() {
        // Stopped by the user, by an error or by a change of camera.
        app.alarms.auto_video = None;
    }
    if raised && app.settings.alarms.record_on_raise && app.video.is_none() {
        if let Ok(path) = app.start_video() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            app.alarms.push(format!("MP4 녹화 시작: {name}"), None);
            app.alarms.auto_video = Some(None);
        }
    }
    let Some(quiet_since) = app.alarms.auto_video else {
        return;
    };
    if app.alarms.monitor.any_active() {
        app.alarms.auto_video = Some(None);
        return;
    }
    let since = quiet_since.unwrap_or_else(Instant::now);
    app.alarms.auto_video = Some(Some(since));
    let after = Duration::from_secs_f32(app.settings.alarms.record_after_s.clamp(0.0, 3600.0));
    if since.elapsed() >= after {
        app.alarms.auto_video = None;
        let msg = match app.stop_video() {
            Ok((path, n)) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                format!("MP4 녹화 종료: {name} ({n}프레임)")
            }
            Err(e) => format!("MP4 녹화 종료: {e}"),
        };
        app.alarms.push(msg, None);
    }
}

/// Append to `alarm_log.csv` when the option is on. A failure is shown once, not per event.
fn write_file(app: &mut ThermalApp, event: Event, what: &str, value: Option<f32>) {
    if !app.settings.alarms.log_to_file {
        return;
    }
    match alarm_log::append(&app.settings.snapshot_dir(), event, what, value) {
        Ok(_) => app.alarms.file_failed = false,
        Err(e) => {
            log::warn!("writing the alarm log failed: {e:#}");
            if !app.alarms.file_failed {
                app.alarms.file_failed = true;
                app.alarms
                    .push(format!("경보 기록 파일 저장 실패: {e}"), Some(AMBER));
            }
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

/// Everything a rule (or a difference) can read: frame statistics, the center, each Spot and
/// each statistic of each Box and Line. Never empty.
pub(super) fn source_options(spots: &Spots, shapes: &Shapes) -> Vec<Source> {
    let mut out: Vec<Source> = Metric::ALL.iter().map(|m| Source::Frame(*m)).collect();
    out.push(Source::Center);
    out.extend(spots.items.iter().map(|s| Source::Spot(s.id)));
    for s in &shapes.items {
        out.extend(Metric::ALL.iter().map(|m| Source::Shape(s.id, *m)));
    }
    out
}

/// Rule editor and event log, in the control panel.
/// One-line state for the header of the panel section.
pub(super) fn summary(app: &ThermalApp) -> String {
    let cfg = &app.settings.alarms;
    let m = &app.alarms.monitor;
    let active = cfg.rules.iter().filter(|r| m.is_active(r.id)).count();
    if active > 0 {
        format!("경보 {active}건")
    } else if cfg.is_armed() {
        let armed = cfg.rules.iter().filter(|r| r.enabled).count();
        format!("감시 중 · 규칙 {armed}")
    } else {
        "꺼짐".into()
    }
}

pub(super) fn controls(ui: &mut egui::Ui, app: &mut ThermalApp) {
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
                // Wrapped: a row wider than the panel would widen everything under it.
                ui.horizontal_wrapped(|ui| {
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
                    ui.label("−");
                    egui::ComboBox::from_id_salt("minus")
                        .width(72.0)
                        .selected_text(r.minus.map_or("없음".into(), |s| s.label(shapes)))
                        .show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut r.minus, None, "없음").changed();
                            for s in options.iter().filter(|s| **s != r.source) {
                                changed |= ui
                                    .selectable_value(&mut r.minus, Some(*s), s.label(shapes))
                                    .changed();
                            }
                        })
                        .response
                        .on_hover_text("다른 측정을 고르면 두 값의 차(왼쪽 − 오른쪽)를 감시합니다");
                });
                if r.minus == Some(r.source) {
                    r.minus = None;
                }
                let before = r.condition;
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("condition")
                        .width(60.0)
                        .selected_text(r.condition.label())
                        .show_ui(ui, |ui| {
                            for c in Condition::ALL {
                                changed |= ui
                                    .selectable_value(&mut r.condition, c, c.label())
                                    .changed();
                            }
                        });
                    if r.condition == Condition::Outside
                        && before != Condition::Outside
                        && r.upper <= r.threshold
                    {
                        // Rules saved before ranges existed have no upper limit yet.
                        r.upper = r.threshold + 10.0;
                    }
                    // The field clamps what it shows: a difference can be well below −40.
                    let limits = if r.minus.is_some() {
                        -640.0..=640.0
                    } else {
                        -40.0..=600.0
                    };
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut r.threshold)
                                .speed(0.1)
                                .range(limits.clone())
                                .suffix(" °C"),
                        )
                        .changed();
                    if r.condition == Condition::Outside {
                        ui.label("~");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut r.upper)
                                    .speed(0.1)
                                    .range(limits)
                                    .suffix(" °C"),
                            )
                            .on_hover_text("이 범위를 벗어나면 경보")
                            .changed();
                    }
                });
                ui.horizontal(|ui| {
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
        changed |= ui
            .checkbox(&mut cfg.snapshot_on_raise, "경보 발생 시 스냅샷 저장")
            .on_hover_text("경보가 발생한 순간의 영상을 저장 폴더에 스냅샷으로 남깁니다")
            .changed();
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .checkbox(&mut cfg.record_on_raise, "경보 중 MP4 녹화")
                .on_hover_text(
                    "경보가 발생하면 MP4 녹화를 시작하고, 모든 경보가 해제된 뒤 정한 시간이 \
                     지나면 끝냅니다. 이미 녹화 중이면 그 녹화를 그대로 둡니다. 발생 전 \
                     영상은 담기지 않습니다",
                )
                .changed();
            ui.add_enabled_ui(cfg.record_on_raise, |ui| {
                ui.label("해제 후");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut cfg.record_after_s)
                            .speed(0.5)
                            .range(0.0..=600.0)
                            .suffix(" 초"),
                    )
                    .changed();
            });
        });
        changed |= ui
            .checkbox(&mut cfg.log_to_file, "경보 기록을 파일로 저장")
            .on_hover_text(format!(
                "발생·해제·감시 중단·재개를 저장 폴더의 {}에 한 줄씩 덧붙입니다",
                alarm_log::FILE_NAME
            ))
            .changed();
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
    fn a_held_image_suspends_monitoring() {
        let (mut app, ctx) = rig(Some(0.01));
        update(&mut app, &ctx, &streaming(), Some(0.0));
        assert_eq!(app.alarms.monitor.suspended(), None);
        app.set_hold(true).unwrap();
        update(&mut app, &ctx, &streaming(), None);
        assert_eq!(app.alarms.monitor.suspended(), Some(Suspended::Frozen));
        assert!(app.alarms.log.back().unwrap().text.contains("화면 정지 중"));
        app.set_hold(false).unwrap();
        update(&mut app, &ctx, &streaming(), Some(5.0));
        assert_eq!(app.alarms.monitor.suspended(), None);
    }

    #[test]
    fn a_raised_alarm_can_save_a_snapshot() {
        let dir = std::env::temp_dir().join(format!("thermalview_alarm_{}", std::process::id()));
        let (mut app, ctx) = rig(Some(0.01));
        app.settings.snapshot_dir = Some(dir.clone());
        // A second rule that trips together with the first: still one picture.
        app.settings
            .alarms
            .add(Source::Center, Condition::Above, 20.0);
        app.rgba = vec![0; 160 * 120 * 4];
        let pictures = |dir: &std::path::Path| crate::storage::snapshot::list(dir).len();

        update(&mut app, &ctx, &streaming(), Some(0.0));
        update(&mut app, &ctx, &streaming(), Some(1.0));
        assert_eq!(app.alarms.log.len(), 2, "two rules raised");
        assert_eq!(pictures(&dir), 0, "off by default");

        // Clear and raise again with the option on.
        update(&mut app, &ctx, &CameraStatus::default(), None);
        app.settings.alarms.snapshot_on_raise = true;
        update(&mut app, &ctx, &streaming(), Some(2.0));
        update(&mut app, &ctx, &streaming(), Some(3.0));
        assert_eq!(pictures(&dir), 1);
        assert!(app
            .alarms
            .log
            .back()
            .unwrap()
            .text
            .starts_with("스냅샷 저장: thermal_"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_alarm_records_an_mp4_until_it_clears() {
        let dir = std::env::temp_dir().join(format!("thermalview_alarmmp4_{}", std::process::id()));
        let (mut app, ctx) = rig(Some(0.01));
        app.settings.snapshot_dir = Some(dir.clone());
        app.settings.alarms.record_on_raise = true;
        app.settings.alarms.record_after_s = 0.0;
        update(&mut app, &ctx, &streaming(), Some(0.0));
        assert!(app.video.is_none(), "not before the hold time");
        update(&mut app, &ctx, &streaming(), Some(1.0));
        assert!(app.video.is_some() && app.alarms.auto_video());
        // Still active: keeps recording.
        update(&mut app, &ctx, &streaming(), Some(2.0));
        assert!(app.video.is_some());
        // Raising the threshold restarts the rule: nothing is active, and after 0 s it ends.
        app.settings.alarms.rules[0].threshold = 50.0;
        update(&mut app, &ctx, &streaming(), Some(3.0));
        assert!(app.video.is_none() && !app.alarms.auto_video());
        assert!(app
            .alarms
            .log
            .back()
            .unwrap()
            .text
            .starts_with("MP4 녹화 종료"));

        // A recording the user started is neither taken over nor ended.
        app.settings.alarms.rules[0].threshold = 20.0;
        app.start_video().unwrap();
        update(&mut app, &ctx, &streaming(), Some(4.0));
        update(&mut app, &ctx, &streaming(), Some(5.0));
        app.settings.alarms.rules[0].threshold = 50.0;
        update(&mut app, &ctx, &streaming(), Some(6.0));
        assert!(app.video.is_some() && !app.alarms.auto_video());
        let _ = app.stop_video();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn events_go_to_the_alarm_file_when_asked() {
        let dir =
            std::env::temp_dir().join(format!("thermalview_alarmfile_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let (mut app, ctx) = rig(Some(0.01));
        app.settings.snapshot_dir = Some(dir.clone());
        let file = dir.join(alarm_log::FILE_NAME);
        update(&mut app, &ctx, &streaming(), Some(0.0));
        update(&mut app, &ctx, &streaming(), Some(1.0));
        assert!(!file.exists(), "off by default");

        app.settings.alarms.log_to_file = true;
        update(&mut app, &ctx, &CameraStatus::default(), None);
        update(&mut app, &ctx, &streaming(), Some(2.0));
        update(&mut app, &ctx, &streaming(), Some(3.0));
        let text = std::fs::read_to_string(&file).unwrap();
        let events: Vec<&str> = text
            .lines()
            .skip(1)
            .map(|l| l.split(',').nth(1).unwrap())
            .collect();
        assert_eq!(events, ["paused", "resumed", "raised"]);
        assert!(
            text.contains(",raised,화면 최고 20.0 °C 이상,26.85"),
            "{text}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The settings panel starts 300 points wide: a rule with every field shown (a range, a
    /// difference of two long names) and the options under it must fit without widening it.
    #[test]
    fn the_rule_editor_fits_the_panel() {
        let (mut app, _) = rig(Some(0.01));
        let ctx = egui::Context::default();
        let mut first = ctx.run_ui(egui::RawInput::default(), |_| {});
        first.textures_delta.clear();
        let b = app
            .settings
            .shapes
            .add(crate::thermal::shapes::ShapeKind::Box, (1, 1), (9, 9))
            .unwrap();
        let s = app.settings.spots.add(5, 5).unwrap();
        let cfg = &mut app.settings.alarms;
        cfg.rules[0].source = Source::Shape(b, Metric::Mean);
        cfg.rules[0].minus = Some(Source::Spot(s));
        cfg.rules[0].condition = Condition::Outside;
        cfg.rules[0].threshold = -123.4;
        cfg.rules[0].upper = 456.7;
        cfg.record_on_raise = true;
        const WIDTH: f32 = 280.0;
        let mut used = 0.0;
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let r = ui.allocate_ui(egui::vec2(WIDTH, 2000.0), |ui| {
                    ui.set_max_width(WIDTH);
                    controls(ui, &mut app);
                    ui.min_rect().width()
                });
                used = r.inner;
            });
            out.textures_delta.clear();
        }
        assert!(used <= WIDTH + 0.5, "the editor is {used} wide");
        // Not clamped to the −40 °C a plain temperature allows.
        assert_eq!(app.settings.alarms.rules[0].threshold, -123.4);
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
