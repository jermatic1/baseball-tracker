//! A tracker with no camera: the real server, store, sessions, and games,
//! fed made-up pitches and hits on a timer, so simulators and the pages can
//! be worked on anywhere. A pitching session gets pitches, a hitting
//! session gets hits, and with no session the two alternate.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracker::geom::launch_velocity;
use tracker::units::metres;
use tracker::{
    flight, Activity, ClipMeta, Config, DataDir, EventKind, HitRecord, HitType, Location,
    PitchPlate, Rng, Store,
};

use crate::capture::server;
use crate::hub::Hub;

pub async fn mock(path: PathBuf, period: f64) -> Result<(), String> {
    let data = DataDir::create(&path).map_err(|e| e.to_string())?;
    let store = Store::open(data.dir.join("tracker.db")).map_err(|e| e.to_string())?;
    seed(&store)?;
    let hub = Hub::with_store(data.clone(), store)?;
    #[cfg(feature = "challenges")]
    hub.register(Box::new(challenges::Challenges::load(&data.dir)?));
    let c = data.config.capture.clone();
    let cam =
        device::SyntheticCamera::new(c.width.min(640), c.height.min(400), c.fps.max(1.0) as f32);
    let calib = crate::capture::stereo_calib(&data, None);
    let generator = tokio::spawn(generate(Arc::clone(&hub), Duration::from_secs_f64(period)));
    println!("mock tracker: an event every {period} s while a client is connected");
    let result = server::run(data, cam, c.exposure, c.gain, calib, None, hub).await;
    generator.abort();
    result
}

/// Give an empty store one user so a client can start a session right away.
fn seed(store: &Store) -> Result<(), String> {
    if store.users().map_err(|e| e.to_string())?.is_empty() {
        store.add_user("Demo").map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn generate(hub: Arc<Hub>, period: Duration) {
    let mut rng = Rng::seeded();
    let mut n = 0u32;
    loop {
        tokio::time::sleep(period).await;
        if !hub.capturing() {
            continue;
        }
        let (dir, session) = hub.target();
        let cfg = hub.config();
        let kind = match hub.activity() {
            Some(Activity::Pitching) => EventKind::Pitch,
            Some(Activity::Hitting) => EventKind::Hit,
            None if n.is_multiple_of(2) => EventKind::Pitch,
            None => EventKind::Hit,
        };
        n += 1;
        let published = dir
            .next_clip_id()
            .map_err(|e| e.to_string())
            .and_then(|id| {
                let record = match kind {
                    EventKind::Pitch => mock_pitch(&mut rng, &cfg, &id),
                    _ => mock_hit(&mut rng, &cfg, &id),
                };
                dir.write_clip_meta(&id, &meta(&cfg, &record))
                    .map_err(|e| e.to_string())?;
                hub.publish(&dir, session, &id, vec![record])
            });
        if let Err(e) = published {
            eprintln!("mock event lost: {e}");
        }
    }
}

fn meta(cfg: &Config, record: &HitRecord) -> ClipMeta {
    ClipMeta {
        width: cfg.capture.width,
        height: cfg.capture.height,
        fps: cfg.capture.fps,
        frame_count: record.frame_end + 1,
        exposure_us: cfg.capture.exposure,
        gain: cfg.capture.gain,
        sequence_gaps: false,
        ir_flood: 0.0,
        ir_dot: 0.0,
    }
}

fn base(kind: EventKind, clip: &str, samples: usize) -> HitRecord {
    HitRecord {
        clip: clip.to_string(),
        kind,
        segment: 0,
        frame_start: 0,
        frame_end: samples.saturating_sub(1),
        start: 0.0,
        exit_velocity: 0.0,
        launch_angle: 0.0,
        spray_angle: 0.0,
        pitch_velocity: None,
        hit_type: None,
        contact: None,
        samples,
        confident: true,
        hit: None,
        pitch: None,
    }
}

/// A pitch somewhere around the zone, called the way the live path would.
pub fn mock_pitch(rng: &mut Rng, cfg: &Config, clip: &str) -> HitRecord {
    let zone = cfg.zone;
    let speed = rng.between(35.0, 55.0);
    let location = Location {
        side: rng.normal() * zone.width / 2.0,
        height: (zone.bottom + zone.top) / 2.0 + rng.normal() * (zone.top - zone.bottom) / 2.0,
    };
    let simulated_location = Location {
        side: location.side,
        height: location.height - 2.0,
    };
    let call = if zone.contains(simulated_location.side, simulated_location.height) {
        tracker::Call::Strike
    } else {
        tracker::Call::Ball
    };
    HitRecord {
        exit_velocity: speed,
        launch_angle: rng.between(-3.0, 3.0),
        pitch: Some(PitchPlate {
            plate_velocity: speed - 2.0,
            location,
            simulated_location,
            call,
            zone,
        }),
        ..base(EventKind::Pitch, clip, 12)
    }
}

/// A hit with the same predicted flight a real one gets.
pub fn mock_hit(rng: &mut Rng, cfg: &Config, clip: &str) -> HitRecord {
    let exit_velocity = rng.between(40.0, 80.0);
    let launch_angle = rng.between(-5.0, 35.0);
    let spray_angle = rng.between(-30.0, 30.0);
    let v = launch_velocity(exit_velocity, launch_angle, spray_angle);
    let air = flight::air_density(metres(cfg.flight.altitude));
    HitRecord {
        exit_velocity,
        launch_angle,
        spray_angle,
        pitch_velocity: Some(rng.between(35.0, 50.0)),
        hit_type: Some(HitType::from_launch_deg(launch_angle)),
        hit: Some(flight::predict([0.0, 0.9, 0.0], v, air)),
        ..base(EventKind::Hit, clip, 9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_adds_one_user_once() {
        let store = Store::in_memory().unwrap();
        seed(&store).unwrap();
        seed(&store).unwrap();
        let users = store.users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "Demo");
    }

    #[test]
    fn mock_events_look_like_real_ones() {
        let cfg = Config::example();
        let mut rng = Rng::with_seed(3);
        for _ in 0..50 {
            let p = mock_pitch(&mut rng, &cfg, "0001");
            let plate = p.pitch.unwrap();
            assert_eq!(p.kind, EventKind::Pitch);
            assert_eq!(
                plate.call == tracker::Call::Strike,
                cfg.zone.contains(
                    plate.simulated_location.side,
                    plate.simulated_location.height
                )
            );
            let h = mock_hit(&mut rng, &cfg, "0002");
            assert_eq!(h.kind, EventKind::Hit);
            let flight = h.hit.unwrap();
            assert!(flight.carry > 0.0 && flight.path.len() >= 2);
            assert!(h.hit_type.is_some() && h.pitch_velocity.is_some());
        }
    }
}
