//! The Train page: builds the dataset from the sessions marked for training,
//! sends it to the configured training server, follows the job, and
//! promotes the candidate model it returns. One job at a time.

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

use crate::error::AppError;

const QUICK: Duration = Duration::from_secs(5);
/// Long enough to upload a large dataset or download a model.
const TRANSFER: Duration = Duration::from_secs(600);
const MODEL_LIMIT: u64 = 1 << 30;
const CANDIDATE: &str = "ball-candidate.onnx";

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
}

pub fn router(root: PathBuf, server: &str, model: PathBuf) -> Router {
    let trainer = Arc::new(Trainer {
        root,
        server: base_url(server),
        model,
        job: Mutex::new(Job::default()),
    });
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
    job: Job,
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
        job,
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
    let (archive, built) = blocking(move || {
        let chosen: Chosen = dataset::chosen_sessions(&root).map_err(|e| e.to_string())?;
        if chosen.include.is_empty() {
            return Err("no sessions are marked include; choose some on the Sessions page".into());
        }
        dataset::build_archive(&chosen, &crate::jpeg::encode_gray_jpeg, Some(&model))
            .map_err(|e| e.to_string())
    })
    .await?;
    set(t, |job| {
        job.phase = "sending".into();
        job.built = Some(built);
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

/// The job, refreshed from the server while it is queued or running.
async fn job(State(t): State<Arc<Trainer>>) -> Result<Json<Job>, AppError> {
    let current = t.job.lock().map_err(|e| e.to_string())?.clone();
    let (Some(id), true) = (
        current.id.clone(),
        matches!(current.phase.as_str(), "queued" | "running"),
    ) else {
        return Ok(Json(current));
    };
    let url = format!("{}/jobs/{id}", t.server);
    let fetched = blocking(move || get_json(&url, QUICK)).await;
    let mut job = t.job.lock().map_err(|e| e.to_string())?;
    if job.id.as_deref() != Some(id.as_str()) {
        return Ok(Json(job.clone()));
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
    Ok(Json(job.clone()))
}

/// Download the finished job's model next to the running one as the
/// candidate, then rename it over the running model.
async fn promote(State(t): State<Arc<Trainer>>) -> Response {
    let id = match t.job.lock() {
        Ok(job) if job.phase == "done" && job.promoted.is_none() => job.id.clone(),
        Ok(_) => None,
        Err(_) => return AppError("job lock poisoned".into()).into_response(),
    };
    let Some(id) = id else {
        return (StatusCode::CONFLICT, "no finished job to promote").into_response();
    };
    let url = format!("{}/jobs/{id}/model", t.server);
    let model = t.model.clone();
    let outcome = blocking(move || {
        let mut resp = agent(TRANSFER)
            .get(&url)
            .call()
            .map_err(|e| e.to_string())?;
        if resp.status() != 200 {
            let status = resp.status().as_u16();
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("the training server answered {status}: {body}"));
        }
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(MODEL_LIMIT)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        install(&model, &bytes)
    })
    .await;
    match outcome {
        Ok(message) => {
            set(&t, |job| job.promoted = Some(message.clone()));
            Json(serde_json::json!({ "message": message })).into_response()
        }
        Err(e) => AppError(e).into_response(),
    }
}

/// Write `bytes` as the candidate beside `model`, then rename it into place.
fn install(model: &Path, bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("the training server sent an empty model".into());
    }
    let candidate = model.with_file_name(CANDIDATE);
    if let Some(dir) = candidate.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&candidate, bytes).map_err(|e| format!("{}: {e}", candidate.display()))?;
    std::fs::rename(&candidate, model).map_err(|e| format!("{}: {e}", model.display()))?;
    Ok(format!(
        "{} is now the new model; restart the tracker to load it",
        model.display()
    ))
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
        install(&model, b"new").unwrap();
        assert_eq!(std::fs::read(&model).unwrap(), b"new");
        assert!(!dir.join("models").join(CANDIDATE).exists());
        assert!(install(&model, b"").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
