//! The YOLO dataset built from labelled sessions, laid out and sampled as
//! `tools/train.py` builds it, so a dataset made here trains the same:
//! clicked balls plus straight-line fills between them, frames marked empty,
//! and a few frames from each clip marked as having no ball.
//!
//! Sessions marked `include` give `train/` (every fifth frame to `val`);
//! those marked `holdout` give `holdout/`, which training never sees. The
//! archive sent to the training server is both, a `manifest.json`, and the
//! running model as `current.onnx`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::session_meta::{SessionMeta, Training, SESSION_FILE};
use crate::TrackerError;

pub const CLASSES: [&str; 1] = ["ball"];
/// Longest run of frames filled between two clicks of one class.
pub const MAX_GAP: usize = 12;
pub const EMPTY_CLIP_STRIDE: usize = 10;
pub const EMPTY_CLIP_CAP: usize = 2;
pub const DEFAULT_BOX_PX: u32 = 32;
pub const MIN_CLICKS: usize = 8;
/// Every fifth frame, counting from the first, is validation.
pub const VAL_EVERY: usize = 5;
/// The id of the folder where clips with no session go.
pub const NO_SESSION: &str = "none";

/// Turns a gray frame into JPEG bytes.
pub type Encode<'a> = &'a dyn Fn(u32, u32, &[u8]) -> Result<Vec<u8>, String>;

/// A session folder by its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRef {
    pub id: String,
    #[serde(skip)]
    pub dir: PathBuf,
}

/// The sessions marked for training and for holding out, in id order with
/// `none` last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chosen {
    pub include: Vec<SessionRef>,
    pub holdout: Vec<SessionRef>,
}

/// What one session adds to a dataset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Frames with a ball box, clicked or filled.
    pub ball: usize,
    /// Frames with no ball.
    pub empty: usize,
    /// Boxes the user placed, not filled.
    pub clicks: usize,
}

/// Frames written to each part of a dataset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Built {
    pub train: usize,
    pub val: usize,
    pub holdout: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Labels {
    box_px: Option<u32>,
    #[serde(rename = "box")]
    box_alias: Option<u32>,
    marks: Vec<Mark>,
    empty_clips: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Mark {
    clip: String,
    index: usize,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    cx: Option<f64>,
    #[serde(default)]
    cy: Option<f64>,
    #[serde(default)]
    cls: Option<String>,
}

impl Mark {
    fn is(&self, kind: &str) -> bool {
        self.kind.as_deref() == Some(kind)
    }

    fn class(&self) -> &str {
        self.cls
            .as_deref()
            .filter(|c| !c.is_empty())
            .unwrap_or(CLASSES[0])
    }
}

#[derive(Debug, Deserialize)]
struct FrameMeta {
    width: u32,
    height: u32,
    frame_count: usize,
}

#[derive(Debug, Deserialize)]
struct DetRows {
    #[serde(default)]
    frames: Vec<DetRow>,
}

#[derive(Debug, Deserialize)]
struct DetRow {
    index: i64,
    #[serde(default)]
    balls: Vec<IgnoredAny>,
}

#[derive(Debug, Clone, PartialEq)]
struct Ball {
    clip: String,
    index: usize,
    cls: String,
    cx: f64,
    cy: f64,
    filled: bool,
}

/// A ball box's class and centre in pixels.
type BallAt = (String, f64, f64);

/// One frame of the dataset; `ball` is `None` for a frame with no ball.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    source: usize,
    clip: String,
    index: usize,
    ball: Option<BallAt>,
    box_px: u32,
    width: u32,
    height: u32,
}

