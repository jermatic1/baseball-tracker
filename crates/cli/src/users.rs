//! Renaming and removing users over HTTP. The socket's `users` message
//! carries each change to every page.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::hub::Hub;

pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/api/users/{id}", put(rename).delete(remove))
        .route("/api/users/{id}/sessions", delete(remove_sessions))
        .with_state(hub)
}

#[derive(Deserialize)]
struct Rename {
    name: String,
}

/// `?sessions=true` removes the user's sessions too; without it a user
/// who has sessions stays.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Remove {
    sessions: bool,
}

type Reply = Result<(), (StatusCode, String)>;

fn reply(result: Result<(), String>) -> Reply {
    result.map_err(|e| (StatusCode::BAD_REQUEST, e))
}

async fn rename(
    State(hub): State<Arc<Hub>>,
    Path(id): Path<i64>,
    Json(body): Json<Rename>,
) -> Reply {
    reply(hub.rename_user(id, &body.name))
}

async fn remove(
    State(hub): State<Arc<Hub>>,
    Path(id): Path<i64>,
    Query(q): Query<Remove>,
) -> Reply {
    reply(hub.delete_user(id, q.sessions))
}

async fn remove_sessions(State(hub): State<Arc<Hub>>, Path(id): Path<i64>) -> Reply {
    reply(hub.delete_user_sessions(id))
}
