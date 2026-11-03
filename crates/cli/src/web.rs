//! The web app, built by `task web` into `web/build` and embedded here. Every
//! path that is not a file in the build is a page the app routes itself, so
//! it gets `index.html`.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::Embed)]
#[folder = "../../web/build"]
struct Assets;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(file) = Assets::get(path) {
        let cache = if path.starts_with("_app/immutable/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, file.metadata.mimetype()),
                (header::CACHE_CONTROL, cache),
            ],
            file.data,
        )
            .into_response();
    }
    match Assets::get("index.html") {
        Some(index) => (
            [
                (header::CONTENT_TYPE, "text/html"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            index.data,
        )
            .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the web app is not built; run task web",
        )
            .into_response(),
    }
}

/// A session's page served on its own, as `tracker label` and `tracker
/// review` do: the API under the same prefix the tracker would use, with
/// `local` for the session id, and the app for everything else.
pub fn standalone(page: &str, api: axum::Router) -> axum::Router {
    let path = format!("/sessions/local/{page}");
    let home = path.clone();
    axum::Router::new()
        .route(
            "/",
            axum::routing::get(move || async move { axum::response::Redirect::temporary(&home) }),
        )
        .nest(&path, api)
        .fallback(serve)
}
