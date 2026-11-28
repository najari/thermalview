//! Bottom graph panel: temperature-over-time and histogram.

use eframe::egui::{self, Color32};
use egui_plot::{Bar, BarChart, Legend, Line, Plot};

use super::ThermalApp;
use crate::thermal::histogram::Histogram;
use crate::thermal::history::ShapeValue;
use crate::thermal::shapes::ShapeKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Time,
    Histogram,
    Profile,
}

pub struct GraphState {
    tab: Tab,
    /// Time window shown, in seconds.
    window: f64,
    show_max: bool,
    show_min: bool,
    show_mean: bool,
    show_center: bool,
    /// Maximum / mean of each Box and Line.
    show_shape_max: bool,
    show_shape_mean: bool,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            tab: Tab::Time,
            window: 60.0,
            show_max: true,
            show_min: true,
            show_mean: false,
            show_center: false,
            show_shape_max: true,
            show_shape_mean: false,
        }
    }
}

const MAX_COLOR: Color32 = Color32::from_rgb(230, 60, 60);
const MIN_COLOR: Color32 = Color32::from_rgb(70, 140, 240);
const MEAN_COLOR: Color32 = Color32::from_rgb(150, 150, 150);
const CENTER_COLOR: Color32 = Color32::from_rgb(60, 170, 90);
const SPOT_COLORS: [Color32; 6] = [
    Color32::from_rgb(240, 160, 0),
    Color32::from_rgb(170, 90, 220),
    Color32::from_rgb(0, 170, 170),
    Color32::from_rgb(220, 90, 150),
    Color32::from_rgb(120, 160, 40),
    Color32::from_rgb(200, 120, 60),
];

/// Legend on the left, where the oldest samples are: the newest values stay uncovered. Entries
/// in the order they were added (S2 before S10), small and see-through.
fn legend() -> Legend {
    Legend::default()
        .position(egui_plot::Corner::LeftTop)
        .text_style(egui::TextStyle::Small)
        .background_alpha(0.6)
        .follow_insertion_order(true)
}

pub(super) fn show(ui: &mut egui::Ui, app: &mut ThermalApp) {
    let unit = app.frame.as_ref().map(|f| f.unit()).unwrap_or("°C");
    let g = &mut app.graphs;
    // Wrapped: a row wider than the panel would widen the plot under it too, and push the
    // newest seconds out of the window.
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut g.tab, Tab::Time, "시간 그래프");
        ui.selectable_value(&mut g.tab, Tab::Histogram, "히스토그램");
        ui.selectable_value(&mut g.tab, Tab::Profile, "선 프로파일");
        ui.separator();
        if g.tab == Tab::Time {
            for (secs, label) in [(60.0, "1분"), (300.0, "5분"), (1800.0, "30분")] {
                ui.selectable_value(&mut g.window, secs, label);
            }
            ui.separator();
            ui.checkbox(&mut g.show_max, "최고");
            ui.checkbox(&mut g.show_min, "최저");
            ui.checkbox(&mut g.show_mean, "평균");
            ui.checkbox(&mut g.show_center, "중앙");
            if !app.settings.shapes.items.is_empty() {
                ui.separator();
                ui.checkbox(&mut g.show_shape_max, "영역 최고")
                    .on_hover_text("영역(B)과 선(L)마다 그 안의 최고 온도");
                ui.checkbox(&mut g.show_shape_mean, "영역 평균");
            }
            if ui.button("지우기").clicked() {
                app.history.clear();
            }
        }
    });

    match app.graphs.tab {
        Tab::Time => time_plot(ui, app, unit),
        Tab::Histogram => histogram_plot(ui, app, unit),
        Tab::Profile => profile_plot(ui, app, unit),
    }
}

