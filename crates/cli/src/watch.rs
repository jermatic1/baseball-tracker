//! A live session: the capture server plus a detector that sees every
//! frame, a watcher that cuts the windows where a ball moved, and a
//! coordinator that measures each window, posts hits, and saves it as a clip.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracker::{
    merge_clip_hits, process_clip, ClipMeta, ClipWriter, Detections, Episode, Intrinsics, Session,
    StereoCalib, StoredFrame, WatchConfig, WatchState, Watcher,
};

use crate::capture::server::{self, LiveStatus, LiveTap};
use crate::convert::print_hit;
use crate::detector::{Detector, FrameView};
use crate::live::post_hits;

/// Frames queued for the detector before the newest are dropped: about
/// three seconds, enough for the TensorRT engine build on a first run.
const QUEUE: usize = 300;

struct Finished {
    id: String,
    episode: Episode,
}

pub async fn watch(path: PathBuf, model: String, from: Option<PathBuf>) -> Result<(), String> {
    let session = Session::create(&path).map_err(|e| e.to_string())?;
    let detector = Detector::load(&model)?;
    let status = Arc::new(Mutex::new(LiveStatus {
        provider: detector.provider().to_string(),
        state: "idle".into(),
        ..LiveStatus::default()
    }));
    let (frame_tx, frame_rx) = mpsc::sync_channel(QUEUE);
    let (done_tx, done_rx) = unbounded_channel();
    let tap = LiveTap {
        frames: frame_tx,
        sent: Arc::new(AtomicU64::new(0)),
        drops: Arc::new(AtomicU64::new(0)),
        status: Arc::clone(&status),
    };
    let sent = Arc::clone(&tap.sent);
    let c = session.config.capture.clone();
    let coordinator = tokio::spawn(coordinate(
        done_rx,
        session.clone(),
        Arc::clone(&status),
        Intrinsics::from_fov(c.width, c.height, 80.0, 55.0),
    ));
    match from {
        Some(source) => {
            let source = Session::open(&source).map_err(|e| e.to_string())?;
            let calibration = source.dir.join("calibration.json");
            if calibration.exists() && !session.dir.join("calibration.json").exists() {
                std::fs::copy(&calibration, session.dir.join("calibration.json"))
                    .map_err(|e| e.to_string())?;
            }
            let device_json = std::fs::read_to_string(session.dir.join("calibration.json")).ok();
            let calib = crate::capture::stereo_calib(&session, device_json.as_deref());
            let cam = crate::replay::ClipCamera::open(&source)?;
            spawn_detector(frame_rx, detector, &session, calib, status, sent, done_tx);
            server::run(session, cam, c.exposure_us, c.gain, calib, Some(tap)).await?;
        }
        None => {
            #[cfg(not(feature = "oak"))]
            {
                let _ = (frame_rx, done_tx, sent);
                return Err("rebuild with --features oak, or pass --from <session>".into());
            }
            #[cfg(feature = "oak")]
            {
                let (cam, calib) = crate::capture::open_oak(&session)?;
                spawn_detector(frame_rx, detector, &session, calib, status, sent, done_tx);
                server::run(session, cam, c.exposure_us, c.gain, calib, Some(tap)).await?;
            }
        }
    }
    match tokio::time::timeout(Duration::from_secs(30), coordinator).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(format!("coordinator failed: {e}")),
        Err(_) => Err("timed out finishing the last episode".into()),
    }
}

fn spawn_detector(
    rx: Receiver<Arc<StoredFrame>>,
    detector: Detector,
    session: &Session,
    calib: StereoCalib,
    status: Arc<Mutex<LiveStatus>>,
    sent: Arc<AtomicU64>,
    done: UnboundedSender<Finished>,
) {
    let session = session.clone();
    std::thread::spawn(move || {
        if let Err(e) = detect_loop(rx, detector, session, calib, status, sent, done) {
            eprintln!("detector stopped: {e}");
        }
    });
}

