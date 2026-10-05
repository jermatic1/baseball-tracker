pub mod config;
pub mod geom;
pub mod launch;
pub mod mot;
pub mod plate;
pub mod session;
pub mod stereo;
pub mod track;

pub use config::{CaptureConfig, MountConfig, SessionConfig, SimulatorConfig};
pub use geom::{fit_samples, fit_trajectory, HitEstimate, Intrinsics, Sample, Trajectory};
pub use launch::postable;
pub use mot::{Detection, Observation, TrackedObject, Tracker, TrackerConfig};
pub use session::{
    write_synth, Clip, ClipMeta, ClipWriter, Contact, ContactKind, Detections, EventKind,
    HitRecord, HitType, Session, StoredFrame,
};
pub use stereo::{stereo_calib_from_device, StereoCalib};
pub use track::{
    ball_at, motion_segments, select_events, track_balls, tracker_config, Anchor, AnchorKind,
    BallBox, DetFrame, Event, MotionParams, Segment, SplitKind,
};

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

/// Every moving-ball event in a clip, fitted as a trajectory.
pub fn process_clip(
    clip_id: &str,
    frames: &[DetFrame],
    cfg: &SessionConfig,
    intr: &Intrinsics,
) -> Vec<HitRecord> {
    let params = MotionParams::default();
    let default_depth = cfg.mount.distance_from_plate_m;
    let tracks = track_balls(frames, tracker_config(intr.fx, default_depth, &params));
    let events = select_events(&tracks, frames, intr.fx, default_depth, &params);
    let mut last_pitch_mph: std::collections::HashMap<u32, f64> = Default::default();
    let mut out = Vec::new();
    for ev in &events {
        let Some(track) = tracks.iter().find(|t| t.id == ev.track_id) else {
            continue;
        };
        let obs = &track.history[ev.segment.start..=ev.segment.end];
        let depths: Vec<Option<f64>> = obs
            .iter()
            .map(|o| ball_at(frames, o.frame, o.tag).and_then(|b| b.depth_m))
            .collect();
        let depths = smooth_depths(&depths, ev.median_depth_m);
        let mut samples = Vec::with_capacity(obs.len() + 1);
        if let Some(a) = &ev.segment.anchor {
            let depth = a.depth_m.unwrap_or(ev.median_depth_m);
            samples.push(sample(a.t_ns, a.cx, a.cy, depth, intr, cfg));
        }
        for (o, depth) in obs.iter().zip(depths) {
            samples.push(sample(o.t_ns, o.cx, o.cy, depth, intr, cfg));
        }
        let Some((traj, used)) = fit_free_flight(&samples) else {
            continue;
        };
        let Some(est) = traj.estimate(used) else {
            continue;
        };
        let kind = if ev.segment.after_impact() {
            EventKind::Bounce
        } else if est.spray_angle_deg.abs() > 90.0 {
            EventKind::Pitch
        } else {
            EventKind::Hit
        };
        if kind == EventKind::Pitch {
            last_pitch_mph.insert(ev.track_id, est.exit_velocity_mph);
        }
        if kind == EventKind::Hit && est.exit_velocity_mph < cfg.tracking.min_hit_mph {
            continue;
        }
        let pivot = matches!(
            ev.segment.anchor,
            Some(Anchor {
                kind: AnchorKind::Pivot,
                ..
            })
        );
        let pitch_mph = if kind == EventKind::Hit && pivot {
            last_pitch_mph.get(&ev.track_id).copied()
        } else {
            None
        };
        // A sample the fit rejected is where the flight ended; the next
        // segment starting with an impact says the same when all fit.
        let impact_next = motion_segments(track, frames, &params)
            .iter()
            .any(|s| s.start == ev.segment.end + 1 && s.after_impact());
        let contact = samples
            .get(used)
            .or_else(|| impact_next.then(|| samples.last()).flatten())
            .map(|s| contact_at(&traj, s.t_ns));
        out.push(HitRecord {
            clip: clip_id.to_string(),
            kind,
            segment: out.len(),
            frame_start: obs[0].frame,
            frame_end: obs[obs.len() - 1].frame,
            anchored: ev.segment.anchor.is_some(),
            t_start_ns: samples[0].t_ns,
            exit_velocity_mph: est.exit_velocity_mph,
            launch_angle_deg: est.launch_angle_deg,
            spray_angle_deg: est.spray_angle_deg,
            pitch_mph,
            hit_type: (kind == EventKind::Hit)
                .then(|| HitType::from_launch_deg(est.launch_angle_deg)),
            contact,
            samples: est.samples,
            confident: est.confident,
            posted: false,
        });
    }
    out
}

