//! Oracle quotes: the wire format, the verified per-height cache, and tick assembly.
//!
//! A quote's authority is OUR BIP340 signature over `tick_digest(height, price, backing_k)`,
//! verified against the covenant oracle key of its slot BEFORE anything is cached - the
//! transport signature (Nostr) is carriage, not trust. The Nostr key of an oracle is a
//! separate identity used only for relay filtering and replacement semantics.
//!
//! `QuoteBook` keys quotes by (height, slot) and assembles `OracleTick`s: the tick
//! constructor already enforces the 3-distinct-slots quorum shape, and the covenant verifies
//! one common backing_k across the quorum, so assembly groups by backing_k and picks the
//! value with the widest support.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use styx_core::elements::secp256k1_zkp as zkp;
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::oracle::{tick_digest, OracleSlot, OracleTick, SignedQuote, TickPayload};
use styx_core::units::{BlockHeight, Price, RatioK};

/// One oracle quote as it travels: the JSON content of a transport event. `sig` is the
/// BIP340 signature (128 hex chars) over the tick digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireQuote {
    pub slot: u8,
    pub height: u32,
    pub price: u32,
    pub backing_k: u32,
    pub sig: String,
    /// The oracle's self-declared display label, carried for the explorer. Outside the
    /// signed digest (the covenant never sees it); the Nostr event's own signature is what
    /// authenticates it as this oracle's. Empty when the oracle set no name.
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuoteError {
    #[error("malformed quote json: {0}")]
    Json(String),
    #[error("slot {0} out of range (0..=4)")]
    Slot(u8),
    #[error("zero price from slot {slot} at height {height}")]
    ZeroPrice { slot: u8, height: u32 },
    #[error("malformed signature encoding")]
    SigEncoding,
    #[error("signature does not verify for slot {slot} at height {height}")]
    BadSignature { slot: u8, height: u32 },
    #[error("height {height} is stale: tip {tip}, horizon {horizon}")]
    Stale { height: u32, tip: u32, horizon: u32 },
    #[error("height {height} is ahead of tip {tip} beyond the drift allowance")]
    Future { height: u32, tip: u32 },
}

impl WireQuote {
    /// Sign a payload as the given slot. `sign_quote` is deterministic (no aux rand), so a
    /// re-publication of the same payload is byte-identical.
    pub fn sign(kp: &zkp::Keypair, slot: OracleSlot, payload: &TickPayload) -> WireQuote {
        let q = styx_core::oracle::sign_quote(kp, payload);
        WireQuote {
            slot: slot.index() as u8,
            height: payload.height.raw(),
            price: payload.price.raw(),
            backing_k: payload.backing_k.raw(),
            sig: q.sig.iter().map(|b| format!("{b:02x}")).collect(),
            name: String::new(),
        }
    }

    pub fn payload(&self) -> TickPayload {
        TickPayload {
            height: BlockHeight::new(self.height),
            price: Price::new(self.price),
            backing_k: RatioK::new(self.backing_k),
        }
    }

    pub fn to_json(&self) -> String {
        // A struct of plain integers and a string cannot fail to serialize.
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<WireQuote, QuoteError> {
        serde_json::from_str(s).map_err(|e| QuoteError::Json(e.to_string()))
    }

    fn sig_bytes(&self) -> Result<[u8; 64], QuoteError> {
        let mut out = [0u8; 64];
        if self.sig.len() != 128 {
            return Err(QuoteError::SigEncoding);
        }
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&self.sig[i * 2..i * 2 + 2], 16)
                .map_err(|_| QuoteError::SigEncoding)?;
        }
        Ok(out)
    }
}

/// The verified quote cache. Every insert re-derives the tick digest from the claimed
/// fields and verifies the signature against the slot's covenant key, so a quote lying
/// about any field (a signature transplanted onto another height, a foreign key, a wrong
/// slot) never enters the book.
pub struct QuoteBook {
    oracle_pks: [XOnlyPublicKey; 5],
    /// Accepted window below the tip; older heights are rejected and pruned.
    horizon: u32,
    /// Accepted blocks above the tip (clock skew between nodes).
    drift: u32,
    heights: BTreeMap<u32, [Option<(RatioK, SignedQuote)>; 5]>,
}

