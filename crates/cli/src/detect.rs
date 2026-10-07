use std::path::PathBuf;
use std::time::Instant;

use tracker::{DetFrame, Detections, StereoCalib};

use crate::detector::{Detector, FrameView};

pub async fn detect(path: PathBuf, model: String, clip: Option<String>) -> Result<(), String> {
    let session = tracker::Session::open(&path).map_err(|e| e.to_string())?;
    let mut detector = Detector::load(&model)?;
    let mut ids = session.clip_ids().map_err(|e| e.to_string())?;
    if let Some(clip) = clip {
        let id = match clip.parse::<u32>() {
            Ok(n) => format!("{n:04}"),
            Err(_) => clip,
        };
        if !ids.iter().any(|s| s == &id) {
            return Err(format!("no clip {id}"));
        }
        ids = vec![id];
    }
    let offset = session.config.stereo.disparity_offset_px;
    for id in ids {
        let clip = session.load_clip(&id).map_err(|e| e.to_string())?;
        // Factory rectification when the session saved its calibration;
        // otherwise raw frames with the hand-calibrated offset.
        let rectifier = session.rectifier(clip.meta.width, clip.meta.height);
        let calib = match &rectifier {
            Some(r) => r.calib(),
            None => StereoCalib::nominal(clip.meta.width).with_offset(offset),
        };
        let stamps = clip.stamps().map_err(|e| e.to_string())?;
        let started = Instant::now();
        let mut frames = Vec::with_capacity(clip.meta.frame_count);
        for i in 0..clip.meta.frame_count {
            let left = clip.left_frame(i).map_err(|e| e.to_string())?;
            let right = clip.right_frame(i).ok();
            let depth = clip.depth_frame(i).map_err(|e| e.to_string())?;
            let balls = detector.detect(&FrameView {
                width: clip.meta.width,
                height: clip.meta.height,
                left: &left,
                right: right.as_deref(),
                depth: Some(&depth),
                rectifier: rectifier.as_ref(),
                calib,
            })?;
            frames.push(DetFrame {
                index: i,
                t_ns: stamps.get(i).copied().unwrap_or(0),
                balls,
            });
        }
        let fps = clip.meta.frame_count as f64 / started.elapsed().as_secs_f64().max(1e-6);
        let n: usize = frames.iter().map(|f| f.balls.len()).sum();
        session
            .save_detections(&id, &Detections { frames })
            .map_err(|e| e.to_string())?;
        println!("{id} detections={n} {fps:.0} fps");
    }
    Ok(())
}