/// Half a ball above the floor still counts as the floor; well above it is
/// the net or something else in the cage.
const CONTACT_GROUND_M: f64 = 0.1;
const CONTACT_NET_M: f64 = 0.3;

fn contact_at(traj: &Trajectory, t_ns: u64) -> Contact {
    let [x, y, z] = traj.at(t_ns);
    let kind = if y < CONTACT_GROUND_M {
        ContactKind::Ground
    } else if y > CONTACT_NET_M {
        ContactKind::Net
    } else {
        ContactKind::Unknown
    };
    Contact {
        t_ns,
        x,
        y,
        z,
        kind,
    }
}

/// Largest residual a sample may have from the fitted flight before the ball
/// is taken to have hit something (net, floor, bat).
const FIT_RESIDUAL_M: f64 = 0.25;
const FIT_SEED: usize = 4;

/// Fit the free-flight prefix: grow the fit one sample at a time and stop at
/// the first sample that leaves the predicted path. Returns the fit and how
/// many samples it used.
fn fit_free_flight(samples: &[Sample]) -> Option<(Trajectory, usize)> {
    let mut ordered = samples.to_vec();
    ordered.sort_by_key(|s| s.t_ns);
    let mut n = ordered.len().min(FIT_SEED);
    let mut traj = fit_trajectory(&ordered[..n])?;
    while n < ordered.len() {
        let s = &ordered[n];
        let p = traj.at(s.t_ns);
        let residual = ((p[0] - s.x).powi(2) + (p[1] - s.y).powi(2) + (p[2] - s.z).powi(2)).sqrt();
        if residual > FIT_RESIDUAL_M {
            break;
        }
        n += 1;
        traj = fit_trajectory(&ordered[..n])?;
    }
    Some((traj, n))
}

