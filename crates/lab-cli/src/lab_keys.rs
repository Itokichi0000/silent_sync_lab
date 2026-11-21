//! The lab's fixed, TEST-ONLY receiver.
//!
//! No secret is introduced by this repository: the key material is the
//! receiver of the first case in the public BIP352 test-vector file
//! (`fixtures/bip352/send_and_receive_test_vectors.json`), which anyone can
//! read in bitcoin/bips. Never send anything of value to an address derived
//! from it.
//!
//! Only two values are read: the scan private key (needed to scan) and the
//! spend *public* key. The spend private key, although it is printed in the
//! same public file, is never parsed. The scan private key stays inside
//! [`ReceiverKeys`], whose `Debug` output is redacted; this module never
//! formats or returns it in any other form.
//!
//! Note: the vector file is embedded in the binary as a whole, so the binary
//! contains those public test keys as text.

use sync_core::{
    bitcoin::{
        key::Secp256k1,
        secp256k1::{PublicKey, SecretKey},
    },
    protocol::ReceiverKeys,
};

const VECTORS_JSON: &str =
    include_str!("../../../fixtures/bip352/send_and_receive_test_vectors.json");

pub const DESCRIPTION: &str = "BIP352 test vector #0 receiver (public test key, test-only)";

pub fn lab_receiver_keys() -> Result<ReceiverKeys, String> {
    let vectors: serde_json::Value =
        serde_json::from_str(VECTORS_JSON).map_err(|e| format!("embedded BIP352 vectors: {e}"))?;
    let field = |value: &serde_json::Value, name: &str| -> Result<String, String> {
        value[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("embedded BIP352 vectors: missing {name}"))
    };
    // Parse errors are not forwarded: they must never echo key bytes.
    let invalid = |name: &str| format!("embedded BIP352 vectors: invalid {name}");

    let receiving = &vectors[0]["receiving"][0]["given"]["key_material"];
    let scan: SecretKey = field(receiving, "scan_priv_key")?
        .parse()
        .map_err(|_| invalid("scan_priv_key"))?;

    // The same receiver as seen by the sender: public keys only.
    let recipient = &vectors[0]["sending"][0]["given"]["recipients"][0];
    let scan_public: PublicKey = field(recipient, "scan_pub_key")?
        .parse()
        .map_err(|_| invalid("scan_pub_key"))?;
    let spend_public: PublicKey = field(recipient, "spend_pub_key")?
        .parse()
        .map_err(|_| invalid("spend_pub_key"))?;

    if scan.public_key(&Secp256k1::new()) != scan_public {
        return Err(
            "embedded BIP352 vectors: sending and receiving halves of vector #0 disagree"
                .to_string(),
        );
    }
    Ok(ReceiverKeys::new(scan, spend_public))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sync_core::network::LabNetwork;

    /// The address derived from the two values read above must be the one the
    /// vector file itself states (modulo the network prefix, which the lab
    /// cannot produce for mainnet).
    #[test]
    fn lab_receiver_is_the_vector_zero_receiver() {
        let keys = lab_receiver_keys().expect("embedded vectors are valid");
        let vectors: serde_json::Value = serde_json::from_str(VECTORS_JSON).expect("json");
        let expected = vectors[0]["receiving"][0]["expected"]["addresses"][0]
            .as_str()
            .expect("address");
        assert!(expected.starts_with("sp1"));
        // bech32m: identical data part up to the 6-character checksum.
        let payload = |address: &str, hrp: &str| {
            let data = address.strip_prefix(hrp).expect("prefix").to_string();
            data[..data.len() - 6].to_string()
        };
        let ours = keys.address(LabNetwork::Signet).to_string();
        assert_eq!(payload(&ours, "tsp1"), payload(expected, "sp1"));
    }
}
