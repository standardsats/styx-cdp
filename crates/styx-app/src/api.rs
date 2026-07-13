//! The JSON API over one wallet session, plus the SSE event stream. Handlers run the
//! blocking wallet machinery on the blocking pool; the session lives behind a mutex (one
//! wallet, one writer at a time - ops serialize through the singletons anyway).

use std::sync::{Arc, Mutex, RwLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use styx_core::elements::OutPoint;
use styx_core::oracle::OracleTick;
use styx_core::units::{Obol, Sats};
use styx_keeper::keeper::{Keeper, KeeperError, KeeperOpts};
use styx_wallet::ops::OpReport;
use styx_wallet::wallet::{Wallet, WalletError};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::auth::{gate, Gate};

/// An injected tick older than this is treated as absent. Generous against every chain
/// this runs on (styxnet blocks are seconds, the testnet about a minute; the relay loop
/// refreshes every couple of seconds while the relays are alive).
pub const TICK_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(180);

/// One line on the event stream: what happened, machine-readable enough for the UI.
/// Kinds: tick / status / open|repay|draw|refresh|close|redeem (successes) / rejected
/// (typed refusals - the UI's sticky surfaces) / op_error (infrastructure) / keeper /
/// performed / keeper_error.
#[derive(Debug, Clone, Serialize)]
pub struct AppEvent {
    pub kind: &'static str,
    pub detail: String,
}

/// Keeper mode as an API state: the exit-65 semantics of the daemon become a sticky
/// alert here - the loop stops, the process lives, the UI shows the banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperMode {
    Idle,
    Running,
    Alert(String),
}

struct KeeperCtl {
    mode: KeeperMode,
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
}

pub struct AppState {
    pub wallet: Arc<Mutex<Wallet>>,
    pub events: tokio::sync::broadcast::Sender<AppEvent>,
    /// The freshest assembled oracle tick and when it landed. Injected here at the lib
    /// boundary: the binary's relay loop keeps it current, tests set it directly - the API
    /// layer stays transport-free.
    tick: RwLock<Option<(OracleTick, std::time::Instant)>>,
    keeper_ctl: Mutex<KeeperCtl>,
    keeper_opts: KeeperOpts,
    /// The keeper loop's pace between steps.
    poll: std::time::Duration,
    /// Unsigned plans exported for external owner signing, keyed by txid. Owned by this
    /// state (not the process), and bounded: at capacity the oldest export is dropped, so
    /// exports that are never applied cannot grow without limit.
    pending: Mutex<std::collections::VecDeque<(String, styx_pset::plan::TxPlan)>>,
}

/// The most unsigned exports held at once; older ones are evicted (re-export to sign them).
const PENDING_CAP: usize = 32;

impl AppState {
    pub fn new(wallet: Wallet, keeper_opts: KeeperOpts, poll: std::time::Duration) -> Arc<AppState> {
        Arc::new(AppState {
            wallet: Arc::new(Mutex::new(wallet)),
            events: tokio::sync::broadcast::channel(256).0,
            tick: RwLock::new(None),
            keeper_ctl: Mutex::new(KeeperCtl { mode: KeeperMode::Idle, stop: None }),
            keeper_opts,
            poll,
            pending: Mutex::new(std::collections::VecDeque::new()),
        })
    }

    fn stash_pending(&self, txid: String, plan: styx_pset::plan::TxPlan) {
        let mut q = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        q.retain(|(t, _)| *t != txid);
        q.push_back((txid, plan));
        while q.len() > PENDING_CAP {
            q.pop_front();
        }
    }

    fn take_pending(&self, txid: &str) -> Option<styx_pset::plan::TxPlan> {
        let q = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        q.iter().find(|(t, _)| t == txid).map(|(_, p)| p.clone())
    }

