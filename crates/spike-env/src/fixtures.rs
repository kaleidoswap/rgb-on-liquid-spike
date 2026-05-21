//! Load the JSON sidecars written by `scripts/bootstrap.sh`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct AssetFixture {
    pub asset_id: String,
    pub entropy: String,
    pub issued: u64,
    pub units_to_a: u64,
    pub lbtc_asset: String,
}

#[derive(Debug, Deserialize)]
pub struct AddressPair {
    pub confidential: String,
    pub unconfidential: String,
}

#[derive(Debug, Deserialize)]
pub struct AddressFixture {
    pub a: AddressPair,
    pub b: AddressPair,
    pub issuer: AddressPair,
}

pub fn locate_out_dir() -> Result<PathBuf> {
    let mut here = std::env::current_dir()?;
    for _ in 0..6 {
        let candidate = here.join("out");
        if candidate.join("asset.json").is_file() {
            return Ok(candidate);
        }
        if !here.pop() {
            break;
        }
    }
    anyhow::bail!("could not find out/asset.json — run scripts/bootstrap.sh first");
}

pub fn load_asset(out_dir: &Path) -> Result<AssetFixture> {
    let path = out_dir.join("asset.json");
    let s = std::fs::read_to_string(&path).with_context(|| format!("reading {path:?}"))?;
    Ok(serde_json::from_str(&s)?)
}

pub fn load_addresses(out_dir: &Path) -> Result<AddressFixture> {
    let path = out_dir.join("addresses.json");
    let s = std::fs::read_to_string(&path).with_context(|| format!("reading {path:?}"))?;
    Ok(serde_json::from_str(&s)?)
}
