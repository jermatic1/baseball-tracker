//! Live tracking: the capture server plus a detector that sees every frame,
//! a watcher that cuts the windows where a ball moved, and a coordinator
//! that measures each window, broadcasts its events, records them under the
//! active session, and saves it as a clip. The web pages run from startup,
//! but the camera and the detector come up only while the hub wants them
//! (see `hub::Gate`), and are released a while after it stops (see
//! `standby`). Detection pauses during calibration. With no model file yet
//! the camera still runs, for the preview and for recording clips to label.
//! A promoted model loads on its own thread while the old one keeps
//! detecting, then is handed to the detector thread, which swaps it in
//! between frames; in standby it is built and dropped, so the next start
//! finds its engine cached.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracker::{
    ball_position, process_clip, watch::box_changed, Activity, ClipMeta, ClipWriter, DataDir,
    Detections, Episode, Intrinsics, Rectifier, StereoCalib, StoredFrame, WatchConfig, WatchState,
    Watcher,
};

use crate::capture::server::{self, LiveTap};
use crate::convert::print_hit;
use crate::detector::{Detector, FrameView};
use crate::hub::Hub;
use crate::protocol::{LiveBall, LiveStatus};
use crate::stall::Stages;
use crate::standby::{Engine, OnDemand, Open};

/// Frames queued for the detector before the newest are dropped: about
/// three seconds.
const QUEUE: usize = 300;
/// How often the detector thread checks for engine changes with no frames.
const ENGINE_POLL: Duration = Duration::from_millis(200);
/// How many frames back a detection's pixels are compared, to tell a
/// moving ball from something fixed that the detector keeps finding.
const CHANGE_FRAMES: usize = 3;

/// Where a window's clip goes and the session it belongs to, fixed when
/// the window opens.
struct Target {
    dir: DataDir,
    session: Option<i64>,
    activity: Option<Activity>,
}

impl Target {
    fn now(hub: &Hub) -> Self {
        let (dir, session) = hub.target();
        Self {
            dir,
            session,
            activity: hub.activity(),
        }
    }
}

/// A finished window and its clip id.
struct Finished {
    target: Target,
    id: String,
    episode: Episode,
}

/// Handles the detector thread shares with the server and its clients.
struct Shared {
    status: Arc<Mutex<LiveStatus>>,
    sent: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    hub: Arc<Hub>,
    engine: Receiver<Engine>,
}

/// The detector while the camera is up, with the stereo geometry it reads.
struct Running {
    detector: Detector,
    calib: StereoCalib,
    rectifier: Option<Rectifier>,
}

/// Loads a model with its engine built, one load at a time so a start and a
/// promoted model never build engines side by side.
struct Models {
    conf: f32,
    width: u32,
    height: u32,
    lock: Mutex<()>,
}

impl Models {
    fn load(&self, model: &Path) -> Result<(Detector, tracker::ModelInfo), String> {
        let _one = self.lock.lock().map_err(|e| e.to_string())?;
        let detector = Detector::load_warm(model, self.conf, self.width, self.height)?;
        let info = model_info(model, detector.model_hash());
        Ok((detector, info))
    }
}

