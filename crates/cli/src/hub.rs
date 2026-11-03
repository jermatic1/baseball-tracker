//! What the socket clients, the server tasks, and the detector share: the
//! broadcast channel, who is connected, whether capture is allowed, the
//! store with the active session, and the game modules.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;
use tokio::sync::broadcast;
use tracker::{
    session_stats, summary_lines, Activity, Config, DataDir, Game, HitRecord, Mode, ModelInfo,
    ModelRecord, MountConfig, Session, SessionMeta, Store, User,
};

use crate::protocol::{ModelStatus, ServerMsg, SessionInfo, UserInfo};

/// Detection runs while a session is active or a client other than a page
/// is connected, except during calibration. The camera is wanted then, and
/// also while calibrating, recording a clip by hand, or while a client
/// watches the preview, which need frames but not detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    /// Connected clients that are not pages.
    pub players: usize,
    pub session: bool,
    pub calibrating: bool,
    pub recording: bool,
    pub preview: bool,
}

impl Gate {
    pub fn wanted(&self) -> bool {
        self.players > 0 || self.session || self.calibrating || self.recording || self.preview
    }

    pub fn capturing(&self) -> bool {
        (self.players > 0 || self.session) && !self.calibrating
    }
}

/// Whether a client's hello names one of the tracker's own web pages, which
/// connect as "page" or "<name> page" and do not start the camera by
/// connecting.
pub fn is_page(client: &str) -> bool {
    client == "page" || client.ends_with(" page")
}

#[derive(Debug, Clone)]
pub enum Outbound {
    Text(Arc<str>),
    Preview(Arc<Vec<u8>>),
    /// A game body, for clients that listed the game in their hello.
    Game {
        game: Arc<str>,
        text: Arc<str>,
    },
}

