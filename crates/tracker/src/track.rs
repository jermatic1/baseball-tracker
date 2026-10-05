//! Baseball motion on top of the generic tracker: which tracked objects are
//! at rest, when one starts moving, and which moving segments are events.

use serde::{Deserialize, Serialize};

use crate::mot::{secs, Detection, TrackedObject, Tracker, TrackerConfig};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BallBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub conf: f64,
    pub depth_m: Option<f64>,
}

impl BallBox {
    pub fn centroid(&self) -> (f64, f64) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetFrame {
    pub index: usize,
    pub t_ns: u64,
    pub balls: Vec<BallBox>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionParams {
    /// Below this many ball diameters per second a step counts as rest.
    pub rest_diam_per_s: f64,
    /// Slowest moving segment that counts as an event, in m/s.
    pub min_event_mps: f64,
    /// Fewest observations a moving segment needs to be an event.
    pub min_points: usize,
    /// Fastest plausible ball, used to size the tracker's jump gate.
    pub max_speed_mps: f64,
}

impl Default for MotionParams {
    fn default() -> Self {
        Self {
            rest_diam_per_s: 40.0,
            min_event_mps: 6.7,
            min_points: 3,
            max_speed_mps: 55.0,
        }
    }
}

/// Where a ball sat before it moved, and when it left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub cx: f64,
    pub cy: f64,
    pub depth_m: Option<f64>,
    pub t_ns: u64,
}

/// A run of observations in one track, indices into `history`, inclusive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
    pub moving: bool,
    pub anchor: Option<Anchor>,
    pub after_reversal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    pub track_id: u32,
    pub segment: Segment,
    pub speed_mps: f64,
    pub median_depth_m: f64,
    pub t_start: u64,
    pub t_end: u64,
}

pub fn tracker_config(fx: f64, depth_m: f64, p: &MotionParams) -> TrackerConfig {
    TrackerConfig {
        max_jump_per_s: p.max_speed_mps * fx / depth_m.max(0.1),
        ..TrackerConfig::default()
    }
}

/// Run the generic tracker over a clip. `tag` is the index into `balls`.
pub fn track_balls(frames: &[DetFrame], cfg: TrackerConfig) -> Vec<TrackedObject> {
    let mut tracker = Tracker::new(cfg);
    for f in frames {
        let dets: Vec<Detection> = f
            .balls
            .iter()
            .enumerate()
            .map(|(tag, b)| {
                let (cx, cy) = b.centroid();
                Detection {
                    cx,
                    cy,
                    w: b.w,
                    h: b.h,
                    conf: b.conf,
                    tag,
                }
            })
            .collect();
        tracker.update(f.index, f.t_ns, &dets);
    }
    tracker.into_tracks()
}

pub fn ball_at(frames: &[DetFrame], frame: usize, tag: usize) -> Option<&BallBox> {
    let f = match frames.get(frame) {
        Some(f) if f.index == frame => f,
        _ => frames.iter().find(|f| f.index == frame)?,
    };
    f.balls.get(tag)
}

