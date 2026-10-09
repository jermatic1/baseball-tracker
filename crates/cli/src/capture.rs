use std::path::PathBuf;
use std::time::Duration;

use device::Camera;

use crate::convert::to_stored;

pub async fn capture(path: PathBuf, demo: bool, once: bool) -> Result<(), String> {
    if demo {
        return capture_demo(path);
    }
    if once {
        return capture_once(path);
    }
    capture_oak(path).await
}

fn capture_demo(path: PathBuf) -> Result<(), String> {
    let session = tracker::Session::create(&path).map_err(|e| e.to_string())?;
    let width = session.config.capture.width;
    let height = session.config.capture.height;
    let fps = session.config.capture.fps as f32;
    let (w, h) = if width <= 640 && height <= 400 {
        (width, height)
    } else {
        (320, 240)
    };
    let n = (fps.max(1.0) * 2.0).round().clamp(1.0, 60.0) as usize;
    let mut cam = device::SyntheticCamera::new(w, h, fps.max(1.0));
    let mut frames = Vec::with_capacity(n);
    for _ in 0..n {
        if let Some(frame) = cam
            .poll(Duration::from_millis(0))
            .map_err(|e| e.to_string())?
        {
            frames.push(to_stored(frame));
        }
    }
    let exposure_us = session.config.capture.exposure_us;
    let gain = session.config.capture.gain;
    let id = session
        .save_clip(&frames, exposure_us, gain)
        .map_err(|e| e.to_string())?;
    println!("{id}");
    Ok(())
}

#[cfg(not(feature = "oak"))]
fn oak_required() -> Result<(), String> {
    Err("rebuild with --features oak".into())
}

#[cfg(not(feature = "oak"))]
fn capture_once(_path: PathBuf) -> Result<(), String> {
    oak_required()
}

#[cfg(not(feature = "oak"))]
async fn capture_oak(_path: PathBuf) -> Result<(), String> {
    oak_required()
}

