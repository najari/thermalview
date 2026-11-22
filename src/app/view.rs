//! Central image view: thermal image, color bar, extreme markers, spot meters, hover readout.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};

use super::ThermalApp;
use crate::thermal::measure::Spots;

const BAR_WIDTH: f32 = 18.0;
const BAR_LABELS: f32 = 70.0;

/// Draw the image view. Returns true if the spot list changed.
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
        Sense::click(),
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

    // Clicks: left adds a spot, right removes the nearest one.
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos() {
        let px = (p.x - img_rect.min.x) / scale;
        let py = (p.y - img_rect.min.y) / scale;
        if resp.secondary_clicked() {
            if let Some(id) = app.settings.spots.hit(px, py, 6.0) {
                app.settings.spots.remove(id);
                changed = true;
            }
        } else if resp.clicked() {
            if let Some((x, y)) = to_pixel(p) {
                if app.settings.spots.add(x, y).is_none() {
                    app.notify(format!("측정점은 최대 {}개입니다", Spots::MAX));
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
