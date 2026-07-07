//! The explorer's live state: the indexed chain (candidate-free - every vault is tracked
//! opaque, which is all a public view needs), a ring of recent protocol events, the
//! freshest assembled tick, and per-slot quote recency from the relay.

use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};
use std::time::Instant;

use serde::Serialize;
use styx_core::consts::{K_FULL_LIQ_CAP, K_HEALTH_GATE, K_PAR};
use styx_core::math::coll_at_cr;
use styx_core::oracle::OracleTick;
use styx_core::units::{Obol, Price, Sats};
use styx_watch::index::{IndexState, Notice};

/// How many recent events the feed keeps.
const EVENTS_KEPT: usize = 100;

/// A tick older than this is not shown as current (the app's constant, same rationale).
pub const TICK_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(180);
/// A tick older than this still prices the bands (until `TICK_MAX_AGE`) but is flagged
/// stale: the oracle publishes roughly per block, so a tick this old has missed one and
/// the health it implies may be behind the chain.
pub const TICK_FRESH: std::time::Duration = std::time::Duration::from_secs(90);

pub struct ExplorerState {
    pub index: RwLock<IndexState>,
    events: Mutex<VecDeque<Notice>>,
    tick: RwLock<Option<(OracleTick, Instant)>>,
    slot_seen: Mutex<[Option<Instant>; 5]>,
    slot_name: Mutex<[Option<String>; 5]>,
    slot_price: Mutex<[Option<u32>; 5]>,
    /// The quote relay users can point a client at; shown on the page. From the config.
    relay: Option<String>,
}

impl ExplorerState {
    pub fn new(index: IndexState, relay: Option<String>) -> std::sync::Arc<ExplorerState> {
        std::sync::Arc::new(ExplorerState {
            index: RwLock::new(index),
            events: Mutex::new(VecDeque::new()),
            tick: RwLock::new(None),
            slot_seen: Mutex::new([None; 5]),
            slot_name: Mutex::new([const { None }; 5]),
            slot_price: Mutex::new([None; 5]),
            relay,
        })
    }

    pub fn push_events(&self, notices: Vec<Notice>) {
        let mut ev = self.events.lock().unwrap_or_else(|e| e.into_inner());
        for n in notices {
            ev.push_front(n);
        }
        ev.truncate(EVENTS_KEPT);
    }

    pub fn set_tick(&self, tick: OracleTick) {
        *self.tick.write().unwrap_or_else(|e| e.into_inner()) = Some((tick, Instant::now()));
    }

    pub fn tick(&self) -> Option<OracleTick> {
        self.tick
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(_, at)| at.elapsed() <= TICK_MAX_AGE)
            .map(|(t, _)| t.clone())
    }

    pub fn saw_slot(&self, slot: usize, name: &str, price: u32) {
        if let Some(s) = self.slot_seen.lock().unwrap_or_else(|e| e.into_inner()).get_mut(slot) {
            *s = Some(Instant::now());
        }
        if let Some(n) = self.slot_name.lock().unwrap_or_else(|e| e.into_inner()).get_mut(slot) {
            *n = (!name.is_empty()).then(|| name.to_string());
        }
        if let Some(p) = self.slot_price.lock().unwrap_or_else(|e| e.into_inner()).get_mut(slot) {
            *p = Some(price);
        }
    }

    /// One consistent snapshot for both the page and /api/state.
    pub fn view(&self) -> View {
        // Read the tick with its age in one snapshot: the bands and the freshness marker
        // must describe the same tick.
        let ticked: Option<(OracleTick, u64)> = self
            .tick
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(_, at)| at.elapsed() <= TICK_MAX_AGE)
            .map(|(t, at)| (t.clone(), at.elapsed().as_secs()));
        let hi = ticked.as_ref().map(|(t, _)| t.price_range().1);
        let index = self.index.read().unwrap_or_else(|e| e.into_inner());
        let protocol = index.protocol().map(|p| {
            let singleton = |name: &'static str, op: &styx_core::elements::OutPoint| SingletonView {
                name,
                outpoint: op.to_string(),
                txid: op.txid.to_string(),
            };
            ProtocolView {
                pot_units: p.pot.value.raw(),
                reserve_sats: p.reserve.value.raw(),
                issuer_anchor: p.issuer.state.last_mint_height.raw(),
                singletons: vec![
                    singleton("pot", &p.pot.outpoint),
                    singleton("reserve", &p.reserve.outpoint),
                    singleton("issuer", &p.issuer.outpoint),
                ],
            }
        });
        let mut vaults: Vec<VaultView> = index
            .vaults
            .iter()
            .map(|(op, v)| VaultView {
                outpoint: op.to_string(),
                txid: op.txid.to_string(),
                debt_units: v.debt.raw(),
                collateral_sats: v.value.raw(),
                last_height: v.last_height.raw(),
                cr_percent: cr_percent(v.debt, v.value, hi),
                band: band(v.debt, v.value, hi),
            })
            .collect();
        vaults.sort_by(|a, b| a.cr_percent.unwrap_or(u32::MAX).cmp(&b.cr_percent.unwrap_or(u32::MAX)));
        let events = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|n| EventView {
                height: n.height,
                txid: n.txid.to_string(),
                what: format!("{:?}", n.event),
            })
            .collect();
        let ages = *self.slot_seen.lock().unwrap_or_else(|e| e.into_inner());
        let names = self.slot_name.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let prices = *self.slot_price.lock().unwrap_or_else(|e| e.into_inner());
        View {
            height: index.height,
            protocol,
            tick: ticked.as_ref().map(|(t, age)| {
                let (lo, hi) = t.price_range();
                TickView { height: t.height().raw(), lo: lo.raw(), hi: hi.raw(), age_secs: *age }
            }),
            pricing_stale: ticked.as_ref().is_some_and(|(_, age)| *age > TICK_FRESH.as_secs()),
            vaults,
            lost: index.lost.len(),
            events,
            oracle_age_secs: ages.map(|s| s.map(|at| at.elapsed().as_secs())),
            oracle_name: names,
            oracle_price: prices,
            relay: self.relay.clone(),
        }
    }
}

