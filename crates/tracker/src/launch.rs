use crate::geom::HitEstimate;

pub fn postable(hit: &HitEstimate) -> Result<(), String> {
    if !hit.confident {
        return Err("not confident".into());
    }
    if ![
        hit.exit_velocity_mph,
        hit.launch_angle_deg,
        hit.spray_angle_deg,
    ]
    .iter()
    .all(|v| v.is_finite())
    {
        return Err("non-finite".into());
    }
    if !(1.0..=150.0).contains(&hit.exit_velocity_mph) {
        return Err("exit_velocity_mph out of range".into());
    }
    if !(-10.0..=90.0).contains(&hit.launch_angle_deg) {
        return Err("launch_angle_deg out of range".into());
    }
    if !(-45.0..=45.0).contains(&hit.spray_angle_deg) {
        return Err("spray_angle_deg out of range".into());
    }
    Ok(())
}
