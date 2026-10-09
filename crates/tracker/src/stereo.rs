use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::Value;

use crate::TrackerError;

#[derive(Debug, Clone, Copy)]
pub struct StereoCalib {
    pub fx: f64,
    pub baseline_m: f64,
    /// Disparity the raw, unrectified eyes show for an object at infinity.
    pub disparity_offset_px: f64,
}

impl StereoCalib {
    pub fn nominal(width: u32) -> Self {
        let hfov = 80.0_f64.to_radians();
        let fx = (width as f64 * 0.5) / (hfov * 0.5).tan();
        Self {
            fx,
            baseline_m: 0.075,
            disparity_offset_px: 0.0,
        }
    }

    pub fn with_offset(self, disparity_offset_px: f64) -> Self {
        Self {
            disparity_offset_px,
            ..self
        }
    }

    pub fn depth_m(&self, disparity_px: f64) -> Option<f64> {
        let d = disparity_px - self.disparity_offset_px;
        if d < 1.0 {
            return None;
        }
        let z = self.fx * self.baseline_m / d;
        (0.2..20.0).contains(&z).then_some(z)
    }
}

/// Full-resolution depth of one point, matched in a window around it with a
/// small vertical search for eye misalignment. Used for detected balls, where
/// the coarse depth file is too quantized.
pub fn depth_at(
    left: &[u8],
    right: &[u8],
    w: u32,
    h: u32,
    cx: f64,
    cy: f64,
    calib: StereoCalib,
) -> Option<f64> {
    const WIN: i32 = 5;
    const DY: i32 = 2;
    const MAX_D: i32 = 120;
    let n = (w as usize) * (h as usize);
    if left.len() < n || right.len() < n {
        return None;
    }
    let x = cx.round() as i32;
    let y = cy.round() as i32;
    let max_d = MAX_D.min(x - WIN - 1);
    if max_d < 3 || y - WIN - DY < 0 || y + WIN + DY >= h as i32 || x + WIN >= w as i32 {
        return None;
    }
    let cost = |d: i32, dy: i32| -> u32 {
        let mut sum = 0u32;
        for yy in -WIN..=WIN {
            let yl = (y + yy) as u32;
            let yr = (y + yy + dy) as u32;
            for xx in -WIN..=WIN {
                let xl = (x + xx) as u32;
                let xr = (x + xx - d) as u32;
                let l = left[(yl * w + xl) as usize];
                let r = right[(yr * w + xr) as usize];
                sum += l.abs_diff(r) as u32;
            }
        }
        sum
    };
    let mut costs = Vec::with_capacity(((2 * DY + 1) * max_d) as usize);
    for dy in -DY..=DY {
        for d in 1..max_d {
            costs.push((cost(d, dy), d, dy));
        }
    }
    let &(best_cost, d, dy) = costs.iter().min_by_key(|c| c.0)?;
    // Reject a match that a clearly different disparity explains nearly as well.
    let rival = costs
        .iter()
        .filter(|c| (c.1 - d).abs() > 2)
        .map(|c| c.0)
        .min()
        .unwrap_or(u32::MAX);
    if rival != u32::MAX && best_cost as u64 * 10 > rival as u64 * 8 {
        return None;
    }
    let mut disparity = d as f64;
    if d > 1 && d + 1 < max_d {
        let lo = cost(d - 1, dy) as f64;
        let hi = cost(d + 1, dy) as f64;
        let denom = lo + hi - 2.0 * best_cost as f64;
        if denom.abs() > 1e-6 {
            disparity += (0.5 * (lo - hi) / denom).clamp(-0.5, 0.5);
        }
    }
    calib.depth_m(disparity)
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
    let (left_socket, right_socket) = stereo_sockets(&v);
    let left = camera_json(cameras, left_socket)
        .ok_or_else(|| TrackerError::Other("left camera missing from calibration".into()))?;
    let fx = matrix_fx(left.get("intrinsicMatrix"))
        .ok_or_else(|| TrackerError::Other("left fx missing".into()))?;
    let left_w = left.get("width").and_then(|w| w.as_u64());
    let baseline_m = stereo_baseline_m(cameras, left_socket, right_socket)?;
    if !fx.is_finite() || fx < 100.0 {
        return Err(TrackerError::Other("calibration fx out of range".into()));
    }
    let calib_w = left_w.unwrap_or(width as u64).max(1) as f64;
    let fx = fx * width as f64 / calib_w;
    Ok(StereoCalib {
        fx,
        baseline_m,
        disparity_offset_px: 0.0,
    })
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

/// The stereo pair's sockets: from `stereoRectificationData` when present,
/// else depthai's conventional left 1 and right 2.
pub(crate) fn stereo_sockets(root: &Value) -> (i64, i64) {
    let rect = root.get("stereoRectificationData");
    let get = |k: &str| rect.and_then(|r| r.get(k)).and_then(|s| s.as_i64());
    (
        get("leftCameraSocket").unwrap_or(1),
        get("rightCameraSocket").unwrap_or(2),
    )
}

/// `cameraData` entry for a socket; the map serializes as `[socket, info]` pairs.
pub(crate) fn camera_json(cameras: &[Value], socket: i64) -> Option<&Value> {
    cameras.iter().find_map(|entry| {
        let pair = entry.as_array()?;
        (pair.first()?.as_i64()? == socket)
            .then(|| pair.get(1))
            .flatten()
    })
}

/// Distance between the two eyes from the extrinsics chain. Each camera
/// stores its translation to one other camera (`toCameraSocket`), so the
/// baseline is the direct link when one exists, else the difference of the
/// two links when both point at the same third camera.
pub(crate) fn stereo_baseline_m(
    cameras: &[Value],
    left_socket: i64,
    right_socket: i64,
) -> Result<f64, TrackerError> {
    let link = |socket: i64| -> Option<([f64; 3], i64)> {
        let ext = camera_json(cameras, socket)?.get("extrinsics")?;
        let t = ext.get("translation")?;
        let v = [
            t.get("x")?.as_f64()?,
            t.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
            t.get("z").and_then(|v| v.as_f64()).unwrap_or(0.0),
        ];
        Some((v, ext.get("toCameraSocket")?.as_i64()?))
    };
    let norm = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let (left, right) = (link(left_socket), link(right_socket));
    let delta = match (left, right) {
        (Some((t, to)), _) if to == right_socket => norm(t),
        (_, Some((t, to))) if to == left_socket => norm(t),
        (Some((tl, to_l)), Some((tr, to_r))) if to_l == to_r => {
            norm([tl[0] - tr[0], tl[1] - tr[1], tl[2] - tr[2]])
        }
        _ => {
            return Err(TrackerError::Other(
                "calibration extrinsics do not link the stereo pair".into(),
            ))
        }
    };
    let m = baseline_meters(delta);
    if !(0.02..0.2).contains(&m) {
        return Err(TrackerError::Other(format!(
            "baseline {m:.3} m out of range"
        )));
    }
    Ok(m)
}

/// depthai stores translations in centimetres; accept millimetres too.
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
            let Some(z) = calib.depth_m(d_sub * step as f64) else {
                continue;
            };
            let mm = (z * 1000.0).round() as u16;
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
                     "extrinsics": {"translation": {"x": -3.75, "y": 0, "z": 0}, "toCameraSocket": 0}}],
                [2, {"width": 1280, "intrinsicMatrix": [[800.0, 0, 640], [0, 800, 400], [0, 0, 1]],
                     "extrinsics": {"translation": {"x": 3.75, "y": 0, "z": 0}, "toCameraSocket": 0}}]
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
            disparity_offset_px: 0.0,
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

    #[test]
    fn baseline_follows_the_extrinsics_chain() {
        // Both eyes link to the colour camera: left 3.75 cm one way, right
        // 3.75 cm the other, 7.5 cm apart.
        let via_third = serde_json::json!([
            [0, {"extrinsics": {"translation": {"x": 0.0, "y": 0.0, "z": 0.0}, "toCameraSocket": -1}}],
            [1, {"extrinsics": {"translation": {"x": 3.75, "y": 0.02, "z": 0.0}, "toCameraSocket": 0}}],
            [2, {"extrinsics": {"translation": {"x": -3.75, "y": 0.0, "z": 0.0}, "toCameraSocket": 0}}]
        ]);
        let b = stereo_baseline_m(via_third.as_array().unwrap(), 1, 2).unwrap();
        assert!((b - 0.075).abs() < 0.001, "{b}");
        // Left links straight to right.
        let direct = serde_json::json!([
            [1, {"extrinsics": {"translation": {"x": -7.5, "y": 0.0, "z": 0.0}, "toCameraSocket": 2}}],
            [2, {"extrinsics": {"translation": {"x": -3.75, "y": 0.0, "z": 0.0}, "toCameraSocket": 0}}]
        ]);
        let b = stereo_baseline_m(direct.as_array().unwrap(), 1, 2).unwrap();
        assert!((b - 0.075).abs() < 0.001, "{b}");
        // The old mistake: subtracting unrelated links must not be accepted.
        let unrelated = serde_json::json!([
            [1, {"extrinsics": {"translation": {"x": -7.5, "y": 0.0, "z": 0.0}, "toCameraSocket": 2}}],
            [2, {"extrinsics": {"translation": {"x": -3.75, "y": 0.0, "z": 0.0}, "toCameraSocket": 0}}]
        ]);
        assert!(
            (stereo_baseline_m(unrelated.as_array().unwrap(), 1, 2).unwrap() - 0.075).abs() < 0.001
        );
    }

    #[test]
    fn depth_at_recovers_shift_with_offset() {
        let (w, h) = (160u32, 80u32);
        let mut left = vec![30u8; (w * h) as usize];
        let mut right = vec![30u8; (w * h) as usize];
        // A textured disc at (100, 40) in the left eye, shifted 24 px in the
        // right eye: 8 px of true disparity plus a 16 px rig offset.
        for y in 0..h {
            for x in 0..w {
                let dx = x as i32 - 100;
                let dy = y as i32 - 40;
                if dx * dx + dy * dy <= 100 {
                    let v = 120 + ((x * 7 + y * 13) % 90) as u8;
                    left[(y * w + x) as usize] = v;
                    right[(y * w + x - 24) as usize] = v;
                }
            }
        }
        let calib = StereoCalib {
            fx: 200.0,
            baseline_m: 0.1,
            disparity_offset_px: 16.0,
        };
        let z = depth_at(&left, &right, w, h, 100.0, 40.0, calib).expect("depth");
        assert!((z - 2.5).abs() < 0.05, "z {z}");
        assert!(depth_at(&left, &right, w, h, 100.0, 40.0, calib.with_offset(0.0)).unwrap() < 1.0);
    }
}