    fn drop_pending(&self, txid: &str) {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|(t, _)| t != txid);
    }

    pub fn emit(&self, kind: &'static str, detail: impl Into<String>) {
        // Send fails only with no subscribers; events are advisory either way.
        let _ = self.events.send(AppEvent { kind, detail: detail.into() });
    }

    pub fn set_tick(&self, tick: OracleTick) {
        *self.tick.write().unwrap_or_else(|e| e.into_inner()) = Some((tick, std::time::Instant::now()));
    }

    /// The injected tick, unless it went stale: quiet relays must surface as "no quorum"
    /// (one clear 409, the keeper idling) rather than ops signing against a frozen price.
    /// The chain's own recency gates would refuse eventually - this fails earlier and
    /// with better words.
    pub fn tick(&self) -> Option<OracleTick> {
        self.tick
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(_, at)| at.elapsed() <= TICK_MAX_AGE)
            .map(|(t, _)| t.clone())
    }

    fn current_tick(&self) -> Result<OracleTick, ApiError> {
        self.tick().ok_or(ApiError::NoQuorum)
    }

    /// Ask a running keeper loop to stop (idempotent; also the shutdown path - a
    /// spawn_blocking loop that never exits would hang the runtime's drop forever).
    pub fn request_keeper_stop(&self) -> &'static str {
        let mut ctl = self.keeper_ctl.lock().unwrap_or_else(|e| e.into_inner());
        match (&ctl.mode, &ctl.stop) {
            (KeeperMode::Running, Some(stop)) => {
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
                "stopping"
            }
            // Acknowledging an alert returns the keeper to idle; a fresh start clears it.
            (KeeperMode::Alert(_), _) => {
                ctl.mode = KeeperMode::Idle;
                "idle"
            }
            _ => "idle",
        }
    }

    pub fn keeper_mode(&self) -> KeeperMode {
        self.keeper_ctl.lock().unwrap_or_else(|e| e.into_inner()).mode.clone()
    }

    fn keeper_status(&self) -> String {
        match self.keeper_mode() {
            KeeperMode::Idle => "idle".into(),
            KeeperMode::Running => "running".into(),
            KeeperMode::Alert(m) => format!("alert: {m}"),
        }
    }
}

/// Start the keeper loop on the shared purse. The loop consumes the same injected tick as
/// the op endpoints and runs one `step` per poll; `Rejected` parks it in the sticky Alert
/// state (the loop stops, the API keeps answering), every other error is logged to the
/// event stream and the loop continues - the daemon's posture, process death excluded.
pub fn start_keeper(s: &Arc<AppState>) -> Result<(), ApiError> {
    let stop = {
        let mut ctl = s.keeper_ctl.lock().unwrap_or_else(|e| e.into_inner());
        if ctl.mode == KeeperMode::Running {
            return Err(ApiError::BadRequest("the keeper is already running".into()));
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        ctl.mode = KeeperMode::Running;
        ctl.stop = Some(stop.clone());
        stop
    };
    let state = s.clone();
    tokio::task::spawn_blocking(move || {
        let mut keeper = Keeper::new(state.wallet.clone(), state.keeper_opts);
        state.emit("keeper", "started");
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            if let Some(tick) = state.tick() {
                match keeper.step(&tick) {
                    Ok(Some(done)) => state.emit("performed", format!("{done:?}")),
                    Ok(None) => {}
                    Err(e @ KeeperError::Rejected { .. }) => {
                        let msg = e.to_string();
                        state.emit("rejected", msg.clone());
                        let mut ctl = state.keeper_ctl.lock().unwrap_or_else(|e| e.into_inner());
                        ctl.mode = KeeperMode::Alert(msg);
                        ctl.stop = None;
                        return;
                    }
                    Err(e) => state.emit("keeper_error", e.to_string()),
                }
            }
            std::thread::sleep(state.poll);
        }
        let mut ctl = state.keeper_ctl.lock().unwrap_or_else(|e| e.into_inner());
        ctl.mode = KeeperMode::Idle;
        ctl.stop = None;
        state.emit("keeper", "stopped");
    });
    Ok(())
}

