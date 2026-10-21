//! Where a batted ball goes after the camera loses it: RK4 under gravity
//! and drag. Spin is not measured, so none is assumed.

use crate::clips::{HitFlight, Landing};
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

/// Flight from position `p0` with velocity `v0`, both metric, reported in
/// feet and seconds.
pub fn predict(p0: [f64; 3], v0: [f64; 3], air_density: f64) -> HitFlight {
    let k =
        0.5 * air_density * std::f64::consts::PI * BALL_RADIUS_M * BALL_RADIUS_M * DRAG_COEFFICIENT
            / BALL_MASS_KG;
    let accel = |v: [f64; 3]| -> [f64; 3] {
        let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [-k * speed * v[0], -k * speed * v[1] - G, -k * speed * v[2]]
    };
    let step = |p: [f64; 3], v: [f64; 3]| -> ([f64; 3], [f64; 3]) {
        let h = STEP_S;
        let k1v = accel(v);
        let k2v = accel(axpy(v, k1v, 0.5 * h));
        let k3v = accel(axpy(v, k2v, 0.5 * h));
        let k4v = accel(axpy(v, k3v, h));
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
    };

    let mut path = vec![point(0.0, p0)];
    let mut next_path_t = PATH_STEP_S;
    let (mut t, mut p, mut v) = (0.0, p0, v0);
    let mut peak = p0[1];
    let (landing, hang_time) = loop {
        let (np, nv) = step(p, v);
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
    use crate::geom::launch_velocity;
    use crate::units::metres;

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
