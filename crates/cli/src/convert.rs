pub fn to_stored(frame: device::Frame) -> tracker::StoredFrame {
    tracker::StoredFrame {
        width: frame.width,
        height: frame.height,
        left: frame.left,
        right: frame.right,
        depth_mm: frame.depth_mm,
        t_ns: frame.t_ns,
        sequence: frame.sequence,
    }
}

pub fn config_summary(cfg: &tracker::SessionConfig) -> serde_json::Value {
    serde_json::to_value(cfg).unwrap_or_default()
}

pub fn print_estimate(est: &tracker::HitEstimate) {
    println!(
        "ev={} launch={} spray={} samples={} confident={}",
        est.exit_velocity, est.launch_angle, est.spray_angle, est.samples, est.confident
    );
}

pub fn print_hit(hit: &tracker::HitRecord) {
    let hit_type = hit.hit_type.map(|t| t.as_str()).unwrap_or("-");
    let pitch = hit
        .pitch_velocity
        .map(|p| format!("{p:.1}"))
        .unwrap_or_else(|| "-".into());
    let contact = hit
        .contact
        .map(|c| format!("{:?}", c.kind).to_lowercase())
        .unwrap_or_else(|| "-".into());
    println!(
        "{} {} {} #{} frames {}..{} ev={:.1} pitch={} launch={:.1} spray={:.1} contact={} samples={} confident={}",
        hit.clip,
        hit.kind.as_str(),
        hit_type,
        hit.segment,
        hit.frame_start,
        hit.frame_end,
        hit.exit_velocity,
        pitch,
        hit.launch_angle,
        hit.spray_angle,
        contact,
        hit.samples,
        hit.confident
    );
}
