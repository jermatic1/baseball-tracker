//! A camera that plays a recorded session's clips back at their own frame
//! rate, so the live path can run on a desk without the OAK.

use std::time::{Duration, Instant};

use device::{Camera, Frame, Intrinsics};
use tracker::{Clip, Session};

/// Silence between clips in the replayed stream.
const GAP: Duration = Duration::from_secs(1);

pub struct ClipCamera {
    clips: Vec<Clip>,
    clip: usize,
    frame: usize,
    stamps: Vec<u64>,
    /// Wall-clock moment of the current clip's first frame.
    clip_start: Instant,
    /// Stream time of the current clip's first frame.
    base_ns: u64,
    sequence: u64,
    fps: f64,
}

impl ClipCamera {
    pub fn open(source: &Session) -> Result<Self, String> {
        let mut clips = Vec::new();
        for id in source.clip_ids().map_err(|e| e.to_string())? {
            if let Ok(clip) = source.load_clip(&id) {
                clips.push(clip);
            }
        }
        if clips.is_empty() {
            return Err(format!("no clips in {}", source.dir.display()));
        }
        println!(
            "replaying {} clips from {}",
            clips.len(),
            source.dir.display()
        );
        let fps = clips[0].meta.fps.max(1.0);
        let mut cam = Self {
            clips,
            clip: 0,
            frame: 0,
            stamps: Vec::new(),
            clip_start: Instant::now() + GAP,
            base_ns: 1_000_000_000,
            sequence: 0,
            fps,
        };
        cam.load_stamps()?;
        Ok(cam)
    }

    fn load_stamps(&mut self) -> Result<(), String> {
        let clip = &self.clips[self.clip];
        let stamps = clip.stamps().map_err(|e| e.to_string())?;
        let paced = stamps.iter().any(|&t| t != 0);
        self.stamps = (0..clip.meta.frame_count)
            .map(|i| {
                if paced {
                    stamps[i].saturating_sub(stamps[0])
                } else {
                    (i as f64 / self.fps * 1e9) as u64
                }
            })
            .collect();
        Ok(())
    }
}

impl Camera for ClipCamera {
    fn intrinsics(&self) -> Intrinsics {
        let m = &self.clips[0].meta;
        Intrinsics::from_fov(m.width, m.height, 80.0, 55.0)
    }

    fn set_exposure_us(&mut self, _exposure_us: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_gain(&mut self, _gain: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_fps(&mut self, _fps: f32) -> Result<(), String> {
        Err("fps comes from the recording".into())
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Option<Frame>, String> {
        let Some(clip) = self.clips.get(self.clip) else {
            return Ok(None);
        };
        let rel_ns = self.stamps[self.frame];
        if Instant::now() < self.clip_start + Duration::from_nanos(rel_ns) {
            return Ok(None);
        }
        let left = clip.left_frame(self.frame).map_err(|e| e.to_string())?;
        let right = clip.right_frame(self.frame).unwrap_or_default();
        let frame = Frame {
            width: clip.meta.width,
            height: clip.meta.height,
            left,
            right,
            depth_mm: Vec::new(),
            t_ns: self.base_ns + rel_ns,
            sequence: self.sequence,
        };
        self.sequence += 1;
        self.frame += 1;
        if self.frame >= clip.meta.frame_count {
            self.clip_start += Duration::from_nanos(rel_ns) + GAP;
            self.base_ns += rel_ns + GAP.as_nanos() as u64;
            self.clip += 1;
            self.frame = 0;
            if self.clip < self.clips.len() {
                self.load_stamps()?;
            } else {
                println!("replay finished; ctrl-c to stop");
            }
        }
        Ok(Some(frame))
    }
}
