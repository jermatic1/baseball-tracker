//! The sessions page, and each session folder's label and playback pages
//! served under `/sessions/{id}/label/` and `/sessions/{id}/playback/`.
//! The id `none` is the folder where clips cut with no session active go.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Serialize;
use tower::ServiceExt;
use tracker::{MetaUpdate, ModelRecord, Session, SessionMeta};

use crate::error::AppError;
use crate::hub::Hub;

const NO_SESSION: &str = "none";

/// A session folder's counts and its `session.toml`.
#[derive(Debug, Serialize)]
struct Row {
    id: String,
    clips: usize,
    reviewed: usize,
    /// The live model trained on it.
    trained: bool,
    #[serde(flatten)]
    meta: SessionMeta,
}

struct Sites {
    root: PathBuf,
    hub: Arc<Hub>,
    /// One router per (page, id), built on first visit, so a folder's
    /// labels stay in one place however many tabs have it open.
    routers: Mutex<HashMap<(String, String), Router>>,
}

pub fn router(root: PathBuf, hub: Arc<Hub>) -> Router {
    let sites = Arc::new(Sites {
        root,
        hub,
        routers: Mutex::new(HashMap::new()),
    });
    Router::new()
        .route("/sessions", get(page))
        .route("/api/sessions", get(list))
        .route("/api/sessions/{id}", put(update))
        .route("/sessions/{id}/{page}", get(add_slash))
        .route(
            "/sessions/{id}/{page}/",
            get(forward_index).post(forward_index),
        )
        .route("/sessions/{id}/{page}/{*rest}", get(forward).post(forward))
        .with_state(sites)
}

async fn page() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html")],
        include_str!("sessions.html"),
    )
        .into_response()
}

async fn list(State(sites): State<Arc<Sites>>) -> Result<Json<Vec<Row>>, AppError> {
    let record = sites.hub.model_record();
    let mut rows = Vec::new();
    for (session, user) in sites.hub.sessions()? {
        let id = session.id.to_string();
        let dir = session_dir(&sites.root, &id);
        if !dir.is_dir() {
            continue;
        }
        let meta = SessionMeta::load_or_create(&dir, || older_session(&session, user))
            .map_err(|e| e.to_string())?;
        rows.push(row(id, &dir, meta, record.as_ref())?);
    }
    let loose = session_dir(&sites.root, NO_SESSION);
    if clip_count(&loose) > 0 {
        let meta =
            SessionMeta::load_or_create(&loose, SessionMeta::default).map_err(|e| e.to_string())?;
        rows.push(row(NO_SESSION.into(), &loose, meta, record.as_ref())?);
    }
    Ok(Json(rows))
}

/// What the store knows of a session from before `session.toml`.
fn older_session(session: &Session, user: String) -> SessionMeta {
    SessionMeta {
        started: Some(tracker::session_meta::sqlite_to_rfc3339(&session.started)),
        user: Some(user),
        activity: Some(session.activity.to_string()),
        ..SessionMeta::default()
    }
}

fn row(
    id: String,
    dir: &FsPath,
    meta: SessionMeta,
    record: Option<&ModelRecord>,
) -> Result<Row, String> {
    Ok(Row {
        trained: record.is_some_and(|r| r.trained_on(&id)),
        id,
        clips: clip_count(dir),
        reviewed: crate::label::reviewed_clips(dir)?,
        meta,
    })
}

/// Change any of a session's tags, notes, and training choice.
async fn update(
    State(sites): State<Arc<Sites>>,
    Path(id): Path<String>,
    Json(change): Json<MetaUpdate>,
) -> Response {
    let Some(dir) = resolve(&sites.root, &id) else {
        return (StatusCode::NOT_FOUND, "no such session").into_response();
    };
    let reviewed = match crate::label::reviewed_clips(&dir) {
        Ok(n) => n,
        Err(e) => return AppError(e).into_response(),
    };
    let mut meta = match SessionMeta::load(&dir) {
        Ok(meta) => meta,
        Err(e) => return AppError(e.to_string()).into_response(),
    };
    if let Err(e) = meta.apply(change, reviewed) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    match meta
        .save(&dir)
        .map_err(|e| e.to_string())
        .and_then(|()| row(id, &dir, meta, sites.hub.model_record().as_ref()))
    {
        Ok(row) => Json(row).into_response(),
        Err(e) => AppError(e).into_response(),
    }
}

fn clip_count(dir: &FsPath) -> usize {
    tracker::DataDir::open(dir)
        .and_then(|d| d.clip_ids())
        .map_or(0, |ids| ids.len())
}

fn session_dir(root: &FsPath, id: &str) -> PathBuf {
    root.join("sessions").join(id)
}

/// The folder an id names: a session number or `none`, with a folder.
fn resolve(root: &FsPath, id: &str) -> Option<PathBuf> {
    if id != NO_SESSION {
        id.parse::<i64>().ok()?;
    }
    let dir = session_dir(root, id);
    dir.is_dir().then_some(dir)
}

/// The pages use relative paths, so they need the trailing slash.
async fn add_slash(Path((id, page)): Path<(String, String)>) -> Redirect {
    Redirect::permanent(&format!("/sessions/{id}/{page}/"))
}

async fn forward_index(
    State(sites): State<Arc<Sites>>,
    Path((id, page)): Path<(String, String)>,
    req: Request,
) -> Response {
    dispatch(&sites, id, page, "", req).await
}

async fn forward(
    State(sites): State<Arc<Sites>>,
    Path((id, page, rest)): Path<(String, String, String)>,
    req: Request,
) -> Response {
    dispatch(&sites, id, page, &rest, req).await
}

async fn dispatch(sites: &Sites, id: String, page: String, rest: &str, req: Request) -> Response {
    let router = match site(sites, id, page) {
        Ok(Some(router)) => router,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such page").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let path = match req.uri().query() {
        Some(q) => format!("/{rest}?{q}"),
        None => format!("/{rest}"),
    };
    let uri = match path.parse::<Uri>() {
        Ok(uri) => uri,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    // A fresh request, so the outer route's path parameters, kept in the
    // extensions, do not reach the inner router's extractors.
    let (parts, body) = req.into_parts();
    let mut inner = Request::new(body);
    *inner.method_mut() = parts.method;
    *inner.uri_mut() = uri;
    *inner.version_mut() = parts.version;
    *inner.headers_mut() = parts.headers;
    match router.oneshot(inner).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

fn site(sites: &Sites, id: String, page: String) -> Result<Option<Router>, String> {
    let mut routers = sites.routers.lock().map_err(|e| e.to_string())?;
    let key = (page, id);
    if let Some(router) = routers.get(&key) {
        return Ok(Some(router.clone()));
    }
    let Some(dir) = resolve(&sites.root, &key.1) else {
        return Ok(None);
    };
    let router = match key.0.as_str() {
        "label" => crate::label::router(&dir)?,
        "playback" => crate::review::live_router(&dir)?,
        _ => return Ok(None),
    };
    routers.insert(key, router.clone());
    Ok(Some(router))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_resolve_only_to_folders_under_the_root() {
        let root = std::env::temp_dir().join(format!("sessions-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sessions/3")).unwrap();
        assert_eq!(resolve(&root, "none"), None);
        std::fs::create_dir_all(root.join("sessions/none")).unwrap();
        assert_eq!(resolve(&root, "none"), Some(root.join("sessions/none")));
        assert_eq!(resolve(&root, "3"), Some(root.join("sessions/3")));
        assert_eq!(resolve(&root, "4"), None);
        assert_eq!(resolve(&root, ".."), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
