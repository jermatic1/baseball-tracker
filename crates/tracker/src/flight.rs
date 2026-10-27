//! Flight under gravity and drag, integrated with RK4: where a batted ball
//! goes after the camera loses it, and where a pitch was and will be along
//! its path. Spin is not measured, so none is assumed.

use crate::clips::{HitFlight, Landing};
use crate::geom::{Sample, Trajectory};
use crate::units::feet;

const BALL_RADIUS_M: f64 = 0.0368;
const BALL_MASS_KG: f64 = 0.145;
const G: f64 = 9.80665;
/// Tuned against published carry tables.
const DRAG_COEFFICIENT: f64 = 0.33;
const STEP_S: f64 = 1.0 / 240.0;
const PATH_STEP_S: f64 = 0.1;
const MAX_FLIGHT_S: f64 = 20.0;
const SEA_LEVEL_AIR_DENSITY: f64 = 1.225;

/// Standard-atmosphere air density, kg/m^3, at an altitude in metres.
pub fn air_density(altitude_m: f64) -> f64 {
    SEA_LEVEL_AIR_DENSITY * (1.0 - 2.25577e-5 * altitude_m).max(0.0).powf(4.2559)
}

/// Drag deceleration per unit speed squared, 1/m, for the air density.
fn drag_constant(air_density: f64) -> f64 {
    0.5 * air_density * std::f64::consts::PI * BALL_RADIUS_M * BALL_RADIUS_M * DRAG_COEFFICIENT
        / BALL_MASS_KG
}

fn accel(v: [f64; 3], k: f64) -> [f64; 3] {
    let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [-k * speed * v[0], -k * speed * v[1] - G, -k * speed * v[2]]
}

/// One RK4 step of `h` seconds; negative `h` runs the flight backward.
fn step(p: [f64; 3], v: [f64; 3], h: f64, k: f64) -> ([f64; 3], [f64; 3]) {
    let k1v = accel(v, k);
    let k2v = accel(axpy(v, k1v, 0.5 * h), k);
    let k3v = accel(axpy(v, k2v, 0.5 * h), k);
    let k4v = accel(axpy(v, k3v, h), k);
    let k1p = v;
    let k2p = axpy(v, k1v, 0.5 * h);
    let k3p = axpy(v, k2v, 0.5 * h);
    let k4p = axpy(v, k3v, h);
    let mut np = p;
    let mut nv = v;
    for i in 0..3 {
        np[i] += h / 6.0 * (k1p[i] + 2.0 * k2p[i] + 2.0 * k3p[i] + k4p[i]);
        nv[i] += h / 6.0 * (k1v[i] + 2.0 * k2v[i] + 2.0 * k3v[i] + k4v[i]);
    }
    (np, nv)
}

/// Position and velocity where the flight from `p0`, `v0` crosses depth
/// `z`, looking backward in time when `z` is behind the ball. None when the
/// ball is not moving toward it or never gets there.
pub fn at_depth(
    p0: [f64; 3],
    v0: [f64; 3],
    z: f64,
    air_density: f64,
) -> Option<([f64; 3], [f64; 3])> {
    let k = drag_constant(air_density);
    let toward = z - p0[2];
    if toward == 0.0 {
        return Some((p0, v0));
    }
    if v0[2] == 0.0 || !toward.is_finite() {
        return None;
    }
    let h = if toward.signum() == v0[2].signum() {
        STEP_S
    } else {
        -STEP_S
    };
    let (mut p, mut v) = (p0, v0);
    let mut elapsed = 0.0;
    while elapsed < MAX_FLIGHT_S {
        let (np, nv) = step(p, v, h, k);
        if (np[2] - z).signum() != (p[2] - z).signum() || np[2] == z {
            let f = (z - p[2]) / (np[2] - p[2]);
            return Some((lerp(p, np, f), lerp(v, nv, f)));
        }
        (p, v) = (np, nv);
        elapsed += STEP_S;
    }
    None
}

