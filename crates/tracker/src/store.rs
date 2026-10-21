//! Users, their sessions, and the events recorded under them, in one SQLite
//! file. Stats are computed from events when asked, never stored.

use std::fmt;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::clips::HitRecord;
use crate::TrackerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Activity {
    Hitting,
    Pitching,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Game,
    Practice,
}

impl Activity {
    pub fn as_str(self) -> &'static str {
        match self {
            Activity::Hitting => "hitting",
            Activity::Pitching => "pitching",
        }
    }

    fn parse(s: &str) -> Result<Self, TrackerError> {
        match s {
            "hitting" => Ok(Activity::Hitting),
            "pitching" => Ok(Activity::Pitching),
            other => Err(TrackerError::Other(format!("unknown activity {other}"))),
        }
    }
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Game => "game",
            Mode::Practice => "practice",
        }
    }

    fn parse(s: &str) -> Result<Self, TrackerError> {
        match s {
            "game" => Ok(Mode::Game),
            "practice" => Ok(Mode::Practice),
            other => Err(TrackerError::Other(format!("unknown mode {other}"))),
        }
    }
}

impl fmt::Display for Activity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A real person being tracked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub name: String,
}

/// One user hitting or pitching from start to end. Times are UTC text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub user: i64,
    pub activity: Activity,
    pub mode: Mode,
    pub started: String,
    pub ended: Option<String>,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY,
    user INTEGER NOT NULL REFERENCES users(id),
    activity TEXT NOT NULL,
    mode TEXT NOT NULL,
    started TEXT NOT NULL DEFAULT (datetime('now')),
    ended TEXT
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    session INTEGER NOT NULL REFERENCES sessions(id),
    clip TEXT NOT NULL,
    kind TEXT NOT NULL,
    record TEXT NOT NULL,
    created TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS events_session ON events(session);
