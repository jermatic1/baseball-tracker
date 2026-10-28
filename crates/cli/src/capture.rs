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
    let session = tracker::DataDir::create(&path).map_err(|e| e.to_string())?;
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
    let exposure_us = session.config.capture.exposure;
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
    let session = tracker::DataDir::create(&path).map_err(|e| e.to_string())?;
    let width = session.config.capture.width;
    let height = session.config.capture.height;
    let fps = session.config.capture.fps as f32;
    let mut cam = device::OakCamera::open(
        width,
        height,
        fps.max(1.0),
        session.config.capture.exposure,
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
    let session = tracker::DataDir::create(&path).map_err(|e| e.to_string())?;
    let exposure_us = session.config.capture.exposure;
    let gain = session.config.capture.gain;
    let (cam, calib) = open_oak(&session)?;
    let hub = crate::hub::Hub::open(session.clone())?;
    server::run(session, cam, exposure_us, gain, calib, None, hub).await
}

/// Open the OAK at the session's capture settings and save its calibration
/// into the session.
#[cfg(feature = "oak")]
pub(crate) fn open_oak(
    session: &tracker::DataDir,
) -> Result<(device::OakCamera, tracker::StereoCalib), String> {
    let c = &session.config.capture;
    let cam = device::OakCamera::open(
        c.width,
        c.height,
        (c.fps as f32).max(1.0),
        c.exposure,
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
    session: &tracker::DataDir,
    device_json: Option<&str>,
) -> tracker::StereoCalib {
    let c = &session.config.capture;
    let calib = match session.rectifier(c.width, c.height) {
        Some(r) => {
            println!("stereo rectification from device calibration");
            r.calib()
        }
        None => {
            println!("no device calibration; using the disparity offset from config");
            tracker::stereo_calib_from_device(device_json, c.width)
                .with_offset(session.config.stereo.disparity_offset)
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
    use std::time::Duration;

    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    use axum::extract::State;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use device::Camera;
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::broadcast::error::RecvError;
    use tokio::sync::watch;
    use tracker::geom::Intrinsics;
    use tracker::plate::{check_fit, solve_pose, window_depth_m, PlateCorners, PoseFit};

    use crate::convert::to_stored;
    use crate::hub::{Hub, Outbound};
    use crate::jpeg::encode_gray_jpeg;
    use crate::protocol::{
        CalibrateAction, CameraStats, ClientMsg, FitSource, LiveStatus, RecordAction, ServerMsg,
        Status,
    };

    const BIND: &str = "0.0.0.0:7880";
    const RECORD_QUEUE: usize = 32;
    const STATUS_PERIOD: Duration = Duration::from_millis(500);
    const PREVIEW_PERIOD: Duration = Duration::from_millis(100);
    const CALIBRATION_PERIOD: Duration = Duration::from_millis(200);

    /// What a live watcher plugs into the capture loop: every frame goes
    /// down `frames` in order, and the status ticker reads `status`.
    pub struct LiveTap {
        pub frames: SyncSender<Arc<tracker::StoredFrame>>,
        pub sent: Arc<AtomicU64>,
        pub drops: Arc<AtomicU64>,
        pub status: Arc<Mutex<LiveStatus>>,
        /// Set when the server is shutting down so the detector can skip
        /// its backlog if no episode is open.
        pub stop: Arc<AtomicBool>,
    }

    /// A plate fit with where its corners came from and what looks wrong.
    #[derive(Clone)]
    struct Fit {
        corners: PlateCorners,
        pose: PoseFit,
        source: FitSource,
        warnings: Vec<String>,
    }

    impl Fit {
        fn checked(
            corners: PlateCorners,
            pose: PoseFit,
            source: FitSource,
            frame: &tracker::StoredFrame,
        ) -> Self {
            let intr = Intrinsics::from_fov(frame.width, frame.height, 80.0, 55.0);
            let depth = |px| window_depth_m(&frame.depth_mm, frame.width, frame.height, px);
            let has_depth = !frame.depth_mm.is_empty();
            let warnings = check_fit(
                &pose,
                &corners,
                &intr,
                has_depth.then_some(&depth as &dyn Fn((f64, f64)) -> Option<f64>),
            );
            Self {
                corners,
                pose,
                source,
                warnings,
            }
        }
    }

    struct CaptureState {
        latest: Option<Arc<tracker::StoredFrame>>,
        stats: Option<CameraStats>,
        exposure_us: u32,
        gain: u32,
        ir_flood: f32,
        ir_dot: f32,
        calib: tracker::StereoCalib,
        session: tracker::DataDir,
        record_tx: Option<SyncSender<Option<Arc<tracker::StoredFrame>>>>,
        record_drops: Arc<AtomicU64>,
        record_done: Option<Receiver<Result<String, String>>>,
        live: Option<LiveTap>,
        stop: Arc<AtomicBool>,
        /// Latest plate fit while calibration mode is on: the auto-detector's
        /// good fits until the user sends clicked corners.
        last_fit: Option<Fit>,
    }

    impl CaptureState {
        fn has_clicked_fit(&self) -> bool {
            self.last_fit
                .as_ref()
                .is_some_and(|f| f.source == FitSource::Clicked)
        }
    }

    #[derive(Clone)]
    struct App {
        state: Arc<Mutex<CaptureState>>,
        hub: Arc<Hub>,
        stop: watch::Receiver<bool>,
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run<C: Camera + Send + 'static>(
        session: tracker::DataDir,
        cam: C,
        exposure_us: u32,
        gain: u32,
        calib: tracker::StereoCalib,
        live: Option<LiveTap>,
        hub: Arc<Hub>,
    ) -> Result<(), String> {
        let stopping = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(CaptureState {
            latest: None,
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
            last_fit: None,
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let app = App {
            state: Arc::clone(&state),
            hub,
            stop: stop_rx,
        };
        let poll_state = Arc::clone(&state);
        let poller = std::thread::spawn(move || poll_loop(cam, poll_state));
        tokio::spawn(status_ticker(app.clone(), Arc::clone(&stopping)));
        tokio::spawn(preview_producer(app.clone(), Arc::clone(&stopping)));
        tokio::spawn(calibration_sampler(app.clone(), Arc::clone(&stopping)));
        let router = Router::new()
            .route("/", get(|| page("text/html", include_str!("tracker.html"))))
            .route(
                "/setup",
                get(|| page("text/html", include_str!("setup.html"))),
            )
            .route(
                "/page.js",
                get(|| page("text/javascript", include_str!("page.js"))),
            )
            .route(
                "/page.css",
                get(|| page("text/css", include_str!("page.css"))),
            )
            .route("/api/history", get(history))
            .route("/games", get(games))
            .route("/games/{name}", get(game_page))
            .route("/ws", get(ws))
            .with_state(app);
        let addr: SocketAddr = BIND
            .parse()
            .map_err(|e: std::net::AddrParseError| e.to_string())?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| e.to_string())?;
        println!("http://{BIND}  ws://{BIND}/ws");
        match crate::discovery::spawn(addr.port()) {
            Ok(()) => println!("discovery udp://0.0.0.0:{}", crate::discovery::PORT),
            Err(e) => eprintln!("discovery off: {e}"),
        }
        let shutdown = {
            let stopping = Arc::clone(&stopping);
            async move {
                let _ = tokio::signal::ctrl_c().await;
                println!("stopping");
                stopping.store(true, Ordering::Relaxed);
                let _ = stop_tx.send(true);
                tokio::spawn(async {
                    let _ = tokio::signal::ctrl_c().await;
                    std::process::exit(130);
                });
            }
        };
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| e.to_string())?;
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
        let _ = tokio::task::spawn_blocking(move || poller.join()).await;
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
                    st.stats = Some(CameraStats::from_device(stats, drops));
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

    async fn page(content_type: &'static str, body: &'static str) -> Response {
        ([(axum::http::header::CONTENT_TYPE, content_type)], body).into_response()
    }

    async fn history(
        State(app): State<App>,
    ) -> Result<axum::Json<crate::history::History>, crate::error::AppError> {
        Ok(axum::Json(app.hub.history()?))
    }

    /// The registered game modules, for the pages' navigation.
    async fn games(State(app): State<App>) -> axum::Json<Vec<String>> {
        axum::Json(app.hub.game_names())
    }

    async fn game_page(
        axum::extract::Path(name): axum::extract::Path<String>,
        State(app): State<App>,
    ) -> Response {
        match app.hub.game_page(&name) {
            Some(html) => page("text/html", html).await,
            None => (axum::http::StatusCode::NOT_FOUND, "no such game").into_response(),
        }
    }

    async fn ws(ws: WebSocketUpgrade, State(app): State<App>) -> Response {
        ws.on_upgrade(move |socket| handle_socket(socket, app))
    }

    /// One connection: the first message names the client, then commands
    /// come in while the hub's broadcast and this client's errors go out.
    async fn handle_socket(socket: WebSocket, app: App) {
        let (mut sink, mut stream) = socket.split();
        let (id, games) = loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::Hello { client, games, .. }) => {
                        break (app.hub.join(&client), games)
                    }
                    _ => {
                        let _ = sink
                            .send(Message::Text(error_text(None, "say hello first").into()))
                            .await;
                        return;
                    }
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => continue,
            }
        };
        let preview = Arc::new(AtomicBool::new(false));
        let (direct_tx, mut direct_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut rx = app.hub.tx.subscribe();
        for msg in app.hub.welcome(&games) {
            if let Ok(text) = serde_json::to_string(&msg) {
                let _ = direct_tx.send(text);
            }
        }
        let wants_preview = Arc::clone(&preview);
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = rx.recv() => match msg {
                        Ok(Outbound::Text(text)) => {
                            if sink.send(Message::Text(text.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                        Ok(Outbound::Preview(bytes)) => {
                            if wants_preview.load(Ordering::Relaxed)
                                && sink
                                    .send(Message::Binary(bytes.as_slice().to_vec().into()))
                                    .await
                                    .is_err()
                            {
                                break;
                            }
                        }
                        Ok(Outbound::Game { game, text }) => {
                            if games.iter().any(|g| **g == *game)
                                && sink
                                    .send(Message::Text(text.to_string().into()))
                                    .await
                                    .is_err()
                            {
                                break;
                            }
                        }
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    },
                    direct = direct_rx.recv() => match direct {
                        Some(text) => {
                            if sink.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
        });
        let mut stop = app.stop.clone();
        loop {
            let msg = tokio::select! {
                msg = stream.next() => msg,
                _ = stop.changed() => break,
            };
            let Some(Ok(msg)) = msg else { break };
            let text = match msg {
                Message::Text(text) => text,
                Message::Close(_) => break,
                _ => continue,
            };
            let (request, outcome) = match serde_json::from_str::<ClientMsg>(&text) {
                Ok(cmd) => (Some(cmd.name()), handle_command(cmd, &app, &preview).await),
                Err(e) => (None, Err(format!("bad message: {e}"))),
            };
            if let Err(message) = outcome {
                let _ = direct_tx.send(error_text(request, &message));
            }
        }
        if preview.swap(false, Ordering::Relaxed) {
            app.hub.preview_subs.fetch_sub(1, Ordering::Relaxed);
        }
        app.hub.leave(id);
        writer.abort();
    }

    fn error_text(request: Option<&str>, message: &str) -> String {
        serde_json::to_string(&ServerMsg::Error {
            request: request.map(str::to_string),
            message: message.to_string(),
        })
        .unwrap_or_default()
    }

    async fn handle_command(
        cmd: ClientMsg,
        app: &App,
        preview: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        match cmd {
            ClientMsg::Hello { .. } | ClientMsg::Unknown => Ok(()),
            ClientMsg::AddUser { name } => app.hub.add_user(&name),
            ClientMsg::StartSession {
                user,
                activity,
                mode,
            } => app.hub.start_session(&user, activity, mode),
            ClientMsg::EndSession { session } => app.hub.end_session(&session),
            ClientMsg::Game { game, body } => app.hub.game_message(&game, body),
            ClientMsg::Preview { enabled } => {
                if preview.swap(enabled, Ordering::Relaxed) != enabled {
                    if enabled {
                        app.hub.preview_subs.fetch_add(1, Ordering::Relaxed);
                    } else {
                        app.hub.preview_subs.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                Ok(())
            }
            ClientMsg::Set {
                exposure,
                gain,
                ir_flood,
                ir_dot,
            } => {
                let mut st = app.state.lock().map_err(|e| e.to_string())?;
                if let Some(v) = exposure {
                    st.exposure_us = v.clamp(1, 1_000_000);
                }
                if let Some(v) = gain {
                    st.gain = v.clamp(100, 1600);
                }
                if let Some(v) = ir_flood {
                    st.ir_flood = v.clamp(0.0, 1.0);
                }
                if let Some(v) = ir_dot {
                    st.ir_dot = v.clamp(0.0, 1.0);
                }
                Ok(())
            }
            ClientMsg::Record { action } => match action {
                RecordAction::Start => {
                    let mut st = app.state.lock().map_err(|e| e.to_string())?;
                    start_record(&mut st)
                }
                RecordAction::Stop => stop_record(app).await.map(|id| println!("saved clip {id}")),
            },
            ClientMsg::Calibrate { action, corners } => match action {
                CalibrateAction::Start => {
                    if let Ok(mut st) = app.state.lock() {
                        st.last_fit = None;
                    }
                    app.hub.set_calibrating(true);
                    Ok(())
                }
                CalibrateAction::Stop => {
                    app.hub.set_calibrating(false);
                    Ok(())
                }
                CalibrateAction::Apply => apply_calibration(app),
                CalibrateAction::Corners => clicked_calibration(app, corners).await,
            },
        }
    }

    fn start_record(st: &mut CaptureState) -> Result<(), String> {
        if st.stop.load(Ordering::Relaxed) {
            return Err("stopping".into());
        }
        if st.record_tx.is_some() {
            return Err("already recording".into());
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
        Ok(())
    }

    async fn stop_record(app: &App) -> Result<String, String> {
        let done = {
            let mut st = app.state.lock().map_err(|e| e.to_string())?;
            st.record_tx.take();
            st.record_done.take()
        };
        let Some(done) = done else {
            return Err("not recording".into());
        };
        tokio::task::spawn_blocking(move || done.recv())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| "recorder stopped".to_string())?
    }

    /// Save the latest good plate fit as the session's mount.
    fn apply_calibration(app: &App) -> Result<(), String> {
        let (fit, dir) = {
            let st = app.state.lock().map_err(|e| e.to_string())?;
            (st.last_fit.clone(), st.session.dir.clone())
        };
        let fit = fit.ok_or("no plate fit yet; keep the plate clear or click its corners")?;
        let config = {
            let mut cfg = app.hub.config.write().map_err(|e| e.to_string())?;
            cfg.mount = fit.pose.mount.clone();
            cfg.clone()
        };
        config
            .save(dir.join("config.toml"))
            .map_err(|e| e.to_string())?;
        crate::calibrate::print_fit(&fit.pose);
        app.hub.send(&calibration_msg(Some(&fit), true));
        app.hub.set_calibrating(false);
        Ok(())
    }

    /// Fit the pose to the user's clicked corners, seeded by the configured
    /// mount, and keep it for Apply even when it has warnings.
    async fn clicked_calibration(app: &App, corners: Option<Vec<[f64; 2]>>) -> Result<(), String> {
        let px: [[f64; 2]; 5] = corners
            .ok_or("corners missing")?
            .try_into()
            .map_err(|_| "need five corners")?;
        let corners = PlateCorners {
            px: px.map(|[x, y]| (x, y)),
        };
        let frame = app.state.lock().map_err(|e| e.to_string())?.latest.clone();
        let frame = frame.ok_or("no frame yet")?;
        let seed = app.hub.config().mount;
        let fit = tokio::task::spawn_blocking(move || {
            let intr = Intrinsics::from_fov(frame.width, frame.height, 80.0, 55.0);
            solve_pose(&corners, &intr, &seed)
                .map(|pose| Fit::checked(corners, pose, FitSource::Clicked, &frame))
        })
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no pose fits those corners")?;
        app.state.lock().map_err(|e| e.to_string())?.last_fit = Some(fit.clone());
        app.hub.send(&calibration_msg(Some(&fit), false));
        Ok(())
    }

    fn calibration_msg(fit: Option<&Fit>, applied: bool) -> ServerMsg {
        let pts = |p: &[(f64, f64); 5]| p.iter().map(|&(x, y)| [x, y]).collect::<Vec<_>>();
        ServerMsg::Calibration {
            found: fit.is_some(),
            corners: fit.map(|f| pts(&f.corners.px)),
            reprojected: fit.map(|f| pts(&f.pose.reprojected)),
            rms: fit.map(|f| f.pose.rms_px),
            mount: fit.map(|f| f.pose.mount.clone()),
            applied,
            source: fit.map_or(FitSource::Auto, |f| f.source),
            warnings: fit.map(|f| f.warnings.clone()).unwrap_or_default(),
        }
    }

    async fn status_ticker(app: App, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(STATUS_PERIOD).await;
            let Ok(st) = app.state.lock() else { break };
            let detector = st.live.as_ref().and_then(|tap| {
                tap.status.lock().ok().map(|s| {
                    let mut s = s.clone();
                    s.drops = tap.drops.load(Ordering::Relaxed);
                    s
                })
            });
            let status = Status {
                camera: st.stats.clone(),
                detector,
                clients: app.hub.client_names(),
                capturing: app.hub.capturing(),
                calibrating: app.hub.calibrating(),
                recording: st.record_tx.is_some(),
                exposure: st.exposure_us,
                gain: st.gain,
                ir_flood: st.ir_flood,
                ir_dot: st.ir_dot,
                mount: app.hub.mount(),
            };
            drop(st);
            app.hub.send(&ServerMsg::Status(status));
        }
    }

    /// Encode the latest frame for subscribed clients, once per new frame.
    async fn preview_producer(app: App, stop: Arc<AtomicBool>) {
        let mut last_t = 0u64;
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(PREVIEW_PERIOD).await;
            if app.hub.preview_subs.load(Ordering::Relaxed) == 0 {
                continue;
            }
            let frame = app.state.lock().ok().and_then(|st| st.latest.clone());
            let Some(frame) = frame.filter(|f| f.t_ns != last_t) else {
                continue;
            };
            last_t = frame.t_ns;
            let encoded = tokio::task::spawn_blocking(move || {
                encode_gray_jpeg(frame.width, frame.height, &frame.left)
            })
            .await;
            if let Ok(Ok(jpeg)) = encoded {
                app.hub.send_preview(jpeg);
            }
        }
    }

    /// In calibration mode, fit the plate on the live frame a few times a
    /// second and tell clients what it sees, until the user clicks corners.
    async fn calibration_sampler(app: App, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(CALIBRATION_PERIOD).await;
            if !app.hub.calibrating() || app.state.lock().is_ok_and(|st| st.has_clicked_fit()) {
                continue;
            }
            let frame = app.state.lock().ok().and_then(|st| st.latest.clone());
            let Some(frame) = frame else { continue };
            let cfg = app.hub.config();
            let fit = tokio::task::spawn_blocking(move || {
                tracker::plate::calibrate_from_frame(&frame.left, frame.width, frame.height, &cfg)
                    .map(|(corners, pose)| Fit::checked(corners, pose, FitSource::Auto, &frame))
            })
            .await
            .ok()
            .flatten();
            let Ok(mut st) = app.state.lock() else { break };
            if st.has_clicked_fit() {
                continue;
            }
            if fit.is_some() {
                st.last_fit = fit.clone();
            }
            drop(st);
            app.hub.send(&calibration_msg(fit.as_ref(), false));
        }
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