pub async fn watch(
    path: PathBuf,
    model: String,
    from: Option<PathBuf>,
    gap: f64,
    bind: std::net::SocketAddr,
) -> Result<(), String> {
    let session = DataDir::create(&path).map_err(|e| e.to_string())?;
    let hub = Hub::open(session.clone())?;
    let model_path = Path::new(&model);
    if model_path.is_file() {
        hub.set_model(model_info(
            model_path,
            &tracker::model_record::file_sha1(model_path)?,
        ));
        println!("live model: {model}");
    } else {
        println!(
            "no detector at {model}: record clips from the tracker page, label them, and train one"
        );
    }
    #[cfg(feature = "challenges")]
    hub.register(Box::new(challenges::Challenges::load(&session.dir)?));
    let (open, calib) = camera(&session, from, gap)?;
    let status = Arc::new(Mutex::new(LiveStatus {
        state: "standby".into(),
        ..LiveStatus::default()
    }));
    let (frame_tx, frame_rx) = mpsc::sync_channel(QUEUE);
    let (done_tx, done_rx) = unbounded_channel();
    let (engine_tx, engine_rx) = mpsc::channel();
    let c = session.config.capture.clone();
    let models = Arc::new(Models {
        conf: session.config.detector.conf,
        width: c.width,
        height: c.height,
        lock: Mutex::new(()),
    });
    let tap = LiveTap {
        frames: frame_tx,
        sent: Arc::new(AtomicU64::new(0)),
        drops: Arc::new(AtomicU64::new(0)),
        status: Arc::clone(&status),
        stop: Arc::new(AtomicBool::new(false)),
        reload: loader(engine_tx.clone(), Arc::clone(&models), Arc::clone(&hub)),
    };
    let shared = Shared {
        status: Arc::clone(&status),
        sent: Arc::clone(&tap.sent),
        stop: Arc::clone(&tap.stop),
        hub: Arc::clone(&hub),
        engine: engine_rx,
    };
    let load = {
        let hub = Arc::clone(&hub);
        let model = PathBuf::from(&model);
        Box::new(move || {
            if !model.is_file() {
                return Ok(None);
            }
            let (detector, info) = models.load(&model)?;
            hub.set_model(info);
            Ok(Some(detector))
        })
    };
    let cam = OnDemand::new(
        open,
        load,
        engine_tx,
        Arc::clone(&hub),
        Arc::clone(&status),
        (c.width, c.height),
    );
    let watch_status = Arc::clone(&status);
    let coordinator = tokio::spawn(coordinate(
        done_rx,
        Arc::clone(&hub),
        Arc::clone(&status),
        Intrinsics::from_fov(c.width, c.height, 80.0, 55.0),
    ));
    let worker = spawn_detector(frame_rx, &session, shared, done_tx);
    server::run(
        session,
        cam,
        c.exposure,
        c.gain,
        calib,
        Some(tap),
        hub,
        model.into(),
        bind,
    )
    .await?;
    let pending = || {
        watch_status
            .lock()
            .map(|s| format!("{} queued frames, detector {}", s.lag, s.state))
            .unwrap_or_default()
    };
    println!("waiting for the detector to finish: {}", pending());
    let result = match tokio::time::timeout(Duration::from_secs(30), coordinator).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(format!("coordinator failed: {e}")),
        Err(_) => Err(format!(
            "gave up after 30 s waiting for the detector: {}",
            pending()
        )),
    };
    let _ = tokio::task::spawn_blocking(move || worker.join()).await;
    result
}

/// How to open the camera, and the stereo geometry to assume until it is
/// open: replayed clips from `from`, else the OAK.
fn camera(
    session: &DataDir,
    from: Option<PathBuf>,
    gap: f64,
) -> Result<(Open, StereoCalib), String> {
    let saved = |session: &DataDir| {
        let json = std::fs::read_to_string(session.dir.join("calibration.json")).ok();
        crate::capture::stereo_calib(session, json.as_deref())
    };
    match from {
        Some(source) => {
            let source = DataDir::open(&source).map_err(|e| e.to_string())?;
            let calibration = source.dir.join("calibration.json");
            if calibration.exists() && !session.dir.join("calibration.json").exists() {
                std::fs::copy(&calibration, session.dir.join("calibration.json"))
                    .map_err(|e| e.to_string())?;
            }
            let calib = saved(session);
            let gap = Duration::from_secs_f64(gap);
            let open: Open = Box::new(move || {
                let cam = crate::replay::ClipCamera::open(&source, gap)?;
                Ok((Box::new(cam) as Box<dyn device::Camera + Send>, calib))
            });
            Ok((open, calib))
        }
        None => {
            #[cfg(not(feature = "oak"))]
            {
                let _ = saved;
                Err("rebuild with --features oak, or pass --from <session>".into())
            }
            #[cfg(feature = "oak")]
            {
                let session = session.clone();
                let calib = saved(&session);
                let open: Open = Box::new(move || {
                    let (cam, calib) = crate::capture::open_oak(&session)?;
                    Ok((Box::new(cam) as Box<dyn device::Camera + Send>, calib))
                });
                Ok((open, calib))
            }
        }
    }
}