/// Every frame: detect, feed the watcher, keep the pre-roll ring, and write
/// the open episode's frames to its clip as they arrive.
fn detect_loop(
    rx: Receiver<Arc<StoredFrame>>,
    mut detector: Detector,
    session: Session,
    calib: StereoCalib,
    status: Arc<Mutex<LiveStatus>>,
    sent: Arc<AtomicU64>,
    done: UnboundedSender<Finished>,
) -> Result<(), String> {
    let cfg: WatchConfig = session.config.watch.clone();
    let c = session.config.capture.clone();
    let rectifier = session.rectifier(c.width, c.height);
    let mut watcher = Watcher::new(cfg.clone());
    let mut ring: VecDeque<(usize, Arc<StoredFrame>)> = VecDeque::new();
    let preroll_ns = (cfg.preroll_s * 1e9) as u64;
    let mut writer: Option<ClipWriter> = None;
    let mut index = 0usize;
    let mut taken = 0u64;
    let mut meter = (Instant::now(), 0u32);
    let mut last_status = Instant::now();
    while let Ok(frame) = rx.recv() {
        taken += 1;
        let balls = match detector.detect(&FrameView {
            width: frame.width,
            height: frame.height,
            left: &frame.left,
            right: (!frame.right.is_empty()).then_some(frame.right.as_slice()),
            depth: (!frame.depth_mm.is_empty()).then_some(frame.depth_mm.as_slice()),
            rectifier: rectifier.as_ref(),
            calib,
        }) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("detect: {e}");
                Vec::new()
            }
        };
        let boxes: Vec<[f64; 4]> = balls.iter().map(|b| [b.x, b.y, b.w, b.h]).collect();
        let was_open = watcher.state() == WatchState::Episode;
        let episode = watcher.observe(index, frame.t_ns, balls);
        ring.push_back((index, Arc::clone(&frame)));
        let cutoff = frame.t_ns.saturating_sub(preroll_ns);
        while ring.front().is_some_and(|(_, f)| f.t_ns < cutoff) {
            ring.pop_front();
        }
        if cfg.keep_clips {
            let result = if was_open {
                writer.as_mut().map_or(Ok(()), |w| w.write(&frame))
            } else if watcher.state() == WatchState::Episode {
                let start = watcher.window_start().unwrap_or(index);
                session
                    .begin_clip(c.exposure_us, c.gain, 0.0, 0.0, calib)
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
        }
        if let Some(episode) = episode {
            finish_episode(&session, writer.take(), episode, &c, &done);
        }
        index += 1;
        meter.1 += 1;
        if last_status.elapsed() >= Duration::from_millis(200) {
            let dt = meter.0.elapsed().as_secs_f64();
            if let Ok(mut s) = status.lock() {
                s.fps = meter.1 as f64 / dt.max(1e-3);
                s.lag = sent.load(Ordering::Relaxed).saturating_sub(taken);
                s.state = match watcher.state() {
                    WatchState::Idle => "idle".into(),
                    WatchState::Episode => "episode".into(),
                };
                s.boxes = boxes;
            }
            if dt >= 1.0 {
                meter = (Instant::now(), 0);
            }
            last_status = Instant::now();
        }
    }
    if let Some(episode) = watcher.flush() {
        finish_episode(&session, writer.take(), episode, &c, &done);
    }
    Ok(())
}

/// Close the clip for a finished window and hand it to the coordinator.
fn finish_episode(
    session: &Session,
    writer: Option<ClipWriter>,
    episode: Episode,
    c: &tracker::CaptureConfig,
    done: &UnboundedSender<Finished>,
) {
    let id = match writer {
        Some(w) => w.finish(),
        None => session.next_clip_id().and_then(|id| {
            let meta = ClipMeta {
                width: c.width,
                height: c.height,
                fps: c.fps,
                frame_count: episode.frames.len(),
                exposure_us: c.exposure_us,
                gain: c.gain,
                sequence_gaps: false,
                ir_flood: 0.0,
                ir_dot: 0.0,
            };
            session.write_clip_meta(&id, &meta)?;
            Ok(id)
        }),
    };
    match id {
        Ok(id) => {
            let _ = done.send(Finished { id, episode });
        }
        Err(e) => eprintln!("episode lost: {e}"),
    }
}

/// Measure each finished window, record and post its events.
async fn coordinate(
    mut rx: UnboundedReceiver<Finished>,
    session: Session,
    status: Arc<Mutex<LiveStatus>>,
    intr: Intrinsics,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    while let Some(Finished { id, episode }) = rx.recv().await {
        let frames = episode.as_clip();
        session
            .save_detections(
                &id,
                &Detections {
                    frames: frames.clone(),
                },
            )
            .map_err(|e| e.to_string())?;
        let records = process_clip(&id, &frames, &session.config, &intr);
        let summary = if records.is_empty() {
            format!("{id}: ball moved, no event")
        } else {
            records
                .iter()
                .map(|r| {
                    format!(
                        "{id}: {} {:.1} mph launch {:.1}",
                        r.kind.as_str(),
                        r.exit_velocity_mph,
                        r.launch_angle_deg
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        println!("{summary}");
        for r in &records {
            print_hit(r);
        }
        let existing = session.hits().map_err(|e| e.to_string())?;
        let mut hits = merge_clip_hits(existing, &id, records);
        session.write_hits(&hits).map_err(|e| e.to_string())?;
        post_hits(&session, &mut hits, &client).await?;
        if let Ok(mut s) = status.lock() {
            s.clips += 1;
            s.last_event = summary;
        }
    }
    Ok(())
}
