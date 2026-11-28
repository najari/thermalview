//! Remote control: runs the TCP server according to the settings and carries out its commands
//! on the UI thread, so they see and change exactly the state the window shows.

use eframe::egui::{self, Color32};

use super::{calibration, view, webcam, Hold, ThermalApp};
use crate::camera::{CameraStatus, ConnState, HEIGHT, WIDTH};
use crate::remote::server::{Pending, Reply, Server};
use crate::remote::{self as proto, DiffSet, IsoSet, RangeSet, Request};
use crate::settings::RemoteSettings;
use crate::thermal::alarm::Suspended;
use crate::thermal::calibration::MIN_EMISSIVITY;
use crate::thermal::frame::ThermalFrame;
use crate::thermal::isotherm::Band;
use crate::thermal::measure::Spot;
use crate::thermal::range::RangeMode;

const RED: Color32 = Color32::from_rgb(220, 60, 60);
const AMBER: Color32 = Color32::from_rgb(235, 170, 30);

#[derive(Default)]
pub(super) struct RemoteState {
    server: Option<Server>,
    /// Settings the server was last started (or failed to start) with.
    applied: Option<RemoteSettings>,
    error: Option<String>,
}

impl RemoteState {
    pub(super) fn stop(&mut self) {
        self.server = None;
    }
}

/// Start, stop or restart the server when its settings change.
pub(super) fn sync(app: &mut ThermalApp, ctx: &egui::Context) {
    let want = app.settings.remote;
    if app.remote.applied == Some(want) {
        return;
    }
    app.remote.applied = Some(want);
    // Release the old port before binding again.
    app.remote.server = None;
    app.remote.error = None;
    if !want.enabled {
        return;
    }
    let ctx = ctx.clone();
    match Server::start(want.port, want.allow_lan, move || ctx.request_repaint()) {
        Ok(s) => app.remote.server = Some(s),
        Err(e) => {
            log::warn!("remote control: cannot listen on port {}: {e}", want.port);
            let msg = format!("포트 {}을(를) 열 수 없습니다: {e}", want.port);
            // Also in the status line: the panel section may be folded.
            app.notify(format!("원격 제어: {msg}"));
            app.remote.error = Some(msg);
        }
    }
}

/// Answer every command that is waiting.
pub(super) fn process(app: &mut ThermalApp, ctx: &egui::Context, status: &CameraStatus) {
    while let Some(p) = app.remote.server.as_ref().and_then(Server::try_recv) {
        if p.request == Request::Screenshot {
            screenshot(app, ctx, p);
            continue;
        }
        let r = execute(app, status, p.request);
        match &r {
            Ok(_) => log::debug!("remote {}: {:?}", p.peer, p.request),
            Err(e) => log::info!("remote {}: {:?} failed: {e}", p.peer, p.request),
        }
        p.respond(r);
    }
}

/// `SCREENSHOT`: answered once the next drawn frame has been captured and saved. A minimized
/// window is not drawn, so the client then gets the server's "not responding" answer.
fn screenshot(app: &ThermalApp, ctx: &egui::Context, p: Pending) {
    let dir = app.settings.snapshot_dir();
    ctx.request_screenshot(move |image| p.respond(save_window(&dir, &image)));
}

fn save_window(dir: &std::path::Path, image: &egui::ColorImage) -> Reply {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let name = chrono::Local::now()
        .format("window_%Y%m%d_%H%M%S_%3f.png")
        .to_string();
    let path = dir.join(name);
    image::save_buffer(
        &path,
        image.as_raw(),
        image.width() as u32,
        image.height() as u32,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| format!("cannot write the picture: {e}"))?;
    Ok(path.display().to_string())
}

fn frame(app: &ThermalApp) -> Result<&ThermalFrame, String> {
    app.frame.as_ref().ok_or_else(|| "no frame yet".to_string())
}

/// Image size, from the current frame or the Lepton default before the first one.
fn dims(app: &ThermalApp) -> (usize, usize) {
    app.frame
        .as_ref()
        .map_or((WIDTH, HEIGHT), |f| (f.width, f.height))
}

fn check_pixel(app: &ThermalApp, x: usize, y: usize) -> Result<(), String> {
    let (w, h) = dims(app);
    if x < w && y < h {
        Ok(())
    } else {
        Err(format!("pixel ({x}, {y}) is outside the {w}x{h} image"))
    }
}

