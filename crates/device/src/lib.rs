use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct Intrinsics {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub width: u32,
    pub height: u32,
}

impl Intrinsics {
    pub fn from_fov(width: u32, height: u32, hfov_deg: f64, vfov_deg: f64) -> Self {
        let fx = (width as f64 * 0.5) / (hfov_deg.to_radians() * 0.5).tan();
        let fy = (height as f64 * 0.5) / (vfov_deg.to_radians() * 0.5).tan();
        Self {
            fx,
            fy,
            cx: width as f64 * 0.5,
            cy: height as f64 * 0.5,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub left: Vec<u8>,
    pub right: Vec<u8>,
    pub depth_mm: Vec<u16>,
    pub t_ns: u64,
    pub sequence: u64,
}

pub trait Camera {
    fn intrinsics(&self) -> Intrinsics;
    fn set_exposure_us(&mut self, exposure_us: u32) -> Result<(), String>;
    fn set_gain(&mut self, gain: u32) -> Result<(), String>;
    fn set_fps(&mut self, fps: f32) -> Result<(), String>;
    fn poll(&mut self, timeout: Duration) -> Result<Option<Frame>, String>;
}

pub struct SyntheticCamera {
    width: u32,
    height: u32,
    fps: f32,
    exposure_us: u32,
    gain: u32,
    sequence: u64,
    t_ns: u64,
}

impl SyntheticCamera {
    pub fn new(width: u32, height: u32, fps: f32) -> Self {
        Self {
            width,
            height,
            fps: fps.max(1.0),
            exposure_us: 1000,
            gain: 100,
            sequence: 0,
            t_ns: 0,
        }
    }
}

impl Camera for SyntheticCamera {
    fn intrinsics(&self) -> Intrinsics {
        Intrinsics::from_fov(self.width, self.height, 80.0, 55.0)
    }

    fn set_exposure_us(&mut self, exposure_us: u32) -> Result<(), String> {
        self.exposure_us = exposure_us.max(1);
        Ok(())
    }

    fn set_gain(&mut self, gain: u32) -> Result<(), String> {
        self.gain = gain;
        Ok(())
    }

    fn set_fps(&mut self, fps: f32) -> Result<(), String> {
        self.fps = fps.max(1.0);
        Ok(())
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Option<Frame>, String> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut left = vec![12u8; w * h];
        let mut depth = vec![0u16; w * h];
        let t = self.sequence as f64 / self.fps as f64;
        let cx = (self.width as f64 * 0.35 + t * 180.0) as i32;
        let cy = (self.height as f64 * 0.55 - t * 90.0) as i32;
        paint_disk(
            &mut left,
            &mut depth,
            self.width,
            self.height,
            Disk {
                cx,
                cy,
                radius: 8,
                value: 40,
                depth_mm: 2100,
            },
        );
        let ground_x = 100 + (self.sequence % 5) as i32;
        paint_disk(
            &mut left,
            &mut depth,
            self.width,
            self.height,
            Disk {
                cx: ground_x,
                cy: self.height as i32 - 30,
                radius: 6,
                value: 30,
                depth_mm: 2500,
            },
        );
        let frame = Frame {
            width: self.width,
            height: self.height,
            left,
            right: Vec::new(),
            depth_mm: depth,
            t_ns: self.t_ns,
            sequence: self.sequence,
        };
        self.sequence += 1;
        self.t_ns += (1e9 / self.fps as f64) as u64;
        let _ = self.exposure_us;
        Ok(Some(frame))
    }
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

#[cfg(any(test, feature = "oak"))]
mod pair;

#[cfg(feature = "oak")]
mod oak;
#[cfg(feature = "oak")]
pub use oak::{CaptureStats, OakCamera};
