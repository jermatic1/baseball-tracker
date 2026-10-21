//! Messages on the `/ws` socket. JSON text frames both ways; preview
//! JPEGs travel as binary frames. Feet, inches, mph, degrees, seconds;
//! exposure in microseconds.

use serde::{Deserialize, Serialize};
use tracker::{Activity, HitRecord, Mode, MountConfig, Session, SummaryLine, User};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Must be the first message; names the client and the game modules it
    /// plays.
    Hello {
        client: String,
        #[serde(default)]
        sessions: Option<u32>,
        #[serde(default)]
        games: Vec<String>,
    },
    /// Subscribe to or drop preview frames.
    Preview {
        enabled: bool,
    },
    /// Absolute camera controls; absent fields are left alone.
    Set {
        #[serde(default)]
        exposure: Option<u32>,
        #[serde(default)]
        gain: Option<u32>,
        #[serde(default)]
        ir_flood: Option<f32>,
        #[serde(default)]
        ir_dot: Option<f32>,
    },
    /// A manual clip.
    Record {
        action: RecordAction,
    },
    Calibrate {
        action: CalibrateAction,
    },
    /// A new user, from the tracker's own page.
    AddUser {
        name: String,
    },
    /// Ends the active session first, if there is one.
    StartSession {
        user: String,
        activity: Activity,
        mode: Mode,
    },
    EndSession {
        session: String,
    },
    /// A type this tracker does not know; ignored so clients can grow.
    #[serde(other)]
    Unknown,
}

