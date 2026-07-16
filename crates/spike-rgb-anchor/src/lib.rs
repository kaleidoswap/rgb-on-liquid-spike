//! RGB anchoring on Liquid, built on the rgb-protocol 0.11 stack
//! (`rgb-consensus`, lib name `rgbcore`).
//!
//! `rgbcore`'s `Layer1` enum already has a `Liquid` variant and the
//! `ChainNet` enum has `LiquidMainnet` / `LiquidTestnet`, so the
//! consensus layer can already describe a Liquid contract. The only
//! piece missing upstream is witness-transaction verification, which
//! is typed against `bitcoin::Transaction`. This crate exercises an
//! end-to-end RGB anchor on Liquid against a patched `rgbcore` (see
//! `vendor/rgb-consensus-patched/` and `docs/RFC_RGB_ON_LIQUID.md`).

pub mod anchor;
pub mod bfa;
pub mod bundle;
pub mod liquid_dbc;
pub mod mint;
pub mod mpc;
pub mod patched_anchor;
pub mod rgb20;
pub mod rgb_real;
pub mod seal;
pub mod swap;
