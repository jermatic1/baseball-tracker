//! The Train page: builds the dataset from the sessions marked for training,
//! sends it to the configured training server, follows the job, and
//! promotes the candidate model it returns, loading it into the running
//! detector and writing what it trained on beside it. One job at a time.
//! It also keeps the status line's model fields current.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use tracker::dataset::{self, Chosen, Counts, SessionRef};
use tracker::model_record::{self, file_sha1, ModelRecord};
use tracker::SessionMeta;

use crate::error::AppError;
use crate::hub::Hub;
use crate::protocol::ModelStatus;

const QUICK: Duration = Duration::from_secs(5);
/// Long enough to upload a large dataset or download a model.
const TRANSFER: Duration = Duration::from_secs(600);
const MODEL_LIMIT: u64 = 1 << 30;
const CANDIDATE: &str = "ball-candidate.onnx";
/// The running model, set aside while its replacement loads.
const PREVIOUS: &str = "ball-previous.onnx";
/// A promoted model that failed to load.
const FAILED: &str = "ball-failed.onnx";
/// How often the untrained-session count is recounted.
const RECOUNT: Duration = Duration::from_secs(5);

/// Loads the model at a path into the running detector, returning once it
/// is ready and swapped in. A tracker with no detector has none.
pub type Loader = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// Where a job is: `idle`, `building`, `sending`, then the server's
/// `queued`, `running`, `done`, or `failed`.
#[derive(Debug, Clone, Serialize)]
struct Job {
    phase: String,
    id: Option<String>,
    log: String,
    result: Option<Value>,
    error: Option<String>,
    built: Option<dataset::Built>,
    /// Session ids the job trains on and holds out, as its manifest lists.
    sessions: Vec<String>,
    holdout: Vec<String>,
    promoted: Option<String>,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            id: None,
            log: String::new(),
            result: None,
            error: None,
            built: None,
            sessions: Vec::new(),
            holdout: Vec::new(),
            promoted: None,
        }
    }
}

impl Job {
    fn active(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "building" | "sending" | "queued" | "running"
        )
    }
}

struct Trainer {
    root: PathBuf,
    /// Base URL with no trailing slash; empty when none is configured.
    server: String,
    /// The model the tracker loaded.
    model: PathBuf,
    job: Mutex<Job>,
    hub: Arc<Hub>,
    loader: Option<Loader>,
}

/// The Train page's routes. Also describes the model in the status and
/// recounts its untrained sessions every few seconds.
pub fn router(
    root: PathBuf,
    server: &str,
    model: PathBuf,
    hub: Arc<Hub>,
    loader: Option<Loader>,
) -> Router {
    let trainer = Arc::new(Trainer {
        root,
        server: base_url(server),
        model,
        job: Mutex::new(Job::default()),
        hub,
        loader,
    });
    describe(&trainer);
    tokio::spawn(recount_forever(Arc::clone(&trainer)));
    Router::new()
        .route("/train", get(page))
        .route("/api/train", get(overview).post(start))
        .route("/api/train/job", get(job))
        .route("/api/train/promote", post(promote))
        .with_state(trainer)
}

fn base_url(server: &str) -> String {
    let server = server.trim().trim_end_matches('/');
    if server.is_empty() || server.contains("://") {
        server.to_string()
    } else {
        format!("http://{server}")
    }
}

async fn page() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html")],
        include_str!("train.html"),
    )
        .into_response()
}

#[derive(Serialize)]
struct SessionRow {
    id: String,
    #[serde(flatten)]
    counts: Counts,
    error: Option<String>,
}

#[derive(Serialize)]
struct Overview {
    server: String,
    health: Option<Value>,
    health_error: Option<String>,
    include: Vec<SessionRow>,
    holdout: Vec<SessionRow>,
    model: String,
    /// What the live model trained on, when the Train page promoted it.
    record: Option<ModelRecord>,
    job: JobView,
}

/// The chosen sessions with what each adds, the server and its health, and
/// the job.
async fn overview(State(t): State<Arc<Trainer>>) -> Result<Json<Overview>, AppError> {
    let root = t.root.clone();
    let chosen = tokio::task::spawn_blocking(move || dataset::chosen_sessions(&root))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let (health, health_error) = if t.server.is_empty() {
        (None, None)
    } else {
        let url = format!("{}/health", t.server);
        match blocking(move || get_json(&url, QUICK)).await {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        }
    };
    let job = t.job.lock().map_err(|e| e.to_string())?.clone();
    Ok(Json(Overview {
        server: t.server.clone(),
        health,
        health_error,
        include: rows(&chosen.include),
        holdout: rows(&chosen.holdout),
        model: t.model.display().to_string(),
        record: t.hub.model_record(),
        job: view(&t, job),
    }))
}

