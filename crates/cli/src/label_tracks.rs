//! Ball tracks for the label page, linked by the tracker's own `track_balls`.

use serde::{Deserialize, Serialize};
use tracker::{ball_at, track_balls, tracker_config, DetFrame, Intrinsics, MotionParams};

/// Centroid travel, in px, above which a track is guessed to be moving.
const MOVING_PX: f64 = 40.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub index: usize,
    pub cx: f64,
    pub cy: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackView {
    /// `moving`, `resting`, or `stray` for the detections no track kept.
    pub kind: &'static str,
    pub start: usize,
    pub end: usize,
    pub conf: f64,
    pub points: Vec<Point>,
    /// `ball` or `reject` once reviewed.
    pub verdict: Option<String>,
}

/// Link a clip's detections into tracks; single-detection tracks are pooled
/// into one trailing `stray` group so every detection is listed.
pub fn clip_tracks(
    frames: &[DetFrame],
    width: u32,
    height: u32,
    default_depth_m: f64,
) -> Vec<TrackView> {
    let params = MotionParams::default();
    let fx = Intrinsics::from_fov(width, height, 80.0, 55.0).fx;
    let tracks = track_balls(frames, tracker_config(fx, default_depth_m, &params));
    let mut out = Vec::new();
    let mut stray: Vec<(Point, f64)> = Vec::new();
    for track in &tracks {
        let rows: Vec<(Point, f64)> = track
            .history
            .iter()
            .map(|o| {
                let conf = ball_at(frames, o.frame, o.tag).map_or(0.0, |b| b.conf);
                let point = Point {
                    index: o.frame,
                    cx: o.cx,
                    cy: o.cy,
                };
                (point, conf)
            })
            .collect();
        if rows.len() < 2 {
            stray.extend(rows);
            continue;
        }
        let first = &rows[0].0;
        let travel = rows
            .iter()
            .map(|(p, _)| (p.cx - first.cx).hypot(p.cy - first.cy))
            .fold(0.0, f64::max);
        let kind = if travel > MOVING_PX {
            "moving"
        } else {
            "resting"
        };
        out.push(view(kind, rows));
    }
    out.sort_by_key(|t| (t.start, t.end));
    if !stray.is_empty() {
        stray.sort_by_key(|(p, _)| p.index);
        out.push(view("stray", stray));
    }
    out
}

fn view(kind: &'static str, rows: Vec<(Point, f64)>) -> TrackView {
    let conf = rows.iter().map(|(_, c)| c).sum::<f64>() / rows.len() as f64;
    TrackView {
        kind,
        start: rows[0].0.index,
        end: rows[rows.len() - 1].0.index,
        conf,
        points: rows.into_iter().map(|(p, _)| p).collect(),
        verdict: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracker::BallBox;

    fn frame(index: usize, centres: &[(f64, f64)]) -> DetFrame {
        DetFrame {
            index,
            t_ns: index as u64 * 10_000_000,
            balls: centres
                .iter()
                .map(|&(cx, cy)| BallBox {
                    x: cx - 10.0,
                    y: cy - 10.0,
                    w: 20.0,
                    h: 20.0,
                    conf: 0.8,
                    depth_m: None,
                })
                .collect(),
        }
    }

    #[test]
    fn groups_moving_resting_and_stray() {
        let mut frames: Vec<DetFrame> = (0..20)
            .map(|i| {
                let jitter = (i % 2) as f64 * 0.5;
                frame(
                    i,
                    &[(100.0 + jitter, 300.0), (200.0 + i as f64 * 8.0, 100.0)],
                )
            })
            .collect();
        frames[10].balls.push(BallBox {
            x: 590.0,
            y: 360.0,
            w: 20.0,
            h: 20.0,
            conf: 0.3,
            depth_m: None,
        });
        let tracks = clip_tracks(&frames, 640, 400, 2.0);
        let kinds: Vec<_> = tracks.iter().map(|t| t.kind).collect();
        assert_eq!(kinds.len(), 3, "{kinds:?}");
        assert!(kinds.contains(&"moving") && kinds.contains(&"resting"));
        assert_eq!(kinds[2], "stray");
        for t in &tracks[..2] {
            assert_eq!((t.start, t.end, t.points.len()), (0, 19, 20));
        }
        let stray = &tracks[2];
        assert_eq!(
            stray.points,
            vec![Point {
                index: 10,
                cx: 600.0,
                cy: 370.0
            }]
        );
        assert!((stray.conf - 0.3).abs() < 1e-9);
    }
}