async fn keeper_start(State(s): State<Arc<AppState>>) -> Result<&'static str, ApiError> {
    start_keeper(&s)?;
    Ok("running")
}

async fn keeper_stop(State(s): State<Arc<AppState>>) -> &'static str {
    s.request_keeper_stop()
}

/// The API error shape: the typed wallet REFUSALS become 409s with their Display text (the
/// same words the CLI prints); everything not on that list - infrastructure failures,
/// invariant breaks, panics - is a 500. Whitelisting the refusals keeps a new error
/// variant defaulting to "internal", never to "your request was wrong".
pub enum ApiError {
    Wallet(WalletError),
    /// No oracle quorum assembled yet: a refusal, not a failure - retry once quotes flow.
    NoQuorum,
    /// A malformed request field (an unparseable outpoint, contradictory sizing).
    BadRequest(String),
    /// A previously exported plan no longer applies: the chain moved under it.
    Stale(String),
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
            ApiError::NoQuorum => {
                (StatusCode::CONFLICT, "no oracle quorum assembled yet".to_string()).into_response()
            }
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            ApiError::Stale(m) => (StatusCode::CONFLICT, m).into_response(),
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
    /// CR percent at the current tick's max quote (the liquidation-relevant side);
    /// absent without a tick or for a zero-debt husk.
    pub cr_percent: Option<u32>,
}

#[derive(Serialize)]
pub struct TickView {
    pub height: u32,
    pub lo: u32,
    pub hi: u32,
}

#[derive(Serialize)]
pub struct Protocol {
    pub pot_units: u64,
    pub reserve_sats: u64,
    pub issuer_anchor: u32,
}

#[derive(Serialize)]
pub struct Status {
    pub height: u32,
    pub funding_address: String,
    pub lbtc_sats: u64,
    pub obol_units: u64,
    pub vaults: Vec<VaultView>,
    /// The protocol singletons; absent until the deployment is visible on this chain.
    pub protocol: Option<Protocol>,
    /// The injected oracle tick, when fresh.
    pub tick: Option<TickView>,
    /// Keeper mode: "idle" | "running" | "alert: <message>".
    pub keeper: String,
}

