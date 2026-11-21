//! The lab's fixed, TEST-ONLY receiver.
//!
//! No secret is introduced by this repository: the key material is the
//! receiver of the first case in the public BIP352 test-vector file
//! (`fixtures/bip352/send_and_receive_test_vectors.json`), which anyone can
//! read in bitcoin/bips. Never send anything of value to an address derived
//! from it.
//!
//! The scan private key stays inside [`ReceiverKeys`], whose `Debug` output is
//! redacted; this module never formats or returns it in any other form.

use sync_core::{
    bitcoin::{key::Secp256k1, secp256k1::SecretKey},
    protocol::ReceiverKeys,
};

const VECTORS_JSON: &str =
    include_str!("../../../fixtures/bip352/send_and_receive_test_vectors.json");

pub const DESCRIPTION: &str = "BIP352 test vector #0 receiver (public test key, test-only)";

pub fn lab_receiver_keys() -> Result<ReceiverKeys, String> {
    let vectors: serde_json::Value =
        serde_json::from_str(VECTORS_JSON).map_err(|e| format!("embedded BIP352 vectors: {e}"))?;
    let material = &vectors[0]["receiving"][0]["given"]["key_material"];
    let parse = |field: &str| -> Result<SecretKey, String> {
        material[field]
            .as_str()
            .ok_or_else(|| format!("embedded BIP352 vectors: missing {field}"))?
            .parse()
            // The parse error is not forwarded: it must never echo key bytes.
            .map_err(|_| format!("embedded BIP352 vectors: invalid {field}"))
    };
    let scan = parse("scan_priv_key")?;
    let spend = parse("spend_priv_key")?;
    // Only the spend *public* key is kept; the private half is dropped here.
    Ok(ReceiverKeys::new(scan, spend.public_key(&Secp256k1::new())))
}
