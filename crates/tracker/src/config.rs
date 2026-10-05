use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::TrackerError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub exposure_us: u32,
    pub gain: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountConfig {
    pub distance_from_plate_m: f64,
    pub height_m: f64,
    pub lateral_offset_m: f64,
    pub pitch_deg: f64,
    pub yaw_deg: f64,
    pub positive_depth_is_rf: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatorConfig {
    pub launch_url: String,
}

/// Event selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackingConfig {
    /// Slowest batted ball reported as a hit. Pitches have no floor.
    pub min_hit_mph: f64,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self { min_hit_mph: 15.0 }
    }
}

/// Stereo correction for this rig. The raw eyes are not rectified, so a
/// fixed horizontal misalignment shows up as extra disparity everywhere.
/// Calibrate it from one measured distance: offset = measured disparity -
/// fx * baseline / distance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StereoConfig {
    pub disparity_offset_px: f64,
}

impl Default for StereoConfig {
    fn default() -> Self {
        Self {
            disparity_offset_px: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub capture: CaptureConfig,
    pub mount: MountConfig,
    pub simulator: SimulatorConfig,
    #[serde(default)]
    pub stereo: StereoConfig,
    #[serde(default)]
    pub tracking: TrackingConfig,
}

impl SessionConfig {
    pub fn example() -> Self {
        Self {
            capture: CaptureConfig {
                width: 640,
                height: 400,
                fps: 100.0,
                exposure_us: 600,
                gain: 1600,
            },
            mount: MountConfig {
                distance_from_plate_m: 1.74,
                height_m: 0.9,
                lateral_offset_m: -1.61,
                pitch_deg: 30.5,
                yaw_deg: 23.0,
                positive_depth_is_rf: true,
            },
            simulator: SimulatorConfig {
                launch_url: "http://127.0.0.1:7878/launch".to_string(),
            },
            stereo: StereoConfig {
                disparity_offset_px: 17.1,
            },
            tracking: TrackingConfig::default(),
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, TrackerError> {
        let text = fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), TrackerError> {
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
