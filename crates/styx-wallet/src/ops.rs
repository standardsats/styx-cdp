//! The owner operations, one function per CLI command: sync, select coins, build, sign
//! (owner and/or funding), broadcast. Ticks are injected - the binary assembles them from
//! the relays, the tests sign their own - so this layer stays runtime-free.

use styx_core::consts::{K_FEE_HALF_PERCENT, K_OPEN_MIN};
use styx_core::domain::{OnChain, VaultState};
use styx_core::elements::{OutPoint, Txid};
use styx_core::math::coll_at_cr;
use styx_core::oracle::OracleTick;
use styx_core::units::{Obol, Sats};
use styx_pset::build;
use styx_pset::intent::{
    CloseIntent, DrawIntent, FundingCoin, ObolCoin, OpenIntent, RedeemIntent, RefreshIntent, RepayIntent,
};
use styx_pset::sign::owner_sign;

use crate::wallet::{Wallet, WalletError, FEE};

/// What a broadcast op reports back: the txid and the successor vault, when one exists.
#[derive(Debug)]
pub struct OpReport {
    pub txid: Txid,
    pub vault: Option<OnChain<VaultState>>,
}

impl Wallet {
    fn funding_coin(&self, outpoint: OutPoint, value: u64) -> FundingCoin {
        FundingCoin { outpoint, value: Sats::new(value), spk: self.funding_spk() }
    }

    fn obol_coin(&self, outpoint: OutPoint, value: u64) -> ObolCoin {
        ObolCoin { outpoint, value: Obol::new(value), spk: self.funding_spk() }
    }

    /// The smallest sufficient L-BTC coin, or a typed shortfall.
    pub fn pick_lbtc(&self, need: u64) -> Result<(OutPoint, u64), WalletError> {
        let coins = self.lbtc_coins()?;
        Wallet::select_at_least(&coins, need)
            .ok_or(WalletError::InsufficientLbtc { need, have: coins.iter().map(|(_, v)| *v).sum() })
    }

    /// One OBOL coin covering `need`: the builder layouts carry a single payer input, so a
    /// wallet whose change has fragmented consolidates first (a self-spend merge chained in
    /// the mempool) rather than failing while solvent.
    pub fn ensure_obol(&self, need: u64) -> Result<(OutPoint, u64), WalletError> {
        let coins = self.obol_coins()?;
        if let Some(c) = Wallet::select_at_least(&coins, need) {
            return Ok(c);
        }
        let have: u64 = coins.iter().map(|(_, v)| *v).sum();
        if have < need {
            return Err(WalletError::InsufficientObol { need, have });
        }
        self.consolidate_obol()
    }

    /// Move `sats` from the NODE's wallet to the funding spk: the on-ramp.
    pub fn fund(&self, sats: u64) -> Result<OutPoint, WalletError> {
        Ok(self.node.fund_address(self.ctx.params.policy, &self.funding_spk(), sats)?)
    }

    /// OPEN a vault. The frozen layout needs exact funding (collateral + 0.5% borrow fee at
    /// the min quote + tx fee); if no coin matches exactly, a shaping self-spend makes one
    /// first and the open chains on it in the mempool.
    pub fn open(
        &mut self,
        principal: Obol,
        collateral: Sats,
        tick: &OracleTick,
    ) -> Result<OpReport, WalletError> {
        let protocol = self.protocol()?;
        let (lo, _) = tick.price_range();
        let debt_cents = principal.covenant_cents()?;
        let need =
            collateral.checked_add(coll_at_cr(debt_cents, lo, K_FEE_HALF_PERCENT))?.checked_add(FEE)?;
        let coins = self.lbtc_coins()?;
        let funding = match coins.iter().find(|(_, v)| *v == need.raw()) {
            Some((op, v)) => self.funding_coin(*op, *v),
            None => {
                let (exact, _) = self.shape_exact(need)?;
                self.funding_coin(exact, need.raw())
            }
        };
        let built = build::open::open(
            &self.ctx,
            &protocol,
            &OpenIntent {
                owner: self.owner_pk(),
                principal,
                collateral,
                borrower_spk: self.funding_spk(),
                funding,
                tick: tick.clone(),
                fee: FEE,
            },
        )?;
        let txid = self.sign_and_broadcast(&built.plan)?;
        Ok(OpReport { txid, vault: Some(built.expected.vault) })
    }

    /// The suggested collateral for a principal at a target CR (default: the 150% open
    /// minimum), so the CLI can size an open without the user doing covenant math.
    pub fn collateral_for(
        principal: Obol,
        tick: &OracleTick,
        cr_percent: Option<u32>,
    ) -> Result<Sats, WalletError> {
        let (lo, _) = tick.price_range();
        let k = match cr_percent {
            Some(p) => styx_core::units::RatioK::from_cr_percent(p),
            None => K_OPEN_MIN,
        };
        Ok(coll_at_cr(principal.covenant_cents()?, lo, k))
    }

