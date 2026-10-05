use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::convert::{config_summary, print_hit};
use crate::error::AppError;
use crate::jpeg::encode_gray_jpeg;

struct ReviewState {
    session: tracker::Session,
    hits: Vec<tracker::HitRecord>,
    cached: Option<(String, tracker::Clip)>,
}

pub async fn review(path: PathBuf, bind: SocketAddr) -> Result<(), String> {
    let session = tracker::Session::open(&path).map_err(|e| e.to_string())?;
    let hits = tracker::recompute_hits(&session).map_err(|e| e.to_string())?;
    session.write_hits(&hits).map_err(|e| e.to_string())?;
    for hit in &hits {
        print_hit(hit);
    }
    let state = Arc::new(Mutex::new(ReviewState {
        session,
        hits,
        cached: None,
    }));
    let app = Router::new()
        .route("/", get(index))
        .route("/api/session", get(session_api))
        .route("/api/clips/{id}", get(clip_api))
        .route("/api/clips/{id}/frame/{index}", get(frame_api))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("http://{bind}");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("review.html"))
}

async fn session_api(
    State(state): State<Arc<Mutex<ReviewState>>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let st = state.lock().map_err(|e| e.to_string())?;
    let clips = st.session.clip_ids().map_err(|e| e.to_string())?;
    let hits: Vec<_> = st.hits.iter().map(hit_json).collect();
    Ok(Json(serde_json::json!({
        "clips": clips,
        "hits": hits,
        "config": config_summary(&st.session.config),
    })))
}

async fn clip_api(
    Path(id): Path<String>,
    State(state): State<Arc<Mutex<ReviewState>>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut st = state.lock().map_err(|e| e.to_string())?;
    let clip = cached_clip(&mut st, &id)?;
    let meta = serde_json::json!({
        "id": id,
        "frames": clip.meta.frame_count,
        "width": clip.meta.width,
        "height": clip.meta.height,
        "fps": clip.meta.fps,
        "exposure_us": clip.meta.exposure_us,
        "gain": clip.meta.gain,
        "ir_flood": clip.meta.ir_flood,
        "ir_dot": clip.meta.ir_dot,
        "sequence_gaps": clip.meta.sequence_gaps,
    });
    let detections = st.session.load_detections(&id).ok();
    let events: Vec<_> = st
        .hits
        .iter()
        .filter(|h| h.clip == id)
        .map(hit_json)
        .collect();
    let hit = st
        .hits
        .iter()
        .find(|h| h.clip == id && h.kind == tracker::EventKind::Hit)
        .map(hit_json);
    Ok(Json(serde_json::json!({
        "meta": meta,
        "detections": detections,
        "events": events,
        "hit": hit,
    })))
}

async fn frame_api(
    Path((id, index)): Path<(String, usize)>,
    State(state): State<Arc<Mutex<ReviewState>>>,
) -> Result<Response, AppError> {
    let (width, height, left) = {
        let mut st = state.lock().map_err(|e| e.to_string())?;
        let clip = cached_clip(&mut st, &id)?;
        if index >= clip.meta.frame_count {
            return Ok(StatusCode::NOT_FOUND.into_response());
        }
        let left = clip.left_frame(index).map_err(|e| e.to_string())?;
        (clip.meta.width, clip.meta.height, left)
    };
    let bytes = encode_gray_jpeg(width, height, &left)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

fn hit_json(hit: &tracker::HitRecord) -> serde_json::Value {
    serde_json::to_value(hit).unwrap_or(serde_json::Value::Null)
}

fn cached_clip<'a>(st: &'a mut ReviewState, id: &str) -> Result<&'a tracker::Clip, String> {
    let miss = match &st.cached {
        Some((cached, _)) => cached != id,
        None => true,
    };
    if miss {
        let clip = st.session.load_clip(id).map_err(|e| e.to_string())?;
        st.cached = Some((id.to_string(), clip));
    }
    Ok(&st.cached.as_ref().unwrap().1)
}
