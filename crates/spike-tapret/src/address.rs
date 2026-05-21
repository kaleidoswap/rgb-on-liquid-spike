//! Encode an x-only output key as a Liquid taproot address.
//!
//! Elements uses the same bech32m P2TR encoding as Bitcoin, just with a
//! different HRP per network:
//!   - mainnet (liquidv1): "ex"
//!   - regtest (elementsregtest): "ert"
//!   - testnet: "tex"
//!
//! Witness version 1, 32-byte program (the x-only output key).

use anyhow::{anyhow, Result};
use bech32::{hrp::Hrp, segwit};

/// Liquid regtest HRP for native segwit / taproot addresses.
pub const HRP_REGTEST: &str = "ert";
pub const HRP_LIQUIDV1: &str = "ex";
pub const HRP_TESTNET: &str = "tex";

/// Encode an x-only output key (32 bytes) as a P2TR address on the chosen
/// network.
pub fn encode_p2tr(network_hrp: &str, output_key: &[u8; 32]) -> Result<String> {
    let hrp = Hrp::parse(network_hrp).map_err(|e| anyhow!("bad hrp '{network_hrp}': {e}"))?;
    // Witness version 1, 32-byte program.
    let fp = bech32::Fe32::try_from(1u8).expect("1 is a valid fe32");
    segwit::encode(hrp, fp, output_key.as_ref()).map_err(|e| anyhow!("bech32m encode failed: {e}"))
}

/// Decode a P2TR address back into the x-only output key.
pub fn decode_p2tr(addr: &str) -> Result<(String, [u8; 32])> {
    let (hrp, witver, prog) =
        segwit::decode(addr).map_err(|e| anyhow!("bech32m decode of '{addr}': {e}"))?;
    if witver.to_u8() != 1 {
        anyhow::bail!("not a witness v1 address (got v{})", witver.to_u8());
    }
    if prog.len() != 32 {
        anyhow::bail!("witness program is {} bytes, expected 32", prog.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&prog);
    Ok((hrp.to_string(), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_regtest() {
        let key = [0x42u8; 32];
        let addr = encode_p2tr(HRP_REGTEST, &key).unwrap();
        assert!(addr.starts_with("ert1p"), "got {addr}");
        let (hrp, decoded) = decode_p2tr(&addr).unwrap();
        assert_eq!(hrp, "ert");
        assert_eq!(decoded, key);
    }
}
