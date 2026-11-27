//! AI upscaling of snapshots with Upscayl, after stared/thermal-upscale (docs/DEVELOPMENT.md,
//! stage 10).
//!
//! Upscayl is a separate program the user installs (AGPL-3.0); ThermalView only starts its
//! `upscayl-bin` as an external process, so nothing of it is linked or shipped. One image takes
//! about 0.4–2 s, too slow for live video, so this is for snapshots only.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Models tried in order. thermal-upscale's choice for images made from raw data, after Wiener
/// sharpening: "lite" (= RealESRGAN General v3), soft but faithful. "standard" is sharper but on
/// an integrated GPU took 32–36 s per snapshot here against 4.6 s.
pub const MODELS: [&str; 2] = ["upscayl-lite-4x", "upscayl-standard-4x"];

/// An installed Upscayl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upscayl {
    pub bin: PathBuf,
    pub models: PathBuf,
    pub model: String,
}

/// Look in the per-user and per-machine install folders, then on PATH.
pub fn find() -> Option<Upscayl> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    for root in [
        env("LOCALAPPDATA").map(|p| p.join("Programs").join("upscayl")),
        env("ProgramFiles").map(|p| p.join("Upscayl")),
    ]
    .into_iter()
    .flatten()
    {
        let res = root.join("resources");
        candidates.push((res.join("bin").join("upscayl-bin.exe"), res.join("models")));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let models = dir.parent().map(|p| p.join("models")).unwrap_or_default();
            candidates.push((dir.join("upscayl-bin.exe"), models));
        }
    }
    candidates
        .into_iter()
        .find_map(|(bin, models)| at(&bin, &models))
}

/// An Upscayl made of this program and model folder, if both are there.
pub fn at(bin: &Path, models: &Path) -> Option<Upscayl> {
    if !bin.is_file() {
        return None;
    }
    let model = MODELS.iter().find(|m| {
        models.join(format!("{m}.param")).is_file() && models.join(format!("{m}.bin")).is_file()
    })?;
    Some(Upscayl {
        bin: bin.to_path_buf(),
        models: models.to_path_buf(),
        model: (*model).to_owned(),
    })
}

/// Where the AI-upscaled copy of a snapshot's color PNG goes.
pub fn output_for(color_png: &Path) -> PathBuf {
    let stem = color_png
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    color_png.with_file_name(format!("{stem}_ai_x4.png"))
}

/// The sensor image the network gets: raw counts Wiener-sharpened (σ 0.81, the strong setting,
/// as thermal-upscale does), colored with the view's palette and range.
pub struct Input {
    pub raw: Vec<u16>,
    pub width: usize,
    pub height: usize,
    /// Raw counts at the bottom and top of the palette.
    pub lo: u16,
    pub hi: u16,
    pub lut: [[u8; 3]; 256],
}

impl Input {
    /// Write the input PNG.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        use crate::thermal::upscale::{Sharpen, Upscaler};
        let sharp = Upscaler::new(Sharpen::Strong).deconvolve(&self.raw, self.width, self.height);
        let mut rgba = Vec::new();
        crate::thermal::palette::colorize_f32(&sharp, self.lo, self.hi, &self.lut, &mut rgba);
        let img = image::RgbaImage::from_raw(self.width as u32, self.height as u32, rgba)
            .ok_or_else(|| anyhow::anyhow!("input image size mismatch"))?;
        image::DynamicImage::ImageRgba8(img).to_rgb8().save(path)?;
        Ok(())
    }
}

/// Make `output` from `input`: write the sharpened sensor image next to it, upscale, remove the
/// temporary image. Blocks for seconds.
pub fn make(u: &Upscayl, input: &Input, output: &Path) -> anyhow::Result<()> {
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = output.with_file_name(format!("{stem}_in.png"));
    input.save(&tmp)?;
    let r = u.run(&tmp, output);
    let _ = std::fs::remove_file(&tmp);
    r
}

impl Upscayl {
    /// Upscale the PNG `input` 4× into the PNG `output`. Blocks for a second or two: call it off
    /// the UI thread.
    pub fn run(&self, input: &Path, output: &Path) -> anyhow::Result<()> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("-i")
            .arg(input)
            .arg("-o")
            .arg(output)
            .arg("-m")
            .arg(&self.models)
            .args(["-n", &self.model, "-s", "4", "-f", "png"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("Upscayl을 실행할 수 없습니다: {e}"))?;
        if !out.status.success() || !output.is_file() {
            let msg = String::from_utf8_lossy(if out.stderr.is_empty() {
                &out.stdout
            } else {
                &out.stderr
            })
            .trim()
            .chars()
            .rev()
            .take(300)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
            anyhow::bail!("Upscayl 실패 ({}): {msg}", out.status);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stand-in `upscayl-bin` (batch file) that copies its input to its output, or fails, and
    /// a model folder with the default model.
    pub(crate) fn fake(dir: &Path, works: bool) -> Upscayl {
        std::fs::create_dir_all(dir.join("models")).unwrap();
        for ext in ["param", "bin"] {
            std::fs::write(
                dir.join("models").join(format!("{}.{ext}", MODELS[0])),
                b"x",
            )
            .unwrap();
        }
        let bin = dir.join("upscayl-bin.cmd");
        // Arguments: -i <in> -o <out> -m <models> -n <model> -s 4 -f png
        let body = if works {
            "@copy /y %2 %4 >nul\r\n"
        } else {
            "@echo model failed 1>&2\r\n@exit /b 3\r\n"
        };
        std::fs::write(&bin, body).unwrap();
        at(&bin, &dir.join("models")).unwrap()
    }

    #[test]
    fn finds_the_model_and_runs_the_program() {
        let dir = std::env::temp_dir().join(format!("thermalview_upscayl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(at(&dir.join("none.exe"), &dir), None);

        let u = fake(&dir.join("ok"), true);
        assert_eq!(u.model, "upscayl-lite-4x");
        let input = dir.join("thermal_1.png");
        std::fs::write(&input, b"png").unwrap();
        let output = output_for(&input);
        assert_eq!(output.file_name().unwrap(), "thermal_1_ai_x4.png");
        u.run(&input, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"png");

        let bad = fake(&dir.join("bad"), false);
        let e = bad.run(&input, &dir.join("never.png")).unwrap_err();
        assert!(e.to_string().contains("model failed"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
