//! Oracle ticks: the signed (height, price, backing_k) triples the covenants verify.
//!
//! A tick is signed over sha256(height(4 BE) || price(4 BE) || backing_k(4 BE)) and anchored
//! to the chain by `check_lock_height`: the covenant requires the spending tx's nLockTime to
//! be at least the tick height. backing_k is co-signed in the same digest, so one quorum
//! agreement covers both the price and the backing ratio.
//!
//! `verify_quorum` in the covenants requires exactly three active slots out of five;
//! `OracleTick::new` takes exactly three quotes at distinct slots, so every tick this crate
//! can represent meets the quorum shape.

use simplicityhl::elements::hashes::{sha256, Hash};
use simplicityhl::elements::secp256k1_zkp as zkp;

use crate::units::{BlockHeight, Price, RatioK};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OracleError {
    #[error("oracle slot {0} out of range (0..=4)")]
    InvalidSlot(u8),
    #[error("duplicate oracle slot {0}")]
    DuplicateSlot(u8),
}

/// What a tick commits to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickPayload {
    pub height: BlockHeight,
    pub price: Price,
    pub backing_k: RatioK,
}

/// The digest each oracle signs: sha256(height BE || price BE || backing_k BE).
pub fn tick_digest(p: &TickPayload) -> [u8; 32] {
    let mut buf = p.height.raw().to_be_bytes().to_vec();
    buf.extend_from_slice(&p.price.raw().to_be_bytes());
    buf.extend_from_slice(&p.backing_k.raw().to_be_bytes());
    sha256::Hash::hash(&buf).to_byte_array()
}

/// One of the five oracle slots (the position of a key in `Params::oracle_pks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleSlot(u8);

impl OracleSlot {
    pub fn new(index: u8) -> Result<Self, OracleError> {
        if index > 4 {
            return Err(OracleError::InvalidSlot(index));
        }
        Ok(OracleSlot(index))
    }
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A price signed by one oracle. The signature covers the full tick digest (height, price,
/// backing_k), not the price alone.
///
/// A zero price is representable here and the covenants reject it (vault.simf:84 asserts
/// 0 < price); builders must refuse to build with one before that.
#[derive(Debug, Clone, Copy)]
pub struct SignedQuote {
    pub price: Price,
    pub sig: [u8; 64],
}

/// A 3-of-5 oracle tick: exactly three quotes at distinct slots.
#[derive(Debug, Clone)]
pub struct OracleTick {
    height: BlockHeight,
    backing_k: RatioK,
    slots: [Option<SignedQuote>; 5],
}

impl OracleTick {
    pub fn new(
        height: BlockHeight,
        backing_k: RatioK,
        quotes: [(OracleSlot, SignedQuote); 3],
    ) -> Result<Self, OracleError> {
        let mut slots = [None; 5];
        for (slot, quote) in quotes {
            if slots[slot.index()].is_some() {
                return Err(OracleError::DuplicateSlot(slot.0));
            }
            slots[slot.index()] = Some(quote);
        }
        Ok(OracleTick { height, backing_k, slots })
    }

    pub fn height(&self) -> BlockHeight {
        self.height
    }
    pub fn backing_k(&self) -> RatioK {
        self.backing_k
    }
    pub fn slots(&self) -> &[Option<SignedQuote>; 5] {
        &self.slots
    }
}

/// Sign a tick payload with one oracle key (BIP340, no auxiliary randomness, so test fixtures
/// are deterministic).
pub fn sign_quote(kp: &zkp::Keypair, payload: &TickPayload) -> SignedQuote {
    let msg = zkp::Message::from_digest(tick_digest(payload));
    let sig = crate::secp().sign_schnorr_no_aux_rand(&msg, kp);
    SignedQuote { price: payload.price, sig: *sig.as_ref() }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn quote() -> SignedQuote {
        SignedQuote { price: Price::new(120_000), sig: [0u8; 64] }
    }

    #[test]
    fn duplicate_slot_is_rejected() {
        let err = OracleTick::new(
            BlockHeight::new(1),
            RatioK::from_cr_percent(100),
            [
                (OracleSlot::new(0).unwrap(), quote()),
                (OracleSlot::new(0).unwrap(), quote()),
                (OracleSlot::new(2).unwrap(), quote()),
            ],
        );
        assert_eq!(err.map(|_| ()), Err(OracleError::DuplicateSlot(0)));
    }

    #[test]
    fn slot_range_is_checked() {
        assert_eq!(OracleSlot::new(5).map(|_| ()), Err(OracleError::InvalidSlot(5)));
        assert!(OracleSlot::new(4).is_ok());
    }

    #[test]
    fn sign_quote_verifies_against_the_tick_digest() {
        let secp = zkp::Secp256k1::new();
        let mut sk = [0u8; 32];
        sk[31] = 7;
        let kp = zkp::Keypair::from_seckey_slice(&secp, &sk).unwrap();
        let payload = TickPayload {
            height: BlockHeight::new(100),
            price: Price::new(120_000),
            backing_k: RatioK::from_cr_percent(100),
        };
        let q = sign_quote(&kp, &payload);
        assert_eq!(q.price, payload.price);

        let sig = zkp::schnorr::Signature::from_slice(&q.sig).unwrap();
        let msg = zkp::Message::from_digest(tick_digest(&payload));
        secp.verify_schnorr(&sig, &msg, &kp.x_only_public_key().0).unwrap();

        // The same signature must not verify for any other payload.
        let other = zkp::Message::from_digest(tick_digest(&TickPayload {
            height: BlockHeight::new(101),
            ..payload
        }));
        assert!(secp.verify_schnorr(&sig, &other, &kp.x_only_public_key().0).is_err());

        // no_aux_rand makes the signature deterministic; this pins the whole signing path
        // (digest construction included).
        let sig_hex: String = q.sig.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(sig_hex, GOLDEN_SIG);
    }

    const GOLDEN_SIG: &str = "3124c7c89059427620c8e79c1331c55bcc10db5c665df63cf52a15ca6a858284a8aad148b4dead57db0ed3c181904256b16fbe7f9af263c365ec90a8fc5761b4";

    #[test]
    fn tick_digest_commits_to_every_field() {
        let base = TickPayload {
            height: BlockHeight::new(100),
            price: Price::new(120_000),
            backing_k: RatioK::from_cr_percent(100),
        };
        let d = tick_digest(&base);
        assert_ne!(d, tick_digest(&TickPayload { height: BlockHeight::new(101), ..base }));
        assert_ne!(d, tick_digest(&TickPayload { price: Price::new(120_001), ..base }));
        assert_ne!(d, tick_digest(&TickPayload { backing_k: RatioK::new(1), ..base }));
    }
}
