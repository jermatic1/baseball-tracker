//! The aim tool over HTTP: where a pitcher throwing from closer than the
//! simulated mound should aim at their target to land in the strike zone.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracker::aim::{self, Aim, DEFAULT_RELEASE_HEIGHT_IN};
use tracker::flight;
use tracker::units::metres;

use crate::error::AppError;
use crate::hub::Hub;

pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/api/aim", get(aim_api))
        .with_state(hub)
}

/// The release speed to place the target for, mph, and whose measured
/// release height to use.
#[derive(Deserialize)]
struct AimQuery {
    speed: f64,
    user: Option<i64>,
}

#[derive(Serialize)]
struct AimReply {
    /// Pitches the release height was averaged from; zero means the
    /// default was assumed.
    measured_from: usize,
    actual_distance: f64,
    simulated_distance: f64,
    aim: Option<Aim>,
}

async fn aim_api(
    State(hub): State<Arc<Hub>>,
    Query(q): Query<AimQuery>,
) -> Result<Json<AimReply>, AppError> {
    let cfg = hub.config();
    let measured = match q.user {
        Some(user) => hub.release_height(user)?,
        None => None,
    };
    let (release_height, measured_from) = measured.unwrap_or((DEFAULT_RELEASE_HEIGHT_IN, 0));
    let air = flight::air_density(metres(cfg.flight.altitude));
    Ok(Json(AimReply {
        measured_from,
        actual_distance: cfg.pitching.actual_distance,
        simulated_distance: cfg.pitching.simulated_distance,
        aim: aim::aim(&cfg.pitching, &cfg.zone, q.speed, release_height, air),
    }))
}
