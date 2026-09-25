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
        if let Some(frame) = cam.poll(Duration::from_millis(0)).map_err(|e| e.to_string())? {
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
    let width = session.config.capture.width;
    let height = session.config.capture.height;
    let fps = session.config.capture.fps as f32;
    let exposure_us = session.config.capture.exposure_us;
    let gain = session.config.capture.gain;
    let cam = device::OakCamera::open(width, height, fps.max(1.0), exposure_us, gain)?;
    server::run(session, cam, fps.max(1.0), exposure_us, gain).await
}

#[cfg(feature = "oak")]
mod server {
    use std::collections::VecDeque;
    use std::net::SocketAddr;
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

    const BIND: &str = "127.0.0.1:7880";

    struct CaptureState {
        ring: VecDeque<tracker::StoredFrame>,
        ring_cap: usize,
        jpeg: Option<(Instant, Vec<u8>)>,
        exposure_us: u32,
        gain: u32,
        fps: f32,
        session: tracker::Session,
    }

    pub async fn run(
        session: tracker::Session,
        cam: device::OakCamera,
        fps: f32,
        exposure_us: u32,
        gain: u32,
    ) -> Result<(), String> {
        let ring_cap = (fps * 2.0).round().max(1.0) as usize;
        let state = Arc::new(Mutex::new(CaptureState {
            ring: VecDeque::with_capacity(ring_cap),
            ring_cap,
            jpeg: None,
            exposure_us,
            gain,
            fps,
            session,
        }));
        let poll_state = Arc::clone(&state);
        std::thread::spawn(move || poll_loop(cam, poll_state));
        let app = Router::new()
            .route("/", get(index))
            .route("/preview.jpg", get(preview))
            .route("/api/exposure", post(set_exposure))
            .route("/api/gain", post(set_gain))
            .route("/api/fps", post(set_fps))
            .route("/api/save", post(save))
            .with_state(state);
        let addr: SocketAddr = BIND
            .parse()
            .map_err(|e: std::net::AddrParseError| e.to_string())?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| e.to_string())?;
        println!("http://{BIND}");
        axum::serve(listener, app).await.map_err(|e| e.to_string())
    }

    fn poll_loop(mut cam: device::OakCamera, state: Arc<Mutex<CaptureState>>) {
        loop {
            match cam.poll(Duration::from_millis(0)) {
                Ok(Some(frame)) => {
                    let mut st = match state.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    let stored = to_stored(frame);
                    let cap = st.ring_cap.max(1);
                    st.ring.push_back(stored);
                    while st.ring.len() > cap {
                        st.ring.pop_front();
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => {
                    eprintln!("{e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    async fn index() -> Html<&'static str> {
        Html(include_str!("capture.html"))
    }

    async fn preview(State(state): State<Arc<Mutex<CaptureState>>>) -> Result<Response, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        if let Some((t, bytes)) = &st.jpeg {
            if t.elapsed() < Duration::from_millis(100) {
                return Ok(jpeg_response(bytes.clone()));
            }
        }
        let Some(frame) = st.ring.back() else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        };
        let bytes = encode_gray_jpeg(frame.width, frame.height, &frame.left)?;
        st.jpeg = Some((Instant::now(), bytes.clone()));
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
        let next = (st.gain as i64 + body.delta as i64).clamp(0, 100_000) as u32;
        st.gain = next;
        Ok(Json(serde_json::json!({ "gain": next })))
    }

    async fn set_fps(
        State(state): State<Arc<Mutex<CaptureState>>>,
        Json(body): Json<FpsBody>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        let fps = body.fps.max(1.0);
        st.fps = fps;
        st.ring_cap = (fps * 2.0).round().max(1.0) as usize;
        Ok(Json(serde_json::json!({ "fps": fps })))
    }

    async fn save(
        State(state): State<Arc<Mutex<CaptureState>>>,
    ) -> Result<Json<serde_json::Value>, AppError> {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        let frames: Vec<_> = st.ring.drain(..).collect();
        let result = st
            .session
            .save_clip(&frames, st.exposure_us, st.gain)
            .map_err(|e| e.to_string());
        st.ring.extend(frames);
        let id = result?;
        Ok(Json(serde_json::json!({ "clip": id })))
    }
}