fn sample(t_ns: u64, u: f64, v: f64, depth: f64, intr: &Intrinsics, cfg: &SessionConfig) -> Sample {
    let p = unproject(u, v, depth, intr, &cfg.mount);
    Sample {
        t_ns,
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

/// A point keeps its own depth; a gap takes its neighbours' median, then the
/// segment median.
fn smooth_depths(depths: &[Option<f64>], fallback: f64) -> Vec<f64> {
    (0..depths.len())
        .map(|i| {
            if let Some(d) = depths[i] {
                return d;
            }
            let lo = i.saturating_sub(1);
            let hi = (i + 1).min(depths.len() - 1);
            track::median(depths[lo..=hi].iter().flatten().copied()).unwrap_or(fallback)
        })
        .collect()
}

pub fn recompute_hits(session: &Session) -> Result<Vec<HitRecord>, TrackerError> {
    let prev: std::collections::HashMap<String, bool> = session
        .hits()?
        .into_iter()
        .map(|h| (h.key(), h.posted))
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
        for mut hit in process_clip(&id, &dets.frames, &session.config, &intr) {
            if prev.get(&hit.key()) == Some(&true) {
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

    /// Synthetic detections at 640x400 and 100 fps from field-space motion.
    struct Synth {
        intr: Intrinsics,
        cfg: SessionConfig,
        frames: Vec<DetFrame>,
    }

    const SYNTH_FPS: f64 = 100.0;

    impl Synth {
        fn new(n: usize) -> Self {
            let frames = (0..n)
                .map(|i| DetFrame {
                    index: i,
                    t_ns: (i as f64 / SYNTH_FPS * 1e9).round() as u64,
                    balls: Vec::new(),
                })
                .collect();
            // A fixed generic placement so the scenes don't follow the rig config.
            let mut cfg = SessionConfig::example();
            cfg.mount = MountConfig {
                distance_from_plate_m: 2.1,
                height_m: 0.8,
                lateral_offset_m: 0.5,
                pitch_deg: 0.0,
                yaw_deg: 0.0,
                positive_depth_is_rf: true,
            };
            Self {
                intr: Intrinsics::from_fov(640, 400, 80.0, 55.0),
                cfg,
                frames,
            }
        }

        fn push(&mut self, i: usize, u: f64, v: f64, depth: f64) {
            self.frames[i].balls.push(BallBox {
                x: u - 9.0,
                y: v - 9.0,
                w: 18.0,
                h: 18.0,
                conf: 0.9,
                depth_m: Some(depth),
            });
        }

        fn static_px(&mut self, cx: f64, cy: f64) {
            for i in 0..self.frames.len() {
                self.push(i, cx, cy, 2.5);
            }
        }

        /// A ball sitting at `point` with +-`jitter` px of detector wobble.
        fn rest(&mut self, point: [f64; 3], frames: std::ops::Range<usize>, jitter: f64) {
            let (u, v, z) = project(point, &self.intr, &self.cfg.mount).unwrap();
            for i in frames {
                let j = ((i % 3) as f64 - 1.0) * jitter;
                self.push(i, u + j, v, z);
            }
        }

        /// Ballistic flight from `start` with `vel`, launched at `t0` seconds.
        fn flight(
            &mut self,
            start: [f64; 3],
            vel: [f64; 3],
            t0: f64,
            frames: std::ops::Range<usize>,
            skip: &[usize],
        ) {
            for i in frames {
                if skip.contains(&i) {
                    continue;
                }
                let t = i as f64 / SYNTH_FPS - t0;
                let p = field_point(start, vel, t);
                let (u, v, z) = project(p, &self.intr, &self.cfg.mount).unwrap();
                self.push(i, u, v, z);
            }
        }

        fn records(&self) -> Vec<HitRecord> {
            process_clip("0001", &self.frames, &self.cfg, &self.intr)
        }
    }

    /// Rest point left of the plate; hits travel across the image to the right.
    const TEE: [f64; 3] = [-0.8, 0.9, 0.0];
    const HIT_MPH: f64 = 55.0;
    fn hit_vel() -> [f64; 3] {
        launch_velocity(HIT_MPH, 10.0, 80.0)
    }

    #[test]
    fn tee_hit_with_static_balls() {
        let mut s = Synth::new(140);
        s.static_px(80.0, 360.0);
        s.static_px(560.0, 380.0);
        s.rest(TEE, 0..130, 1.0);
        s.flight(TEE, hit_vel(), 129.5 / SYNTH_FPS, 130..140, &[]);
        let params = MotionParams::default();
        let tracks = track_balls(
            &s.frames,
            tracker_config(s.intr.fx, s.cfg.mount.distance_from_plate_m, &params),
        );
        assert_eq!(tracks.len(), 3);
        let (u, v, _) = project(TEE, &s.intr, &s.cfg.mount).unwrap();
        let tee = tracks
            .iter()
            .find(|t| (t.history[0].cx - u).abs() < 2.0 && (t.history[0].cy - v).abs() < 2.0)
            .expect("tee track");
        assert_eq!(tee.history.len(), 140, "flight joined the rest track");
        let segs = motion_segments(tee, &s.frames, &params);
        assert_eq!(segs.len(), 2, "{segs:?}");
        assert!(!segs[0].moving && segs[1].moving);
        assert_eq!((segs[1].start, segs[1].end), (130, 139));
        let anchor = segs[1].anchor.expect("anchor");
        assert!((anchor.cx - u).abs() < 0.5 && (anchor.cy - v).abs() < 0.5);
        assert!(anchor.t_ns > s.frames[129].t_ns && anchor.t_ns < s.frames[130].t_ns);
        let recs = s.records();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].kind, EventKind::Hit);
        assert!(recs[0].anchored);
        assert_eq!(recs[0].samples, 11);
        assert!(
            (recs[0].exit_velocity_mph - HIT_MPH).abs() < 1.0,
            "mph {}",
            recs[0].exit_velocity_mph
        );
        assert!((recs[0].launch_angle_deg - 10.0).abs() < 1.0);
    }

    #[test]
    fn pitch_only() {
        let mut s = Synth::new(12);
        let vel = launch_velocity(45.0, 0.0, -100.0);
        s.flight([1.2, 1.0, -0.6], vel, 0.0, 0..12, &[]);
        let recs = s.records();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].kind, EventKind::Pitch);
        assert!((recs[0].exit_velocity_mph - 45.0).abs() < 1.0);
    }

    #[test]
    fn pitch_then_hit() {
        let mut s = Synth::new(24);
        let pitch_vel = launch_velocity(40.0, 0.0, -100.0);
        let pivot = field_point([1.2, 1.0, -0.6], pitch_vel, 12.0 / SYNTH_FPS);
        s.flight([1.2, 1.0, -0.6], pitch_vel, 0.0, 0..13, &[]);
        s.flight(pivot, hit_vel(), 12.0 / SYNTH_FPS, 13..24, &[]);
        let recs = s.records();
        assert_eq!(recs.len(), 2, "{recs:?}");
        assert_eq!(recs[0].kind, EventKind::Pitch);
        assert_eq!(recs[1].kind, EventKind::Hit);
        assert!((recs[1].exit_velocity_mph - HIT_MPH).abs() < 1.0);
    }

    #[test]
    fn front_toss_at_an_angle_then_hit() {
        let mut s = Synth::new(30);
        // A slow toss arriving 40 degrees off the hit line, struck at frame 12.
        let toss_vel = launch_velocity(12.0, 0.0, -140.0);
        let toss_start = [1.0, 1.0, -0.4];
        let pivot = field_point(toss_start, toss_vel, 12.0 / SYNTH_FPS);
        let hit = launch_velocity(HIT_MPH, 15.0, 80.0);
        s.flight(toss_start, toss_vel, 0.0, 0..13, &[]);
        s.flight(pivot, hit, 12.0 / SYNTH_FPS, 13..30, &[]);
        let recs = s.records();
        assert_eq!(recs.len(), 2, "{recs:?}");
        assert_eq!(recs[0].kind, EventKind::Pitch);
        assert!((recs[0].exit_velocity_mph - 12.0).abs() < 1.0);
        let hit = &recs[1];
        assert_eq!(hit.kind, EventKind::Hit);
        assert!(hit.anchored);
        assert!((hit.pitch_mph.unwrap() - 12.0).abs() < 1.0);
        assert!(
            (hit.exit_velocity_mph - HIT_MPH).abs() < 1.0,
            "{}",
            hit.exit_velocity_mph
        );
        assert_eq!(hit.hit_type, Some(HitType::LineDrive));
    }

    #[test]
    fn slow_toss_alone_is_a_pitch_not_a_hit() {
        let mut s = Synth::new(20);
        s.flight(
            [1.0, 1.0, -0.4],
            launch_velocity(8.0, 0.0, -140.0),
            0.0,
            0..20,
            &[],
        );
        let recs = s.records();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].kind, EventKind::Pitch);
    }

    #[test]
    fn ground_ball_bounce() {
        let mut s = Synth::new(80);
        s.rest(TEE, 0..30, 1.0);
        let vel = launch_velocity(45.0, -10.0, 80.0);
        // Time to the floor from the tee height: 0 = y0 + vy t - g t^2 / 2.
        let g = 9.80665;
        let t_floor = (vel[1] + (vel[1] * vel[1] + 2.0 * g * TEE[1]).sqrt()) / g;
        let t0 = 29.5 / SYNTH_FPS;
        let first_bounce_frame = ((t0 + t_floor) * SYNTH_FPS).ceil() as usize;
        s.flight(TEE, vel, t0, 30..first_bounce_frame, &[]);
        let land = field_point(TEE, vel, t_floor);
        let vy_at_floor = vel[1] - g * t_floor;
        let bounce_vel = [vel[0] * 0.8, -vy_at_floor * 0.6, vel[2] * 0.8];
        s.flight(
            [land[0], 0.0, land[2]],
            bounce_vel,
            t0 + t_floor,
            first_bounce_frame..80,
            &[],
        );
        let recs = s.records();
        let hits: Vec<_> = recs.iter().filter(|r| r.kind == EventKind::Hit).collect();
        assert_eq!(hits.len(), 1, "{recs:?}");
        assert_eq!(hits[0].hit_type, Some(HitType::Ground));
        let contact = hits[0].contact.expect("contact");
        assert_eq!(contact.kind, ContactKind::Ground, "{contact:?}");
        assert!(
            (hits[0].exit_velocity_mph - 45.0).abs() < 1.5,
            "{}",
            hits[0].exit_velocity_mph
        );
        assert!(recs.iter().any(|r| r.kind == EventKind::Bounce), "{recs:?}");
    }

    #[test]
    fn occlusion_after_contact() {
        let mut s = Synth::new(140);
        s.rest(TEE, 0..130, 1.0);
        s.flight(TEE, hit_vel(), 129.5 / SYNTH_FPS, 130..140, &[131, 132]);
        let recs = s.records();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert!(recs[0].anchored);
        assert_eq!(recs[0].samples, 9);
        assert!((recs[0].exit_velocity_mph - HIT_MPH).abs() < 1.0);
    }

    #[test]
    fn rest_jitter_is_not_an_event() {
        let mut s = Synth::new(60);
        s.rest(TEE, 0..60, 2.0);
        s.static_px(80.0, 360.0);
        assert!(s.records().is_empty());
    }

    #[test]
    fn select_events_ignores_static_ball() {
        let w = 640u32;
        let h = 400u32;
        let mut frames = Vec::new();
        for i in 0..6 {
            let t_ns = i as u64 * 16_666_667;
            let ground = box_at(80.0, 360.0);
            let hit = box_at(120.0 + 50.0 * i as f64, 220.0 - 30.0 * i as f64);
            frames.push(DetFrame {
                index: i,
                t_ns,
                balls: vec![ground, hit],
            });
        }
        let intr = Intrinsics::from_fov(w, h, 80.0, 55.0);
        let params = MotionParams::default();
        let tracks = track_balls(&frames, tracker_config(intr.fx, 2.0, &params));
        assert_eq!(tracks.len(), 2);
        let events = select_events(&tracks, &frames, intr.fx, 2.0, &params);
        assert_eq!(events.len(), 1);
        let moving = tracks.iter().find(|t| t.id == events[0].track_id).unwrap();
        assert!(moving.history[0].cx > 100.0);
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
        assert_eq!(session.config.capture.width, 640);
        let w = 8u32;
        let h = 4u32;
        let frames = vec![
            StoredFrame {
                width: w,
                height: h,
                left: (0..32).collect(),
                right: Vec::new(),
                depth_mm: (0..32).map(|i| i as u16 * 10).collect(),
                t_ns: 0,
                sequence: 1,
            },
            StoredFrame {
                width: w,
                height: h,
                left: (32..64).map(|x| x as u8).collect(),
                right: Vec::new(),
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
            kind: EventKind::Hit,
            segment: 0,
            frame_start: 0,
            frame_end: 1,
            anchored: false,
            t_start_ns: 0,
            exit_velocity_mph: 65.0,
            launch_angle_deg: 18.0,
            spray_angle_deg: -12.0,
            pitch_mph: None,
            hit_type: Some(HitType::LineDrive),
            contact: None,
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
