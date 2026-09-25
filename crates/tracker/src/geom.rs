use crate::config::MountConfig;

const G: f64 = 9.80665;
const MPS_PER_MPH: f64 = 0.44704;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intrinsics {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub width: u32,
    pub height: u32,
}

impl Intrinsics {
    pub fn from_fov(width: u32, height: u32, hfov_deg: f64, vfov_deg: f64) -> Self {
        let fx = (width as f64 * 0.5) / (hfov_deg.to_radians() * 0.5).tan();
        let fy = (height as f64 * 0.5) / (vfov_deg.to_radians() * 0.5).tan();
        Self {
            fx,
            fy,
            cx: width as f64 * 0.5,
            cy: height as f64 * 0.5,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t_ns: u64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitEstimate {
    pub exit_velocity_mph: f64,
    pub launch_angle_deg: f64,
    pub spray_angle_deg: f64,
    pub samples: usize,
    pub confident: bool,
}

#[derive(Clone, Copy)]
struct Pose {
    pos: [f64; 3],
    right: [f64; 3],
    down: [f64; 3],
    forward: [f64; 3],
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let n = norm(a);
    if n < 1e-15 {
        a
    } else {
        scale(a, 1.0 / n)
    }
}

fn rotate_y(v: [f64; 3], rad: f64) -> [f64; 3] {
    let (s, c) = rad.sin_cos();
    [v[0] * c + v[2] * s, v[1], -v[0] * s + v[2] * c]
}

fn rodrigues(v: [f64; 3], axis: [f64; 3], rad: f64) -> [f64; 3] {
    let (s, c) = rad.sin_cos();
    let k = normalize(axis);
    let kxv = cross(k, v);
    let kdv = dot(k, v);
    add(add(scale(v, c), scale(kxv, s)), scale(k, kdv * (1.0 - c)))
}

fn camera_pose(mount: &MountConfig) -> Pose {
    // field: +X RF, +Y up, -Z CF
    let lat = mount.lateral_offset_m;
    let dist = mount.distance_from_plate_m;
    let z = -(dist * dist - lat * lat).max(0.0).sqrt();
    let pos = [lat, mount.height_m, z];

    let mut look = scale(pos, -1.0);
    if norm(look) < 1e-12 {
        look = [0.0, 0.0, 1.0];
    } else {
        look = normalize(look);
    }

    look = rotate_y(look, mount.yaw_deg.to_radians());

    let world_up = [0.0, 1.0, 0.0];
    let mut right = cross(world_up, look);
    if norm(right) < 1e-9 {
        right = cross([0.0, 0.0, 1.0], look);
    }
    right = normalize(right);

    look = normalize(rodrigues(look, right, -mount.pitch_deg.to_radians()));
    let down = normalize(cross(right, look));
    Pose {
        pos,
        right,
        down,
        forward: look,
    }
}

fn needs_x_flip(mount: &MountConfig) -> bool {
    let pose = camera_pose(mount);
    let depth_is_rf = pose.forward[0] > 0.0;
    depth_is_rf != mount.positive_depth_is_rf
}

pub fn unproject(u: f64, v: f64, depth_m: f64, intr: &Intrinsics, mount: &MountConfig) -> [f64; 3] {
    let pose = camera_pose(mount);
    let z = depth_m;
    let xc = (u - intr.cx) * z / intr.fx;
    let yc = (v - intr.cy) * z / intr.fy;
    let mut p = add(
        pose.pos,
        add(
            add(scale(pose.right, xc), scale(pose.down, yc)),
            scale(pose.forward, z),
        ),
    );
    if needs_x_flip(mount) {
        p[0] = -p[0];
    }
    p
}

pub fn project(
    p_field: [f64; 3],
    intr: &Intrinsics,
    mount: &MountConfig,
) -> Option<(f64, f64, f64)> {
    let pose = camera_pose(mount);
    let mut p = p_field;
    if needs_x_flip(mount) {
        p[0] = -p[0];
    }
    let d = sub(p, pose.pos);
    let xc = dot(pose.right, d);
    let yc = dot(pose.down, d);
    let zc = dot(pose.forward, d);
    if zc.abs() < 1e-12 {
        return None;
    }
    let u = intr.fx * xc / zc + intr.cx;
    let v = intr.fy * yc / zc + intr.cy;
    Some((u, v, zc))
}

fn fit_line(ts: &[f64], ys: &[f64]) -> Option<(f64, f64)> {
    let n = ts.len() as f64;
    if ts.len() < 2 {
        return None;
    }
    let sum_t: f64 = ts.iter().sum();
    let sum_t2: f64 = ts.iter().map(|t| t * t).sum();
    let sum_y: f64 = ys.iter().sum();
    let sum_ty: f64 = ts.iter().zip(ys).map(|(t, y)| t * y).sum();
    let det = n * sum_t2 - sum_t * sum_t;
    if !det.is_finite() || det.abs() < 1e-18 {
        return None;
    }
    let a = (sum_y * sum_t2 - sum_t * sum_ty) / det;
    let b = (n * sum_ty - sum_t * sum_y) / det;
    if !a.is_finite() || !b.is_finite() {
        return None;
    }
    Some((a, b))
}

pub fn fit_samples(samples: &[Sample]) -> Option<HitEstimate> {
    if samples.len() < 2 {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_by_key(|s| s.t_ns);
    let t0 = ordered[0].t_ns;
    let ts: Vec<f64> = ordered.iter().map(|s| (s.t_ns - t0) as f64 / 1e9).collect();
    let xs: Vec<f64> = ordered.iter().map(|s| s.x).collect();
    let zs: Vec<f64> = ordered.iter().map(|s| s.z).collect();
    let ys: Vec<f64> = ordered
        .iter()
        .zip(&ts)
        .map(|(s, t)| s.y + 0.5 * G * t * t)
        .collect();
    let (_x0, vx) = fit_line(&ts, &xs)?;
    let (_z0, vz) = fit_line(&ts, &zs)?;
    let (_y0, vy) = fit_line(&ts, &ys)?;
    let speed = (vx * vx + vy * vy + vz * vz).sqrt();
    let exit_velocity_mph = speed / MPS_PER_MPH;
    let launch_angle_deg = vy.atan2((vx * vx + vz * vz).sqrt()).to_degrees();
    let spray_angle_deg = vx.atan2(-vz).to_degrees();
    if ![exit_velocity_mph, launch_angle_deg, spray_angle_deg]
        .iter()
        .all(|v| v.is_finite())
    {
        return None;
    }
    Some(HitEstimate {
        exit_velocity_mph,
        launch_angle_deg,
        spray_angle_deg,
        samples: ordered.len(),
        confident: ordered.len() >= 4,
    })
}

pub fn launch_velocity(mph: f64, launch_deg: f64, spray_deg: f64) -> [f64; 3] {
    let speed = mph * MPS_PER_MPH;
    let launch = launch_deg.to_radians();
    let spray = spray_deg.to_radians();
    let vy = speed * launch.sin();
    let vh = speed * launch.cos();
    let vx = vh * spray.sin();
    let vz = -vh * spray.cos();
    [vx, vy, vz]
}

pub fn field_point(start: [f64; 3], vel: [f64; 3], t: f64) -> [f64; 3] {
    [
        start[0] + vel[0] * t,
        start[1] + vel[1] * t - 0.5 * G * t * t,
        start[2] + vel[2] * t,
    ]
}