fn model_info(model: &Path, sha1: &str) -> tracker::ModelInfo {
    tracker::ModelInfo {
        file: model.file_name().map_or_else(
            || model.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        sha1: sha1.to_string(),
    }
}

/// Load a promoted model with its engine built, then send it to the
/// detector thread.
fn loader(
    engine: mpsc::Sender<Engine>,
    models: Arc<Models>,
    hub: Arc<Hub>,
) -> crate::train::Loader {
    Arc::new(move |model: &Path| {
        let (detector, info) = models.load(model)?;
        engine
            .send(Engine::Swap(detector))
            .map_err(|_| "the detector has stopped".to_string())?;
        hub.set_model(info);
        Ok(())
    })
}

fn spawn_detector(
    rx: Receiver<Arc<StoredFrame>>,
    session: &DataDir,
    shared: Shared,
    done: UnboundedSender<Finished>,
) -> std::thread::JoinHandle<()> {
    let session = session.clone();
    std::thread::spawn(move || {
        if let Err(e) = detect_loop(rx, session, shared, done) {
            eprintln!("detector stopped: {e}");
        }
    })
}

/// Every frame: detect, feed the watcher, keep the pre-roll ring, and write
/// the open episode's frames to its clip as they arrive. Frames are counted
/// and dropped while capture is gated off or there is no detector.
fn detect_loop(
    rx: Receiver<Arc<StoredFrame>>,
    session: DataDir,
    shared: Shared,
    done: UnboundedSender<Finished>,
) -> Result<(), String> {
    let Shared {
        status,
        sent,
        stop,
        hub,
        engine: control,
    } = shared;
    let cfg: WatchConfig = session.config.watch.clone();
    let c = session.config.capture.clone();
    let intr = Intrinsics::from_fov(c.width, c.height, 80.0, 55.0);
    let mut engine: Option<Running> = None;
    // The stereo geometry while the camera is up, so a model promoted
    // before any exists can start detecting at once.
    let mut up: Option<StereoCalib> = None;
    let mut watcher = Watcher::new(cfg.clone());
    let mut ring: VecDeque<(usize, Arc<StoredFrame>)> = VecDeque::new();
    let cage = session.config.cage.clone();
    let mut writer: Option<ClipWriter> = None;
    let mut target: Option<Target> = None;
    let mut index = 0usize;
    let mut taken = 0u64;
    let mut meter = (Instant::now(), 0u32);
    let mut last_status = Instant::now();
    let mut stages = Stages::new("detector");
    let set_status = |change: &dyn Fn(&mut LiveStatus)| {
        if let Ok(mut s) = status.lock() {
            change(&mut s);
        }
    };
    let mut report =
        |watcher: &Watcher, balls: Vec<LiveBall>, taken: u64, meter: &mut (Instant, u32)| {
            if last_status.elapsed() < Duration::from_millis(200) {
                return;
            }
            let dt = meter.0.elapsed().as_secs_f64();
            set_status(&|s| {
                s.rate = meter.1 as f64 / dt.max(1e-3);
                s.lag = sent.load(Ordering::Relaxed).saturating_sub(taken);
                s.state = match watcher.state() {
                    WatchState::Idle => "idle".into(),
                    WatchState::Episode => "episode".into(),
                };
                s.balls = balls.clone();
            });
            if dt >= 1.0 {
                *meter = (Instant::now(), 0);
            }
            last_status = Instant::now();
        };
    loop {
        let frame = match rx.recv_timeout(ENGINE_POLL) {
            Ok(frame) => Some(frame),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        while let Ok(change) = control.try_recv() {
            match change {
                Engine::Start { detector, calib } => {
                    up = Some(calib);
                    engine = detector.map(|detector| Running {
                        detector,
                        calib,
                        rectifier: session.rectifier(c.width, c.height),
                    });
                    watcher = Watcher::new(cfg.clone());
                    meter = (Instant::now(), 0);
                    let state = if engine.is_some() {
                        "idle"
                    } else {
                        "no detector"
                    };
                    set_status(&|s| s.state = state.into());
                }
                Engine::Swap(next) => match (engine.as_mut(), &up) {
                    (Some(running), _) => {
                        running.detector = next;
                        println!("detector: swapped in the promoted model");
                    }
                    (None, Some(calib)) => {
                        engine = Some(Running {
                            detector: next,
                            calib: *calib,
                            rectifier: session.rectifier(c.width, c.height),
                        });
                        meter = (Instant::now(), 0);
                        set_status(&|s| s.state = "idle".into());
                        println!("detector: the promoted model is live");
                    }
                    (None, None) => {
                        println!("detector: the promoted model is ready for the next start")
                    }
                },
                Engine::Release => {
                    up = None;
                    if let Some(episode) = gate_closed(&mut watcher) {
                        let target = target.take().unwrap_or_else(|| Target::now(&hub));
                        finish_episode(target, writer.take(), episode, &c, &done);
                    }
                    ring.clear();
                    engine = None;
                    set_status(&|s| {
                        *s = LiveStatus {
                            state: "standby".into(),
                            provider: s.provider.clone(),
                            clips: s.clips,
                            last_event: s.last_event.clone(),
                            ..LiveStatus::default()
                        }
                    });
                }
            }
        }
        let Some(frame) = frame else { continue };
        taken += 1;
        if stop.load(Ordering::Relaxed) && watcher.state() == WatchState::Idle {
            let skipped = sent.load(Ordering::Relaxed).saturating_sub(taken) + 1;
            println!("stopping with no episode open; skipping {skipped} queued frames");
            break;
        }
        stages.begin();
        let Some(running) = engine.as_mut().filter(|_| hub.capturing()) else {
            if let Some(episode) = gate_closed(&mut watcher) {
                let target = target.take().unwrap_or_else(|| Target::now(&hub));
                finish_episode(target, writer.take(), episode, &c, &done);
            }
            ring.clear();
            index += 1;
            meter.1 += 1;
            if engine.is_some() {
                report(&watcher, Vec::new(), taken, &mut meter);
            }
            continue;
        };
        let balls = match running.detector.detect(&FrameView {
            width: frame.width,
            height: frame.height,
            left: &frame.left,
            right: (!frame.right.is_empty()).then_some(frame.right.as_slice()),
            depth: (!frame.depth_mm.is_empty()).then_some(frame.depth_mm.as_slice()),
            rectifier: running.rectifier.as_ref(),
            calib: running.calib,
        }) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("detect: {e}");
                Vec::new()
            }
        };
        stages.mark("detect");
        let calib = running.calib;
        let mount = hub.mount();
        let live: Vec<LiveBall> = balls
            .iter()
            .map(|b| LiveBall {
                rect: [b.x, b.y, b.w, b.h],
                position: ball_position(b, &intr, &mount),
            })
            .collect();
        let before = ring
            .iter()
            .rev()
            .find(|(i, _)| *i + CHANGE_FRAMES <= index)
            .map(|(_, f)| f);
        let can_move: Vec<bool> = balls
            .iter()
            .map(|b| {
                !tracker::outside_cage(b, &intr, &mount, &cage)
                    && before.is_none_or(|f| {
                        box_changed(&frame.left, &f.left, frame.width, frame.height, b)
                    })
            })
            .collect();
        let was_open = watcher.state() == WatchState::Episode;
        let episode = watcher.observe_gated(index, frame.t_ns, balls, &can_move);
        stages.mark("watch");
        if !was_open && watcher.state() == WatchState::Episode {
            target = Some(Target::now(&hub));
            stages.mark("open episode");
        }
        ring.push_back((index, Arc::clone(&frame)));
        let cutoff = frame.t_ns.saturating_sub(watcher.keep_ns());
        while ring.front().is_some_and(|(_, f)| f.t_ns < cutoff) {
            ring.pop_front();
        }
        if target.is_some() && cfg.keep_clips {
            let result = if was_open {
                writer.as_mut().map_or(Ok(()), |w| w.write(&frame))
            } else if let Some(t) = target
                .as_ref()
                .filter(|_| watcher.state() == WatchState::Episode)
            {
                let start = watcher.window_start().unwrap_or(index);
                t.dir
                    .begin_clip(c.exposure, c.gain, 0.0, 0.0, calib)
                    .and_then(|w| {
                        let mut w = w.without_depth();
                        for (_, f) in ring.iter().filter(|(i, _)| *i >= start) {
                            w.write(f)?;
                        }
                        writer = Some(w);
                        Ok(())
                    })
            } else {
                Ok(())
            };
            if let Err(e) = result {
                eprintln!("clip write failed: {e}");
                writer = None;
            }
            stages.mark(if was_open { "clip" } else { "clip start" });
        }
        if let Some(episode) = episode {
            let target = target.take().unwrap_or_else(|| Target::now(&hub));
            finish_episode(target, writer.take(), episode, &c, &done);
            stages.mark("finish episode");
        }
        index += 1;
        meter.1 += 1;
        report(&watcher, live, taken, &mut meter);
        stages.mark("status");
        let lag = sent.load(Ordering::Relaxed).saturating_sub(taken);
        stages.check(format_args!("lag {lag} frames"));
    }
    if let Some(episode) = watcher.flush() {
        let target = target.take().unwrap_or_else(|| Target::now(&hub));
        finish_episode(target, writer.take(), episode, &c, &done);
    }
    Ok(())
}

