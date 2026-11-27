//! Webcam overlay: settings section, the capture thread's life, the screen texture, and the
//! picture the image view, MP4 and snapshots mix in.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use eframe::egui::{self, Color32};

use super::ThermalApp;
use crate::webcam::align::{self, Found};
use crate::webcam::blend::{Alignment, Mode, Overlay};
use crate::webcam::{self, Device, Picture, State, Webcam};

const RED: Color32 = Color32::from_rgb(220, 60, 60);

#[derive(Default)]
pub(super) struct WebcamState {
    cam: Option<Webcam>,
    devices: Option<Vec<Device>>,
    listing: Option<Receiver<Result<Vec<Device>, String>>>,
    list_error: Option<String>,
    texture: Option<egui::TextureHandle>,
    /// Picture number and mode the texture holds.
    uploaded: Option<(u64, Mode)>,
    /// Automatic alignment running on a helper thread.
    aligning: Option<Receiver<Option<Found>>>,
    /// Outcome of the last automatic alignment, for the panel.
    align_note: Option<(bool, String)>,
}

impl WebcamState {
    pub(super) fn stop(&mut self) {
        self.cam = None;
    }
}

/// Open or close the camera to match the settings, and collect a finished device listing.
/// Runs in `logic`.
pub(super) fn sync(app: &mut ThermalApp, ctx: &egui::Context) {
    let s = &app.settings.webcam;
    let want = if s.enabled { s.device.clone() } else { None };
    let have = app.webcam.cam.as_ref().map(|c| c.link().to_owned());
    if want != have {
        // Stop the old camera before opening another.
        app.webcam.cam = None;
        app.webcam.uploaded = None;
        if let Some(link) = want {
            let ctx = ctx.clone();
            app.webcam.cam = Some(Webcam::open(&link, move || ctx.request_repaint()));
        }
    }
    if let Some(rx) = &app.webcam.aligning {
        match rx.try_recv() {
            Ok(found) => {
                app.webcam.aligning = None;
                // The outcome is shown in the panel.
                let _ = apply_alignment(app, found);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                app.webcam.aligning = None;
                app.webcam.align_note = Some((false, "자동 정렬이 중단되었습니다".into()));
            }
        }
    }
    if let Some(rx) = &app.webcam.listing {
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("장치 목록을 읽지 못했습니다".into()),
        };
        app.webcam.listing = None;
        match result {
            Ok(list) => {
                app.webcam.devices = Some(list);
                app.webcam.list_error = None;
            }
            Err(e) => app.webcam.list_error = Some(e),
        }
    }
}

