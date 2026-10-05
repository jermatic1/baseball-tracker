//! Home plate detection and the camera pose solved from its corners.
//!
//! A regulation plate is a bright, flat pentagon of known size, so one clean
//! frame of it fixes where the camera is. The pose is fitted directly in the
//! `MountConfig` parameters by minimising reprojection error through
//! `geom::project`, which keeps it consistent with everything downstream.

use std::collections::VecDeque;

use crate::config::{MountConfig, SessionConfig};
use crate::geom::{project, Intrinsics};

const INCH: f64 = 0.0254;

/// Regulation plate corners in the field frame: apex at the origin, front
/// edge toward the pitcher (-Z). Clockwise seen from above: apex, right
/// side, right front, left front, left side.
pub const PLATE_FIELD: [[f64; 3]; 5] = [
    [0.0, 0.0, 0.0],
    [8.5 * INCH, 0.0, -8.5 * INCH],
    [8.5 * INCH, 0.0, -17.0 * INCH],
    [-8.5 * INCH, 0.0, -17.0 * INCH],
    [-8.5 * INCH, 0.0, -8.5 * INCH],
];

/// Plate corners in the image, in outline order. Which one is the apex is
/// decided by the pose fit, since perspective hides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateCorners {
    pub px: [(f64, f64); 5],
}

/// Worst fit accepted as a plate: a wrong shape never fits this well.
pub const MAX_RMS_PX: f64 = 3.0;

#[derive(Debug, Clone, PartialEq)]
pub struct PoseFit {
    pub mount: MountConfig,
    /// Root-mean-square reprojection error of the five corners.
    pub rms_px: f64,
    /// Where the fitted pose puts the corners, in the detected order.
    pub reprojected: [(f64, f64); 5],
}

const MIN_AREA: usize = 150;
const MAX_AREA: usize = 40_000;

/// Find the plate as the best bright pentagon in the frame.
pub fn find_plate(gray: &[u8], w: u32, h: u32) -> Option<PlateCorners> {
    let (w, h) = (w as usize, h as usize);
    if gray.len() < w * h || w < 16 || h < 16 {
        return None;
    }
    let threshold = bright_threshold(gray);
    let mask: Vec<bool> = gray.iter().map(|&v| v >= threshold).collect();
    let mut seen = vec![false; w * h];
    let mut best: Option<(usize, PlateCorners)> = None;
    for start in 0..w * h {
        if !mask[start] || seen[start] {
            continue;
        }
        let pixels = flood(&mask, &mut seen, w, h, start);
        if pixels.len() < MIN_AREA || pixels.len() > MAX_AREA {
            continue;
        }
        let boundary: Vec<(f64, f64)> = pixels
            .iter()
            .filter(|&&i| is_boundary(&mask, w, h, i))
            .map(|&i| ((i % w) as f64, (i / w) as f64))
            .collect();
        let hull = convex_hull(&boundary);
        if hull.len() < 5 {
            continue;
        }
        let perimeter: f64 = hull
            .iter()
            .zip(hull.iter().cycle().skip(1))
            .map(|(a, b)| dist(*a, *b))
            .sum();
        let poly = simplify_closed(&hull, (0.02 * perimeter).max(2.0));
        if poly.len() != 5 {
            continue;
        }
        let px = refine_corners(&boundary, &poly);
        if best.as_ref().is_none_or(|(area, _)| pixels.len() > *area) {
            best = Some((pixels.len(), PlateCorners { px }));
        }
    }
    best.map(|(_, c)| c)
}

