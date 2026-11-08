//! Live episode detection. Watches the stream of per-frame ball detections
//! and cuts out each window where a ball moved, with pre-roll and settle
//! time, so the window can be measured and saved like a recorded clip.
//!
//! A window opens only on a run of moving detections that follow on from
//! each other, away from spots where detections keep landing, so a lone
//! false detection or a flickering reflection never starts one.

use std::collections::{HashMap, VecDeque};

use crate::config::WatchConfig;
use crate::track::{BallBox, DetFrame};

/// How far back a ball may match a previous position and still count as
/// the same resting ball. Covers a detector that drops a frame or two.
const MATCH_WINDOW_S: f64 = 0.1;
/// Moving detections in a run, each following on from the last, that open
/// a window.
const CONFIRM: usize = 3;
/// Longest wait between two detections of one run.
const RUN_GAP_S: f64 = 0.05;
/// Fastest a ball crosses the image, for linking a run.
const MAX_PX_PER_S: f64 = 20_000.0;
/// Grid cell, in pixels, that repeat detections are counted in.
const CELL_PX: f64 = 16.0;
/// How long a cell takes to forget a detection: its count falls by e.
const CELL_MEMORY_S: f64 = 2.0;
/// The decayed count of detections that makes a cell a static spot.
const STATIC_HITS: f64 = 8.0;

/// One window of frames around ball motion, numbered as in the stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub frames: Vec<DetFrame>,
}

impl Episode {
    pub fn first(&self) -> usize {
        self.frames[0].index
    }

    pub fn last(&self) -> usize {
        self.frames[self.frames.len() - 1].index
    }

