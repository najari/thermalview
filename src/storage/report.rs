//! One-page HTML report of a snapshot: the annotated image, the webcam picture, the conditions
//! and every reading. Self-contained (images embedded), so it can be mailed as one file or
//! printed to PDF from a browser.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::snapshot::SnapshotMeta;

/// What the report shows besides the snapshot's metadata.
pub struct Extras<'a> {
    /// The image as the view shows it (measurements, color bar, time), PNG.
    pub view_png: &'a [u8],
    /// The webcam picture, PNG.
    pub webcam_png: Option<&'a [u8]>,
    /// One-line description of the temperature correction in use.
    pub calibration: Option<String>,
    /// Base name of the snapshot's other files.
    pub base: &'a str,
}

/// `<base>_report.html` next to the snapshot's color PNG.
pub fn path_for(color_png: &Path) -> PathBuf {
    let stem = color_png
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    color_png.with_file_name(format!("{stem}_report.html"))
}

pub fn write(path: &Path, meta: &SnapshotMeta, extras: &Extras<'_>) -> anyhow::Result<()> {
    std::fs::write(path, html(meta, extras))?;
    Ok(())
}

/// Text made safe for HTML.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn img(png: &[u8], alt: &str) -> String {
    format!(
        "<img alt=\"{}\" src=\"data:image/png;base64,{}\">",
        esc(alt),
        crate::remote::base64(png)
    )
}

pub fn html(meta: &SnapshotMeta, x: &Extras<'_>) -> String {
    let unit = meta.unit;
    let v = |t: f32| format!("{t:.2} {unit}");
    // "2026-11-28T16:08:05.876+09:00" → "2026-11-28 16:08:05"
    let when = meta
        .captured_local
        .get(..19)
        .unwrap_or(&meta.captured_local)
        .replace('T', " ");

    let mut conditions: Vec<(&str, String)> = vec![("촬영 시각", when.clone())];
    if let Some(d) = &meta.device {
        conditions.push(("카메라", d.clone()));
    }
    if let Some(p) = &meta.lepton_part {
        conditions.push(("센서", p.clone()));
    }
    conditions.push(("영상 크기", format!("{} × {}", meta.width, meta.height)));
    conditions.push((
        "온도 단위",
        match meta.kelvin_per_count {
            Some(k) => format!("{unit} (원시값 1 = {k} K)"),
            None => "원시값(온도 아님)".into(),
        },
    ));
    conditions.push((
        "보정",
        x.calibration.clone().unwrap_or_else(|| "없음".into()),
    ));
    conditions.push(("팔레트", meta.palette.clone()));
    conditions.push((
        "색 범위",
        format!("{:.1} ~ {:.1} {unit}", meta.range.0, meta.range.1),
    ));
    if let Some(n) = meta.noise_filter {
        conditions.push(("잡음 필터", format!("{n}프레임 평균")));
    }
    if let Some(d) = &meta.difference {
        conditions.push((
            "차이 영상",
            format!(
                "기준 {} · 색 ±{:.1} {unit} · 변화 {:+.2} ~ {:+.2} {unit}",
                d.reference_local, d.span, d.min, d.max
            ),
        ));
    }
    if let Some(t) = meta.fpa_c {
        conditions.push(("센서 온도", format!("{t:.2} °C")));
    }
    if let Some(m) = &meta.shutter_mode {
        conditions.push(("셔터 모드", m.clone()));
    }
    if let Some(w) = &meta.webcam {
        conditions.push(("웹캠", format!("{} ({} × {})", w.device, w.width, w.height)));
    }

    let mut rows: Vec<[String; 4]> =
        vec![["화면 전체".into(), v(meta.max), v(meta.min), v(meta.mean)]];
    for s in &meta.spots {
        rows.push([
            format!("S{} ({}, {})", s.id, s.x, s.y),
            v(s.value),
            String::new(),
            String::new(),
        ]);
    }
    for s in &meta.shapes {
        let kind = match s.kind.as_str() {
            "Box" => "영역",
            "Ellipse" => "원",
            "Line" => "선",
            other => other,
        };
        rows.push([format!("{} {kind}", s.label), v(s.max), v(s.min), v(s.mean)]);
    }

    let mut h = String::with_capacity(x.view_png.len() * 4 / 3 + 8192);
    let title = format!("열화상 측정 보고서 {when}");
    let _ = write!(
        h,
        "<!doctype html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title><style>{CSS}</style></head><body>\n<h1>열화상 측정 보고서</h1>\n\
         <p class=\"sub\">{} · {}</p>\n",
        esc(&title),
        esc(&when),
        esc(x.base)
    );
    h.push_str("<div class=\"pics\">");
    h.push_str(&img(x.view_png, "열화상"));
    if let Some(w) = x.webcam_png {
        h.push_str(&img(w, "웹캠"));
    }
    h.push_str("</div>\n<h2>측정값</h2>\n<table><tr><th>측정</th><th>최고 / 값</th><th>최저</th><th>평균</th></tr>\n");
    for r in &rows {
        let _ = writeln!(
            h,
            "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>",
            esc(&r[0]),
            esc(&r[1]),
            esc(&r[2]),
            esc(&r[3])
        );
    }
    h.push_str("</table>\n");
    if !meta.deltas.is_empty() {
        h.push_str("<h2>측정의 차</h2>\n<table><tr><th>차</th><th>값</th></tr>\n");
        for d in &meta.deltas {
            let _ = writeln!(
                h,
                "<tr><td>{}</td><td class=\"n\">{}</td></tr>",
                esc(&d.label),
                esc(&format!("{:+.2} {unit}", d.value))
            );
        }
        h.push_str("</table>\n");
    }
    h.push_str("<h2>조건</h2>\n<table>\n");
    for (k, val) in &conditions {
        let _ = writeln!(h, "<tr><th>{}</th><td>{}</td></tr>", esc(k), esc(val));
    }
    let _ = write!(
        h,
        "</table>\n<p class=\"foot\">{} {} · 원본: {}.png, {}_raw16.png(16비트), {}.json</p>\n</body></html>\n",
        esc(meta.app),
        esc(meta.version),
        esc(x.base),
        esc(x.base),
        esc(x.base)
    );
    h
}

