//! Central image view: thermal image, color bar, extreme markers, measurements, hover readout.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind};

use std::time::Instant;

use super::{DragOp, ThermalApp, Tool};
use crate::thermal::measure::Spots;
use crate::thermal::shapes::{Shape, ShapeKind, Shapes};
use crate::webcam::blend::Mode;

const BAR_WIDTH: f32 = 18.0;
const BAR_LABELS: f32 = 70.0;
pub(super) const SHAPE_COLOR: Color32 = Color32::from_rgb(120, 255, 120);
/// Marker and reading of the hottest / coldest pixel.
pub(super) const MAX_COLOR: Color32 = Color32::from_rgb(235, 60, 60);
pub(super) const MIN_COLOR: Color32 = Color32::from_rgb(60, 140, 240);

/// Magnification of the image view. Snapshots and MP4 always hold the whole image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Zoom {
    /// 1 shows the whole image.
    pub factor: f32,
    /// Thermal pixel (fractional) at the top-left corner of the view.
    pub origin: (f32, f32),
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            factor: 1.0,
            origin: (0.0, 0.0),
        }
    }
}

impl Zoom {
    pub const MAX: f32 = crate::remote::MAX_ZOOM;

    /// Keep the factor in range and the view inside a `w`×`h` image.
    fn clamp(&mut self, w: f32, h: f32) {
        self.factor = if self.factor.is_finite() {
            self.factor.clamp(1.0, Self::MAX)
        } else {
            1.0
        };
        self.origin.0 = self.origin.0.clamp(0.0, w - w / self.factor);
        self.origin.1 = self.origin.1.clamp(0.0, h - h / self.factor);
    }

    /// Magnify by `by`, keeping the thermal pixel `at` where it is on screen.
    fn zoom_about(&mut self, by: f32, at: (f32, f32), w: f32, h: f32) {
        let old = self.factor;
        self.factor = (old * by).clamp(1.0, Self::MAX);
        let keep = old / self.factor;
        self.origin.0 = at.0 - (at.0 - self.origin.0) * keep;
        self.origin.1 = at.1 - (at.1 - self.origin.1) * keep;
        self.clamp(w, h);
    }

    /// Show the image `factor` times enlarged with `center` in the middle of the view.
    pub fn centered(factor: f32, center: (f32, f32), w: f32, h: f32) -> Self {
        let mut z = Self {
            factor,
            origin: (0.0, 0.0),
        };
        z.clamp(w, h);
        z.origin = (center.0 - w / z.factor / 2.0, center.1 - h / z.factor / 2.0);
        z.clamp(w, h);
        z
    }

    /// The thermal pixel in the middle of the view.
    pub fn center(&self, w: f32, h: f32) -> (f32, f32) {
        (
            self.origin.0 + w / self.factor / 2.0,
            self.origin.1 + h / self.factor / 2.0,
        )
    }
}

