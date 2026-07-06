//! The snapshot file: the indexed state as JSON, written atomically (tmp + rename) so a
//! crash mid-write leaves the previous snapshot intact. A daemon loads it at start and
//! catch-up scans from `height + 1`; no snapshot means a scan from genesis.

use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use styx_core::domain::{IssuerState, OnChain, PotState, ReserveState};
use styx_core::elements::hex::{FromHex, ToHex};
use styx_core::elements::secp256k1_zkp::XOnlyPublicKey;
use styx_core::elements::{BlockHash, OutPoint, Script, Txid};
use styx_core::units::{BlockHeight, Obol, Sats};

use crate::index::{IndexState, Lost, TrackedVault};

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(String),
    #[error("field {field}: {value}")]
    Field { field: &'static str, value: String },
    #[error("snapshot version {0} is not supported")]
    Version(u32),
}

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    height: u32,
    tip: String,
    pot: Option<SingletonDoc>,
    reserve: Option<SingletonDoc>,
    issuer: Option<IssuerDoc>,
    vaults: Vec<VaultDoc>,
    lost: Vec<LostDoc>,
}

#[derive(Serialize, Deserialize)]
struct OutPointDoc {
    txid: String,
    vout: u32,
}

#[derive(Serialize, Deserialize)]
struct SingletonDoc {
    outpoint: OutPointDoc,
    value: u64,
}

#[derive(Serialize, Deserialize)]
struct IssuerDoc {
    outpoint: OutPointDoc,
    anchor: u32,
}

#[derive(Serialize, Deserialize)]
struct VaultDoc {
    outpoint: OutPointDoc,
    debt: u64,
    last_height: u32,
    value: u64,
    owner: Option<String>,
    spk: String,
}

#[derive(Serialize, Deserialize)]
struct LostDoc {
    vault: OutPointDoc,
    txid: String,
    reason: String,
}

fn op_doc(op: OutPoint) -> OutPointDoc {
    OutPointDoc { txid: op.txid.to_string(), vout: op.vout }
}

fn op_parse(doc: &OutPointDoc) -> Result<OutPoint, SnapshotError> {
    let txid = Txid::from_str(&doc.txid)
        .map_err(|_| SnapshotError::Field { field: "txid", value: doc.txid.clone() })?;
    Ok(OutPoint::new(txid, doc.vout))
}

fn doc(state: &IndexState) -> Doc {
    Doc {
        version: VERSION,
        height: state.height,
        tip: state.tip.to_string(),
        pot: state.pot.map(|p| SingletonDoc { outpoint: op_doc(p.outpoint), value: p.value.raw() }),
        reserve: state
            .reserve
            .map(|r| SingletonDoc { outpoint: op_doc(r.outpoint), value: r.value.raw() }),
        issuer: state
            .issuer
            .map(|i| IssuerDoc { outpoint: op_doc(i.outpoint), anchor: i.state.last_mint_height.raw() }),
        vaults: state
            .vaults
            .iter()
            .map(|(op, v)| VaultDoc {
                outpoint: op_doc(*op),
                debt: v.debt.raw(),
                last_height: v.last_height.raw(),
                value: v.value.raw(),
                owner: v.owner.map(|pk| pk.to_string()),
                spk: v.spk.as_bytes().to_hex(),
            })
            .collect(),
        lost: state
            .lost
            .iter()
            .map(|(op, l)| LostDoc {
                vault: op_doc(*op),
                txid: l.txid.to_string(),
                reason: l.reason.clone(),
            })
            .collect(),
    }
}

fn state(doc: &Doc) -> Result<IndexState, SnapshotError> {
    if doc.version != VERSION {
        return Err(SnapshotError::Version(doc.version));
    }
    let tip = BlockHash::from_str(&doc.tip)
        .map_err(|_| SnapshotError::Field { field: "tip", value: doc.tip.clone() })?;
    let mut st = IndexState::genesis(tip);
    st.height = doc.height;
    st.pot = match &doc.pot {
        Some(p) => Some(OnChain {
            state: PotState,
            outpoint: op_parse(&p.outpoint)?,
            value: Obol::new(p.value),
        }),
        None => None,
    };
    st.reserve = match &doc.reserve {
        Some(r) => Some(OnChain {
            state: ReserveState,
            outpoint: op_parse(&r.outpoint)?,
            value: Sats::new(r.value),
        }),
        None => None,
    };
    st.issuer = match &doc.issuer {
        Some(i) => Some(OnChain {
            state: IssuerState { last_mint_height: BlockHeight::new(i.anchor) },
            outpoint: op_parse(&i.outpoint)?,
            value: 1,
        }),
        None => None,
    };
    for v in &doc.vaults {
        let owner = v
            .owner
            .as_ref()
            .map(|s| {
                XOnlyPublicKey::from_str(s)
                    .map_err(|_| SnapshotError::Field { field: "owner", value: s.clone() })
            })
            .transpose()?;
        let spk = Vec::<u8>::from_hex(&v.spk)
            .map_err(|_| SnapshotError::Field { field: "spk", value: v.spk.clone() })?;
        st.vaults.insert(
            op_parse(&v.outpoint)?,
            TrackedVault {
                debt: Obol::new(v.debt),
                last_height: BlockHeight::new(v.last_height),
                value: Sats::new(v.value),
                owner,
                spk: Script::from(spk),
            },
        );
    }
    for l in &doc.lost {
        let txid = Txid::from_str(&l.txid)
            .map_err(|_| SnapshotError::Field { field: "lost txid", value: l.txid.clone() })?;
        st.lost.insert(op_parse(&l.vault)?, Lost { txid, reason: l.reason.clone() });
    }
    Ok(st)
}

/// Write the snapshot atomically: serialize next to the target and rename over it. No fsync
/// before the rename: a power cut can lose the newest snapshot but never corrupts one, and
/// the state is always reconstructible from genesis.
pub fn save(state_: &IndexState, path: &Path) -> Result<(), SnapshotError> {
    let json =
        serde_json::to_string_pretty(&doc(state_)).map_err(|e| SnapshotError::Json(e.to_string()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<IndexState, SnapshotError> {
    let text = std::fs::read_to_string(path)?;
    let doc: Doc = serde_json::from_str(&text).map_err(|e| SnapshotError::Json(e.to_string()))?;
    state(&doc)
}
