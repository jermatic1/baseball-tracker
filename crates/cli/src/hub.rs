//! What the socket clients, the server tasks, and the detector share: the
//! broadcast channel, who is connected, and whether capture is allowed.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::broadcast;
use tracker::{MountConfig, SessionConfig};

use crate::protocol::ServerMsg;

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

pub struct Hub {
    pub tx: broadcast::Sender<Outbound>,
    clients: Mutex<Vec<(u64, String)>>,
    next_id: AtomicU64,
    pub preview_subs: AtomicUsize,
    capturing: AtomicBool,
    calibrating: AtomicBool,
    /// The live config; an applied calibration lands here and in the file.
    pub config: RwLock<SessionConfig>,
}

impl Hub {
    pub fn new(config: SessionConfig) -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        Arc::new(Self {
            tx,
            clients: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            preview_subs: AtomicUsize::new(0),
            capturing: AtomicBool::new(false),
            calibrating: AtomicBool::new(false),
            config: RwLock::new(config),
        })
    }

    pub fn join(&self, name: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut c) = self.clients.lock() {
            c.push((id, name.to_string()));
        }
        self.recompute();
        id
    }

    pub fn leave(&self, id: u64) {
        let empty = self
            .clients
            .lock()
            .map(|mut c| {
                c.retain(|(i, _)| *i != id);
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
            .map(|c| c.iter().map(|(_, n)| n.clone()).collect())
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

    pub fn config(&self) -> SessionConfig {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    pub fn mount(&self) -> MountConfig {
        self.config().mount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let hub = Hub::new(SessionConfig::example());
        assert!(!hub.capturing());
        let a = hub.join("sim");
        let b = hub.join("page");
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
}