fn rows(sessions: &[SessionRef]) -> Vec<SessionRow> {
    sessions
        .iter()
        .map(|s| match dataset::session_counts(&s.dir) {
            Ok(counts) => SessionRow {
                id: s.id.clone(),
                counts,
                error: None,
            },
            Err(e) => SessionRow {
                id: s.id.clone(),
                counts: Counts::default(),
                error: Some(e.to_string()),
            },
        })
        .collect()
}

/// Build the dataset and send it; the job then runs on the server.
async fn start(State(t): State<Arc<Trainer>>) -> Response {
    if t.server.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "no training server is configured; set [training] server in config.toml",
        )
            .into_response();
    }
    {
        let Ok(mut job) = t.job.lock() else {
            return AppError("job lock poisoned".into()).into_response();
        };
        if job.active() {
            return (
                StatusCode::CONFLICT,
                "a training job is already running; only one runs at a time",
            )
                .into_response();
        }
        *job = Job {
            phase: "building".into(),
            ..Job::default()
        };
    }
    tokio::spawn(submit(Arc::clone(&t)));
    StatusCode::ACCEPTED.into_response()
}

async fn submit(t: Arc<Trainer>) {
    let outcome = send(&t).await;
    if let Ok(mut job) = t.job.lock() {
        match outcome {
            Ok(id) => {
                job.phase = "queued".into();
                job.id = Some(id);
            }
            Err(e) => {
                job.phase = "failed".into();
                job.error = Some(e);
            }
        }
    }
}

async fn send(t: &Trainer) -> Result<String, String> {
    let root = t.root.clone();
    let model = t.model.clone();
    let (archive, built, (sessions, holdout)) = blocking(move || {
        let chosen: Chosen = dataset::chosen_sessions(&root).map_err(|e| e.to_string())?;
        if chosen.include.is_empty() {
            return Err("no sessions are marked include; choose some on the Sessions page".into());
        }
        let ids = |s: &[SessionRef]| s.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        let sessions = (ids(&chosen.include), ids(&chosen.holdout));
        dataset::build_archive(&chosen, &crate::jpeg::encode_gray_jpeg, Some(&model))
            .map(|(archive, built)| (archive, built, sessions))
            .map_err(|e| e.to_string())
    })
    .await?;
    set(t, |job| {
        job.phase = "sending".into();
        job.built = Some(built);
        job.sessions = sessions;
        job.holdout = holdout;
    });
    let url = format!("{}/train", t.server);
    let (status, body) = blocking(move || {
        let mut resp = agent(TRANSFER)
            .post(&url)
            .header("content-type", "application/gzip")
            .send(&archive[..])
            .map_err(|e| e.to_string())?;
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        Ok((resp.status().as_u16(), body))
    })
    .await?;
    match status {
        202 => serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["job"].as_str().map(str::to_string))
            .ok_or_else(|| format!("the training server answered without a job id: {body}")),
        409 => Err("the training server is busy with another job; only one runs at a time".into()),
        _ => Err(format!("the training server answered {status}: {body}")),
    }
}

/// A job with the live model's status, so the page can follow a promoted
/// model loading without opening the socket, which would start capture.
#[derive(Serialize)]
struct JobView {
    #[serde(flatten)]
    job: Job,
    model: ModelStatus,
}

fn view(t: &Trainer, job: Job) -> JobView {
    JobView {
        job,
        model: t.hub.model_status(),
    }
}

/// The job, refreshed from the server while it is queued or running.
async fn job(State(t): State<Arc<Trainer>>) -> Result<Json<JobView>, AppError> {
    let current = t.job.lock().map_err(|e| e.to_string())?.clone();
    let (Some(id), true) = (
        current.id.clone(),
        matches!(current.phase.as_str(), "queued" | "running"),
    ) else {
        return Ok(Json(view(&t, current)));
    };
    let url = format!("{}/jobs/{id}", t.server);
    let fetched = blocking(move || get_json(&url, QUICK)).await;
    let mut job = t.job.lock().map_err(|e| e.to_string())?;
    if job.id.as_deref() != Some(id.as_str()) {
        return Ok(Json(view(&t, job.clone())));
    }
    match fetched {
        Ok(v) => {
            if let Some(state) = v["state"].as_str() {
                job.phase = state.to_string();
            }
            job.log = v["log"].as_str().unwrap_or_default().to_string();
            job.result = v.get("result").filter(|r| !r.is_null()).cloned();
            job.error = None;
        }
        Err(e) => job.error = Some(format!("cannot reach the training server: {e}")),
    }
    Ok(Json(view(&t, job.clone())))
}