";

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackerError> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, TrackerError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, TrackerError> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn add_user(&self, name: &str) -> Result<User, TrackerError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TrackerError::Other("a user needs a name".into()));
        }
        self.conn
            .execute("INSERT INTO users (name) VALUES (?1)", params![name])?;
        Ok(User {
            id: self.conn.last_insert_rowid(),
            name: name.to_string(),
        })
    }

    pub fn users(&self) -> Result<Vec<User>, TrackerError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM users ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(User {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn user(&self, id: i64) -> Result<Option<User>, TrackerError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name FROM users WHERE id = ?1",
                params![id],
                |r| {
                    Ok(User {
                        id: r.get(0)?,
                        name: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Open a session for a user. The caller ends any active one first.
    pub fn start_session(
        &self,
        user: i64,
        activity: Activity,
        mode: Mode,
    ) -> Result<Session, TrackerError> {
        if self.user(user)?.is_none() {
            return Err(TrackerError::Other("no such user".into()));
        }
        self.conn.execute(
            "INSERT INTO sessions (user, activity, mode) VALUES (?1, ?2, ?3)",
            params![user, activity.as_str(), mode.as_str()],
        )?;
        let id = self.conn.last_insert_rowid();
        self.session(id)?
            .ok_or_else(|| TrackerError::Other("session vanished".into()))
    }

    pub fn end_session(&self, id: i64) -> Result<(), TrackerError> {
        let n = self.conn.execute(
            "UPDATE sessions SET ended = datetime('now') WHERE id = ?1 AND ended IS NULL",
            params![id],
        )?;
        if n == 0 {
            return Err(TrackerError::Other("no such active session".into()));
        }
        Ok(())
    }

    pub fn session(&self, id: i64) -> Result<Option<Session>, TrackerError> {
        self.one_session("WHERE id = ?1", params![id])
    }

    pub fn active_session(&self) -> Result<Option<Session>, TrackerError> {
        self.one_session("WHERE ended IS NULL ORDER BY id DESC LIMIT 1", [])
    }

    pub fn last_ended_session(&self) -> Result<Option<Session>, TrackerError> {
        self.one_session(
            "WHERE ended IS NOT NULL ORDER BY ended DESC, id DESC LIMIT 1",
            [],
        )
    }

    /// Every session, newest first, or one user's.
    pub fn sessions(&self, user: Option<i64>) -> Result<Vec<Session>, TrackerError> {
        let sql = format!(
            "{SESSION_COLUMNS} {} ORDER BY id DESC",
            if user.is_some() {
                "WHERE user = ?1"
            } else {
                ""
            }
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match user {
            Some(u) => stmt.query_map(params![u], session_row)?,
            None => stmt.query_map([], session_row)?,
        };
        rows.map(|r| r?).collect()
    }

    fn one_session<P: rusqlite::Params>(
        &self,
        clause: &str,
        p: P,
    ) -> Result<Option<Session>, TrackerError> {
        let sql = format!("{SESSION_COLUMNS} {clause}");
        self.conn
            .query_row(&sql, p, session_row)
            .optional()?
            .transpose()
    }

    pub fn add_event(&self, session: i64, record: &HitRecord) -> Result<(), TrackerError> {
        self.conn.execute(
            "INSERT INTO events (session, clip, kind, record) VALUES (?1, ?2, ?3, ?4)",
            params![
                session,
                record.clip,
                record.kind.as_str(),
                serde_json::to_string(record)?
            ],
        )?;
        Ok(())
    }

    /// A session's events in the order they were recorded.
    pub fn events(&self, session: i64) -> Result<Vec<HitRecord>, TrackerError> {
        let mut stmt = self
            .conn
            .prepare("SELECT record FROM events WHERE session = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![session], |r| r.get::<_, String>(0))?;
        rows.map(|text| Ok(serde_json::from_str(&text?)?)).collect()
    }
}

const SESSION_COLUMNS: &str = "SELECT id, user, activity, mode, started, ended FROM sessions";

fn session_row(r: &Row<'_>) -> rusqlite::Result<Result<Session, TrackerError>> {
    let activity: String = r.get(2)?;
    let mode: String = r.get(3)?;
    Ok((|| {
        Ok(Session {
            id: r.get(0)?,
            user: r.get(1)?,
            activity: Activity::parse(&activity)?,
            mode: Mode::parse(&mode)?,
            started: r.get(4)?,
            ended: r.get(5)?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clips::EventKind;

    fn record(clip: &str, kind: EventKind) -> HitRecord {
        HitRecord {
            clip: clip.into(),
            kind,
            segment: 0,
            frame_start: 0,
            frame_end: 10,
            start: 0.0,
            exit_velocity: 50.0,
            launch_angle: 10.0,
            spray_angle: 0.0,
            pitch_velocity: None,
            hit_type: None,
            contact: None,
            samples: 10,
            confident: true,
            hit: None,
            pitch: None,
        }
    }

    #[test]
    fn users_sessions_and_events_round_trip() {
        let store = Store::in_memory().unwrap();
        let sam = store.add_user("Sam").unwrap();
        assert!(store.add_user("  ").is_err());
        assert!(store.add_user("Sam").is_err(), "names are unique");
        assert_eq!(store.users().unwrap(), vec![sam.clone()]);

        assert!(store.active_session().unwrap().is_none());
        let s = store
            .start_session(sam.id, Activity::Hitting, Mode::Game)
            .unwrap();
        assert_eq!(store.active_session().unwrap().as_ref(), Some(&s));
        assert!(store
            .start_session(99, Activity::Hitting, Mode::Game)
            .is_err());

        store
            .add_event(s.id, &record("0001", EventKind::Hit))
            .unwrap();
        store
            .add_event(s.id, &record("0002", EventKind::Bounce))
            .unwrap();
        let events = store.events(s.id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].clip, "0001");

        store.end_session(s.id).unwrap();
        assert!(store.end_session(s.id).is_err(), "already ended");
        assert!(store.active_session().unwrap().is_none());
        let ended = store.last_ended_session().unwrap().unwrap();
        assert_eq!(ended.id, s.id);
        assert!(ended.ended.is_some());
        assert_eq!(store.sessions(Some(sam.id)).unwrap().len(), 1);
        assert_eq!(store.sessions(None).unwrap().len(), 1);
    }
}
