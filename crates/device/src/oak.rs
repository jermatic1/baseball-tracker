use std::time::Duration;

use depthai::camera::{
    CameraBoardSocket, CameraNode, CameraOutputConfig, ImageFrame, ImageFrameType, ResizeMode,
};
use depthai::pipeline::Pipeline;
use depthai::{Device, MessageQueue, StereoDepthNode, StereoPresetMode};

use crate::{Camera, Frame, Intrinsics};

pub struct OakCamera {
    _pipeline: Pipeline,
    q_left: MessageQueue,
    q_depth: MessageQueue,
    width: u32,
    height: u32,
    intrinsics: Intrinsics,
    exposure_us: u32,
    gain: u32,
}

impl OakCamera {
    pub fn open(width: u32, height: u32, fps: f32, exposure_us: u32, gain: u32) -> Result<Self, String> {
        let device = Device::new().map_err(|e| e.to_string())?;
        let _ = device.set_ir_laser_dot_projector_intensity(0.0);
        let pipeline = Pipeline::new()
            .with_device(&device)
            .build()
            .map_err(|e| e.to_string())?;
        let left = pipeline
            .create_with::<CameraNode, _>(CameraBoardSocket::CamB)
            .map_err(|e| e.to_string())?;
        let right = pipeline
            .create_with::<CameraNode, _>(CameraBoardSocket::CamC)
            .map_err(|e| e.to_string())?;
        let cam_cfg = CameraOutputConfig {
            size: (width, height),
            frame_type: Some(ImageFrameType::GRAY8),
            resize_mode: ResizeMode::Crop,
            fps: Some(fps),
            enable_undistortion: None,
        };
        let out_left = left.request_output(cam_cfg.clone()).map_err(|e| e.to_string())?;
        let out_right = right.request_output(cam_cfg).map_err(|e| e.to_string())?;
        let stereo = pipeline
            .create::<StereoDepthNode>()
            .map_err(|e| e.to_string())?;
        stereo.set_default_profile_preset(StereoPresetMode::Robotics);
        stereo.set_left_right_check(true);
        stereo.set_output_size(width as i32, height as i32);
        out_left
            .link_to(stereo.as_node(), Some("left"))
            .map_err(|e| e.to_string())?;
        out_right
            .link_to(stereo.as_node(), Some("right"))
            .map_err(|e| e.to_string())?;
        let _ = stereo.as_node().output("rectifiedLeft");
        let depth_src = stereo.depth().map_err(|e| e.to_string())?;
        let q_left = out_left
            .create_message_queue(4, false)
            .map_err(|e| e.to_string())?;
        let q_depth = depth_src
            .create_message_queue(4, false)
            .map_err(|e| e.to_string())?;
        pipeline.start().map_err(|e| e.to_string())?;
        Ok(Self {
            _pipeline: pipeline,
            q_left,
            q_depth,
            width,
            height,
            intrinsics: Intrinsics::from_fov(width, height, 80.0, 55.0),
            exposure_us,
            gain,
        })
    }
}

impl Camera for OakCamera {
    fn intrinsics(&self) -> Intrinsics {
        self.intrinsics
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
        let _ = fps;
        Err("fps is fixed at open".into())
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Option<Frame>, String> {
        let Some(left) = try_frame(&self.q_left)? else {
            return Ok(None);
        };
        let depth = try_frame(&self.q_depth)?;
        build_frame(&left, depth.as_ref(), self.width, self.height).map(Some)
    }
}

fn try_frame(q: &MessageQueue) -> Result<Option<ImageFrame>, String> {
    match q.try_get().map_err(|e| e.to_string())? {
        None => Ok(None),
        Some(m) => m.as_frame().map_err(|e| e.to_string()),
    }
}

fn build_frame(left: &ImageFrame, depth: Option<&ImageFrame>, width: u32, height: u32) -> Result<Frame, String> {
    let t_ns = match left.timestamp_device() {
        Ok(t) => t.as_nanoseconds().max(0) as u64,
        Err(_) => {
            // Device timestamp unavailable; host time in nanoseconds.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        }
    };
    let sequence = left.sequence_num().ok().filter(|&s| s >= 0).unwrap_or(0) as u64;
    Ok(Frame {
        width,
        height,
        left: copy_gray(left, width, height)?,
        depth_mm: match depth {
            Some(frame) => copy_depth_mm(frame, width, height)?,
            None => vec![0; width as usize * height as usize],
        },
        t_ns,
        sequence,
    })
}

fn copy_gray(frame: &ImageFrame, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let src_w = frame.width();
    let src_h = frame.height();
    let bytes = frame.as_bytes().map_err(|e| e.to_string())?;
    let stride = frame
        .stride()
        .ok()
        .filter(|&s| s > 0)
        .unwrap_or(src_w);
    let mut out = vec![0u8; width as usize * height as usize];
    let copy_w = src_w.min(width) as usize;
    let copy_h = src_h.min(height) as usize;
    for y in 0..copy_h {
        let src = y * stride as usize;
        let dst = y * width as usize;
        if src + copy_w <= bytes.len() {
            out[dst..dst + copy_w].copy_from_slice(&bytes[src..src + copy_w]);
        }
    }
    Ok(out)
}

fn copy_depth_mm(frame: &ImageFrame, width: u32, height: u32) -> Result<Vec<u16>, String> {
    let src_w = frame.width();
    let src_h = frame.height();
    let bytes = frame.as_bytes().map_err(|e| e.to_string())?;
    let stride = frame
        .stride()
        .ok()
        .filter(|&s| s > 0)
        .unwrap_or(src_w.saturating_mul(2));
    let mut out = vec![0u16; width as usize * height as usize];
    let copy_w = src_w.min(width) as usize;
    let copy_h = src_h.min(height) as usize;
    for y in 0..copy_h {
        let row = y * stride as usize;
        let dst_row = y * width as usize;
        for x in 0..copy_w {
            let i = row + x * 2;
            if i + 1 < bytes.len() {
                out[dst_row + x] = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
            }
        }
    }
    Ok(out)
}
