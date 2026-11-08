use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::jpeg::encode_gray_jpeg;
use crate::label_tracks::{clip_tracks, Point};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Mark {
    clip: String,
    index: usize,
    kind: String,
    cx: Option<f64>,
    cy: Option<f64>,
    /// Detector class; absent means `ball`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cls: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LabelFile {
    box_px: u32,
    marks: Vec<Mark>,
    /// Clips with no ball anywhere; training takes background frames from them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    empty_clips: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reviewed_tracks: Vec<Reviewed>,
}

/// A track the user accepted or rejected, keyed by clip, frame range, and
/// first centre (two resting balls can share a range).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reviewed {
    clip: String,
    start: usize,
    end: usize,
    cx: f64,
    cy: f64,
    verdict: String,
}

impl Reviewed {
    fn matches(&self, clip: &str, start: usize, end: usize, cx: f64, cy: f64) -> bool {
        self.clip == clip
            && self.start == start
            && self.end == end
            && (self.cx - cx).hypot(self.cy - cy) < 1.0
    }
}

struct LabelState {
    session: tracker::DataDir,
    path: PathBuf,
    labels: LabelFile,
    cached: Option<(String, tracker::Clip)>,
}

pub async fn label(path: PathBuf, bind: SocketAddr) -> Result<(), String> {
    let app = crate::web::standalone("label", router(&path)?);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| e.to_string())?;
    println!("http://{bind}");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

/// The label page's API over `dir`'s clips, writing `dir/labels.json`,
/// under `/api/`, for serving under a session's prefix.
pub fn router(dir: &std::path::Path) -> Result<Router, String> {
    let session = tracker::DataDir::open(dir).map_err(|e| e.to_string())?;
    let file = session.dir.join("labels.json");
    let labels = read_labels(&file)?;
    let state = Arc::new(Mutex::new(LabelState {
        session,
        path: file,
        labels,
        cached: None,
    }));
    Ok(Router::new()
        .route("/api/session", get(session_api))
        .route("/api/clips/{id}", get(clip_api))
        .route("/api/clips/{id}/frame/{index}", get(frame_api))
        .route("/api/clips/{id}/tracks", get(tracks_api))
        .route("/api/tracks/review", post(review_track))
        .route("/api/labels", get(labels_api).post(save_label))
        .route("/api/empty_clips", post(save_empty_clip))
        .with_state(state))
}

/// How many clips in `dir/labels.json` have any label, verdict, or empty
/// mark; zero when there is no file.
pub fn reviewed_clips(dir: &std::path::Path) -> Result<usize, String> {
    let labels = read_labels(&dir.join("labels.json"))?;
    let mut clips: Vec<&str> = labels
        .marks
        .iter()
        .map(|m| m.clip.as_str())
        .chain(labels.empty_clips.iter().map(String::as_str))
        .chain(labels.reviewed_tracks.iter().map(|r| r.clip.as_str()))
        .collect();
    clips.sort_unstable();
    clips.dedup();
    Ok(clips.len())
}

