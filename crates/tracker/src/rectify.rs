//! Stereo rectification from the camera's own factory calibration.
//!
//! The OAK's EEPROM holds each eye's intrinsics, distortion, the relative
//! extrinsics, and the rectification rotations. Remapping both eyes through
//! them lines the rows up and removes the fixed disparity offset the raw
//! frames show, so point depth needs no hand calibration.

use serde_json::Value;

use crate::stereo::{stereo_baseline_m, StereoCalib};
use crate::TrackerError;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Eye {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pinhole {
    fx: f64,
    fy: f64,
    cx: f64,
    cy: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct EyeModel {
    k: Pinhole,
    /// OpenCV order: k1 k2 p1 p2 k3 k4 k5 k6, anything beyond is ignored.
    dist: [f64; 8],
    /// Rectification rotation: rectified ray = r * raw ray.
    r: [[f64; 3]; 3],
}

/// Per-eye remap tables for one capture size.
#[derive(Debug, Clone)]
pub struct Rectifier {
    width: u32,
    height: u32,
    /// Common intrinsics of the rectified pair.
    k: Pinhole,
    /// Kept for mapping raw left pixels into the rectified image.
    left: EyeModel,
    baseline_m: f64,
    /// Raw sample position for every rectified pixel, per eye.
    map_left: Vec<(f32, f32)>,
    map_right: Vec<(f32, f32)>,
}

impl Rectifier {
    /// Build from the device calibration JSON for frames of `width` x `height`.
    pub fn from_json(json: &str, width: u32, height: u32) -> Result<Self, TrackerError> {
        let v: Value = serde_json::from_str(json)?;
        let rect = v
            .get("stereoRectificationData")
            .ok_or_else(|| TrackerError::Other("no stereoRectificationData".into()))?;
        let left_socket = rect
            .get("leftCameraSocket")
            .and_then(|s| s.as_i64())
            .unwrap_or(1);
        let right_socket = rect
            .get("rightCameraSocket")
            .and_then(|s| s.as_i64())
            .unwrap_or(2);
        let (left_cam, left_w, left_h) = camera_entry(&v, left_socket)?;
        let (right_cam, _, _) = camera_entry(&v, right_socket)?;
        let sx = f64::from(width) / left_w;
        let sy = f64::from(height) / left_h;
        let left = EyeModel {
            k: scaled_pinhole(left_cam, sx, sy)?,
            dist: distortion(left_cam),
            r: matrix3(rect.get("rectifiedRotationLeft"))?,
        };
        let right = EyeModel {
            k: scaled_pinhole(right_cam, sx, sy)?,
            dist: distortion(right_cam),
            r: matrix3(rect.get("rectifiedRotationRight"))?,
        };
        let cameras = v
            .get("cameraData")
            .and_then(|c| c.as_array())
            .ok_or_else(|| TrackerError::Other("calibration has no cameraData".into()))?;
        let baseline_m = stereo_baseline_m(cameras, left_socket, right_socket)?;
        let k = left.k;
        let map_left = build_map(&left, k, width, height);
        let map_right = build_map(&right, k, width, height);
        Ok(Self {
            width,
            height,
            k,
            left,
            baseline_m,
            map_left,
            map_right,
        })
    }

    /// Calibration of the rectified pair: no disparity offset by construction.
    pub fn calib(&self) -> StereoCalib {
        StereoCalib {
            fx: self.k.fx,
            baseline_m: self.baseline_m,
            disparity_offset_px: 0.0,
        }
    }

    pub fn rectify(&self, raw: &[u8], eye: Eye) -> Vec<u8> {
        let map = match eye {
            Eye::Left => &self.map_left,
            Eye::Right => &self.map_right,
        };
        map.iter()
            .map(|&(x, y)| sample_bilinear(raw, self.width, self.height, x, y))
            .collect()
    }

    /// Where a raw left-eye pixel lands in the rectified left image.
    pub fn to_rectified_left(&self, u: f64, v: f64) -> (f64, f64) {
        let m = &self.left;
        let xd = (u - m.k.cx) / m.k.fx;
        let yd = (v - m.k.cy) / m.k.fy;
        let (xu, yu) = undistort(xd, yd, &m.dist);
        let ray = mul3(&m.r, [xu, yu, 1.0]);
        (
            self.k.fx * ray[0] / ray[2] + self.k.cx,
            self.k.fy * ray[1] / ray[2] + self.k.cy,
        )
    }
}

fn camera_entry(v: &Value, socket: i64) -> Result<(&Value, f64, f64), TrackerError> {
    let cameras = v
        .get("cameraData")
        .and_then(|c| c.as_array())
        .ok_or_else(|| TrackerError::Other("calibration has no cameraData".into()))?;
    for entry in cameras {
        let Some(pair) = entry.as_array() else {
            continue;
        };
        if pair.first().and_then(|s| s.as_i64()) != Some(socket) {
            continue;
        }
        let cam = pair
            .get(1)
            .ok_or_else(|| TrackerError::Other("bad cameraData entry".into()))?;
        let w = cam.get("width").and_then(|x| x.as_f64()).unwrap_or(1280.0);
        let h = cam.get("height").and_then(|x| x.as_f64()).unwrap_or(800.0);
        return Ok((cam, w, h));
    }
    Err(TrackerError::Other(format!(
        "socket {socket} not in calibration"
    )))
}

fn scaled_pinhole(cam: &Value, sx: f64, sy: f64) -> Result<Pinhole, TrackerError> {
    let m = matrix3(cam.get("intrinsicMatrix"))?;
    Ok(Pinhole {
        fx: m[0][0] * sx,
        fy: m[1][1] * sy,
        cx: m[0][2] * sx,
        cy: m[1][2] * sy,
    })
}

fn distortion(cam: &Value) -> [f64; 8] {
    let mut out = [0.0; 8];
    if let Some(arr) = cam.get("distortionCoeff").and_then(|d| d.as_array()) {
        for (slot, v) in out.iter_mut().zip(arr) {
            *slot = v.as_f64().unwrap_or(0.0);
        }
    }
    out
}

fn matrix3(v: Option<&Value>) -> Result<[[f64; 3]; 3], TrackerError> {
    let rows = v
        .and_then(|m| m.as_array())
        .ok_or_else(|| TrackerError::Other("missing 3x3 matrix".into()))?;
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        let cols = rows
            .get(r)
            .and_then(|x| x.as_array())
            .ok_or_else(|| TrackerError::Other("short matrix".into()))?;
        for (c, slot) in row.iter_mut().enumerate() {
            *slot = cols
                .get(c)
                .and_then(|x| x.as_f64())
                .ok_or_else(|| TrackerError::Other("bad matrix value".into()))?;
        }
    }
    Ok(out)
}

fn mul3(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn transpose(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut t = [[0.0; 3]; 3];
    for (r, row) in m.iter().enumerate() {
        for (c, &v) in row.iter().enumerate() {
            t[c][r] = v;
        }
    }
    t
}

/// OpenCV rational model: distorted normalised coordinates from ideal ones.
fn distort(x: f64, y: f64, d: &[f64; 8]) -> (f64, f64) {
    let [k1, k2, p1, p2, k3, k4, k5, k6] = *d;
    let r2 = x * x + y * y;
    let radial = (1.0 + k1 * r2 + k2 * r2 * r2 + k3 * r2 * r2 * r2)
        / (1.0 + k4 * r2 + k5 * r2 * r2 + k6 * r2 * r2 * r2);
    (
        x * radial + 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x),
        y * radial + p1 * (r2 + 2.0 * y * y) + 2.0 * p2 * x * y,
    )
}

/// Inverse of `distort` by fixed-point iteration; converges fast for lens
/// distortion of this size.
fn undistort(xd: f64, yd: f64, d: &[f64; 8]) -> (f64, f64) {
    let (mut x, mut y) = (xd, yd);
    for _ in 0..8 {
        let (dx, dy) = distort(x, y, d);
        x += xd - dx;
        y += yd - dy;
    }
    (x, y)
}

/// For each rectified pixel: rotate its ray back to the raw camera, distort,
/// and project with the raw intrinsics.
fn build_map(eye: &EyeModel, k: Pinhole, width: u32, height: u32) -> Vec<(f32, f32)> {
    let rt = transpose(&eye.r);
    let mut map = Vec::with_capacity((width * height) as usize);
    for v in 0..height {
        for u in 0..width {
            let ray = mul3(
                &rt,
                [
                    (f64::from(u) - k.cx) / k.fx,
                    (f64::from(v) - k.cy) / k.fy,
                    1.0,
                ],
            );
            if ray[2] <= 1e-9 {
                map.push((-1.0, -1.0));
                continue;
            }
            let (xd, yd) = distort(ray[0] / ray[2], ray[1] / ray[2], &eye.dist);
            map.push((
                (eye.k.fx * xd + eye.k.cx) as f32,
                (eye.k.fy * yd + eye.k.cy) as f32,
            ));
        }
    }
    map
}

fn sample_bilinear(img: &[u8], w: u32, h: u32, x: f32, y: f32) -> u8 {
    if x < 0.0 || y < 0.0 || x >= (w - 1) as f32 || y >= (h - 1) as f32 {
        return 0;
    }
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: u32, yy: u32| img[(yy * w + xx) as usize] as f32;
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
    let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
    (top * (1.0 - fy) + bottom * fy).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot_y(deg: f64) -> [[f64; 3]; 3] {
        let (s, c) = deg.to_radians().sin_cos();
        [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
    }

    fn json(r_left: [[f64; 3]; 3], r_right: [[f64; 3]; 3]) -> String {
        let k = [[760.0, 0.0, 640.0], [0.0, 760.0, 400.0], [0.0, 0.0, 1.0]];
        serde_json::json!({
            "cameraData": [
                [1, {"width": 1280, "height": 800, "intrinsicMatrix": k, "distortionCoeff": [0.05, -0.1, 0.0, 0.0, 0.0],
                     "extrinsics": {"translation": {"x": -7.5, "y": 0.0, "z": 0.0}, "toCameraSocket": 2}}],
                [2, {"width": 1280, "height": 800, "intrinsicMatrix": k, "distortionCoeff": [0.05, -0.1, 0.0, 0.0, 0.0],
                     "extrinsics": {"translation": {"x": 0.0, "y": 0.0, "z": 0.0}, "toCameraSocket": -1}}]
            ],
            "stereoRectificationData": {
                "leftCameraSocket": 1, "rightCameraSocket": 2,
                "rectifiedRotationLeft": r_left, "rectifiedRotationRight": r_right
            }
        })
        .to_string()
    }

    /// Raw pixel of a point for an eye whose frame is `r_cam_from_left`
    /// applied to left-camera coordinates, with distortion.
    fn raw_pixel(p_left: [f64; 3], r_cam: &[[f64; 3]; 3], k: Pinhole, d: &[f64; 8]) -> (f64, f64) {
        let p = mul3(r_cam, p_left);
        let (xd, yd) = distort(p[0] / p[2], p[1] / p[2], d);
        (k.fx * xd + k.cx, k.fy * yd + k.cy)
    }

    fn paint_disc(img: &mut [u8], w: u32, h: u32, cx: f64, cy: f64) {
        for y in 0..h {
            for x in 0..w {
                if (f64::from(x) - cx).powi(2) + (f64::from(y) - cy).powi(2) <= 36.0 {
                    img[(y * w + x) as usize] = 220;
                }
            }
        }
    }

    fn centroid(img: &[u8], w: u32) -> (f64, f64) {
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for (i, &v) in img.iter().enumerate() {
            if v > 100 {
                sx += (i as u32 % w) as f64;
                sy += (i as u32 / w) as f64;
                n += 1.0;
            }
        }
        (sx / n, sy / n)
    }

    #[test]
    fn rectified_pair_is_row_aligned_with_true_disparity() {
        // Right eye yawed 2 degrees relative to left; rectify with
        // R_left = I and R_right = R_rel^T so both end up parallel.
        let r_rel = rot_y(2.0);
        let rect = Rectifier::from_json(
            &json(
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                transpose(&r_rel),
            ),
            640,
            400,
        )
        .unwrap();
        let (w, h) = (640u32, 400u32);
        let k = Pinhole {
            fx: 380.0,
            fy: 380.0,
            cx: 320.0,
            cy: 200.0,
        };
        let d = [0.05, -0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        // A point 1.6 m ahead, slightly up and left, in left-camera coordinates.
        let p = [-0.2, -0.1, 1.6];
        // The right eye sits 7.5 cm to the right, so the point is further left in it.
        let p_right = [p[0] - 0.075, p[1], p[2]];
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let (ul, vl) = raw_pixel(p, &identity, k, &d);
        let (ur, vr) = raw_pixel(p_right, &r_rel, k, &d);
        let mut left = vec![20u8; (w * h) as usize];
        let mut right = vec![20u8; (w * h) as usize];
        paint_disc(&mut left, w, h, ul, vl);
        paint_disc(&mut right, w, h, ur, vr);
        // Raw rows differ by several pixels and the raw shift is not the
        // true disparity; after rectification both are right.
        assert!((vl - vr).abs() > 1.0 || ((ul - ur) - 380.0 * 0.075 / 1.6).abs() > 1.0);
        let rl = rect.rectify(&left, Eye::Left);
        let rr = rect.rectify(&right, Eye::Right);
        let (cl, dl) = centroid(&rl, w);
        let (cr, dr) = centroid(&rr, w);
        assert!((dl - dr).abs() < 0.5, "rows {dl} vs {dr}");
        let expected = rect.calib().fx * rect.calib().baseline_m / 1.6;
        assert!(
            ((cl - cr) - expected).abs() < 0.5,
            "disparity {} vs {expected}",
            cl - cr
        );
        let (mu, mv) = rect.to_rectified_left(ul, vl);
        assert!(
            (mu - cl).abs() < 0.7 && (mv - dl).abs() < 0.7,
            "map ({mu},{mv}) vs ({cl},{dl})"
        );
    }
}
