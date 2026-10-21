pub mod clips;
pub mod config;
pub mod flight;
pub mod geom;
pub mod mot;
pub mod plate;
pub mod rectify;
pub mod stats;
pub mod stereo;
pub mod store;
pub mod track;
pub mod units;
pub mod watch;

pub use clips::{
    write_synth, Call, Clip, ClipMeta, ClipWriter, Contact, ContactKind, DataDir, Detections,
    EventKind, HitFlight, HitRecord, HitType, Landing, Location, PitchPlate, StoredFrame,
};
pub use config::{
    CaptureConfig, Config, FlightConfig, MountConfig, PitchingConfig, WatchConfig, ZoneConfig,
};
pub use geom::{fit_samples, fit_trajectory, HitEstimate, Intrinsics, Sample, Trajectory};
pub use mot::{Detection, Observation, TrackedObject, Tracker, TrackerConfig};
pub use rectify::{Eye, Rectifier};
pub use stats::{summary_lines, SummaryLine};
pub use stereo::{stereo_calib_from_device, StereoCalib};
pub use store::{Activity, Mode, Session, Store, User};
pub use track::{
    ball_at, motion_segments, select_events, track_balls, tracker_config, Anchor, AnchorKind,
    BallBox, DetFrame, Event, MotionParams, Segment, SplitKind,
};
pub use watch::{Episode, WatchState, Watcher};

use crate::units::{feet, inches, metres, mph};

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
    #[error("store: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