/// Start loading the finished job's model; the status follows it.
async fn promote(State(t): State<Arc<Trainer>>) -> Response {
    let job = match t.job.lock() {
        Ok(job) => job.clone(),
        Err(_) => return AppError("job lock poisoned".into()).into_response(),
    };
    let Some(id) = job
        .id
        .clone()
        .filter(|_| job.phase == "done" && job.promoted.is_none())
    else {
        return (StatusCode::CONFLICT, "no finished job to promote").into_response();
    };
    let mut started = false;
    t.hub.update_model_status(|m| {
        if !m.loading {
            m.loading = true;
            m.error = None;
            started = true;
        }
    });
    if !started {
        return (StatusCode::CONFLICT, "a promoted model is still loading").into_response();
    }
    tokio::spawn(load_promoted(Arc::clone(&t), id, job));
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "message": "loading the new model" })),
    )
        .into_response()
}

/// Download the model, install and load it, and record what it trained on.
async fn load_promoted(t: Arc<Trainer>, id: String, job: Job) {
    let url = format!("{}/jobs/{id}/model", t.server);
    let trainer = Arc::clone(&t);
    let outcome = blocking(move || {
        let bytes = download(&url)?;
        install(&trainer.model, &bytes, trainer.loader.as_ref())?;
        let sha1 = file_sha1(&trainer.model)?;
        record(sha1, &job)
            .save(&trainer.model)
            .map_err(|e| format!("the new model is live, but its record was not written: {e}"))?;
        describe(&trainer);
        recount(&trainer);
        Ok(())
    })
    .await;
    let status = t.hub.model_status();
    let message = format!("{} {} is now the live model", status.file, status.sha1);
    set(&t, |j| {
        if j.id.as_deref() == Some(id.as_str()) && outcome.is_ok() {
            j.promoted = Some(message);
        }
    });
    t.hub.update_model_status(|m| {
        m.loading = false;
        m.error = outcome.err();
    });
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = agent(TRANSFER).get(url).call().map_err(|e| e.to_string())?;
    if resp.status() != 200 {
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(format!("the training server answered {status}: {body}"));
    }
    resp.body_mut()
        .with_config()
        .limit(MODEL_LIMIT)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// Write `bytes` as the candidate beside `model` and rename it into place,
/// keeping the old model aside until `load` has the new one running. If it
/// fails to load, the candidate goes aside as `ball-failed.onnx` and the
/// old model returns.
fn install(model: &Path, bytes: &[u8], load: Option<&Loader>) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("the training server sent an empty model".into());
    }
    let named = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let candidate = model.with_file_name(CANDIDATE);
    let previous = model.with_file_name(PREVIOUS);
    if let Some(dir) = candidate.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| named(dir, e))?;
    }
    std::fs::write(&candidate, bytes).map_err(|e| named(&candidate, e))?;
    let had = model.exists();
    if had {
        std::fs::rename(model, &previous).map_err(|e| named(model, e))?;
    }
    let restore = || {
        if had {
            std::fs::rename(&previous, model).map_err(|e| named(model, e))?;
        }
        Ok::<(), String>(())
    };
    if let Err(e) = std::fs::rename(&candidate, model) {
        restore()?;
        return Err(named(model, e));
    }
    match load.map_or(Ok(()), |load| load(model)) {
        Ok(()) => {
            if had {
                std::fs::remove_file(&previous).map_err(|e| named(&previous, e))?;
            }
            Ok(())
        }
        Err(e) => {
            let failed = model.with_file_name(FAILED);
            std::fs::rename(model, &failed).map_err(|e| named(model, e))?;
            restore()?;
            Err(format!(
                "the new model did not load, so the old one is still running; it was kept as {}: {e}",
                failed.display()
            ))
        }
    }
}

