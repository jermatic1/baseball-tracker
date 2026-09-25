use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use tracker::{BallBox, DetFrame, Detections};

pub async fn detect(path: PathBuf, model: String) -> Result<(), String> {
    let session = tracker::Session::open(&path).map_err(|e| e.to_string())?;
    let mut yolo = ultralytics_inference::YOLOModel::load(&model).map_err(|e| e.to_string())?;
    let tmp = std::env::temp_dir().join("tracker-detect.jpg");
    for id in session.clip_ids().map_err(|e| e.to_string())? {
        let clip = session.load_clip(&id).map_err(|e| e.to_string())?;
        let stamps = read_stamps(&clip.dir.join("stamps.bin"), clip.meta.frame_count)?;
        let mut frames = Vec::with_capacity(clip.meta.frame_count);
        for i in 0..clip.meta.frame_count {
            let left = clip.left_frame(i).map_err(|e| e.to_string())?;
            let depth = clip.depth_frame(i).map_err(|e| e.to_string())?;
            write_jpeg(&tmp, &left, clip.meta.width, clip.meta.height)?;
            let results = yolo
                .predict(tmp.to_str().ok_or("temp path is not utf-8")?)
                .map_err(|e| e.to_string())?;
            let mut balls = Vec::new();
            if let Some(result) = results.first() {
                if let Some(boxes) = &result.boxes {
                    let xyxy = boxes.xyxy();
                    for b in 0..boxes.len() {
                        let class_id = boxes.cls()[b] as usize;
                        let name = result.names.get(&class_id).map(String::as_str).unwrap_or("");
                        if class_id != 32 && name != "sports ball" {
                            continue;
                        }
                        let x1 = xyxy[[b, 0]] as f64;
                        let y1 = xyxy[[b, 1]] as f64;
                        let x2 = xyxy[[b, 2]] as f64;
                        let y2 = xyxy[[b, 3]] as f64;
                        balls.push(BallBox {
                            x: x1,
                            y: y1,
                            w: (x2 - x1).max(1.0),
                            h: (y2 - y1).max(1.0),
                            conf: boxes.conf()[b] as f64,
                            depth_m: median_depth(
                                &depth,
                                clip.meta.width,
                                clip.meta.height,
                                x1,
                                y1,
                                x2,
                                y2,
                            ),
                        });
                    }
                }
            }
            frames.push(DetFrame {
                index: i,
                t_ns: stamps.get(i).copied().unwrap_or(0),
                balls,
            });
        }
        let n: usize = frames.iter().map(|f| f.balls.len()).sum();
        session
            .save_detections(&id, &Detections { frames })
            .map_err(|e| e.to_string())?;
        println!("{id} detections={n}");
    }
    let _ = std::fs::remove_file(tmp);
    Ok(())
}

fn write_jpeg(path: &std::path::Path, gray: &[u8], width: u32, height: u32) -> Result<(), String> {
    let img = image::GrayImage::from_raw(width, height, gray.to_vec())
        .ok_or("frame size does not match pixels")?;
    img.save(path).map_err(|e| e.to_string())
}

fn read_stamps(path: &std::path::Path, n: usize) -> Result<Vec<u64>, String> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(vec![0; n]),
    };
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(n);
    for chunk in bytes.chunks_exact(16).take(n) {
        let t = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
        out.push(t);
    }
    Ok(out)
}

fn median_depth(
    depth: &[u16],
    width: u32,
    height: u32,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
) -> Option<f64> {
    let x0 = x1.max(0.0) as u32;
    let y0 = y1.max(0.0) as u32;
    let x3 = (x2 as u32).min(width);
    let y3 = (y2 as u32).min(height);
    let mut vals = Vec::new();
    for y in y0..y3 {
        for x in x0..x3 {
            let d = depth[(y * width + x) as usize];
            if d > 0 {
                vals.push(d);
            }
        }
    }
    if vals.is_empty() {
        return None;
    }
    vals.sort_unstable();
    Some(vals[vals.len() / 2] as f64 / 1000.0)
}
