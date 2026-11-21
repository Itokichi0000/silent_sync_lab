//! Network identity. Mainnet is not representable in this crate's types.

use crate::bitcoin::{blockdata::constants::genesis_block, BlockHash, Network as BitcoinNetwork};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The only networks the lab may operate on.
///
/// There is deliberately no mainnet variant: code that holds a `Network`
/// cannot be pointed at mainnet, and every parser rejects mainnet names with a
/// dedicated error (design spec §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Network {
    Regtest,
    Signet,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
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
        network: Network,
        expected: BlockHash,
        actual: BlockHash,
    },
}

impl Network {
    pub const fn as_str(self) -> &'static str {
        match self {
            Network::Regtest => "regtest",
            Network::Signet => "signet",
        }
    }

    /// The corresponding `bitcoin::Network`. Never returns `bitcoin::Network::Bitcoin`.
    pub const fn to_bitcoin(self) -> BitcoinNetwork {
        match self {
            Network::Regtest => BitcoinNetwork::Regtest,
            Network::Signet => BitcoinNetwork::Signet,
        }
    }

    /// Fallible conversion from `bitcoin::Network`; mainnet is refused.
    pub fn try_from_bitcoin(network: BitcoinNetwork) -> Result<Self, NetworkError> {
        match network {
            BitcoinNetwork::Regtest => Ok(Network::Regtest),
            BitcoinNetwork::Signet => Ok(Network::Signet),
            BitcoinNetwork::Bitcoin => Err(NetworkError::MainnetRefused),
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

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Network {
    type Err = NetworkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "regtest" => Ok(Network::Regtest),
            "signet" => Ok(Network::Signet),
            "mainnet" | "main" | "bitcoin" | "livenet" | "prod" | "production" => {
                Err(NetworkError::MainnetRefused)
            }
            other => Err(NetworkError::Unsupported(other.to_string())),
        }
    }
}

impl TryFrom<String> for Network {
    type Error = NetworkError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Network> for String {
    fn from(value: Network) -> Self {
        value.as_str().to_string()
    }
}

/// Network plus genesis hash. Two sources are only comparable when their
/// `ChainIdentity` is equal.
///
/// Invariant: `genesis_hash` is the genesis block of `network`. The fields are
/// private and deserialization goes through [`ChainIdentity::verified`], so a
/// value that pairs a lab network name with another chain's genesis (mainnet's
/// in particular) cannot exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawChainIdentity", into = "RawChainIdentity")]
pub struct ChainIdentity {
    network: Network,
    genesis_hash: BlockHash,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChainIdentity {
    network: Network,
    genesis_hash: BlockHash,
}

impl TryFrom<RawChainIdentity> for ChainIdentity {
    type Error = NetworkError;

    fn try_from(raw: RawChainIdentity) -> Result<Self, Self::Error> {
        Self::verified(raw.network, raw.genesis_hash)
    }
}

impl From<ChainIdentity> for RawChainIdentity {
    fn from(chain: ChainIdentity) -> Self {
        Self {
            network: chain.network,
            genesis_hash: chain.genesis_hash,
        }
    }
}

impl ChainIdentity {
    pub fn network(&self) -> Network {
        self.network
    }

    pub fn genesis_hash(&self) -> BlockHash {
        self.genesis_hash
    }

    /// The canonical identity of `network`.
    pub fn for_network(network: Network) -> Self {
        Self {
            network,
            genesis_hash: network.genesis_hash(),
        }
    }

    /// Builds an identity from externally supplied data, verifying that the
    /// genesis hash is the one expected for the claimed network. This also
    /// rejects a mainnet chain mislabelled as a test network.
    pub fn verified(network: Network, genesis_hash: BlockHash) -> Result<Self, NetworkError> {
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
                name.parse::<Network>(),
                Err(NetworkError::MainnetRefused),
                "{name}"
            );
        }
    }

