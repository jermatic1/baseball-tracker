use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::Value;

use crate::TrackerError;

#[derive(Debug, Clone, Copy)]
pub struct StereoCalib {
    pub fx: f64,
    pub baseline_m: f64,
}

impl StereoCalib {
    pub fn nominal(width: u32) -> Self {
        let hfov = 80.0_f64.to_radians();
        let fx = (width as f64 * 0.5) / (hfov * 0.5).tan();
        Self {
            fx,
            baseline_m: 0.075,
        }
    }
}

pub fn stereo_calib_from_device(json: Option<&str>, width: u32) -> StereoCalib {
    let Some(json) = json else {
        return StereoCalib::nominal(width);
    };
    parse_eeprom(json, width).unwrap_or_else(|_| StereoCalib::nominal(width))
}

fn parse_eeprom(json: &str, width: u32) -> Result<StereoCalib, TrackerError> {
    let v: Value = serde_json::from_str(json)?;
    let cameras = v
        .get("cameraData")
        .and_then(|c| c.as_array())
        .ok_or_else(|| TrackerError::Other("calibration has no cameraData".into()))?;
    let mut left_fx = None;
    let mut left_w = None;
    let mut left_tx = None;
    let mut right_tx = None;
    for entry in cameras {
        let Some(pair) = entry.as_array() else {
            continue;
        };
        let Some(socket) = pair.first().and_then(|s| s.as_i64()) else {
            continue;
        };
        let Some(cam) = pair.get(1) else {
            continue;
        };
        if socket == 1 {
            left_fx = matrix_fx(cam.get("intrinsicMatrix"));
            left_w = cam.get("width").and_then(|w| w.as_u64());
            left_tx = translation_x(cam.get("extrinsics"));
        } else if socket == 2 {
            right_tx = translation_x(cam.get("extrinsics"));
        }
    }
    let fx = left_fx.ok_or_else(|| TrackerError::Other("left fx missing".into()))?;
    let tx_l = left_tx.ok_or_else(|| TrackerError::Other("left translation missing".into()))?;
    let tx_r = right_tx.ok_or_else(|| TrackerError::Other("right translation missing".into()))?;
    let baseline_m = baseline_meters(tx_r - tx_l);
    if !(0.02..0.2).contains(&baseline_m) || !fx.is_finite() || fx < 100.0 {
        return Err(TrackerError::Other("calibration out of range".into()));
    }
    let calib_w = left_w.unwrap_or(width as u64).max(1) as f64;
    let fx = fx * width as f64 / calib_w;
    Ok(StereoCalib { fx, baseline_m })
}

fn matrix_fx(matrix: Option<&Value>) -> Option<f64> {
    let row = matrix?.as_array()?.first()?;
    let fx = if let Some(row) = row.as_array() {
        row.first()?.as_f64()?
    } else {
        matrix?.as_array()?.first()?.as_f64()?
    };
    fx.is_finite().then_some(fx)
}

fn translation_x(extrinsics: Option<&Value>) -> Option<f64> {
    let ext = extrinsics?;
    let t = ext
        .get("translation")
        .or_else(|| ext.get("specTranslation"))?;
    if let Some(x) = t.get("x").and_then(|v| v.as_f64()) {
        return Some(x);
    }
    t.as_array()?.first()?.as_f64()
}

fn baseline_meters(delta: f64) -> f64 {
    let a = delta.abs();
    if (1.0..30.0).contains(&a) {
        a / 100.0
    } else if (30.0..300.0).contains(&a) {
        a / 1000.0
    } else {
        a
    }
}

pub fn depth_mm(left: &[u8], right: &[u8], w: u32, h: u32, calib: StereoCalib) -> Vec<u16> {
    let n = (w as usize) * (h as usize);
    if left.len() < n || right.len() < n || w < 16 || h < 16 {
        return vec![0; n];
    }
    let step = if w >= 640 { 4 } else { 1 };
    let sw = w / step;
    let sh = h / step;
    let win = 2i32;
    let max_d = (sw / 4).clamp(8, 40);
    let view = View {
        left,
        right,
        w,
        h,
        step,
        win,
    };
    let mut out = vec![0u16; n];
    for y in win as u32..sh.saturating_sub(win as u32) {
        for x in (win as u32 + max_d)..sw.saturating_sub(win as u32) {
            let (best_d, best, second) = best_disparity(&view, x, y, max_d);
            if best_d == 0 || (second != u32::MAX && best as u64 * 10 > second as u64 * 8) {
                continue;
            }
            let d_sub = subpixel(&view, x, y, best_d, max_d, best);
            let disparity = d_sub * step as f64;
            if disparity < 1.0 {
                continue;
            }
            let z_mm = calib.fx * calib.baseline_m * 1000.0 / disparity;
            if !(200.0..20_000.0).contains(&z_mm) {
                continue;
            }
            let mm = z_mm.round() as u16;
            fill_block(&mut out, w, h, x * step, y * step, step, mm);
        }
    }
    out
}

