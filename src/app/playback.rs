//! Playback of recorded `.tvseq` sequences, paced by the recorded timestamps.

use std::path::Path;
use std::time::Instant;

use eframe::egui;

use crate::storage::sequence::SeqReader;
use crate::thermal::frame::ThermalFrame;

pub struct Player {
    reader: SeqReader,
    index: usize,
    playing: bool,
    /// Wall-clock instant and recording time (ms) that playback is anchored to.
    anchor: (Instant, u64),
    speed: f64,
    total_ms: u64,
    /// Frame to show on the next tick (after open/seek).
    pending: Option<(u64, ThermalFrame)>,
}

pub enum PlayerAction {
    None,
    Close,
}

impl Player {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut reader = SeqReader::open(path)?;
        anyhow::ensure!(!reader.is_empty(), "녹화된 프레임이 없습니다");
        let total_ms = reader.timestamp(reader.len() - 1);
        let first = reader.frame(0)?;
        Ok(Self {
            reader,
            index: 0,
            playing: true,
            anchor: (Instant::now(), first.0),
            speed: 1.0,
            total_ms,
            pending: Some(first),
        })
    }

    pub fn file_name(&self) -> String {
        self.reader
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn current_ms(&self) -> u64 {
        if self.playing {
            let dt = self.anchor.0.elapsed().as_secs_f64() * 1000.0 * self.speed;
            (self.anchor.1 as f64 + dt) as u64
        } else {
            self.anchor.1
        }
    }

    fn seek(&mut self, i: usize) -> anyhow::Result<()> {
        let (t, f) = self.reader.frame(i)?;
        self.index = i;
        self.anchor = (Instant::now(), t);
        self.pending = Some((t, f));
        Ok(())
    }

    /// Advance according to the wall clock. Returns a frame when the shown frame changes.
    pub fn tick(&mut self) -> anyhow::Result<Option<(u64, ThermalFrame)>> {
        if let Some(p) = self.pending.take() {
            return Ok(Some(p));
        }
        if !self.playing {
            return Ok(None);
        }
        let t = self.current_ms();
        if t >= self.total_ms {
            self.playing = false;
            self.anchor = (Instant::now(), self.total_ms);
        }
        let i = self.reader.index_at(t.min(self.total_ms));
        if i != self.index {
            self.index = i;
            return Ok(Some(self.reader.frame(i)?));
        }
        Ok(None)
    }

    /// Transport bar. Returns what the app should do.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> anyhow::Result<PlayerAction> {
        let mut action = PlayerAction::None;
        ui.horizontal(|ui| -> anyhow::Result<()> {
            ui.strong(format!("재생: {}", self.file_name()));
            let label = if self.playing { "⏸" } else { "▶" };
            if ui.button(label).clicked() {
                if self.playing {
                    self.anchor = (Instant::now(), self.current_ms());
                    self.playing = false;
                } else {
                    if self.index + 1 >= self.reader.len() {
                        self.seek(0)?;
                    }
                    self.anchor = (Instant::now(), self.anchor.1);
                    self.playing = true;
                }
            }
            if ui.button("⏮").on_hover_text("한 프레임 뒤로").clicked() && self.index > 0 {
                self.playing = false;
                self.seek(self.index - 1)?;
            }
            if ui.button("⏭").on_hover_text("한 프레임 앞으로").clicked()
                && self.index + 1 < self.reader.len()
            {
                self.playing = false;
                self.seek(self.index + 1)?;
            }
            let mut idx = self.index;
            let last = self.reader.len() - 1;
            let slider = ui.add(
                egui::Slider::new(&mut idx, 0..=last)
                    .show_value(false)
                    .trailing_fill(true),
            );
            if slider.changed() && idx != self.index {
                self.seek(idx)?;
            }
            ui.label(format!(
                "{} / {}  ({}/{})",
                mmss(self.anchor_ms_display()),
                mmss(self.total_ms),
                self.index + 1,
                self.reader.len()
            ));
            let mut speed = self.speed;
            egui::ComboBox::from_id_salt("speed")
                .selected_text(format!("{speed}×"))
                .width(60.0)
                .show_ui(ui, |ui| {
                    for s in [0.25, 0.5, 1.0, 2.0, 4.0, 8.0] {
                        ui.selectable_value(&mut speed, s, format!("{s}×"));
                    }
                });
            if speed != self.speed {
                self.anchor = (Instant::now(), self.current_ms());
                self.speed = speed;
            }
            if ui.button("실시간으로 돌아가기").clicked() {
                action = PlayerAction::Close;
            }
            Ok(())
        })
        .inner?;
        if self.playing {
            ui.ctx().request_repaint();
        }
        Ok(action)
    }

    fn anchor_ms_display(&self) -> u64 {
        self.current_ms().min(self.total_ms)
    }
}

fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}.{}", s / 60, s % 60, (ms % 1000) / 100)
}
