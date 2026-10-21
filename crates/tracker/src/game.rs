//! A game module runs the game side of a session: it sees every session
//! change and event, exchanges opaque JSON bodies with the front ends that
//! named it in their hello, and may add lines to the session summary. Core
//! carries the bodies without reading them.

use serde_json::Value;

use crate::clips::HitRecord;
use crate::stats::SummaryLine;
use crate::store::{Session, User};

pub trait Game: Send {
    /// The name front ends list in `hello.games` and the envelope carries.
    fn name(&self) -> &'static str;

    /// The module's own page, served at `/games/<name>`.
    fn page(&self) -> Option<&'static str> {
        None
    }

    /// Bodies that bring a front end that just connected up to date.
    fn welcome(&self) -> Vec<Value>;

    fn session_started(&mut self, session: &Session, user: &User) -> Vec<Value>;

    /// Lines to add to the session's summary.
    fn session_ended(&mut self, session: &Session) -> Vec<SummaryLine>;

    /// An event recorded under the active session.
    fn event(&mut self, session: &Session, record: &HitRecord) -> Vec<Value>;

    /// A body from a front end. The error is sent back to that front end.
    fn message(&mut self, body: Value) -> Result<Vec<Value>, String>;
}
