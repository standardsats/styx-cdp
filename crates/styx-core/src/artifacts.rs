//! The five frozen v1 covenant sources, their compilation, and taproot derivation.
//!
//! The `.simf` files live at the repo root (`covenants/`), next to the spec site that documents
//! them. They are behaviourally frozen: `golden::frozen_cmrs_match_v1_freeze` compiles each one
//! against fixed dummy params and asserts its CMR against the values recorded at the freeze.
//!
//! `Artifacts::compile` builds a full deploy from `Params`: it compiles in CMR-DAG order
//! (POT leaves, STABILITY, VAULT, ISSUER) and wires every cross-covenant pin (POT_SPK,
//! STABILITY_SPK, VAULT_CMR) from the just-compiled sibling, so a stale pin cannot exist.

use std::collections::HashMap;

use simplicityhl::ast::ElementsJetHinter;
use simplicityhl::elements::hashes::{sha256, Hash};
use simplicityhl::elements::taproot::{ControlBlock, LeafVersion, TaprootBuilder, TaprootSpendInfo};
use simplicityhl::elements::{Address, AddressParams, Script};
use simplicityhl::str::WitnessName;
use simplicityhl::value::ValueConstructible;
use simplicityhl::{elements, simplicity, Arguments, CompiledProgram, Value};

use crate::consts::{issuer_leaf_vc, leaf_vc, nums_key, tapleaf_tag};
use crate::domain::{IssuerState, VaultState};
use crate::leaves::{issuer_data_leaf, vault_data_leaf};
use crate::params::{asset_u256, xonly_u256, Params, PreflightError};
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

/// A covenant source failed to compile under the given params. The sources are frozen and
/// embedded, so a failure here means bad params.
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

