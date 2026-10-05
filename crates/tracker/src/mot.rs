//! Generic 2-D multi-object tracking by detection.
//!
//! A `Tracker` is configured once and fed one frame of detections at a time,
//! the same shape as SORT or Norfair. It predicts each track with constant
//! velocity, matches detections greedily by gated distance, and keeps tracks
//! alive through a few missed frames. It knows nothing about what the
//! objects are; callers map results back through `tag`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub conf: f64,
    /// Opaque index the caller uses to find its own detection again.
    pub tag: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    /// Base gate as a multiple of object size, grown by each missed frame.
    pub gate_size_factor: f64,
    /// Gate growth per unit of predicted travel (`|v| * dt`).
    pub gate_velocity_factor: f64,
    /// Gate in px/s for tracks with no velocity yet, so a resting object can
    /// be joined to its first fast step. Callers derive it from the fastest
    /// plausible motion.
    pub max_jump_per_s: f64,
    /// Frames a track survives without a match.
    pub max_misses: usize,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            gate_size_factor: 1.0,
            gate_velocity_factor: 0.5,
            max_jump_per_s: 1000.0,
            max_misses: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub frame: usize,
    pub t_ns: u64,
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub tag: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackedObject {
    pub id: u32,
    /// Matched observations only, in frame order.
    pub history: Vec<Observation>,
    /// Pixels per second from the last two observations; zero with one.
    pub velocity: (f64, f64),
    pub misses: usize,
    /// Median of width and height over recent observations.
    pub size: f64,
}

impl TrackedObject {
    pub fn last(&self) -> &Observation {
        self.history
            .last()
            .expect("a track always has at least one observation")
    }

    fn observe(&mut self, obs: Observation) {
        let prev = *self.last();
        let dt = secs(obs.t_ns.saturating_sub(prev.t_ns));
        if dt > 0.0 {
            self.velocity = ((obs.cx - prev.cx) / dt, (obs.cy - prev.cy) / dt);
        }
        self.history.push(obs);
        self.misses = 0;
        self.size = median_size(&self.history);
    }
}

pub struct Tracker {
    config: TrackerConfig,
    next_id: u32,
    tracks: Vec<TrackedObject>,
}

impl Tracker {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            next_id: 0,
            tracks: Vec::new(),
        }
    }

    pub fn config(&self) -> &TrackerConfig {
        &self.config
    }

    /// Every track seen so far, finished ones included, in creation order.
    pub fn tracks(&self) -> &[TrackedObject] {
        &self.tracks
    }

    pub fn into_tracks(self) -> Vec<TrackedObject> {
        self.tracks
    }

    /// Constant-velocity prediction. The motion model lives here alone so a
    /// filter could replace it without touching callers.
    pub fn predict(track: &TrackedObject, t_ns: u64) -> (f64, f64) {
        let last = track.last();
        let dt = secs(t_ns.saturating_sub(last.t_ns));
        (
            last.cx + track.velocity.0 * dt,
            last.cy + track.velocity.1 * dt,
        )
    }

    /// Feed one frame. Returns the tracks matched or created in it.
    pub fn update(
        &mut self,
        frame: usize,
        t_ns: u64,
        detections: &[Detection],
    ) -> Vec<&TrackedObject> {
        let cfg = self.config;
        let live: Vec<usize> = (0..self.tracks.len())
            .filter(|&i| self.tracks[i].misses <= cfg.max_misses)
            .collect();
        let mut track_used = vec![false; self.tracks.len()];
        let mut det_used = vec![false; detections.len()];
        let mut touched = Vec::new();

        // Pass 1: tracks with a velocity, gated around their prediction.
        // Pass 2: everything still unmatched, gated by the maximum jump.
        for pass in 0..2 {
            let mut candidates = Vec::new();
            for &ti in &live {
                if track_used[ti] {
                    continue;
                }
                let tr = &self.tracks[ti];
                let has_velocity = tr.velocity != (0.0, 0.0);
                if pass == 0 && !has_velocity {
                    continue;
                }
                let dt = secs(t_ns.saturating_sub(tr.last().t_ns));
                let (px, py) = Self::predict(tr, t_ns);
                let gate = if pass == 0 {
                    let speed = tr.velocity.0.hypot(tr.velocity.1);
                    cfg.gate_size_factor * tr.size * (1.0 + tr.misses as f64)
                        + cfg.gate_velocity_factor * speed * dt
                } else {
                    cfg.max_jump_per_s * dt
                };
                if gate <= 0.0 {
                    continue;
                }
                for (di, d) in detections.iter().enumerate() {
                    if det_used[di] {
                        continue;
                    }
                    let dist = (d.cx - px).hypot(d.cy - py);
                    if dist <= gate {
                        candidates.push((dist / gate, ti, di));
                    }
                }
            }
            candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            for (_, ti, di) in candidates {
                if track_used[ti] || det_used[di] {
                    continue;
                }
                track_used[ti] = true;
                det_used[di] = true;
                self.tracks[ti].observe(observation(frame, t_ns, &detections[di]));
                touched.push(ti);
            }
        }

        for &ti in &live {
            if !track_used[ti] {
                self.tracks[ti].misses += 1;
            }
        }
        for (di, d) in detections.iter().enumerate() {
            if det_used[di] {
                continue;
            }
            let obs = observation(frame, t_ns, d);
            self.tracks.push(TrackedObject {
                id: self.next_id,
                history: vec![obs],
                velocity: (0.0, 0.0),
                misses: 0,
                size: (d.w + d.h) * 0.5,
            });
            self.next_id += 1;
            touched.push(self.tracks.len() - 1);
        }
        touched.sort_unstable();
        touched.iter().map(|&ti| &self.tracks[ti]).collect()
    }
}