/// Integer CR percent at `hi`. DISPLAY ONLY: the floor division loses up to a percentage
/// point at a boundary; the band never derives from it.
fn cr_percent(debt: Obol, coll: Sats, hi: Option<Price>) -> Option<u32> {
    let hi = hi?;
    let cents = debt.covenant_cents().ok().filter(|c| *c > 0)?;
    let par = coll_at_cr(cents, hi, K_PAR).raw();
    if par == 0 {
        return None;
    }
    u32::try_from(coll.raw().saturating_mul(100) / par).ok()
}

/// The covenant band, classified in the SATOSHI domain with the same three `coll_at_cr`
/// thresholds the keeper ladder compares against (decide.rs): strict below par, inclusive
/// 115 cap, strict 130 gate. An integer CR percent is a whole point too coarse at the cap
/// (every collateral in (cap, next-percent] floors to 115), and a public page naming the
/// wrong band is the kind of thing that gets noticed.
fn band(debt: Obol, coll: Sats, hi: Option<Price>) -> &'static str {
    if debt == Obol::ZERO {
        return "husk";
    }
    let (Some(hi), Ok(cents)) = (hi, debt.covenant_cents()) else {
        return "unpriced";
    };
    let floor = coll_at_cr(cents, hi, K_PAR);
    let cap = coll_at_cr(cents, hi, K_FULL_LIQ_CAP);
    let gate = coll_at_cr(cents, hi, K_HEALTH_GATE);
    if coll < floor {
        "bad-debt"
    } else if coll <= cap {
        "full-liq"
    } else if coll < gate {
        "partial"
    } else {
        "healthy"
    }
}

#[derive(Serialize)]
pub struct ProtocolView {
    pub pot_units: u64,
    pub reserve_sats: u64,
    pub issuer_anchor: u32,
    /// The protocol's on-chain singleton outputs, for cross-checking against a public explorer.
    pub singletons: Vec<SingletonView>,
}

#[derive(Serialize)]
pub struct SingletonView {
    pub name: &'static str,
    /// The full `txid:vout` for display.
    pub outpoint: String,
    /// The txid alone, for the public-explorer link.
    pub txid: String,
}

#[derive(Serialize)]
pub struct TickView {
    pub height: u32,
    pub lo: u32,
    pub hi: u32,
    /// Seconds since this tick was assembled - how current the vault bands are.
    pub age_secs: u64,
}

#[derive(Serialize)]
pub struct VaultView {
    pub outpoint: String,
    /// The txid alone, for the public-explorer link.
    pub txid: String,
    pub debt_units: u64,
    pub collateral_sats: u64,
    pub last_height: u32,
    pub cr_percent: Option<u32>,
    pub band: &'static str,
}

#[derive(Serialize)]
pub struct EventView {
    pub height: u32,
    pub txid: String,
    pub what: String,
}

#[derive(Serialize)]
pub struct View {
    pub height: u32,
    pub protocol: Option<ProtocolView>,
    pub tick: Option<TickView>,
    /// The current tick is older than `TICK_FRESH`: bands may lag the chain.
    pub pricing_stale: bool,
    pub vaults: Vec<VaultView>,
    pub lost: usize,
    pub events: Vec<EventView>,
    /// Seconds since each oracle slot last published to the relay; None = never seen.
    pub oracle_age_secs: [Option<u64>; 5],
    /// Each slot's self-declared name from its latest quote; None = unnamed or never seen.
    pub oracle_name: [Option<String>; 5],
    /// Each slot's last published price (USD); None = never seen.
    pub oracle_price: [Option<u32>; 5],
    /// The quote relay address to show, if configured.
    pub relay: Option<String>,
}
