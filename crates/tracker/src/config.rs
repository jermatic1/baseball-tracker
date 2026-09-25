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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub capture: CaptureConfig,
    pub mount: MountConfig,
    pub simulator: SimulatorConfig,
}

impl SessionConfig {
    pub fn example() -> Self {
        Self {
            capture: CaptureConfig {
                width: 1280,
                height: 800,
                fps: 60.0,
                exposure_us: 1000,
                gain: 100,
            },
            mount: MountConfig {
                distance_from_plate_m: 2.1,
                height_m: 0.8,
                lateral_offset_m: 0.5,
                pitch_deg: 0.0,
                yaw_deg: 0.0,
                positive_depth_is_rf: true,
            },
            simulator: SimulatorConfig {
                launch_url: "http://127.0.0.1:7878/launch".to_string(),
            },
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
