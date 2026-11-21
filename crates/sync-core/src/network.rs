//! Network identity. Mainnet is not representable in this crate's types.

use crate::bitcoin::{blockdata::constants::genesis_block, BlockHash, Network};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The only networks the lab may operate on.
///
/// There is deliberately no mainnet variant: code that holds a `LabNetwork`
/// cannot be pointed at mainnet, and every parser rejects mainnet names with a
/// dedicated error (design spec §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum LabNetwork {
    Regtest,
    Signet,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkError {
    #[error(
        "mainnet is refused: this is experimental, unaudited software and must not be used \
         with mainnet or real funds"
    )]
    MainnetRefused,
    #[error("unsupported network `{0}`: only `regtest` and `signet` are supported")]
    Unsupported(String),
    #[error("genesis hash mismatch for {network}: expected {expected}, got {actual}")]
    GenesisMismatch {
        network: LabNetwork,
        expected: BlockHash,
        actual: BlockHash,
    },
}

impl LabNetwork {
    pub const fn as_str(self) -> &'static str {
        match self {
            LabNetwork::Regtest => "regtest",
            LabNetwork::Signet => "signet",
        }
    }

    /// The corresponding `bitcoin::Network`. Never returns `Network::Bitcoin`.
    pub const fn to_bitcoin(self) -> Network {
        match self {
            LabNetwork::Regtest => Network::Regtest,
            LabNetwork::Signet => Network::Signet,
        }
    }

    /// Fallible conversion from `bitcoin::Network`; mainnet is refused.
    pub fn try_from_bitcoin(network: Network) -> Result<Self, NetworkError> {
        match network {
            Network::Regtest => Ok(LabNetwork::Regtest),
            Network::Signet => Ok(LabNetwork::Signet),
            Network::Bitcoin => Err(NetworkError::MainnetRefused),
            other => Err(NetworkError::Unsupported(other.to_string())),
        }
    }

    /// Hash of the well-known genesis block of this network.
    ///
    /// Note: custom signets share the default signet genesis block, so the
    /// genesis hash alone does not distinguish them.
    pub fn genesis_hash(self) -> BlockHash {
        genesis_block(self.to_bitcoin()).block_hash()
    }
}

impl fmt::Display for LabNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LabNetwork {
    type Err = NetworkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "regtest" => Ok(LabNetwork::Regtest),
            "signet" => Ok(LabNetwork::Signet),
            "mainnet" | "main" | "bitcoin" | "livenet" | "prod" | "production" => {
                Err(NetworkError::MainnetRefused)
            }
            other => Err(NetworkError::Unsupported(other.to_string())),
        }
    }
}

impl TryFrom<String> for LabNetwork {
    type Error = NetworkError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<LabNetwork> for String {
    fn from(value: LabNetwork) -> Self {
        value.as_str().to_string()
    }
}

/// Network plus genesis hash. Two sources are only comparable when their
/// `ChainIdentity` is equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChainIdentity {
    pub network: LabNetwork,
    pub genesis_hash: BlockHash,
}

impl ChainIdentity {
    /// The canonical identity of `network`.
    pub fn for_network(network: LabNetwork) -> Self {
        Self {
            network,
            genesis_hash: network.genesis_hash(),
        }
    }

    /// Builds an identity from externally supplied data, verifying that the
    /// genesis hash is the one expected for the claimed network. This also
    /// rejects a mainnet chain mislabelled as a test network.
    pub fn verified(network: LabNetwork, genesis_hash: BlockHash) -> Result<Self, NetworkError> {
        let expected = network.genesis_hash();
        if genesis_hash != expected {
            return Err(NetworkError::GenesisMismatch {
                network,
                expected,
                actual: genesis_hash,
            });
        }
        Ok(Self {
            network,
            genesis_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_names_are_refused() {
        for name in [
            "mainnet",
            "main",
            "bitcoin",
            "Mainnet",
            " BITCOIN ",
            "livenet",
        ] {
            assert_eq!(
                name.parse::<LabNetwork>(),
                Err(NetworkError::MainnetRefused),
                "{name}"
            );
        }
    }

    #[test]
    fn mainnet_is_refused_by_the_deserializer() {
        let err = serde_json::from_str::<LabNetwork>("\"mainnet\"")
            .expect_err("mainnet must not deserialize");
        assert!(err.to_string().contains("mainnet is refused"), "{err}");
        assert!(serde_json::from_str::<LabNetwork>("\"bitcoin\"").is_err());
    }

    #[test]
    fn mainnet_bitcoin_network_is_refused() {
        assert_eq!(
            LabNetwork::try_from_bitcoin(Network::Bitcoin),
            Err(NetworkError::MainnetRefused)
        );
    }

    #[test]
    fn other_networks_are_unsupported() {
        for name in ["testnet", "testnet4", "", "regtest2"] {
            assert!(matches!(
                name.parse::<LabNetwork>(),
                Err(NetworkError::Unsupported(_))
            ));
        }
        assert!(matches!(
            LabNetwork::try_from_bitcoin(Network::Testnet),
            Err(NetworkError::Unsupported(_))
        ));
    }

    #[test]
    fn supported_networks_round_trip() {
        for network in [LabNetwork::Regtest, LabNetwork::Signet] {
            assert_eq!(network.as_str().parse::<LabNetwork>(), Ok(network));
            assert_ne!(network.to_bitcoin(), Network::Bitcoin);
            let json = serde_json::to_string(&network).expect("serialize");
            assert_eq!(
                serde_json::from_str::<LabNetwork>(&json).ok(),
                Some(network)
            );
        }
    }

    #[test]
    fn mainnet_genesis_is_rejected_even_when_labelled_regtest() {
        let mainnet_genesis = genesis_block(Network::Bitcoin).block_hash();
        for network in [LabNetwork::Regtest, LabNetwork::Signet] {
            assert!(matches!(
                ChainIdentity::verified(network, mainnet_genesis),
                Err(NetworkError::GenesisMismatch { .. })
            ));
            assert_ne!(network.genesis_hash(), mainnet_genesis);
        }
        assert!(
            ChainIdentity::verified(LabNetwork::Regtest, LabNetwork::Regtest.genesis_hash())
                .is_ok()
        );
    }
}