#[cfg(feature = "oak")]
fn capture_once(path: PathBuf) -> Result<(), String> {
    let session = tracker::Session::create(&path).map_err(|e| e.to_string())?;
    let width = session.config.capture.width;
    let height = session.config.capture.height;
    let fps = session.config.capture.fps as f32;
    let mut cam = device::OakCamera::open(
        width,
        height,
        fps.max(1.0),
        session.config.capture.exposure_us,
        session.config.capture.gain,
    )?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Some(frame) = cam
            .poll(Duration::from_millis(0))
            .map_err(|e| e.to_string())?
        {
            let jpeg = crate::jpeg::encode_gray_jpeg(frame.width, frame.height, &frame.left)?;
            let out = session.dir.join("camera.jpg");
            std::fs::write(&out, jpeg).map_err(|e| e.to_string())?;
            println!("{}", out.display());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("no frame from camera".into())
}

#[cfg(feature = "oak")]
async fn capture_oak(path: PathBuf) -> Result<(), String> {
    let session = tracker::Session::create(&path).map_err(|e| e.to_string())?;
    let exposure_us = session.config.capture.exposure_us;
    let gain = session.config.capture.gain;
    let (cam, calib) = open_oak(&session)?;
    server::run(session, cam, exposure_us, gain, calib, None).await
}

/// Open the OAK at the session's capture settings and save its calibration
/// into the session.
#[cfg(feature = "oak")]
pub(crate) fn open_oak(
    session: &tracker::Session,
) -> Result<(device::OakCamera, tracker::StereoCalib), String> {
    let c = &session.config.capture;
    let cam = device::OakCamera::open(
        c.width,
        c.height,
        (c.fps as f32).max(1.0),
        c.exposure_us,
        c.gain,
    )?;
    let device_calib = cam.calibration_json();
    if let Some(json) = &device_calib {
        let _ = std::fs::write(session.dir.join("calibration.json"), json);
    }
    let calib = stereo_calib(session, device_calib.as_deref());
    Ok((cam, calib))
}

/// Stereo geometry for point depth: the device calibration when the session
/// has one, else the nominal rig with the configured disparity offset.
pub(crate) fn stereo_calib(
    session: &tracker::Session,
    device_json: Option<&str>,
) -> tracker::StereoCalib {
    let c = &session.config.capture;
    // Rectified frames have no offset by construction; raw frames get the
    // hand-calibrated one from the config.
    let calib = match session.rectifier(c.width, c.height) {
        Some(r) => {
            println!("stereo rectification from device calibration");
            r.calib()
        }
        None => {
            println!("no device calibration; using the disparity offset from config");
            tracker::stereo_calib_from_device(device_json, c.width)
                .with_offset(session.config.stereo.disparity_offset_px)
        }
    };
    println!(
        "stereo fx {:.1} baseline {:.3} m, disparity offset {:.1} px",
        calib.fx, calib.baseline_m, calib.disparity_offset_px
    );
    calib
}

pub(crate) mod server {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use axum::extract::State;
    use axum::http::{header, StatusCode};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use device::Camera;
    use serde::Deserialize;

    use crate::convert::to_stored;
    use crate::error::AppError;
    use crate::jpeg::encode_gray_jpeg;

    const BIND: &str = "0.0.0.0:7880";
    const RECORD_QUEUE: usize = 32;

    /// What a live watcher plugs into the capture loop: every frame goes
    /// down `frames` in order, and the page reads `status`.
    pub struct LiveTap {
        pub frames: SyncSender<Arc<tracker::StoredFrame>>,
        pub sent: Arc<AtomicU64>,
        pub drops: Arc<AtomicU64>,
        pub status: Arc<Mutex<LiveStatus>>,
        /// Set when the server is shutting down so the detector can skip
        /// its backlog if no episode is open.
        pub stop: Arc<AtomicBool>,
    }

    #[derive(Debug, Clone, Default, serde::Serialize)]
    pub struct LiveStatus {
        pub state: String,
        pub provider: String,
        pub fps: f64,
        pub lag: u64,
        pub clips: u64,
        pub last_event: String,
        pub boxes: Vec<[f64; 4]>,
    }

    struct CaptureState {
        latest: Option<Arc<tracker::StoredFrame>>,
        jpeg: Option<(Instant, Vec<u8>)>,
        stats: Option<serde_json::Value>,
        exposure_us: u32,
        gain: u32,
        ir_flood: f32,
        ir_dot: f32,
        calib: tracker::StereoCalib,
        session: tracker::Session,
        record_tx: Option<SyncSender<Option<Arc<tracker::StoredFrame>>>>,
        record_drops: Arc<AtomicU64>,
        record_done: Option<Receiver<Result<String, String>>>,
        live: Option<LiveTap>,
        stop: Arc<AtomicBool>,
    }

    pub async fn run<C: Camera + Send + 'static>(
        session: tracker::Session,
        cam: C,
        exposure_us: u32,
        gain: u32,
        calib: tracker::StereoCalib,
        live: Option<LiveTap>,
    ) -> Result<(), String> {
        let stopping = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(CaptureState {
            latest: None,
            jpeg: None,
            stats: None,
            exposure_us,
            gain,
            ir_flood: 0.0,
            ir_dot: 0.0,
            calib,
            session,
            record_tx: None,
            record_drops: Arc::new(AtomicU64::new(0)),
            record_done: None,
            live,
            stop: Arc::clone(&stopping),
        }));
        let poll_state = Arc::clone(&state);
        std::thread::spawn(move || poll_loop(cam, poll_state));
        let app = Router::new()
            .route("/", get(index))
            .route("/preview.jpg", get(preview))
            .route("/api/stats", get(stats))
            .route("/api/calibrate", post(calibrate))
            .route("/api/exposure", post(set_exposure))
            .route("/api/gain", post(set_gain))
            .route("/api/flood", post(set_flood))
            .route("/api/dot", post(set_dot))
            .route("/api/fps", post(set_fps))
            .route("/api/record", post(record))
            .route("/api/stop", post(stop))
            .with_state(Arc::clone(&state));
        let addr: SocketAddr = BIND
            .parse()
            .map_err(|e: std::net::AddrParseError| e.to_string())?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| e.to_string())?;
        println!("http://{BIND}");
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                println!("stopping");
            })
            .await
            .map_err(|e| e.to_string())?;
        stopping.store(true, Ordering::Relaxed);
        // Finish a manual clip and hand the live watcher its last frame.
        let (done, _tap) = {
            let mut st = state.lock().map_err(|e| e.to_string())?;
            st.record_tx.take();
            if let Some(tap) = &st.live {
                tap.stop.store(true, Ordering::Relaxed);
            }
            (st.record_done.take(), st.live.take())
        };
        if let Some(done) = done {
            let _ = tokio::task::spawn_blocking(move || done.recv()).await;
        }
        Ok(())
    }

    fn poll_loop<C: Camera>(mut cam: C, state: Arc<Mutex<CaptureState>>) {
        let (record_drops, stop, live) = match state.lock() {
            Ok(st) => (
                Arc::clone(&st.record_drops),
                Arc::clone(&st.stop),
                st.live.as_ref().map(|tap| {
                    (
                        tap.frames.clone(),
                        Arc::clone(&tap.sent),
                        Arc::clone(&tap.drops),
                    )
                }),
            ),
            Err(_) => return,
        };
        while !stop.load(Ordering::Relaxed) {
            if let Some(stats) = cam.take_stats() {
                let drops = record_drops.swap(0, Ordering::Relaxed);
                if let Ok(mut st) = state.lock() {
                    st.stats = Some(stats_json(stats, drops));
                }
            }
            let (exposure_us, gain, ir_flood, ir_dot) = {
                let st = match state.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                (st.exposure_us, st.gain, st.ir_flood, st.ir_dot)
            };
            if let Err(e) = cam.apply_controls(exposure_us, gain) {
                eprintln!("{e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            if let Err(e) = cam.apply_lights(ir_flood, ir_dot) {
                eprintln!("{e}");
            }
            match cam.poll(Duration::from_millis(0)) {
                Ok(Some(frame)) => {
                    let stored = Arc::new(to_stored(frame));
                    let mut st = match state.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    let tx = st.record_tx.clone();
                    st.latest = Some(Arc::clone(&stored));
                    drop(st);
                    if let Some(tx) = tx {
                        if tx.try_send(Some(Arc::clone(&stored))).is_err() {
                            record_drops.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if let Some((tx, sent, drops)) = &live {
                        match tx.try_send(stored) {
                            Ok(()) => sent.fetch_add(1, Ordering::Relaxed),
                            Err(_) => drops.fetch_add(1, Ordering::Relaxed),
                        };
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => {
                    eprintln!("{e}");
                    if e.starts_with("frame size") {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    async fn index() -> Html<&'static str> {
        Html(include_str!("capture.html"))
    }

    async fn stats(
        State(state): State<Arc<Mutex<CaptureState>>>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let st = state.lock().map_err(|e| e.to_string())?;
        let mut stats = st.stats.clone().unwrap_or_else(|| serde_json::json!({}));
        if let Some(tap) = &st.live {
            let mut live = tap
                .status
                .lock()
                .map(|s| serde_json::to_value(&*s).unwrap_or_default())
                .unwrap_or_default();
            live["drops"] = tap.drops.load(Ordering::Relaxed).into();
            stats["live"] = live;
        }
        Ok(Json(stats))
    }

    /// Solve the mount from the plate in the latest frame and save it.
    async fn calibrate(
        State(state): State<Arc<Mutex<CaptureState>>>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        let frame = st
            .latest
            .clone()
            .ok_or_else(|| "no frame yet".to_string())?;
        let Some((corners, fit)) = tracker::plate::calibrate_from_frame(
            &frame.left,
            frame.width,
            frame.height,
            &st.session.config,
        ) else {
            return Err("plate not found: clear the plate and keep the ball off it"
                .to_string()
                .into());
        };
        st.session.config.mount = fit.mount.clone();
        st.session
            .config
            .save(st.session.dir.join("config.toml"))
            .map_err(|e| e.to_string())?;
        crate::calibrate::print_fit(&fit);
        Ok(Json(serde_json::json!({
            "corners": corners.px,
            "reprojected": fit.reprojected,
            "rms_px": fit.rms_px,
            "mount": fit.mount,
        })))
    }

    fn stats_json(s: device::CaptureStats, record_drops: u64) -> serde_json::Value {
        let per_s = |n: u64| (n as f64 / s.seconds.max(1e-3)).round();
        serde_json::json!({
            "left_fps": per_s(s.left),
            "right_fps": per_s(s.right),
            "pair_fps": per_s(s.pairs),
            "left_gaps": s.left_gaps,
            "right_gaps": s.right_gaps,
            "unpaired": s.unpaired,
            "record_drops": record_drops,
        })
    }

    async fn preview(State(state): State<Arc<Mutex<CaptureState>>>) -> Result<Response, AppError> {
        let cached = {
            let st = state.lock().map_err(|e| e.to_string())?;
            st.jpeg.as_ref().and_then(|(t, bytes)| {
                (t.elapsed() < Duration::from_millis(100)).then(|| bytes.clone())
            })
        };
        if let Some(bytes) = cached {
            return Ok(jpeg_response(bytes));
        }
        let frame = {
            let st = state.lock().map_err(|e| e.to_string())?;
            st.latest
                .as_ref()
                .map(|frame| (frame.width, frame.height, Arc::clone(frame)))
        };
        let Some((width, height, frame)) = frame else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        };
        let bytes = encode_gray_jpeg(width, height, &frame.left)?;
        if let Ok(mut st) = state.lock() {
            st.jpeg = Some((Instant::now(), bytes.clone()));
        }
        Ok(jpeg_response(bytes))
    }

    fn jpeg_response(bytes: Vec<u8>) -> Response {
        (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response()
    }

    #[derive(Deserialize)]
    struct DeltaBody {
        delta: i32,
    }

    #[derive(Deserialize)]
    struct FpsBody {
        fps: f32,
    }

    async fn set_exposure(
        State(state): State<Arc<Mutex<CaptureState>>>,
        Json(body): Json<DeltaBody>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        let next = (st.exposure_us as i64 + body.delta as i64).clamp(1, 1_000_000) as u32;
        st.exposure_us = next;
        Ok(Json(serde_json::json!({ "exposure_us": next })))
    }

    async fn set_gain(
        State(state): State<Arc<Mutex<CaptureState>>>,
        Json(body): Json<DeltaBody>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        let next = (st.gain as i64 + body.delta as i64).clamp(100, 1600) as u32;
        st.gain = next;
        Ok(Json(serde_json::json!({ "gain": next })))
    }

    async fn set_flood(
        State(state): State<Arc<Mutex<CaptureState>>>,
        Json(body): Json<DeltaBody>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        st.ir_flood = bump_light(st.ir_flood, body.delta);
        Ok(Json(serde_json::json!({ "ir_flood": st.ir_flood })))
    }

    async fn set_dot(
        State(state): State<Arc<Mutex<CaptureState>>>,
        Json(body): Json<DeltaBody>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        st.ir_dot = bump_light(st.ir_dot, body.delta);
        Ok(Json(serde_json::json!({ "ir_dot": st.ir_dot })))
    }

    fn bump_light(current: f32, delta_percent: i32) -> f32 {
        let next = (current * 100.0).round() as i32 + delta_percent;
        next.clamp(0, 100) as f32 / 100.0
    }

    async fn set_fps(
        State(state): State<Arc<Mutex<CaptureState>>>,
        Json(body): Json<FpsBody>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let _st = state.lock().map_err(|e| e.to_string())?;
        let _ = body.fps.max(1.0);
        Err("fps is fixed at open".to_string().into())
    }

    async fn record(
        State(state): State<Arc<Mutex<CaptureState>>>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        if st.record_tx.is_some() {
            return Err("already recording".to_string().into());
        }
        let writer = st
            .session
            .begin_clip(st.exposure_us, st.gain, st.ir_flood, st.ir_dot, st.calib)
            .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::sync_channel(RECORD_QUEUE);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || record_loop(writer, rx, done_tx));
        st.record_tx = Some(tx);
        st.record_done = Some(done_rx);
        println!("recording");
        Ok(Json(serde_json::json!({ "recording": true })))
    }

    async fn stop(
        State(state): State<Arc<Mutex<CaptureState>>>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let done = {
            let mut st = state.lock().map_err(|e| e.to_string())?;
            st.record_tx.take();
            st.record_done.take()
        };
        let Some(done) = done else {
            return Err("not recording".to_string().into());
        };
        let id = tokio::task::spawn_blocking(move || done.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| "recorder stopped".to_string())??;
        println!("saved clip {id}");
        Ok(Json(serde_json::json!({ "clip": id })))
    }

    fn record_loop(
        mut writer: tracker::ClipWriter,
        rx: Receiver<Option<Arc<tracker::StoredFrame>>>,
        done: mpsc::Sender<Result<String, String>>,
    ) {
        while let Ok(Some(frame)) = rx.recv() {
            if let Err(e) = writer.write(&frame) {
                let _ = done.send(Err(e.to_string()));
                return;
            }
        }
        let _ = done.send(writer.finish().map_err(|e| e.to_string()));
    }
}
