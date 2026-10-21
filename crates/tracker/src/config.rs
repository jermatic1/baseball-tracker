//! DataDir settings. Distances are feet, speeds mph, angles degrees, times
//! seconds, exposure microseconds.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::TrackerError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub exposure: u32,
    pub gain: u32,
}

/// Camera pose relative to the plate apex.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MountConfig {
    pub distance: f64,
    pub height: f64,
    pub lateral: f64,
    pub pitch: f64,
    pub yaw: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackingConfig {
    /// Slowest batted ball reported as a hit. Pitches have no floor.
    pub min_hit_velocity: f64,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            min_hit_velocity: 15.0,
        }
    }
}

/// Live watching: when a detected ball counts as moving and how much of the
/// stream around the motion becomes a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchConfig {
    /// Pixels a ball must be from every recently seen ball to have moved.
    pub motion: f64,
    /// Seconds kept before the first motion.
    pub preroll: f64,
    /// Seconds without motion that end the window.
    pub settle: f64,
    /// Longest window, in case something keeps moving.
    pub max_episode: f64,
    /// Write the frames of each window as a clip. Off keeps only the
    /// detections and events.
    pub keep_clips: bool,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            motion: 4.0,
            preroll: 0.5,
            settle: 0.15,
            max_episode: 6.0,
            keep_clips: true,
        }
    }
}

/// Stereo correction for sessions recorded without the camera's saved
/// calibration: the disparity the raw eyes show at infinity, in pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StereoConfig {
    pub disparity_offset: f64,
}

impl Default for StereoConfig {
    fn default() -> Self {
        Self {
            disparity_offset: 0.0,
        }
    }
}

/// Where the pitcher actually throws from, and the mound distance each
/// pitch's plate location is projected to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PitchingConfig {
    pub actual_distance: f64,
    pub simulated_distance: f64,
}

impl Default for PitchingConfig {
    fn default() -> Self {
        Self {
            actual_distance: 46.0,
            simulated_distance: 46.0,
        }
    }
}

/// The strike zone, in inches: centred on the plate, `width` across, from
/// `bottom` to `top` above the ground. A pitch is judged on its simulated
/// crossing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoneConfig {
    pub width: f64,
    pub bottom: f64,
    pub top: f64,
}

impl Default for ZoneConfig {
    fn default() -> Self {
        Self {
            width: 17.0,
            bottom: 18.0,
            top: 42.0,
        }
    }
}

impl ZoneConfig {
    pub fn contains(&self, side: f64, height: f64) -> bool {
        side.abs() <= self.width / 2.0 && height >= self.bottom && height <= self.top
    }
}

/// Conditions for the predicted flight of a hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlightConfig {
    pub altitude: f64,
}

impl Default for FlightConfig {
    fn default() -> Self {
        Self { altitude: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub capture: CaptureConfig,
    pub mount: MountConfig,
    #[serde(default)]
    pub stereo: StereoConfig,
    #[serde(default)]
    pub tracking: TrackingConfig,
    #[serde(default)]
    pub watch: WatchConfig,
    #[serde(default)]
    pub pitching: PitchingConfig,
    #[serde(default)]
    pub flight: FlightConfig,
    #[serde(default)]
    pub zone: ZoneConfig,
}

impl Config {
    pub fn example() -> Self {
        Self {
            capture: CaptureConfig {
                width: 640,
                height: 400,
                fps: 100.0,
                exposure: 600,
                gain: 1600,
            },
            mount: MountConfig {
                distance: 5.71,
                height: 2.95,
                lateral: -5.28,
                pitch: 30.5,
                yaw: 23.0,
            },
            stereo: StereoConfig {
                disparity_offset: 17.1,
            },
            tracking: TrackingConfig::default(),
            watch: WatchConfig::default(),
            pitching: PitchingConfig::default(),
            flight: FlightConfig::default(),
            zone: ZoneConfig::default(),
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