/// The sessions under `root` marked include or holdout in their
/// `session.toml`, the loose clips' folder among them as `none`.
pub fn chosen_sessions(root: &Path) -> Result<Chosen, TrackerError> {
    let mut numbered: Vec<(i64, PathBuf)> = Vec::new();
    let sessions = root.join("sessions");
    if sessions.is_dir() {
        for entry in fs::read_dir(&sessions)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(id) = name.parse::<i64>() {
                if entry.path().join(SESSION_FILE).is_file() {
                    numbered.push((id, entry.path()));
                }
            }
        }
    }
    numbered.sort();
    let mut all: Vec<SessionRef> = numbered
        .into_iter()
        .map(|(id, dir)| SessionRef {
            id: id.to_string(),
            dir,
        })
        .collect();
    let loose = sessions.join(NO_SESSION);
    if loose.join(SESSION_FILE).is_file() {
        all.push(SessionRef {
            id: NO_SESSION.into(),
            dir: loose,
        });
    }
    let mut chosen = Chosen::default();
    for session in all {
        match SessionMeta::load(&session.dir)?.training {
            Training::Include => chosen.include.push(session),
            Training::Holdout => chosen.holdout.push(session),
            Training::None => {}
        }
    }
    Ok(chosen)
}

/// What a session would add, without reading any frames. A session with no
/// `labels.json` adds nothing.
pub fn session_counts(dir: &Path) -> Result<Counts, TrackerError> {
    if !dir.join("labels.json").exists() {
        return Ok(Counts::default());
    }
    let (items, clicks) = session_items(0, dir)?;
    let ball = items.iter().filter(|i| i.ball.is_some()).count();
    Ok(Counts {
        ball,
        empty: items.len() - ball,
        clicks,
    })
}

/// Clicked boxes plus straight-line fills between clicks of one class.
fn interpolate(marks: &[Mark]) -> Vec<Ball> {
    let mut clips: Vec<(&str, Vec<&Mark>)> = Vec::new();
    for mark in marks {
        match clips.iter_mut().find(|(clip, _)| *clip == mark.clip) {
            Some((_, rows)) => rows.push(mark),
            None => clips.push((&mark.clip, vec![mark])),
        }
    }
    let mut out = Vec::new();
    for (clip, mut rows) in clips {
        rows.sort_by_key(|m| m.index);
        let clicks: Vec<&Mark> = rows
            .iter()
            .copied()
            .filter(|m| m.is("ball") && m.cx.is_some() && m.cy.is_some())
            .collect();
        for mark in &clicks {
            out.push(Ball {
                clip: clip.to_string(),
                index: mark.index,
                cls: mark.class().to_string(),
                cx: mark.cx.unwrap_or_default(),
                cy: mark.cy.unwrap_or_default(),
                filled: false,
            });
        }
        let classes: BTreeSet<&str> = clicks.iter().map(|m| m.class()).collect();
        for cls in classes {
            let balls: Vec<&Mark> = clicks
                .iter()
                .copied()
                .filter(|m| m.class() == cls)
                .collect();
            fill_gaps(clip, cls, &rows, &balls, &mut out);
        }
    }
    out
}

fn fill_gaps(clip: &str, cls: &str, rows: &[&Mark], balls: &[&Mark], out: &mut Vec<Ball>) {
    for pair in balls.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let gap = b.index - a.index;
        if !(2..=MAX_GAP).contains(&gap) {
            continue;
        }
        if rows
            .iter()
            .any(|m| m.is("empty") && a.index < m.index && m.index < b.index)
        {
            continue;
        }
        let (ax, ay) = (a.cx.unwrap_or_default(), a.cy.unwrap_or_default());
        let (bx, by) = (b.cx.unwrap_or_default(), b.cy.unwrap_or_default());
        for index in a.index + 1..b.index {
            let t = (index - a.index) as f64 / gap as f64;
            out.push(Ball {
                clip: clip.to_string(),
                index,
                cls: cls.to_string(),
                cx: ax + (bx - ax) * t,
                cy: ay + (by - ay) * t,
                filled: true,
            });
        }
    }
}

/// `cap` items spread evenly over `items`, or all of them.
fn even_sample(items: Vec<usize>, cap: usize) -> Vec<usize> {
    if items.len() <= cap {
        return items;
    }
    (0..cap).map(|k| items[k * items.len() / cap]).collect()
}

