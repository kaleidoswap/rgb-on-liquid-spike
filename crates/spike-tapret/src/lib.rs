//! Naked tapret commitment on Liquid.
//!
//! A "naked tapret" is a 32-byte commitment `C` placed in a taproot output
//! via the BIP-341 key tweak, with no script tree. This crate isolates
//! the tweak math and shows it works against Elements transactions
//! independently of the RGB stack.
//!
//! ## Tweak
//!
//! Given:
//! - internal x-only key `P` (32 bytes)
//! - 32-byte commitment `C`
//!
//! Compute:
//!
//! ```text
//! t = tagged_hash("TapTweak", P || C)
//! Q = P + t·G                 (output x-only key)
//! ```
//!
//! `C` is treated as if it were a Merkle root (RGB tapret convention:
//! single-leaf or empty-tree, in both cases the value placed in the tweak
//! is a 32-byte digest). This matches what `bp-core` does on Bitcoin.

pub mod address;
pub mod tweak;

pub use tweak::{prove, verify, ProofError, TweakedKey};
