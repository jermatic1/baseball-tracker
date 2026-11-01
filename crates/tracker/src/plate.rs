//! Home plate detection and the camera pose solved from its corners.
//!
//! A regulation plate is a bright, flat pentagon of known size, so one clean
//! frame of it fixes where the camera is. The pose is fitted directly in the
//! `MountConfig` parameters by minimising reprojection error through
//! `geom::project`, which keeps it consistent with everything downstream.

use std::collections::VecDeque;

use crate::config::{Config, MountConfig};
use crate::geom::{project, Intrinsics};
use crate::units::{feet, metres};

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

/// The largest bright pentagon in the frame.
pub fn find_plate(gray: &[u8], w: u32, h: u32) -> Option<PlateCorners> {
    find_plates(gray, w, h).into_iter().next()
}

/// Every bright pentagon in the frame, largest first. Bright door panels
/// and mats make pentagons too, so the pose fit decides which is the plate.
/// A shaded plate can sit below the frame's brightest tenth, so blobs are
/// also cut at lower thresholds; near-duplicates are dropped.
pub fn find_plates(gray: &[u8], w: u32, h: u32) -> Vec<PlateCorners> {
    let (w, h) = (w as usize, h as usize);
    if gray.len() < w * h || w < 16 || h < 16 {
        return Vec::new();
    }
    let top = bright_threshold(gray);
    let mut found: Vec<(usize, PlateCorners)> = Vec::new();
    for threshold in [top, top - top / 4, top / 2] {
        for (area, c) in pentagons_at(gray, w, h, threshold) {
            if !found.iter().any(|(_, f)| same_outline(f, &c)) {
                found.push((area, c));
            }
        }
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, c)| c).collect()
}

/// Pentagons among the blobs at or above `threshold`, with their areas.
fn pentagons_at(gray: &[u8], w: usize, h: usize, threshold: u8) -> Vec<(usize, PlateCorners)> {
    let mask: Vec<bool> = gray[..w * h].iter().map(|&v| v >= threshold).collect();
    let mut seen = vec![false; w * h];
    let mut found = Vec::new();
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
        found.push((pixels.len(), PlateCorners { px }));
    }
    found
}

/// Whether every corner of `a` has a corner of `b` within a few pixels.
fn same_outline(a: &PlateCorners, b: &PlateCorners) -> bool {
    a.px.iter().all(|&p| b.px.iter().any(|&q| dist(p, q) < 3.0))
}

