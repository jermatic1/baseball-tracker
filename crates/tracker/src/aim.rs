//! Where to hang a pitcher's target when they throw from closer than the
//! simulated mound: the throw that would land in the strike zone from the
//! simulated distance, followed to the target plane behind the real plate.

use serde::Serialize;

use crate::config::{PitchingConfig, ZoneConfig};
use crate::flight;
use crate::geom::launch_velocity;
use crate::units::{inches, metres};
use crate::PLATE_FRONT_M;

/// Release height, inches, assumed until a pitcher's own has been measured.
pub const DEFAULT_RELEASE_HEIGHT_IN: f64 = 60.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Aim {
    /// Release speed the target is placed for, mph.
    pub speed: f64,
    /// Release height the throw is assumed to leave from, inches.
    pub release_height: f64,
    /// Feet behind the front of the plate.
    pub target_behind: f64,
    /// Inches above the ground to aim at on the target plane.
    pub height: f64,
    /// The strike zone carried onto the target plane: a box this size,
    /// centred on the plate, is the zone as the pitcher sees it.
    pub zone: ZoneConfig,
}

/// The aim point and zone on the target plane for a release speed and
/// height, or none when the target would hang past the simulated plate
/// or the throw cannot reach the zone.
pub fn aim(
    cfg: &PitchingConfig,
    zone: &ZoneConfig,
    speed: f64,
    release_height: f64,
    air: f64,
) -> Option<Aim> {
    let p0 = [
        0.0,
        metres(release_height / 12.0),
        -metres(cfg.simulated_distance),
    ];
    let target_z = PLATE_FRONT_M - metres(cfg.simulated_distance - cfg.actual_distance)
        + metres(cfg.target_behind);
    if speed <= 0.0 || target_z <= p0[2] || target_z > PLATE_FRONT_M {
        return None;
    }
    // Height at the front of the plate rises with the launch angle, so the
    // angle that hits a plate height is found by bisection, then followed
    // to the target plane.
    let plate_height = |deg: f64| {
        flight::at_depth(p0, launch_velocity(speed, deg, 180.0), PLATE_FRONT_M, air)
            .map(|(p, _)| p[1])
    };
    let on_target = |height_in: f64| -> Option<f64> {
        let want = metres(height_in / 12.0);
        let (mut lo, mut hi) = (-30.0, 45.0);
        if plate_height(lo)? > want || plate_height(hi)? < want {
            return None;
        }
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if plate_height(mid)? < want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let v = launch_velocity(speed, 0.5 * (lo + hi), 180.0);
        flight::at_depth(p0, v, target_z, air).map(|(p, _)| inches(p[1]))
    };
    let across = (target_z - p0[2]) / (PLATE_FRONT_M - p0[2]);
    Some(Aim {
        speed,
        release_height,
        target_behind: cfg.target_behind,
        height: on_target((zone.bottom + zone.top) / 2.0)?,
        zone: ZoneConfig {
            width: zone.width * across,
            bottom: on_target(zone.bottom)?,
            top: on_target(zone.top)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AIR: f64 = 1.225;

    #[test]
    fn target_at_the_plate_from_the_mound_is_the_zone_itself() {
        let cfg = PitchingConfig::default();
        let zone = ZoneConfig::default();
        let a = aim(&cfg, &zone, 50.0, 60.0, AIR).unwrap();
        assert!((a.height - 30.0).abs() < 0.1, "{a:?}");
        assert!((a.zone.width - zone.width).abs() < 1e-9);
        assert!((a.zone.bottom - zone.bottom).abs() < 0.1, "{a:?}");
        assert!((a.zone.top - zone.top).abs() < 0.1, "{a:?}");
    }

    #[test]
    fn closer_throws_aim_higher_through_a_smaller_box() {
        let cfg = PitchingConfig {
            actual_distance: 20.0,
            simulated_distance: 46.0,
            target_behind: 5.0,
        };
        let zone = ZoneConfig::default();
        let fast = aim(&cfg, &zone, 55.0, 60.0, AIR).unwrap();
        let slow = aim(&cfg, &zone, 40.0, 60.0, AIR).unwrap();
        assert!(fast.height > 45.0 && fast.height < 75.0, "{fast:?}");
        assert!(slow.height > fast.height, "{slow:?} vs {fast:?}");
        assert!(fast.zone.width < 10.0 && fast.zone.width > 8.0, "{fast:?}");
        assert!(fast.zone.top - fast.zone.bottom < zone.top - zone.bottom);
        let higher = aim(&cfg, &zone, 55.0, 72.0, AIR).unwrap();
        assert!(higher.height > fast.height);
    }

    #[test]
    fn target_past_the_simulated_plate_has_no_answer() {
        let cfg = PitchingConfig {
            actual_distance: 40.0,
            simulated_distance: 46.0,
            target_behind: 8.0,
        };
        assert!(aim(&cfg, &ZoneConfig::default(), 50.0, 60.0, AIR).is_none());
    }
}