struct View<'a> {
    left: &'a [u8],
    right: &'a [u8],
    w: u32,
    h: u32,
    step: u32,
    win: i32,
}

fn best_disparity(view: &View, x: u32, y: u32, max_d: u32) -> (u32, u32, u32) {
    let mut best = u32::MAX;
    let mut second = u32::MAX;
    let mut best_d = 0u32;
    for d in 1..max_d {
        let sad = sad(view, x, y, d);
        if sad < best {
            second = best;
            best = sad;
            best_d = d;
        } else if sad < second {
            second = sad;
        }
    }
    (best_d, best, second)
}

fn subpixel(view: &View, x: u32, y: u32, best_d: u32, max_d: u32, best: u32) -> f64 {
    if best_d == 0 || best_d + 1 >= max_d {
        return best_d as f64;
    }
    let lo = sad(view, x, y, best_d - 1) as f64;
    let hi = sad(view, x, y, best_d + 1) as f64;
    let mid = best as f64;
    let denom = lo + hi - 2.0 * mid;
    if denom.abs() < 1e-6 {
        return best_d as f64;
    }
    let delta = (0.5 * (lo - hi) / denom).clamp(-0.5, 0.5);
    best_d as f64 + delta
}

fn sad(view: &View, x: u32, y: u32, d: u32) -> u32 {
    let mut sum = 0u32;
    for dy in -view.win..=view.win {
        for dx in -view.win..=view.win {
            let yy = (y as i32 + dy) as u32;
            let xl = (x as i32 + dx) as u32;
            let xr = xl - d;
            let l = sample(view.left, view.w, view.h, view.step, xl, yy);
            let r = sample(view.right, view.w, view.h, view.step, xr, yy);
            sum += l.abs_diff(r) as u32;
        }
    }
    sum
}

fn sample(img: &[u8], w: u32, h: u32, step: u32, x: u32, y: u32) -> u8 {
    let px = (x * step).min(w - 1);
    let py = (y * step).min(h - 1);
    img[(py * w + px) as usize]
}

fn fill_block(out: &mut [u16], w: u32, h: u32, x: u32, y: u32, step: u32, mm: u16) {
    for dy in 0..step {
        for dx in 0..step {
            let xx = x + dx;
            let yy = y + dy;
            if xx >= w || yy >= h {
                continue;
            }
            out[(yy * w + xx) as usize] = mm;
        }
    }
}

pub fn write_depth_file(
    dir: &Path,
    width: u32,
    height: u32,
    count: usize,
    calib: StereoCalib,
    depth: &mut File,
) -> Result<(), TrackerError> {
    let n = (width as usize) * (height as usize);
    let mut left_f = File::open(dir.join("left.gray"))?;
    let mut right_f = File::open(dir.join("right.gray"))?;
    depth.seek(SeekFrom::Start(0))?;
    let mut left = vec![0u8; n];
    let mut right = vec![0u8; n];
    let mut raw = vec![0u8; n * 2];
    for _ in 0..count {
        left_f.read_exact(&mut left)?;
        right_f.read_exact(&mut right)?;
        let mm = depth_mm(&left, &right, width, height, calib);
        for (i, v) in mm.iter().enumerate() {
            raw[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        depth.write_all(&raw)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_centimeter_baseline() {
        let json = r#"{
            "cameraData": [
                [1, {"width": 1280, "intrinsicMatrix": [[800.0, 0, 640], [0, 800, 400], [0, 0, 1]],
                     "extrinsics": {"translation": {"x": -3.75, "y": 0, "z": 0}}}],
                [2, {"width": 1280, "intrinsicMatrix": [[800.0, 0, 640], [0, 800, 400], [0, 0, 1]],
                     "extrinsics": {"translation": {"x": 3.75, "y": 0, "z": 0}}}]
            ]
        }"#;
        let c = stereo_calib_from_device(Some(json), 1280);
        assert!((c.fx - 800.0).abs() < 1.0);
        assert!((c.baseline_m - 0.075).abs() < 1e-6);
    }

    #[test]
    fn shifted_square_has_expected_depth() {
        let w = 64u32;
        let h = 32u32;
        let mut left = vec![0u8; (w * h) as usize];
        let mut right = vec![0u8; (w * h) as usize];
        paint(&mut left, w, 36, 12);
        paint(&mut right, w, 28, 12);
        let calib = StereoCalib {
            fx: 100.0,
            baseline_m: 0.1,
        };
        let depth = depth_mm(&left, &right, w, h, calib);
        let z = depth[(15 * w + 39) as usize];
        assert!((z as i32 - 1250).abs() < 200, "depth {z}");
    }

    fn paint(img: &mut [u8], w: u32, ox: u32, oy: u32) {
        for y in 0..7 {
            for x in 0..7 {
                img[((oy + y) * w + ox + x) as usize] = (30 + x * 17 + y * 9) as u8;
            }
        }
    }
}