struct Client {
    id: u64,
    name: String,
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
    preview_subs: AtomicUsize,
    wanted: AtomicBool,
    capturing: AtomicBool,
    calibrating: AtomicBool,
    recording: AtomicBool,
    /// The live config; an applied calibration lands here and in the file.
    pub config: RwLock<Config>,
    root: DataDir,
    store: Mutex<Store>,
    active: RwLock<Option<Active>>,
    games: Mutex<Vec<Box<dyn Game>>>,
    /// Exposure and gain as last set, recorded in each new session.
    controls: Mutex<(u32, u32)>,
    model: RwLock<Option<ModelInfo>>,
    /// The live model for the status line, and its record when it has one.
    model_status: RwLock<ModelStatus>,
    model_record: RwLock<Option<ModelRecord>>,
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
        let hub = Arc::new(Self {
            tx,
            clients: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            preview_subs: AtomicUsize::new(0),
            wanted: AtomicBool::new(false),
            capturing: AtomicBool::new(false),
            calibrating: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            config: RwLock::new(root.config.clone()),
            controls: Mutex::new((root.config.capture.exposure, root.config.capture.gain)),
            model: RwLock::new(None),
            model_status: RwLock::new(ModelStatus::default()),
            model_record: RwLock::new(None),
            root,
            store: Mutex::new(store),
            active: RwLock::new(active),
            games: Mutex::new(Vec::new()),
        });
        hub.recompute();
        Ok(hub)
    }

    /// Record the camera's exposure and gain for the next session started.
    pub fn set_controls(&self, exposure: u32, gain: u32) {
        if let Ok(mut c) = self.controls.lock() {
            *c = (exposure, gain);
        }
    }

    /// Record the detector model new sessions are recorded with.
    #[cfg_attr(not(feature = "detect"), allow(dead_code))]
    pub fn set_model(&self, model: ModelInfo) {
        if let Ok(mut m) = self.model.write() {
            *m = Some(model);
        }
    }

    pub fn model_status(&self) -> ModelStatus {
        self.model_status
            .read()
            .map(|m| m.clone())
            .unwrap_or_default()
    }

    pub fn update_model_status(&self, change: impl FnOnce(&mut ModelStatus)) {
        if let Ok(mut m) = self.model_status.write() {
            change(&mut m);
        }
    }

    /// What the live model trained on, when the Train page promoted it.
    pub fn model_record(&self) -> Option<ModelRecord> {
        self.model_record.read().ok().and_then(|r| r.clone())
    }

    pub fn set_model_record(&self, record: Option<ModelRecord>) {
        if let Ok(mut r) = self.model_record.write() {
            *r = record;
        }
    }

    /// Add a game module; the watch command does this for each compiled in.
    #[cfg_attr(not(feature = "challenges"), allow(dead_code))]
    pub fn register(&self, game: Box<dyn Game>) {
        if let Ok(mut games) = self.games.lock() {
            games.push(game);
        }
    }

    pub fn game_names(&self) -> Vec<String> {
        self.games
            .lock()
            .map(|g| g.iter().map(|g| g.name().to_string()).collect())
            .unwrap_or_default()
    }

    pub fn join(&self, name: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = self.clients.lock() {
            c.push(Client {
                id,
                name: name.to_string(),
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

    /// A clip is being recorded by hand, which keeps the camera up.
    pub fn set_recording(&self, on: bool) {
        self.recording.store(on, Ordering::Relaxed);
        self.recompute();
    }

    /// Count a client subscribing to or dropping the preview.
    pub fn watch_preview(&self, on: bool) {
        if on {
            self.preview_subs.fetch_add(1, Ordering::Relaxed);
        } else {
            self.preview_subs.fetch_sub(1, Ordering::Relaxed);
        }
        self.recompute();
    }

    pub fn previewing(&self) -> bool {
        self.preview_subs.load(Ordering::Relaxed) > 0
    }

    /// Whether the camera and the detector should be running.
    #[cfg_attr(not(feature = "detect"), allow(dead_code))]
    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed)
    }

    pub fn capturing(&self) -> bool {
        self.capturing.load(Ordering::Relaxed)
    }

    fn recompute(&self) {
        let gate = Gate {
            players: self
                .clients
                .lock()
                .map(|c| c.iter().filter(|client| !is_page(&client.name)).count())
                .unwrap_or(0),
            session: self.active.read().is_ok_and(|a| a.is_some()),
            calibrating: self.calibrating.load(Ordering::Relaxed),
            recording: self.recording.load(Ordering::Relaxed),
            preview: self.previewing(),
        };
        self.wanted.store(gate.wanted(), Ordering::Relaxed);
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

    fn send_game(&self, game: &str, bodies: Vec<Value>) {
        for body in bodies {
            let msg = ServerMsg::Game {
                game: game.to_string(),
                body,
            };
            if let Ok(text) = serde_json::to_string(&msg) {
                let _ = self.tx.send(Outbound::Game {
                    game: game.into(),
                    text: text.into(),
                });
            }
        }
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
    /// to: the active session's directory, or the folder for loose clips.
    pub fn target(&self) -> (DataDir, Option<i64>) {
        let active = self
            .active
            .read()
            .map(|a| a.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        match active {
            Some(a) => (a.dir, Some(a.session.id)),
            None => (self.loose_dir(), None),
        }
    }

    /// Where clips cut with no session active go: `sessions/none`, set up
    /// like a session's folder.
    fn loose_dir(&self) -> DataDir {
        let dir = self.root.dir.join("sessions").join("none");
        self.snapshot_dir(dir).unwrap_or_else(|e| {
            eprintln!("loose clips go to the data directory: {e}");
            self.root.clone()
        })
    }

    /// What a client is told after hello: the users, the session, the most
    /// recent summary, and the state of each game it listed.
    pub fn welcome(&self, games: &[String]) -> Vec<ServerMsg> {
        let Ok(store) = self.store.lock() else {
            return Vec::new();
        };
        let mut out = vec![self.users_msg(&store), self.session_msg()];
        if let Some(active) = self.active.read().ok().and_then(|a| a.clone()) {
            if let Ok(stats) = Self::stats_msg(&store, active.session.id) {
                out.push(stats);
            }
        }
        if let Ok(Some(last)) = store.last_ended_session() {
            if let Ok(summary) = Self::summary(&store, &last) {
                out.push(summary);
            }
        }
        if let Ok(modules) = self.games.lock() {
            for game in modules
                .iter()
                .filter(|g| games.iter().any(|n| n == g.name()))
            {
                out.extend(game.welcome().into_iter().map(|body| ServerMsg::Game {
                    game: game.name().to_string(),
                    body,
                }));
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

    /// Renames a user everywhere: the store and each of their session
    /// folders, which keep the name too.
    pub fn rename_user(&self, id: i64, name: &str) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        store.rename_user(id, name).map_err(|e| e.to_string())?;
        for session in store.sessions(Some(id)).map_err(|e| e.to_string())? {
            let dir = self.session_path(session.id);
            if let Ok(mut meta) = SessionMeta::load(&dir) {
                meta.user = Some(name.trim().to_string());
                meta.save(&dir).map_err(|e| e.to_string())?;
            }
        }
        self.send(&self.users_msg(&store));
        Ok(())
    }

    /// Removes a user, and first their sessions when asked; otherwise a
    /// user with sessions stays.
    pub fn delete_user(&self, id: i64, with_sessions: bool) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        if with_sessions {
            self.delete_sessions_of(&store, id)?;
        }
        store.delete_user(id).map_err(|e| e.to_string())?;
        self.send(&self.users_msg(&store));
        Ok(())
    }

    /// Removes every session a user has, keeping the user.
    pub fn delete_user_sessions(&self, id: i64) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        self.delete_sessions_of(&store, id)
    }

    /// Removes a session, its events, and its folder of clips and labels.
    pub fn delete_session(&self, id: i64) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        self.remove_session(&store, id)
    }

    fn delete_sessions_of(&self, store: &Store, user: i64) -> Result<(), String> {
        for session in store.sessions(Some(user)).map_err(|e| e.to_string())? {
            self.remove_session(store, session.id)?;
        }
        Ok(())
    }

    /// The active session has to end first; its clips are still being cut.
    fn remove_session(&self, store: &Store, id: i64) -> Result<(), String> {
        let active = self
            .active
            .read()
            .map(|a| a.as_ref().is_some_and(|a| a.session.id == id))
            .unwrap_or(false);
        if active {
            return Err(format!("session {id} is running; end it first"));
        }
        store.delete_session(id).map_err(|e| e.to_string())?;
        match std::fs::remove_dir_all(self.session_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn session_path(&self, id: i64) -> PathBuf {
        self.root.dir.join("sessions").join(id.to_string())
    }

    /// Start a session for a user, ending the active one first. Sends the
    /// old session's summary, then the new session, then what each game
    /// has to say about it.
    pub fn start_session(&self, user: &str, activity: Activity, mode: Mode) -> Result<(), String> {
        let user: i64 = user.parse().map_err(|_| "no such user".to_string())?;
        let store = self.store.lock().map_err(|e| e.to_string())?;
        let who = store
            .user(user)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no such user".to_string())?;
        if let Some(old) = self.take_active() {
            store
                .end_session(old.session.id)
                .map_err(|e| e.to_string())?;
            self.send(&self.ended_summary(&store, &old.session)?);
        }
        let session = store
            .start_session(user, activity, mode)
            .map_err(|e| e.to_string())?;
        let dir = self.session_dir(&session)?;
        self.session_meta(&store, &session, &who)?
            .save(&dir.dir)
            .map_err(|e| e.to_string())?;
        if let Ok(mut active) = self.active.write() {
            *active = Some(Active {
                session: session.clone(),
                dir,
            });
        }
        self.recompute();
        self.send(&self.session_msg());
        self.send(&Self::stats_msg(&store, session.id)?);
        if let Ok(mut modules) = self.games.lock() {
            for game in modules.iter_mut() {
                let bodies = game.session_started(&session, &who);
                self.send_game(game.name(), bodies);
            }
        }
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
        self.recompute();
        self.send(&self.session_msg());
        self.send(&self.ended_summary(&store, &old.session)?);
        Ok(())
    }

    /// The active session's activity, if any.
    pub fn activity(&self) -> Option<Activity> {
        self.active
            .read()
            .ok()
            .and_then(|a| a.as_ref().map(|a| a.session.activity))
    }

    /// File a clip's records: into the directory's hits file, under the
    /// session if there is one, out to every client, and then to the games,
    /// so a game's reply always follows the event it is about.
    pub fn publish(
        &self,
        dir: &DataDir,
        session: Option<i64>,
        clip: &str,
        records: Vec<HitRecord>,
    ) -> Result<(), String> {
        let existing = dir.hits().map_err(|e| e.to_string())?;
        let hits = tracker::merge_clip_hits(existing, clip, records.clone());
        dir.write_hits(&hits).map_err(|e| e.to_string())?;
        if let Some(session) = session {
            for record in &records {
                self.store_event(session, record)?;
            }
        }
        for record in &records {
            self.send(&ServerMsg::Event {
                clip: clip.to_string(),
                record: record.clone(),
            });
        }
        if let Some(session) = session {
            if !records.is_empty() {
                let store = self.store.lock().map_err(|e| e.to_string())?;
                self.send(&Self::stats_msg(&store, session)?);
            }
            for record in &records {
                self.notify_games(session, record)?;
            }
        }
        Ok(())
    }

    fn stats_msg(store: &Store, session: i64) -> Result<ServerMsg, String> {
        let events = store.events(session).map_err(|e| e.to_string())?;
        Ok(ServerMsg::SessionStats {
            session: session.to_string(),
            stats: session_stats(&events),
        })
    }

    fn store_event(&self, session: i64, record: &HitRecord) -> Result<(), String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        store.add_event(session, record).map_err(|e| e.to_string())
    }

    fn notify_games(&self, session: i64, record: &HitRecord) -> Result<(), String> {
        let session = {
            let store = self.store.lock().map_err(|e| e.to_string())?;
            store.session(session).map_err(|e| e.to_string())?
        };
        let Some(session) = session else {
            return Ok(());
        };
        if let Ok(mut modules) = self.games.lock() {
            for game in modules.iter_mut() {
                let bodies = game.event(&session, record);
                self.send_game(game.name(), bodies);
            }
        }
        Ok(())
    }

    /// Hand a front end's body to the named game and send what comes back.
    pub fn game_message(&self, game: &str, body: Value) -> Result<(), String> {
        let mut modules = self.games.lock().map_err(|e| e.to_string())?;
        let module = modules
            .iter_mut()
            .find(|g| g.name() == game)
            .ok_or_else(|| format!("no game {game}"))?;
        let replies = module.message(body)?;
        self.send_game(game, replies);
        Ok(())
    }

    pub fn history(&self) -> Result<crate::history::History, String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        crate::history::build(&store).map_err(|e| e.to_string())
    }

    /// Every session, newest first, with its user's name.
    pub fn sessions(&self) -> Result<Vec<(Session, String)>, String> {
        let store = self.store.lock().map_err(|e| e.to_string())?;
        let users = store.users().map_err(|e| e.to_string())?;
        let sessions = store.sessions(None).map_err(|e| e.to_string())?;
        Ok(sessions
            .into_iter()
            .map(|s| {
                let name = users
                    .iter()
                    .find(|u| u.id == s.user)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();
                (s, name)
            })
            .collect())
    }

    fn take_active(&self) -> Option<Active> {
        self.active.write().ok().and_then(|mut a| a.take())
    }

    /// A session's directory: a snapshot of the live config and the camera
    /// calibration, so the session can be replayed on its own.
    fn session_dir(&self, session: &Session) -> Result<DataDir, String> {
        self.snapshot_dir(self.session_path(session.id))
    }

    fn snapshot_dir(&self, dir: PathBuf) -> Result<DataDir, String> {
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

    /// The facts of a session just started. The calibration changed when the
    /// mount differs from the previous session's, read from its
    /// `session.toml` or, for an older session, its config snapshot.
    fn session_meta(
        &self,
        store: &Store,
        session: &Session,
        who: &User,
    ) -> Result<SessionMeta, String> {
        let config = self.config();
        let previous = store
            .sessions(None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|s| s.id < session.id)
            .and_then(|s| {
                let dir = self.root.dir.join("sessions").join(s.id.to_string());
                SessionMeta::load(&dir)
                    .ok()
                    .and_then(|m| m.mount)
                    .or_else(|| Config::load(dir.join("config.toml")).ok().map(|c| c.mount))
            });
        let (exposure, gain) = self
            .controls
            .lock()
            .map(|c| *c)
            .unwrap_or((config.capture.exposure, config.capture.gain));
        Ok(SessionMeta {
            started: Some(tracker::session_meta::sqlite_to_rfc3339(&session.started)),
            user: Some(who.name.clone()),
            activity: Some(session.activity.to_string()),
            exposure: Some(exposure),
            gain: Some(gain),
            calibration_changed: previous.is_some_and(|m| m != config.mount),
            mount: Some(config.mount),
            model: self.model.read().ok().and_then(|m| m.clone()),
            ..SessionMeta::default()
        })
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

    /// The summary of a session that is ending now, with the games' lines.
    fn ended_summary(&self, store: &Store, session: &Session) -> Result<ServerMsg, String> {
        let mut summary = Self::summary(store, session)?;
        if let (ServerMsg::SessionSummary { lines, .. }, Ok(mut modules)) =
            (&mut summary, self.games.lock())
        {
            for game in modules.iter_mut() {
                lines.extend(game.session_ended(session));
            }
        }
        Ok(summary)
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
            other => panic!("{other:?}"),
        }
    }

    fn game_body(rx: &mut broadcast::Receiver<Outbound>) -> (String, Value) {
        match rx.try_recv().expect("a message") {
            Outbound::Game { game, text } => match serde_json::from_str(&text).unwrap() {
                ServerMsg::Game { game: g, body } => {
                    assert_eq!(&*game, g);
                    (g, body)
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    fn pitch(clip: &str) -> HitRecord {
        HitRecord {
            clip: clip.into(),
            kind: tracker::EventKind::Pitch,
            segment: 0,
            frame_start: 0,
            frame_end: 1,
            start: 0.0,
            exit_velocity: 40.0,
            launch_angle: 0.0,
            spray_angle: 0.0,
            pitch_velocity: None,
            hit_type: None,
            contact: None,
            samples: 1,
            confident: true,
            hit: None,
            pitch: None,
        }
    }

    #[test]
    fn gate_rule() {
        let g = |players, session, calibrating, preview| Gate {
            players,
            session,
            calibrating,
            recording: false,
            preview,
        };
        assert!(!g(0, false, false, false).wanted());
        assert!(!g(0, false, false, false).capturing());
        assert!(g(1, false, false, false).capturing());
        assert!(g(3, false, false, false).capturing());
        assert!(g(0, true, false, false).capturing());
        assert!(!g(1, false, true, false).capturing());
        assert!(!g(0, true, true, false).capturing());
        assert!(g(1, false, true, false).wanted());
        // Calibration and the preview need frames, not detection.
        assert!(g(0, false, true, false).wanted());
        assert!(g(0, false, false, true).wanted());
        assert!(!g(0, false, false, true).capturing());
        let recording = Gate {
            recording: true,
            ..g(0, false, false, false)
        };
        assert!(recording.wanted());
        assert!(!recording.capturing());
    }

    #[test]
    fn pages_do_not_open_the_gate_but_their_preview_and_calibration_do() {
        let hub = test_hub();
        let tracker = hub.join("tracker page");
        let setup = hub.join("setup page");
        assert!(!hub.wanted());
        hub.watch_preview(true);
        assert!(hub.wanted() && !hub.capturing());
        hub.watch_preview(false);
        assert!(!hub.wanted());
        hub.set_calibrating(true);
        assert!(hub.wanted() && !hub.capturing());
        hub.leave(tracker);
        assert!(hub.calibrating());
        hub.leave(setup);
        assert!(!hub.calibrating() && !hub.wanted());
    }

    #[test]
    fn other_clients_drive_capture() {
        let hub = test_hub();
        let page = hub.join("page");
        assert!(!hub.wanted());
        let sim = hub.join("sim");
        assert!(hub.wanted() && hub.capturing());
        assert_eq!(hub.client_names(), vec!["page", "sim"]);
        hub.set_calibrating(true);
        assert!(!hub.capturing());
        hub.set_calibrating(false);
        hub.leave(sim);
        assert!(!hub.wanted());
        hub.leave(page);
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

        assert!(!hub.wanted());
        hub.start_session("1", Activity::Hitting, Mode::Game)
            .unwrap();
        assert!(hub.wanted() && hub.capturing());
        let ServerMsg::Session { session: Some(s) } = next(&mut rx) else {
            panic!("session");
        };
        assert_eq!(s.activity, Activity::Hitting);
        let ServerMsg::SessionStats { session, stats } = next(&mut rx) else {
            panic!("stats");
        };
        assert_eq!(
            (session.as_str(), stats.pitches, stats.top_exit_mph),
            ("1", 0, None)
        );
        let welcome = hub.welcome(&[]);
        assert!(matches!(&welcome[2], ServerMsg::SessionStats { session, .. } if session == "1"));
        let (dir, id) = hub.target();
        assert_eq!(id, Some(1));
        assert!(dir.dir.join("config.toml").exists());
        let meta = SessionMeta::load(&dir.dir).unwrap();
        assert_eq!(meta.user.as_deref(), Some("Sam"));
        assert_eq!(meta.activity.as_deref(), Some("hitting"));
        assert_eq!(meta.mount, Some(hub.config().mount));
        assert!(!meta.calibration_changed);

        // Replacing sends the old summary, then the new session.
        hub.start_session("1", Activity::Pitching, Mode::Practice)
            .unwrap();
        assert!(
            matches!(next(&mut rx), ServerMsg::SessionSummary { session, .. } if session == "1")
        );
        assert!(matches!(next(&mut rx), ServerMsg::Session { session: Some(s) } if s.id == "2"));
        assert!(matches!(next(&mut rx), ServerMsg::SessionStats { session, .. } if session == "2"));

        assert!(hub.end_session("1").is_err());
        hub.end_session("2").unwrap();
        assert!(!hub.wanted() && !hub.capturing());
        assert!(matches!(
            next(&mut rx),
            ServerMsg::Session { session: None }
        ));
        assert!(
            matches!(next(&mut rx), ServerMsg::SessionSummary { session, .. } if session == "2")
        );
        assert_eq!(hub.target().1, None);

        let welcome = hub.welcome(&[]);
        assert_eq!(welcome.len(), 3, "users, session, last summary");
        assert!(matches!(&welcome[2], ServerMsg::SessionSummary { session, .. } if session == "2"));
    }

    /// A game that echoes what it sees, to test routing.
    struct Echo;

    impl Game for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn welcome(&self) -> Vec<Value> {
            vec![serde_json::json!({"type": "hello"})]
        }
        fn session_started(&mut self, session: &Session, user: &tracker::User) -> Vec<Value> {
            vec![serde_json::json!({"type": "started", "session": session.id, "user": user.name})]
        }
        fn session_ended(&mut self, _: &Session) -> Vec<tracker::SummaryLine> {
            vec![tracker::SummaryLine {
                label: "ECHOED".into(),
                value: "1".into(),
            }]
        }
        fn event(&mut self, _: &Session, record: &HitRecord) -> Vec<Value> {
            vec![serde_json::json!({"type": "saw", "clip": record.clip})]
        }
        fn message(&mut self, body: Value) -> Result<Vec<Value>, String> {
            if body["type"] == "bad" {
                return Err("bad body".into());
            }
            Ok(vec![body])
        }
    }

    #[test]
    fn games_see_sessions_and_events_and_answer_their_clients() {
        let hub = test_hub();
        hub.register(Box::new(Echo));
        assert_eq!(hub.game_names(), vec!["echo"]);
        assert_eq!(
            hub.welcome(&["echo".into()]).len(),
            3,
            "users, session, hello"
        );
        assert_eq!(hub.welcome(&["other".into()]).len(), 2);

        hub.add_user("Sam").unwrap();
        let mut rx = hub.tx.subscribe();
        hub.start_session("1", Activity::Pitching, Mode::Game)
            .unwrap();
        assert!(matches!(next(&mut rx), ServerMsg::Session { .. }));
        assert!(matches!(next(&mut rx), ServerMsg::SessionStats { .. }));
        let (game, body) = game_body(&mut rx);
        assert_eq!(
            (game.as_str(), &body["user"]),
            ("echo", &Value::from("Sam"))
        );

        let (dir, _) = hub.target();
        hub.publish(&dir, Some(1), "0001", vec![pitch("0001")])
            .unwrap();
        assert!(matches!(next(&mut rx), ServerMsg::Event { clip, .. } if clip == "0001"));
        let ServerMsg::SessionStats { stats, .. } = next(&mut rx) else {
            panic!("stats after the event");
        };
        assert_eq!(stats.pitches, 1);
        assert_eq!(
            game_body(&mut rx).1["clip"],
            "0001",
            "games answer after the event"
        );
        assert_eq!(dir.hits().unwrap().len(), 1);

        hub.game_message("echo", serde_json::json!({"type": "ping"}))
            .unwrap();
        assert_eq!(game_body(&mut rx).1["type"], "ping");
        assert_eq!(
            hub.game_message("echo", serde_json::json!({"type": "bad"})),
            Err("bad body".into())
        );
        assert!(hub.game_message("none", Value::Null).is_err());

        hub.end_session("1").unwrap();
        assert!(matches!(
            next(&mut rx),
            ServerMsg::Session { session: None }
        ));
        let ServerMsg::SessionSummary { lines, .. } = next(&mut rx) else {
            panic!("summary");
        };
        assert_eq!(lines.last().unwrap().label, "ECHOED");
    }
}
