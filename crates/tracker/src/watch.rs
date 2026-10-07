//! Live episode detection. Watches the stream of per-frame ball detections
//! and cuts out each window where a ball moved, with pre-roll and settle
//! time, so the window can be measured and saved like a recorded clip.

use std::collections::VecDeque;

use crate::config::WatchConfig;
use crate::track::{BallBox, DetFrame};

/// How far back a ball may match a previous position and still count as
/// the same resting ball. Covers a detector that drops a frame or two.
const MATCH_WINDOW_S: f64 = 0.1;

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
}

impl Watcher {
    pub fn new(cfg: WatchConfig) -> Self {
        Self {
            cfg,
            frames: VecDeque::new(),
            recent: VecDeque::new(),
            first_ns: None,
            episode: None,
        }
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
        let first_ns = *self.first_ns.get_or_insert(t_ns);
        let moving = self.moving(t_ns, first_ns, &balls);
        self.remember(t_ns, &balls);
        self.frames.push_back(DetFrame { index, t_ns, balls });
        let preroll = secs_ns(self.cfg.preroll_s);
        match &mut self.episode {
            None => {
                self.trim(t_ns.saturating_sub(preroll));
                if moving {
                    self.episode = Some(Running {
                        start_ns: t_ns,
                        last_motion_ns: t_ns,
                    });
                }
                None
            }
            Some(run) => {
                if moving {
                    run.last_motion_ns = t_ns;
                }
                let settled = t_ns - run.last_motion_ns >= secs_ns(self.cfg.settle_s);
                let too_long = t_ns - run.start_ns >= secs_ns(self.cfg.max_episode_s);
                if !(settled || too_long) {
                    return None;
                }
                self.episode = None;
                let frames: Vec<DetFrame> = self.frames.iter().cloned().collect();
                self.trim(t_ns.saturating_sub(preroll));
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
    fn moving(&self, t_ns: u64, first_ns: u64, balls: &[BallBox]) -> bool {
        if balls.is_empty() || t_ns - first_ns < secs_ns(MATCH_WINDOW_S) {
            return false;
        }
        let limit = self.cfg.move_px * self.cfg.move_px;
        balls.iter().any(|b| {
            let (cx, cy) = b.centroid();
            !self.recent.iter().any(|(_, centres)| {
                centres
                    .iter()
                    .any(|(px, py)| (cx - px).powi(2) + (cy - py).powi(2) <= limit)
            })
        })
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

fn secs_ns(s: f64) -> u64 {
    (s * 1e9).round() as u64
}
