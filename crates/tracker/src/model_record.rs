//! `ball.toml` beside the detector model: the model's SHA-1, when it was
//! promoted, the sessions it trained on, and its scores on the held-out
//! sessions. It describes the model only while the SHA-1 matches the file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::session_meta::{SessionMeta, Training};
use crate::TrackerError;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRecord {
    pub sha1: String,
    /// RFC 3339, UTC.
    pub promoted: String,
    /// Session ids whose clips trained the model.
    pub sessions: Vec<String>,
    pub holdout: Vec<String>,
    /// The candidate's scores on the held-out sessions, by name.
    pub scores: BTreeMap<String, f64>,
}

/// Where the record of `model` lives: the model's path with `.toml`.
pub fn record_path(model: &Path) -> PathBuf {
    model.with_extension("toml")
}

impl ModelRecord {
    /// The record beside `model`, or none when it has no file.
    pub fn load(model: &Path) -> Result<Option<Self>, TrackerError> {
        let path = record_path(model);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(toml::from_str(&std::fs::read_to_string(path)?)?))
    }

    pub fn save(&self, model: &Path) -> Result<(), TrackerError> {
        std::fs::write(record_path(model), toml::to_string(self)?)?;
        Ok(())
    }

    pub fn trained_on(&self, id: &str) -> bool {
        self.sessions.iter().any(|s| s == id)
    }
}

/// Sessions with reviewed clips marked for training that `record`'s model
/// did not train on. `sessions` gives each id, its metadata, and its
/// reviewed clip count.
pub fn untrained<'a>(
    sessions: impl IntoIterator<Item = (&'a str, &'a SessionMeta, usize)>,
    record: Option<&ModelRecord>,
) -> usize {
    sessions
        .into_iter()
        .filter(|(id, meta, reviewed)| {
            meta.training == Training::Include
                && *reviewed > 0
                && !record.is_some_and(|r| r.trained_on(id))
        })
        .count()
}

/// Hex SHA-1 of a file.
pub fn file_sha1(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha1::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_beside_the_model() {
        let dir = std::env::temp_dir().join(format!("model-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("ball.onnx");
        assert_eq!(ModelRecord::load(&model).unwrap(), None);
        let record = ModelRecord {
            sha1: "abc".into(),
            promoted: "2026-10-31T12:00:00Z".into(),
            sessions: vec!["2".into(), "none".into()],
            holdout: vec!["4".into()],
            scores: [("recall".to_string(), 0.91), ("map50".to_string(), 0.88)].into(),
        };
        record.save(&model).unwrap();
        assert!(dir.join("ball.toml").is_file());
        assert_eq!(ModelRecord::load(&model).unwrap(), Some(record));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn counts_reviewed_included_sessions_the_model_did_not_train_on() {
        let meta = |training| SessionMeta {
            training,
            ..SessionMeta::default()
        };
        let (include, holdout, none) = (
            meta(Training::Include),
            meta(Training::Holdout),
            meta(Training::None),
        );
        let sessions = [
            ("1", &include, 3),
            ("2", &include, 5),
            ("3", &include, 0),
            ("4", &holdout, 2),
            ("5", &none, 2),
            ("none", &include, 1),
        ];
        assert_eq!(untrained(sessions, None), 3);
        let record = ModelRecord {
            sessions: vec!["1".into(), "3".into(), "4".into()],
            ..ModelRecord::default()
        };
        assert_eq!(untrained(sessions, Some(&record)), 2);
    }
}
