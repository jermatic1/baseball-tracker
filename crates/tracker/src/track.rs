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
    /// Noise floor for any moving segment to count as an event, in m/s.
    /// The hit speed floor is applied later from the session config.
    pub min_event_mps: f64,
    /// Fewest observations a moving segment needs to be an event.
    pub min_points: usize,
    /// Fastest plausible ball, used to size the tracker's jump gate.
    pub max_speed_mps: f64,
    /// Cosine of the turn between consecutive steps below which the motion
    /// is a new flight (contact): 0.5 is a turn of more than 60 degrees.
    pub reversal_cos_max: f64,
    /// A step slower than this fraction of the previous one is an impact.
    pub speed_drop_ratio: f64,
    /// Vertical step, in ball diameters, both sides of a bounce must show.
    pub bounce_min_diam: f64,
}

impl Default for MotionParams {
    fn default() -> Self {
        Self {
            rest_diam_per_s: 40.0,
            min_event_mps: 2.0,
            min_points: 3,
            max_speed_mps: 55.0,
            reversal_cos_max: 0.5,
            speed_drop_ratio: 0.6,
            bounce_min_diam: 0.25,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// The ball sat still here before it moved.
    Rest,
    /// The last observation of the incoming flight, where it was struck.
    Pivot,
}

/// Where the ball was when its flight began, and when.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub cx: f64,
    pub cy: f64,
    pub depth_m: Option<f64>,
    pub t_ns: u64,
    pub kind: AnchorKind,
}

/// Why a moving run was cut at the start of a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitKind {
    /// Sharp turn: the ball was struck.
    Reversal,
    /// Descending then ascending in the image: it bounced.
    Bounce,
    /// Sudden loss of speed: it hit something.
    SpeedDrop,
}

/// A run of observations in one track, indices into `history`, inclusive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
    pub moving: bool,
    pub anchor: Option<Anchor>,
    pub after: Option<SplitKind>,
}

impl Segment {
    pub fn after_reversal(&self) -> bool {
        self.after == Some(SplitKind::Reversal)
    }

    pub fn after_impact(&self) -> bool {
        matches!(self.after, Some(SplitKind::Bounce | SplitKind::SpeedDrop))
    }
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

struct Step {
    dx: f64,
    dy: f64,
    px_per_s: f64,
    moving: bool,
    /// Both observations are from consecutive frames, so the step is clean.
    consecutive: bool,
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
    let steps: Vec<Step> = h
        .windows(2)
        .map(|w| {
            let dt = secs(w[1].t_ns.saturating_sub(w[0].t_ns)).max(1e-6);
            let dx = w[1].cx - w[0].cx;
            let dy = w[1].cy - w[0].cy;
            let px_per_s = dx.hypot(dy) / dt;
            Step {
                dx,
                dy,
                px_per_s,
                moving: px_per_s / diam > p.rest_diam_per_s,
                consecutive: w[1].frame == w[0].frame + 1,
            }
        })
        .collect();

    let mut out: Vec<Segment> = Vec::new();
    let mut start = 0usize;
    let mut moving = steps.first().map(|s| s.moving).unwrap_or(false);
    let mut after: Option<SplitKind> = None;
    let close = |out: &mut Vec<Segment>, start: usize, end: usize, moving: bool, after| {
        if end >= start {
            out.push(Segment {
                start,
                end,
                moving,
                anchor: None,
                after,
            });
        }
    };
    for (k, step) in steps.iter().enumerate() {
        // Step k joins point k to point k+1.
        if step.moving != moving {
            close(&mut out, start, k, moving, after);
            start = k + 1;
            moving = step.moving;
            after = None;
            continue;
        }
        if !moving || k == 0 || !steps[k - 1].moving || k <= start {
            continue;
        }
        let prev = &steps[k - 1];
        if let Some(kind) = split_kind(prev, step, steps.get(k + 1), diam, p) {
            // Point k is the last of the old flight; the new one starts after it.
            close(&mut out, start, k, true, after);
            start = k + 1;
            after = Some(kind);
        }
    }
    close(&mut out, start, h.len() - 1, moving, after);

    for i in 1..out.len() {
        let (prev, seg) = (out[i - 1], out[i]);
        if !seg.moving {
            continue;
        }
        if seg.after == Some(SplitKind::Reversal) {
            let pivot = h[prev.end];
            out[i].anchor = Some(Anchor {
                cx: pivot.cx,
                cy: pivot.cy,
                depth_m: ball_at(frames, pivot.frame, pivot.tag).and_then(|b| b.depth_m),
                t_ns: pivot.t_ns,
                kind: AnchorKind::Pivot,
            });
            continue;
        }
        if prev.moving {
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
            kind: AnchorKind::Rest,
        });
    }
    out
}