/// Sync to the tip and summarize the session. The one blocking round-trip every UI screen
/// starts from.
async fn status(State(s): State<Arc<AppState>>) -> Result<Json<Status>, ApiError> {
    let wallet = s.wallet.clone();
    let keeper = s.keeper_status();
    let tick = s.tick();
    let status = tokio::task::spawn_blocking(move || -> Result<Status, WalletError> {
        // Poison recovery is sound HERE: sync() rebuilds the wallet view from the node, so
        // a panic in a previous holder leaves nothing this read path depends on. The op
        // endpoints must revisit this - a panic mid-op leaves the in-memory session at an
        // unknown point, and continuing silently is not obviously right.
        let mut w = wallet.lock().unwrap_or_else(|e| e.into_inner());
        w.sync()?;
        let hi = tick.as_ref().map(|t| t.price_range().1);
        let vaults = w
            .my_vaults()
            .iter()
            .map(|v| VaultView {
                outpoint: outpoint_str(&v.outpoint),
                debt_units: v.state.debt.raw(),
                collateral_sats: v.value.raw(),
                last_height: v.state.last_height.raw(),
                cr_percent: cr_percent(v.state.debt, v.value, hi),
            })
            .collect();
        let protocol = w.protocol().ok().map(|p| Protocol {
            pot_units: p.pot.value.raw(),
            reserve_sats: p.reserve.value.raw(),
            issuer_anchor: p.issuer.state.last_mint_height.raw(),
        });
        Ok(Status {
            height: w.state.height,
            funding_address: w.funding_address().to_string(),
            lbtc_sats: w.lbtc_coins()?.iter().map(|(_, v)| *v).sum(),
            obol_units: w.obol_coins()?.iter().map(|(_, v)| *v).sum(),
            vaults,
            protocol,
            tick: tick.as_ref().map(|t| {
                let (lo, hi) = t.price_range();
                TickView { height: t.height().raw(), lo: lo.raw(), hi: hi.raw() }
            }),
            keeper,
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

/// Integer CR percent: collateral against the par value of the debt at `hi`. None for a
/// zero debt (a husk has no ratio) or without a price.
fn cr_percent(debt: Obol, coll: Sats, hi: Option<styx_core::units::Price>) -> Option<u32> {
    let hi = hi?;
    let cents = debt.covenant_cents().ok().filter(|c| *c > 0)?;
    let par = styx_core::math::coll_at_cr(cents, hi, styx_core::consts::K_PAR).raw();
    if par == 0 {
        return None;
    }
    u32::try_from(coll.raw().saturating_mul(100) / par).ok()
}

// --- the owner ops -----------------------------------------------------------------

/// `txid:vout`, the same shape the CLI prints and takes. Parsed via the elements
/// `FromStr`, which also swallows the `[elements]` prefix its `Display` adds - so a
/// pasted debug print still resolves.
fn outpoint(s: &str) -> Result<OutPoint, ApiError> {
    s.parse().map_err(|_| ApiError::BadRequest(format!("vault: expected txid:vout, got {s}")))
}

/// The plain form for responses; `OutPoint`'s own `Display` prefixes `[elements]`,
/// which is a debug shape, not an interchange one.
fn outpoint_str(op: &OutPoint) -> String {
    format!("{}:{}", op.txid, op.vout)
}

fn vault_arg(v: &Option<String>) -> Result<Option<OutPoint>, ApiError> {
    v.as_deref().map(outpoint).transpose()
}

/// What a broadcast op answers with: the txid and the successor vault, when one survives.
#[derive(Serialize)]
pub struct OpView {
    pub txid: String,
    pub vault: Option<VaultView>,
}

fn op_view(report: OpReport) -> OpView {
    OpView {
        txid: report.txid.to_string(),
        vault: report.vault.map(|v| VaultView {
            outpoint: outpoint_str(&v.outpoint),
            debt_units: v.state.debt.raw(),
            collateral_sats: v.value.raw(),
            last_height: v.state.last_height.raw(),
            // Op responses skip the ratio; the status refresh right after carries it.
            cr_percent: None,
        }),
    }
}

/// Run one blocking wallet call on the blocking pool. Poison recovery matches `status`:
/// every op begins with a sync that rebuilds the view, and the ops themselves hand state
/// changes to the chain - nothing in-memory survives a panic that the next sync would trust.
async fn run_op<F>(s: &Arc<AppState>, kind: &'static str, f: F) -> Result<Json<OpView>, ApiError>
where
    F: FnOnce(&mut Wallet) -> Result<OpReport, WalletError> + Send + 'static,
{
    let wallet = s.wallet.clone();
    let report = tokio::task::spawn_blocking(move || {
        let mut w = wallet.lock().unwrap_or_else(|e| e.into_inner());
        w.sync()?; // every op acts on a tip-fresh view, the CLI convention
        f(&mut w)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("{kind} task: {e}")))?
    .map_err(|e| {
        // Failures reach the SSE stream too, split the way the status codes split: a typed
        // refusal is the UI's sticky surface, an infrastructure error is not "you were
        // wrong".
        let event = if is_refusal(&e) { "rejected" } else { "op_error" };
        s.emit(event, format!("{kind}: {e}"));
        ApiError::from(e)
    })?;
    let view = op_view(report);
    s.emit(kind, view.txid.clone());
    Ok(Json(view))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenReq {
    pub principal: u64,
    /// Explicit collateral in sats; alternatively `cr_percent` sizes it from the tick.
    pub collateral: Option<u64>,
    pub cr_percent: Option<u32>,
}

async fn open(
    State(s): State<Arc<AppState>>,
    Json(req): Json<OpenReq>,
) -> Result<Json<OpView>, ApiError> {
    let tick = s.current_tick()?;
    let principal = Obol::new(req.principal);
    let collateral = match (req.collateral, req.cr_percent) {
        (Some(sats), None) => Sats::new(sats),
        (None, cr) => Wallet::collateral_for(principal, &tick, cr).map_err(ApiError::Wallet)?,
        (Some(_), Some(_)) => {
            return Err(ApiError::BadRequest("collateral and cr_percent are exclusive".into()))
        }
    };
    run_op(&s, "open", move |w| w.open(principal, collateral, &tick)).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmountReq {
    pub vault: Option<String>,
    pub amount: u64,
}

async fn repay(
    State(s): State<Arc<AppState>>,
    Json(req): Json<AmountReq>,
) -> Result<Json<OpView>, ApiError> {
    let which = vault_arg(&req.vault)?;
    run_op(&s, "repay", move |w| w.repay(which, Obol::new(req.amount))).await
}

async fn draw(
    State(s): State<Arc<AppState>>,
    Json(req): Json<AmountReq>,
) -> Result<Json<OpView>, ApiError> {
    let tick = s.current_tick()?;
    let which = vault_arg(&req.vault)?;
    run_op(&s, "draw", move |w| w.draw(which, Obol::new(req.amount), &tick)).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultReq {
    pub vault: Option<String>,
}

async fn refresh(
    State(s): State<Arc<AppState>>,
    Json(req): Json<VaultReq>,
) -> Result<Json<OpView>, ApiError> {
    let tick = s.current_tick()?;
    let which = vault_arg(&req.vault)?;
    run_op(&s, "refresh", move |w| w.refresh(which, &tick)).await
}

async fn close(
    State(s): State<Arc<AppState>>,
    Json(req): Json<VaultReq>,
) -> Result<Json<OpView>, ApiError> {
    let which = vault_arg(&req.vault)?;
    run_op(&s, "close", move |w| w.close(which)).await
}

async fn redeem(
    State(s): State<Arc<AppState>>,
    Json(req): Json<AmountReq>,
) -> Result<Json<OpView>, ApiError> {
    let tick = s.current_tick()?;
    let which = vault_arg(&req.vault)?;
    run_op(&s, "redeem", move |w| w.redeem(which, Obol::new(req.amount), &tick)).await
}

// --- external owner signing (the M6/M10 seam, in the UI) --------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportReq {
    /// The owner op to export: "close", "repay", or "draw" (the signature-bearing ops).
    pub op: String,
    pub vault: Option<String>,
    /// Required for repay / draw, ignored for close.
    pub amount: Option<u64>,
}

/// Export an owner op for external signing: build it unsigned, stash the plan on THIS
/// state (bounded), and return the owner digest for an off-machine key. The owner key need
/// never touch this process; the funding key stays hot and signs at apply time.
async fn export_op(
    State(s): State<Arc<AppState>>,
    Json(req): Json<ExportReq>,
) -> Result<Json<styx_pset::signing::OwnerSigningRequest>, ApiError> {
    let which = vault_arg(&req.vault)?;
    let tick = match req.op.as_str() {
        "draw" => Some(s.current_tick()?),
        _ => None,
    };
    let amount = req.amount.map(Obol::new);
    let op = req.op.clone();
    let wallet = s.wallet.clone();
    let state = s.clone();
    let request = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let mut w = wallet.lock().unwrap_or_else(|e| e.into_inner());
        let plan = match op.as_str() {
            "close" => w.close_unsigned(which)?,
            "repay" => w.repay_unsigned(which, amount.ok_or_else(|| miss("amount"))?)?,
            "draw" => {
                let tick = tick.ok_or(ApiError::NoQuorum)?;
                w.draw_unsigned(which, amount.ok_or_else(|| miss("amount"))?, &tick)?
            }
            other => return Err(ApiError::BadRequest(format!("not an owner op: {other}"))),
        };
        let request = styx_pset::signing::owner_signing_request(&w.ctx, &plan)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        state.stash_pending(request.txid.clone(), plan);
        Ok(request)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("export task: {e}")))??;
    s.emit("export", request.txid.clone());
    Ok(Json(request))
}

fn miss(field: &str) -> ApiError {
    ApiError::BadRequest(format!("{field} is required for this op"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyReq {
    pub txid: String,
    /// The external owner signature over the exported digest, hex.
    pub owner_sig: String,
}

/// Complete a previously exported plan: install the verified owner signature, sign the
/// funding inputs with the hot key, broadcast. An unknown txid is a 400; a chain that moved
/// under the plan (broadcast conflict) is a 409 telling the caller to re-export.
async fn apply_signed(
    State(s): State<Arc<AppState>>,
    Json(req): Json<ApplyReq>,
) -> Result<Json<OpView>, ApiError> {
    let wallet = s.wallet.clone();
    let state = s.clone();
    let view = tokio::task::spawn_blocking(move || -> Result<OpView, ApiError> {
        // Work on the stashed CLONE and only drop it on success: a rejected signature (wrong
        // device, typo) must not force a re-export.
        let mut plan = state
            .take_pending(&req.txid)
            .ok_or_else(|| ApiError::BadRequest(format!("no pending plan for {}", req.txid)))?;
        let w = wallet.lock().unwrap_or_else(|e| e.into_inner());
        styx_pset::signing::apply_owner_sig(&w.ctx, &mut plan, &req.owner_sig)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        match w.finalize_broadcast(&plan) {
            Ok(txid) => {
                state.drop_pending(&req.txid);
                Ok(OpView { txid: txid.to_string(), vault: None })
            }
            // The chain moved under the exported plan (the vault was liquidated or spent):
            // this plan can never land - drop it, 409, re-export.
            Err(WalletError::Broadcast(styx_node::BroadcastError::Conflict(m))) => {
                state.drop_pending(&req.txid);
                Err(ApiError::Stale(m))
            }
            // A transient failure (the node is down): the signature is still good and the
            // plan may yet apply - KEEP it pending, surface the error, let the caller retry.
            Err(e) => Err(ApiError::from(e)),
        }
    })
    .await
    .map_err(|e| ApiError::Internal(format!("apply task: {e}")))??;
    s.emit("apply", view.txid.clone());
    Ok(Json(view))
}

/// The full application router: the token-gated API merged with the page-tier UI (its
/// own Host/Origin-only gate).
pub fn router(state: Arc<AppState>, g: Arc<Gate>) -> Router {
    crate::ui::ui_router(g.clone()).merge(api_router(state, g))
}

fn api_router(state: Arc<AppState>, g: Arc<Gate>) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/open", post(open))
        .route("/api/repay", post(repay))
        .route("/api/draw", post(draw))
        .route("/api/refresh", post(refresh))
        .route("/api/close", post(close))
        .route("/api/redeem", post(redeem))
        .route("/api/keeper/start", post(keeper_start))
        .route("/api/keeper/stop", post(keeper_stop))
        .route("/api/export", post(export_op))
        .route("/api/apply", post(apply_signed))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(g, gate))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn unknown_request_fields_are_refused() {
        // A typo ("colateral") must be a deserialization error, never a silent default:
        // this is a money API.
        assert!(serde_json::from_str::<OpenReq>(r#"{"principal":1,"colateral":2}"#).is_err());
        assert!(serde_json::from_str::<AmountReq>(r#"{"amount":1,"valut":"x:0"}"#).is_err());
        assert!(serde_json::from_str::<VaultReq>(r#"{"vualt":"x:0"}"#).is_err());
        assert!(serde_json::from_str::<OpenReq>(r#"{"principal":1,"collateral":2}"#).is_ok());
    }
}
