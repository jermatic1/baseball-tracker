use crate::config::MountConfig;
use crate::units::metres;

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
    pub exit_velocity: f64,
    pub launch_angle: f64,
    pub spray_angle: f64,
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

/// Camera basis in field space: x right, y down, z forward (right-handed).
/// Yaw rotates about +Y, so positive yaw turns the camera to its left
/// (counter-clockwise seen from above); pitch tilts it up.
fn camera_pose(mount: &MountConfig) -> Pose {
    // field: +X RF, +Y up, -Z CF
    let lat = metres(mount.lateral);
    let dist = metres(mount.distance);
    let z = -(dist * dist - lat * lat).max(0.0).sqrt();
    let pos = [lat, metres(mount.height), z];

    let mut look = scale(pos, -1.0);
    if norm(look) < 1e-12 {
        look = [0.0, 0.0, 1.0];
    } else {
        look = normalize(look);
    }

    look = rotate_y(look, mount.yaw.to_radians());

    let world_up = [0.0, 1.0, 0.0];
    let mut right = cross(look, world_up);
    if norm(right) < 1e-9 {
        right = cross(look, [0.0, 0.0, -1.0]);
    }
    right = normalize(right);

    look = normalize(rodrigues(look, right, mount.pitch.to_radians()));
    let down = normalize(cross(look, right));
    Pose {
        pos,
        right,
        down,
        forward: look,
    }
}

pub fn unproject(u: f64, v: f64, depth_m: f64, intr: &Intrinsics, mount: &MountConfig) -> [f64; 3] {
    let pose = camera_pose(mount);
    let z = depth_m;
    let xc = (u - intr.cx) * z / intr.fx;
    let yc = (v - intr.cy) * z / intr.fy;
    add(
        pose.pos,
        add(
            add(scale(pose.right, xc), scale(pose.down, yc)),
            scale(pose.forward, z),
        ),
    )
}

pub fn project(
    p_field: [f64; 3],
    intr: &Intrinsics,
    mount: &MountConfig,
) -> Option<(f64, f64, f64)> {
    let pose = camera_pose(mount);
    let d = sub(p_field, pose.pos);
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

/// Free flight under gravity: position `p0` and velocity `v` at `t0_ns`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trajectory {
    pub t0_ns: u64,
    pub p0: [f64; 3],
    pub v: [f64; 3],
}

impl Trajectory {
    pub fn at(&self, t_ns: u64) -> [f64; 3] {
        self.at_s((t_ns as f64 - self.t0_ns as f64) / 1e9)
    }

    /// Position `t` seconds after `t0_ns`; negative looks back.
    pub fn at_s(&self, t: f64) -> [f64; 3] {
        [
            self.p0[0] + self.v[0] * t,
            self.p0[1] + self.v[1] * t - 0.5 * G * t * t,
            self.p0[2] + self.v[2] * t,
        ]
    }

    pub fn velocity_s(&self, t: f64) -> [f64; 3] {
        [self.v[0], self.v[1] - G * t, self.v[2]]
    }

    pub fn estimate(&self, samples: usize) -> Option<HitEstimate> {
        let [vx, vy, vz] = self.v;
        let speed = (vx * vx + vy * vy + vz * vz).sqrt();
        let exit_velocity = speed / MPS_PER_MPH;
        let launch_angle = vy.atan2((vx * vx + vz * vz).sqrt()).to_degrees();
        let spray_angle = vx.atan2(-vz).to_degrees();
        if ![exit_velocity, launch_angle, spray_angle]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        Some(HitEstimate {
            exit_velocity,
            launch_angle,
            spray_angle,
            samples,
            confident: samples >= 4,
        })
    }
}

/// Least-squares fit of x and z against time and of gravity-compensated y.
pub fn fit_trajectory(samples: &[Sample]) -> Option<Trajectory> {
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
    let (x0, vx) = fit_line(&ts, &xs)?;
    let (z0, vz) = fit_line(&ts, &zs)?;
    let (y0, vy) = fit_line(&ts, &ys)?;
    Some(Trajectory {
        t0_ns: t0,
        p0: [x0, y0, z0],
        v: [vx, vy, vz],
    })
}