/// Rescale a drag-free fit's speed so that a flight with drag from its start
/// best matches the samples. The drag-free fit lands on the average speed
/// over the window; the rescaled one is the speed at the first sample.
pub fn refine_speed(traj: &Trajectory, samples: &[Sample], air_density: f64) -> Trajectory {
    let k = drag_constant(air_density);
    let mut times: Vec<(f64, [f64; 3])> = samples
        .iter()
        .filter(|s| s.t_ns >= traj.t0_ns)
        .map(|s| ((s.t_ns - traj.t0_ns) as f64 / 1e9, [s.x, s.y, s.z]))
        .collect();
    times.sort_by(|a, b| a.0.total_cmp(&b.0));
    if times.len() < 2 {
        return *traj;
    }
    let cost = |scale: f64| -> f64 {
        let (mut t, mut p, mut v) = (0.0, traj.p0, scale_v(traj.v, scale));
        let mut sum = 0.0;
        for (ts, s) in &times {
            while t + STEP_S <= *ts {
                (p, v) = step(p, v, STEP_S, k);
                t += STEP_S;
            }
            let (np, _) = step(p, v, ts - t, k);
            sum += (np[0] - s[0]).powi(2) + (np[1] - s[1]).powi(2) + (np[2] - s[2]).powi(2);
        }
        sum
    };
    // Golden-section search over the scale; drag only slows the ball, so
    // the true start speed is at least the window average.
    let (mut lo, mut hi) = (0.98, 1.3);
    let phi = (5f64.sqrt() - 1.0) / 2.0;
    let (mut a, mut b) = (hi - phi * (hi - lo), lo + phi * (hi - lo));
    let (mut fa, mut fb) = (cost(a), cost(b));
    for _ in 0..24 {
        if fa < fb {
            hi = b;
            b = a;
            fb = fa;
            a = hi - phi * (hi - lo);
            fa = cost(a);
        } else {
            lo = a;
            a = b;
            fa = fb;
            b = lo + phi * (hi - lo);
            fb = cost(b);
        }
    }
    Trajectory {
        v: scale_v(traj.v, (lo + hi) / 2.0),
        ..*traj
    }
}

fn scale_v(v: [f64; 3], s: f64) -> [f64; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}

/// Flight from position `p0` with velocity `v0`, both metric, reported in
/// feet and seconds.
pub fn predict(p0: [f64; 3], v0: [f64; 3], air_density: f64) -> HitFlight {
    let k = drag_constant(air_density);
    let mut path = vec![point(0.0, p0)];
    let mut next_path_t = PATH_STEP_S;
    let (mut t, mut p, mut v) = (0.0, p0, v0);
    let mut peak = p0[1];
    let (landing, hang_time) = loop {
        let (np, nv) = step(p, v, STEP_S, k);
        let nt = t + STEP_S;
        peak = peak.max(np[1]);
        if np[1] <= 0.0 && nv[1] < 0.0 {
            let f = p[1] / (p[1] - np[1]);
            break (lerp(p, np, f), t + f * STEP_S);
        }
        if nt > MAX_FLIGHT_S {
            break (np, nt);
        }
        (t, p, v) = (nt, np, nv);
        if t + 1e-9 >= next_path_t {
            path.push(point(t, p));
            next_path_t += PATH_STEP_S;
        }
    };
    path.push(point(hang_time, [landing[0], 0.0, landing[2]]));
    HitFlight {
        carry: feet((landing[0] * landing[0] + landing[2] * landing[2]).sqrt()),
        hang_time,
        peak_height: feet(peak),
        landing: Landing {
            x: feet(landing[0]),
            z: feet(landing[2]),
        },
        path,
    }
}

fn axpy(a: [f64; 3], b: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] + s * b[0], a[1] + s * b[1], a[2] + s * b[2]]
}

fn lerp(a: [f64; 3], b: [f64; 3], f: f64) -> [f64; 3] {
    axpy(a, [b[0] - a[0], b[1] - a[1], b[2] - a[2]], f)
}

