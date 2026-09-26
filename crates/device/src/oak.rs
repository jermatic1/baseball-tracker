use std::collections::VecDeque;
use std::time::Duration;

use depthai::camera::{
    CameraBoardSocket, CameraNode, CameraOutputConfig, ImageFrame, ImageFrameType, ManualExposure,
    ResizeMode,
};
use depthai::pipeline::Pipeline;
use depthai::{Device, InputQueue, MessageQueue, StereoDepthNode, StereoPresetMode};

use crate::pair::{self, Stamped};
use crate::{Camera, Frame, Intrinsics};

const QUEUE: u32 = 32;
const PENDING: usize = 8;

pub struct OakCamera {
    _pipeline: Pipeline,
    q_left: MessageQueue,
    q_depth: MessageQueue,
    ctrl_left: InputQueue,
    ctrl_right: InputQueue,
    width: u32,
    height: u32,
    intrinsics: Intrinsics,
    exposure_us: u32,
    gain: u32,
    controls_sent: bool,
    pending_left: VecDeque<Stamped<Vec<u8>>>,
    pending_depth: VecDeque<Stamped<Vec<u16>>>,
    logged_size: bool,
}

impl OakCamera {
    pub fn open(
        width: u32,
        height: u32,
        fps: f32,
        exposure_us: u32,
        gain: u32,
    ) -> Result<Self, String> {
        let exposure_us = exposure_us.max(1);
        let iso = gain.clamp(100, 1600);
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
        left.set_initial_manual_exposure(exposure_us, iso)
            .map_err(|e| e.to_string())?;
        right
            .set_initial_manual_exposure(exposure_us, iso)
            .map_err(|e| e.to_string())?;
        let cam_cfg = CameraOutputConfig {
            size: (width, height),
            frame_type: Some(ImageFrameType::GRAY8),
            resize_mode: ResizeMode::Crop,
            fps: Some(fps),
            enable_undistortion: None,
        };
        let out_left = left
            .request_output(cam_cfg.clone())
            .map_err(|e| e.to_string())?;
        let out_right = right.request_output(cam_cfg).map_err(|e| e.to_string())?;
        let stereo = pipeline
            .create::<StereoDepthNode>()
            .map_err(|e| e.to_string())?;
        stereo.set_default_profile_preset(StereoPresetMode::Robotics);
        stereo.set_left_right_check(true);
        stereo
            .set_input_resolution(width as i32, height as i32)
            .map_err(|e| e.to_string())?;
        stereo
            .set_temporal_filter(false)
            .map_err(|e| e.to_string())?;
        stereo
            .set_spatial_filter(false)
            .map_err(|e| e.to_string())?;
        stereo.set_decimation(1).map_err(|e| e.to_string())?;
        stereo.set_output_size(width as i32, height as i32);
        out_left
            .link_to(stereo.as_node(), Some("left"))
            .map_err(|e| e.to_string())?;
        out_right
            .link_to(stereo.as_node(), Some("right"))
            .map_err(|e| e.to_string())?;
        let rectified = stereo
            .as_node()
            .output("rectifiedLeft")
            .map_err(|e| e.to_string())?;
        let depth_src = stereo.depth().map_err(|e| e.to_string())?;
        let q_left = rectified
            .create_message_queue(QUEUE, false)
            .map_err(|e| e.to_string())?;
        let q_depth = depth_src
            .create_message_queue(QUEUE, false)
            .map_err(|e| e.to_string())?;
        let ctrl_left = left
            .inputControl()
            .map_err(|e| e.to_string())?
            .create_input_queue(4, false)
            .map_err(|e| e.to_string())?;
        let ctrl_right = right
            .inputControl()
            .map_err(|e| e.to_string())?
            .create_input_queue(4, false)
            .map_err(|e| e.to_string())?;
        pipeline.start().map_err(|e| e.to_string())?;
        let mut cam = Self {
            _pipeline: pipeline,
            q_left,
            q_depth,
            ctrl_left,
            ctrl_right,
            width,
            height,
            intrinsics: Intrinsics::from_fov(width, height, 80.0, 55.0),
            exposure_us,
            gain: iso,
            controls_sent: false,
            pending_left: VecDeque::new(),
            pending_depth: VecDeque::new(),
            logged_size: false,
        };
        cam.send_exposure(exposure_us, iso)?;
        cam.controls_sent = true;
        println!("manual exposure {exposure_us} us iso {iso}");
        Ok(cam)
    }

    pub fn apply_controls(&mut self, exposure_us: u32, gain: u32) -> Result<(), String> {
        let exposure_us = exposure_us.max(1);
        let iso = gain.clamp(100, 1600);
        if self.controls_sent && self.exposure_us == exposure_us && self.gain == iso {
            return Ok(());
        }
        self.send_exposure(exposure_us, iso)?;
        self.exposure_us = exposure_us;
        self.gain = iso;
        self.controls_sent = true;
        println!("manual exposure {exposure_us} us iso {iso}");
        Ok(())
    }

