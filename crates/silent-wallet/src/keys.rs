//! The wallet's keys.
//!
//! Both private keys live here and nowhere else: the scan key (finds
//! payments) and the spend key (spends them). They are derived from a seed
//! the caller supplies on every start; nothing in this crate writes a key or
//! a seed to disk, prints one, or includes one in an error.

use bdk_sp::bitcoin::{
    bip32::{DerivationPath, Xpriv},
    hex::FromHex,
    secp256k1::{Secp256k1, SecretKey},
};
use silent_sync::{
    protocol::ProtocolError, Network, ReceiveScanner, ReceiverKeys, SilentPaymentAddress,
};
use std::{fmt, str::FromStr};

/// Why keys could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    #[error("the seed must be 16 to 64 bytes, given as hexadecimal digits")]
    /// The seed is not hexadecimal or has an unusable length. The text of
    /// the seed is never included.
    InvalidSeed,
    #[error("key derivation failed")]
    /// BIP32 derivation failed (practically impossible for a valid seed).
    Derivation,
}

/// The scan and spend private keys of one wallet.
#[derive(Clone)]
pub struct WalletKeys {
    scan: SecretKey,
    spend: SecretKey,
}

impl WalletKeys {
    /// Derives the keys from a BIP32 seed along the BIP352 paths for test
    /// networks: `m/352'/1'/0'/1'/0` (scan) and `m/352'/1'/0'/0'/0` (spend).
    ///
    /// Coin type 1 is used for both networks this crate knows; mainnet's
    /// coin type 0 is never derived.
    pub fn from_seed(seed: &[u8], network: Network) -> Result<Self, KeyError> {
        if !(16..=64).contains(&seed.len()) {
            return Err(KeyError::InvalidSeed);
        }
        let secp = Secp256k1::signing_only();
        let master =
            Xpriv::new_master(network.to_bitcoin(), seed).map_err(|_| KeyError::Derivation)?;
        let derive = |path: &str| -> Result<SecretKey, KeyError> {
            let path = DerivationPath::from_str(path).map_err(|_| KeyError::Derivation)?;
            master
                .derive_priv(&secp, &path)
                .map(|key| key.private_key)
                .map_err(|_| KeyError::Derivation)
        };
        Ok(Self {
            scan: derive("352h/1h/0h/1h/0")?,
            spend: derive("352h/1h/0h/0h/0")?,
        })
    }

    /// Like [`Self::from_seed`], with the seed as hexadecimal digits (as it
    /// arrives from an environment variable). Surrounding whitespace is
    /// ignored.
    pub fn from_seed_hex(seed_hex: &str, network: Network) -> Result<Self, KeyError> {
        let seed = Vec::<u8>::from_hex(seed_hex.trim()).map_err(|_| KeyError::InvalidSeed)?;
        Self::from_seed(&seed, network)
    }

    /// Keys given directly, for callers with their own derivation.
    pub fn from_secret_keys(scan: SecretKey, spend: SecretKey) -> Self {
        Self { scan, spend }
    }

    /// What the receiving side needs: the scan private key and the spend
    /// *public* key. This is what scanning uses; it cannot spend.
    pub fn receiver(&self) -> ReceiverKeys {
        ReceiverKeys::new(self.scan, self.spend.public_key(&Secp256k1::signing_only()))
    }

    /// The spend private key, for the signing module of this crate only.
    pub(crate) fn spend_secret(&self) -> SecretKey {
        self.spend
    }
}

impl fmt::Debug for WalletKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalletKeys")
            .field("scan", &"<redacted>")
            .field("spend", &"<redacted>")
            .finish()
    }
}

/// One wallet on one network: its keys and the scanner made from them.
///
/// Built in one step so that the scanner can never belong to other keys
/// than the ones that sign.
pub struct Wallet {
    keys: WalletKeys,
    network: Network,
    scanner: ReceiveScanner,
}

impl Wallet {
    /// A wallet that scans for the unlabelled address, the change label and
    /// the labels in `labels`.
    pub fn new(keys: WalletKeys, network: Network, labels: &[u32]) -> Result<Self, ProtocolError> {
        let scanner = ReceiveScanner::new(&keys.receiver(), labels)?;
        Ok(Self {
            keys,
            network,
            scanner,
        })
    }

    /// The network this wallet is on.
    pub fn network(&self) -> Network {
        self.network
    }

    /// The scanner for this wallet's keys and labels.
    pub fn scanner(&self) -> &ReceiveScanner {
        &self.scanner
    }

    /// The receiving side of the keys (cannot spend).
    pub fn receiver(&self) -> ReceiverKeys {
        self.keys.receiver()
    }

    /// The address to hand out: unlabelled, or with label `m`. Label 0 is
    /// the change label, which BIP352 reserves for the wallet's own change:
    /// asking for it is an error.
    pub fn address(&self, label: Option<u32>) -> Result<SilentPaymentAddress, ProtocolError> {
        match label {
            None => Ok(self.receiver().address(self.network)),
            Some(0) => Err(ProtocolError::Address(
                "label 0 is the change label; it is not handed out".to_string(),
            )),
            Some(m) => self.receiver().labelled_address(self.network, m),
        }
    }

    pub(crate) fn keys(&self) -> &WalletKeys {
        &self.keys
    }
}

impl fmt::Debug for Wallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wallet")
            .field("keys", &"<redacted>")
            .field("network", &self.network)
            .field("labels", &self.scanner.labels().collect::<Vec<_>>())
            .finish()
    }
}