fn profile_plot(ui: &mut egui::Ui, app: &ThermalApp, unit: &str) {
    let Some(f) = &app.frame else {
        ui.label("영상이 없습니다");
        return;
    };
    let lines: Vec<Line<'static>> = app
        .settings
        .shapes
        .items
        .iter()
        .filter(|s| s.kind == ShapeKind::Line)
        .enumerate()
        .map(|(i, s)| {
            Line::new(s.label(), s.profile(f))
                .color(SPOT_COLORS[i % SPOT_COLORS.len()])
                .width(1.5)
        })
        .collect();
    if lines.is_empty() {
        ui.label(
            "측정 도구에서 '선'을 고르고 영상 위를 드래그하면 선을 따라 온도 변화가 표시됩니다.",
        );
        return;
    }
    Plot::new("profile")
        .legend(legend())
        .x_axis_label("거리 (화소)")
        .y_axis_label(unit.to_string())
        .allow_scroll(false)
        .show(ui, |plot_ui| {
            for l in lines {
                plot_ui.line(l);
            }
        });
}

fn time_plot(ui: &mut egui::Ui, app: &ThermalApp, unit: &str) {
    let g = &app.graphs;
    let h = &app.history;
    let window = g.window;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut add = |on: bool, name: &str, color: Color32, pts: Vec<[f64; 2]>| {
        if on && !pts.is_empty() {
            lines.push(Line::new(name.to_string(), pts).color(color).width(1.5));
        }
    };
    add(g.show_max, "최고", MAX_COLOR, h.series(window, |s| s.max));
    add(g.show_min, "최저", MIN_COLOR, h.series(window, |s| s.min));
    add(
        g.show_mean,
        "평균",
        MEAN_COLOR,
        h.series(window, |s| s.mean),
    );
    add(
        g.show_center,
        "중앙",
        CENTER_COLOR,
        h.series(window, |s| s.center),
    );
    for (i, s) in app.settings.spots.items.iter().enumerate() {
        add(
            true,
            &format!("S{}", s.id),
            SPOT_COLORS[i % SPOT_COLORS.len()],
            h.spot_points(s.id, window),
        );
    }
    // Boxes and lines continue the spot colors; the mean is the same color, thinner in tone.
    let spots = app.settings.spots.items.len();
    for (i, s) in app.settings.shapes.items.iter().enumerate() {
        let color = SPOT_COLORS[(spots + i) % SPOT_COLORS.len()];
        add(
            g.show_shape_max,
            &format!("{} 최고", s.label()),
            color,
            h.shape_points(s.id, window, ShapeValue::Max),
        );
        add(
            g.show_shape_mean,
            &format!("{} 평균", s.label()),
            color.gamma_multiply(0.6),
            h.shape_points(s.id, window, ShapeValue::Mean),
        );
    }

    let unit = unit.to_string();
    Plot::new("time_plot")
        .legend(legend())
        .x_axis_label("초 (0 = 지금)")
        .y_axis_label(unit.clone())
        .default_x_bounds(-window, 0.0)
        .auto_bounds([false, true])
        .allow_scroll(false)
        .show(ui, move |plot_ui| {
            plot_ui.set_plot_bounds_x(-window..=0.0);
            for l in lines {
                plot_ui.line(l);
            }
        });
}

fn histogram_plot(ui: &mut egui::Ui, app: &ThermalApp, unit: &str) {
    let Some(f) = &app.frame else {
        ui.label("영상이 없습니다");
        return;
    };
    let (lo, hi) = app.display_range;
    let hist = Histogram::compute(f, lo, hi, 64);
    let lut = &app.lut.1;
    let n = hist.counts.len();
    let bars: Vec<Bar> = hist
        .fractions()
        .enumerate()
        .map(|(i, frac)| {
            let c = lut[(i * 255) / (n - 1).max(1)];
            Bar::new(hist.bin_center(i) as f64, frac as f64 * 100.0)
                .width(hist.bin_width as f64 * 0.95)
                .fill(Color32::from_rgb(c[0], c[1], c[2]))
        })
        .collect();
    Plot::new("histogram")
        .x_axis_label(unit.to_string())
        .y_axis_label("화소 %")
        .allow_scroll(false)
        .allow_drag(false)
        .allow_zoom(false)
        .show(ui, |plot_ui| {
            plot_ui.bar_chart(BarChart::new("분포", bars));
        });
}