fn read_labels(path: &std::path::Path) -> Result<LabelFile, String> {
    if !path.exists() {
        return Ok(LabelFile {
            box_px: 32,
            marks: Vec::new(),
            empty_clips: Vec::new(),
            reviewed_tracks: Vec::new(),
        });
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn write_labels(path: &std::path::Path, labels: &LabelFile) -> Result<(), String> {
    let text = serde_json::to_string_pretty(labels).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| e.to_string())
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
    let labels: Vec<serde_json::Value> = st
        .labels
        .marks
        .iter()
        .filter_map(|m| match (m.kind.as_str(), m.cx, m.cy) {
            ("empty", _, _) => Some(serde_json::json!({
                "clip": m.clip, "index": m.index, "kind": "empty",
            })),
            ("ball", Some(cx), Some(cy)) => Some(serde_json::json!({
                "clip": m.clip, "index": m.index, "kind": "ball", "cx": cx, "cy": cy,
            })),
            _ => None,
        })
        .collect();
    Ok(Json(serde_json::json!({
        "box": st.labels.box_px,
        "labels": labels,
        "empty_clips": st.labels.empty_clips,
    })))
}

#[derive(Deserialize)]
struct SaveBody {
    clip: String,
    index: usize,
    kind: String,
    cx: Option<f64>,
    cy: Option<f64>,
    cls: Option<String>,
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
    let mark = Mark {
        clip: body.clip,
        index: body.index,
        kind: body.kind,
        cx: body.cx,
        cy: body.cy,
        cls: body.cls,
    };
    let near = st.labels.box_px as f64 / 2.0;
    put_mark(&mut st.labels.marks, mark, near);
    write_labels(&st.path, &st.labels)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Put a mark on its frame. A frame holds any number of balls but never two
/// of one class within `near` pixels, which are one object: a ball replaces
/// the one it is near and any empty mark. `empty` replaces everything on the
/// frame. `clear` removes the ball near its centre, or with no centre the
/// frame.
fn put_mark(marks: &mut Vec<Mark>, mark: Mark, near: f64) {
    let class = |m: &Mark| m.cls.clone().unwrap_or_else(|| "ball".into());
    let close = |m: &Mark| match (m.cx, m.cy, mark.cx, mark.cy) {
        (Some(a), Some(b), Some(x), Some(y)) => {
            class(m) == class(&mark) && (a - x).hypot(b - y) < near
        }
        _ => false,
    };
    let gone = |m: &Mark| match (mark.kind.as_str(), mark.cx.is_some()) {
        ("ball", true) => m.kind != "ball" || close(m),
        ("clear", true) => m.kind == "ball" && close(m),
        _ => true,
    };
    marks.retain(|m| !(m.clip == mark.clip && m.index == mark.index && gone(m)));
    if mark.kind != "clear" {
        marks.push(mark);
    }
}

async fn tracks_api(
    Path(id): Path<String>,
    State(state): State<Arc<Mutex<LabelState>>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut st = state.lock().map_err(|e| e.to_string())?;
    let (width, height) = {
        let clip = cached_clip(&mut st, &id)?;
        (clip.meta.width, clip.meta.height)
    };
    let frames = match st.session.load_detections(&id) {
        Ok(dets) => dets.frames,
        Err(_) => Vec::new(),
    };
    let depth = tracker::units::metres(st.session.config.mount.distance);
    let mut tracks = clip_tracks(&frames, width, height, depth);
    for t in &mut tracks {
        let p = &t.points[0];
        t.verdict = st
            .labels
            .reviewed_tracks
            .iter()
            .find(|r| r.matches(&id, t.start, t.end, p.cx, p.cy))
            .map(|r| r.verdict.clone());
    }
    Ok(Json(serde_json::json!({ "tracks": tracks })))
}

#[derive(Deserialize)]
struct ReviewBody {
    clip: String,
    /// `ball` writes a mark per point; `reject` writes none.
    verdict: String,
    points: Vec<Point>,
}

async fn review_track(
    State(state): State<Arc<Mutex<LabelState>>>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut st = state.lock().map_err(|e| e.to_string())?;
    apply_review(&mut st.labels, body)?;
    write_labels(&st.path, &st.labels)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Record a verdict on a track. Rejecting a track accepted earlier removes the
/// ball marks it wrote, left where they still sit on its points.
fn apply_review(labels: &mut LabelFile, body: ReviewBody) -> Result<(), String> {
    if body.verdict != "ball" && body.verdict != "reject" {
        return Err(format!("unknown verdict {}", body.verdict));
    }
    let (Some(first), Some(last)) = (body.points.first(), body.points.last()) else {
        return Err("track has no points".into());
    };
    let key = Reviewed {
        clip: body.clip.clone(),
        start: first.index,
        end: last.index,
        cx: first.cx,
        cy: first.cy,
        verdict: body.verdict.clone(),
    };
    labels
        .reviewed_tracks
        .retain(|r| !r.matches(&key.clip, key.start, key.end, key.cx, key.cy));
    labels.reviewed_tracks.push(key);
    let near = labels.box_px as f64 / 2.0;
    for p in &body.points {
        if body.verdict == "ball" {
            put_mark(
                &mut labels.marks,
                Mark {
                    clip: body.clip.clone(),
                    index: p.index,
                    kind: "ball".into(),
                    cx: Some(p.cx),
                    cy: Some(p.cy),
                    cls: None,
                },
                near,
            );
        } else {
            labels.marks.retain(|m| {
                let on_point = m.clip == body.clip
                    && m.index == p.index
                    && m.kind == "ball"
                    && matches!((m.cx, m.cy), (Some(x), Some(y)) if (x - p.cx).hypot(y - p.cy) < 0.5);
                !on_point
            });
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct EmptyClipBody {
    clip: String,
    empty: bool,
}

async fn save_empty_clip(
    State(state): State<Arc<Mutex<LabelState>>>,
    Json(body): Json<EmptyClipBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut st = state.lock().map_err(|e| e.to_string())?;
    set_empty_clip(&mut st.labels.empty_clips, body.clip, body.empty);
    write_labels(&st.path, &st.labels)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn set_empty_clip(clips: &mut Vec<String>, clip: String, empty: bool) {
    clips.retain(|c| *c != clip);
    if empty {
        clips.push(clip);
        clips.sort_unstable();
    }
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
            cls: None,
        }
    }

    #[test]
    fn a_frame_holds_several_balls_but_one_box_per_ball() {
        let mut marks = vec![ball(3, 100.0)];
        put_mark(&mut marks, ball(3, 300.0), 16.0);
        put_mark(&mut marks, ball(3, 305.0), 16.0);
        let xs: Vec<_> = marks.iter().map(|m| m.cx).collect();
        assert_eq!(xs, [Some(100.0), Some(305.0)]);
        let clear = Mark {
            kind: "clear".into(),
            ..ball(3, 98.0)
        };
        put_mark(&mut marks, clear, 16.0);
        assert_eq!(marks, [ball(3, 305.0)]);
        let empty = Mark {
            kind: "empty".into(),
            cx: None,
            cy: None,
            ..ball(3, 0.0)
        };
        put_mark(&mut marks, empty.clone(), 16.0);
        assert_eq!(marks, [empty]);
        put_mark(&mut marks, ball(3, 50.0), 16.0);
        assert_eq!(marks, [ball(3, 50.0)]);
    }

    #[test]
    fn empty_clips_toggle_and_stay_sorted() {
        let mut clips = Vec::new();
        set_empty_clip(&mut clips, "0044".into(), true);
        set_empty_clip(&mut clips, "0002".into(), true);
        set_empty_clip(&mut clips, "0044".into(), true);
        assert_eq!(clips, vec!["0002", "0044"]);
        set_empty_clip(&mut clips, "0002".into(), false);
        assert_eq!(clips, vec!["0044"]);
    }

    #[test]
    fn accepting_then_rejecting_a_track_writes_then_removes_its_marks() {
        let mut labels = LabelFile {
            box_px: 32,
            marks: vec![ball(9, 5.0)],
            empty_clips: Vec::new(),
            reviewed_tracks: Vec::new(),
        };
        let review = |verdict: &str| ReviewBody {
            clip: "0001".into(),
            verdict: verdict.into(),
            points: (3..6)
                .map(|index| Point {
                    index,
                    cx: index as f64,
                    cy: 10.0,
                })
                .collect(),
        };
        apply_review(&mut labels, review("ball")).unwrap();
        assert_eq!(labels.marks.len(), 4);
        assert_eq!(labels.marks[3], ball(5, 5.0));
        apply_review(&mut labels, review("reject")).unwrap();
        assert_eq!(labels.marks, vec![ball(9, 5.0)]);
        assert_eq!(labels.reviewed_tracks.len(), 1);
        assert_eq!(labels.reviewed_tracks[0].verdict, "reject");
    }

    #[test]
    fn reviewed_clips_counts_each_clip_once() {
        let dir = std::env::temp_dir().join(format!("label-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(reviewed_clips(&dir).unwrap(), 0);
        std::fs::write(
            dir.join("labels.json"),
            r#"{"box_px":32,"marks":[{"clip":"0001","index":0,"kind":"ball","cx":1,"cy":1},{"clip":"0001","index":1,"kind":"empty","cx":null,"cy":null}],"empty_clips":["0002","0003"],"reviewed_tracks":[{"clip":"0003","start":0,"end":4,"cx":1,"cy":1,"verdict":"reject"},{"clip":"0004","start":0,"end":4,"cx":1,"cy":1,"verdict":"ball"}]}"#,
        )
        .unwrap();
        assert_eq!(reviewed_clips(&dir).unwrap(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_label_files_still_load() {
        let file: LabelFile =
            serde_json::from_str(r#"{"box_px":32,"marks":[{"clip":"0001","index":0,"kind":"empty","cx":null,"cy":null}]}"#)
                .unwrap();
        assert!(file.empty_clips.is_empty());
        let text = serde_json::to_string(&file).unwrap();
        assert!(!text.contains("cls") && !text.contains("empty_clips"));
        assert!(!text.contains("reviewed_tracks"));
    }
}