pub fn fit_samples(samples: &[Sample]) -> Option<HitEstimate> {
    fit_trajectory(samples)?.estimate(samples.len())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn intr() -> Intrinsics {
        Intrinsics::from_fov(640, 400, 80.0, 55.0)
    }

    /// Level camera 68 in to the first-base side (or the third-base side
    /// when `side` is -1), facing straight across the ball path.
    fn beside(side: f64) -> MountConfig {
        MountConfig {
            distance: 6.94,
            height: 2.83,
            lateral: side * 5.67,
            pitch: 22.0,
            yaw: side * -35.2,
        }
    }

    #[test]
    fn first_base_camera_sees_the_plate_left_and_the_pitcher_right() {
        let (intr, mount) = (intr(), beside(1.0));
        let (apex_u, _, _) = project([0.0, 0.0, 0.0], &intr, &mount).unwrap();
        let (pitcher_u, _, z) = project([0.0, 0.0, -3.0], &intr, &mount).unwrap();
        assert!(apex_u < intr.cx, "apex at u={apex_u}");
        assert!(
            pitcher_u > intr.cx && z > 0.0,
            "pitcher side at u={pitcher_u}"
        );
    }

    #[test]
    fn third_base_camera_sees_the_plate_right() {
        let (intr, mount) = (intr(), beside(-1.0));
        let (apex_u, _, _) = project([0.0, 0.0, 0.0], &intr, &mount).unwrap();
        assert!(apex_u > intr.cx, "apex at u={apex_u}");
    }

    #[test]
    fn point_ahead_projects_to_the_centre() {
        let intr = intr();
        for side in [1.0, -1.0] {
            let mount = beside(side);
            let ahead = unproject(intr.cx, intr.cy, 2.0, &intr, &mount);
            let (u, v, z) = project(ahead, &intr, &mount).unwrap();
            assert!((u - intr.cx).abs() < 1e-9 && (v - intr.cy).abs() < 1e-9);
            assert!((z - 2.0).abs() < 1e-9);
        }
        let mount = MountConfig {
            distance: 6.94,
            height: 0.0,
            lateral: 5.67,
            pitch: 0.0,
            yaw: 0.0,
        };
        let (u, v, _) = project([0.0, 0.0, 0.0], &intr, &mount).unwrap();
        assert!((u - intr.cx).abs() < 1e-9 && (v - intr.cy).abs() < 1e-9);
    }

    #[test]
    fn unproject_inverts_project() {
        let intr = intr();
        for side in [1.0, -1.0] {
            let mount = beside(side);
            let p = [0.3, 0.8, -1.2];
            let (u, v, z) = project(p, &intr, &mount).unwrap();
            let q = unproject(u, v, z, &intr, &mount);
            assert!(norm(sub(p, q)) < 1e-9, "{p:?} -> {q:?}");
        }
    }

    #[test]
    fn spray_follows_field_x_from_either_side() {
        let intr = intr();
        for side in [1.0, -1.0] {
            let mount = beside(side);
            // A ball at the image centre moving away from (or toward) a
            // camera that faces across the field: from first base, away is
            // toward third (-X); from third base, away is toward first (+X).
            for away in [1.0, -1.0] {
                let samples: Vec<Sample> = (0..5)
                    .map(|i| {
                        let t = i as f64 * 0.01;
                        let depth = 2.0 + away * 10.0 * t;
                        let [x, y, z] = unproject(intr.cx, intr.cy, depth, &intr, &mount);
                        Sample {
                            t_ns: (t * 1e9) as u64,
                            x,
                            y,
                            z,
                        }
                    })
                    .collect();
                let spray = fit_samples(&samples).unwrap().spray_angle;
                let expected = -side * away * 90.0;
                assert!(
                    (spray - expected).abs() < 1.0,
                    "side {side} away {away}: spray {spray}"
                );
            }
        }
    }

    #[test]
    fn positive_yaw_turns_the_camera_left() {
        let intr = intr();
        let level = MountConfig {
            pitch: 0.0,
            yaw: 0.0,
            ..beside(1.0)
        };
        let turned = MountConfig {
            yaw: 10.0,
            ..level.clone()
        };
        let (u0, _, _) = project([0.0, 0.0, 0.0], &intr, &level).unwrap();
        let (u1, _, _) = project([0.0, 0.0, 0.0], &intr, &turned).unwrap();
        assert!(u1 > u0, "apex moved from {u0} to {u1}");
    }
}