/// List the cameras on a helper thread (it takes a moment).
fn start_listing(app: &mut ThermalApp, ctx: &egui::Context) {
    if app.webcam.listing.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("webcam-list".into())
        .spawn(move || {
            let r =
                webcam::list_devices().map_err(|e| format!("장치 목록을 읽지 못했습니다: {e:#}"));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    match spawned {
        Ok(_) => app.webcam.listing = Some(rx),
        Err(e) => app.webcam.list_error = Some(format!("장치 목록을 읽지 못했습니다: {e}")),
    }
}

/// What the automatic alignment compares: the thermal image as shown (turned, if set) and the
/// newest webcam picture.
struct AlignInput {
    thermal: Vec<f32>,
    width: usize,
    height: usize,
    picture: Arc<Picture>,
}

impl AlignInput {
    fn take(app: &ThermalApp) -> Result<Self, String> {
        let (picture, _) = layer(app).ok_or("웹캠 영상이 없습니다")?;
        let f = app.frame.as_ref().ok_or("열화상 영상이 없습니다")?;
        Ok(Self {
            thermal: f.raw.iter().map(|&v| v as f32).collect(),
            width: f.width,
            height: f.height,
            picture,
        })
    }

    fn find(&self) -> Option<Found> {
        let p = &self.picture;
        align::find(
            &self.thermal,
            self.width,
            self.height,
            &p.rgba,
            p.width,
            p.height,
        )
    }
}

/// Start the automatic alignment on a helper thread (it takes up to about a second).
fn start_auto_align(app: &mut ThermalApp, ctx: &egui::Context) {
    if app.webcam.aligning.is_some() {
        return;
    }
    let input = match AlignInput::take(app) {
        Ok(i) => i,
        Err(e) => {
            app.webcam.align_note = Some((false, e));
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("webcam-align".into())
        .spawn(move || {
            let _ = tx.send(input.find());
            ctx.request_repaint();
        });
    match spawned {
        Ok(_) => {
            app.webcam.aligning = Some(rx);
            app.webcam.align_note = None;
        }
        Err(e) => {
            app.webcam.align_note = Some((false, format!("자동 정렬을 시작하지 못했습니다: {e}")))
        }
    }
}

/// Use a search result if it can be trusted; note the outcome for the panel either way.
fn apply_alignment(app: &mut ThermalApp, found: Option<Found>) -> Result<Found, String> {
    let r = match found {
        Some(f) if f.trusted() => {
            app.settings.webcam.overlay.align = f.align;
            app.mark_dirty();
            Ok(f)
        }
        Some(f) => Err(format!(
            "맞는 곳을 찾지 못해 정렬을 바꾸지 않았습니다(일치도 {:.2}). 두 카메라에 함께 보이는 \
             물체의 윤곽이 뚜렷한 장면에서 다시 해 보세요.",
            f.score
        )),
        None => Err("영상에 비교할 윤곽이 없어 정렬을 바꾸지 않았습니다.".into()),
    };
    app.webcam.align_note = Some(match &r {
        Ok(f) => (true, format!("맞췄습니다(일치도 {:.2}).", f.score)),
        Err(e) => (false, e.clone()),
    });
    r
}

/// Remote `ALIGN`: search now, on the calling (UI) thread, and use the result if trusted.
pub(super) fn auto_align_now(app: &mut ThermalApp) -> Result<String, String> {
    let found = AlignInput::take(app)?.find();
    let f = apply_alignment(app, found)?;
    Ok(format!(
        "scale={:.3} dx={:.3} dy={:.3} score={:.2}",
        f.align.scale, f.align.dx, f.align.dy, f.score
    ))
}

/// The webcam picture to mix in and how: only over live video, while the camera runs.
pub(super) fn layer(app: &ThermalApp) -> Option<(Arc<Picture>, Overlay)> {
    if app.player.is_some() || !app.settings.webcam.enabled {
        return None;
    }
    let cam = app.webcam.cam.as_ref()?;
    if !matches!(cam.state(), State::Running { .. }) {
        return None;
    }
    Some((cam.latest()?, app.settings.webcam.overlay))
}

/// Upload the newest picture (or its edges) for the screen. Only from `ui`, like the thermal
/// texture: updates made while the window is hidden would pile up.
pub(super) fn upload_texture(app: &mut ThermalApp, ctx: &egui::Context) {
    let Some((pic, ov)) = layer(app) else {
        return;
    };
    let key = (pic.seq, ov.mode);
    if app.webcam.uploaded == Some(key) && app.webcam.texture.is_some() {
        return;
    }
    let size = [pic.width, pic.height];
    let image = match ov.mode {
        Mode::Blend => egui::ColorImage::from_rgba_unmultiplied(size, &pic.rgba),
        Mode::Edges => {
            let rgba: Vec<u8> = pic.edges.iter().flat_map(|&e| [255, 255, 255, e]).collect();
            egui::ColorImage::from_rgba_unmultiplied(size, &rgba)
        }
    };
    let options = egui::TextureOptions::LINEAR;
    match &mut app.webcam.texture {
        Some(t) => t.set(image, options),
        None => app.webcam.texture = Some(ctx.load_texture("webcam", image, options)),
    }
    app.webcam.uploaded = Some(key);
}

/// What the image view draws under or over the thermal image.
pub(super) struct ViewLayer {
    pub tex: egui::TextureId,
    /// Picture rectangle in thermal pixel units.
    pub rect: [f32; 4],
    pub mode: Mode,
    pub strength: f32,
}

pub(super) fn view_layer(app: &ThermalApp) -> Option<ViewLayer> {
    let (pic, ov) = layer(app)?;
    let f = app.frame.as_ref()?;
    let tex = app.webcam.texture.as_ref()?;
    // The texture is for the other mode until the next upload.
    if app.webcam.uploaded.map(|(_, m)| m) != Some(ov.mode) {
        return None;
    }
    Some(ViewLayer {
        tex: tex.id(),
        rect: ov
            .align
            .placement((f.width, f.height), (pic.width, pic.height)),
        mode: ov.mode,
        strength: ov.strength(),
    })
}

fn percent(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

/// Settings panel section.
pub(super) fn controls(ui: &mut egui::Ui, app: &mut ThermalApp) {
    ui.heading("웹캠 합성");
    let before = app.settings.webcam.clone();
    let mut refresh = app.webcam.devices.is_none()
        && app.webcam.listing.is_none()
        && app.webcam.list_error.is_none();
    let mut retry = false;

    let s = &mut app.settings.webcam;
    ui.checkbox(&mut s.enabled, "웹캠 영상과 겹쳐 보기")
        .on_hover_text(
            "일반 카메라 영상을 열화상과 겹쳐 뜨거운 곳이 어디인지 알아보기 쉽게 합니다",
        );
    ui.horizontal(|ui| {
        let selected = if s.device_name.is_empty() {
            "장치 선택".to_owned()
        } else {
            s.device_name.clone()
        };
        egui::ComboBox::from_id_salt("webcam_device")
            .selected_text(selected)
            .width(190.0)
            .show_ui(ui, |ui| match &app.webcam.devices {
                Some(list) if list.is_empty() => {
                    ui.label("웹캠이 없습니다");
                }
                Some(list) => {
                    for d in list {
                        let on = s.device.as_deref() == Some(d.link.as_str());
                        if ui.selectable_label(on, &d.name).clicked() {
                            s.device = Some(d.link.clone());
                            s.device_name = d.name.clone();
                        }
                    }
                }
                None => {
                    ui.label("목록을 읽는 중…");
                }
            });
        if ui
            .button("⟳")
            .on_hover_text("장치 목록 새로 고침")
            .clicked()
        {
            refresh = true;
        }
    });
    if let Some(e) = &app.webcam.list_error {
        ui.colored_label(RED, e);
    }
    if s.enabled {
        match app.webcam.cam.as_ref().map(Webcam::state) {
            None => {
                ui.label("장치를 고르세요.");
            }
            Some(State::Opening) => {
                ui.label("연결 중…");
            }
            Some(State::Running { width, height }) => {
                ui.label(format!("{width}×{height} 수신 중"));
            }
            Some(State::Failed(e)) => {
                ui.colored_label(RED, e);
                retry = ui.button("다시 연결").clicked();
            }
        }
        if app.player.is_some() {
            ui.label("녹화 파일을 재생하는 동안에는 합성하지 않습니다.");
        }
    }

    let ov = &mut s.overlay;
    ui.horizontal(|ui| {
        ui.label("방식");
        for m in [Mode::Blend, Mode::Edges] {
            ui.radio_value(&mut ov.mode, m, m.label());
        }
    });
    let (value, text) = match ov.mode {
        Mode::Blend => (&mut ov.thermal_opacity, "열화상 불투명도"),
        Mode::Edges => (&mut ov.edge_strength, "윤곽 진하기"),
    };
    ui.add(
        egui::Slider::new(value, 0.0..=1.0)
            .text(text)
            .custom_formatter(|v, _| percent(v)),
    );
    ui.label("정렬");
    let a = &mut ov.align;
    ui.add(
        egui::Slider::new(&mut a.scale, Alignment::SCALE)
            .text("크기")
            .step_by(0.01)
            .custom_formatter(|v, _| format!("{v:.2}배")),
    );
    ui.add(
        egui::Slider::new(&mut a.dx, Alignment::SHIFT)
            .text("가로 위치")
            .step_by(0.002)
            .custom_formatter(|v, _| percent(v)),
    );
    ui.add(
        egui::Slider::new(&mut a.dy, Alignment::SHIFT)
            .text("세로 위치")
            .step_by(0.002)
            .custom_formatter(|v, _| percent(v)),
    );
    let mut auto = false;
    ui.horizontal(|ui| {
        let busy = app.webcam.aligning.is_some();
        let can = s.enabled && !busy && app.player.is_none();
        auto = ui
            .add_enabled(can, egui::Button::new("자동 정렬"))
            .on_hover_text(
                "열화상과 웹캠 영상의 윤곽을 비교해 크기와 위치를 맞춥니다. 두 카메라에 함께 \
                 보이는 물체(사람, 컵, 창틀 등)가 화면에 있을 때 잘 맞습니다",
            )
            .clicked();
        if busy {
            ui.spinner();
            ui.label("찾는 중…");
        }
        if ui.button("정렬 초기화").clicked() {
            *a = Alignment::default();
        }
    });
    if let Some((ok, note)) = &app.webcam.align_note {
        if *ok {
            ui.label(note);
        } else {
            ui.colored_label(RED, note);
        }
    }
    ui.label(
        "두 카메라의 위치가 달라 정렬은 한 거리에서만 정확합니다. 거리가 바뀌면 다시 맞추세요. \
         측정은 계속 열화상 기준입니다.",
    );

    if app.settings.webcam != before {
        app.mark_dirty();
    }
    if auto {
        start_auto_align(app, ui.ctx());
    }
    if retry {
        // The next `sync` opens it again.
        app.webcam.cam = None;
    }
    if refresh {
        app.webcam.list_error = None;
        start_listing(app, ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::settings::Settings;

    #[test]
    fn camera_opens_only_when_enabled_with_a_device() {
        let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
        let ctx = egui::Context::default();
        sync(&mut app, &ctx);
        assert!(app.webcam.cam.is_none());
        app.settings.webcam.enabled = true;
        sync(&mut app, &ctx);
        assert!(app.webcam.cam.is_none(), "no device chosen yet");
        // A device that does not exist: the thread starts and reports the failure.
        app.settings.webcam.device = Some(r"\\?\no-such-camera".into());
        sync(&mut app, &ctx);
        let cam = app.webcam.cam.as_ref().expect("opened");
        let t = std::time::Instant::now();
        while matches!(cam.state(), State::Opening) && t.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(cam.state(), State::Failed(_)), "{:?}", cam.state());
        assert!(layer(&app).is_none());
        // The same settings do not reopen it over and over; switching off closes it.
        sync(&mut app, &ctx);
        assert!(app.webcam.cam.is_some());
        app.settings.webcam.enabled = false;
        sync(&mut app, &ctx);
        assert!(app.webcam.cam.is_none());
    }

    /// Live thermal frame and a running (fake) webcam showing solid blue.
    fn rig(dir: &std::path::Path) -> ThermalApp {
        let settings = Settings {
            snapshot_dir: Some(dir.to_path_buf()),
            ..Default::default()
        };
        let mut app = ThermalApp::with_camera(settings, Camera::disconnected());
        let link = "fake-cam";
        app.settings.webcam.enabled = true;
        app.settings.webcam.device = Some(link.into());
        app.settings.webcam.device_name = "Test cam".into();
        let blue = [0, 0, 200, 255].repeat(64 * 48);
        app.webcam.cam = Some(Webcam::fake(link, Picture::new(1, 64, 48, blue)));
        app.camera.inject(crate::thermal::frame::ThermalFrame::new(
            160,
            120,
            vec![30000; 160 * 120],
            Some(0.01),
        ));
        app.tick(&egui::Context::default());
        app
    }

    #[test]
    fn auto_alignment_keeps_the_setting_when_nothing_matches() {
        let dir = std::env::temp_dir().join(format!("thermalview_align_{}", std::process::id()));
        let mut app = rig(&dir);
        let before = Alignment {
            scale: 1.3,
            dx: 0.1,
            dy: 0.0,
        };
        app.settings.webcam.overlay.align = before;
        // Solid thermal frame and solid webcam picture: no outlines to compare.
        assert!(auto_align_now(&mut app).is_err());
        assert_eq!(app.settings.webcam.overlay.align, before);
        assert!(matches!(app.webcam.align_note, Some((false, _))));

        // The button's way: a helper thread, collected by `sync`.
        let ctx = egui::Context::default();
        app.webcam.align_note = None;
        start_auto_align(&mut app, &ctx);
        let t = std::time::Instant::now();
        while app.webcam.aligning.is_some() && t.elapsed().as_secs() < 20 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            sync(&mut app, &ctx);
        }
        assert!(app.webcam.aligning.is_none());
        assert!(matches!(app.webcam.align_note, Some((false, _))));
        assert_eq!(app.settings.webcam.overlay.align, before);

        app.settings.webcam.enabled = false;
        assert!(auto_align_now(&mut app).is_err(), "no webcam picture");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn live_frames_mix_in_the_webcam_everywhere() {
        let dir = std::env::temp_dir().join(format!("thermalview_webcam_{}", std::process::id()));
        let mut app = rig(&dir);
        assert!(app.webcam.cam.is_some(), "sync keeps the fake camera");
        assert!(layer(&app).is_some());

        // Screen: the texture is uploaded and placed over the whole thermal image (4:3 on 4:3).
        let ctx = egui::Context::default();
        let mut first = ctx.run_ui(egui::RawInput::default(), |_| {});
        first.textures_delta.clear();
        upload_texture(&mut app, &ctx);
        let v = view_layer(&app).expect("view layer");
        assert_eq!(v.rect, [0.0, 0.0, 160.0, 120.0]);
        assert_eq!((v.mode, v.strength), (Mode::Blend, 0.5));
        app.settings.webcam.overlay.mode = Mode::Edges;
        assert!(
            view_layer(&app).is_none(),
            "texture still holds the blend picture"
        );
        upload_texture(&mut app, &ctx);
        assert_eq!(view_layer(&app).map(|v| v.mode), Some(Mode::Edges));
        app.settings.webcam.overlay.mode = Mode::Blend;

        // MP4: the scene carries the picture; the drawn frame is half thermal, half blue.
        let scene = app.video_scene(0.0).unwrap();
        let cv = crate::video::overlay::compose(&scene, &crate::video::overlay::Text::none());
        let thermal = &app.rgba[..3];
        let px = cv.get(300, 200);
        for k in 0..3 {
            let want = (thermal[k] as f32 * 0.5 + [0.0, 0.0, 200.0][k] * 0.5).round() as u8;
            assert!(px[k].abs_diff(want) <= 1, "{px:?} vs {thermal:?}");
        }

        // Snapshot: the webcam picture is saved next to the thermal files.
        let png = app
            .take_snapshot(&crate::camera::CameraStatus::default())
            .unwrap();
        let base = png.to_string_lossy().trim_end_matches(".png").to_owned();
        let cam_png = image::open(format!("{base}_webcam.png"))
            .unwrap()
            .into_rgb8();
        assert_eq!(cam_png.dimensions(), (64, 48));
        assert_eq!(cam_png.get_pixel(10, 10).0, [0, 0, 200]);
        let meta = std::fs::read_to_string(format!("{base}.json")).unwrap();
        assert!(meta.contains("\"device\": \"Test cam\""), "{meta}");

        // Playback is never mixed with the live webcam.
        app.start_seq(&crate::camera::CameraStatus::default())
            .unwrap();
        app.camera.inject(crate::thermal::frame::ThermalFrame::new(
            160,
            120,
            vec![30000; 160 * 120],
            Some(0.01),
        ));
        app.tick(&egui::Context::default());
        let (seq, _) = app.stop_seq().unwrap();
        app.open_sequence(&seq);
        assert!(layer(&app).is_none());
        assert!(app.video_scene(0.0).unwrap().webcam.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
