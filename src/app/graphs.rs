//! Bottom graph panel: temperature-over-time and histogram.

use eframe::egui::{self, Color32};
use egui_plot::{Bar, BarChart, Legend, Line, Plot};

use super::ThermalApp;
use crate::thermal::histogram::Histogram;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Time,
    Histogram,
}

pub struct GraphState {
    pub visible: bool,
    tab: Tab,
    /// Time window shown, in seconds.
    window: f64,
    show_max: bool,
    show_min: bool,
    show_mean: bool,
    show_center: bool,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            visible: true,
            tab: Tab::Time,
            window: 60.0,
            show_max: true,
            show_min: true,
            show_mean: false,
            show_center: false,
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

pub(super) fn show(ui: &mut egui::Ui, app: &mut ThermalApp) {
    let unit = app.frame.as_ref().map(|f| f.unit()).unwrap_or("°C");
    let g = &mut app.graphs;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut g.tab, Tab::Time, "시간 그래프");
        ui.selectable_value(&mut g.tab, Tab::Histogram, "히스토그램");
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
            if ui.button("지우기").clicked() {
                app.history.clear();
            }
        }
    });

    match app.graphs.tab {
        Tab::Time => time_plot(ui, app, unit),
        Tab::Histogram => histogram_plot(ui, app, unit),
    }
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

    let unit = unit.to_string();
    Plot::new("time_plot")
        .legend(Legend::default())
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