/// Frames of a clip with no ball: those the live detector fired on, else
/// every tenth, at most `EMPTY_CLIP_CAP` spread over the clip.
fn empty_clip_frames(clip_dir: &Path, frame_count: usize) -> Result<Vec<usize>, TrackerError> {
    let path = clip_dir.join("detections.json");
    let frames = if path.exists() {
        let rows: DetRows = serde_json::from_slice(&fs::read(path)?)?;
        let mut fired: Vec<usize> = rows
            .frames
            .iter()
            .filter(|r| !r.balls.is_empty() && r.index >= 0 && (r.index as usize) < frame_count)
            .map(|r| r.index as usize)
            .collect();
        fired.sort_unstable();
        fired
    } else {
        (0..frame_count).step_by(EMPTY_CLIP_STRIDE).collect()
    };
    Ok(even_sample(frames, EMPTY_CLIP_CAP))
}

/// One label line; the box is a square of `box_px` pixels.
fn yolo_line(
    cls: &str,
    cx: f64,
    cy: f64,
    box_px: u32,
    width: u32,
    height: u32,
) -> Result<String, TrackerError> {
    let Some(k) = CLASSES.iter().position(|c| *c == cls) else {
        return Err(TrackerError::Other(format!(
            "unknown class {cls:?}; known: {}",
            CLASSES.join(", ")
        )));
    };
    let (w, h, b) = (width as f64, height as f64, box_px as f64);
    Ok(format!(
        "{k} {:.6} {:.6} {:.6} {:.6}\n",
        cx / w,
        cy / h,
        b / w,
        b / h
    ))
}

fn read_meta(session: &Path, clip: &str) -> Result<FrameMeta, TrackerError> {
    let path = session.join("clips").join(clip).join("meta.json");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// The labelled frames of one session, and its count of ball clicks.
fn session_items(source: usize, session: &Path) -> Result<(Vec<Item>, usize), TrackerError> {
    let path = session.join("labels.json");
    if !path.exists() {
        return Err(TrackerError::Other(format!(
            "no labels at {}; label the session first",
            path.display()
        )));
    }
    let labels: Labels = serde_json::from_slice(&fs::read(&path)?)?;
    let box_px = labels
        .box_px
        .filter(|&b| b > 0)
        .or(labels.box_alias.filter(|&b| b > 0))
        .unwrap_or(DEFAULT_BOX_PX);
    let balls = interpolate(&labels.marks);
    let clicks = balls.iter().filter(|b| !b.filled).count();
    let mut rows: Vec<(String, usize, Option<BallAt>)> = balls
        .into_iter()
        .map(|b| (b.clip, b.index, Some((b.cls, b.cx, b.cy))))
        .collect();
    rows.extend(
        labels
            .marks
            .iter()
            .filter(|m| m.is("empty"))
            .map(|m| (m.clip.clone(), m.index, None)),
    );
    let marked: HashSet<(&str, usize)> = labels
        .marks
        .iter()
        .map(|m| (m.clip.as_str(), m.index))
        .collect();
    let mut metas: HashMap<String, FrameMeta> = HashMap::new();
    for clip in &labels.empty_clips {
        let meta = read_meta(session, clip)?;
        let clip_dir = session.join("clips").join(clip);
        for index in empty_clip_frames(&clip_dir, meta.frame_count)? {
            if !marked.contains(&(clip.as_str(), index)) {
                rows.push((clip.clone(), index, None));
            }
        }
        metas.insert(clip.clone(), meta);
    }
    let mut items = Vec::with_capacity(rows.len());
    for (clip, index, ball) in rows {
        if !metas.contains_key(&clip) {
            metas.insert(clip.clone(), read_meta(session, &clip)?);
        }
        let meta = &metas[&clip];
        items.push(Item {
            source,
            width: meta.width,
            height: meta.height,
            clip,
            index,
            ball,
            box_px,
        });
    }
    Ok((items, clicks))
}

fn load_frame(session: &Path, item: &Item) -> Result<Vec<u8>, TrackerError> {
    let n = item.width as usize * item.height as usize;
    let path = session.join("clips").join(&item.clip).join("left.gray");
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start((item.index * n) as u64))?;
    let mut raw = Vec::with_capacity(n);
    f.take(n as u64).read_to_end(&mut raw)?;
    if raw.len() != n {
        return Err(TrackerError::Other(format!(
            "short frame {} {}",
            item.clip, item.index
        )));
    }
    Ok(raw)
}