const CSS: &str = "body{font-family:'Malgun Gothic','Segoe UI',sans-serif;margin:24px auto;\
max-width:960px;padding:0 16px;color:#1a1a1a;background:#fff}\
h1{font-size:22px;margin:0}h2{font-size:16px;margin:24px 0 8px}\
.sub{color:#666;margin:4px 0 16px}.pics{display:flex;flex-wrap:wrap;gap:12px}\
.pics img{max-width:100%;height:auto;border:1px solid #ccc}\
table{border-collapse:collapse;width:100%;font-size:14px}\
th,td{border:1px solid #d0d0d0;padding:4px 8px;text-align:left}\
th{background:#f2f2f2;font-weight:600;white-space:nowrap}td.n{text-align:right;\
font-variant-numeric:tabular-nums}.foot{color:#888;font-size:12px;margin-top:24px}\
@media print{body{margin:0;max-width:none}.pics img{max-height:80vh}}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::snapshot::{DeltaReading, ShapeReading, SpotReading};

    fn meta() -> SnapshotMeta {
        SnapshotMeta {
            app: "thermalview",
            version: "0.1.0",
            captured_local: "2026-11-28T16:08:05.876+09:00".into(),
            width: 256,
            height: 192,
            kelvin_per_count: Some(0.015625),
            unit: "°C",
            min: 13.21,
            max: 29.53,
            mean: 19.97,
            palette: "Ironbow".into(),
            range: (13.0, 30.0),
            spots: vec![SpotReading {
                id: 2,
                x: 10,
                y: 20,
                value: 21.5,
            }],
            shapes: vec![ShapeReading {
                label: "E3".into(),
                kind: "Ellipse".into(),
                a: (1, 1),
                b: (9, 9),
                min: 15.0,
                max: 25.0,
                mean: 20.0,
            }],
            deltas: vec![DeltaReading {
                label: "화면 최고 − S2".into(),
                value: 8.03,
            }],
            noise_filter: None,
            difference: None,
            device: Some("P3 <test>".into()),
            lepton_part: None,
            fpa_c: None,
            shutter_mode: None,
            calibration: None,
            webcam: None,
            upscale: None,
            ai_model: None,
        }
    }

    #[test]
    fn holds_the_readings_and_escapes_text() {
        let m = meta();
        let page = html(
            &m,
            &Extras {
                view_png: b"\x89PNG",
                webcam_png: None,
                calibration: Some("방사율 0.95".into()),
                base: "thermal_20261128_160805_876",
            },
        );
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("2026-11-28 16:08:05"));
        assert!(page.contains("P3 &lt;test&gt;"), "escaped");
        assert!(!page.contains("P3 <test>"));
        assert!(page.contains("src=\"data:image/png;base64,iVBORw==\""));
        assert_eq!(page.matches("<img").count(), 1, "no webcam picture");
        for text in [
            "29.53 °C",
            "S2 (10, 20)",
            "E3 원",
            "25.00 °C",
            "화면 최고 − S2",
            "+8.03 °C",
            "방사율 0.95",
            "13.0 ~ 30.0 °C",
            "thermal_20261128_160805_876_raw16.png",
        ] {
            assert!(page.contains(text), "{text}");
        }
        assert_eq!(
            path_for(Path::new(r"C:\x\thermal_1.png")),
            PathBuf::from(r"C:\x\thermal_1_report.html")
        );
    }
}