/// Fit the mount parameters to the detected corners, starting from a guess
/// (the current config). Every assignment of outline corners to plate
/// corners is tried (five rotations, both directions, both depth-sign
/// conventions); the best fit wins.
pub fn solve_pose(
    corners: &PlateCorners,
    intr: &Intrinsics,
    initial: &MountConfig,
) -> Option<PoseFit> {
    let mut best: Option<PoseFit> = None;
    let mut assignments: Vec<[(f64, f64); 5]> = Vec::new();
    for rotation in 0..5 {
        let mut px = [(0.0, 0.0); 5];
        for (k, slot) in px.iter_mut().enumerate() {
            *slot = corners.px[(rotation + k) % 5];
        }
        assignments.push(px);
        let mut rev = px;
        rev[1..].reverse();
        assignments.push(rev);
    }
    for px in assignments {
        for rf in [true, false] {
            let cost = |p: &[f64; 5]| -> f64 {
                let mount = mount_from(p, rf);
                let mut sum = 0.0;
                for (field, &(u, v)) in PLATE_FIELD.iter().zip(px.iter()) {
                    match project(*field, intr, &mount) {
                        Some((pu, pv, z)) if z > 0.05 => {
                            sum += (pu - u).powi(2) + (pv - v).powi(2);
                        }
                        _ => return 1e12,
                    }
                }
                sum
            };
            let start = [
                initial.distance_from_plate_m,
                initial.height_m,
                initial.lateral_offset_m,
                initial.yaw_deg,
                initial.pitch_deg,
            ];
            let (p, sum) = nelder_mead(cost, start, [0.3, 0.2, 0.3, 10.0, 10.0]);
            let rms = (sum / 5.0).sqrt();
            if best.as_ref().is_none_or(|b| rms < b.rms_px) {
                let mount = mount_from(&p, rf);
                let mut reprojected = [(0.0, 0.0); 5];
                for (i, field) in PLATE_FIELD.iter().enumerate() {
                    if let Some((u, v, _)) = project(*field, intr, &mount) {
                        reprojected[i] = (u, v);
                    }
                }
                best = Some(PoseFit {
                    mount,
                    rms_px: rms,
                    reprojected,
                });
            }
        }
    }
    best
}

/// Detect the plate in a frame and fit the mount, starting from the config.
pub fn calibrate_from_frame(
    gray: &[u8],
    w: u32,
    h: u32,
    cfg: &SessionConfig,
) -> Option<(PlateCorners, PoseFit)> {
    let corners = find_plate(gray, w, h)?;
    let intr = Intrinsics::from_fov(w, h, 80.0, 55.0);
    let fit = solve_pose(&corners, &intr, &cfg.mount)?;
    (fit.rms_px <= MAX_RMS_PX).then_some((corners, fit))
}

fn mount_from(p: &[f64; 5], positive_depth_is_rf: bool) -> MountConfig {
    let distance = p[0].max(0.3);
    MountConfig {
        distance_from_plate_m: distance,
        height_m: p[1],
        lateral_offset_m: p[2].clamp(-distance, distance),
        pitch_deg: p[4],
        yaw_deg: p[3],
        positive_depth_is_rf,
    }
}

fn bright_threshold(gray: &[u8]) -> u8 {
    let mut hist = [0usize; 256];
    for &v in gray {
        hist[v as usize] += 1;
    }
    let target = gray.len() * 9 / 10;
    let mut seen = 0;
    for (v, &n) in hist.iter().enumerate() {
        seen += n;
        if seen >= target {
            return (v as u8).clamp(120, 230);
        }
    }
    230
}

fn flood(mask: &[bool], seen: &mut [bool], w: usize, h: usize, start: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([start]);
    seen[start] = true;
    while let Some(i) = queue.pop_front() {
        out.push(i);
        let (x, y) = (i % w, i / w);
        let neighbours = [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then(|| i + 1),
            (y > 0).then(|| i - w),
            (y + 1 < h).then(|| i + w),
        ];
        for j in neighbours.into_iter().flatten() {
            if mask[j] && !seen[j] {
                seen[j] = true;
                queue.push_back(j);
            }
        }
    }
    out
}

