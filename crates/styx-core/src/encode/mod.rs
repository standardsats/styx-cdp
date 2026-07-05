//! Type-safe witness encoders for the three op-dispatching covenants.
//!
//! Each covenant's `witness::OP` is a nested `Either` sum. This module mirrors those sums as
//! Rust type aliases over `ToSimf` (so the encoding is total and the nesting is checked by the
//! compiler) and exposes one friendly enum per covenant with a single `lower` function - the
//! only place arm positions are spelled.
//!
//! The `witness_types` test tier asserts these types equal the covenant-declared ones; M5's
//! discrimination matrix proves each variant prunes to its intended arm.

mod issuer_op;
mod simf;
mod stability_op;
mod vault_op;

pub use issuer_op::{issuer_op_value, lower_issuer_op, IssuerOp, IssuerOpSum};
pub use simf::{Either, Sig, ToSimf};
pub use stability_op::{lower_stability_op, stability_op_value, StabilityOp, StabilityOpSum};
pub use vault_op::{lower_vault_op, vault_op_value, OwnerKind, VaultOp, VaultOpSum};

use crate::oracle::OracleTick;

/// One oracle slot: absent, or (price, signature).
pub type Slot = Either<(), (u32, Sig)>;

/// A 3-of-5 tick as the covenants read it: (height, (backing_k, [slot; 5])).
pub type Tick = (u32, (u32, [Slot; 5]));

/// Place the tick's three quotes into their slots; the other two slots are absent.
pub fn lower_tick(tick: &OracleTick) -> Tick {
    let slots = tick
        .slots()
        .map(|s| match s {
            None => Either::Left(()),
            Some(q) => Either::Right((q.price.raw(), Sig(q.sig))),
        });
    (tick.height().raw(), (tick.backing_k().raw(), slots))
}
