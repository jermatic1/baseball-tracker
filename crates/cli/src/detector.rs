//! The ball detector: one trained YOLO model run on grayscale frames, with
//! stereo depth measured at each box. Shared by the offline `detect` command
//! and the live watcher.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::{DynamicImage, GrayImage};
use tracker::{BallBox, Eye, Rectifier, StereoCalib};
use ultralytics_inference::{InferenceConfig, Quantization, YOLOModel};

pub struct Detector {
    yolo: YOLOModel,
    provider: &'static str,
    /// SHA-1 of the model file.
    hash: String,
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
        Self::open(model, None)
    }

    /// Load `model` and run one `width`x`height` frame through it, so its
    /// engine is built. On failure the previous model's engine cache is kept.
    pub fn load_warm(model: &str, width: u32, height: u32) -> Result<Self, String> {
        Self::open(model, Some((width, height)))
    }

    fn open(model: &str, warm: Option<(u32, u32)>) -> Result<Self, String> {
        let provider = runtime()?;
        ultralytics_inference::logging::set_verbose(false);
        let config = InferenceConfig::default().with_quantize(Quantization::Fp16);
        let cache = engine_cache_dir(Path::new(model));
        let hash = tracker::model_record::file_sha1(Path::new(model))?;
        let aside = set_aside_stale_cache(&cache, &hash)?;
        if aside.is_some() {
            println!(
                "set aside TensorRT engine cache {}: model changed",
                cache.display()
            );
        }
        let opened = (|| {
            let yolo = YOLOModel::load_with_config(model, config).map_err(|e| e.to_string())?;
            let mut detector = Self {
                yolo,
                provider,
                hash: hash.clone(),
            };
            if let Some((width, height)) = warm {
                detector.warm_up(width, height)?;
            }
            if cache.is_dir() {
                std::fs::write(cache.join(MODEL_MARKER), &hash)
                    .map_err(|e| format!("{}: {e}", cache.display()))?;
            }
            Ok(detector)
        })();
        let detector = match (opened, aside) {
            (Ok(detector), aside) => {
                if let Some(aside) = aside {
                    if let Err(e) = std::fs::remove_dir_all(&aside) {
                        eprintln!("{}: {e}", aside.display());
                    }
                }
                detector
            }
            (Err(e), None) => return Err(e),
            (Err(e), Some(aside)) => {
                return Err(match restore_stale_cache(&cache, &aside) {
                    Ok(()) => e,
                    Err(restore) => format!("{e}; {restore}"),
                })
            }
        };
        println!("detector {model} on {provider}");
        Ok(detector)
    }

    pub fn provider(&self) -> &str {
        self.provider
    }

    pub fn model_hash(&self) -> &str {
        &self.hash
    }

    /// Run one blank frame so a TensorRT engine is built before the
    /// detector sees the camera.
    pub fn warm_up(&mut self, width: u32, height: u32) -> Result<(), String> {
        let blank = vec![0u8; (width * height) as usize];
        self.detect(&FrameView {
            width,
            height,
            left: &blank,
            right: None,
            depth: None,
            rectifier: None,
            calib: StereoCalib::nominal(width),
        })
        .map(|_| ())
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

/// Holds the SHA-1 of the model file the cached TensorRT engine was built from.
const MODEL_MARKER: &str = "model.sha1";

/// Where the model loader keeps the TensorRT engine for an fp16 YOLO model,
/// mirroring `YOLOModel::trt_cache_dir`.
fn engine_cache_dir(model: &Path) -> PathBuf {
    let stem = model
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model");
    let parent = model.parent().unwrap_or_else(|| Path::new("."));
    parent.join(".trt_cache").join(format!("{stem}_fp16"))
}

/// TensorRT names engines by graph structure, so retrained weights with the
/// same architecture would reuse the old engine. Unless its marker matches
/// `hash`, rename the cache aside so a failed load can put it back; returns
/// the aside path.
fn set_aside_stale_cache(cache: &Path, hash: &str) -> Result<Option<PathBuf>, String> {
    if !cache.is_dir() {
        return Ok(None);
    }
    let marker = std::fs::read_to_string(cache.join(MODEL_MARKER)).unwrap_or_default();
    if marker.trim() == hash {
        return Ok(None);
    }
    let aside = stale_dir(cache);
    if aside.exists() {
        std::fs::remove_dir_all(&aside).map_err(|e| format!("{}: {e}", aside.display()))?;
    }
    std::fs::rename(cache, &aside).map_err(|e| format!("{}: {e}", cache.display()))?;
    Ok(Some(aside))
}

/// After a failed load, drop whatever cache it built and return the one set
/// aside.
fn restore_stale_cache(cache: &Path, aside: &Path) -> Result<(), String> {
    if cache.exists() {
        std::fs::remove_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    }
    std::fs::rename(aside, cache).map_err(|e| format!("{}: {e}", aside.display()))
}

fn stale_dir(cache: &Path) -> PathBuf {
    let mut name = cache.file_name().unwrap_or_default().to_os_string();
    name.push(".stale");
    cache.with_file_name(name)
}

/// Open the runtime once per process and pick its provider.
fn runtime() -> Result<&'static str, String> {
    static RUNTIME: OnceLock<Result<&'static str, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| load_runtime().map(|()| available_provider()))
        .clone()
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
    use super::{is_ball, restore_stale_cache, set_aside_stale_cache, MODEL_MARKER};

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

    #[test]
    fn sets_the_engine_cache_aside_only_when_the_model_changed() {
        let root = std::env::temp_dir().join(format!("tracker-trt-{}", std::process::id()));
        let cache = root.join(".trt_cache").join("ball_fp16");
        let aside = root.join(".trt_cache").join("ball_fp16.stale");

        assert_eq!(set_aside_stale_cache(&cache, "abc").unwrap(), None);

        std::fs::create_dir_all(&cache).unwrap();
        assert_eq!(
            set_aside_stale_cache(&cache, "abc").unwrap(),
            Some(aside.clone())
        );
        assert!(!cache.exists());
        assert!(aside.is_dir());
        std::fs::remove_dir_all(&aside).unwrap();

        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(MODEL_MARKER), "abc").unwrap();
        assert_eq!(set_aside_stale_cache(&cache, "abc").unwrap(), None);
        assert!(cache.exists());
        assert_eq!(
            set_aside_stale_cache(&cache, "def").unwrap(),
            Some(aside.clone())
        );
        assert!(!cache.exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_load_puts_the_old_engine_cache_back() {
        let root = std::env::temp_dir().join(format!("tracker-trt-restore-{}", std::process::id()));
        let cache = root.join(".trt_cache").join("ball_fp16");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(MODEL_MARKER), "old").unwrap();
        std::fs::write(cache.join("engine"), "old engine").unwrap();

        let aside = set_aside_stale_cache(&cache, "new").unwrap().unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("partial"), "").unwrap();
        restore_stale_cache(&cache, &aside).unwrap();

        assert!(!aside.exists());
        assert!(!cache.join("partial").exists());
        assert_eq!(
            std::fs::read_to_string(cache.join(MODEL_MARKER)).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(cache.join("engine")).unwrap(),
            "old engine"
        );
        assert_eq!(set_aside_stale_cache(&cache, "old").unwrap(), None);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
