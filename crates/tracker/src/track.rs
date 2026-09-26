use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub points: Vec<(usize, BallBox)>,
}

struct Obs {
    index: usize,
    ball: BallBox,
    t_ns: u64,
    cx: f64,
    cy: f64,
}

pub fn select_hit(frames: &[DetFrame], width: u32, height: u32) -> Option<Track> {
    let _ = width;
    let mut tracks: Vec<Vec<Obs>> = Vec::new();
    for fr in frames {
        let mut used_track = vec![false; tracks.len()];
        let mut used_ball = vec![false; fr.balls.len()];
        let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
        for (bi, b) in fr.balls.iter().enumerate() {
            let (cx, cy) = b.centroid();
            for (ti, tr) in tracks.iter().enumerate() {
                let last = tr.last().unwrap();
                let d = (cx - last.cx).hypot(cy - last.cy);
                if d <= 300.0 {
                    pairs.push((ti, bi, d));
                }
            }
        }
        pairs.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        for (ti, bi, _) in pairs {
            if used_track[ti] || used_ball[bi] {
                continue;
            }
            used_track[ti] = true;
            used_ball[bi] = true;
            let b = fr.balls[bi];
            let (cx, cy) = b.centroid();
            tracks[ti].push(Obs {
                index: fr.index,
                ball: b,
                t_ns: fr.t_ns,
                cx,
                cy,
            });
        }
        for (bi, b) in fr.balls.iter().enumerate() {
            if used_ball[bi] {
                continue;
            }
            let (cx, cy) = b.centroid();
            tracks.push(vec![Obs {
                index: fr.index,
                ball: *b,
                t_ns: fr.t_ns,
                cx,
                cy,
            }]);
        }
    }

    let h = height as f64;
    let mut best: Option<(f64, Track)> = None;
    for tr in &tracks {
        if tr.len() < 2 {
            continue;
        }
        let first = &tr[0];
        let last = tr.last().unwrap();
        let dx = last.cx - first.cx;
        let dy = last.cy - first.cy;
        let dt = (last.t_ns as f64 - first.t_ns as f64) / 1e9;
        if dt <= 0.0 {
            continue;
        }
        let speed = dx.hypot(dy) / dt;
        let mut ys: Vec<f64> = tr.iter().map(|p| p.cy).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med_y = ys[ys.len() / 2];
        if med_y > 0.65 * h && speed < 300.0 {
            continue;
        }
        if !((dx > 20.0 || dy < -20.0) && speed > 400.0) {
            continue;
        }
        let track = Track {
            points: tr.iter().map(|p| (p.index, p.ball)).collect(),
        };
        if best.as_ref().map(|(s, _)| speed > *s).unwrap_or(true) {
            best = Some((speed, track));
        }
    }
    best.map(|(_, t)| t)
}