    #[test]
    fn mainnet_is_refused_by_the_deserializer() {
        let err = serde_json::from_str::<Network>("\"mainnet\"")
            .expect_err("mainnet must not deserialize");
        assert!(err.to_string().contains("mainnet is refused"), "{err}");
        assert!(serde_json::from_str::<Network>("\"bitcoin\"").is_err());
    }

    #[test]
    fn mainnet_bitcoin_network_is_refused() {
        assert_eq!(
            Network::try_from_bitcoin(BitcoinNetwork::Bitcoin),
            Err(NetworkError::MainnetRefused)
        );
    }

    #[test]
    fn other_networks_are_unsupported() {
        for name in ["testnet", "testnet4", "", "regtest2"] {
            assert!(matches!(
                name.parse::<Network>(),
                Err(NetworkError::Unsupported(_))
            ));
        }
        assert!(matches!(
            Network::try_from_bitcoin(BitcoinNetwork::Testnet),
            Err(NetworkError::Unsupported(_))
        ));
    }

    #[test]
    fn supported_networks_round_trip() {
        for network in [Network::Regtest, Network::Signet] {
            assert_eq!(network.as_str().parse::<Network>(), Ok(network));
            assert_ne!(network.to_bitcoin(), BitcoinNetwork::Bitcoin);
            let json = serde_json::to_string(&network).expect("serialize");
            assert_eq!(serde_json::from_str::<Network>(&json).ok(), Some(network));
        }
    }

    /// Pins the mapping to concrete chains: a mix-up such as signet -> testnet
    /// would otherwise go unnoticed.
    #[test]
    fn networks_map_to_their_well_known_genesis_blocks() {
        assert_eq!(Network::Regtest.to_bitcoin(), BitcoinNetwork::Regtest);
        assert_eq!(Network::Signet.to_bitcoin(), BitcoinNetwork::Signet);
        assert_eq!(
            Network::Regtest.genesis_hash().to_string(),
            "0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206"
        );
        assert_eq!(
            Network::Signet.genesis_hash().to_string(),
            "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6"
        );
        let regtest = ChainIdentity::for_network(Network::Regtest);
        assert_eq!(regtest.network(), Network::Regtest);
        assert_eq!(regtest.genesis_hash(), Network::Regtest.genesis_hash());
        assert_ne!(regtest, ChainIdentity::for_network(Network::Signet));
    }

    #[test]
    fn chain_identity_cannot_be_deserialized_with_a_foreign_genesis() {
        let mainnet = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
        let signet = Network::Signet.genesis_hash();
        for json in [
            format!(r#"{{"network":"regtest","genesis_hash":"{mainnet}"}}"#),
            format!(r#"{{"network":"signet","genesis_hash":"{mainnet}"}}"#),
            format!(r#"{{"network":"regtest","genesis_hash":"{signet}"}}"#),
            format!(r#"{{"network":"mainnet","genesis_hash":"{mainnet}"}}"#),
        ] {
            assert!(
                serde_json::from_str::<ChainIdentity>(&json).is_err(),
                "{json}"
            );
        }
        let good = ChainIdentity::for_network(Network::Signet);
        let json = serde_json::to_string(&good).expect("serialize");
        assert_eq!(
            json,
            format!(r#"{{"network":"signet","genesis_hash":"{signet}"}}"#)
        );
        assert_eq!(
            serde_json::from_str::<ChainIdentity>(&json).ok(),
            Some(good)
        );
    }

    #[test]
    fn mainnet_genesis_is_rejected_even_when_labelled_regtest() {
        let mainnet_genesis = genesis_block(BitcoinNetwork::Bitcoin).block_hash();
        for network in [Network::Regtest, Network::Signet] {
            assert!(matches!(
                ChainIdentity::verified(network, mainnet_genesis),
                Err(NetworkError::GenesisMismatch { .. })
            ));
            assert_ne!(network.genesis_hash(), mainnet_genesis);
        }
        assert!(ChainIdentity::verified(Network::Regtest, Network::Regtest.genesis_hash()).is_ok());
    }
}