fn split_kind(
    prev: &Step,
    step: &Step,
    next: Option<&Step>,
    diam: f64,
    p: &MotionParams,
) -> Option<SplitKind> {
    let norm = prev.dx.hypot(prev.dy) * step.dx.hypot(step.dy);
    if norm <= 0.0 {
        return None;
    }
    let cos = (prev.dx * step.dx + prev.dy * step.dy) / norm;
    if cos < p.reversal_cos_max {
        return Some(SplitKind::Reversal);
    }
    // Image y grows downward: falling then rising is a bounce. The apex of a
    // flight is the opposite order and must not split. A bounce between two
    // frames leaves one mixed step, so the rise may show up one step later.
    let min_dy = p.bounce_min_diam * diam;
    let rises_now = step.dy < -min_dy;
    let rises_next = step.dy.abs() < min_dy && next.is_some_and(|n| n.dy < -min_dy);
    if prev.dy > min_dy && (rises_now || rises_next) {
        return Some(SplitKind::Bounce);
    }
    if prev.consecutive && step.consecutive && step.px_per_s < p.speed_drop_ratio * prev.px_per_s {
        return Some(SplitKind::SpeedDrop);
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mot::Observation;

    const DT: u64 = 10_000_000;

    fn track_from(points: &[(f64, f64)]) -> TrackedObject {
        let history = points
            .iter()
            .enumerate()
            .map(|(i, &(cx, cy))| Observation {
                frame: i,
                t_ns: i as u64 * DT,
                cx,
                cy,
                w: 18.0,
                h: 18.0,
                tag: 0,
            })
            .collect();
        TrackedObject {
            id: 0,
            history,
            velocity: (0.0, 0.0),
            misses: 0,
            size: 18.0,
        }
    }

    /// Five steps one way, then five steps turned by `turn_deg`.
    fn turned(turn_deg: f64) -> Vec<(f64, f64)> {
        let mut pts = vec![(100.0, 200.0)];
        for _ in 0..5 {
            let (x, y) = *pts.last().unwrap();
            pts.push((x + 40.0, y));
        }
        let (s, c) = turn_deg.to_radians().sin_cos();
        for _ in 0..5 {
            let (x, y) = *pts.last().unwrap();
            pts.push((x + 40.0 * c, y + 40.0 * s));
        }
        pts
    }

    fn kinds(points: &[(f64, f64)]) -> Vec<Option<SplitKind>> {
        let t = track_from(points);
        motion_segments(&t, &[], &MotionParams::default())
            .iter()
            .map(|s| s.after)
            .collect()
    }

    #[test]
    fn sharp_turns_split_gentle_ones_do_not() {
        assert_eq!(kinds(&turned(170.0)), vec![None, Some(SplitKind::Reversal)]);
        assert_eq!(kinds(&turned(100.0)), vec![None, Some(SplitKind::Reversal)]);
        assert_eq!(kinds(&turned(65.0)), vec![None, Some(SplitKind::Reversal)]);
        assert_eq!(kinds(&turned(30.0)), vec![None]);
    }

    #[test]
    fn speed_drop_splits() {
        let mut pts = vec![(100.0, 200.0)];
        for _ in 0..5 {
            let (x, y) = *pts.last().unwrap();
            pts.push((x + 40.0, y));
        }
        for _ in 0..5 {
            let (x, y) = *pts.last().unwrap();
            pts.push((x + 15.0, y));
        }
        assert_eq!(kinds(&pts), vec![None, Some(SplitKind::SpeedDrop)]);
    }

    #[test]
    fn bounce_splits_but_apex_does_not() {
        let falling_then_rising: Vec<(f64, f64)> = (0..12)
            .map(|i| {
                let x = 100.0 + 40.0 * i as f64;
                let y = if i <= 6 {
                    100.0 + 12.0 * i as f64
                } else {
                    172.0 - 12.0 * (i - 6) as f64
                };
                (x, y)
            })
            .collect();
        assert_eq!(
            kinds(&falling_then_rising),
            vec![None, Some(SplitKind::Bounce)]
        );
        let rising_then_falling: Vec<(f64, f64)> = falling_then_rising
            .iter()
            .map(|&(x, y)| (x, 300.0 - y))
            .collect();
        assert_eq!(kinds(&rising_then_falling), vec![None]);
    }

    #[test]
    fn pivot_anchor_after_reversal() {
        let t = track_from(&turned(170.0));
        let segs = motion_segments(&t, &[], &MotionParams::default());
        let anchor = segs[1].anchor.expect("pivot");
        assert_eq!(anchor.kind, AnchorKind::Pivot);
        assert_eq!((anchor.cx, anchor.cy), t.history[5].cx_cy());
        assert_eq!(segs[1].start, 6);
    }

    impl Observation {
        fn cx_cy(&self) -> (f64, f64) {
            (self.cx, self.cy)
        }
    }
}
