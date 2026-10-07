//! The ball detector: one trained YOLO model run on grayscale frames, with
//! stereo depth measured at each box. Shared by the offline `detect` command
//! and the live watcher.

use image::{DynamicImage, GrayImage};
use tracker::{BallBox, Eye, Rectifier, StereoCalib};
use ultralytics_inference::YOLOModel;

pub struct Detector {
    yolo: YOLOModel,
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
        #[cfg(feature = "load-dynamic")]
        load_runtime()?;
        ultralytics_inference::logging::set_verbose(false);
        let yolo = YOLOModel::load(model).map_err(|e| e.to_string())?;
        println!("detector {model} on {}", yolo.execution_provider());
        Ok(Self { yolo })
    }

    pub fn provider(&self) -> &str {
        self.yolo.execution_provider()
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

fn is_ball(class_id: usize, name: &str, n_classes: usize) -> bool {
    name == "sports ball"
        || name == "ball"
        || name == "baseball"
        || class_id == 32
        || (n_classes == 1 && class_id == 0)
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

/// Open the ONNX Runtime installed on this machine. `ORT_DYLIB_PATH` names it
/// directly; otherwise it is the shared library inside the `onnxruntime`
/// Python package, which is how a Jetson gets its GPU build.
#[cfg(feature = "load-dynamic")]
fn load_runtime() -> Result<(), String> {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return Ok(());
    }
    let out = std::process::Command::new("python3")
        .args([
            "-c",
            "import onnxruntime, os; print(os.path.dirname(onnxruntime.__file__))",
        ])
        .output()
        .map_err(|e| format!("python3: {e}"))?;
    if !out.status.success() {
        return Err(
            "ONNX Runtime not found: pip install onnxruntime-gpu or set ORT_DYLIB_PATH".into(),
        );
    }
    let capi = std::path::Path::new(String::from_utf8_lossy(&out.stdout).trim()).join("capi");
    let lib = std::fs::read_dir(&capi)
        .map_err(|e| format!("{}: {e}", capi.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libonnxruntime.so"))
        })
        .ok_or_else(|| format!("no libonnxruntime.so in {}", capi.display()))?;
    ort::init_from(&lib).map_err(|e| e.to_string())?.commit();
    Ok(())
}