/// Every moving-ball event in a clip, fitted as a trajectory.
pub fn process_clip(
    clip_id: &str,
    frames: &[DetFrame],
    cfg: &Config,
    intr: &Intrinsics,
) -> Vec<HitRecord> {
    let params = MotionParams::default();
    let default_depth = metres(cfg.mount.distance);
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
        } else if est.spray_angle.abs() > 90.0 {
            EventKind::Pitch
        } else {
            EventKind::Hit
        };
        if kind == EventKind::Pitch {
            last_pitch_mph.insert(ev.track_id, est.exit_velocity);
        }
        if kind == EventKind::Hit && est.exit_velocity < cfg.tracking.min_hit_velocity {
            continue;
        }
        let pivot = matches!(
            ev.segment.anchor,
            Some(Anchor {
                kind: AnchorKind::Pivot,
                ..
            })
        );
        let pitch_velocity = if kind == EventKind::Hit && pivot {
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
        let hit = (kind == EventKind::Hit).then(|| {
            let air = flight::air_density(metres(cfg.flight.altitude));
            flight::predict(traj.p0, traj.v, air)
        });
        let pitch = (kind == EventKind::Pitch)
            .then(|| pitch_plate(&traj, &cfg.pitching, &cfg.zone))
            .flatten();
        out.push(HitRecord {
            clip: clip_id.to_string(),
            kind,
            segment: out.len(),
            frame_start: obs[0].frame,
            frame_end: obs[obs.len() - 1].frame,
            start: samples[0].t_ns as f64 / 1e9,
            exit_velocity: est.exit_velocity,
            launch_angle: est.launch_angle,
            spray_angle: est.spray_angle,
            pitch_velocity,
            hit_type: (kind == EventKind::Hit).then(|| HitType::from_launch_deg(est.launch_angle)),
            contact,
            samples: est.samples,
            confident: est.confident,
            hit,
            pitch,
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
        time: t_ns as f64 / 1e9,
        x: feet(x),
        y: feet(y),
        z: feet(z),
        kind,
    }
}

/// Front edge of the plate, where pitch location is read.
const PLATE_FRONT_M: f64 = -17.0 * 0.0254;
/// Slowest approach toward the plate that counts as a pitch crossing it.
const MIN_APPROACH_MPS: f64 = 1.0;

/// Where the fitted flight crosses the front of the plate, and where it would
/// have crossed had the same throw started the simulated distance away.
fn pitch_plate(traj: &Trajectory, cfg: &PitchingConfig, zone: &ZoneConfig) -> Option<PitchPlate> {
    let vz = traj.v[2];
    if vz < MIN_APPROACH_MPS {
        return None;
    }
    let t = (PLATE_FRONT_M - traj.p0[2]) / vz;
    let [x, y, _] = traj.at_s(t);
    if y <= 0.0 {
        return None;
    }
    let extra = metres(cfg.simulated_distance - cfg.actual_distance);
    let [sx, sy, _] = traj.at_s(t + extra / vz);
    let [vx, vy, vz] = traj.velocity_s(t);
    let simulated_location = Location {
        side: inches(sx),
        height: inches(sy),
    };
    let call = if zone.contains(simulated_location.side, simulated_location.height) {
        Call::Strike
    } else {
        Call::Ball
    };
    Some(PitchPlate {
        plate_velocity: mph((vx * vx + vy * vy + vz * vz).sqrt()),
        location: Location {
            side: inches(x),
            height: inches(y),
        },
        simulated_location,
        call,
        zone: *zone,
    })
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

fn sample(t_ns: u64, u: f64, v: f64, depth: f64, intr: &Intrinsics, cfg: &Config) -> Sample {
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

/// The events of one clip from its saved detections.
pub fn clip_hits(session: &DataDir, id: &str) -> Result<Vec<HitRecord>, TrackerError> {
    let dets = session.load_detections(id)?;
    let clip = session.load_clip(id)?;
    let intr = Intrinsics::from_fov(clip.meta.width, clip.meta.height, 80.0, 55.0);
    Ok(process_clip(id, &dets.frames, &session.config, &intr))
}

/// Replace a clip's records in the session's list, keeping the rest.
pub fn merge_clip_hits(
    existing: Vec<HitRecord>,
    clip: &str,
    new: Vec<HitRecord>,
) -> Vec<HitRecord> {
    let mut out: Vec<HitRecord> = existing.into_iter().filter(|h| h.clip != clip).collect();
    out.extend(new);
    out.sort_by(|a, b| a.clip.cmp(&b.clip));
    out
}

pub fn recompute_hits(session: &DataDir) -> Result<Vec<HitRecord>, TrackerError> {
    let prev = session.hits()?;
    let mut out = Vec::new();
    for id in session.clip_ids()? {
        if session.load_detections(&id).is_err() {
            continue;
        }
        let old: Vec<HitRecord> = prev.iter().filter(|h| h.clip == id).cloned().collect();
        out.extend(merge_clip_hits(old, &id, clip_hits(session, &id)?));
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
        cfg: Config,
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
            let mut cfg = Config::example();
            cfg.mount = MountConfig {
                distance: feet(2.1),
                height: feet(0.8),
                lateral: feet(0.5),
                pitch: 0.0,
                yaw: 0.0,
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
            tracker_config(s.intr.fx, metres(s.cfg.mount.distance), &params),
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
        assert_eq!(recs[0].samples, 11);
        assert!(
            (recs[0].exit_velocity - HIT_MPH).abs() < 1.0,
            "mph {}",
            recs[0].exit_velocity
        );
        assert!((recs[0].launch_angle - 10.0).abs() < 1.0);
    }

    #[test]
    fn pitch_only() {
        let mut s = Synth::new(12);
        let vel = launch_velocity(45.0, 0.0, -100.0);
        s.flight([1.2, 1.0, -0.6], vel, 0.0, 0..12, &[]);
        let recs = s.records();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].kind, EventKind::Pitch);
        assert!((recs[0].exit_velocity - 45.0).abs() < 1.0);
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
        assert!((recs[1].exit_velocity - HIT_MPH).abs() < 1.0);
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
        assert!((recs[0].exit_velocity - 12.0).abs() < 1.0);
        let hit = &recs[1];
        assert_eq!(hit.kind, EventKind::Hit);
        assert!((hit.pitch_velocity.unwrap() - 12.0).abs() < 1.0);
        assert!(
            (hit.exit_velocity - HIT_MPH).abs() < 1.0,
            "{}",
            hit.exit_velocity
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
            (hits[0].exit_velocity - 45.0).abs() < 1.5,
            "{}",
            hits[0].exit_velocity
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
        assert_eq!(recs[0].samples, 9);
        assert!((recs[0].exit_velocity - HIT_MPH).abs() < 1.0);
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
        assert!((est.exit_velocity - 65.0).abs() < 0.05);
        assert!(est.launch_angle.abs() < 0.05);
        assert!(est.spray_angle.abs() < 0.05);
    }

    #[test]
    fn launch_and_spray_with_gravity() {
        let samples = traj_samples(65.0, 18.0, -12.0, 8, 60.0);
        let est = fit_samples(&samples).unwrap();
        assert!((est.exit_velocity - 65.0).abs() < 0.1);
        assert!((est.launch_angle - 18.0).abs() < 0.1);
        assert!((est.spray_angle - (-12.0)).abs() < 0.1);
    }

    #[test]
    fn positive_spray_is_positive() {
        let samples = traj_samples(70.0, 10.0, 15.0, 6, 60.0);
        let est = fit_samples(&samples).unwrap();
        assert!(est.spray_angle > 0.0);
    }

    #[test]
    fn unproject_project_roundtrip() {
        let cfg = Config::example();
        let intr = Intrinsics::from_fov(1280, 800, 80.0, 55.0);
        let p = [0.12, 1.05, -0.4];
        let (u, v, z) = project(p, &intr, &cfg.mount).unwrap();
        let q = unproject(u, v, z, &intr, &cfg.mount);
        assert!((p[0] - q[0]).abs() < 1e-9);
        assert!((p[1] - q[1]).abs() < 1e-9);
        assert!((p[2] - q[2]).abs() < 1e-9);
    }

    #[test]
    fn session_save_load_roundtrip() {
        let dir = temp_dir();
        let session = DataDir::create(&dir).unwrap();
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
            start: 0.0,
            exit_velocity: 65.0,
            launch_angle: 18.0,
            spray_angle: -12.0,
            pitch_velocity: None,
            hit_type: Some(HitType::LineDrive),
            contact: None,
            samples: 8,
            confident: true,
            hit: None,
            pitch: None,
        }];
        session.write_hits(&hits).unwrap();
        let opened = DataDir::open(&dir).unwrap();
        let h2 = opened.hits().unwrap();
        assert_eq!(h2[0].clip, id);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_synth_recovery() {
        let dir = temp_dir();
        let est = write_synth(&dir).unwrap();
        assert!(
            (est.exit_velocity - 65.0).abs() < 1.0,
            "exit {}",
            est.exit_velocity
        );
        assert!(
            (est.launch_angle - 18.0).abs() < 1.0,
            "launch {}",
            est.launch_angle
        );
        assert!(
            (est.spray_angle - (-12.0)).abs() < 1.0,
            "spray {}",
            est.spray_angle
        );
        let session = DataDir::open(&dir).unwrap();
        let hits = recompute_hits(&session).unwrap();
        assert_eq!(hits.len(), 1);
        assert!((hits[0].exit_velocity - 65.0).abs() < 1.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn feed(s: &Synth, cfg: WatchConfig) -> Vec<Episode> {
        let mut w = Watcher::new(cfg);
        s.frames
            .iter()
            .filter_map(|f| w.observe(f.index, f.t_ns, f.balls.clone()))
            .collect()
    }

    #[test]
    fn watcher_ignores_a_resting_ball() {
        let mut s = Synth::new(300);
        s.rest(TEE, 0..300, 1.0);
        for i in (0..300).step_by(7) {
            s.frames[i].balls.clear();
        }
        let mut w = Watcher::new(WatchConfig::default());
        for f in &s.frames {
            assert!(w.observe(f.index, f.t_ns, f.balls.clone()).is_none());
        }
        assert_eq!(w.state(), WatchState::Idle);
        assert!(w.flush().is_none());
    }

    #[test]
    fn watcher_window_measures_like_the_whole_clip() {
        let mut s = Synth::new(180);
        s.static_px(80.0, 360.0);
        s.rest(TEE, 0..130, 1.0);
        s.flight(TEE, hit_vel(), 129.5 / SYNTH_FPS, 130..140, &[]);
        let eps = feed(&s, WatchConfig::default());
        assert_eq!(eps.len(), 1, "{eps:?}");
        let ep = &eps[0];
        assert_eq!(ep.first(), 80, "half a second of pre-roll before frame 130");
        assert_eq!(ep.last(), 154, "settles 0.15 s after the last flight frame");
        let live = process_clip("0001", &ep.frames, &s.cfg, &s.intr);
        let batch = s.records();
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(batch.len(), 1);
        assert_eq!(live[0].kind, EventKind::Hit);
        assert_eq!(live[0].samples, batch[0].samples);
        assert!((live[0].exit_velocity - batch[0].exit_velocity).abs() < 0.1);
        assert!((live[0].launch_angle - batch[0].launch_angle).abs() < 0.1);
        assert_eq!(live[0].frame_start, batch[0].frame_start);
        let clip = ep.as_clip();
        assert_eq!(clip[0].index, 0);
        let saved = process_clip("0002", &clip, &s.cfg, &s.intr);
        assert_eq!(saved[0].frame_start, batch[0].frame_start - 80);
    }

    #[test]
    fn watcher_catches_a_pitch_entering_the_image() {
        let mut s = Synth::new(60);
        let vel = launch_velocity(45.0, 0.0, -100.0);
        s.flight([1.2, 1.0, -0.6], vel, 0.0, 0..12, &[]);
        let eps = feed(&s, WatchConfig::default());
        assert_eq!(eps.len(), 1, "{eps:?}");
        assert_eq!(eps[0].first(), 0);
        let live = process_clip("0001", &eps[0].frames, &s.cfg, &s.intr);
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].kind, EventKind::Pitch);
    }

    #[test]
    fn watcher_closes_a_window_that_runs_too_long() {
        let cfg = WatchConfig {
            max_episode: 1.0,
            ..WatchConfig::default()
        };
        let mut s = Synth::new(300);
        for i in 0..300 {
            s.push(i, 50.0 + (i as f64 * 10.0) % 500.0, 200.0, 2.5);
        }
        let eps = feed(&s, cfg);
        assert_eq!(
            eps.len(),
            2,
            "{:?}",
            eps.iter()
                .map(|e| (e.first(), e.last()))
                .collect::<Vec<_>>()
        );
        assert_eq!(eps[0].last(), 110);
        assert_eq!(eps[1].first(), 61, "pre-roll survives the cut");
        assert_eq!(eps[1].last(), 211);
    }

    #[test]
    fn pitch_location_drops_when_projected_farther() {
        let traj = Trajectory {
            t0_ns: 0,
            p0: [0.1, 1.5, -8.0],
            v: [0.0, 0.5, 20.0],
        };
        let cfg = PitchingConfig {
            actual_distance: 30.0,
            simulated_distance: 46.0,
        };
        let zone = ZoneConfig::default();
        let p = pitch_plate(&traj, &cfg, &zone).expect("crosses the plate");
        assert!(p.location.height > 0.0);
        assert!(p.simulated_location.height < p.location.height - 12.0);
        assert!((p.location.side - inches(0.1)).abs() < 1e-9);
        assert!((p.plate_velocity - mph(20.0)).abs() < 1.0);
        let same = pitch_plate(&traj, &PitchingConfig::default(), &zone).unwrap();
        assert_eq!(same.location, same.simulated_location);
        let away = Trajectory {
            v: [0.0, 0.5, -20.0],
            ..traj
        };
        assert!(pitch_plate(&away, &cfg, &zone).is_none());
    }

    #[test]
    fn pitches_are_called_on_the_simulated_crossing() {
        let zone = ZoneConfig::default();
        let cfg = PitchingConfig::default();
        let middle = Trajectory {
            t0_ns: 0,
            p0: [0.0, 1.2, -8.0],
            v: [0.0, 0.5, 20.0],
        };
        let p = pitch_plate(&middle, &cfg, &zone).unwrap();
        assert_eq!(p.call, Call::Strike);
        assert_eq!(p.zone, zone);
        let wide = Trajectory {
            p0: [0.3, 0.9, -8.0],
            ..middle
        };
        assert_eq!(pitch_plate(&wide, &cfg, &zone).unwrap().call, Call::Ball);
        // Thrown from closer, the same ball drops below the zone by the mound.
        let close = PitchingConfig {
            actual_distance: 30.0,
            simulated_distance: 46.0,
        };
        let low = Trajectory {
            p0: [0.0, 0.95, -8.0],
            v: [0.0, 2.0, 20.0],
            ..middle
        };
        let p = pitch_plate(&low, &close, &zone).unwrap();
        assert!(zone.contains(p.location.side, p.location.height));
        assert_eq!(p.call, Call::Ball);
    }

    #[test]
    fn merge_clip_hits_replaces_one_clip() {
        let mut s = Synth::new(12);
        s.flight(
            [1.2, 1.0, -0.6],
            launch_velocity(45.0, 0.0, -100.0),
            0.0,
            0..12,
            &[],
        );
        let rec = s.records().remove(0);
        let with = |clip: &str, segment: usize| HitRecord {
            clip: clip.to_string(),
            segment,
            ..rec.clone()
        };
        let existing = vec![
            with("0001", 0),
            with("0002", 0),
            with("0002", 1),
            with("0003", 0),
        ];
        let merged = merge_clip_hits(existing, "0002", vec![with("0002", 0)]);
        let keys: Vec<_> = merged
            .iter()
            .map(|h| (h.clip.as_str(), h.segment))
            .collect();
        assert_eq!(keys, vec![("0001", 0), ("0002", 0), ("0003", 0)]);
    }
}