/// Parse a frozen covenant without instantiating params. Exposes what `CompiledProgram` does
/// not: the declared witness types, which the encoder conformance tests compare against.
pub fn template(cov: Covenant) -> Result<simplicityhl::TemplateProgram, CompileError> {
    simplicityhl::TemplateProgram::new(cov.source(), Box::new(ElementsJetHinter::new()))
        .map_err(|message| CompileError { covenant: cov.name(), message })
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

#[derive(Debug, thiserror::Error)]
pub enum DeployError {
    #[error(transparent)]
    Preflight(#[from] PreflightError),
    #[error(transparent)]
    Compile(#[from] CompileError),
}

/// The pot's two script paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PotLeaf {
    /// `reserve_repay`: grow-only inflow.
    Inflow,
    /// `pot_outflow`: issuer-gated release.
    Outflow,
}

/// A compiled deploy: the five programs plus the taproot trees of the two constant-address
/// covenants. Vault and issuer trees depend on their state and are derived per call.
pub struct Artifacts {
    pub reserve_repay: CompiledProgram,
    pub pot_outflow: CompiledProgram,
    pub stability: CompiledProgram,
    pub vault: CompiledProgram,
    pub issuer: CompiledProgram,
    pot_info: TaprootSpendInfo,
    stability_info: TaprootSpendInfo,
}

impl Artifacts {
    /// Compile the deploy. Runs `Params::preflight` first; pins are wired from the compiled
    /// siblings, in DAG order: the pot references nobody, stability pins POT_SPK, the vault
    /// pins POT_SPK and STABILITY_SPK, the issuer additionally pins VAULT_CMR.
    pub fn compile(params: &Params) -> Result<Self, DeployError> {
        params.preflight()?;
        let obol = asset_u256(params.obol);
        let token = asset_u256(params.issuer_token);
        let policy = asset_u256(params.policy);

        let reserve_repay = compile(Covenant::ReserveRepay, args(vec![w("OBOL_ID", Value::u256(obol))]))?;
        let pot_outflow = compile(
            Covenant::PotOutflow,
            args(vec![w("OBOL_ID", Value::u256(obol)), w("ISSUER_TOKEN_ID", Value::u256(token))]),
        )?;
        let pot_info = two_leaf_tree(
            (cmr_script(&reserve_repay), simplicity::leaf_version()),
            (cmr_script(&pot_outflow), simplicity::leaf_version()),
        );
        let pot_spk = spk_hash(&p2tr_spk(&pot_info));

        let stability = compile(
            Covenant::Stability,
            args(vec![
                w("POT_SPK", Value::u256(pot_spk)),
                w("ISSUER_TOKEN_ID", Value::u256(token)),
                w("OBOL_ID", Value::u256(obol)),
                w("POLICY", Value::u256(policy)),
            ]),
        )?;
        let stability_info = single_leaf_tree(cmr_script(&stability));
        let stability_spk = spk_hash(&p2tr_spk(&stability_info));

        let mut vault_args = vec![
            w("NUMS", Value::u256(U256::from_byte_array(nums_key().serialize()))),
            w("TAPLEAF_TAG", Value::u256(tapleaf_tag())),
            w("LEAF_VC", Value::u16(leaf_vc())),
            w("OBOL_ID", Value::u256(obol)),
            w("POLICY", Value::u256(policy)),
            w("POT_SPK", Value::u256(pot_spk)),
            w("STABILITY_SPK", Value::u256(stability_spk)),
        ];
        push_oracle_args(&mut vault_args, params);
        let vault = compile(Covenant::Vault, args(vault_args))?;

        let mut issuer_args = vec![
            w("NUMS", Value::u256(U256::from_byte_array(nums_key().serialize()))),
            w("TAPLEAF_TAG", Value::u256(tapleaf_tag())),
            w("LEAF_VC", Value::u16(leaf_vc())),
            w("ISSUER_LEAF_VC", Value::u16(issuer_leaf_vc())),
            w("VAULT_CMR", Value::u256(cmr_u256(&vault))),
            w("STABILITY_SPK", Value::u256(stability_spk)),
            w("OBOL_ID", Value::u256(obol)),
            w("POLICY", Value::u256(policy)),
            w("POT_SPK", Value::u256(pot_spk)),
            w("ISSUER_TOKEN_ID", Value::u256(token)),
        ];
        push_oracle_args(&mut issuer_args, params);
        let issuer = compile(Covenant::Issuer, args(issuer_args))?;

        Ok(Artifacts { reserve_repay, pot_outflow, stability, vault, issuer, pot_info, stability_info })
    }

    // --- pot (constant address) ---------------------------------------------

    pub fn pot_spk(&self) -> Script {
        p2tr_spk(&self.pot_info)
    }
    /// sha256 of the pot's scriptPubKey: the flat POT_SPK pin.
    pub fn pot_spk_hash(&self) -> U256 {
        spk_hash(&self.pot_spk())
    }
    pub fn pot_control_block(&self, leaf: PotLeaf) -> ControlBlock {
        let prog = match leaf {
            PotLeaf::Inflow => &self.reserve_repay,
            PotLeaf::Outflow => &self.pot_outflow,
        };
        control_block(&self.pot_info, cmr_script(prog), simplicity::leaf_version())
    }

    // --- stability reserve (constant address) --------------------------------

    pub fn stability_spk(&self) -> Script {
        p2tr_spk(&self.stability_info)
    }
    /// sha256 of the reserve's scriptPubKey: the flat STABILITY_SPK pin.
    pub fn stability_spk_hash(&self) -> U256 {
        spk_hash(&self.stability_spk())
    }
    pub fn stability_control_block(&self) -> ControlBlock {
        control_block(&self.stability_info, cmr_script(&self.stability), simplicity::leaf_version())
    }

    // --- vault (address derives from state) -----------------------------------

    /// The CMR the issuer pins as VAULT_CMR.
    pub fn vault_cmr(&self) -> U256 {
        cmr_u256(&self.vault)
    }
    pub fn vault_spend_info(&self, state: &VaultState) -> TaprootSpendInfo {
        let (leaf, ver) = vault_data_leaf(state);
        two_leaf_tree((cmr_script(&self.vault), simplicity::leaf_version()), (leaf, ver))
    }
    pub fn vault_spk(&self, state: &VaultState) -> Script {
        p2tr_spk(&self.vault_spend_info(state))
    }
    pub fn vault_control_block(&self, state: &VaultState) -> ControlBlock {
        control_block(&self.vault_spend_info(state), cmr_script(&self.vault), simplicity::leaf_version())
    }

    // --- issuer (address derives from the mint anchor) -------------------------

    pub fn issuer_spend_info(&self, state: &IssuerState) -> TaprootSpendInfo {
        let (leaf, ver) = issuer_data_leaf(state.last_mint_height);
        two_leaf_tree((cmr_script(&self.issuer), simplicity::leaf_version()), (leaf, ver))
    }
    pub fn issuer_spk(&self, state: &IssuerState) -> Script {
        p2tr_spk(&self.issuer_spend_info(state))
    }
    pub fn issuer_control_block(&self, state: &IssuerState) -> ControlBlock {
        control_block(&self.issuer_spend_info(state), cmr_script(&self.issuer), simplicity::leaf_version())
    }
}

fn w(name: &str, v: Value) -> (WitnessName, Value) {
    (WitnessName::from_str_unchecked(name), v)
}
fn args(pairs: Vec<(WitnessName, Value)>) -> Arguments {
    Arguments::from(pairs.into_iter().collect::<HashMap<_, _>>())
}
fn push_oracle_args(m: &mut Vec<(WitnessName, Value)>, params: &Params) {
    for (i, pk) in params.oracle_pks.iter().enumerate() {
        m.push(w(&format!("ORACLE_PK_{}", i + 1), Value::u256(xonly_u256(pk))));
    }
}

/// Two leaves at depth 1. The shape is fixed, so building it cannot fail.
#[allow(clippy::unwrap_used)]
fn two_leaf_tree(a: (Script, LeafVersion), b: (Script, LeafVersion)) -> TaprootSpendInfo {
    TaprootBuilder::new()
        .add_leaf_with_ver(1, a.0, a.1)
        .unwrap()
        .add_leaf_with_ver(1, b.0, b.1)
        .unwrap()
        .finalize(crate::secp(), nums_key())
        .unwrap()
}

/// A single leaf at depth 0. The shape is fixed, so building it cannot fail.
#[allow(clippy::unwrap_used)]
fn single_leaf_tree(leaf: Script) -> TaprootSpendInfo {
    TaprootBuilder::new()
        .add_leaf_with_ver(0, leaf, simplicity::leaf_version())
        .unwrap()
        .finalize(crate::secp(), nums_key())
        .unwrap()
}

/// The control block of a leaf that is in the tree by construction.
#[allow(clippy::unwrap_used)]
fn control_block(info: &TaprootSpendInfo, script: Script, ver: LeafVersion) -> ControlBlock {
    info.control_block(&(script, ver)).unwrap()
}

/// The p2tr scriptPubKey of a finalized tree. Network-independent: only the address encoding
/// differs per network, the spk does not.
fn p2tr_spk(info: &TaprootSpendInfo) -> Script {
    Address::p2tr(crate::secp(), info.internal_key(), info.merkle_root(), None, &AddressParams::ELEMENTS)
        .script_pubkey()
}

fn spk_hash(spk: &Script) -> U256 {
    U256::from_byte_array(sha256::Hash::hash(spk.as_bytes()).to_byte_array())
}
