//! MP4 recording of what the image view shows.

use std::path::PathBuf;
use std::time::SystemTime;

use super::ThermalApp;
use crate::thermal::shapes::ShapeKind;
use crate::video::overlay::{Mark, Scene, MAX_COLOR, MIN_COLOR};
use crate::video::Mp4Writer;

impl ThermalApp {
    pub(super) fn toggle_video(&mut self) {
        let _ = if self.video.is_some() {
            self.stop_video().map(drop)
        } else {
            self.start_video().map(drop)
        };
    }

    /// Start recording the view (live or playback). The user is notified of the outcome; the
    /// error text is for remote clients.
    pub(super) fn start_video(&mut self) -> Result<PathBuf, String> {
        if self.video.is_some() {
            return Err("already recording MP4".into());
        }
        let Some(f) = &self.frame else {
            return Err("no frame yet".into());
        };
        match Mp4Writer::start(&self.settings.snapshot_dir(), f.width, f.height) {
            Ok(w) => {
                let path = w.path().to_path_buf();
                self.notify(format!("MP4 녹화 시작: {}", path.display()));
                self.video = Some(w);
                Ok(path)
            }
            Err(e) => {
                self.notify(format!("MP4 녹화를 시작할 수 없습니다: {e:#}"));
                Err(format!("cannot start the MP4: {e:#}"))
            }
        }
    }

    /// Finish the MP4; returns its path and frame count.
    pub(super) fn stop_video(&mut self) -> Result<(PathBuf, u64), String> {
        let w = self.video.take().ok_or("not recording MP4")?;
        match w.finish() {
            Ok((_, 0)) => {
                self.notify("MP4 녹화 종료: 기록된 프레임이 없어 파일을 만들지 않았습니다");
                Err("no frames were recorded; no file written".into())
            }
            Ok((path, n)) => {
                self.notify(format!("MP4 녹화 종료: {n}프레임 → {}", path.display()));
                Ok((path, n))
            }
            Err(e) => {
                self.notify(format!("MP4 저장 실패: {e:#}"));
                Err(format!("saving the MP4 failed: {e:#}"))
            }
        }
    }

    /// Add the frame just rendered, shown at `t` seconds, to the MP4.
    pub(super) fn push_video(&mut self, t: f64) {
        let Some(w) = &self.video else {
            return;
        };
        let Some(scene) = self.video_scene(t) else {
            return;
        };
        if (scene.width, scene.height) != w.size() {
            return;
        }
        let ok = self.video.as_mut().is_some_and(|w| w.push(t, scene));
        if !ok {
            // The writer thread stopped on an error; finishing reports it.
            if let Err(e) = self.stop_video() {
                self.notify(format!("MP4 녹화 중단: {e}"));
            }
        }
    }

    /// The current frame with the same markers and labels as the image view.
    fn video_scene(&self, t: f64) -> Option<Scene> {
        let (f, st) = (self.frame.as_ref()?, self.stats.as_ref()?);
        if self.rgba.len() != f.width * f.height * 4 {
            return None;
        }
        let unit = f.unit();
        let mut marks = Vec::new();
        if self.settings.show_extremes {
            for (at, v, color) in [
                (st.max_pos, f.value(st.max), MAX_COLOR),
                (st.min_pos, f.value(st.min), MIN_COLOR),
            ] {
                marks.push(Mark::Cross {
                    at,
                    text: format!("{v:.1}{unit}"),
                    color,
                });
            }
        }
        for s in &self.settings.spots.items {
            marks.push(Mark::Spot {
                at: (s.x, s.y),
                text: s
                    .value(f)
                    .map(|v| format!("S{} {v:.1}{unit}", s.id))
                    .unwrap_or_default(),
            });
        }
        for s in &self.settings.shapes.items {
            let text = s
                .stats(f)
                .map(|st| format!("{} ↑{:.1} ~{:.1}{unit}", s.label(), st.max, st.mean))
                .unwrap_or_default();
            marks.push(match s.kind {
                ShapeKind::Box => Mark::Box {
                    a: s.a,
                    b: s.b,
                    text,
                },
                ShapeKind::Line => Mark::Line {
                    a: s.a,
                    b: s.b,
                    text,
                },
            });
        }
        // Played-back frames carry no capture time; show the position in the recording.
        let mut caption = if f.captured == SystemTime::UNIX_EPOCH {
            let ms = (t * 1000.0).max(0.0) as u64;
            format!(
                "재생 {:02}:{:02}.{}",
                ms / 60_000,
                ms / 1000 % 60,
                ms / 100 % 10
            )
        } else {
            chrono::DateTime::<chrono::Local>::from(f.captured)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        };
        if f.calibration.is_some() {
            caption.push_str(&format!("  보정: {}", self.settings.calibration.summary()));
        }
        Some(Scene {
            width: f.width,
            height: f.height,
            rgba: self.rgba.clone(),
            lut: self.lut.1.to_vec(),
            range: self.display_range,
            unit,
            caption,
            marks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{Camera, CameraStatus};
    use crate::settings::Settings;
    use crate::thermal::frame::ThermalFrame;
    use eframe::egui;

    #[test]
    fn records_every_new_frame_and_stops_on_source_change() {
        let dir = std::env::temp_dir().join(format!("thermalview_appmp4_{}", std::process::id()));
        let settings = Settings {
            snapshot_dir: Some(dir.clone()),
            ..Default::default()
        };
        let mut app = ThermalApp::with_camera(settings, Camera::disconnected());
        let ctx = egui::Context::default();
        let frame = |seq| {
            let mut raw = vec![30000; 160 * 120];
            raw[seq as usize * 50] = 31000;
            let mut f = ThermalFrame::new(160, 120, raw, Some(0.01));
            f.sequence = seq;
            f
        };
        assert_eq!(app.start_video(), Err("no frame yet".into()));
        app.camera.inject(frame(1));
        app.tick(&ctx);
        app.settings.spots.add(10, 10);
        let path = app.start_video().unwrap();
        assert!(app.start_video().is_err());
        let scene = app.video_scene(0.0).unwrap();
        assert_eq!(scene.marks.len(), 3, "max, min and S1");
        for seq in 2..=10 {
            app.camera.inject(frame(seq));
        }
        app.tick(&ctx);
        // A palette change re-renders the same frame: nothing new to record.
        app.render();
        assert_eq!(app.video.as_ref().unwrap().frames(), 9);
        let (done, n) = app.stop_video().unwrap();
        assert_eq!((done, n), (path, 9));
        assert!(app.stop_video().is_err());

        // Opening a recording for playback ends the live MP4: one file, one source.
        app.start_seq(&CameraStatus::default()).unwrap();
        app.camera.inject(frame(11));
        app.tick(&ctx);
        let (seq, _) = app.stop_seq().unwrap();
        app.start_video().unwrap();
        app.camera.inject(frame(12));
        app.tick(&ctx);
        app.open_sequence(&seq);
        assert!(app.player.is_some());
        assert!(app.video.is_none());
        // Recording the playback works too (export to MP4).
        app.start_video().unwrap();
        app.tick(&ctx);
        assert_eq!(app.stop_video().unwrap().1, 1, "the first played frame");
        std::fs::remove_dir_all(&dir).ok();
    }
}