    fn send_exposure(&self, exposure_us: u32, iso: u32) -> Result<(), String> {
        let left = ManualExposure::new(exposure_us, iso).map_err(|e| e.to_string())?;
        let right = ManualExposure::new(exposure_us, iso).map_err(|e| e.to_string())?;
        self.ctrl_left
            .send_buffer(&left)
            .map_err(|e| e.to_string())?;
        self.ctrl_right
            .send_buffer(&right)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn drain(&mut self) -> Result<(), String> {
        while let Some(frame) = try_frame(&self.q_left)? {
            let packed = pack_left(&frame, self.width, self.height)?;
            pair::push_pending(&mut self.pending_left, packed, PENDING);
        }
        while let Some(frame) = try_frame(&self.q_depth)? {
            let packed = pack_depth(&frame, self.width, self.height)?;
            pair::push_pending(&mut self.pending_depth, packed, PENDING);
        }
        Ok(())
    }
}

impl Camera for OakCamera {
    fn intrinsics(&self) -> Intrinsics {
        self.intrinsics
    }

    fn set_exposure_us(&mut self, exposure_us: u32) -> Result<(), String> {
        self.apply_controls(exposure_us, self.gain)
    }

    fn set_gain(&mut self, gain: u32) -> Result<(), String> {
        self.apply_controls(self.exposure_us, gain)
    }

    fn set_fps(&mut self, fps: f32) -> Result<(), String> {
        let _ = fps;
        Err("fps is fixed at open".into())
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Option<Frame>, String> {
        self.drain()?;
        let Some((left, depth)) =
            pair::take_matched(&mut self.pending_left, &mut self.pending_depth)
        else {
            return Ok(None);
        };
        if !self.logged_size {
            self.logged_size = true;
            println!(
                "left {}x{} depth {}x{}",
                self.width, self.height, self.width, self.height
            );
        }
        Ok(Some(Frame {
            width: self.width,
            height: self.height,
            left: left.value,
            depth_mm: depth.value,
            t_ns: left.t_ns,
            sequence: left.seq,
        }))
    }
}

fn try_frame(q: &MessageQueue) -> Result<Option<ImageFrame>, String> {
    match q.try_get().map_err(|e| e.to_string())? {
        None => Ok(None),
        Some(m) => m.as_frame().map_err(|e| e.to_string()),
    }
}

fn stamp_of(frame: &ImageFrame) -> Result<(u64, u64), String> {
    let seq = frame.sequence_num().map_err(|e| e.to_string())?.max(0) as u64;
    let t_ns = frame
        .timestamp_device()
        .map(|t| t.as_nanoseconds().max(0) as u64)
        .unwrap_or(0);
    Ok((seq, t_ns))
}

fn pack_left(frame: &ImageFrame, width: u32, height: u32) -> Result<Stamped<Vec<u8>>, String> {
    let (seq, t_ns) = stamp_of(frame)?;
    Ok(Stamped {
        seq,
        t_ns,
        value: copy_gray(frame, width, height)?,
    })
}

fn pack_depth(frame: &ImageFrame, width: u32, height: u32) -> Result<Stamped<Vec<u16>>, String> {
    let (seq, t_ns) = stamp_of(frame)?;
    Ok(Stamped {
        seq,
        t_ns,
        value: copy_depth_mm(frame, width, height)?,
    })
}

fn require_size(kind: &str, src_w: u32, src_h: u32, width: u32, height: u32) -> Result<(), String> {
    if src_w != width || src_h != height {
        return Err(format!(
            "frame size {kind} is {src_w}x{src_h}, expected {width}x{height}"
        ));
    }
    Ok(())
}

fn copy_gray(frame: &ImageFrame, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let src_w = frame.width();
    let src_h = frame.height();
    require_size("left", src_w, src_h, width, height)?;
    let bytes = frame.as_bytes().map_err(|e| e.to_string())?;
    let stride = frame.stride().ok().filter(|&s| s > 0).unwrap_or(src_w) as usize;
    let row = width as usize;
    let mut out = vec![0u8; row * height as usize];
    for y in 0..height as usize {
        let src = y * stride;
        let dst = y * row;
        if src + row > bytes.len() {
            return Err("left frame shorter than its size".into());
        }
        out[dst..dst + row].copy_from_slice(&bytes[src..src + row]);
    }
    Ok(out)
}

fn copy_depth_mm(frame: &ImageFrame, width: u32, height: u32) -> Result<Vec<u16>, String> {
    let src_w = frame.width();
    let src_h = frame.height();
    require_size("depth", src_w, src_h, width, height)?;
    let bytes = frame.as_bytes().map_err(|e| e.to_string())?;
    let stride = frame
        .stride()
        .ok()
        .filter(|&s| s > 0)
        .unwrap_or(src_w.saturating_mul(2)) as usize;
    let row = width as usize;
    let row_bytes = row * 2;
    let mut out = vec![0u16; row * height as usize];
    if cfg!(target_endian = "little")
        && stride == row_bytes
        && bytes.len() >= row_bytes * height as usize
    {
        let dst =
            unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut u8, out.len() * 2) };
        dst.copy_from_slice(&bytes[..out.len() * 2]);
        return Ok(out);
    }
    for y in 0..height as usize {
        let src = y * stride;
        if src + row_bytes > bytes.len() {
            return Err("depth frame shorter than its size".into());
        }
        let row_src = &bytes[src..src + row_bytes];
        let dst = &mut out[y * row..(y + 1) * row];
        for (d, chunk) in dst.iter_mut().zip(row_src.chunks_exact(2)) {
            *d = u16::from_le_bytes([chunk[0], chunk[1]]);
        }
    }
    Ok(out)
}
