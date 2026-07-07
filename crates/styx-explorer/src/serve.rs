//! The HTTP surface, separated from the daemon loops so the fast tier can prove the
//! hardening headers ride every route, not just live in a constant.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::render::{css, page, CSP, FAVICON};
use crate::state::ExplorerState;

pub struct App {
    pub state: Arc<ExplorerState>,
    pub app_url: Option<String>,
}

fn harden() -> [(header::HeaderName, &'static str); 3] {
    [
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ]
}

async fn home(State(app): State<Arc<App>>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        harden(),
        page(&app.state.view(), app.app_url.as_deref()),
    )
}

async fn api_state(State(app): State<Arc<App>>) -> impl IntoResponse {
    (harden(), Json(app.state.view()))
}

async fn stylesheet() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], harden(), css())
}

async fn favicon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "max-age=86400")],
        harden(),
        FAVICON,
    )
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/api/state", get(api_state))
        .route("/explorer.css", get(stylesheet))
        .route("/favicon.svg", get(favicon))
        .with_state(app)
}
