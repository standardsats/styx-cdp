//! The issuer covenant's OP sum (issuer.simf, `fn main`):
//!
//! ```text
//! open   = Left( (principal, (owner, tick)) )
//! draw   = Right(Left( (old_debt, (owner, (old_last_height, (new_debt, draw_height)))) ))
//! poke   = Right(Right(Left( tick )))
//! attest = Right(Right(Right( (debt, (owner, (last_height, tick))) )))
//! ```

use super::{lower_tick, Either, Tick, ToSimf};
use crate::oracle::OracleTick;
use crate::units::{BlockHeight, Obol};
use crate::U256;
use simplicityhl::Value;

pub type OpenSum = (u64, (U256, Tick));
pub type DrawSum = (u64, (U256, (u32, (u64, u32))));
pub type AttestSum = (u64, (U256, (u32, Tick)));

/// The full OP sum, structurally identical to the covenant's witness type.
pub type IssuerOpSum = Either<OpenSum, Either<DrawSum, Either<Tick, AttestSum>>>;

/// One issuer operation. `owner` is a raw u256: the covenant commits whatever the witness
/// carries, valid x-only point or not (see `VaultState::owner`).
#[derive(Debug, Clone)]
pub enum IssuerOp {
    Open {
        principal: Obol,
        owner: U256,
        tick: OracleTick,
    },
    Draw {
        old_debt: Obol,
        owner: U256,
        old_last_height: BlockHeight,
        new_debt: Obol,
        draw_height: BlockHeight,
    },
    Poke {
        tick: OracleTick,
    },
    Attest {
        debt: Obol,
        owner: U256,
        last_height: BlockHeight,
        tick: OracleTick,
    },
}

/// The one place issuer arm positions are spelled.
pub fn lower_issuer_op(op: &IssuerOp) -> IssuerOpSum {
    use Either::{Left, Right};
    match op {
        IssuerOp::Open { principal, owner, tick } => Left((principal.raw(), (*owner, lower_tick(tick)))),
        IssuerOp::Draw { old_debt, owner, old_last_height, new_debt, draw_height } => Right(Left((
            old_debt.raw(),
            (*owner, (old_last_height.raw(), (new_debt.raw(), draw_height.raw()))),
        ))),
        IssuerOp::Poke { tick } => Right(Right(Left(lower_tick(tick)))),
        IssuerOp::Attest { debt, owner, last_height, tick } => {
            Right(Right(Right((debt.raw(), (*owner, (last_height.raw(), lower_tick(tick)))))))
        }
    }
}

pub fn issuer_op_value(op: &IssuerOp) -> Value {
    lower_issuer_op(op).value()
}
