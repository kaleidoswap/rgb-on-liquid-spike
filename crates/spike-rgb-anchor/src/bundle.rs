//! `BundleId` helpers.
//!
//! In an RGB consignment a `BundleId` is the `Message` value committed
//! for a contract in the MPC tree. A real `BundleId` is the commitment
//! of a fully-built `TransitionBundle`; `rgb20::build_transfer` produces
//! those. This module provides a deterministic placeholder `BundleId`
//! for tests and demos that do not need a full bundle.

use anyhow::Result;
use rgbcore::commit_verify::{Digest, DigestExt, Sha256};
use rgbcore::BundleId;

/// Derive a deterministic `BundleId` from a textual tag.
pub fn synthetic_bundle_id(tag: &str) -> Result<BundleId> {
    let mut hasher = Sha256::default();
    hasher.update(b"bundle-");
    hasher.update(tag.as_bytes());
    let h: [u8; 32] = DigestExt::<32>::finish(hasher);
    Ok(BundleId::from(h))
}