fn observation(frame: usize, t_ns: u64, d: &Detection) -> Observation {
    Observation {
        frame,
        t_ns,
        cx: d.cx,
        cy: d.cy,
        w: d.w,
        h: d.h,
        tag: d.tag,
    }
}

fn median_size(history: &[Observation]) -> f64 {
    let recent = &history[history.len().saturating_sub(15)..];
    let mut sizes: Vec<f64> = recent.iter().map(|o| (o.w + o.h) * 0.5).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sizes[sizes.len() / 2]
}

pub fn secs(ns: u64) -> f64 {
    ns as f64 / 1e9
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: u64 = 10_000_000;

    fn det(cx: f64, cy: f64, tag: usize) -> Detection {
        Detection {
            cx,
            cy,
            w: 18.0,
            h: 18.0,
            conf: 0.9,
            tag,
        }
    }

    fn config() -> TrackerConfig {
        TrackerConfig {
            max_jump_per_s: 12_000.0,
            ..Default::default()
        }
    }

    #[test]
    fn stationary_objects_keep_separate_tracks() {
        let mut tr = Tracker::new(config());
        for i in 0..10 {
            tr.update(
                i,
                i as u64 * DT,
                &[det(100.0, 200.0, 0), det(400.0, 210.0, 1)],
            );
        }
        assert_eq!(tr.tracks().len(), 2);
        assert!(tr.tracks().iter().all(|t| t.history.len() == 10));
    }

    #[test]
    fn mover_keeps_one_id() {
        let mut tr = Tracker::new(config());
        for i in 0..6 {
            let x = 100.0 + 45.0 * i as f64;
            tr.update(i, i as u64 * DT, &[det(x, 200.0 - 5.0 * i as f64, 0)]);
        }
        assert_eq!(tr.tracks().len(), 1);
        assert_eq!(tr.tracks()[0].history.len(), 6);
        assert!((tr.tracks()[0].velocity.0 - 4500.0).abs() < 1e-6);
    }

    #[test]
    fn gap_is_bridged_by_prediction() {
        let mut tr = Tracker::new(config());
        for i in 0..8 {
            if i == 3 || i == 4 {
                tr.update(i, i as u64 * DT, &[]);
                continue;
            }
            tr.update(i, i as u64 * DT, &[det(100.0 + 45.0 * i as f64, 200.0, 0)]);
        }
        assert_eq!(tr.tracks().len(), 1);
        assert_eq!(tr.tracks()[0].history.len(), 6);
    }

    #[test]
    fn rest_then_fast_departure_is_one_track() {
        let mut tr = Tracker::new(config());
        for i in 0..20 {
            let jitter = (i % 3) as f64 - 1.0;
            tr.update(i, i as u64 * DT, &[det(150.0 + jitter, 240.0, 0)]);
        }
        for i in 20..26 {
            let x = 150.0 + 45.0 * (i - 19) as f64;
            tr.update(
                i,
                i as u64 * DT,
                &[det(x, 240.0 - 6.0 * (i - 19) as f64, 0)],
            );
        }
        assert_eq!(tr.tracks().len(), 1);
        assert_eq!(tr.tracks()[0].history.len(), 26);
    }

    #[test]
    fn crossing_objects_keep_ids() {
        let mut tr = Tracker::new(config());
        for i in 0..10 {
            let t = i as f64;
            let a = det(100.0 + 40.0 * t, 200.0, 0);
            let b = det(460.0 - 40.0 * t, 200.0, 1);
            tr.update(i, i as u64 * DT, &[a, b]);
        }
        assert_eq!(tr.tracks().len(), 2);
        let a = &tr.tracks()[0];
        let b = &tr.tracks()[1];
        assert!(a.velocity.0 > 0.0 && b.velocity.0 < 0.0);
        assert_eq!(a.history.len(), 10);
        assert_eq!(b.history.len(), 10);
    }

    #[test]
    fn dead_tracks_are_not_extended() {
        let mut tr = Tracker::new(TrackerConfig {
            max_misses: 1,
            ..config()
        });
        tr.update(0, 0, &[det(100.0, 100.0, 0)]);
        tr.update(1, DT, &[]);
        tr.update(2, 2 * DT, &[]);
        tr.update(3, 3 * DT, &[det(100.0, 100.0, 0)]);
        assert_eq!(tr.tracks().len(), 2);
    }
}