pub fn motion_segments(
    track: &TrackedObject,
    frames: &[DetFrame],
    p: &MotionParams,
) -> Vec<Segment> {
    let h = &track.history;
    if h.is_empty() {
        return Vec::new();
    }
    let diam = track.size.max(1.0);
    let steps: Vec<(f64, f64, bool)> = h
        .windows(2)
        .map(|w| {
            let dt = secs(w[1].t_ns.saturating_sub(w[0].t_ns)).max(1e-6);
            let dx = w[1].cx - w[0].cx;
            let dy = w[1].cy - w[0].cy;
            let moving = dx.hypot(dy) / diam / dt > p.rest_diam_per_s;
            (dx, dy, moving)
        })
        .collect();

    let mut out: Vec<Segment> = Vec::new();
    let mut start = 0usize;
    let mut moving = steps.first().map(|s| s.2).unwrap_or(false);
    let mut after_reversal = false;
    let close = |out: &mut Vec<Segment>, start: usize, end: usize, moving: bool, rev: bool| {
        out.push(Segment {
            start,
            end,
            moving,
            anchor: None,
            after_reversal: rev,
        });
    };
    for (k, step) in steps.iter().enumerate() {
        // Step k joins point k to point k+1.
        if step.2 != moving {
            close(&mut out, start, k, moving, after_reversal);
            start = k + 1;
            moving = step.2;
            after_reversal = false;
            continue;
        }
        if moving && k > 0 && steps[k - 1].2 && k > start {
            let dot = steps[k - 1].0 * step.0 + steps[k - 1].1 * step.1;
            if dot < 0.0 {
                // Reversal at point k; it ends one flight and starts the next.
                close(&mut out, start, k, true, after_reversal);
                start = k;
                after_reversal = true;
            }
        }
    }
    close(&mut out, start, h.len() - 1, moving, after_reversal);

    for i in 1..out.len() {
        let (prev, seg) = (out[i - 1], out[i]);
        if !seg.moving || prev.moving {
            continue;
        }
        let rest = &h[prev.start..=prev.end];
        let n = rest.len() as f64;
        let cx = rest.iter().map(|o| o.cx).sum::<f64>() / n;
        let cy = rest.iter().map(|o| o.cy).sum::<f64>() / n;
        let depth_m = median(
            rest.iter()
                .filter_map(|o| ball_at(frames, o.frame, o.tag).and_then(|b| b.depth_m)),
        );
        let first = h[seg.start];
        let t_last_rest = h[prev.end].t_ns;
        let t_ns = match h.get(seg.start + 1) {
            Some(next) => {
                let dt = secs(next.t_ns.saturating_sub(first.t_ns)).max(1e-6);
                let speed = (next.cx - first.cx).hypot(next.cy - first.cy) / dt;
                let back = (first.cx - cx).hypot(first.cy - cy) / speed.max(1e-6);
                let t = first.t_ns as f64 - back * 1e9;
                (t.round() as u64).clamp(t_last_rest + 1, first.t_ns.saturating_sub(1))
            }
            None => (t_last_rest + first.t_ns) / 2,
        };
        out[i].anchor = Some(Anchor {
            cx,
            cy,
            depth_m,
            t_ns,
        });
    }
    out
}

pub fn select_events(
    tracks: &[TrackedObject],
    frames: &[DetFrame],
    fx: f64,
    default_depth_m: f64,
    p: &MotionParams,
) -> Vec<Event> {
    let mut events = Vec::new();
    for track in tracks {
        for segment in motion_segments(track, frames, p) {
            if !segment.moving || segment.end + 1 - segment.start < p.min_points {
                continue;
            }
            let obs = &track.history[segment.start..=segment.end];
            let px_per_s = median(obs.windows(2).map(|w| {
                let dt = secs(w[1].t_ns.saturating_sub(w[0].t_ns)).max(1e-6);
                (w[1].cx - w[0].cx).hypot(w[1].cy - w[0].cy) / dt
            }))
            .unwrap_or(0.0);
            let median_depth_m = median(
                obs.iter()
                    .filter_map(|o| ball_at(frames, o.frame, o.tag).and_then(|b| b.depth_m)),
            )
            .unwrap_or(default_depth_m);
            let speed_mps = px_per_s * median_depth_m / fx;
            if speed_mps < p.min_event_mps {
                continue;
            }
            events.push(Event {
                track_id: track.id,
                segment,
                speed_mps,
                median_depth_m,
                t_start: obs[0].t_ns,
                t_end: obs[obs.len() - 1].t_ns,
            });
        }
    }
    events.sort_by_key(|e| e.t_start);
    // Only one ball moves at a time; overlapping events keep the faster.
    let mut kept: Vec<Event> = Vec::new();
    for e in events {
        match kept.last_mut() {
            Some(last) if e.t_start <= last.t_end => {
                if e.speed_mps > last.speed_mps {
                    *last = e;
                }
            }
            _ => kept.push(e),
        }
    }
    kept
}

pub fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}