/// The record of a promoted job's model: the sessions the server says it
/// trained on, or else those it was sent, and the candidate's scores.
fn record(sha1: String, job: &Job) -> ModelRecord {
    let result = job.result.as_ref();
    let ids = |key: &str, sent: &[String]| {
        result
            .and_then(|r| r[key].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| sent.to_vec())
    };
    let scores = result
        .and_then(|r| r["candidate"].as_object())
        .map(|c| {
            c.iter()
                .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
                .collect()
        })
        .unwrap_or_default();
    ModelRecord {
        sha1,
        promoted: dataset::rfc3339_now(),
        sessions: ids("sessions", &job.sessions),
        holdout: ids("holdout", &job.holdout),
        scores,
    }
}

/// Put the model file, its SHA-1, and its record when it matches into the
/// status.
fn describe(t: &Trainer) {
    let sha1 = file_sha1(&t.model).ok();
    let record = sha1.as_deref().and_then(|sha1| {
        ModelRecord::load(&t.model)
            .ok()
            .flatten()
            .filter(|r| r.sha1 == sha1)
    });
    let file = t
        .model
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    t.hub.update_model_status(|m| {
        m.file = file;
        m.sha1 = sha1.map_or_else(String::new, |s| s.chars().take(7).collect());
        m.promoted = record.as_ref().map(|r| r.promoted.clone());
    });
    t.hub.set_model_record(record);
}

/// Count the reviewed sessions marked for training that the live model did
/// not train on. A session whose files cannot be read counts as unreviewed.
fn recount(t: &Trainer) {
    let Ok(chosen) = dataset::chosen_sessions(&t.root) else {
        return;
    };
    let sessions: Vec<(String, SessionMeta, usize)> = chosen
        .include
        .iter()
        .filter_map(|s| {
            let meta = SessionMeta::load(&s.dir).ok()?;
            let reviewed = crate::label::reviewed_clips(&s.dir).unwrap_or(0);
            Some((s.id.clone(), meta, reviewed))
        })
        .collect();
    let record = t.hub.model_record();
    let n = model_record::untrained(
        sessions.iter().map(|(id, m, r)| (id.as_str(), m, *r)),
        record.as_ref(),
    );
    t.hub.update_model_status(|m| m.untrained = n);
}

async fn recount_forever(t: Arc<Trainer>) {
    loop {
        let trainer = Arc::clone(&t);
        let _ = blocking(move || {
            recount(&trainer);
            Ok(())
        })
        .await;
        tokio::time::sleep(RECOUNT).await;
    }
}

fn set(t: &Trainer, change: impl FnOnce(&mut Job)) {
    if let Ok(mut job) = t.job.lock() {
        change(&mut job);
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

fn get_json(url: &str, timeout: Duration) -> Result<Value, String> {
    let mut resp = agent(timeout).get(url).call().map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("{status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_get_a_scheme_and_lose_trailing_slashes() {
        assert_eq!(base_url(""), "");
        assert_eq!(base_url(" gpu:7882/ "), "http://gpu:7882");
        assert_eq!(base_url("https://gpu/"), "https://gpu");
    }

    #[test]
    fn promoting_renames_the_candidate_over_the_model() {
        let dir = std::env::temp_dir().join(format!("train-install-{}", std::process::id()));
        let model = dir.join("models/ball.onnx");
        install(&model, b"new", None).unwrap();
        assert_eq!(std::fs::read(&model).unwrap(), b"new");
        let loads: Loader = Arc::new(|_| Ok(()));
        install(&model, b"newer", Some(&loads)).unwrap();
        assert_eq!(std::fs::read(&model).unwrap(), b"newer");
        for aside in [CANDIDATE, PREVIOUS, FAILED] {
            assert!(!dir.join("models").join(aside).exists(), "{aside}");
        }
        assert!(install(&model, b"", None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_model_that_fails_to_load_is_set_aside_and_the_old_one_kept() {
        let dir = std::env::temp_dir().join(format!("train-failed-{}", std::process::id()));
        let model = dir.join("models/ball.onnx");
        install(&model, b"old", None).unwrap();
        let fails: Loader = Arc::new(|_| Err("bad graph".into()));
        let err = install(&model, b"broken", Some(&fails)).unwrap_err();
        assert!(err.contains("bad graph"), "{err}");
        assert_eq!(std::fs::read(&model).unwrap(), b"old");
        assert_eq!(
            std::fs::read(dir.join("models").join(FAILED)).unwrap(),
            b"broken"
        );
        assert!(!dir.join("models").join(PREVIOUS).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
