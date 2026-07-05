//! The vault covenant's OP sum (vault.simf, `fn main`):
//!
//! ```text
//! owner op  = Left( (owner_sig, kind) )   kind = Left(()) close | Right(Left(r)) repay
//!                                                | Right(Right((d, tick))) draw
//! partial   = Right( Left( (dd, tick) ) )
//! full-liq  = Right( Right( Left( tick ) ) )
//! bad-debt  = Right( Right( Right( Left( tick ) ) ) )
//! redeem    = Right( Right( Right( Right( Left( (x, tick) ) ) ) ) )
//! refresh   = Right( Right( Right( Right( Right( tick ) ) ) ) )
//! ```

use super::{lower_tick, Either, Sig, Tick, ToSimf};
use crate::oracle::OracleTick;
use crate::units::Obol;
use simplicityhl::Value;

/// close | repay(r) | draw(d, tick).
pub type OwnerKind = Either<(), Either<u64, (u64, Tick)>>;

/// The full OP sum, structurally identical to the covenant's witness type.
pub type VaultOpSum =
    Either<(Sig, OwnerKind), Either<(u64, Tick), Either<Tick, Either<Tick, Either<(u64, Tick), Tick>>>>>;

/// One vault operation. Owner ops carry the signature as data: encoding is separate from
/// signing, which the PSET pipeline needs (the sighash exists only once the tx body is fixed).
#[derive(Debug, Clone)]
pub enum VaultOp {
    Close { owner_sig: Sig },
    Repay { owner_sig: Sig, amount: Obol },
    Draw { owner_sig: Sig, amount: Obol, tick: OracleTick },
    Liquidate { dd: Obol, tick: OracleTick },
    FullLiq { tick: OracleTick },
    BadDebt { tick: OracleTick },
    Redeem { x: Obol, tick: OracleTick },
    Refresh { tick: OracleTick },
}

/// The one place vault arm positions are spelled.
pub fn lower_vault_op(op: &VaultOp) -> VaultOpSum {
    use Either::{Left, Right};
    match op {
        VaultOp::Close { owner_sig } => Left((*owner_sig, Left(()))),
        VaultOp::Repay { owner_sig, amount } => Left((*owner_sig, Right(Left(amount.raw())))),
        VaultOp::Draw { owner_sig, amount, tick } => {
            Left((*owner_sig, Right(Right((amount.raw(), lower_tick(tick))))))
        }
        VaultOp::Liquidate { dd, tick } => Right(Left((dd.raw(), lower_tick(tick)))),
        VaultOp::FullLiq { tick } => Right(Right(Left(lower_tick(tick)))),
        VaultOp::BadDebt { tick } => Right(Right(Right(Left(lower_tick(tick))))),
        VaultOp::Redeem { x, tick } => Right(Right(Right(Right(Left((x.raw(), lower_tick(tick))))))),
        VaultOp::Refresh { tick } => Right(Right(Right(Right(Right(lower_tick(tick)))))),
    }
}

pub fn vault_op_value(op: &VaultOp) -> Value {
    lower_vault_op(op).value()
}