/// Draw the image view. Returns true if the measurements changed.
pub(super) fn image_view(ui: &mut egui::Ui, app: &mut ThermalApp) -> bool {
    let webcam = super::webcam::view_layer(app);
    let (Some(tex), Some(f), Some(st)) = (&app.texture, &app.frame, &app.stats) else {
        ui.centered_and_justified(|ui| {
            ui.label("영상을 기다리는 중…");
        });
        return false;
    };
    let unit = f.unit();
    let (w, h) = (f.width as f32, f.height as f32);

    // Fit the image (4:3) next to the color bar.
    let avail = ui.available_size();
    let max_w = (avail.x - BAR_WIDTH - BAR_LABELS - 16.0).max(80.0);
    let fit = (max_w / w).min(avail.y / h).max(0.5);
    let img_size = vec2(w * fit, h * fit);
    let used = img_size.x + 16.0 + BAR_WIDTH + BAR_LABELS;
    let (resp, bare) =
        ui.allocate_painter(vec2(avail.x.max(used), img_size.y), Sense::click_and_drag());
    // Image and color bar together, centered in the width that is left.
    let left = resp.rect.min.x + ((resp.rect.width() - used) / 2.0).max(0.0).floor();
    let img_rect = Rect::from_min_size(pos2(left, resp.rect.min.y), img_size);

    // Zoom with the wheel or a pinch about the pointer; pan with the right or middle button.
    let mut zoom = app.zoom;
    zoom.clamp(w, h);
    if let Some(p) = resp.hover_pos().filter(|p| img_rect.contains(*p)) {
        let (wheel, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let by = pinch * (wheel * 0.004).exp();
        if by != 1.0 {
            let s = fit * zoom.factor;
            let at = (
                zoom.origin.0 + (p.x - img_rect.min.x) / s,
                zoom.origin.1 + (p.y - img_rect.min.y) / s,
            );
            zoom.zoom_about(by, at, w, h);
        }
    }
    let panning = resp.dragged_by(egui::PointerButton::Secondary)
        || resp.dragged_by(egui::PointerButton::Middle);
    if panning && zoom.factor > 1.0 {
        let d = resp.drag_delta() / (fit * zoom.factor);
        zoom.origin = (zoom.origin.0 - d.x, zoom.origin.1 - d.y);
        zoom.clamp(w, h);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    app.zoom = zoom;
    // Screen points per thermal pixel, and where thermal pixel (0, 0) is on screen (outside
    // the view when zoomed in).
    let scale = fit * zoom.factor;
    let origin = img_rect.min - vec2(zoom.origin.0, zoom.origin.1) * scale;
    // Everything on the image is cut off at its edge.
    let painter = bare.with_clip_rect(img_rect.intersect(bare.clip_rect()));

    let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let whole = Rect::from_min_size(origin, vec2(w, h) * scale);
    match &webcam {
        None => {
            painter.image(tex.id(), whole, full_uv, Color32::WHITE);
        }
        Some(cam) => {
            // Same placement as the MP4 (webcam::blend), in thermal pixels.
            let [x0, y0, x1, y1] = cam.rect;
            let cam_rect =
                Rect::from_min_max(origin + vec2(x0, y0) * scale, origin + vec2(x1, y1) * scale);
            let fade = Color32::WHITE.gamma_multiply(cam.strength);
            match cam.mode {
                Mode::Blend => {
                    painter.rect_filled(img_rect, 0.0, Color32::BLACK);
                    painter.image(cam.tex, cam_rect, full_uv, Color32::WHITE);
                    painter.image(tex.id(), whole, full_uv, fade);
                }
                Mode::Edges => {
                    painter.image(tex.id(), whole, full_uv, Color32::WHITE);
                    painter.image(cam.tex, cam_rect, full_uv, fade);
                }
            }
        }
    }

    let to_screen = |x: usize, y: usize| {
        pos2(
            origin.x + (x as f32 + 0.5) * scale,
            origin.y + (y as f32 + 0.5) * scale,
        )
    };
    let to_pixel = |p: egui::Pos2| -> Option<(usize, usize)> {
        if !img_rect.contains(p) {
            return None;
        }
        let x = ((p.x - origin.x) / scale) as usize;
        let y = ((p.y - origin.y) / scale) as usize;
        (x < f.width && y < f.height).then_some((x, y))
    };
    let font = FontId::proportional(13.0);
    let clamp_px = |p: egui::Pos2| -> (usize, usize) {
        let x = ((p.x - origin.x) / scale).clamp(0.0, w - 1.0) as usize;
        let y = ((p.y - origin.y) / scale).clamp(0.0, h - 1.0) as usize;
        (x, y)
    };
    let image_xy = |p: egui::Pos2| ((p.x - origin.x) / scale, (p.y - origin.y) / scale);
    // A marker scrolled out of the view takes its label with it.
    let in_view = |p: egui::Pos2| img_rect.expand(1.0).contains(p);
    // About 7 screen pixels, whatever the zoom.
    let grab_tol = (7.0 / scale).max(0.75);
    let mut changed = false;

    // Start and follow drags first, so edited measurements are drawn in their new place.
    let what_is_at = |app: &ThermalApp, p: egui::Pos2| -> Option<DragOp> {
        if !img_rect.contains(p) {
            return None;
        }
        let (x, y) = image_xy(p);
        if let Some(id) = app.settings.spots.hit(x, y, grab_tol) {
            return Some(DragOp::MoveSpot { id });
        }
        // Inside a box the Spot tool moves it; the Box/Line tools draw a new shape there.
        let interior = app.tool == Tool::Spot;
        if let Some((original, grab)) = app.settings.shapes.grab(x, y, grab_tol, interior) {
            return Some(DragOp::EditShape {
                original,
                grab,
                origin: clamp_px(p),
            });
        }
        app.tool.shape_kind().map(|kind| DragOp::Draw {
            kind,
            from: clamp_px(p),
            to: clamp_px(p),
        })
    };
    // The left button measures; the others pan (above).
    let primary = egui::PointerButton::Primary;
    if resp.drag_started_by(primary) {
        app.drag = ui
            .input(|i| i.pointer.press_origin())
            .and_then(|p| what_is_at(app, p));
    }
    if resp.dragged_by(primary) {
        if let Some(p) = resp.interact_pointer_pos() {
            let now = clamp_px(p);
            match &mut app.drag {
                Some(DragOp::Draw { to, .. }) => *to = now,
                Some(DragOp::MoveSpot { id }) => app.settings.spots.set_pos(*id, now.0, now.1),
                Some(DragOp::EditShape {
                    original,
                    grab,
                    origin,
                }) => {
                    let dx = now.0 as i64 - origin.0 as i64;
                    let dy = now.1 as i64 - origin.1 as i64;
                    let edited = original.dragged(*grab, dx, dy, f.width, f.height);
                    app.settings.shapes.update(edited);
                }
                None => {}
            }
        }
    }
    if app.drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if let Some(p) = resp.hover_pos().filter(|_| !panning) {
        if matches!(
            what_is_at(app, p),
            Some(DragOp::MoveSpot { .. } | DragOp::EditShape { .. })
        ) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    // Max / min markers: of the temperatures, or of the differences in a difference image
    // (where it warmed up and cooled down the most).
    if app.settings.show_extremes {
        let (red, blue) = (
            Color32::from_rgb(255, 60, 60),
            Color32::from_rgb(80, 160, 255),
        );
        let marks = match app.diff_stats {
            Some(d) => [
                (d.max_pos, format!("Δ{:+.1}{unit}", d.max), red),
                (d.min_pos, format!("Δ{:+.1}{unit}", d.min), blue),
            ],
            None => [
                (st.max_pos, format!("{:.1}{unit}", f.value(st.max)), red),
                (st.min_pos, format!("{:.1}{unit}", f.value(st.min)), blue),
            ],
        };
        for (pos, text, color) in marks {
            let c = to_screen(pos.0, pos.1);
            if !in_view(c) {
                continue;
            }
            cross(&painter, c, 7.0, color);
            label(&painter, c, vec2(9.0, -9.0), img_rect, &text, &font, color);
        }
    }

    // Spot meters.
    for s in &app.settings.spots.items {
        let c = to_screen(s.x, s.y);
        if !in_view(c) {
            continue;
        }
        painter.circle_stroke(c, 1.5 * scale.max(3.0), Stroke::new(2.0, Color32::BLACK));
        painter.circle_stroke(c, 1.5 * scale.max(3.0), Stroke::new(1.0, Color32::WHITE));
        let v = s
            .value(f)
            .map(|v| format!("S{} {v:.1}{unit}", s.id))
            .unwrap_or_default();
        label(
            &painter,
            c,
            vec2(8.0, 6.0),
            img_rect,
            &v,
            &font,
            Color32::WHITE,
        );
    }

    // Boxes and lines.
    let draw_shape = |s: &Shape, color: Color32| {
        let shadow = Stroke::new(3.0, Color32::BLACK);
        let line = Stroke::new(1.5, color);
        match s.kind {
            ShapeKind::Box => {
                let r = Rect::from_two_pos(
                    pos2(
                        origin.x + s.a.0.min(s.b.0) as f32 * scale,
                        origin.y + s.a.1.min(s.b.1) as f32 * scale,
                    ),
                    pos2(
                        origin.x + (s.a.0.max(s.b.0) + 1) as f32 * scale,
                        origin.y + (s.a.1.max(s.b.1) + 1) as f32 * scale,
                    ),
                );
                painter.rect_stroke(r, 0.0, shadow, StrokeKind::Middle);
                painter.rect_stroke(r, 0.0, line, StrokeKind::Middle);
                r.left_top()
            }
            ShapeKind::Line => {
                let (a, b) = (to_screen(s.a.0, s.a.1), to_screen(s.b.0, s.b.1));
                painter.line_segment([a, b], shadow);
                painter.line_segment([a, b], line);
                for p in [a, b] {
                    painter.circle_filled(p, 3.0, color);
                }
                a
            }
        }
    };
    for s in &app.settings.shapes.items {
        let corner = draw_shape(s, SHAPE_COLOR);
        if let Some(st) = s.stats(f) {
            // Where the hottest pixel of an area is.
            if s.kind == ShapeKind::Box && st.pixels > 1 {
                let c = to_screen(st.max_pos.0, st.max_pos.1);
                cross(&painter, c, 4.0, SHAPE_COLOR);
            }
            // The label stays in the view while any part of the shape may be visible.
            let anchor = corner.clamp(img_rect.min, img_rect.max);
            let txt = format!("{} ↑{:.1} ~{:.1}{unit}", s.label(), st.max, st.mean);
            label(
                &painter,
                anchor,
                vec2(2.0, -18.0),
                img_rect,
                &txt,
                &font,
                SHAPE_COLOR,
            );
        }
    }

    // Shape being drawn.
    if let Some(DragOp::Draw { kind, from, to }) = app.drag {
        draw_shape(
            &Shape {
                id: 0,
                kind,
                a: from,
                b: to,
            },
            Color32::YELLOW,
        );
    }

    // Hover readout.
    if let Some((x, y)) = resp.hover_pos().and_then(to_pixel) {
        let mut txt = format!("({x}, {y})  {:.2} {unit}", f.value(f.at(x, y)));
        if let Some(d) = app.diff_values.get(y * f.width + x) {
            txt.push_str(&format!("  Δ {d:+.2}"));
        }
        label(
            &painter,
            img_rect.left_bottom(),
            vec2(6.0, -22.0),
            img_rect,
            &txt,
            &font,
            Color32::WHITE,
        );
    }

    // Color bar with labels.
    let bar = Rect::from_min_size(
        pos2(img_rect.max.x + 16.0, img_rect.min.y),
        vec2(BAR_WIDTH, img_size.y),
    );
    // The bar and its labels are beside the image: drawn without the image's clip.
    let painter = bare;
    let lut = &app.lut.1;
    let steps = 128;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let c = lut[(((1.0 - t0) * 255.0).round() as usize).min(255)];
        let r = Rect::from_min_max(
            pos2(bar.min.x, bar.min.y + t0 * bar.height()),
            pos2(bar.max.x, bar.min.y + t1 * bar.height() + 0.5),
        );
        painter.rect_filled(r, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
    }
    let (lo, hi) = app.display_range;
    // The isotherm band, as a strip in its color along the part of the bar it covers.
    let iso = &app.settings.isotherm;
    if let (Some((a, b)), true) = (iso.limits(), hi > lo) {
        let y_of = |v: f32| bar.min.y + ((hi - v) / (hi - lo)).clamp(0.0, 1.0) * bar.height();
        let (top, bottom) = (y_of(b), y_of(a));
        if bottom > top {
            let strip =
                Rect::from_min_max(pos2(bar.min.x - 7.0, top), pos2(bar.min.x - 2.0, bottom));
            let [r, g, b] = iso.color;
            painter.rect_filled(strip, 0.0, Color32::from_rgb(r, g, b));
        }
    }
    let text_color = ui.visuals().text_color();
    for k in 0..=4 {
        let t = k as f32 / 4.0;
        let v = hi - (hi - lo) * t;
        let y = bar.min.y + t * bar.height();
        painter.line_segment(
            [pos2(bar.max.x, y), pos2(bar.max.x + 4.0, y)],
            Stroke::new(1.0, text_color),
        );
        let align = match k {
            0 => Align2::LEFT_TOP,
            4 => Align2::LEFT_BOTTOM,
            _ => Align2::LEFT_CENTER,
        };
        // Differences carry their sign.
        let text = match app.diff_stats {
            Some(_) => format!("{v:+.1}{unit}"),
            None => format!("{v:.1}{unit}"),
        };
        painter.text(
            pos2(bar.max.x + 6.0, y),
            align,
            text,
            font.clone(),
            text_color,
        );
    }

    // Pointer: click adds a spot (Spot tool); drag edits what is under the pointer, or draws a
    // new shape (Box/Line tools); right-click deletes the nearest spot or shape.
    if resp.drag_stopped() {
        match app.drag.take() {
            Some(DragOp::Draw { kind, from, to }) if from != to => {
                if app.settings.shapes.add(kind, from, to).is_none() {
                    app.message = Some((
                        format!("영역·선은 최대 {}개입니다", Shapes::MAX),
                        Instant::now(),
                    ));
                }
                changed = true;
            }
            Some(DragOp::Draw { .. }) | None => {}
            Some(_) => changed = true,
        }
    }
    if let Some(p) = resp.interact_pointer_pos() {
        let (px, py) = image_xy(p);
        if resp.secondary_clicked() {
            // Six thermal pixels at full view, fewer when zoomed in.
            let tol = (6.0 / zoom.factor).max(grab_tol);
            let spot = app.settings.spots.hit(px, py, tol);
            let shape = app.settings.shapes.hit(px, py, tol);
            match (spot, shape) {
                (Some(id), _) => app.settings.spots.remove(id),
                (None, Some(id)) => app.settings.shapes.remove(id),
                _ => {}
            }
            changed |= spot.is_some() || shape.is_some();
        } else if resp.clicked()
            && app.tool == Tool::Spot
            && app.settings.spots.hit(px, py, grab_tol).is_none()
        {
            if let Some((x, y)) = to_pixel(p) {
                if app.settings.spots.add(x, y).is_none() {
                    app.message = Some((
                        format!("측정점은 최대 {}개입니다", Spots::MAX),
                        Instant::now(),
                    ));
                }
                changed = true;
            }
        }
    }
    changed
}

fn cross(p: &egui::Painter, c: egui::Pos2, r: f32, color: Color32) {
    for stroke in [Stroke::new(3.0, Color32::BLACK), Stroke::new(1.5, color)] {
        p.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], stroke);
        p.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
    }
}

/// Text with a dark backdrop so it stays readable on any palette. The text is placed at
/// `anchor + offset`; if that would leave `bounds`, it flips to the other side of the anchor.
fn label(
    p: &egui::Painter,
    anchor: egui::Pos2,
    offset: egui::Vec2,
    bounds: Rect,
    text: &str,
    font: &FontId,
    color: Color32,
) {
    if text.is_empty() {
        return;
    }
    let galley = p.layout_no_wrap(text.to_owned(), font.clone(), color);
    let size = galley.size();
    let mut pos = anchor + offset;
    if pos.x + size.x > bounds.max.x {
        pos.x = anchor.x - offset.x.abs() - size.x;
    }
    if pos.y < bounds.min.y {
        pos.y = anchor.y + offset.y.abs();
    }
    if pos.y + size.y > bounds.max.y {
        pos.y = anchor.y - offset.y.abs() - size.y;
    }
    pos.x = pos.x.max(bounds.min.x);
    let rect = Rect::from_min_size(pos, size).expand(2.0);
    p.rect_filled(rect, 2.0, Color32::from_black_alpha(160));
    p.galley(pos, galley, color);
}

#[cfg(test)]
mod tests {
    //! Headless UI tests: real egui input events, no window, GPU, mouse or camera.

    use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput};

    use super::*;
    use crate::camera::Camera;
    use crate::settings::Settings;
    use crate::thermal::frame::ThermalFrame;

    struct Rig {
        ctx: egui::Context,
        app: ThermalApp,
        time: f64,
    }

    /// With an 800×600 view the image starts at (0, 0) at this many points per pixel.
    const SCALE: f32 = (800.0 - BAR_WIDTH - BAR_LABELS - 16.0) / 160.0;

    fn at(x: usize, y: usize) -> Pos2 {
        pos2((x as f32 + 0.5) * SCALE, (y as f32 + 0.5) * SCALE)
    }

    impl Rig {
        fn new() -> Self {
            let ctx = egui::Context::default();
            // Textures can only be created after the first pass.
            let mut first = ctx.run_ui(RawInput::default(), |_| {});
            first.textures_delta.clear(); // no renderer here: drop the texture updates
            let mut app = ThermalApp::with_camera(Settings::default(), Camera::disconnected());
            let f = ThermalFrame::new(160, 120, vec![30000; 160 * 120], Some(0.01));
            app.stats = Some(f.stats());
            app.frame = Some(f);
            app.render();
            app.upload_texture(&ctx);
            let mut rig = Self {
                ctx,
                app,
                time: 0.0,
            };
            // egui hit-tests against the previous frame's widgets.
            rig.step(vec![]);
            rig.step(vec![]);
            rig
        }

        fn step(&mut self, events: Vec<Event>) {
            self.time += 1.0 / 30.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let app = &mut self.app;
            let mut out = self.ctx.run_ui(raw, |ui| {
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    image_view(ui, app);
                });
            });
            out.textures_delta.clear();
        }

        fn button(&mut self, pos: Pos2, button: PointerButton, pressed: bool) {
            self.step(vec![Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            }]);
        }

        fn click(&mut self, pos: Pos2, button: PointerButton) {
            self.step(vec![Event::PointerMoved(pos)]);
            self.button(pos, button, true);
            self.button(pos, button, false);
            self.step(vec![]);
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.step(vec![Event::PointerMoved(from)]);
            self.button(from, PointerButton::Primary, true);
            for i in 1..=6 {
                self.step(vec![Event::PointerMoved(from.lerp(to, i as f32 / 6.0))]);
            }
            self.button(to, PointerButton::Primary, false);
            self.step(vec![]);
        }

        fn spots(&self) -> Vec<(usize, usize)> {
            let items = &self.app.settings.spots.items;
            items.iter().map(|s| (s.x, s.y)).collect()
        }

        fn shapes(&self) -> Vec<((usize, usize), (usize, usize))> {
            let items = &self.app.settings.shapes.items;
            items.iter().map(|s| (s.a, s.b)).collect()
        }
    }

    #[test]
    fn click_adds_one_spot_and_right_click_removes_it() {
        let mut r = Rig::new();
        r.click(at(40, 50), PointerButton::Primary);
        assert_eq!(r.spots(), vec![(40, 50)]);
        // Clicking the same spot again must not stack a second one on it.
        r.click(at(40, 50), PointerButton::Primary);
        assert_eq!(r.spots().len(), 1);
        r.click(at(40, 50), PointerButton::Secondary);
        assert!(r.spots().is_empty());
    }

    #[test]
    fn dragging_a_spot_moves_it() {
        let mut r = Rig::new();
        r.click(at(40, 50), PointerButton::Primary);
        r.drag(at(40, 50), at(70, 60));
        assert_eq!(r.spots(), vec![(70, 60)]);
        assert!(r.app.drag.is_none());
    }

    #[test]
    fn box_tool_draws_and_corner_resizes() {
        let mut r = Rig::new();
        r.app.tool = Tool::Box;
        r.drag(at(20, 20), at(60, 50));
        assert_eq!(r.shapes(), vec![((20, 20), (60, 50))]);
        // Bottom-right corner of the outline is at pixel edge (61, 51).
        let corner = pos2(61.0 * SCALE, 51.0 * SCALE);
        r.drag(corner, corner + vec2(10.0, 5.0) * SCALE);
        assert_eq!(r.shapes(), vec![((20, 20), (70, 55))]);
    }

    #[test]
    fn inside_a_box_spot_tool_moves_but_box_tool_draws() {
        let mut r = Rig::new();
        r.app.tool = Tool::Box;
        r.drag(at(20, 20), at(60, 50));
        // Box tool, starting inside: a second, new box.
        r.drag(at(30, 30), at(40, 40));
        assert_eq!(r.shapes().len(), 2);
        r.click(at(35, 35), PointerButton::Secondary);
        assert_eq!(r.shapes().len(), 1);
        // Spot tool, starting inside: moves the box and keeps its size.
        r.app.tool = Tool::Spot;
        r.drag(at(30, 30), at(50, 40));
        assert_eq!(r.shapes(), vec![((40, 30), (80, 60))]);
        assert!(r.spots().is_empty());
    }

    #[test]
    fn dragging_a_line_end_leaves_the_other_end() {
        let mut r = Rig::new();
        r.app.tool = Tool::Line;
        r.drag(at(10, 80), at(150, 80));
        assert_eq!(r.shapes(), vec![((10, 80), (150, 80))]);
        r.drag(at(150, 80), at(120, 100));
        assert_eq!(r.shapes(), vec![((10, 80), (120, 100))]);
    }

    #[test]
    fn zoom_keeps_the_view_inside_the_image() {
        let (w, h) = (160.0, 120.0);
        let z = Zoom::centered(4.0, (80.0, 60.0), w, h);
        assert_eq!(z.origin, (60.0, 45.0));
        assert_eq!(z.center(w, h), (80.0, 60.0));
        // A corner cannot be centered: the view stops at the edge.
        assert_eq!(Zoom::centered(4.0, (0.0, 0.0), w, h).origin, (0.0, 0.0));
        assert_eq!(
            Zoom::centered(2.0, (159.0, 119.0), w, h).origin,
            (80.0, 60.0)
        );
        // Out-of-range factors are brought back.
        assert_eq!(Zoom::centered(100.0, (80.0, 60.0), w, h).factor, Zoom::MAX);
        assert_eq!(
            Zoom::centered(f32::NAN, (80.0, 60.0), w, h),
            Zoom::default()
        );
        // Zooming out to the whole image puts it back in place.
        let mut z = Zoom::centered(2.0, (120.0, 90.0), w, h);
        z.zoom_about(0.25, (150.0, 100.0), w, h);
        assert_eq!(z, Zoom::default());
    }

    #[test]
    fn pinch_zooms_about_the_pointer_and_clicks_follow() {
        let mut r = Rig::new();
        // Twice as large, about the pixel under the pointer.
        r.step(vec![Event::PointerMoved(at(40, 50))]);
        r.step(vec![Event::Zoom(2.0)]);
        assert_eq!(r.app.zoom.factor, 2.0);
        let (x, y) = r.app.zoom.origin;
        assert!(
            (x - 20.25).abs() < 0.01 && (y - 25.25).abs() < 0.01,
            "{x} {y}"
        );
        // The same screen point is still pixel (40, 50)…
        r.click(at(40, 50), PointerButton::Primary);
        assert_eq!(r.spots(), vec![(40, 50)]);
        // …and the top-left corner of the view is now pixel (20, 25).
        r.click(at(0, 0), PointerButton::Primary);
        assert_eq!(r.spots(), vec![(40, 50), (20, 25)]);
        // A drag still moves a spot by thermal pixels: 10 screen pixels are 5 of them.
        r.drag(at(40, 50), at(50, 50));
        assert_eq!(r.spots()[0], (45, 50));
    }

    #[test]
    fn right_button_pans_and_draws_nothing() {
        let mut r = Rig::new();
        r.app.tool = Tool::Box;
        r.app.zoom = Zoom::centered(2.0, (80.0, 60.0), 160.0, 120.0);
        assert_eq!(r.app.zoom.origin, (40.0, 30.0));
        let (from, to) = (at(80, 60), at(60, 50));
        r.step(vec![Event::PointerMoved(from)]);
        r.button(from, PointerButton::Secondary, true);
        for i in 1..=6 {
            r.step(vec![Event::PointerMoved(from.lerp(to, i as f32 / 6.0))]);
        }
        r.button(to, PointerButton::Secondary, false);
        r.step(vec![]);
        // Dragged 20 × 10 screen pixels to the upper left: the view moves 10 × 5 thermal
        // pixels to the lower right.
        let (x, y) = r.app.zoom.origin;
        assert!((x - 50.0).abs() < 1.0 && (y - 35.0).abs() < 1.0, "{x} {y}");
        assert!(r.shapes().is_empty() && r.spots().is_empty());
        assert!(r.app.drag.is_none());
        // At full view there is nothing to pan.
        r.app.zoom = Zoom::default();
        r.step(vec![Event::PointerMoved(from)]);
        r.button(from, PointerButton::Secondary, true);
        r.step(vec![Event::PointerMoved(to)]);
        r.button(to, PointerButton::Secondary, false);
        assert_eq!(r.app.zoom, Zoom::default());
        assert!(r.shapes().is_empty());
    }

    #[test]
    fn dragging_past_the_edge_stays_in_the_image() {
        let mut r = Rig::new();
        r.click(at(40, 50), PointerButton::Primary);
        r.drag(at(40, 50), pos2(5000.0, 5000.0));
        assert_eq!(r.spots(), vec![(159, 119)]);
    }
}
