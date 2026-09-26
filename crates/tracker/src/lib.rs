pub mod config;
pub mod geom;
pub mod launch;
pub mod session;
pub mod track;

pub use config::{CaptureConfig, MountConfig, SessionConfig, SimulatorConfig};
pub use geom::{fit_samples, HitEstimate, Intrinsics, Sample};
pub use launch::postable;
pub use session::{
    write_synth, Clip, ClipMeta, ClipWriter, Detections, HitRecord, Session, StoredFrame,
};
pub use track::{select_hit, BallBox, DetFrame, Track};

use geom::unproject;

#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("{0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("{0}")]
    Other(String),
}

pub fn process_clip(
    clip_id: &str,
    frames: &[DetFrame],
    cfg: &SessionConfig,
    intr: &Intrinsics,
    width: u32,
    height: u32,
) -> Option<HitRecord> {
    let track = select_hit(frames, width, height)?;
    let mut samples = Vec::new();
    for (idx, b) in &track.points {
        let Some(depth) = b.depth_m else {
            continue;
        };
        let Some(fr) = frames.iter().find(|f| f.index == *idx) else {
            continue;
        };
        let (u, v) = b.centroid();
        let p = unproject(u, v, depth, intr, &cfg.mount);
        samples.push(Sample {
            t_ns: fr.t_ns,
            x: p[0],
            y: p[1],
            z: p[2],
        });
    }
    let est = fit_samples(&samples)?;
    Some(HitRecord {
        clip: clip_id.to_string(),
        exit_velocity_mph: est.exit_velocity_mph,
        launch_angle_deg: est.launch_angle_deg,
        spray_angle_deg: est.spray_angle_deg,
        samples: est.samples,
        confident: est.confident,
        posted: false,
    })
}