/// Fit the mount parameters to the detected corners, starting from a guess
/// (the current config) and from its mirror image on the other side of the
/// plate, since a wrong-side guess settles in a poor local minimum. Every
/// assignment of outline corners to plate corners is tried (five rotations,
/// both directions); the best fit wins.
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
    let starts = [1.0, -1.0].map(|side| {
        [
            metres(initial.distance),
            metres(initial.height),
            side * metres(initial.lateral),
            side * initial.yaw,
            initial.pitch,
        ]
    });
    for (px, start) in assignments
        .iter()
        .flat_map(|px| starts.iter().map(move |s| (px, *s)))
    {
        let cost = |p: &[f64; 5]| -> f64 {
            let mount = mount_from(p);
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
        let (p, sum) = nelder_mead(cost, start, [0.3, 0.2, 0.3, 10.0, 10.0]);
        let rms = (sum / 5.0).sqrt();
        if best.as_ref().is_none_or(|b| rms < b.rms_px) {
            let mount = mount_from(&p);
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
    best
}

/// Detect the plate in a frame and fit the mount, starting from the config.
/// Of every pentagon in the frame, the one that fits the pose best is the
/// plate, if it fits well enough with the camera above the ground.
pub fn calibrate_from_frame(
    gray: &[u8],
    w: u32,
    h: u32,
    cfg: &Config,
) -> Option<(PlateCorners, PoseFit)> {
    let intr = Intrinsics::from_fov(w, h, 80.0, 55.0);
    find_plates(gray, w, h)
        .into_iter()
        .filter_map(|corners| solve_pose(&corners, &intr, &cfg.mount).map(|fit| (corners, fit)))
        .filter(|(_, fit)| fit.rms_px <= MAX_RMS_PX && fit.mount.height > 0.0)
        .min_by(|a, b| a.1.rms_px.total_cmp(&b.1.rms_px))
}

/// Shortest plate edge on screen, in pixels, that clicks can place well.
const MIN_EDGE_PX: f64 = 15.0;
/// Median stereo-versus-fit depth disagreement tolerated, as a fraction.
const MAX_DEPTH_DISAGREEMENT: f64 = 0.15;

/// Reasons a fit looks wrong, for the user to judge before applying it.
/// `depth` gives the stereo depth along the optical axis, in metres, at a
/// pixel; without it the depth check is skipped.
pub fn check_fit(
    fit: &PoseFit,
    corners: &PlateCorners,
    intr: &Intrinsics,
    depth: Option<&dyn Fn((f64, f64)) -> Option<f64>>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if fit.rms_px > MAX_RMS_PX {
        warnings.push(format!(
            "corners do not fit a plate (rms {:.1} px)",
            fit.rms_px
        ));
    }
    let m = &fit.mount;
    let mut pose = Vec::new();
    if m.height < 0.0 {
        pose.push("camera below ground".to_string());
    } else if !(1.0..=12.0).contains(&m.height) {
        pose.push(format!("height {:.1} ft", m.height));
    }
    if !(2.0..=40.0).contains(&m.distance) {
        pose.push(format!("distance {:.1} ft", m.distance));
    }
    if !pose.is_empty() {
        warnings.push(format!("camera pose is implausible: {}", pose.join(", ")));
    }
    let longest = (0..5)
        .map(|i| dist(corners.px[i], corners.px[(i + 1) % 5]))
        .fold(0.0, f64::max);
    if longest < MIN_EDGE_PX {
        warnings.push("plate is too small in the frame".into());
    }
    if let Some(depth) = depth {
        let mut errors: Vec<f64> = corners
            .px
            .iter()
            .filter_map(|&px| {
                // The clicked corner's plate corner is the nearest reprojected one.
                let i = (0..5).min_by(|&a, &b| {
                    dist(fit.reprojected[a], px).total_cmp(&dist(fit.reprojected[b], px))
                })?;
                let (_, _, expected) = project(PLATE_FIELD[i], intr, m)?;
                let measured = depth(px)?;
                (expected > 0.0).then(|| (measured - expected).abs() / expected)
            })
            .collect();
        if !errors.is_empty() {
            errors.sort_by(f64::total_cmp);
            let median = errors[errors.len() / 2];
            if median > MAX_DEPTH_DISAGREEMENT {
                warnings.push(format!(
                    "stereo depth disagrees with the fit by {:.0}%",
                    median * 100.0
                ));
            }
        }
    }
    warnings
}

/// Median of the non-zero millimetre depths in a 5x5 window around a pixel,
/// in metres.
pub fn window_depth_m(depth_mm: &[u16], w: u32, h: u32, (x, y): (f64, f64)) -> Option<f64> {
    if depth_mm.len() < (w * h) as usize || x < 0.0 || y < 0.0 {
        return None;
    }
    let (cx, cy) = (x as i64, y as i64);
    let mut vals: Vec<u16> = (cy - 2..=cy + 2)
        .flat_map(|yy| (cx - 2..=cx + 2).map(move |xx| (xx, yy)))
        .filter(|&(xx, yy)| xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64)
        .map(|(xx, yy)| depth_mm[(yy * w as i64 + xx) as usize])
        .filter(|&d| d > 0)
        .collect();
    if vals.is_empty() {
        return None;
    }
    vals.sort_unstable();
    Some(vals[vals.len() / 2] as f64 / 1000.0)
}

/// Metres and degrees from the optimiser into the feet of the config.
fn mount_from(p: &[f64; 5]) -> MountConfig {
    let distance = p[0].max(0.3);
    MountConfig {
        distance: feet(distance),
        height: feet(p[1]),
        lateral: feet(p[2].clamp(-distance, distance)),
        pitch: p[4],
        yaw: p[3],
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
            distance: feet(1.74),
            height: feet(0.9),
            lateral: feet(-1.61),
            pitch: 15.0,
            yaw: 23.0,
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
            distance: feet(2.1),
            height: feet(0.8),
            lateral: feet(-1.4),
            pitch: 22.0,
            yaw: 15.0,
        };
        let fit = solve_pose(&found, &intr, &guess).expect("pose");
        assert!(fit.rms_px < 1.0, "rms {}", fit.rms_px);
        let m = &fit.mount;
        let msg = format!("fit {m:?} vs {truth:?} rms {}", fit.rms_px);
        assert!((m.distance - truth.distance).abs() < 0.07, "{msg}");
        assert!((m.height - truth.height).abs() < 0.07, "{msg}");
        assert!((m.lateral - truth.lateral).abs() < 0.07, "{msg}");
        assert!((m.yaw - truth.yaw).abs() < 0.5, "{msg}");
        assert!((m.pitch - truth.pitch).abs() < 0.5, "{msg}");
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
        let cfg = Config::example();
        assert!(calibrate_from_frame(&img, 640, 400, &cfg).is_none());
    }

    /// A plausible side mount, aimed at the plate.
    fn field_rig() -> MountConfig {
        MountConfig {
            distance: 8.0,
            height: 4.0,
            lateral: -5.0,
            pitch: 0.0,
            yaw: 0.0,
        }
    }

    fn fit_rendered(mount: &MountConfig) -> (Intrinsics, PlateCorners, PoseFit) {
        let intr = Intrinsics::from_fov(640, 400, 80.0, 55.0);
        let (_, px) = render(mount, &intr);
        let corners = PlateCorners { px };
        let fit = solve_pose(&corners, &intr, mount).expect("pose");
        (intr, corners, fit)
    }

    #[test]
    fn good_fit_has_no_warnings() {
        let truth = field_rig();
        let (intr, corners, fit) = fit_rendered(&truth);
        let depth = |px: (f64, f64)| {
            let i = corners.px.iter().position(|&c| c == px)?;
            project(PLATE_FIELD[i], &intr, &truth).map(|p| p.2)
        };
        let warnings = check_fit(&fit, &corners, &intr, Some(&depth));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(check_fit(&fit, &corners, &intr, None).is_empty());
    }

    #[test]
    fn stereo_depth_disagreement_is_reported() {
        let truth = field_rig();
        let (intr, corners, fit) = fit_rendered(&truth);
        let depth = |px: (f64, f64)| {
            let i = corners.px.iter().position(|&c| c == px)?;
            project(PLATE_FIELD[i], &intr, &truth).map(|p| p.2 * 1.3)
        };
        let warnings = check_fit(&fit, &corners, &intr, Some(&depth));
        assert_eq!(warnings, ["stereo depth disagrees with the fit by 30%"]);
        let none = |_: (f64, f64)| None;
        assert!(check_fit(&fit, &corners, &intr, Some(&none)).is_empty());
    }

    #[test]
    fn misplaced_corner_does_not_fit() {
        let truth = field_rig();
        let intr = Intrinsics::from_fov(640, 400, 80.0, 55.0);
        let (_, mut px) = render(&truth, &intr);
        px[2].0 += 25.0;
        let corners = PlateCorners { px };
        let fit = solve_pose(&corners, &intr, &truth).expect("pose");
        let warnings = check_fit(&fit, &corners, &intr, None);
        assert!(
            warnings
                .iter()
                .any(|w| w.starts_with("corners do not fit a plate")),
            "{warnings:?}"
        );
    }

    #[test]
    fn implausible_pose_is_reported() {
        let (intr, corners, mut fit) = fit_rendered(&field_rig());
        fit.mount.height = 0.5;
        fit.mount.distance = 50.0;
        let warnings = check_fit(&fit, &corners, &intr, None);
        assert_eq!(
            warnings,
            ["camera pose is implausible: height 0.5 ft, distance 50.0 ft"]
        );
        fit.mount.height = -1.0;
        fit.mount.distance = 8.0;
        let warnings = check_fit(&fit, &corners, &intr, None);
        assert_eq!(
            warnings,
            ["camera pose is implausible: camera below ground"]
        );
    }

    #[test]
    fn tiny_plate_is_flagged() {
        let (intr, mut corners, fit) = fit_rendered(&field_rig());
        let (cx, cy) = corners.px[0];
        for p in corners.px.iter_mut() {
            *p = (cx + (p.0 - cx) * 0.1, cy + (p.1 - cy) * 0.1);
        }
        let warnings = check_fit(&fit, &corners, &intr, None);
        assert!(
            warnings.contains(&"plate is too small in the frame".to_string()),
            "{warnings:?}"
        );
    }

    #[test]
    fn window_depth_ignores_holes() {
        let (w, h) = (10u32, 10u32);
        let mut depth = vec![0u16; 100];
        depth[5 * 10 + 5] = 2000;
        depth[4 * 10 + 4] = 2100;
        depth[6 * 10 + 6] = 1900;
        assert_eq!(window_depth_m(&depth, w, h, (5.2, 5.7)), Some(2.0));
        assert_eq!(window_depth_m(&depth, w, h, (0.0, 0.0)), None);
    }
}
