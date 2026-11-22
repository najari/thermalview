//! Central image view: thermal image, color bar, extreme markers, measurements, hover readout.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind};

use std::time::Instant;

use super::{DragOp, ThermalApp, Tool};
use crate::thermal::measure::Spots;
use crate::thermal::shapes::{Shape, ShapeKind, Shapes};

const BAR_WIDTH: f32 = 18.0;
const BAR_LABELS: f32 = 70.0;
pub(super) const SHAPE_COLOR: Color32 = Color32::from_rgb(120, 255, 120);

/// Draw the image view. Returns true if the measurements changed.
pub(super) fn image_view(ui: &mut egui::Ui, app: &mut ThermalApp) -> bool {
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
    let scale = (max_w / w).min(avail.y / h).max(0.5);
    let img_size = vec2(w * scale, h * scale);
    let (resp, painter) = ui.allocate_painter(
        vec2(img_size.x + 16.0 + BAR_WIDTH + BAR_LABELS, img_size.y),
        Sense::click_and_drag(),
    );
    let img_rect = Rect::from_min_size(resp.rect.min, img_size);
    painter.image(
        tex.id(),
        img_rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    let to_screen = |x: usize, y: usize| {
        pos2(
            img_rect.min.x + (x as f32 + 0.5) * scale,
            img_rect.min.y + (y as f32 + 0.5) * scale,
        )
    };
    let to_pixel = |p: egui::Pos2| -> Option<(usize, usize)> {
        if !img_rect.contains(p) {
            return None;
        }
        let x = ((p.x - img_rect.min.x) / scale) as usize;
        let y = ((p.y - img_rect.min.y) / scale) as usize;
        (x < f.width && y < f.height).then_some((x, y))
    };
    let font = FontId::proportional(13.0);
    let clamp_px = |p: egui::Pos2| -> (usize, usize) {
        let x = ((p.x - img_rect.min.x) / scale).clamp(0.0, w - 1.0) as usize;
        let y = ((p.y - img_rect.min.y) / scale).clamp(0.0, h - 1.0) as usize;
        (x, y)
    };
    let image_xy = |p: egui::Pos2| {
        (
            (p.x - img_rect.min.x) / scale,
            (p.y - img_rect.min.y) / scale,
        )
    };
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
    if resp.drag_started() {
        app.drag = ui
            .input(|i| i.pointer.press_origin())
            .and_then(|p| what_is_at(app, p));
    }
    if resp.dragged() {
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
    } else if let Some(p) = resp.hover_pos() {
        if matches!(
            what_is_at(app, p),
            Some(DragOp::MoveSpot { .. } | DragOp::EditShape { .. })
        ) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    // Max / min markers.
    if app.settings.show_extremes {
        for (pos, v, color) in [
            (st.max_pos, f.value(st.max), Color32::from_rgb(255, 60, 60)),
            (st.min_pos, f.value(st.min), Color32::from_rgb(80, 160, 255)),
        ] {
            let c = to_screen(pos.0, pos.1);
            cross(&painter, c, 7.0, color);
            label(
                &painter,
                c,
                vec2(9.0, -9.0),
                img_rect,
                &format!("{v:.1}{unit}"),
                &font,
                color,
            );
        }
    }

    // Spot meters.
    for s in &app.settings.spots.items {
        let c = to_screen(s.x, s.y);
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
                        img_rect.min.x + s.a.0.min(s.b.0) as f32 * scale,
                        img_rect.min.y + s.a.1.min(s.b.1) as f32 * scale,
                    ),
                    pos2(
                        img_rect.min.x + (s.a.0.max(s.b.0) + 1) as f32 * scale,
                        img_rect.min.y + (s.a.1.max(s.b.1) + 1) as f32 * scale,
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
            let txt = format!("{} ↑{:.1} ~{:.1}{unit}", s.label(), st.max, st.mean);
            label(
                &painter,
                corner,
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
        let txt = format!("({x}, {y})  {:.2} {unit}", f.value(f.at(x, y)));
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
        painter.text(
            pos2(bar.max.x + 6.0, y),
            align,
            format!("{v:.1}{unit}"),
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
        let px = (p.x - img_rect.min.x) / scale;
        let py = (p.y - img_rect.min.y) / scale;
        if resp.secondary_clicked() {
            let tol = 6.0;
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
            app.render(&ctx);
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
    fn dragging_past_the_edge_stays_in_the_image() {
        let mut r = Rig::new();
        r.click(at(40, 50), PointerButton::Primary);
        r.drag(at(40, 50), pos2(5000.0, 5000.0));
        assert_eq!(r.spots(), vec![(159, 119)]);
    }
}