fn num(v: f32) -> String {
    format!("{v:.2}")
}

fn unit_token(f: &ThermalFrame) -> &'static str {
    if f.is_radiometric() {
        "C"
    } else {
        "cnt"
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// Carry out one command against the application state.
pub(super) fn execute(app: &mut ThermalApp, status: &CameraStatus, req: Request) -> Reply {
    match req {
        Request::Help => Ok(proto::HELP.into()),
        Request::Quit => Ok("bye".into()),
        Request::Status => Ok(status_line(app, status)),
        Request::Measure => measure(app),
        Request::Temp { x, y } => {
            check_pixel(app, x, y)?;
            Ok(num(frame(app)?.value_around(x, y, Spot::RADIUS)))
        }
        Request::SpotList => Ok(app
            .settings
            .spots
            .items
            .iter()
            .map(|s| format!("S{}={},{}", s.id, s.x, s.y))
            .collect::<Vec<_>>()
            .join(" ")),
        Request::SpotAdd { x, y } => {
            check_pixel(app, x, y)?;
            let id = app
                .settings
                .spots
                .add(x, y)
                .ok_or("spot limit reached (16)")?;
            app.mark_dirty();
            app.notify(format!("원격: 측정점 S{id} ({x}, {y}) 추가"));
            Ok(format!("id={id}"))
        }
        Request::SpotDel(id) => {
            if !app.settings.spots.items.iter().any(|s| s.id == id) {
                return Err(format!("no spot S{id}"));
            }
            app.settings.spots.remove(id);
            app.mark_dirty();
            app.notify(format!("원격: 측정점 S{id} 삭제"));
            Ok(String::new())
        }
        Request::SpotClear => {
            // Not `Spots::clear`: that restarts the numbering, and a new spot must never
            // inherit an alarm rule or reference written for a deleted one.
            app.settings.spots.items.clear();
            app.mark_dirty();
            app.notify("원격: 측정점 모두 삭제");
            Ok(String::new())
        }
        Request::Palette(None) => Ok(proto::palette_name(app.settings.palette)),
        Request::Palette(Some(p)) => {
            app.settings.palette = p;
            app.mark_dirty();
            app.render();
            Ok(proto::palette_name(p))
        }
        Request::Range(None) => Ok(range_line(app)),
        Request::Range(Some(r)) => {
            app.settings.range = match r {
                RangeSet::Auto => {
                    app.auto_range.reset();
                    RangeMode::Auto
                }
                RangeSet::Robust => {
                    app.auto_range.reset();
                    RangeMode::Robust
                }
                RangeSet::Manual { lo, hi } => RangeMode::Manual { lo, hi },
            };
            app.mark_dirty();
            app.render();
            Ok(range_line(app))
        }
        Request::Rotate(None) => Ok(proto::rotation_degrees(app.settings.rotation).to_string()),
        Request::Rotate(Some(r)) => {
            app.set_rotation(r).map_err(|why| why.to_string())?;
            Ok(proto::rotation_degrees(r).to_string())
        }
        Request::Flip(None) => Ok(proto::flip_name(app.settings.flip).into()),
        Request::Flip(Some(f)) => {
            app.set_flip(f).map_err(|why| why.to_string())?;
            Ok(proto::flip_name(f).into())
        }
        Request::Emissivity(None) => Ok(format!("{:.3}", app.settings.calibration.emissivity)),
        Request::Emissivity(Some(e)) => {
            if app.csv.is_some() {
                return Err("calibration cannot change while CSV logging runs".into());
            }
            if !(MIN_EMISSIVITY..=1.0).contains(&e) {
                return Err(format!("emissivity must be {MIN_EMISSIVITY:.2} to 1.00"));
            }
            app.settings.calibration.emissivity = e;
            app.mark_dirty();
            calibration::reapply(app);
            app.notify(format!("원격: 방사율 {e:.2}"));
            Ok(format!("{e:.3}"))
        }
        Request::Alarms => Ok(alarms_line(app)),
        Request::Snapshot => app.take_snapshot(status).map(|p| p.display().to_string()),
        Request::Csv(true) => app.start_csv().map(|p| p.display().to_string()),
        Request::Csv(false) => app
            .stop_csv()
            .map(|(p, rows)| format!("rows={rows} {}", p.display())),
        Request::Record(true) => app.start_seq(status).map(|p| p.display().to_string()),
        Request::Record(false) => app
            .stop_seq()
            .map(|(p, n)| format!("frames={n} {}", p.display())),
        Request::Video(true) => app.start_video().map(|p| p.display().to_string()),
        Request::Video(false) => app
            .stop_video()
            .map(|(p, n)| format!("frames={n} {}", p.display())),
        Request::Frame => {
            let f = frame(app)?;
            let bytes: Vec<u8> = f.raw.iter().flat_map(|v| v.to_le_bytes()).collect();
            let k = f.kelvin_per_count.map_or("none".into(), |k| k.to_string());
            Ok(format!(
                "w={} h={} seq={} k={k} data={}",
                f.width,
                f.height,
                f.sequence,
                proto::base64(&bytes)
            ))
        }
        Request::Align => webcam::auto_align_now(app),
        Request::Freeze(None) => Ok(on_off(app.hold.is_some()).into()),
        Request::Freeze(Some(on)) => {
            app.set_hold(on)?;
            Ok(on_off(app.hold.is_some()).into())
        }
        Request::Isotherm(None) => Ok(isotherm_line(app)),
        Request::Isotherm(Some(set)) => {
            let iso = &mut app.settings.isotherm;
            iso.enabled = set != IsoSet::Off;
            match set {
                IsoSet::Off => {}
                IsoSet::Above(t) => (iso.band, iso.lo) = (Band::Above, t),
                IsoSet::Below(t) => (iso.band, iso.hi) = (Band::Below, t),
                IsoSet::Between(lo, hi) => (iso.band, iso.lo, iso.hi) = (Band::Between, lo, hi),
            }
            app.mark_dirty();
            app.render();
            Ok(isotherm_line(app))
        }
        Request::Diff(None) => Ok(diff_line(app)),
        Request::Diff(Some(set)) => {
            match set {
                DiffSet::On => app.set_diff(true)?,
                DiffSet::Off => app.set_diff(false)?,
                DiffSet::Span(span) => {
                    app.settings.diff_span = span.unwrap_or(0.0);
                    app.auto_range.reset();
                    app.mark_dirty();
                    app.render();
                }
            }
            Ok(diff_line(app))
        }
        Request::Filter(None) => Ok(app.settings.noise_filter.max(1).to_string()),
        Request::Filter(Some(n)) => {
            app.settings.noise_filter = n;
            app.mark_dirty();
            Ok(n.to_string())
        }
        Request::Zoom(None) => Ok(zoom_line(app)),
        Request::Zoom(Some(z)) => {
            let (w, h) = dims(app);
            let (w, h) = (w as f32, h as f32);
            let center = match z.center {
                Some((x, y)) => {
                    check_pixel(app, x, y)?;
                    (x as f32 + 0.5, y as f32 + 0.5)
                }
                None => app.zoom.center(w, h),
            };
            app.zoom = view::Zoom::centered(z.factor, center, w, h);
            Ok(zoom_line(app))
        }
        // Answered by `process`, which has the window to capture.
        Request::Screenshot => Err("no window to capture".into()),
    }
}

fn status_line(app: &ThermalApp, status: &CameraStatus) -> String {
    let state = match status.state {
        ConnState::Starting => "starting",
        ConnState::Searching => "searching",
        ConnState::Busy => "busy",
        ConnState::Streaming => "streaming",
        ConnState::Stalled => "stalled",
        ConnState::NeedsReplug => "replug",
    };
    let fpa = status.fpa_c.map_or("none".into(), num);
    let unit = app.frame.as_ref().map_or("none", unit_token);
    let cal = app.frame.as_ref().is_some_and(|f| f.calibration.is_some());
    let monitor = app.alarms.monitor();
    let active = app
        .settings
        .alarms
        .rules
        .iter()
        .filter(|r| monitor.is_active(r.id))
        .count();
    format!(
        "state={state} fps={:.1} frames={} fpa={fpa} unit={unit} source={} csv={} rec={} mp4={} \
         cal={} alarms={active}",
        status.fps,
        status.frames,
        match (&app.player, &app.hold) {
            (Some(_), _) => "playback",
            (None, Some(Hold::Frozen)) => "frozen",
            (None, Some(Hold::Snapshot(_))) => "snapshot",
            (None, None) => "live",
        },
        on_off(app.csv.is_some()),
        on_off(app.seq.is_some()),
        on_off(app.video.is_some()),
        on_off(cal),
    )
}

fn measure(app: &ThermalApp) -> Reply {
    let (f, st) = match (&app.frame, &app.stats) {
        (Some(f), Some(st)) => (f, st),
        _ => return Err("no frame yet".into()),
    };
    let time = chrono::DateTime::<chrono::Local>::from(f.captured)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
    let mut out = format!(
        "seq={} time={time} unit={} max={} min={} mean={} center={} maxpos={},{} minpos={},{}",
        f.sequence,
        unit_token(f),
        num(f.value(st.max)),
        num(f.value(st.min)),
        num(f.value_f(st.mean)),
        num(f.value_around(f.width / 2, f.height / 2, Spot::RADIUS)),
        st.max_pos.0,
        st.max_pos.1,
        st.min_pos.0,
        st.min_pos.1,
    );
    for s in &app.settings.spots.items {
        if let Some(v) = s.value(f) {
            out.push_str(&format!(" S{}={}", s.id, num(v)));
        }
    }
    for s in &app.settings.shapes.items {
        if let Some(st) = s.stats(f) {
            let l = s.label();
            out.push_str(&format!(
                " {l}.max={} {l}.min={} {l}.mean={}",
                num(st.max),
                num(st.min),
                num(st.mean)
            ));
        }
    }
    Ok(out)
}

/// "off", "above 60.00", "below 10.00" or "between 30.00 35.00", then the share of the image
/// inside the band.
fn isotherm_line(app: &ThermalApp) -> String {
    let iso = &app.settings.isotherm;
    if !iso.enabled {
        return "off".into();
    }
    let band = match iso.band {
        Band::Above => format!("above {}", num(iso.lo)),
        Band::Below => format!("below {}", num(iso.hi)),
        Band::Between => format!(
            "between {} {}",
            num(iso.lo.min(iso.hi)),
            num(iso.lo.max(iso.hi))
        ),
    };
    match app.iso_share {
        Some(share) => format!("{band} pixels={:.1}%", share * 100.0),
        None => band,
    }
}

/// "off", or "on span=2.00 min=-0.42 max=3.20 minpos=x,y maxpos=x,y ref=14:32:10": the color
/// span, the largest drop and rise since the reference frame and where they are, and when the
/// reference was taken.
fn diff_line(app: &ThermalApp) -> String {
    match (&app.diff, app.diff_stats) {
        (Some(d), Some(st)) => format!(
            "on span={} min={} max={} minpos={},{} maxpos={},{} ref={}",
            num(app.display_range.1),
            num(st.min),
            num(st.max),
            st.min_pos.0,
            st.min_pos.1,
            st.max_pos.0,
            st.max_pos.1,
            d.taken
        ),
        _ => "off".into(),
    }
}

/// Magnification and the thermal pixel in the middle of the view.
fn zoom_line(app: &ThermalApp) -> String {
    let (w, h) = dims(app);
    let (x, y) = app.zoom.center(w as f32, h as f32);
    format!(
        "{:.2} center={:.0},{:.0}",
        app.zoom.factor,
        x.floor(),
        y.floor()
    )
}

fn range_line(app: &ThermalApp) -> String {
    let mode = match app.settings.range {
        RangeMode::Auto => "auto",
        RangeMode::Robust => "robust",
        RangeMode::Manual { .. } => "manual",
    };
    let (lo, hi) = app.display_range;
    format!("{mode} {} {}", num(lo), num(hi))
}

fn alarms_line(app: &ThermalApp) -> String {
    let cfg = &app.settings.alarms;
    let m = app.alarms.monitor();
    let monitoring = match m.suspended() {
        None => "on",
        Some(Suspended::Playback) => "playback",
        Some(Suspended::Frozen) => "frozen",
        Some(Suspended::NoSignal) => "nosignal",
        Some(Suspended::NotRadiometric) => "counts",
    };
    let mut out = format!("enabled={} monitoring={monitoring}", on_off(cfg.enabled));
    if app.alarms.auto_video() {
        out.push_str(" mp4=alarm");
    }
    for r in &cfg.rules {
        let state = if !r.enabled {
            "off"
        } else if m.is_active(r.id) {
            "active"
        } else if m.is_pending(r.id) {
            "pending"
        } else {
            "clear"
        };
        out.push_str(&format!(" R{}={state}", r.id));
    }
    out
}

/// Settings panel section.
/// One-line state for the header of the panel section.
pub(super) fn summary(app: &ThermalApp) -> String {
    if !app.settings.remote.enabled {
        return "꺼짐".into();
    }
    match (&app.remote.server, &app.remote.error) {
        (Some(s), _) => format!("포트 {} · 연결 {}", s.addr().port(), s.clients()),
        (None, Some(_)) => "오류".into(),
        (None, None) => "시작 중".into(),
    }
}

pub(super) fn controls(ui: &mut egui::Ui, app: &mut ThermalApp) {
    let before = app.settings.remote;
    let r = &mut app.settings.remote;
    ui.checkbox(&mut r.enabled, "TCP 원격 제어 사용")
        .on_hover_text("다른 프로그램이 TCP로 측정값을 읽고 스냅샷·기록을 시작할 수 있습니다");
    ui.add_enabled_ui(!r.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label("포트");
            ui.add(egui::DragValue::new(&mut r.port).range(1..=65535));
        });
        ui.checkbox(&mut r.allow_lan, "다른 PC에서 접속 허용");
    });
    let (enabled, allow_lan) = (r.enabled, r.allow_lan);
    if app.settings.remote != before {
        app.mark_dirty();
    }
    if allow_lan {
        ui.colored_label(
            AMBER,
            "인증이 없습니다. 같은 네트워크의 누구나 측정값을 읽고 \
             스냅샷·기록을 시작할 수 있습니다.",
        );
    }
    if !enabled {
        return;
    }
    match (&app.remote.server, &app.remote.error) {
        (Some(s), _) => {
            let addr = s.addr();
            let place = if addr.ip().is_unspecified() {
                format!("모든 네트워크 주소의 포트 {}", addr.port())
            } else {
                addr.to_string()
            };
            ui.label(format!("{place}에서 대기 중 · 연결 {}", s.clients()));
            ui.label("명령 예: MEAS, SNAPSHOT. 목록은 HELP");
        }
        (None, Some(e)) => {
            ui.colored_label(RED, e);
        }
        (None, None) => {
            ui.label("시작 중…");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::settings::Settings;
    use crate::thermal::palette::Palette;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    /// 26.85 °C everywhere except one 36.85 °C pixel at (100, 40).
    fn rig() -> ThermalApp {
        let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        let mut raw = vec![30000; 160 * 120];
        raw[40 * 160 + 100] = 31000;
        let mut f = ThermalFrame::new(160, 120, raw, Some(0.01));
        f.sequence = 7;
        app.stats = Some(f.stats());
        app.frame = Some(f);
        app
    }

    fn run(app: &mut ThermalApp, cmd: &str) -> Reply {
        let req = proto::parse(cmd)?;
        execute(app, &CameraStatus::default(), req)
    }

    #[test]
    fn measurements_and_spots() {
        let mut app = rig();
        let m = run(&mut app, "MEAS").unwrap();
        assert!(m.starts_with("seq=7 time="), "{m}");
        assert!(m.contains(" unit=C max=36.85 min=26.85 "), "{m}");
        assert!(m.contains(" center=26.85 maxpos=100,40 "), "{m}");
        assert_eq!(run(&mut app, "TEMP 0 0").unwrap(), "26.85");
        assert!(run(&mut app, "TEMP 160 0").is_err());

        assert_eq!(run(&mut app, "SPOT ADD 100 40").unwrap(), "id=1");
        assert_eq!(run(&mut app, "SPOT ADD 5 6").unwrap(), "id=2");
        assert!(run(&mut app, "SPOT ADD 5 120").is_err());
        assert_eq!(run(&mut app, "SPOT").unwrap(), "S1=100,40 S2=5,6");
        // 3×3 mean around the hot pixel: (8 × 26.85 + 36.85) / 9.
        assert!(run(&mut app, "MEAS")
            .unwrap()
            .ends_with(" S1=27.96 S2=26.85"));
        run(&mut app, "SPOT DEL 1").unwrap();
        assert!(run(&mut app, "SPOT DEL 1").is_err());
        run(&mut app, "SPOT CLEAR").unwrap();
        assert_eq!(run(&mut app, "SPOT").unwrap(), "");
        // Numbering continues after CLEAR, so old ids never come back.
        assert_eq!(run(&mut app, "SPOT ADD 1 1").unwrap(), "id=3");
        assert!(app.settings_dirty.is_some());
    }

    #[test]
    fn display_and_calibration_settings() {
        let mut app = rig();
        assert_eq!(run(&mut app, "PALETTE").unwrap(), "ironbow");
        assert_eq!(run(&mut app, "PALETTE blackhot").unwrap(), "blackhot");
        assert_eq!(app.settings.palette, Palette::BlackHot);
        assert!(app.texture_dirty, "image recolored");

        assert_eq!(run(&mut app, "RANGE 20 30").unwrap(), "manual 20.00 30.00");
        assert_eq!(app.settings.range, RangeMode::Manual { lo: 20.0, hi: 30.0 });
        assert!(run(&mut app, "RANGE AUTO").unwrap().starts_with("auto "));

        assert_eq!(run(&mut app, "EMISSIVITY 0.95").unwrap(), "0.950");
        assert!(app.frame.as_ref().unwrap().calibration.is_some());
        assert!(run(&mut app, "STATUS").unwrap().contains(" cal=on "));
        assert!(run(&mut app, "EMISSIVITY 0.05").is_err());
        assert!(run(&mut app, "EMISSIVITY 1.5").is_err());
        assert_eq!(run(&mut app, "EMISSIVITY").unwrap(), "0.950");
    }

    #[test]
    fn view_commands() {
        let mut app = rig();
        // One pixel of 19,200 is 10 K hotter: 36.85 °C in a 26.85 °C image.
        assert_eq!(run(&mut app, "ISOTHERM").unwrap(), "off");
        assert_eq!(
            run(&mut app, "ISOTHERM ABOVE 30").unwrap(),
            "above 30.00 pixels=0.0%"
        );
        assert_eq!(app.iso_share, Some(1.0 / 19200.0));
        assert_eq!(
            run(&mut app, "ISOTHERM BELOW 30").unwrap(),
            "below 30.00 pixels=100.0%"
        );
        assert!(run(&mut app, "ISOTHERM BETWEEN 20 30")
            .unwrap()
            .starts_with("between 20.00 30.00 "));
        assert_eq!(run(&mut app, "ISOTHERM OFF").unwrap(), "off");
        assert!(app.iso_share.is_none());

        assert!(run(&mut app, "RANGE ROBUST")
            .unwrap()
            .starts_with("robust 25.85 27.85"));
        assert_eq!(app.settings.range, RangeMode::Robust);

        assert_eq!(run(&mut app, "FILTER").unwrap(), "1");
        assert_eq!(run(&mut app, "FILTER 8").unwrap(), "8");
        assert_eq!(app.settings.noise_filter, 8);
        assert_eq!(run(&mut app, "FILTER OFF").unwrap(), "1");

        assert_eq!(run(&mut app, "ZOOM").unwrap(), "1.00 center=80,60");
        assert_eq!(
            run(&mut app, "ZOOM 4 100 40").unwrap(),
            "4.00 center=100,40"
        );
        // Only the magnification: the middle of the view stays.
        assert_eq!(run(&mut app, "ZOOM 2").unwrap(), "2.00 center=100,40");
        // A corner cannot be in the middle.
        assert_eq!(run(&mut app, "ZOOM 2 0 0").unwrap(), "2.00 center=40,30");
        assert!(run(&mut app, "ZOOM 2 160 0").is_err());
        assert_eq!(run(&mut app, "ZOOM 1").unwrap(), "1.00 center=80,60");
    }

    #[test]
    fn difference_image_commands() {
        let mut app = rig();
        assert_eq!(run(&mut app, "DIFF").unwrap(), "off");
        let on = run(&mut app, "DIFF ON").unwrap();
        let start = "on span=1.00 min=0.00 max=0.00 minpos=0,0 maxpos=0,0 ref=";
        assert!(on.starts_with(start), "{on}");
        // The hot pixel cools down by 4 K.
        let f = app.frame.as_mut().unwrap();
        f.raw[40 * 160 + 100] = 30600;
        let st = f.stats();
        app.stats = Some(st);
        app.render();
        let line = run(&mut app, "DIFF").unwrap();
        assert!(
            line.contains(" min=-4.00 max=0.00 minpos=100,40 "),
            "{line}"
        );
        assert!(run(&mut app, "DIFF SPAN 5")
            .unwrap()
            .starts_with("on span=5.00 "));
        assert_eq!(app.settings.diff_span, 5.0);
        assert!(run(&mut app, "DIFF SPAN AUTO")
            .unwrap()
            .starts_with("on span=4.00 "));
        // Temperatures are still what is measured.
        assert!(run(&mut app, "MEAS").unwrap().contains(" max=32.85 "));
        assert_eq!(run(&mut app, "DIFF OFF").unwrap(), "off");
        assert!(run(&mut app, "RANGE").unwrap().starts_with("auto 26.85 "));

        let mut empty = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        assert!(run(&mut empty, "DIFF ON").is_err());
        assert_eq!(run(&mut empty, "DIFF OFF").unwrap(), "off");
    }

    #[test]
    fn freezing_holds_the_frame() {
        let mut app = rig();
        assert_eq!(run(&mut app, "FREEZE").unwrap(), "off");
        assert_eq!(run(&mut app, "FREEZE ON").unwrap(), "on");
        assert!(run(&mut app, "STATUS").unwrap().contains(" source=frozen "));
        // The held frame is still measured; nothing can be recorded from it.
        assert!(run(&mut app, "MEAS").unwrap().starts_with("seq=7 "));
        assert!(run(&mut app, "CSV START")
            .unwrap_err()
            .contains("FREEZE OFF"));
        assert!(run(&mut app, "REC START").is_err());
        assert!(run(&mut app, "MP4 START").is_err());
        assert_eq!(
            run(&mut app, "FREEZE ON").unwrap(),
            "on",
            "asking twice is fine"
        );
        assert_eq!(run(&mut app, "FREEZE OFF").unwrap(), "off");
        assert!(run(&mut app, "STATUS").unwrap().contains(" source=live "));

        let mut empty = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        assert!(run(&mut empty, "FREEZE ON").is_err());
        assert_eq!(run(&mut empty, "FREEZE OFF").unwrap(), "off");
    }

    #[test]
    fn rotation_turns_the_image_and_moves_the_spots() {
        let mut app = rig();
        run(&mut app, "SPOT ADD 100 40").unwrap();
        assert_eq!(run(&mut app, "ROTATE").unwrap(), "0");
        assert_eq!(run(&mut app, "ROTATE 90").unwrap(), "90");
        let f = app.frame.as_ref().unwrap();
        assert_eq!((f.width, f.height), (120, 160));
        // Clockwise: (x, y) of a 160 × 120 image → (119 − y, x).
        let s = app.settings.spots.items[0];
        assert_eq!((s.x, s.y), (79, 100));
        assert_eq!(f.at(79, 100), 31000, "the hot pixel moved with the spot");
        assert!(run(&mut app, "FRAME").unwrap().starts_with("w=120 h=160 "));
        // Back to upright by another 270°: everything returns.
        assert_eq!(run(&mut app, "ROTATE 0").unwrap(), "0");
        let s = app.settings.spots.items[0];
        assert_eq!((s.x, s.y), (100, 40));
        assert_eq!(app.frame.as_ref().unwrap().width, 160);
    }

    #[test]
    fn frame_is_raw_counts_in_base64() {
        let mut app = rig();
        let r = run(&mut app, "FRAME").unwrap();
        let data = r.strip_prefix("w=160 h=120 seq=7 k=0.01 data=").unwrap();
        assert_eq!(data.len(), 160 * 120 * 2 / 3 * 4);
        // 30000 = 0x7530 → bytes 30 75 30 75 30 75 → "MHUwdTB1".
        assert!(data.starts_with("MHUwdTB1"));
    }

    #[test]
    fn files_and_locks() {
        let dir = std::env::temp_dir().join(format!("thermalview_remote_{}", std::process::id()));
        let mut app = rig();
        app.settings.snapshot_dir = Some(dir.clone());
        app.render();

        let png = run(&mut app, "SNAPSHOT").unwrap();
        assert!(std::path::Path::new(&png).exists(), "{png}");

        let csv = run(&mut app, "CSV START").unwrap();
        assert!(run(&mut app, "CSV START").is_err());
        assert!(run(&mut app, "STATUS").unwrap().contains(" csv=on "));
        // Calibration and rotation are frozen while a CSV runs, as in the window.
        assert!(run(&mut app, "EMISSIVITY 0.9").is_err());
        assert!(run(&mut app, "ROTATE 90").is_err());
        app.record(0.0);
        let done = run(&mut app, "CSV STOP").unwrap();
        assert_eq!(done, format!("rows=1 {csv}"));
        assert!(run(&mut app, "CSV STOP").is_err());

        let seq = run(&mut app, "REC START").unwrap();
        assert!(seq.ends_with(".tvseq"), "{seq}");
        assert!(run(&mut app, "REC STOP").unwrap().starts_with("frames=0 "));

        let mp4 = run(&mut app, "MP4 START").unwrap();
        assert!(mp4.ends_with(".mp4"), "{mp4}");
        assert!(run(&mut app, "STATUS").unwrap().contains(" mp4=on "));
        // No frame arrived while recording: no file, and the client is told so.
        let stop = run(&mut app, "MP4 STOP").unwrap_err();
        assert!(stop.starts_with("no frames"), "{stop}");
        assert!(!std::path::Path::new(&mp4).exists());
        assert_eq!(run(&mut app, "MP4 STOP"), Err("not recording MP4".into()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_frame_is_an_error_not_a_panic() {
        let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        for cmd in [
            "MEAS",
            "TEMP 1 1",
            "FRAME",
            "SNAPSHOT",
            "CSV START",
            "REC START",
            "MP4 START",
        ] {
            assert_eq!(run(&mut app, cmd), Err("no frame yet".into()), "{cmd}");
        }
        let s = run(&mut app, "STATUS").unwrap();
        assert!(s.starts_with("state=starting fps=0.0 frames=0 fpa=none unit=none source=live"));
        assert_eq!(
            run(&mut app, "ALARMS").unwrap(),
            "enabled=on monitoring=nosignal"
        );
    }

    /// The whole settings panel draws with the server running, and with a port that is taken.
    #[test]
    fn panel_shows_listening_and_port_errors() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ctx = egui::Context::default();
        let draw = |app: &mut ThermalApp| {
            let status = CameraStatus::default();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.controls(ui, &status));
            out.textures_delta.clear();
        };
        let mut app = rig();
        app.settings.remote = RemoteSettings {
            enabled: true,
            port: 0,
            allow_lan: true,
        };
        app.tick(&ctx);
        let addr = app.remote.server.as_ref().unwrap().addr();
        assert!(
            addr.ip().is_unspecified(),
            "LAN access listens on every address"
        );
        draw(&mut app);

        app.settings.remote = RemoteSettings {
            enabled: true,
            port: taken.local_addr().unwrap().port(),
            allow_lan: false,
        };
        app.tick(&ctx);
        assert!(app.remote.server.is_none());
        assert!(app.remote.error.as_deref().unwrap().starts_with("포트 "));
        draw(&mut app);
        // A failed start is not retried every pass; changing the settings retries.
        app.tick(&ctx);
        assert!(app.remote.error.is_some());
        drop(taken);
        app.settings.remote.enabled = false;
        app.tick(&ctx);
        assert!(app.remote.error.is_none());
    }

    /// Through a real socket: the server only answers while the app ticks.
    #[test]
    fn end_to_end_over_tcp() {
        let mut app = rig();
        app.settings.remote = RemoteSettings {
            enabled: true,
            port: 0,
            allow_lan: false,
        };
        let ctx = egui::Context::default();
        app.tick(&ctx);
        let addr = app.remote.server.as_ref().expect("listening").addr();

        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            c.write_all(b"SPOT ADD 100 40\nMEAS\nQUIT\n").unwrap();
            BufReader::new(c)
                .lines()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !client.is_finished() && Instant::now() < deadline {
            app.tick(&ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
        let lines = client.join().unwrap();
        assert_eq!(lines[0], "OK id=1");
        assert!(lines[1].starts_with("OK seq=7 ") && lines[1].ends_with(" S1=27.96"));
        assert_eq!(lines[2], "OK bye");

        // Turning it off frees the port.
        app.settings.remote.enabled = false;
        app.tick(&ctx);
        assert!(app.remote.server.is_none());
        assert!(TcpStream::connect(addr).is_err());
    }
}
