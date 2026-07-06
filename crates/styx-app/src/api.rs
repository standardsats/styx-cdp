//! The JSON API over one wallet session, plus the SSE event stream. Handlers run the
//! blocking wallet machinery on the blocking pool; the session lives behind a mutex (one
//! wallet, one writer at a time - ops serialize through the singletons anyway).

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use styx_wallet::wallet::{Wallet, WalletError};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::auth::{gate, Gate};

/// One line on the event stream: what happened, machine-readable enough for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct AppEvent {
    pub kind: &'static str,
    pub detail: String,
}

pub struct AppState {
    pub wallet: Arc<Mutex<Wallet>>,
    pub events: tokio::sync::broadcast::Sender<AppEvent>,
}

impl AppState {
    pub fn new(wallet: Wallet) -> Arc<AppState> {
        Arc::new(AppState {
            wallet: Arc::new(Mutex::new(wallet)),
            events: tokio::sync::broadcast::channel(256).0,
        })
    }

    pub fn emit(&self, kind: &'static str, detail: impl Into<String>) {
        // Send fails only with no subscribers; events are advisory either way.
        let _ = self.events.send(AppEvent { kind, detail: detail.into() });
    }
}

/// The API error shape: the typed wallet REFUSALS become 409s with their Display text (the
/// same words the CLI prints); everything not on that list - infrastructure failures,
/// invariant breaks, panics - is a 500. Whitelisting the refusals keeps a new error
/// variant defaulting to "internal", never to "your request was wrong".
pub enum ApiError {
    Wallet(WalletError),
    Internal(String),
}

fn is_refusal(e: &WalletError) -> bool {
    matches!(
        e,
        WalletError::Build(_)
            | WalletError::Math(_)
            | WalletError::NotLive
            | WalletError::VaultNotFound(_)
            | WalletError::VaultNotMine(_)
            | WalletError::InsufficientLbtc { .. }
            | WalletError::InsufficientObol { .. }
            | WalletError::AmbiguousVault(_)
            | WalletError::NoVault
            | WalletError::ConflictExhausted { .. }
    )
}

impl From<WalletError> for ApiError {
    fn from(e: WalletError) -> Self {
        ApiError::Wallet(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            ApiError::Wallet(e) if is_refusal(&e) => {
                (StatusCode::CONFLICT, e.to_string()).into_response()
            }
            ApiError::Wallet(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
        }
    }
}

#[derive(Serialize)]
pub struct VaultView {
    pub outpoint: String,
    pub debt_units: u64,
    pub collateral_sats: u64,
    pub last_height: u32,
}

#[derive(Serialize)]
pub struct Status {
    pub height: u32,
    pub funding_address: String,
    pub lbtc_sats: u64,
    pub obol_units: u64,
    pub vaults: Vec<VaultView>,
    /// Keeper mode: "idle" until the duty loop lands in this API.
    pub keeper: &'static str,
}

/// Sync to the tip and summarize the session. The one blocking round-trip every UI screen
/// starts from.
async fn status(State(s): State<Arc<AppState>>) -> Result<Json<Status>, ApiError> {
    let wallet = s.wallet.clone();
    let status = tokio::task::spawn_blocking(move || -> Result<Status, WalletError> {
        // Poison recovery is sound HERE: sync() rebuilds the wallet view from the node, so
        // a panic in a previous holder leaves nothing this read path depends on. The op
        // endpoints must revisit this - a panic mid-op leaves the in-memory session at an
        // unknown point, and continuing silently is not obviously right.
        let mut w = wallet.lock().unwrap_or_else(|e| e.into_inner());
        w.sync()?;
        let vaults = w
            .my_vaults()
            .iter()
            .map(|v| VaultView {
                outpoint: v.outpoint.to_string(),
                debt_units: v.state.debt.raw(),
                collateral_sats: v.value.raw(),
                last_height: v.state.last_height.raw(),
            })
            .collect();
        Ok(Status {
            height: w.state.height,
            funding_address: w.funding_address().to_string(),
            lbtc_sats: w.lbtc_coins()?.iter().map(|(_, v)| *v).sum(),
            obol_units: w.obol_coins()?.iter().map(|(_, v)| *v).sum(),
            vaults,
            keeper: "idle",
        })
    })
    .await
    .map_err(|e| ApiError::Internal(format!("status task: {e}")))??;
    s.emit("status", format!("height {}", status.height));
    Ok(Json(status))
}

/// The event stream sits behind the same gate as everything else, token header included.
/// UI contract: consume it with fetch-based streaming (a ReadableStream over this
/// response), NOT EventSource - EventSource cannot set the token header, and the token
/// does not go into URLs (query strings end up in logs and history).
async fn events(State(s): State<Arc<AppState>>) -> impl IntoResponse {
    let stream = BroadcastStream::new(s.events.subscribe()).filter_map(|item| {
        // A lagged subscriber skips missed events; the stream itself stays up.
        item.ok()
            .and_then(|e| Event::default().json_data(&e).ok().map(Ok::<_, std::convert::Infallible>))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// The full application router: every route behind the gate.
pub fn router(state: Arc<AppState>, g: Arc<Gate>) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(g, gate))
}