pub const DEFAULT_HORIZON: u32 = 32;
pub const DEFAULT_DRIFT: u32 = 2;

impl QuoteBook {
    pub fn new(oracle_pks: [XOnlyPublicKey; 5]) -> Self {
        Self::with_window(oracle_pks, DEFAULT_HORIZON, DEFAULT_DRIFT)
    }

    pub fn with_window(oracle_pks: [XOnlyPublicKey; 5], horizon: u32, drift: u32) -> Self {
        QuoteBook { oracle_pks, horizon, drift, heights: BTreeMap::new() }
    }

    /// Verify and cache one wire quote against the current tip. Rejections are total: a
    /// rejected quote leaves no trace.
    ///
    /// Covenant-invalid quotes are rejected here too: the covenants refuse a zero price at
    /// the quorum layer, so a validly signed zero from one byzantine oracle must never
    /// enter the book - assembly picks slots blindly, and a poisoned tick would jam the
    /// consumer at every height when 3-of-5 is supposed to survive two byzantine slots.
    pub fn insert(&mut self, q: &WireQuote, tip: BlockHeight) -> Result<(), QuoteError> {
        let slot = OracleSlot::new(q.slot).map_err(|_| QuoteError::Slot(q.slot))?;
        if q.price == 0 {
            return Err(QuoteError::ZeroPrice { slot: q.slot, height: q.height });
        }
        let floor = tip.raw().saturating_sub(self.horizon);
        if q.height < floor {
            return Err(QuoteError::Stale { height: q.height, tip: tip.raw(), horizon: self.horizon });
        }
        if q.height > tip.raw().saturating_add(self.drift) {
            return Err(QuoteError::Future { height: q.height, tip: tip.raw() });
        }
        let sig_bytes = q.sig_bytes()?;
        let sig =
            zkp::schnorr::Signature::from_slice(&sig_bytes).map_err(|_| QuoteError::SigEncoding)?;
        let msg = zkp::Message::from_digest(tick_digest(&q.payload()));
        styx_core::secp()
            .verify_schnorr(&sig, &msg, &self.oracle_pks[slot.index()])
            .map_err(|_| QuoteError::BadSignature { slot: q.slot, height: q.height })?;

        let entry = self.heights.entry(q.height).or_insert([None; 5]);
        entry[slot.index()] =
            Some((RatioK::new(q.backing_k), SignedQuote { price: Price::new(q.price), sig: sig_bytes }));
        self.heights.retain(|h, _| *h >= floor);
        Ok(())
    }

    /// How many verified quotes the book holds for a height.
    pub fn count(&self, height: BlockHeight) -> usize {
        self.heights.get(&height.raw()).map(|slots| slots.iter().flatten().count()).unwrap_or(0)
    }

    /// Assemble a 3-of-5 tick for the height, if a quorum exists. The covenant verifies one
    /// common backing_k across the quorum, so quotes are grouped by it and the value with
    /// the widest support wins (ties to the smaller k); the three lowest supporting slots
    /// are taken. Returns None below quorum.
    pub fn assemble_tick(&self, height: BlockHeight) -> Option<OracleTick> {
        let slots = self.heights.get(&height.raw())?;
        let mut by_k: BTreeMap<u32, Vec<(usize, SignedQuote)>> = BTreeMap::new();
        for (i, entry) in slots.iter().enumerate() {
            if let Some((k, q)) = entry {
                by_k.entry(k.raw()).or_default().push((i, *q));
            }
        }
        let (k_raw, quorum) = by_k
            .into_iter()
            .filter(|(_, v)| v.len() >= 3)
            .max_by_key(|(k, v)| (v.len(), std::cmp::Reverse(*k)))?;
        let backing_k = RatioK::new(k_raw);
        let take: Vec<(OracleSlot, SignedQuote)> = quorum
            .into_iter()
            .take(3)
            .filter_map(|(i, q)| Some((OracleSlot::new(i as u8).ok()?, q)))
            .collect();
        let quotes: [(OracleSlot, SignedQuote); 3] = take.try_into().ok()?;
        OracleTick::new(height, backing_k, quotes).ok()
    }
}