    /// The same frames numbered from zero, as a saved clip numbers them.
    pub fn as_clip(&self) -> Vec<DetFrame> {
        let first = self.first();
        self.frames
            .iter()
            .map(|f| DetFrame {
                index: f.index - first,
                t_ns: f.t_ns,
                balls: f.balls.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Idle,
    Episode,
}

struct Running {
    start_ns: u64,
    last_motion_ns: u64,
}

pub struct Watcher {
    cfg: WatchConfig,
    /// Pre-roll while idle, the whole window while an episode runs.
    frames: VecDeque<DetFrame>,
    /// Ball centres from recent frames that had any, newest last.
    recent: VecDeque<(u64, Vec<(f64, f64)>)>,
    first_ns: Option<u64>,
    episode: Option<Running>,
    runs: Vec<Run>,
    spots: Spots,
}

/// The newest detection of a run of moving detections.
struct Run {
    /// When the run's motion began, or none if a window closed during it
    /// and the next detection begins it again.
    first_ns: Option<u64>,
    t_ns: u64,
    at: (f64, f64),
    step: Option<(f64, f64)>,
    len: usize,
}

impl Run {
    /// A detection follows on if it is no farther than a ball could go in
    /// the time and, once the run has a direction, does not turn back.
    fn leads_to(&self, at: (f64, f64), t_ns: u64) -> bool {
        let dt = (t_ns - self.t_ns) as f64 / 1e9;
        let step = (at.0 - self.at.0, at.1 - self.at.1);
        let reach = MAX_PX_PER_S * dt;
        let ahead = self
            .step
            .is_none_or(|(sx, sy)| sx * step.0 + sy * step.1 > 0.0);
        step.0 * step.0 + step.1 * step.1 <= reach * reach && ahead
    }
}

/// Decayed detection counts per grid cell. A cell detections keep landing
/// in, such as a reflection or a resting ball, is a static spot.
#[derive(Default)]
struct Spots {
    cells: HashMap<(i32, i32), (u64, f64)>,
}

impl Spots {
    fn cell((x, y): (f64, f64)) -> (i32, i32) {
        ((x / CELL_PX).floor() as i32, (y / CELL_PX).floor() as i32)
    }

    fn count(&self, cell: (i32, i32), t_ns: u64) -> f64 {
        self.cells.get(&cell).map_or(0.0, |&(seen, n)| {
            n * (-(t_ns.saturating_sub(seen) as f64 / 1e9) / CELL_MEMORY_S).exp()
        })
    }

    fn is_static(&self, at: (f64, f64), t_ns: u64) -> bool {
        self.count(Self::cell(at), t_ns) >= STATIC_HITS
    }

    /// Count each cell once per frame, and forget cells that have faded.
    fn add(&mut self, t_ns: u64, centres: &[(f64, f64)]) {
        let mut cells: Vec<(i32, i32)> = centres.iter().map(|&c| Self::cell(c)).collect();
        cells.sort_unstable();
        cells.dedup();
        for cell in cells {
            let n = self.count(cell, t_ns) + 1.0;
            self.cells.insert(cell, (t_ns, n));
        }
        if self.cells.len() > 64 {
            let faded: Vec<(i32, i32)> = self
                .cells
                .keys()
                .copied()
                .filter(|&c| self.count(c, t_ns) < 0.1)
                .collect();
            for cell in faded {
                self.cells.remove(&cell);
            }
        }
    }
}

impl Watcher {
    pub fn new(cfg: WatchConfig) -> Self {
        Self {
            cfg,
            frames: VecDeque::new(),
            recent: VecDeque::new(),
            first_ns: None,
            episode: None,
            runs: Vec::new(),
            spots: Spots::default(),
        }
    }

    /// How far back frames are needed: the pre-roll before a run's first
    /// detection, which is a few frames before the window opens.
    pub fn keep_ns(&self) -> u64 {
        secs_ns(self.cfg.preroll) + secs_ns(RUN_GAP_S) * (CONFIRM as u64 - 1)
    }

    /// First frame of the open window: the pre-roll start.
    pub fn window_start(&self) -> Option<usize> {
        self.episode.as_ref()?;
        self.frames.front().map(|f| f.index)
    }

    pub fn state(&self) -> WatchState {
        if self.episode.is_some() {
            WatchState::Episode
        } else {
            WatchState::Idle
        }
    }

    /// Feed one frame's detections. Returns the finished window when the
    /// motion in it has settled or it has run too long.
    pub fn observe(&mut self, index: usize, t_ns: u64, balls: Vec<BallBox>) -> Option<Episode> {
        let all = vec![true; balls.len()];
        self.observe_gated(index, t_ns, balls, &all)
    }

    /// As `observe`, but only balls marked in `can_move` may count as
    /// motion; the rest are kept in the window but neither open nor extend
    /// it. Callers mark balls whose pixels did not change, or that sit
    /// outside the cage.
    pub fn observe_gated(
        &mut self,
        index: usize,
        t_ns: u64,
        balls: Vec<BallBox>,
        can_move: &[bool],
    ) -> Option<Episode> {
        let first_ns = *self.first_ns.get_or_insert(t_ns);
        let centres: Vec<(f64, f64)> = balls.iter().map(BallBox::centroid).collect();
        let moved: Vec<(f64, f64)> = centres
            .iter()
            .zip(can_move.iter().chain(std::iter::repeat(&true)))
            .filter(|&(&c, &ok)| {
                ok && !self.spots.is_static(c, t_ns) && self.moved(t_ns, first_ns, c)
            })
            .map(|(&c, _)| c)
            .collect();
        let moving = !moved.is_empty();
        let confirmed = self.extend_runs(t_ns, &moved);
        self.spots.add(t_ns, &centres);
        self.remember(t_ns, &balls);
        self.frames.push_back(DetFrame { index, t_ns, balls });
        let preroll = secs_ns(self.cfg.preroll);
        match &mut self.episode {
            None => {
                self.trim(t_ns.saturating_sub(self.keep_ns()));
                if let Some(start_ns) = confirmed {
                    self.trim(start_ns.saturating_sub(preroll));
                    self.episode = Some(Running {
                        start_ns,
                        last_motion_ns: t_ns,
                    });
                }
                None
            }
            Some(run) => {
                if moving {
                    run.last_motion_ns = t_ns;
                }
                let settled = t_ns - run.last_motion_ns >= secs_ns(self.cfg.settle);
                let too_long = t_ns - run.start_ns >= secs_ns(self.cfg.max_episode);
                if !(settled || too_long) {
                    return None;
                }
                self.episode = None;
                let frames: Vec<DetFrame> = self.frames.iter().cloned().collect();
                self.trim(t_ns.saturating_sub(preroll));
                for r in &mut self.runs {
                    r.first_ns = None;
                }
                Some(Episode { frames })
            }
        }
    }

    /// Whatever window is open, for shutdown.
    pub fn flush(&mut self) -> Option<Episode> {
        self.episode.take()?;
        let frames: Vec<DetFrame> = self.frames.drain(..).collect();
        Some(Episode { frames })
    }

    /// A ball moved if it is away from every ball seen in the match window,
    /// including a ball entering an image that had none. Before the window
    /// has filled once, nothing is known about rest, so nothing moves.
    fn moved(&self, t_ns: u64, first_ns: u64, (cx, cy): (f64, f64)) -> bool {
        if t_ns - first_ns < secs_ns(MATCH_WINDOW_S) {
            return false;
        }
        let limit = self.cfg.motion * self.cfg.motion;
        !self.recent.iter().any(|(_, centres)| {
            centres
                .iter()
                .any(|(px, py)| (cx - px).powi(2) + (cy - py).powi(2) <= limit)
        })
    }

    /// Add this frame's moving balls to the runs they follow on from, or
    /// start runs with them. When a run reaches `CONFIRM`, when it began.
    fn extend_runs(&mut self, t_ns: u64, moved: &[(f64, f64)]) -> Option<u64> {
        let gap = secs_ns(RUN_GAP_S);
        self.runs.retain(|r| t_ns - r.t_ns <= gap);
        let mut next = Vec::with_capacity(moved.len());
        for &at in moved {
            let from = self
                .runs
                .iter()
                .filter(|r| r.t_ns < t_ns && r.leads_to(at, t_ns))
                .max_by_key(|r| r.len);
            next.push(match from {
                Some(r) => Run {
                    first_ns: r.first_ns.or(Some(t_ns)),
                    t_ns,
                    at,
                    step: Some((at.0 - r.at.0, at.1 - r.at.1)),
                    len: r.len + 1,
                },
                None => Run {
                    first_ns: Some(t_ns),
                    t_ns,
                    at,
                    step: None,
                    len: 1,
                },
            });
        }
        let confirmed = next
            .iter()
            .filter(|r| r.len >= CONFIRM)
            .filter_map(|r| r.first_ns)
            .min();
        self.runs.extend(next);
        confirmed
    }

    fn remember(&mut self, t_ns: u64, balls: &[BallBox]) {
        let cutoff = t_ns.saturating_sub(secs_ns(MATCH_WINDOW_S));
        while self.recent.front().is_some_and(|(t, _)| *t < cutoff) {
            self.recent.pop_front();
        }
        if !balls.is_empty() {
            self.recent
                .push_back((t_ns, balls.iter().map(BallBox::centroid).collect()));
        }
    }

    fn trim(&mut self, cutoff_ns: u64) {
        while self.frames.front().is_some_and(|f| f.t_ns < cutoff_ns) {
            self.frames.pop_front();
        }
    }
}

/// Correlation below which the pixels in a box have changed.
const SAME_CORRELATION: f64 = 0.8;
/// Gray-level spread below which a box is flat: noise, not content.
const FLAT_STD: f64 = 4.0;

/// Whether the pixels under `b` differ between two `width`x`height` frames
/// by more than a change of brightness, which is all a flickering light does
/// to a reflection. A box whose content is the same in both has not moved.
pub fn box_changed(now: &[u8], before: &[u8], width: u32, height: u32, b: &BallBox) -> bool {
    let (w, h) = (width as usize, height as usize);
    let x0 = (b.x.max(0.0) as usize).min(w);
    let y0 = (b.y.max(0.0) as usize).min(h);
    let x1 = ((b.x + b.w).max(0.0).ceil() as usize).min(w);
    let y1 = ((b.y + b.h).max(0.0).ceil() as usize).min(h);
    let n = ((x1 - x0) * (y1 - y0)) as f64;
    if n == 0.0 || now.len() < w * h || before.len() < w * h {
        return true;
    }
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let a = now[y * w + x] as f64;
            let b = before[y * w + x] as f64;
            sa += a;
            sb += b;
            saa += a * a;
            sbb += b * b;
            sab += a * b;
        }
    }
    let var_a = (saa - sa * sa / n) / n;
    let var_b = (sbb - sb * sb / n) / n;
    let flat = FLAT_STD * FLAT_STD;
    match (var_a < flat, var_b < flat) {
        (true, true) => false,
        (false, false) => (sab - sa * sb / n) / n / (var_a * var_b).sqrt() < SAME_CORRELATION,
        _ => true,
    }
}

fn secs_ns(s: f64) -> u64 {
    (s * 1e9).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 64;
    const H: u32 = 48;

    fn ball(x: f64, y: f64) -> BallBox {
        BallBox {
            x: x - 8.0,
            y: y - 8.0,
            w: 16.0,
            h: 16.0,
            conf: 0.9,
            depth_m: Some(2.0),
        }
    }

    /// A bright disc at (cx, cy) on a mid-gray floor, all scaled by `gain`.
    fn frame(cx: f64, cy: f64, gain: f64) -> Vec<u8> {
        let mut px = vec![0u8; (W * H) as usize];
        for y in 0..H {
            for x in 0..W {
                let r = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
                let v = if r < 6.0 { 200.0 } else { 60.0 };
                px[(y * W + x) as usize] = (v * gain).min(255.0) as u8;
            }
        }
        px
    }

    #[test]
    fn flicker_is_not_change_but_motion_is() {
        let before = frame(20.0, 20.0, 1.0);
        let b = ball(20.0, 20.0);
        assert!(!box_changed(&frame(20.0, 20.0, 0.6), &before, W, H, &b));
        assert!(box_changed(
            &frame(40.0, 20.0, 1.0),
            &before,
            W,
            H,
            &ball(40.0, 20.0)
        ));
        let flat = vec![60u8; (W * H) as usize];
        assert!(!box_changed(&flat, &flat, W, H, &b));
        assert!(box_changed(&before, &flat, W, H, &b));
    }

    fn feed(
        w: &mut Watcher,
        frames: impl IntoIterator<Item = (u64, Vec<BallBox>)>,
    ) -> Vec<Episode> {
        frames
            .into_iter()
            .filter_map(|(i, balls)| w.observe(i as usize, i * 10_000_000, balls))
            .collect()
    }

    #[test]
    fn a_lone_detection_does_not_open_a_window() {
        let mut w = Watcher::new(WatchConfig::default());
        feed(
            &mut w,
            (0..60).map(|i| {
                (
                    i,
                    if i == 30 {
                        vec![ball(300.0, 200.0)]
                    } else {
                        vec![]
                    },
                )
            }),
        );
        assert_eq!(w.state(), WatchState::Idle);
    }

    #[test]
    fn a_run_of_moving_detections_opens_a_window() {
        let mut w = Watcher::new(WatchConfig::default());
        let at = |i: u64| vec![ball(100.0 + 30.0 * (i as f64 - 20.0), 200.0)];
        feed(
            &mut w,
            (0..22).map(|i| (i, if i >= 20 { at(i) } else { vec![] })),
        );
        assert_eq!(w.state(), WatchState::Idle, "two detections are not a run");
        feed(&mut w, [(22, at(22))]);
        assert_eq!(w.state(), WatchState::Episode);
    }

    #[test]
    fn a_flickering_spot_stops_opening_windows() {
        let mut w = Watcher::new(WatchConfig::default());
        // Three frames in every 25, creeping the same way each time, so each
        // burst looks like a short run until the spot is known.
        let eps = feed(
            &mut w,
            (0..600).map(|i| {
                let k = i % 25;
                let seen = k < 3;
                (
                    i,
                    if seen {
                        vec![ball(300.0 + 6.0 * k as f64, 200.0)]
                    } else {
                        vec![]
                    },
                )
            }),
        );
        assert!(!eps.is_empty(), "the first bursts open windows");
        assert!(
            eps.iter().all(|e| e.first() < 300),
            "{:?}",
            eps.iter().map(Episode::first).collect::<Vec<_>>()
        );
    }

    #[test]
    fn balls_that_may_not_move_do_not_open_a_window() {
        let mut w = Watcher::new(WatchConfig::default());
        for i in 0..40u64 {
            let balls = if i >= 20 {
                vec![ball(100.0 + 30.0 * (i as f64 - 20.0), 200.0)]
            } else {
                vec![]
            };
            w.observe_gated(i as usize, i * 10_000_000, balls, &[false]);
        }
        assert_eq!(w.state(), WatchState::Idle);
    }
}