pub fn recompute_hits(session: &Session) -> Result<Vec<HitRecord>, TrackerError> {
    let prev: std::collections::HashMap<String, bool> = session
        .hits()?
        .into_iter()
        .map(|h| (h.clip, h.posted))
        .collect();
    let mut out = Vec::new();
    for id in session.clip_ids()? {
        let dets = match session.load_detections(&id) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let clip = session.load_clip(&id)?;
        let w = clip.meta.width;
        let h = clip.meta.height;
        let intr = Intrinsics::from_fov(w, h, 80.0, 55.0);
        if let Some(mut hit) = process_clip(&id, &dets.frames, &session.config, &intr, w, h) {
            if prev.get(&id) == Some(&true) {
                hit.posted = true;
            }
            out.push(hit);
        }
    }
    session.write_hits(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{field_point, launch_velocity, project, unproject};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("tracker-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn traj_samples(mph: f64, launch: f64, spray: f64, n: usize, fps: f64) -> Vec<Sample> {
        let vel = launch_velocity(mph, launch, spray);
        let start = [0.0, 0.9, 0.0];
        (0..n)
            .map(|i| {
                let t = i as f64 / fps;
                let p = field_point(start, vel, t);
                Sample {
                    t_ns: (t * 1e9).round() as u64,
                    x: p[0],
                    y: p[1],
                    z: p[2],
                }
            })
            .collect()
    }

    fn box_at(cx: f64, cy: f64) -> BallBox {
        BallBox {
            x: cx - 10.0,
            y: cy - 10.0,
            w: 20.0,
            h: 20.0,
            conf: 0.9,
            depth_m: Some(2.0),
        }
    }

    #[test]
    fn center_field_65_mph() {
        let samples = traj_samples(65.0, 0.0, 0.0, 8, 60.0);
        let est = fit_samples(&samples).unwrap();
        assert!((est.exit_velocity_mph - 65.0).abs() < 0.05);
        assert!(est.launch_angle_deg.abs() < 0.05);
        assert!(est.spray_angle_deg.abs() < 0.05);
    }

    #[test]
    fn launch_and_spray_with_gravity() {
        let samples = traj_samples(65.0, 18.0, -12.0, 8, 60.0);
        let est = fit_samples(&samples).unwrap();
        assert!((est.exit_velocity_mph - 65.0).abs() < 0.1);
        assert!((est.launch_angle_deg - 18.0).abs() < 0.1);
        assert!((est.spray_angle_deg - (-12.0)).abs() < 0.1);
    }

    #[test]
    fn positive_spray_is_positive() {
        let samples = traj_samples(70.0, 10.0, 15.0, 6, 60.0);
        let est = fit_samples(&samples).unwrap();
        assert!(est.spray_angle_deg > 0.0);
    }

    #[test]
    fn select_hit_keeps_up_right_fast_ball() {
        let w = 640u32;
        let h = 400u32;
        let mut frames = Vec::new();
        for i in 0..6 {
            let t_ns = i as u64 * 16_666_667;
            let ground = box_at(80.0, 360.0);
            let pitch = box_at(560.0 - 50.0 * i as f64, 200.0);
            let hit = box_at(120.0 + 50.0 * i as f64, 220.0 - 30.0 * i as f64);
            frames.push(DetFrame {
                index: i,
                t_ns,
                balls: vec![ground, pitch, hit],
            });
        }
        let track = select_hit(&frames, w, h).expect("hit track");
        let first = track.points.first().unwrap().1.centroid();
        let last = track.points.last().unwrap().1.centroid();
        assert!(last.0 - first.0 > 20.0);
        assert!(last.1 - first.1 < -20.0);
        assert!(first.0 < 200.0);
    }

    #[test]
    fn unproject_project_roundtrip() {
        let cfg = SessionConfig::example();
        let intr = Intrinsics::from_fov(1280, 800, 80.0, 55.0);
        let p = [0.12, 1.05, -0.4];
        let (u, v, z) = project(p, &intr, &cfg.mount).unwrap();
        let q = unproject(u, v, z, &intr, &cfg.mount);
        assert!((p[0] - q[0]).abs() < 1e-9);
        assert!((p[1] - q[1]).abs() < 1e-9);
        assert!((p[2] - q[2]).abs() < 1e-9);
    }

    #[test]
    fn positive_depth_is_rf_flips_field_x() {
        let mut cfg = SessionConfig::example();
        let intr = Intrinsics::from_fov(1280, 800, 80.0, 55.0);
        cfg.mount.positive_depth_is_rf = true;
        let p_true = unproject(intr.cx + 40.0, intr.cy + 10.0, 2.2, &intr, &cfg.mount);
        cfg.mount.positive_depth_is_rf = false;
        let p_false = unproject(intr.cx + 40.0, intr.cy + 10.0, 2.2, &intr, &cfg.mount);
        assert!((p_true[0] + p_false[0]).abs() < 1e-9);
        assert!((p_true[1] - p_false[1]).abs() < 1e-9);
        assert!((p_true[2] - p_false[2]).abs() < 1e-9);
        assert!(p_true[0].abs() > 1e-6);
    }

    #[test]
    fn session_save_load_roundtrip() {
        let dir = temp_dir();
        let session = Session::create(&dir).unwrap();
        assert_eq!(session.config.capture.width, 1280);
        let w = 8u32;
        let h = 4u32;
        let frames = vec![
            StoredFrame {
                width: w,
                height: h,
                left: (0..32).collect(),
                depth_mm: (0..32).map(|i| i as u16 * 10).collect(),
                t_ns: 0,
                sequence: 1,
            },
            StoredFrame {
                width: w,
                height: h,
                left: (32..64).map(|x| x as u8).collect(),
                depth_mm: vec![7; 32],
                t_ns: 16_666_667,
                sequence: 2,
            },
        ];
        let id = session.save_clip(&frames, 1000, 100).unwrap();
        assert_eq!(id, "0001");
        let clip = session.load_clip(&id).unwrap();
        assert_eq!(clip.left_frame(0).unwrap(), frames[0].left);
        assert_eq!(clip.depth_frame(1).unwrap(), frames[1].depth_mm);
        assert_eq!(clip.meta.frame_count, 2);
        assert!((clip.meta.fps - 60.0).abs() < 1.0);
        assert!(!clip.meta.sequence_gaps);

        let dets = Detections {
            frames: vec![DetFrame {
                index: 0,
                t_ns: 0,
                balls: vec![BallBox {
                    x: 1.0,
                    y: 2.0,
                    w: 3.0,
                    h: 4.0,
                    conf: 0.9,
                    depth_m: Some(1.5),
                }],
            }],
        };
        session.save_detections(&id, &dets).unwrap();
        let loaded = session.load_detections(&id).unwrap();
        assert_eq!(loaded.frames[0].balls[0].w, 3.0);

        let hits = vec![HitRecord {
            clip: id.clone(),
            exit_velocity_mph: 65.0,
            launch_angle_deg: 18.0,
            spray_angle_deg: -12.0,
            samples: 8,
            confident: true,
            posted: true,
        }];
        session.write_hits(&hits).unwrap();
        let opened = Session::open(&dir).unwrap();
        let h2 = opened.hits().unwrap();
        assert!(h2[0].posted);
        assert!(opened.config.mount.positive_depth_is_rf);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_synth_recovery() {
        let dir = temp_dir();
        let est = write_synth(&dir).unwrap();
        assert!(
            (est.exit_velocity_mph - 65.0).abs() < 1.0,
            "exit {}",
            est.exit_velocity_mph
        );
        assert!(
            (est.launch_angle_deg - 18.0).abs() < 1.0,
            "launch {}",
            est.launch_angle_deg
        );
        assert!(
            (est.spray_angle_deg - (-12.0)).abs() < 1.0,
            "spray {}",
            est.spray_angle_deg
        );
        let session = Session::open(&dir).unwrap();
        let hits = recompute_hits(&session).unwrap();
        assert_eq!(hits.len(), 1);
        assert!((hits[0].exit_velocity_mph - 65.0).abs() < 1.0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
