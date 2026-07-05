//! The five frozen v1 covenant sources, embedded at compile time, and their compilation.
//!
//! The `.simf` files live at the repo root (`covenants/`), next to the spec site that documents
//! them. They are behaviourally frozen: `golden::frozen_cmrs_match_v1_freeze` compiles each one
//! against fixed dummy params and asserts its CMR against the values recorded at the freeze.
//!
//! Compile order follows the CMR DAG (the issuer pins the vault CMR; vault and stability pin the
//! pot and each other's scriptHash as flat params): POT leaves, STABILITY, VAULT, ISSUER.

use simplicityhl::ast::ElementsJetHinter;
use simplicityhl::{elements, Arguments, CompiledProgram};

use crate::U256;

/// One of the five frozen v1 covenants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Covenant {
    /// The OBOL pot's inflow leaf: the pot may only grow (repay / close / liquidate / redeem /
    /// bad-debt all return OBOL to it).
    ReserveRepay,
    /// The OBOL pot's outflow leaf: OBOL leaves only on OPEN / DRAW, gated on the issuer token.
    PotOutflow,
    /// The stability reserve: constant-address L-BTC pool; accumulate / bad-debt arms.
    Stability,
    /// The per-position CDP vault: the 8-op ladder.
    Vault,
    /// The mint authority: issuer singleton with the global mint-recency ratchet.
    Issuer,
}

impl Covenant {
    /// All five, in compile (CMR-DAG) order.
    pub const ALL: [Covenant; 5] = [
        Covenant::ReserveRepay,
        Covenant::PotOutflow,
        Covenant::Stability,
        Covenant::Vault,
        Covenant::Issuer,
    ];

    /// The covenant's file / display name.
    pub fn name(self) -> &'static str {
        match self {
            Covenant::ReserveRepay => "reserve_repay",
            Covenant::PotOutflow => "pot_outflow",
            Covenant::Stability => "stability",
            Covenant::Vault => "vault",
            Covenant::Issuer => "issuer",
        }
    }

    /// The frozen SimplicityHL source, embedded from `covenants/<name>.simf`.
    pub fn source(self) -> &'static str {
        match self {
            Covenant::ReserveRepay => include_str!("../../../covenants/reserve_repay.simf"),
            Covenant::PotOutflow => include_str!("../../../covenants/pot_outflow.simf"),
            Covenant::Stability => include_str!("../../../covenants/stability.simf"),
            Covenant::Vault => include_str!("../../../covenants/vault.simf"),
            Covenant::Issuer => include_str!("../../../covenants/issuer.simf"),
        }
    }
}

/// A covenant source failed to compile under the given params. Since the sources are frozen and
/// embedded, this is a config error (bad params), not a source error.
#[derive(Debug, thiserror::Error)]
#[error("compile {covenant}: {message}")]
pub struct CompileError {
    pub covenant: &'static str,
    pub message: String,
}

/// Compile a frozen covenant against the given per-deploy params.
pub fn compile(cov: Covenant, args: Arguments) -> Result<CompiledProgram, CompileError> {
    CompiledProgram::new(cov.source(), args, false, Box::new(ElementsJetHinter::new())).map_err(|e| {
        CompileError { covenant: cov.name(), message: e.to_string() }
    })
}

/// The covenant's CMR as the taproot leaf script: in Elements' Simplicity leaf encoding the
/// leaf script is the raw 32 CMR bytes.
pub fn cmr_script(prog: &CompiledProgram) -> elements::Script {
    elements::Script::from(prog.commit().cmr().as_ref().to_vec())
}

/// The covenant's CMR as a U256, used for cross-covenant pins (VAULT_CMR) and the freeze gate.
pub fn cmr_u256(prog: &CompiledProgram) -> U256 {
    U256::from_byte_array(prog.commit().cmr().to_byte_array())
}
