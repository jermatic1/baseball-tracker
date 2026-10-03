use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::jpeg::encode_gray_jpeg;

const MAX_GAP: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Mark {
    clip: String,
    index: usize,
    kind: String,
    cx: Option<f64>,
    cy: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LabelFile {
    box_px: u32,
    marks: Vec<Mark>,
}

#[derive(Debug, Clone, Serialize)]
struct Drawn {
    clip: String,
    index: usize,
    kind: String,
    cx: f64,
    cy: f64,
    filled: bool,
}

struct LabelState {
    session: tracker::Session,
    path: PathBuf,
    labels: LabelFile,
    cached: Option<(String, tracker::Clip)>,
}

pub async fn label(path: PathBuf, bind: SocketAddr) -> Result<(), String> {
    let session = tracker::Session::open(&path).map_err(|e| e.to_string())?;
    let file = session.dir.join("labels.json");
    let labels = read_labels(&file)?;
    let state = Arc::new(Mutex::new(LabelState {
        session,
        path: file,
        labels,
        cached: None,
    }));
    let app = Router::new()
        .route("/", get(index))
        .route("/api/session", get(session_api))
        .route("/api/clips/{id}", get(clip_api))
        .route("/api/clips/{id}/frame/{index}", get(frame_api))
        .route("/api/labels", get(labels_api).post(save_label))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("http://{bind}");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

fn read_labels(path: &std::path::Path) -> Result<LabelFile, String> {
    if !path.exists() {
        return Ok(LabelFile {
            box_px: 32,
            marks: Vec::new(),
        });
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn write_labels(path: &std::path::Path, labels: &LabelFile) -> Result<(), String> {
    let text = serde_json::to_string_pretty(labels).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
}

fn interpolate(marks: &[Mark]) -> Vec<Drawn> {
    let mut out = Vec::new();
    let mut clips: Vec<&str> = marks.iter().map(|m| m.clip.as_str()).collect();
    clips.sort_unstable();
    clips.dedup();
    for clip in clips {
        let mut rows: Vec<&Mark> = marks.iter().filter(|m| m.clip == clip).collect();
        rows.sort_by_key(|m| m.index);
        for mark in &rows {
            if mark.kind == "ball" {
                if let (Some(cx), Some(cy)) = (mark.cx, mark.cy) {
                    out.push(Drawn {
                        clip: clip.to_string(),
                        index: mark.index,
                        kind: "ball".into(),
                        cx,
                        cy,
                        filled: false,
                    });
                }
            }
        }
        for pair in rows.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.kind != "ball" || b.kind != "ball" {
                continue;
            }
            let gap = b.index.saturating_sub(a.index);
            if !(2..=MAX_GAP).contains(&gap) {
                continue;
            }
            let blocked = rows
                .iter()
                .any(|m| m.kind == "empty" && m.index > a.index && m.index < b.index);
            if blocked {
                continue;
            }
            let (Some(ax), Some(ay)) = (a.cx, a.cy) else {
                continue;
            };
            let (Some(bx), Some(by)) = (b.cx, b.cy) else {
                continue;
            };
            for index in (a.index + 1)..b.index {
                let t = (index - a.index) as f64 / gap as f64;
                out.push(Drawn {
                    clip: clip.to_string(),
                    index,
                    kind: "ball".into(),
                    cx: ax + (bx - ax) * t,
                    cy: ay + (by - ay) * t,
                    filled: true,
                });
            }
        }
    }
    out
}

async fn index() -> Html<&'static str> {
    Html(include_str!("label.html"))
}

async fn session_api(
    State(state): State<Arc<Mutex<LabelState>>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let st = state.lock().map_err(|e| e.to_string())?;
    let clips = st.session.clip_ids().map_err(|e| e.to_string())?;
    Ok(Json(serde_json::json!({ "clips": clips })))
}

async fn clip_api(
    Path(id): Path<String>,
    State(state): State<Arc<Mutex<LabelState>>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut st = state.lock().map_err(|e| e.to_string())?;
    let clip = cached_clip(&mut st, &id)?;
    Ok(Json(serde_json::json!({
        "meta": { "frames": clip.meta.frame_count }
    })))
}

async fn frame_api(
    Path((id, index)): Path<(String, usize)>,
    State(state): State<Arc<Mutex<LabelState>>>,
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

async fn labels_api(
    State(state): State<Arc<Mutex<LabelState>>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let st = state.lock().map_err(|e| e.to_string())?;
    let drawn = interpolate(&st.labels.marks);
    let mut labels = Vec::new();
    for mark in &st.labels.marks {
        if mark.kind == "empty" {
            labels.push(serde_json::json!({
                "clip": mark.clip,
                "index": mark.index,
                "kind": "empty",
                "filled": false,
            }));
        }
    }
    for box_ in drawn {
        labels.push(serde_json::to_value(box_).map_err(|e| e.to_string())?);
    }
    Ok(Json(serde_json::json!({
        "box": st.labels.box_px,
        "labels": labels,
    })))
}

#[derive(Deserialize)]
struct SaveBody {
    clip: String,
    index: usize,
    kind: String,
    cx: Option<f64>,
    cy: Option<f64>,
    box_px: Option<u32>,
    #[serde(rename = "box")]
    box_alias: Option<u32>,
}

async fn save_label(
    State(state): State<Arc<Mutex<LabelState>>>,
    Json(body): Json<SaveBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut st = state.lock().map_err(|e| e.to_string())?;
    if let Some(px) = body.box_px.or(body.box_alias) {
        st.labels.box_px = px.clamp(8, 80);
    }
    st.labels
        .marks
        .retain(|m| !(m.clip == body.clip && m.index == body.index));
    if body.kind != "clear" {
        st.labels.marks.push(Mark {
            clip: body.clip,
            index: body.index,
            kind: body.kind,
            cx: body.cx,
            cy: body.cy,
        });
    }
    write_labels(&st.path, &st.labels)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn cached_clip<'a>(st: &'a mut LabelState, id: &str) -> Result<&'a tracker::Clip, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ball(index: usize, x: f64) -> Mark {
        Mark {
            clip: "0001".into(),
            index,
            kind: "ball".into(),
            cx: Some(x),
            cy: Some(10.0),
        }
    }

    #[test]
    fn fills_a_short_gap() {
        let drawn = interpolate(&[ball(0, 0.0), ball(4, 40.0)]);
        let filled: Vec<_> = drawn.iter().filter(|d| d.filled).map(|d| d.index).collect();
        assert_eq!(filled, vec![1, 2, 3]);
        assert!((drawn.iter().find(|d| d.index == 2).unwrap().cx - 20.0).abs() < 1e-6);
    }

    #[test]
    fn empty_breaks_the_span() {
        let marks = vec![
            ball(0, 0.0),
            Mark {
                clip: "0001".into(),
                index: 2,
                kind: "empty".into(),
                cx: None,
                cy: None,
            },
            ball(4, 40.0),
        ];
        assert!(interpolate(&marks).iter().all(|d| !d.filled));
    }
}
