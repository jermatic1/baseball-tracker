//! `session.toml` in each session folder: the tags, notes, and training
//! choice the user edits, and the facts filled in when the session starts.
//! The file is the source of truth for the editable fields.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{MountConfig, TrackerError};

pub const SESSION_FILE: &str = "session.toml";

/// Whether a session's labelled clips train the detector, are held out to
/// test it, or are left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Training {
    Include,
    Holdout,
    #[default]
    None,
}

/// The detector a session was recorded with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub file: String,
    pub sha1: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    pub tags: Vec<String>,
    pub notes: String,
    pub training: Training,
    /// RFC 3339, UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gain: Option<u32>,
    /// The mount differs from the previous session's.
    pub calibration_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<MountConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelInfo>,
}

/// The editable fields of a `PUT`; absent ones are left as they are.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetaUpdate {
    pub tags: Option<Vec<String>>,
    pub notes: Option<String>,
    pub training: Option<Training>,
}

impl SessionMeta {
    /// The folder's metadata, or the default when it has no file.
    pub fn load(dir: &Path) -> Result<Self, TrackerError> {
        let path = dir.join(SESSION_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// The folder's metadata, writing `fill` for a folder with no file.
    pub fn load_or_create(dir: &Path, fill: impl FnOnce() -> Self) -> Result<Self, TrackerError> {
        if dir.join(SESSION_FILE).exists() {
            return Self::load(dir);
        }
        let meta = fill();
        meta.save(dir)?;
        Ok(meta)
    }

    pub fn save(&self, dir: &Path) -> Result<(), TrackerError> {
        std::fs::write(dir.join(SESSION_FILE), toml::to_string(self)?)?;
        Ok(())
    }

    /// Apply an edit. A session with no reviewed clips has no labels, so it
    /// can be neither trained on nor held out.
    pub fn apply(&mut self, update: MetaUpdate, reviewed: usize) -> Result<(), String> {
        if let Some(training) = update.training {
            if training != Training::None && reviewed == 0 {
                return Err(
                    "this session has no reviewed clips; label some before including it in training or holding it out".into(),
                );
            }
            self.training = training;
        }
        if let Some(tags) = update.tags {
            let mut clean: Vec<String> = Vec::new();
            for tag in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
                if !clean.iter().any(|c| c == tag) {
                    clean.push(tag.to_string());
                }
            }
            self.tags = clean;
        }
        if let Some(notes) = update.notes {
            self.notes = notes;
        }
        Ok(())
    }
}

/// SQLite's `datetime('now')`, `YYYY-MM-DD HH:MM:SS` in UTC, as RFC 3339.
pub fn sqlite_to_rfc3339(stamp: &str) -> String {
    match stamp.split_once(' ') {
        Some((date, time)) => format!("{date}T{time}Z"),
        None => stamp.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("meta-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = temp("round");
        let meta = SessionMeta {
            tags: vec!["cage".into(), "dusk".into()],
            notes: "new net".into(),
            training: Training::Holdout,
            started: Some("2026-10-31T12:00:00Z".into()),
            user: Some("Sam".into()),
            activity: Some("hitting".into()),
            exposure: Some(600),
            gain: Some(1600),
            calibration_changed: true,
            mount: Some(MountConfig {
                distance: 10.0,
                height: 4.0,
                lateral: 1.0,
                pitch: -2.0,
                yaw: 0.5,
            }),
            model: Some(ModelInfo {
                file: "detector.onnx".into(),
                sha1: "abc".into(),
            }),
        };
        meta.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(SESSION_FILE)).unwrap();
        assert!(text.contains("training = \"holdout\""), "{text}");
        assert_eq!(SessionMeta::load(&dir).unwrap(), meta);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_the_default_and_is_created_on_request() {
        let dir = temp("missing");
        let meta = SessionMeta::load(&dir).unwrap();
        assert_eq!(meta, SessionMeta::default());
        assert_eq!(meta.training, Training::None);
        assert!(!dir.join(SESSION_FILE).exists());
        let made = SessionMeta::load_or_create(&dir, SessionMeta::default).unwrap();
        assert_eq!(made, meta);
        assert!(dir.join(SESSION_FILE).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unreviewed_session_cannot_train_or_be_held_out() {
        let mut meta = SessionMeta::default();
        for training in [Training::Include, Training::Holdout] {
            let update = MetaUpdate {
                training: Some(training),
                ..MetaUpdate::default()
            };
            assert!(meta.apply(update, 0).is_err());
            assert_eq!(meta.training, Training::None);
        }
        let none = MetaUpdate {
            training: Some(Training::None),
            ..MetaUpdate::default()
        };
        assert!(meta.apply(none, 0).is_ok());
        let include = MetaUpdate {
            training: Some(Training::Include),
            tags: Some(vec![" cage ".into(), "cage".into(), "".into()]),
            ..MetaUpdate::default()
        };
        meta.apply(include, 3).unwrap();
        assert_eq!(meta.training, Training::Include);
        assert_eq!(meta.tags, ["cage"]);
    }
}