/// Write `items` as `<root>/images/<name>/*.jpg` and `<root>/labels/<name>/*.txt`;
/// an empty `name` writes straight into `images/` and `labels/`.
fn write_split(
    root: &Path,
    name: &str,
    items: &[Item],
    sessions: &[SessionRef],
    encode: Encode,
) -> Result<(), TrackerError> {
    let images = root.join("images").join(name);
    let labels = root.join("labels").join(name);
    fs::create_dir_all(&images)?;
    fs::create_dir_all(&labels)?;
    for item in items {
        let stem = format!("s{}_{}_{:05}", item.source, item.clip, item.index);
        let raw = load_frame(&sessions[item.source].dir, item)?;
        let jpeg = encode(item.width, item.height, &raw).map_err(TrackerError::Other)?;
        fs::write(images.join(format!("{stem}.jpg")), jpeg)?;
        let text = match &item.ball {
            Some((cls, cx, cy)) => yolo_line(cls, *cx, *cy, item.box_px, item.width, item.height)?,
            None => String::new(),
        };
        fs::write(labels.join(format!("{stem}.txt")), text)?;
    }
    Ok(())
}

/// Every session's items in (session, clip, frame) order, and the clicks.
fn gather(sessions: &[SessionRef]) -> Result<(Vec<Item>, usize), TrackerError> {
    let mut items = Vec::new();
    let mut clicks = 0;
    for (source, session) in sessions.iter().enumerate() {
        let (got, n) = session_items(source, &session.dir)?;
        items.extend(got);
        clicks += n;
    }
    items.sort_by(|a, b| (a.source, &a.clip, a.index).cmp(&(b.source, &b.clip, b.index)));
    Ok((items, clicks))
}

fn split(items: Vec<Item>) -> (Vec<Item>, Vec<Item>) {
    let (mut val, mut train) = (Vec::new(), Vec::new());
    for (i, item) in items.into_iter().enumerate() {
        if i % VAL_EVERY == 0 {
            val.push(item);
        } else {
            train.push(item);
        }
    }
    if val.is_empty() {
        val.extend(train.first().cloned());
    }
    (train, val)
}

/// Write `out/train/` from the included sessions and `out/holdout/` from the
/// held-out ones.
pub fn write_dataset(out: &Path, chosen: &Chosen, encode: Encode) -> Result<Built, TrackerError> {
    let (items, clicks) = gather(&chosen.include)?;
    if clicks < MIN_CLICKS {
        return Err(TrackerError::Other(format!(
            "need at least {MIN_CLICKS} ball clicks before training; the included sessions have {clicks}"
        )));
    }
    let (train, val) = split(items);
    let root = out.join("train");
    for generated in ["images", "labels"] {
        let _ = fs::remove_dir_all(root.join(generated));
    }
    write_split(&root, "train", &train, &chosen.include, encode)?;
    write_split(&root, "val", &val, &chosen.include, encode)?;
    let names: String = CLASSES
        .iter()
        .enumerate()
        .map(|(k, name)| format!("  {k}: {name}\n"))
        .collect();
    fs::write(
        root.join("data.yaml"),
        format!(
            "path: {}\ntrain: images/train\nval: images/val\nnames:\n{names}",
            root.display()
        ),
    )?;
    let mut built = Built {
        train: train.len(),
        val: val.len(),
        holdout: 0,
    };
    if !chosen.holdout.is_empty() {
        let (held, _) = gather(&chosen.holdout)?;
        let dir = out.join("holdout");
        write_split(&dir, "", &held, &chosen.holdout, encode)?;
        built.holdout = held.len();
    }
    Ok(built)
}

