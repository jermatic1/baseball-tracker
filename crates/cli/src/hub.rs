//! What the socket clients, the server tasks, and the detector share: the
//! broadcast channel, who is connected, whether capture is allowed, and the
//! store with the active session.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::broadcast;
use tracker::{
    summary_lines, Activity, Config, DataDir, HitRecord, Mode, MountConfig, Session, Store,
};

use crate::protocol::{ServerMsg, SessionInfo, UserInfo};

/// Capture runs only for connected clients, and never during calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    pub clients: usize,
    pub calibrating: bool,
}

impl Gate {
    pub fn capturing(&self) -> bool {
        self.clients > 0 && !self.calibrating
    }
}

#[derive(Debug, Clone)]
pub enum Outbound {
    Text(Arc<str>),
    Preview(Arc<Vec<u8>>),
}

struct Client {
    id: u64,
    name: String,
    #[allow(dead_code)]
    games: Vec<String>,
}

/// The session clips are being filed under, with its directory.
#[derive(Clone)]
struct Active {
    session: Session,
    dir: DataDir,
}

pub struct Hub {
    pub tx: broadcast::Sender<Outbound>,
    clients: Mutex<Vec<Client>>,
    next_id: AtomicU64,
    pub preview_subs: AtomicUsize,
    capturing: AtomicBool,
    calibrating: AtomicBool,
    /// The live config; an applied calibration lands here and in the file.
    pub config: RwLock<Config>,
    root: DataDir,
    store: Mutex<Store>,
    active: RwLock<Option<Active>>,
}

impl Hub {
    /// Open the store in the data directory and pick up a session left
    /// active by a previous run.
    pub fn open(root: DataDir) -> Result<Arc<Self>, String> {
        let store = Store::open(root.dir.join("tracker.db")).map_err(|e| e.to_string())?;
        Self::with_store(root, store)
    }

    pub fn with_store(root: DataDir, store: Store) -> Result<Arc<Self>, String> {
        let (tx, _) = broadcast::channel(64);
        let active = match store.active_session().map_err(|e| e.to_string())? {
            Some(session) => {
                match DataDir::open(root.dir.join("sessions").join(session.id.to_string())) {
                    Ok(dir) => Some(Active { session, dir }),
                    Err(_) => {
                        store.end_session(session.id).map_err(|e| e.to_string())?;
                        None
                    }
                }
            }
            None => None,
        };
        Ok(Arc::new(Self {
            tx,
            clients: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            preview_subs: AtomicUsize::new(0),
            capturing: AtomicBool::new(false),
            calibrating: AtomicBool::new(false),
            config: RwLock::new(root.config.clone()),
            root,
            store: Mutex::new(store),
            active: RwLock::new(active),
        }))
    }

