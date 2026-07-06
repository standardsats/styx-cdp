//! The debug/admin HTTP surface: /health for liveness, /quote as the fallback quote channel
//! (freshly signed on demand), and the scenario-control lever - POST /price pins a sticky
//! override (a staged crash holds still while the feed keeps ticking underneath), DELETE
//! /price hands the price back to the feed.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use styx_core::units::{BlockHeight, Price};
use styx_watch::quotes::WireQuote;

use crate::service::OracleState;

#[derive(Serialize)]
pub struct Health {
    pub slot: u8,
    /// The last height a quote was published for (0 = none yet).
    pub height: u32,
    pub price: u32,
    /// Where the price comes from: "override" / "feed" / "config".
    pub source: &'static str,
    /// Seconds since the backend last delivered (absent without a backend update). A
    /// growing number means the feed went quiet and the last price is being re-signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feed_age_secs: Option<u64>,
    /// False when the staleness gate is withholding quotes (feed silent past max_age).
    pub publishing: bool,
}

#[derive(Deserialize)]
pub struct QuoteParams {
    pub height: u32,
}

#[derive(Deserialize)]
pub struct SetPrice {
    pub usd: u32,
}

pub fn router(state: Arc<OracleState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/quote", get(quote))
        .route("/price", post(set_price).delete(clear_price))
        .with_state(state)
}

fn health_of(s: &OracleState) -> Health {
    Health {
        slot: s.slot().index() as u8,
        height: s.last_published.load(Ordering::SeqCst),
        price: s.price().raw(),
        source: s.mode().as_str(),
        feed_age_secs: s.feed_age_secs(),
        publishing: s.publishable(),
    }
}

async fn health(State(s): State<Arc<OracleState>>) -> Json<Health> {
    Json(health_of(&s))
}

async fn quote(State(s): State<Arc<OracleState>>, Query(p): Query<QuoteParams>) -> Json<WireQuote> {
    Json(s.quote_for(BlockHeight::new(p.height)))
}

async fn set_price(
    State(s): State<Arc<OracleState>>,
    Json(body): Json<SetPrice>,
) -> Result<Json<Health>, (StatusCode, &'static str)> {
    if body.usd == 0 {
        // The covenants reject a zero price at the quorum layer; an oracle never signs one.
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "zero price"));
    }
    s.set_override(Price::new(body.usd));
    Ok(Json(health_of(&s)))
}

async fn clear_price(State(s): State<Arc<OracleState>>) -> Json<Health> {
    s.clear_override();
    Json(health_of(&s))
}