impl ClientMsg {
    /// The wire name, for `error.request`.
    pub fn name(&self) -> &'static str {
        match self {
            ClientMsg::Hello { .. } => "hello",
            ClientMsg::Preview { .. } => "preview",
            ClientMsg::Set { .. } => "set",
            ClientMsg::Record { .. } => "record",
            ClientMsg::Calibrate { .. } => "calibrate",
            ClientMsg::AddUser { .. } => "add_user",
            ClientMsg::StartSession { .. } => "start_session",
            ClientMsg::EndSession { .. } => "end_session",
            ClientMsg::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordAction {
    Start,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrateAction {
    Start,
    Apply,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Status(Status),
    /// One measured record per hit, pitch, or bounce in a finished clip.
    Event {
        clip: String,
        record: HitRecord,
    },
    /// A plate fit while calibration mode is on; `applied` once saved.
    Calibration {
        found: bool,
        corners: Option<Vec<[f64; 2]>>,
        reprojected: Option<Vec<[f64; 2]>>,
        rms: Option<f64>,
        mount: Option<MountConfig>,
        applied: bool,
    },
    /// Everyone being tracked; sent after hello and whenever it changes.
    Users {
        users: Vec<UserInfo>,
    },
    /// The whole current session state; sent after hello and on every change.
    Session {
        session: Option<SessionInfo>,
    },
    /// Display-ready lines for a session that just ended, resent after hello.
    SessionSummary {
        session: String,
        user: String,
        lines: Vec<SummaryLine>,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<String>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
}

impl From<&User> for UserInfo {
    fn from(u: &User) -> Self {
        Self {
            id: u.id.to_string(),
            name: u.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub user: String,
    pub activity: Activity,
    pub mode: Mode,
}

impl From<&Session> for SessionInfo {
    fn from(s: &Session) -> Self {
        Self {
            id: s.id.to_string(),
            user: s.user.to_string(),
            activity: s.activity,
            mode: s.mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub camera: Option<CameraStats>,
    pub detector: Option<LiveStatus>,
    pub clients: Vec<String>,
    pub capturing: bool,
    pub calibrating: bool,
    pub recording: bool,
    pub exposure: u32,
    pub gain: u32,
    pub ir_flood: f32,
    pub ir_dot: f32,
    pub mount: MountConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraStats {
    pub left_rate: f64,
    pub right_rate: f64,
    pub pair_rate: f64,
    pub left_gaps: u64,
    pub right_gaps: u64,
    pub unpaired: u64,
    pub record_drops: u64,
}

impl CameraStats {
    pub fn from_device(s: device::CaptureStats, record_drops: u64) -> Self {
        let per_s = |n: u64| (n as f64 / s.seconds.max(1e-3)).round();
        Self {
            left_rate: per_s(s.left),
            right_rate: per_s(s.right),
            pair_rate: per_s(s.pairs),
            left_gaps: s.left_gaps,
            right_gaps: s.right_gaps,
            unpaired: s.unpaired,
            record_drops,
        }
    }
}

/// The detector thread's view of itself, read by the status ticker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveStatus {
    pub state: String,
    pub provider: String,
    pub rate: f64,
    pub lag: u64,
    pub drops: u64,
    pub clips: u64,
    pub last_event: String,
    pub boxes: Vec<[f64; 4]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(v: T) {
        let text = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<T>(&text).unwrap(), v, "{text}");
    }

    #[test]
    fn client_messages_round_trip() {
        roundtrip(ClientMsg::Hello {
            client: "page".into(),
            sessions: Some(1),
            games: vec!["challenges".into()],
        });
        roundtrip(ClientMsg::AddUser { name: "Sam".into() });
        roundtrip(ClientMsg::StartSession {
            user: "1".into(),
            activity: Activity::Pitching,
            mode: Mode::Game,
        });
        roundtrip(ClientMsg::EndSession {
            session: "42".into(),
        });
        roundtrip(ClientMsg::Preview { enabled: true });
        roundtrip(ClientMsg::Set {
            exposure: Some(500),
            gain: None,
            ir_flood: Some(0.2),
            ir_dot: None,
        });
        roundtrip(ClientMsg::Record {
            action: RecordAction::Stop,
        });
        roundtrip(ClientMsg::Calibrate {
            action: CalibrateAction::Apply,
        });
    }

    #[test]
    fn wire_shapes() {
        let hello: ClientMsg = serde_json::from_str(r#"{"type":"hello","client":"sim"}"#).unwrap();
        assert_eq!(
            hello,
            ClientMsg::Hello {
                client: "sim".into(),
                sessions: None,
                games: Vec::new(),
            }
        );
        let start: ClientMsg = serde_json::from_str(
            r#"{"type":"start_session","user":"u1","activity":"hitting","mode":"practice"}"#,
        )
        .unwrap();
        assert_eq!(start.name(), "start_session");
        assert!(matches!(
            start,
            ClientMsg::StartSession {
                activity: Activity::Hitting,
                mode: Mode::Practice,
                ..
            }
        ));
        let set: ClientMsg = serde_json::from_str(r#"{"type":"set","gain":1200}"#).unwrap();
        assert!(matches!(
            set,
            ClientMsg::Set {
                gain: Some(1200),
                exposure: None,
                ..
            }
        ));
        let unknown: ClientMsg = serde_json::from_str(r#"{"type":"fly","wings":2}"#).unwrap();
        assert_eq!(unknown, ClientMsg::Unknown);
    }

    #[test]
    fn names_match_wire_tags() {
        let msgs = [
            ClientMsg::Preview { enabled: true },
            ClientMsg::AddUser { name: "x".into() },
            ClientMsg::EndSession {
                session: "1".into(),
            },
        ];
        for m in msgs {
            assert_eq!(serde_json::to_value(&m).unwrap()["type"], m.name());
        }
    }

    #[test]
    fn server_messages_round_trip() {
        let mount = tracker::Config::example().mount;
        roundtrip(ServerMsg::Status(Status {
            camera: Some(CameraStats {
                left_rate: 100.0,
                right_rate: 100.0,
                pair_rate: 100.0,
                left_gaps: 0,
                right_gaps: 0,
                unpaired: 0,
                record_drops: 0,
            }),
            detector: Some(LiveStatus::default()),
            clients: vec!["page".into()],
            capturing: true,
            calibrating: false,
            recording: false,
            exposure: 600,
            gain: 1600,
            ir_flood: 0.0,
            ir_dot: 0.0,
            mount: mount.clone(),
        }));
        roundtrip(ServerMsg::Calibration {
            found: true,
            corners: Some(vec![[1.0, 2.0]; 5]),
            reprojected: None,
            rms: Some(0.4),
            mount: Some(mount),
            applied: false,
        });
        roundtrip(ServerMsg::Error {
            request: Some("start_session".into()),
            message: "no".into(),
        });
        roundtrip(ServerMsg::Users {
            users: vec![UserInfo {
                id: "1".into(),
                name: "Sam".into(),
            }],
        });
        roundtrip(ServerMsg::Session { session: None });
        roundtrip(ServerMsg::SessionSummary {
            session: "42".into(),
            user: "1".into(),
            lines: vec![SummaryLine {
                label: "PITCHES".into(),
                value: "48".into(),
            }],
        });
        let error = serde_json::to_value(ServerMsg::Error {
            request: None,
            message: "x".into(),
        })
        .unwrap();
        assert_eq!(error["type"], "error");
        assert!(error.get("request").is_none());
        let session = serde_json::to_value(ServerMsg::Session {
            session: Some(SessionInfo {
                id: "42".into(),
                user: "1".into(),
                activity: Activity::Pitching,
                mode: Mode::Game,
            }),
        })
        .unwrap();
        assert_eq!(session["session"]["activity"], "pitching");
        assert_eq!(session["session"]["mode"], "game");
    }
}
