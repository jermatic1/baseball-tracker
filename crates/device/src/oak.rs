use std::collections::VecDeque;
use std::time::{Duration, Instant};

use depthai::camera::{
    CameraBoardSocket, CameraBuildConfig, CameraOutputConfig, ImageFrame, ImageFrameType,
    ManualExposure, ResizeMode,
};
use depthai::pipeline::Pipeline;
use depthai::{Device, InputQueue, MessageQueue};

use crate::pair::{self, Stamped};
use crate::{Camera, CaptureStats, Frame, Intrinsics};

const QUEUE: u32 = 120;
const FRAME_POOL: i32 = 16;
const STATS_PERIOD: Duration = Duration::from_secs(1);
/// Aggregate host throughput the RVC2 XLink firmware sustains, per Luxonis.
const XLINK_LIMIT_MB_S: f64 = 150.0;

#[derive(Default)]
struct Counters {
    stats: CaptureStats,
    last_seq_left: Option<u64>,
    last_seq_right: Option<u64>,
    since: Option<Instant>,
}

pub struct OakCamera {
    device: Device,
    _pipeline: Pipeline,
    q_left: MessageQueue,
    q_right: MessageQueue,
    ctrl_left: InputQueue,
    ctrl_right: InputQueue,
    width: u32,
    height: u32,
    intrinsics: Intrinsics,
    exposure_us: u32,
    gain: u32,
    ir_flood: f32,
    ir_dot: f32,
    controls_sent: bool,
    lights_sent: bool,
    pending_left: VecDeque<Stamped<Vec<u8>>>,
    pending_right: VecDeque<Stamped<Vec<u8>>>,
    calibration: Option<String>,
    counters: Counters,
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
        warn_if_over_budget(width, height, fps);
        let _ = device.set_ir_flood_light_intensity(0.0);
        let _ = device.set_ir_laser_dot_projector_intensity(0.0);
        let pipeline = Pipeline::new()
            .with_device(&device)
            .xlink_chunk_size(0)
            .build()
            .map_err(|e| e.to_string())?;
        let out_cfg = CameraOutputConfig {
            size: (width, height),
            frame_type: Some(ImageFrameType::GRAY8),
            resize_mode: ResizeMode::Stretch,
            fps: Some(fps),
            enable_undistortion: None,
        };
        let open_eye = |socket: CameraBoardSocket| -> Result<(MessageQueue, InputQueue), String> {
            // Pin the sensor mode so a 640x400 request uses the binned readout
            // rather than a downscaled full-resolution frame.
            let cam = pipeline
                .create_camera_unbuilt()
                .map_err(|e| e.to_string())?;
            cam.build(CameraBuildConfig {
                board_socket: socket,
                sensor_resolution: Some((width, height)),
                sensor_fps: Some(fps),
            })
            .map_err(|e| e.to_string())?;
            cam.set_raw_num_frames_pool(FRAME_POOL)
                .map_err(|e| e.to_string())?;
            cam.set_isp_num_frames_pool(FRAME_POOL)
                .map_err(|e| e.to_string())?;
            cam.set_initial_manual_exposure(exposure_us, iso)
                .map_err(|e| e.to_string())?;
            let queue = cam
                .request_output(out_cfg.clone())
                .map_err(|e| e.to_string())?
                .create_message_queue(QUEUE, false)
                .map_err(|e| e.to_string())?;
            let ctrl = cam
                .inputControl()
                .map_err(|e| e.to_string())?
                .create_input_queue(4, false)
                .map_err(|e| e.to_string())?;
            Ok((queue, ctrl))
        };
        let (q_left, ctrl_left) = open_eye(CameraBoardSocket::CamB)?;
        let (q_right, ctrl_right) = open_eye(CameraBoardSocket::CamC)?;
        pipeline.start().map_err(|e| e.to_string())?;
        let calibration = pipeline
            .calibration_data_json()
            .ok()
            .flatten()
            .map(|v| v.to_string());
        let mut cam = Self {
            device,
            _pipeline: pipeline,
            q_left,
            q_right,
            ctrl_left,
            ctrl_right,
            width,
            height,
            intrinsics: Intrinsics::from_fov(width, height, 80.0, 55.0),
            exposure_us,
            gain: iso,
            ir_flood: 0.0,
            ir_dot: 0.0,
            controls_sent: false,
            lights_sent: true,
            pending_left: VecDeque::new(),
            pending_right: VecDeque::new(),
            calibration,
            counters: Counters::default(),
        };
        cam.send_exposure(exposure_us, iso)?;
        cam.controls_sent = true;
        println!(
            "left+right mono {width}x{height} @ {fps} fps, exposure {exposure_us} us iso {iso}"
        );
        Ok(cam)
    }

    fn note_received(&mut self, side: Side, seq: u64) {
        let c = &mut self.counters;
        let (count, gaps, last) = match side {
            Side::Left => (
                &mut c.stats.left,
                &mut c.stats.left_gaps,
                &mut c.last_seq_left,
            ),
            Side::Right => (
                &mut c.stats.right,
                &mut c.stats.right_gaps,
                &mut c.last_seq_right,
            ),
        };
        *count += 1;
        if let Some(prev) = *last {
            if seq != prev + 1 {
                *gaps += 1;
            }
        }
        *last = Some(seq);
    }

    fn drain(&mut self, side: Side) -> Result<(), String> {
        loop {
            let frame = match side {
                Side::Left => try_frame(&self.q_left)?,
                Side::Right => try_frame(&self.q_right)?,
            };
            let Some(frame) = frame else {
                return Ok(());
            };
            let stamped = pack_gray(&frame, self.width, self.height)?;
            self.note_received(side, stamped.seq);
            let pending = match side {
                Side::Left => &mut self.pending_left,
                Side::Right => &mut self.pending_right,
            };
            let before = pending.len();
            pair::push_pending(pending, stamped, QUEUE as usize);
            if pending.len() == before {
                self.counters.stats.unpaired += 1;
            }
        }
    }

    fn send_exposure(&self, exposure_us: u32, iso: u32) -> Result<(), String> {
        for ctrl in [&self.ctrl_left, &self.ctrl_right] {
            let control = ManualExposure::new(exposure_us, iso).map_err(|e| e.to_string())?;
            ctrl.send_buffer(&control).map_err(|e| e.to_string())?;
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

    fn apply_controls(&mut self, exposure_us: u32, gain: u32) -> Result<(), String> {
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

    fn apply_lights(&mut self, flood: f32, dot: f32) -> Result<(), String> {
        let flood = flood.clamp(0.0, 1.0);
        let dot = dot.clamp(0.0, 1.0);
        if self.lights_sent
            && (self.ir_flood - flood).abs() < 0.001
            && (self.ir_dot - dot).abs() < 0.001
        {
            return Ok(());
        }
        let flood_result = self
            .device
            .set_ir_flood_light_intensity(flood)
            .map_err(|e| e.to_string());
        let dot_result = self
            .device
            .set_ir_laser_dot_projector_intensity(dot)
            .map_err(|e| e.to_string());
        self.ir_flood = flood;
        self.ir_dot = dot;
        self.lights_sent = true;
        flood_result?;
        dot_result?;
        println!("ir flood {flood:.2} dot {dot:.2}");
        Ok(())
    }

    fn calibration_json(&self) -> Option<String> {
        self.calibration.clone()
    }

    /// Counts since the previous call, once a full stats period has elapsed.
    fn take_stats(&mut self) -> Option<CaptureStats> {
        let now = Instant::now();
        let since = *self.counters.since.get_or_insert(now);
        if now.duration_since(since) < STATS_PERIOD {
            return None;
        }
        let mut stats = std::mem::take(&mut self.counters.stats);
        stats.seconds = now.duration_since(since).as_secs_f64();
        self.counters.since = Some(now);
        Some(stats)
    }

    fn poll(&mut self, _timeout: Duration) -> Result<Option<Frame>, String> {
        self.drain(Side::Left)?;
        self.drain(Side::Right)?;
        let before = self.pending_left.len() + self.pending_right.len();
        let matched =
            pair::take_matched_within(&mut self.pending_left, &mut self.pending_right, 2_000_000);
        let consumed = before - self.pending_left.len() - self.pending_right.len();
        let Some((left, right)) = matched else {
            self.counters.stats.unpaired += consumed as u64;
            return Ok(None);
        };
        self.counters.stats.pairs += 1;
        self.counters.stats.unpaired += (consumed - 2) as u64;
        Ok(Some(Frame {
            width: self.width,
            height: self.height,
            left: left.value,
            right: right.value,
            depth_mm: Vec::new(),
            t_ns: left.t_ns,
            sequence: left.seq,
        }))
    }
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

fn warn_if_over_budget(width: u32, height: u32, fps: f32) {
    let frame_bytes = f64::from(width) * f64::from(height) * 2.0;
    let mb_s = frame_bytes * f64::from(fps) / 1e6;
    if mb_s > XLINK_LIMIT_MB_S {
        let max_fps = XLINK_LIMIT_MB_S * 1e6 / frame_bytes;
        println!(
            "warning: {width}x{height} @ {fps} fps needs {mb_s:.0} MB/s, over the ~{XLINK_LIMIT_MB_S:.0} MB/s XLink limit; expect at most ~{max_fps:.0} fps"
        );
    }
}

fn try_frame(q: &MessageQueue) -> Result<Option<ImageFrame>, String> {
    match q.try_get().map_err(|e| e.to_string())? {
        None => Ok(None),
        Some(m) => m.as_frame().map_err(|e| e.to_string()),
    }
}

fn pack_gray(frame: &ImageFrame, width: u32, height: u32) -> Result<Stamped<Vec<u8>>, String> {
    let seq = frame.sequence_num().map_err(|e| e.to_string())?.max(0) as u64;
    let t_ns = frame
        .timestamp_device()
        .map(|t| t.as_nanoseconds().max(0) as u64)
        .unwrap_or(0);
    Ok(Stamped {
        seq,
        t_ns,
        value: copy_gray(frame, width, height)?,
    })
}

fn copy_gray(frame: &ImageFrame, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let src_w = frame.width();
    let src_h = frame.height();
    if src_w != width || src_h != height {
        return Err(format!(
            "frame size is {src_w}x{src_h}, expected {width}x{height}"
        ));
    }
    let bytes = frame.as_bytes().map_err(|e| e.to_string())?;
    let stride = frame.stride().ok().filter(|&s| s > 0).unwrap_or(src_w) as usize;
    let row = width as usize;
    let mut out = vec![0u8; row * height as usize];
    for y in 0..height as usize {
        let src = y * stride;
        if src + row > bytes.len() {
            return Err("frame shorter than its size".into());
        }
        out[y * row..(y + 1) * row].copy_from_slice(&bytes[src..src + row]);
    }
    Ok(out)
}