#[derive(Serialize)]
struct Manifest<'a> {
    sessions: Vec<&'a str>,
    holdout: Vec<&'a str>,
    frames: usize,
    created: String,
}

/// The dataset as the gzipped tar the training server takes, with `current`
/// as `current.onnx` when it exists.
pub fn build_archive(
    chosen: &Chosen,
    encode: Encode,
    current: Option<&Path>,
) -> Result<(Vec<u8>, Built), TrackerError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = std::env::temp_dir().join(format!("tracker-dataset-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&tmp)?;
    let result = pack(&tmp, chosen, encode, current);
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn pack(
    tmp: &Path,
    chosen: &Chosen,
    encode: Encode,
    current: Option<&Path>,
) -> Result<(Vec<u8>, Built), TrackerError> {
    let built = write_dataset(tmp, chosen, encode)?;
    let manifest = Manifest {
        sessions: chosen.include.iter().map(|s| s.id.as_str()).collect(),
        holdout: chosen.holdout.iter().map(|s| s.id.as_str()).collect(),
        frames: built.train + built.val,
        created: rfc3339_now(),
    };
    fs::write(
        tmp.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.append_path_with_name(tmp.join("manifest.json"), "manifest.json")?;
    tar.append_dir_all("train", tmp.join("train"))?;
    if tmp.join("holdout").is_dir() {
        tar.append_dir_all("holdout", tmp.join("holdout"))?;
    }
    if let Some(model) = current.filter(|p| p.is_file()) {
        tar.append_path_with_name(model, "current.onnx")?;
    }
    let bytes = tar.into_inner()?.finish()?;
    Ok((bytes, built))
}

/// Now, UTC, as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rfc3339(secs)
}

fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 640;
    const H: u32 = 400;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dataset-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clip(session: &Path, id: &str, frames: usize, fired: Option<&[usize]>) {
        let dir = session.join("clips").join(id);
        fs::create_dir_all(&dir).unwrap();
        let meta = serde_json::json!({"width": W, "height": H, "frame_count": frames});
        fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
        let pixels: Vec<u8> = (0..frames)
            .flat_map(|i| vec![i as u8; (W * H) as usize])
            .collect();
        fs::write(dir.join("left.gray"), pixels).unwrap();
        if let Some(fired) = fired {
            let rows: Vec<_> = (0..frames)
                .map(|k| serde_json::json!({"index": k, "balls": if fired.contains(&k) { vec![serde_json::json!({})] } else { vec![] }}))
                .collect();
            let dets = serde_json::json!({ "frames": rows });
            fs::write(dir.join("detections.json"), dets.to_string()).unwrap();
        }
    }

    /// Clip 0001: clicks at 0 and 4 (filled 1..3), and 6 and 9 with an empty
    /// mark at 7 between them (not filled). Clip 0002: clicks 0..=5 in a row.
    /// Clip 0003 is empty with the detector firing on 1, 3, 5, 7; clip 0004 is
    /// empty with no detections and an empty mark on frame 10.
    fn session(dir: &Path) {
        clip(dir, "0001", 10, None);
        clip(dir, "0002", 6, None);
        clip(dir, "0003", 8, Some(&[1, 3, 5, 7]));
        clip(dir, "0004", 25, None);
        let mut marks = vec![
            serde_json::json!({"clip": "0001", "index": 4, "kind": "ball", "cx": 40.0, "cy": 10.0}),
            serde_json::json!({"clip": "0001", "index": 0, "kind": "ball", "cx": 0.0, "cy": 10.0}),
            serde_json::json!({"clip": "0001", "index": 6, "kind": "ball", "cx": 100.5, "cy": 200.25}),
            serde_json::json!({"clip": "0001", "index": 7, "kind": "empty", "cx": null, "cy": null}),
            serde_json::json!({"clip": "0001", "index": 9, "kind": "ball", "cx": 130.0, "cy": 210.0}),
            serde_json::json!({"clip": "0004", "index": 10, "kind": "empty", "cx": null, "cy": null}),
        ];
        for k in 0..6 {
            marks.push(serde_json::json!({"clip": "0002", "index": k, "kind": "ball", "cx": 320.0 + k as f64, "cy": 5.0, "cls": "ball"}));
        }
        let labels =
            serde_json::json!({"box_px": 24, "marks": marks, "empty_clips": ["0003", "0004"]});
        fs::write(dir.join("labels.json"), labels.to_string()).unwrap();
    }

    fn fake_jpeg(_: u32, _: u32, pixels: &[u8]) -> Result<Vec<u8>, String> {
        Ok(vec![pixels[0]])
    }

    fn stems(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn interpolation_and_sampling_follow_train_py() {
        let dir = temp("items");
        session(&dir);
        let (items, clicks) = session_items(0, &dir).unwrap();
        assert_eq!(clicks, 10);
        let ball: Vec<(&str, usize)> = items
            .iter()
            .filter(|i| i.ball.is_some())
            .map(|i| (i.clip.as_str(), i.index))
            .collect();
        assert_eq!(
            ball,
            [
                ("0001", 0),
                ("0001", 4),
                ("0001", 6),
                ("0001", 9),
                ("0001", 1),
                ("0001", 2),
                ("0001", 3),
                ("0001", 5),
                ("0002", 0),
                ("0002", 1),
                ("0002", 2),
                ("0002", 3),
                ("0002", 4),
                ("0002", 5),
            ]
        );
        let empty: Vec<(&str, usize)> = items
            .iter()
            .filter(|i| i.ball.is_none())
            .map(|i| (i.clip.as_str(), i.index))
            .collect();
        // Detector frames 1 and 5 of 0003; every tenth of 0004 but the marked 10.
        assert_eq!(
            empty,
            [
                ("0001", 7),
                ("0004", 10),
                ("0003", 1),
                ("0003", 5),
                ("0004", 0)
            ]
        );
        assert_eq!(even_sample((0..100).collect(), 4), [0, 25, 50, 75]);
        assert_eq!(even_sample(vec![3, 7], 40), [3, 7]);
        assert_eq!(
            session_counts(&dir).unwrap(),
            Counts {
                ball: 14,
                empty: 5,
                clicks: 10
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The split and label files `tools/train.py` writes for the same session.
    #[test]
    fn dataset_matches_train_py() {
        let root = temp("build");
        let dir = root.join("sessions/3");
        session(&dir);
        let chosen = Chosen {
            include: vec![SessionRef {
                id: "3".into(),
                dir: dir.clone(),
            }],
            holdout: vec![SessionRef {
                id: "3".into(),
                dir: dir.clone(),
            }],
        };
        let out = root.join("out");
        let built = write_dataset(&out, &chosen, &fake_jpeg).unwrap();
        assert_eq!(
            built,
            Built {
                train: 15,
                val: 4,
                holdout: 19
            }
        );
        // Printed by train.py's build_dataset on this session.
        let want = [
            (
                "train",
                "s0_0001_00001",
                "0 0.015625 0.025000 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0001_00002",
                "0 0.031250 0.025000 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0001_00003",
                "0 0.046875 0.025000 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0001_00004",
                "0 0.062500 0.025000 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0001_00006",
                "0 0.157031 0.500625 0.037500 0.060000\n",
            ),
            ("train", "s0_0001_00007", ""),
            (
                "train",
                "s0_0001_00009",
                "0 0.203125 0.525000 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0002_00000",
                "0 0.500000 0.012500 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0002_00002",
                "0 0.503125 0.012500 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0002_00003",
                "0 0.504687 0.012500 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0002_00004",
                "0 0.506250 0.012500 0.037500 0.060000\n",
            ),
            (
                "train",
                "s0_0002_00005",
                "0 0.507812 0.012500 0.037500 0.060000\n",
            ),
            ("train", "s0_0003_00005", ""),
            ("train", "s0_0004_00000", ""),
            ("train", "s0_0004_00010", ""),
            (
                "val",
                "s0_0001_00000",
                "0 0.000000 0.025000 0.037500 0.060000\n",
            ),
            (
                "val",
                "s0_0001_00005",
                "0 0.109766 0.262813 0.037500 0.060000\n",
            ),
            (
                "val",
                "s0_0002_00001",
                "0 0.501563 0.012500 0.037500 0.060000\n",
            ),
            ("val", "s0_0003_00001", ""),
        ];
        let mut got = Vec::new();
        for split in ["train", "val"] {
            let labels = out.join("train/labels").join(split);
            for name in stems(&labels) {
                let text = fs::read_to_string(labels.join(&name)).unwrap();
                got.push((split, name.trim_end_matches(".txt").to_string(), text));
            }
        }
        let want: Vec<_> = want
            .iter()
            .map(|(split, stem, text)| (*split, stem.to_string(), text.to_string()))
            .collect();
        assert_eq!(got, want);
        let read = |p: &str| fs::read_to_string(out.join(p)).unwrap();
        assert_eq!(
            fs::read(out.join("train/images/train/s0_0001_00002.jpg")).unwrap(),
            [2]
        );
        assert_eq!(stems(&out.join("holdout/labels")).len(), 19);
        assert!(read("train/data.yaml")
            .ends_with("train: images/train\nval: images/val\nnames:\n  0: ball\n"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn too_few_clicks_is_refused() {
        let dir = temp("few");
        clip(&dir, "0001", 4, None);
        let labels = serde_json::json!({"box_px": 32, "marks": [
            {"clip": "0001", "index": 0, "kind": "ball", "cx": 1.0, "cy": 1.0}
        ]});
        fs::write(dir.join("labels.json"), labels.to_string()).unwrap();
        let chosen = Chosen {
            include: vec![SessionRef {
                id: "none".into(),
                dir: dir.clone(),
            }],
            holdout: Vec::new(),
        };
        assert!(write_dataset(&dir.join("out"), &chosen, &fake_jpeg).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sessions_are_chosen_by_their_training_field() {
        let root = temp("chosen");
        for (id, training) in [
            ("10", Training::Include),
            ("2", Training::Include),
            ("4", Training::Holdout),
            ("5", Training::None),
        ] {
            let dir = root.join("sessions").join(id);
            fs::create_dir_all(&dir).unwrap();
            SessionMeta {
                training,
                ..SessionMeta::default()
            }
            .save(&dir)
            .unwrap();
        }
        let loose = root.join("sessions").join("none");
        fs::create_dir_all(&loose).unwrap();
        SessionMeta {
            training: Training::Holdout,
            ..SessionMeta::default()
        }
        .save(&loose)
        .unwrap();
        let chosen = chosen_sessions(&root).unwrap();
        let ids = |s: &[SessionRef]| s.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&chosen.include), ["2", "10"]);
        assert_eq!(ids(&chosen.holdout), ["4", "none"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn archive_holds_the_contract_layout() {
        let root = temp("archive");
        let dir = root.join("sessions/3");
        session(&dir);
        let model = root.join("ball.onnx");
        fs::write(&model, b"onnx").unwrap();
        let chosen = Chosen {
            include: vec![SessionRef {
                id: "3".into(),
                dir,
            }],
            holdout: Vec::new(),
        };
        let (bytes, built) = build_archive(&chosen, &fake_jpeg, Some(&model)).unwrap();
        let mut names = Vec::new();
        let mut manifest = String::new();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            if name == "manifest.json" {
                entry.read_to_string(&mut manifest).unwrap();
            }
            names.push(name);
        }
        for want in [
            "manifest.json",
            "train/data.yaml",
            "train/images/val/s0_0001_00000.jpg",
            "train/labels/train/s0_0002_00005.txt",
            "current.onnx",
        ] {
            assert!(
                names.iter().any(|n| n == want),
                "{want} missing from {names:?}"
            );
        }
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["sessions"], serde_json::json!(["3"]));
        assert_eq!(manifest["frames"], built.train + built.val);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_793_448_000), "2026-10-31T12:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }
}