fn point(t: f64, p: [f64; 3]) -> [f64; 4] {
    [t, feet(p[0]), feet(p[1]), feet(p[2])]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{fit_trajectory, launch_velocity};
    use crate::units::{metres, mph};

    /// Samples of a flight with drag, as the camera would see them.
    fn dragged_samples(p0: [f64; 3], v0: [f64; 3], n: usize, fps: f64) -> Vec<Sample> {
        let k = drag_constant(air_density(0.0));
        let (mut p, mut v) = (p0, v0);
        let mut out = Vec::new();
        for i in 0..n {
            out.push(Sample {
                t_ns: (i as f64 / fps * 1e9) as u64,
                x: p[0],
                y: p[1],
                z: p[2],
            });
            let mut t = 0.0;
            while t + STEP_S <= 1.0 / fps + 1e-9 {
                (p, v) = step(p, v, STEP_S, k);
                t += STEP_S;
            }
        }
        out
    }

    #[test]
    fn refining_recovers_the_start_speed_the_drag_free_fit_underestimates() {
        let v0 = launch_velocity(50.0, -2.0, 180.0);
        let samples = dragged_samples([0.0, 1.8, -14.0], v0, 20, 60.0);
        let plain = fit_trajectory(&samples).unwrap();
        let plain_mph = mph((plain.v[0].powi(2) + plain.v[1].powi(2) + plain.v[2].powi(2)).sqrt());
        assert!(plain_mph < 49.5, "drag-free fit {plain_mph} mph");
        let refined = refine_speed(&plain, &samples, air_density(0.0));
        let refined_mph =
            mph((refined.v[0].powi(2) + refined.v[1].powi(2) + refined.v[2].powi(2)).sqrt());
        assert!(
            (refined_mph - 50.0).abs() < 0.3,
            "refined {refined_mph} mph"
        );
    }

    #[test]
    fn at_depth_runs_forward_and_backward_with_drag() {
        let v0 = launch_velocity(50.0, 0.0, 180.0);
        let air = air_density(0.0);
        let (p, v) = at_depth([0.0, 1.8, -10.0], v0, 0.0, air).unwrap();
        assert!((p[2]).abs() < 1e-6);
        let ahead = mph((v[0].powi(2) + v[1].powi(2) + v[2].powi(2)).sqrt());
        assert!(
            (48.0..49.5).contains(&ahead),
            "slowed to {ahead} mph over 10 m"
        );
        let (pb, vb) = at_depth(p, v, -10.0, air).unwrap();
        assert!((pb[2] + 10.0).abs() < 1e-6 && (pb[1] - 1.8).abs() < 1e-3);
        let back = mph((vb[0].powi(2) + vb[1].powi(2) + vb[2].powi(2)).sqrt());
        assert!((back - 50.0).abs() < 0.05, "back to {back} mph");
        assert!(at_depth([0.0, 1.8, -10.0], v0, -20.0, air).is_some());
        assert!(at_depth([0.0, 1.8, -10.0], [0.0, 0.0, 0.0], 0.0, air).is_none());
    }

    fn fly(mph: f64, launch: f64, spray: f64) -> HitFlight {
        predict(
            [0.0, metres(3.0), 0.0],
            launch_velocity(mph, launch, spray),
            air_density(0.0),
        )
    }

    #[test]
    fn a_hard_fly_ball_carries_a_realistic_distance() {
        let carry = fly(100.0, 30.0, 0.0).carry;
        assert!((300.0..450.0).contains(&carry), "carry {carry} ft");
    }

    #[test]
    fn drag_keeps_the_ball_well_short_of_the_vacuum_parabola() {
        let v = 100.0 / MPH_PER_MPS;
        let vacuum = feet(v * v * 60f64.to_radians().sin() / G);
        assert!(fly(100.0, 30.0, 0.0).carry < vacuum * 0.8);
    }

    #[test]
    fn harder_contact_carries_farther() {
        assert!(fly(90.0, 25.0, 0.0).carry < fly(100.0, 25.0, 0.0).carry);
    }

    #[test]
    fn spray_angle_sends_the_ball_to_the_named_field() {
        let right = fly(90.0, 20.0, 30.0).landing;
        let left = fly(90.0, 20.0, -30.0).landing;
        assert!(right.x > 0.0 && left.x < 0.0);
        assert!(right.z < 0.0 && left.z < 0.0);
    }

    #[test]
    fn thin_air_carries_farther() {
        let denver = predict(
            [0.0, 1.0, 0.0],
            launch_velocity(100.0, 28.0, 0.0),
            air_density(metres(5280.0)),
        );
        assert!(denver.carry > fly(100.0, 28.0, 0.0).carry + 10.0);
    }

    #[test]
    fn path_runs_from_contact_to_landing() {
        let f = fly(80.0, 15.0, 0.0);
        assert_eq!(f.path[0], [0.0, 0.0, 3.0, 0.0]);
        let last = f.path.last().unwrap();
        assert_eq!(last[0], f.hang_time);
        assert_eq!((last[1], last[3]), (f.landing.x, f.landing.z));
        assert!(f.path.len() > 10 && f.peak_height > 3.0);
    }

    use crate::units::MPH_PER_MPS;
}
