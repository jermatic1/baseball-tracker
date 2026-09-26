use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::SessionConfig;
use crate::track::{BallBox, DetFrame};
use crate::TrackerError;

#[derive(Debug, Clone)]
pub struct StoredFrame {
    pub width: u32,
    pub height: u32,
    pub left: Vec<u8>,
    pub depth_mm: Vec<u16>,
    pub t_ns: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMeta {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frame_count: usize,
    pub exposure_us: u32,
    pub gain: u32,
    pub sequence_gaps: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detections {
    pub frames: Vec<DetFrame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HitRecord {
    pub clip: String,
    pub exit_velocity_mph: f64,
    pub launch_angle_deg: f64,
    pub spray_angle_deg: f64,
    pub samples: usize,
    pub confident: bool,
    pub posted: bool,
}

#[derive(Debug, Clone)]
pub struct Clip {
    pub dir: PathBuf,
    pub meta: ClipMeta,
}

impl Clip {
    pub fn left_frame(&self, i: usize) -> Result<Vec<u8>, TrackerError> {
        if i >= self.meta.frame_count {
            return Err(TrackerError::Other(format!("frame {i} out of range")));
        }
        let n = (self.meta.width as usize) * (self.meta.height as usize);
        let mut f = File::open(self.dir.join("left.gray"))?;
        f.seek(SeekFrom::Start((i * n) as u64))?;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn depth_frame(&self, i: usize) -> Result<Vec<u16>, TrackerError> {
        if i >= self.meta.frame_count {
            return Err(TrackerError::Other(format!("frame {i} out of range")));
        }
        let n = (self.meta.width as usize) * (self.meta.height as usize);
        let mut f = File::open(self.dir.join("depth.u16"))?;
        f.seek(SeekFrom::Start((i * n * 2) as u64))?;
        let mut bytes = vec![0u8; n * 2];
        f.read_exact(&mut bytes)?;
        let mut out = vec![0u16; n];
        for (i, chunk) in bytes.chunks_exact(2).enumerate() {
            out[i] = u16::from_le_bytes([chunk[0], chunk[1]]);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub dir: PathBuf,
    pub config: SessionConfig,
}

pub struct ClipWriter {
    id: String,
    dir: PathBuf,
    left: File,
    depth: File,
    stamps: File,
    count: usize,
    width: u32,
    height: u32,
    first_t: Option<u64>,
    last_t: Option<u64>,
    gaps: bool,
    last_seq: Option<u64>,
    exposure_us: u32,
    gain: u32,
    fps_fallback: f64,
}

impl ClipWriter {
    pub fn write(&mut self, frame: &StoredFrame) -> Result<(), TrackerError> {
        if self.count == 0 {
            self.width = frame.width;
            self.height = frame.height;
            self.first_t = Some(frame.t_ns);
        }
        if let Some(prev) = self.last_seq {
            if frame.sequence != prev + 1 {
                self.gaps = true;
            }
        }
        self.last_seq = Some(frame.sequence);
        self.last_t = Some(frame.t_ns);
        self.left.write_all(&frame.left)?;
        write_depth(&mut self.depth, &frame.depth_mm)?;
        self.stamps.write_all(&frame.t_ns.to_le_bytes())?;
        self.stamps.write_all(&frame.sequence.to_le_bytes())?;
        self.count += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<String, TrackerError> {
        let dt = match (self.first_t, self.last_t) {
            (Some(a), Some(b)) if self.count >= 2 && b > a => (b - a) as f64 / 1e9,
            _ => 0.0,
        };
        let fps = if dt > 0.0 {
            (self.count - 1) as f64 / dt
        } else {
            self.fps_fallback
        };
        let meta = ClipMeta {
            width: self.width,
            height: self.height,
            fps,
            frame_count: self.count,
            exposure_us: self.exposure_us,
            gain: self.gain,
            sequence_gaps: self.gaps,
        };
        fs::write(
            self.dir.join("meta.json"),
            serde_json::to_vec_pretty(&meta)?,
        )?;
        Ok(self.id)
    }
}

fn write_depth(out: &mut impl Write, values: &[u16]) -> Result<(), TrackerError> {
    let mut buf = vec![0u8; values.len() * 2];
    for (i, v) in values.iter().enumerate() {
        buf[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    out.write_all(&buf)?;
    Ok(())
}

impl Session {
    pub fn create(dir: impl AsRef<Path>) -> Result<Self, TrackerError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let cfg_path = dir.join("config.toml");
        if !cfg_path.exists() {
            SessionConfig::example().save(&cfg_path)?;
        }
        Self::open(dir)
    }

    pub fn open(dir: impl AsRef<Path>) -> Result<Self, TrackerError> {
        let dir = dir.as_ref().to_path_buf();
        let config = SessionConfig::load(dir.join("config.toml"))?;
        Ok(Self { dir, config })
    }

    fn clips_dir(&self) -> PathBuf {
        self.dir.join("clips")
    }

    fn clip_dir(&self, id: &str) -> PathBuf {
        self.clips_dir().join(id)
    }

    pub fn clip_ids(&self) -> Result<Vec<String>, TrackerError> {
        let dir = self.clips_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.parse::<u32>().is_ok() {
                ids.push(name);
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn save_clip(
        &self,
        frames: &[StoredFrame],
        exposure_us: u32,
        gain: u32,
    ) -> Result<String, TrackerError> {
        let next = self
            .clip_ids()?
            .iter()
            .filter_map(|s| s.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("{next:04}");
        let dir = self.clip_dir(&id);
        fs::create_dir_all(&dir)?;

        let (width, height) = frames
            .first()
            .map(|f| (f.width, f.height))
            .unwrap_or((self.config.capture.width, self.config.capture.height));
        let fps = if frames.len() >= 2 {
            let dt = (frames[frames.len() - 1].t_ns as f64 - frames[0].t_ns as f64) / 1e9;
            if dt > 0.0 {
                (frames.len() - 1) as f64 / dt
            } else {
                self.config.capture.fps
            }
        } else {
            self.config.capture.fps
        };
        let sequence_gaps = frames
            .windows(2)
            .any(|w| w[1].sequence != w[0].sequence + 1);
        let meta = ClipMeta {
            width,
            height,
            fps,
            frame_count: frames.len(),
            exposure_us,
            gain,
            sequence_gaps,
        };
        fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;

        let mut left = File::create(dir.join("left.gray"))?;
        let mut depth = File::create(dir.join("depth.u16"))?;
        let mut stamps = File::create(dir.join("stamps.bin"))?;
        for fr in frames {
            left.write_all(&fr.left)?;
            write_depth(&mut depth, &fr.depth_mm)?;
            stamps.write_all(&fr.t_ns.to_le_bytes())?;
            stamps.write_all(&fr.sequence.to_le_bytes())?;
        }
        Ok(id)
    }

    pub fn begin_clip(&self, exposure_us: u32, gain: u32) -> Result<ClipWriter, TrackerError> {
        let next = self
            .clip_ids()?
            .iter()
            .filter_map(|s| s.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("{next:04}");
        let dir = self.clip_dir(&id);
        fs::create_dir_all(&dir)?;
        let left = File::create(dir.join("left.gray"))?;
        let depth = File::create(dir.join("depth.u16"))?;
        let stamps = File::create(dir.join("stamps.bin"))?;
        Ok(ClipWriter {
            id,
            dir,
            left,
            depth,
            stamps,
            count: 0,
            width: 0,
            height: 0,
            first_t: None,
            last_t: None,
            gaps: false,
            last_seq: None,
            exposure_us,
            gain,
            fps_fallback: self.config.capture.fps,
        })
    }

    pub fn load_clip(&self, id: &str) -> Result<Clip, TrackerError> {
        let dir = self.clip_dir(id);
        let meta: ClipMeta = serde_json::from_slice(&fs::read(dir.join("meta.json"))?)?;
        Ok(Clip { dir, meta })
    }

    pub fn load_detections(&self, id: &str) -> Result<Detections, TrackerError> {
        let path = self.clip_dir(id).join("detections.json");
        let dets: Detections = serde_json::from_slice(&fs::read(path)?)?;
        Ok(dets)
    }

    pub fn save_detections(&self, id: &str, dets: &Detections) -> Result<(), TrackerError> {
        let dir = self.clip_dir(id);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("detections.json"),
            serde_json::to_vec_pretty(dets)?,
        )?;
        Ok(())
    }

    pub fn hits(&self) -> Result<Vec<HitRecord>, TrackerError> {
        let path = self.dir.join("hits.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(path)?;
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            out.push(serde_json::from_str(line)?);
        }
        Ok(out)
    }

    pub fn write_hits(&self, hits: &[HitRecord]) -> Result<(), TrackerError> {
        let mut f = File::create(self.dir.join("hits.jsonl"))?;
        for hit in hits {
            serde_json::to_writer(&mut f, hit)?;
            f.write_all(b"\n")?;
        }
        Ok(())
    }
}

pub fn write_synth(dir: &Path) -> Result<crate::geom::HitEstimate, TrackerError> {
    use crate::geom::{field_point, launch_velocity, project, Intrinsics};
    use crate::{process_clip, HitEstimate};

    let mut session = Session::create(dir)?;
    session.config.capture.width = 640;
    session.config.capture.height = 400;
    session.config.capture.fps = 60.0;
    session.config.save(session.dir.join("config.toml"))?;

    let w = 640u32;
    let h = 400u32;
    let intr = Intrinsics::from_fov(w, h, 80.0, 55.0);
    let mount = &session.config.mount;
    let vel = launch_velocity(65.0, 18.0, -12.0);
    let start = [0.0, 0.9, 0.0];
    let n = 8usize;
    let fps = 60.0;
    let ground = (120i32, h as i32 - 36, 7i32);
    let mut frames = Vec::new();
    let mut det_frames = Vec::new();
    for i in 0..n {
        let t = i as f64 / fps;
        let t_ns = (t * 1e9).round() as u64;
        let p = field_point(start, vel, t);
        let mut left = vec![12u8; (w * h) as usize];
        let mut depth = vec![0u16; (w * h) as usize];
        paint_disk(
            &mut left,
            &mut depth,
            w,
            h,
            Disk {
                cx: ground.0,
                cy: ground.1,
                radius: ground.2,
                value: 40,
                depth_mm: 3200,
            },
        );
        let mut balls = vec![BallBox {
            x: (ground.0 - ground.2) as f64,
            y: (ground.1 - ground.2) as f64,
            w: (2 * ground.2) as f64,
            h: (2 * ground.2) as f64,
            conf: 0.8,
            depth_m: Some(3.2),
        }];
        if let Some((u, v, zc)) = project(p, &intr, mount) {
            if zc > 0.05 {
                let cx = u.round() as i32;
                let cy = v.round() as i32;
                let r = 8i32;
                let depth_mm = (zc * 1000.0).round().clamp(1.0, u16::MAX as f64) as u16;
                paint_disk(
                    &mut left,
                    &mut depth,
                    w,
                    h,
                    Disk {
                        cx,
                        cy,
                        radius: r,
                        value: 220,
                        depth_mm,
                    },
                );
                balls.push(BallBox {
                    x: u - r as f64,
                    y: v - r as f64,
                    w: (2 * r) as f64,
                    h: (2 * r) as f64,
                    conf: 0.95,
                    depth_m: Some(zc),
                });
            }
        }
        frames.push(StoredFrame {
            width: w,
            height: h,
            left,
            depth_mm: depth,
            t_ns,
            sequence: i as u64,
        });
        det_frames.push(DetFrame {
            index: i,
            t_ns,
            balls,
        });
    }
    let id = session.save_clip(&frames, 1000, 100)?;
    let dets = Detections { frames: det_frames };
    session.save_detections(&id, &dets)?;
    let rec = process_clip(&id, &dets.frames, &session.config, &intr, w, h)
        .ok_or_else(|| TrackerError::Other("synth clip did not produce a hit".into()))?;
    session.write_hits(std::slice::from_ref(&rec))?;
    Ok(HitEstimate {
        exit_velocity_mph: rec.exit_velocity_mph,
        launch_angle_deg: rec.launch_angle_deg,
        spray_angle_deg: rec.spray_angle_deg,
        samples: rec.samples,
        confident: rec.confident,
    })
}

struct Disk {
    cx: i32,
    cy: i32,
    radius: i32,
    value: u8,
    depth_mm: u16,
}

fn paint_disk(left: &mut [u8], depth: &mut [u16], width: u32, height: u32, disk: Disk) {
    let w = width as i32;
    let h = height as i32;
    for dy in -disk.radius..=disk.radius {
        for dx in -disk.radius..=disk.radius {
            if dx * dx + dy * dy > disk.radius * disk.radius {
                continue;
            }
            let x = disk.cx + dx;
            let y = disk.cy + dy;
            if x < 0 || y < 0 || x >= w || y >= h {
                continue;
            }
            let i = (y as u32 * width + x as u32) as usize;
            left[i] = disk.value;
            depth[i] = disk.depth_mm;
        }
    }
}
