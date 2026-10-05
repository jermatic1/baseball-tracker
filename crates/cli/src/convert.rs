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

pub fn to_estimate(hit: &tracker::HitRecord) -> tracker::HitEstimate {
    tracker::HitEstimate {
        exit_velocity_mph: hit.exit_velocity_mph,
        launch_angle_deg: hit.launch_angle_deg,
        spray_angle_deg: hit.spray_angle_deg,
        samples: hit.samples,
        confident: hit.confident,
    }
}

pub fn config_summary(cfg: &tracker::SessionConfig) -> serde_json::Value {
    serde_json::json!({
        "capture": {
            "width": cfg.capture.width,
            "height": cfg.capture.height,
            "fps": cfg.capture.fps,
            "exposure_us": cfg.capture.exposure_us,
            "gain": cfg.capture.gain,
        },
        "mount": {
            "distance_from_plate_m": cfg.mount.distance_from_plate_m,
            "height_m": cfg.mount.height_m,
            "lateral_offset_m": cfg.mount.lateral_offset_m,
            "pitch_deg": cfg.mount.pitch_deg,
            "yaw_deg": cfg.mount.yaw_deg,
            "positive_depth_is_rf": cfg.mount.positive_depth_is_rf,
        },
        "simulator": {
            "launch_url": cfg.simulator.launch_url,
        },
    })
}

pub fn print_estimate(est: &tracker::HitEstimate) {
    println!(
        "ev={} launch={} spray={} samples={} confident={}",
        est.exit_velocity_mph,
        est.launch_angle_deg,
        est.spray_angle_deg,
        est.samples,
        est.confident
    );
}

pub fn print_hit(hit: &tracker::HitRecord) {
    println!(
        "{} {} #{} frames {}..{} ev={:.1} launch={:.1} spray={:.1} samples={} confident={} posted={}",
        hit.clip,
        hit.kind.as_str(),
        hit.segment,
        hit.frame_start,
        hit.frame_end,
        hit.exit_velocity_mph,
        hit.launch_angle_deg,
        hit.spray_angle_deg,
        hit.samples,
        hit.confident,
        hit.posted
    );
}