fn is_boundary(mask: &[bool], w: usize, h: usize, i: usize) -> bool {
    let (x, y) = (i % w, i / w);
    x == 0
        || y == 0
        || x + 1 == w
        || y + 1 == h
        || !mask[i - 1]
        || !mask[i + 1]
        || !mask[i - w]
        || !mask[i + w]
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn cross(o: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// Andrew's monotone chain; returns the hull counter-clockwise in image
/// coordinates (y down), without the repeated first point.
fn convex_hull(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut lower: Vec<(f64, f64)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(f64, f64)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Douglas-Peucker on a closed outline: split at the two farthest-apart
/// points, simplify each open half, and join.
fn simplify_closed(outline: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    let n = outline.len();
    let (mut a, mut b, mut far) = (0, 0, 0.0);
    for i in 0..n {
        for j in i + 1..n {
            let d = dist(outline[i], outline[j]);
            if d > far {
                far = d;
                a = i;
                b = j;
            }
        }
    }
    let first: Vec<(f64, f64)> = (a..=b).map(|i| outline[i]).collect();
    let second: Vec<(f64, f64)> = (b..=a + n).map(|i| outline[i % n]).collect();
    let mut out = simplify_open(&first, epsilon);
    out.pop();
    let mut tail = simplify_open(&second, epsilon);
    tail.pop();
    out.extend(tail);
    out
}

fn simplify_open(line: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if line.len() < 3 {
        return line.to_vec();
    }
    let (first, last) = (line[0], line[line.len() - 1]);
    let base = dist(first, last).max(1e-9);
    let (mut idx, mut far) = (0, 0.0);
    for (i, &p) in line.iter().enumerate().skip(1).take(line.len() - 2) {
        let d = cross(first, last, p).abs() / base;
        if d > far {
            far = d;
            idx = i;
        }
    }
    if far <= epsilon {
        return vec![first, last];
    }
    let mut left = simplify_open(&line[..=idx], epsilon);
    let right = simplify_open(&line[idx..], epsilon);
    left.pop();
    left.extend(right);
    left
}

/// Sub-pixel corners: fit a line through the boundary pixels nearest each
/// polygon edge and intersect neighbouring lines. Traced outlines sit half
/// a pixel inside the true edge, which this removes.
fn refine_corners(boundary: &[(f64, f64)], poly: &[(f64, f64)]) -> [(f64, f64); 5] {
    let n = poly.len();
    let centroid = (
        poly.iter().map(|p| p.0).sum::<f64>() / n as f64,
        poly.iter().map(|p| p.1).sum::<f64>() / n as f64,
    );
    let mut lines: Vec<Option<(f64, f64, f64)>> = Vec::with_capacity(n);
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let len = dist(a, b).max(1e-9);
        let pts: Vec<(f64, f64)> = boundary
            .iter()
            .copied()
            .filter(|&q| {
                let t = ((q.0 - a.0) * (b.0 - a.0) + (q.1 - a.1) * (b.1 - a.1)) / (len * len);
                (0.1..=0.9).contains(&t) && cross(a, b, q).abs() / len < 1.5
            })
            .collect();
        // Boundary pixel centres lie half a pixel inside the true edge.
        lines.push(fit_line(&pts).map(|(a, b, c)| {
            let side = (a * centroid.0 + b * centroid.1 + c).signum();
            (a, b, c + side * 0.5)
        }));
    }
    let mut out = [(0.0, 0.0); 5];
    for (i, slot) in out.iter_mut().enumerate() {
        let prev = lines[(i + n - 1) % n];
        let next = lines[i];
        *slot = match (prev, next) {
            (Some(l1), Some(l2)) => intersect(l1, l2).unwrap_or(poly[i]),
            _ => poly[i],
        };
    }
    out
}

/// Total least squares line `a x + b y + c = 0` through points.
fn fit_line(pts: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    if pts.len() < 3 {
        return None;
    }
    let n = pts.len() as f64;
    let (mx, my) = (
        pts.iter().map(|p| p.0).sum::<f64>() / n,
        pts.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for p in pts {
        let (dx, dy) = (p.0 - mx, p.1 - my);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // Normal is the eigenvector of the smaller eigenvalue of the scatter.
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (a, b) = (-theta.sin(), theta.cos());
    Some((a, b, -(a * mx + b * my)))
}

fn intersect(l1: (f64, f64, f64), l2: (f64, f64, f64)) -> Option<(f64, f64)> {
    let det = l1.0 * l2.1 - l2.0 * l1.1;
    if det.abs() < 1e-9 {
        return None;
    }
    Some((
        (l1.1 * l2.2 - l2.1 * l1.2) / det,
        (l2.0 * l1.2 - l1.0 * l2.2) / det,
    ))
}

/// Nelder-Mead over five parameters. Returns the best point and its cost.
fn nelder_mead(
    cost: impl Fn(&[f64; 5]) -> f64,
    start: [f64; 5],
    scale: [f64; 5],
) -> ([f64; 5], f64) {
    const N: usize = 5;
    let mut simplex: Vec<([f64; 5], f64)> = Vec::with_capacity(N + 1);
    simplex.push((start, cost(&start)));
    for i in 0..N {
        let mut p = start;
        p[i] += scale[i];
        simplex.push((p, cost(&p)));
    }
    for _ in 0..4000 {
        simplex.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let spread = simplex[N].1 - simplex[0].1;
        if spread.abs() < 1e-10 && simplex[0].1 < 1e6 {
            break;
        }
        let mut centroid = [0.0; 5];
        for (p, _) in &simplex[..N] {
            for k in 0..N {
                centroid[k] += p[k] / N as f64;
            }
        }
        let worst = simplex[N];
        let along = |t: f64| {
            let mut p = [0.0; 5];
            for k in 0..N {
                p[k] = centroid[k] + t * (worst.0[k] - centroid[k]);
            }
            p
        };
        let reflected = along(-1.0);
        let fr = cost(&reflected);
        if fr < simplex[0].1 {
            let expanded = along(-2.0);
            let fe = cost(&expanded);
            simplex[N] = if fe < fr {
                (expanded, fe)
            } else {
                (reflected, fr)
            };
        } else if fr < simplex[N - 1].1 {
            simplex[N] = (reflected, fr);
        } else {
            let contracted = along(0.5);
            let fc = cost(&contracted);
            if fc < worst.1 {
                simplex[N] = (contracted, fc);
            } else {
                let best = simplex[0].0;
                for entry in simplex.iter_mut().skip(1) {
                    for (value, &b) in entry.0.iter_mut().zip(best.iter()) {
                        *value = b + 0.5 * (*value - b);
                    }
                    entry.1 = cost(&entry.0);
                }
            }
        }
    }
    simplex.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    simplex[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A side-view pose that keeps the plate inside the frame.
    fn rig() -> MountConfig {
        MountConfig {
            distance_from_plate_m: 1.74,
            height_m: 0.9,
            lateral_offset_m: -1.61,
            pitch_deg: 15.0,
            yaw_deg: 23.0,
            positive_depth_is_rf: true,
        }
    }

    /// Paint a convex polygon by testing each pixel against every edge.
    fn fill_polygon(img: &mut [u8], w: usize, poly: &[(f64, f64)], value: u8) {
        let h = img.len() / w;
        for y in 0..h {
            for x in 0..w {
                let p = (x as f64 + 0.5, y as f64 + 0.5);
                let mut sign = 0.0f64;
                let mut inside = true;
                for i in 0..poly.len() {
                    let c = cross(poly[i], poly[(i + 1) % poly.len()], p);
                    if sign == 0.0 {
                        sign = c.signum();
                    } else if c.signum() != sign && c != 0.0 {
                        inside = false;
                        break;
                    }
                }
                if inside {
                    img[y * w + x] = value;
                }
            }
        }
    }

    fn render(mount: &MountConfig, intr: &Intrinsics) -> (Vec<u8>, [(f64, f64); 5]) {
        let (w, h) = (intr.width as usize, intr.height as usize);
        let mut img = vec![40u8; w * h];
        let mut corners = [(0.0, 0.0); 5];
        for (i, p) in PLATE_FIELD.iter().enumerate() {
            let (u, v, _) = project(*p, intr, mount).unwrap();
            corners[i] = (u, v);
        }
        fill_polygon(&mut img, w, &corners, 220);
        (img, corners)
    }

    #[test]
    fn detects_and_recovers_the_rig() {
        let intr = Intrinsics::from_fov(640, 400, 80.0, 55.0);
        let truth = rig();
        let (img, corners) = render(&truth, &intr);
        let found = find_plate(&img, 640, 400).expect("plate");
        for c in corners {
            assert!(
                found
                    .px
                    .iter()
                    .any(|f| (f.0 - c.0).abs() < 2.5 && (f.1 - c.1).abs() < 2.5),
                "corner {c:?} missing from {:?}",
                found.px
            );
        }
        let guess = MountConfig {
            distance_from_plate_m: 2.1,
            height_m: 0.8,
            lateral_offset_m: -1.4,
            pitch_deg: 22.0,
            yaw_deg: 15.0,
            positive_depth_is_rf: true,
        };
        let fit = solve_pose(&found, &intr, &guess).expect("pose");
        assert!(fit.rms_px < 1.0, "rms {}", fit.rms_px);
        let m = &fit.mount;
        let msg = format!("fit {m:?} vs {truth:?} rms {}", fit.rms_px);
        assert!(
            (m.distance_from_plate_m - truth.distance_from_plate_m).abs() < 0.02,
            "{msg}"
        );
        assert!((m.height_m - truth.height_m).abs() < 0.02, "{msg}");
        assert!(
            (m.lateral_offset_m - truth.lateral_offset_m).abs() < 0.02,
            "{msg}"
        );
        assert!((m.yaw_deg - truth.yaw_deg).abs() < 0.5, "{msg}");
        assert!((m.pitch_deg - truth.pitch_deg).abs() < 0.5, "{msg}");
    }

    #[test]
    fn ball_on_the_plate_is_rejected() {
        let intr = Intrinsics::from_fov(640, 400, 80.0, 55.0);
        let (mut img, corners) = render(&rig(), &intr);
        // A bright ball sitting on the front edge spoils the pentagon.
        let (cx, cy) = ((corners[2].0 + corners[3].0) * 0.5, corners[2].1);
        for y in 0..400usize {
            for x in 0..640usize {
                if (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2) <= 14.0 * 14.0 {
                    img[y * 640 + x] = 230;
                }
            }
        }
        let cfg = SessionConfig::example();
        assert!(calibrate_from_frame(&img, 640, 400, &cfg).is_none());
    }
}