    pub fn repay(&mut self, which: Option<OutPoint>, amount: Obol) -> Result<OpReport, WalletError> {
        let protocol = self.protocol()?;
        let vault = self.pick_vault(which)?;
        let (payer_op, payer_val) = self.ensure_obol(amount.raw())?;
        let fee_coins: Vec<_> =
            self.lbtc_coins()?.into_iter().filter(|(op, _)| *op != payer_op).collect();
        let (fee_op, fee_val) =
            Wallet::select_at_least(&fee_coins, FEE.raw() + 1).ok_or(WalletError::InsufficientLbtc {
                need: FEE.raw() + 1,
                have: fee_coins.iter().map(|(_, v)| *v).sum(),
            })?;
        let mut built = build::repay::repay(
            &self.ctx,
            &protocol.pot,
            &vault,
            &RepayIntent {
                amount,
                payer: self.obol_coin(payer_op, payer_val),
                payer_change_spk: self.funding_spk(),
                fee_coin: self.funding_coin(fee_op, fee_val),
                change_spk: self.funding_spk(),
                fee: FEE,
            },
        )?;
        owner_sign(&self.ctx, &mut built.plan, &self.owner)?;
        let txid = self.sign_and_broadcast(&built.plan)?;
        Ok(OpReport { txid, vault: Some(built.expected.vault) })
    }

    pub fn draw(
        &mut self,
        which: Option<OutPoint>,
        amount: Obol,
        tick: &OracleTick,
    ) -> Result<OpReport, WalletError> {
        let protocol = self.protocol()?;
        let vault = self.pick_vault(which)?;
        let mut built = build::draw::draw(
            &self.ctx,
            &protocol,
            &vault,
            &DrawIntent { amount, borrower_spk: self.funding_spk(), tick: tick.clone(), fee: FEE },
        )?;
        owner_sign(&self.ctx, &mut built.plan, &self.owner)?;
        let txid = self.sign_and_broadcast(&built.plan)?;
        Ok(OpReport { txid, vault: Some(built.expected.vault) })
    }

    pub fn refresh(
        &mut self,
        which: Option<OutPoint>,
        tick: &OracleTick,
    ) -> Result<OpReport, WalletError> {
        let vault = self.pick_vault(which)?;
        let (fee_op, fee_val) = self.pick_lbtc(FEE.raw() + 1)?;
        let built = build::refresh::refresh(
            &self.ctx,
            &vault,
            &RefreshIntent {
                tick: tick.clone(),
                fee_coin: self.funding_coin(fee_op, fee_val),
                change_spk: self.funding_spk(),
                fee: FEE,
            },
        )?;
        let txid = self.sign_and_broadcast(&built.plan)?;
        Ok(OpReport { txid, vault: Some(built.expected.vault) })
    }

    /// CLOSE: repay the full debt, free the collateral to the funding spk.
    pub fn close(&mut self, which: Option<OutPoint>) -> Result<OpReport, WalletError> {
        let protocol = self.protocol()?;
        let vault = self.pick_vault(which)?;
        let (payer_op, payer_val) = self.ensure_obol(vault.state.debt.raw())?;
        let mut built = build::close::close(
            &self.ctx,
            &protocol.pot,
            &vault,
            &CloseIntent {
                payer: self.obol_coin(payer_op, payer_val),
                recipient_spk: self.funding_spk(),
                payer_change_spk: self.funding_spk(),
                fee: FEE,
            },
        )?;
        owner_sign(&self.ctx, &mut built.plan, &self.owner)?;
        let txid = self.sign_and_broadcast(&built.plan)?;
        Ok(OpReport { txid, vault: None })
    }

    /// REDEEM `x` against one of our vaults (permissionless op, but building it needs the
    /// full vault state, so a foreign vault would need its owner bytes - out of this
    /// wallet's reach by design).
    pub fn redeem(
        &mut self,
        which: Option<OutPoint>,
        x: Obol,
        tick: &OracleTick,
    ) -> Result<OpReport, WalletError> {
        let protocol = self.protocol()?;
        let vault = self.pick_vault(which)?;
        let (payer_op, payer_val) = self.ensure_obol(x.raw())?;
        let built = build::redeem::redeem(
            &self.ctx,
            &protocol,
            &vault,
            &RedeemIntent {
                x,
                redeemer: self.obol_coin(payer_op, payer_val),
                redeemer_spk: self.funding_spk(),
                obol_change_spk: self.funding_spk(),
                tick: tick.clone(),
                fee: FEE,
            },
        )?;
        let txid = self.sign_and_broadcast(&built.plan)?;
        Ok(OpReport { txid, vault: Some(built.expected.vault) })
    }
}