/// When capture is gated off, an open episode ends where it is.
fn gate_closed(watcher: &mut Watcher) -> Option<Episode> {
    (watcher.state() == WatchState::Episode)
        .then(|| watcher.flush())
        .flatten()
}

/// Close the clip for a finished window and hand it to the coordinator.
fn finish_episode(
    target: Target,
    writer: Option<ClipWriter>,
    episode: Episode,
    c: &tracker::CaptureConfig,
    done: &UnboundedSender<Finished>,
) {
    let id = match writer {
        Some(w) => w.finish(),
        None => target.dir.next_clip_id().and_then(|id| {
            let meta = ClipMeta {
                width: c.width,
                height: c.height,
                fps: c.fps,
                frame_count: episode.frames.len(),
                exposure_us: c.exposure,
                gain: c.gain,
                sequence_gaps: false,
                ir_flood: 0.0,
                ir_dot: 0.0,
            };
            target.dir.write_clip_meta(&id, &meta)?;
            Ok(id)
        }),
    };
    match id {
        Ok(id) => {
            let _ = done.send(Finished {
                target,
                id,
                episode,
            });
        }
        Err(e) => eprintln!("episode lost: {e}"),
    }
}

/// Measure each finished window, record its events under its session, and
/// broadcast them. Events with no session are broadcast but not recorded.
async fn coordinate(
    mut rx: UnboundedReceiver<Finished>,
    hub: Arc<Hub>,
    status: Arc<Mutex<LiveStatus>>,
    intr: Intrinsics,
) -> Result<(), String> {
    while let Some(Finished {
        target,
        id,
        episode,
    }) = rx.recv().await
    {
        let dir = target.dir;
        let frames = episode.as_clip();
        dir.save_detections(
            &id,
            &Detections {
                frames: frames.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
        let cfg = hub.config();
        let records = process_clip(&id, &frames, &cfg, &intr, target.activity);
        let summary = if records.is_empty() {
            format!("{id}: ball moved, no event")
        } else {
            records
                .iter()
                .map(|r| {
                    format!(
                        "{id}: {} {:.1} mph launch {:.1}",
                        r.kind.as_str(),
                        r.exit_velocity,
                        r.launch_angle
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        println!("{summary}");
        for r in &records {
            print_hit(r);
        }
        hub.publish(&dir, target.session, &id, records)?;
        if let Ok(mut s) = status.lock() {
            s.clips += 1;
            s.last_event = summary;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker::BallBox;

    fn ball(x: f64) -> BallBox {
        BallBox {
            x,
            y: 200.0,
            w: 18.0,
            h: 18.0,
            conf: 0.9,
            depth_m: Some(1.6),
        }
    }

    #[test]
    fn closing_the_gate_flushes_one_open_episode() {
        let mut watcher = Watcher::new(WatchConfig::default());
        assert!(gate_closed(&mut watcher).is_none());
        for i in 0..40 {
            watcher.observe(
                i,
                i as u64 * 10_000_000,
                vec![ball(100.0 + 40.0 * i as f64)],
            );
        }
        assert_eq!(watcher.state(), WatchState::Episode);
        assert!(gate_closed(&mut watcher).is_some());
        assert_eq!(watcher.state(), WatchState::Idle);
        assert!(gate_closed(&mut watcher).is_none());
    }
}
