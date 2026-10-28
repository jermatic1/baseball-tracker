//! The ball detector: one trained YOLO model run on grayscale frames, with
//! stereo depth measured at each box. Shared by the offline `detect` command
//! and the live watcher.

use image::{DynamicImage, GrayImage};
use tracker::{BallBox, Eye, Rectifier, StereoCalib};
use ultralytics_inference::{InferenceConfig, Quantization, YOLOModel};

pub struct Detector {
    yolo: YOLOModel,
    provider: &'static str,
}

/// One frame as the detector needs it. `depth` is a saved depth map, used
/// only when stereo matching at a box fails.
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub left: &'a [u8],
    pub right: Option<&'a [u8]>,
    pub depth: Option<&'a [u16]>,
    pub rectifier: Option<&'a Rectifier>,
    pub calib: StereoCalib,
}

impl Detector {
    pub fn load(model: &str) -> Result<Self, String> {
        load_runtime()?;
        let provider = available_provider();
        ultralytics_inference::logging::set_verbose(false);
        let config = InferenceConfig::default().with_quantize(Quantization::Fp16);
        let yolo = YOLOModel::load_with_config(model, config).map_err(|e| e.to_string())?;
        println!("detector {model} on {provider}");
        Ok(Self { yolo, provider })
    }

    pub fn provider(&self) -> &str {
        self.provider
    }

    pub fn detect(&mut self, frame: &FrameView) -> Result<Vec<BallBox>, String> {
        let (w, h) = (frame.width, frame.height);
        let gray = GrayImage::from_raw(w, h, frame.left.to_vec())
            .ok_or("frame size does not match pixels")?;
        let results = self
            .yolo
            .predict_image(&DynamicImage::ImageLuma8(gray), String::new())
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        if let Some(result) = results.first() {
            if let Some(boxes) = &result.boxes {
                let xyxy = boxes.xyxy();
                for b in 0..boxes.len() {
                    let class_id = boxes.cls()[b] as usize;
                    let name = result
                        .names
                        .get(&class_id)
                        .map(String::as_str)
                        .unwrap_or("");
                    if !is_ball(class_id, name, result.names.len()) {
                        continue;
                    }
                    raw.push((
                        xyxy[[b, 0]] as f64,
                        xyxy[[b, 1]] as f64,
                        xyxy[[b, 2]] as f64,
                        xyxy[[b, 3]] as f64,
                        boxes.conf()[b] as f64,
                    ));
                }
            }
        }
        if raw.is_empty() {
            return Ok(Vec::new());
        }
        // Rectify only when there is something to measure: most frames have
        // no ball, and the remap would cost more than the detector itself.
        let pair = match (frame.rectifier, frame.right) {
            (Some(r), Some(right)) => Some((
                r.rectify(frame.left, Eye::Left),
                r.rectify(right, Eye::Right),
            )),
            (None, Some(right)) => Some((frame.left.to_vec(), right.to_vec())),
            _ => None,
        };
        Ok(raw
            .into_iter()
            .map(|(x1, y1, x2, y2, conf)| {
                let (cx, cy) = ((x1 + x2) * 0.5, (y1 + y2) * 0.5);
                let (cx, cy) = match frame.rectifier {
                    Some(r) => r.to_rectified_left(cx, cy),
                    None => (cx, cy),
                };
                let precise = pair
                    .as_ref()
                    .and_then(|(l, r)| tracker::stereo::depth_at(l, r, w, h, cx, cy, frame.calib));
                BallBox {
                    x: x1,
                    y: y1,
                    w: (x2 - x1).max(1.0),
                    h: (y2 - y1).max(1.0),
                    conf,
                    depth_m: precise.or_else(|| {
                        frame
                            .depth
                            .and_then(|d| median_depth(d, w, h, x1, y1, x2, y2))
                    }),
                }
            })
            .collect())
    }
}

/// A named class is a ball by name, so a trained model may list other classes
/// in any order. An unnamed one is a ball as the only class or as COCO's 32.
fn is_ball(class_id: usize, name: &str, n_classes: usize) -> bool {
    if !name.is_empty() {
        return matches!(name, "ball" | "baseball" | "sports ball");
    }
    (n_classes == 1 && class_id == 0) || class_id == 32
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

/// Open the ONNX Runtime shipped next to the executable, unless `ORT_DYLIB_PATH`
/// names one.
fn load_runtime() -> Result<(), String> {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return Ok(());
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .ok_or("cannot locate the tracker executable")?;
    let lib = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libonnxruntime.so"))
        })
        .ok_or_else(|| {
            format!(
                "no libonnxruntime.so in {}; run `task onnxruntime` or set ORT_DYLIB_PATH",
                dir.display()
            )
        })?;
    ort::init_from(&lib).map_err(|e| e.to_string())?.commit();
    Ok(())
}

/// The first provider ONNX Runtime can open, in the order the model loader
/// registers them. The model loader falls back to the CPU silently, so a GPU
/// provider missing its CUDA, cuDNN, or TensorRT libraries is reported here.
fn available_provider() -> &'static str {
    let gpus = [
        (
            ort::ep::TensorRT::default().build(),
            "TensorRTExecutionProvider",
        ),
        (ort::ep::CUDA::default().build(), "CUDAExecutionProvider"),
    ];
    for (ep, name) in gpus {
        let probe = ort::session::Session::builder()
            .map_err(|e| e.to_string())
            .and_then(|b| {
                b.with_execution_providers([ep.error_on_failure()])
                    .map_err(|e| e.to_string())
            });
        match probe {
            Ok(_) => return name,
            Err(e) => eprintln!("warning: {name} unavailable: {e}"),
        }
    }
    "CPUExecutionProvider"
}

#[cfg(test)]
mod tests {
    use super::is_ball;

    #[test]
    fn picks_the_ball_class_by_name_or_position() {
        assert!(is_ball(0, "ball", 1));
        assert!(is_ball(1, "ball", 3));
        assert!(!is_ball(0, "glove", 3));
        assert!(is_ball(32, "sports ball", 80));
        assert!(!is_ball(0, "person", 80));
        assert!(is_ball(0, "", 1));
        assert!(!is_ball(1, "", 2));
    }
}