    pub fn join(&self, name: &str, games: &[String]) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = self.clients.lock() {
            c.push(Client {
                id,
                name: name.to_string(),
                games: games.to_vec(),
            });
        }
        self.recompute();
        id
    }

    pub fn leave(&self, id: u64) {
        let empty = self
            .clients
            .lock()
            .map(|mut c| {
                c.retain(|client| client.id != id);
                c.is_empty()
            })
            .unwrap_or(true);
        if empty {
            self.calibrating.store(false, Ordering::Relaxed);
        }
        self.recompute();
    }

    pub fn client_names(&self) -> Vec<String> {
        self.clients
            .lock()
            .map(|c| c.iter().map(|client| client.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn set_calibrating(&self, on: bool) {
        self.calibrating.store(on, Ordering::Relaxed);
        self.recompute();
    }

    pub fn calibrating(&self) -> bool {
        self.calibrating.load(Ordering::Relaxed)
    }

    pub fn capturing(&self) -> bool {
        self.capturing.load(Ordering::Relaxed)
    }

    fn recompute(&self) {
        let gate = Gate {
            clients: self.clients.lock().map(|c| c.len()).unwrap_or(0),
            calibrating: self.calibrating.load(Ordering::Relaxed),
        };
        self.capturing.store(gate.capturing(), Ordering::Relaxed);
    }

    pub fn send(&self, msg: &ServerMsg) {
        if let Ok(text) = serde_json::to_string(msg) {
            let _ = self.tx.send(Outbound::Text(text.into()));
        }
    }

    pub fn send_preview(&self, jpeg: Vec<u8>) {
        let _ = self.tx.send(Outbound::Preview(Arc::new(jpeg)));
    }

    pub fn config(&self) -> Config {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn mount(&self) -> MountConfig {
        self.config().mount
    }

    /// Where a clip that starts now is written, and the session it belongs
    /// to: the active session's directory, or the data directory itself.
    pub fn target(&self) -> (DataDir, Option<i64>) {
        let active = self
            .active
            .read()
            .map(|a| a.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        match active {
            Some(a) => (a.dir, Some(a.session.id)),
            None => (self.root.clone(), None),
        }
    }

    /// What every client is told after hello: the users, the session, and
    /// the most recent summary.
    pub fn welcome(&self) -> Vec<ServerMsg> {
        let Ok(store) = self.store.lock() else {
            return Vec::new();
        };
        let mut out = vec![self.users_msg(&store), self.session_msg()];
        if let Ok(Some(last)) = store.last_ended_session() {
            if let Ok(summary) = Self::summary(&store, &last) {
                out.push(summary);
            }
        }
        out
    }

    pub fn add_user(&self, name: &str) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        store.add_user(name).map_err(|e| e.to_string())?;
        self.send(&self.users_msg(&store));
        Ok(())
    }

    /// Start a session for a user, ending the active one first. Sends the
    /// old session's summary, then the new session.
    pub fn start_session(&self, user: &str, activity: Activity, mode: Mode) -> Result<(), String> {
        let user: i64 = user.parse().map_err(|_| "no such user".to_string())?;
        let store = self.store.lock().map_err(|e| e.to_string())?;
        if store.user(user).map_err(|e| e.to_string())?.is_none() {
            return Err("no such user".into());
        }
        if let Some(old) = self.take_active() {
            store
                .end_session(old.session.id)
                .map_err(|e| e.to_string())?;
            self.send(&Self::summary(&store, &old.session)?);
        }
        let session = store
            .start_session(user, activity, mode)
            .map_err(|e| e.to_string())?;
        let dir = self.session_dir(&session)?;
        if let Ok(mut active) = self.active.write() {
            *active = Some(Active { session, dir });
        }
        self.send(&self.session_msg());
        Ok(())
    }

    /// End the active session. Sends the null session, then its summary.
    pub fn end_session(&self, id: &str) -> Result<(), String> {
        let id: i64 = id.parse().map_err(|_| "no such session".to_string())?;
        let store = self.store.lock().map_err(|e| e.to_string())?;
        let is_active = self
            .active
            .read()
            .map(|a| a.as_ref().is_some_and(|a| a.session.id == id))
            .unwrap_or(false);
        if !is_active {
            return Err("no such active session".into());
        }
        let Some(old) = self.take_active() else {
            return Err("no such active session".into());
        };
        store.end_session(id).map_err(|e| e.to_string())?;
        self.send(&self.session_msg());
        self.send(&Self::summary(&store, &old.session)?);
        Ok(())
    }

    pub fn record_event(&self, session: i64, record: &HitRecord) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        store.add_event(session, record).map_err(|e| e.to_string())
    }

    fn take_active(&self) -> Option<Active> {
        self.active.write().ok().and_then(|mut a| a.take())
    }

    /// A session's directory: a snapshot of the live config and the camera
    /// calibration, so the session can be replayed on its own.
    fn session_dir(&self, session: &Session) -> Result<DataDir, String> {
        let dir = self.root.dir.join("sessions").join(session.id.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        self.config()
            .save(dir.join("config.toml"))
            .map_err(|e| e.to_string())?;
        let calibration = self.root.dir.join("calibration.json");
        if calibration.exists() {
            std::fs::copy(&calibration, dir.join("calibration.json")).map_err(|e| e.to_string())?;
        }
        DataDir::open(dir).map_err(|e| e.to_string())
    }

    fn users_msg(&self, store: &Store) -> ServerMsg {
        ServerMsg::Users {
            users: store
                .users()
                .unwrap_or_default()
                .iter()
                .map(UserInfo::from)
                .collect(),
        }
    }

    fn session_msg(&self) -> ServerMsg {
        ServerMsg::Session {
            session: self
                .active
                .read()
                .ok()
                .and_then(|a| a.as_ref().map(|a| SessionInfo::from(&a.session))),
        }
    }

    fn summary(store: &Store, session: &Session) -> Result<ServerMsg, String> {
        let events = store.events(session.id).map_err(|e| e.to_string())?;
        Ok(ServerMsg::SessionSummary {
            session: session.id.to_string(),
            user: session.user.to_string(),
            lines: summary_lines(session.activity, &events),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hub() -> Arc<Hub> {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hub-test-{}-{n}", std::process::id()));
        let root = DataDir::create(&dir).unwrap();
        Hub::with_store(root, Store::in_memory().unwrap()).unwrap()
    }

    fn next(rx: &mut broadcast::Receiver<Outbound>) -> ServerMsg {
        match rx.try_recv().expect("a message") {
            Outbound::Text(text) => serde_json::from_str(&text).unwrap(),
            Outbound::Preview(_) => panic!("preview"),
        }
    }

    #[test]
    fn gate_rule() {
        let g = |clients, calibrating| Gate {
            clients,
            calibrating,
        };
        assert!(!g(0, false).capturing());
        assert!(g(1, false).capturing());
        assert!(g(3, false).capturing());
        assert!(!g(1, true).capturing());
        assert!(!g(0, true).capturing());
    }

    #[test]
    fn clients_drive_capture_and_last_leave_clears_calibration() {
        let hub = test_hub();
        assert!(!hub.capturing());
        let a = hub.join("sim", &[]);
        let b = hub.join("page", &[]);
        assert!(hub.capturing());
        assert_eq!(hub.client_names(), vec!["sim", "page"]);
        hub.set_calibrating(true);
        assert!(!hub.capturing());
        hub.leave(a);
        assert!(hub.calibrating());
        hub.leave(b);
        assert!(!hub.calibrating());
        assert!(!hub.capturing());
    }

    #[test]
    fn sessions_announce_and_file_clips() {
        let hub = test_hub();
        let mut rx = hub.tx.subscribe();
        assert!(hub
            .start_session("1", Activity::Hitting, Mode::Game)
            .is_err());
        hub.add_user("Sam").unwrap();
        assert!(matches!(next(&mut rx), ServerMsg::Users { users } if users[0].name == "Sam"));
        assert_eq!(hub.target().1, None);

        hub.start_session("1", Activity::Hitting, Mode::Game)
            .unwrap();
        let ServerMsg::Session { session: Some(s) } = next(&mut rx) else {
            panic!("session");
        };
        assert_eq!(s.activity, Activity::Hitting);
        let (dir, id) = hub.target();
        assert_eq!(id, Some(1));
        assert!(dir.dir.join("config.toml").exists());

        // Replacing sends the old summary, then the new session.
        hub.start_session("1", Activity::Pitching, Mode::Practice)
            .unwrap();
        assert!(
            matches!(next(&mut rx), ServerMsg::SessionSummary { session, .. } if session == "1")
        );
        assert!(matches!(next(&mut rx), ServerMsg::Session { session: Some(s) } if s.id == "2"));

        assert!(hub.end_session("1").is_err());
        hub.end_session("2").unwrap();
        assert!(matches!(
            next(&mut rx),
            ServerMsg::Session { session: None }
        ));
        assert!(
            matches!(next(&mut rx), ServerMsg::SessionSummary { session, .. } if session == "2")
        );
        assert_eq!(hub.target().1, None);

        let welcome = hub.welcome();
        assert_eq!(welcome.len(), 3, "users, session, last summary");
        assert!(matches!(&welcome[2], ServerMsg::SessionSummary { session, .. } if session == "2"));
    }
}
