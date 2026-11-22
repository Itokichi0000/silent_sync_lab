//! Network identity.
//!
//! Without the `mainnet` feature, mainnet is not representable in this crate's
//! types. With it, `Network::Mainnet` exists for the receive side only.

use crate::bitcoin::{blockdata::constants::genesis_block, BlockHash, Network as BitcoinNetwork};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The networks this crate may operate on.
///
/// Unless the crate is built with the `mainnet` feature there is no mainnet
/// variant: code that holds a `Network` cannot be pointed at mainnet, and
/// every parser rejects mainnet names with a dedicated error.
///
/// The enum is `#[non_exhaustive]` so that code written against the default
/// build keeps compiling when another crate in the same build enables
/// `mainnet`. Code that must stay off mainnet (anything that signs or sends)
/// should check [`Network::is_mainnet`] rather than rely on the feature being
/// off: Cargo enables a feature for every user of the crate at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[non_exhaustive]
pub enum Network {
    /// Local regression-test chain.
    Regtest,
    /// The default public signet.
    Signet,
    /// Bitcoin mainnet. Receive-side scanning only; this code is unaudited.
    /// Exists only with the `mainnet` feature.
    #[cfg(feature = "mainnet")]
    Mainnet,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
/// Why a network was refused.
pub enum NetworkError {
    #[error(
        "mainnet is refused: this is experimental, unaudited software and must not be used \
         with mainnet or real funds"
    )]
    /// Mainnet was named, and the crate was built without the `mainnet`
    /// feature.
    MainnetRefused,
    #[error("unsupported network `{0}`: only `regtest` and `signet` are supported")]
    /// A network this crate does not support (the name is given).
    Unsupported(String),
    #[error("genesis hash mismatch for {network}: expected {expected}, got {actual}")]
    /// The chain does not start at the network's genesis block.
    GenesisMismatch {
        /// Network that was claimed.
        network: Network,
        /// Genesis hash of that network.
        expected: BlockHash,
        /// Genesis hash that was found.
        actual: BlockHash,
    },
}

impl Network {
    /// The network's name as used in text and JSON (`regtest`, `signet`,
    /// and `mainnet` with the feature of that name).
    pub const fn as_str(self) -> &'static str {
        match self {
            Network::Regtest => "regtest",
            Network::Signet => "signet",
            #[cfg(feature = "mainnet")]
            Network::Mainnet => "mainnet",
        }
    }

    /// Whether this is Bitcoin mainnet. Always `false` in a build without
    /// the `mainnet` feature.
    ///
    /// Code that handles spending keys must refuse a network for which this
    /// is `true`: the feature covers receive-side scanning only.
    pub const fn is_mainnet(self) -> bool {
        match self {
            Network::Regtest | Network::Signet => false,
            #[cfg(feature = "mainnet")]
            Network::Mainnet => true,
        }
    }

    /// The corresponding `bitcoin::Network`. Returns
    /// `bitcoin::Network::Bitcoin` only for `Network::Mainnet`.
    pub const fn to_bitcoin(self) -> BitcoinNetwork {
        match self {
            Network::Regtest => BitcoinNetwork::Regtest,
            Network::Signet => BitcoinNetwork::Signet,
            #[cfg(feature = "mainnet")]
            Network::Mainnet => BitcoinNetwork::Bitcoin,
        }
    }

    /// Fallible conversion from `bitcoin::Network`; mainnet is refused
    /// without the `mainnet` feature.
    pub fn try_from_bitcoin(network: BitcoinNetwork) -> Result<Self, NetworkError> {
        match network {
            BitcoinNetwork::Regtest => Ok(Network::Regtest),
            BitcoinNetwork::Signet => Ok(Network::Signet),
            BitcoinNetwork::Bitcoin => mainnet(),
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

/// The one place that decides whether mainnet may be named.
#[cfg(feature = "mainnet")]
pub(crate) const fn mainnet() -> Result<Network, NetworkError> {
    Ok(Network::Mainnet)
}

/// The one place that decides whether mainnet may be named.
#[cfg(not(feature = "mainnet"))]
pub(crate) const fn mainnet() -> Result<Network, NetworkError> {
    Err(NetworkError::MainnetRefused)
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
            "mainnet" | "main" | "bitcoin" => mainnet(),
            // Never a network name here, with or without the feature.
            "livenet" | "prod" | "production" => Err(NetworkError::MainnetRefused),
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
/// value that pairs a network name with another chain's genesis (mainnet's
/// under a test network's name in particular) cannot exist.
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
    /// The network.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Hash of the chain's genesis block.
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

    #[cfg(not(feature = "mainnet"))]
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

    #[cfg(not(feature = "mainnet"))]
    #[test]
    fn mainnet_is_refused_by_the_deserializer() {
        let err = serde_json::from_str::<Network>("\"mainnet\"")
            .expect_err("mainnet must not deserialize");
        assert!(err.to_string().contains("mainnet is refused"), "{err}");
        assert!(serde_json::from_str::<Network>("\"bitcoin\"").is_err());
    }

    #[cfg(not(feature = "mainnet"))]
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
            assert!(!network.is_mainnet());
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
            #[cfg(not(feature = "mainnet"))]
            format!(r#"{{"network":"mainnet","genesis_hash":"{mainnet}"}}"#),
            #[cfg(feature = "mainnet")]
            format!(r#"{{"network":"mainnet","genesis_hash":"{signet}"}}"#),
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

    /// With the feature, mainnet has its own name, its own genesis block and
    /// its own `bitcoin::Network`, and nothing else maps to it.
    #[cfg(feature = "mainnet")]
    #[test]
    fn mainnet_is_a_network_of_its_own_with_the_feature() {
        for name in ["mainnet", "main", "bitcoin", "Mainnet", " BITCOIN "] {
            assert_eq!(name.parse::<Network>(), Ok(Network::Mainnet), "{name}");
        }
        for name in ["livenet", "prod", "production"] {
            assert_eq!(
                name.parse::<Network>(),
                Err(NetworkError::MainnetRefused),
                "{name}"
            );
        }
        assert!(Network::Mainnet.is_mainnet());
        assert_eq!(Network::Mainnet.as_str(), "mainnet");
        assert_eq!(Network::Mainnet.to_bitcoin(), BitcoinNetwork::Bitcoin);
        assert_eq!(
            Network::try_from_bitcoin(BitcoinNetwork::Bitcoin),
            Ok(Network::Mainnet)
        );
        assert_eq!(
            Network::Mainnet.genesis_hash().to_string(),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
        assert_eq!(
            serde_json::from_str::<Network>("\"mainnet\"").ok(),
            Some(Network::Mainnet)
        );
        assert_eq!(
            serde_json::to_string(&Network::Mainnet).ok().as_deref(),
            Some("\"mainnet\"")
        );

        // The identity pairs the name with mainnet's genesis block and with
        // no other; a test network's name does not go with mainnet's block.
        let mainnet = ChainIdentity::for_network(Network::Mainnet);
        assert_ne!(mainnet, ChainIdentity::for_network(Network::Signet));
        for other in [Network::Regtest, Network::Signet] {
            assert!(matches!(
                ChainIdentity::verified(Network::Mainnet, other.genesis_hash()),
                Err(NetworkError::GenesisMismatch { .. })
            ));
            assert!(matches!(
                ChainIdentity::verified(other, Network::Mainnet.genesis_hash()),
                Err(NetworkError::GenesisMismatch { .. })
            ));
        }
        let json = serde_json::to_string(&mainnet).expect("serialize");
        assert_eq!(
            serde_json::from_str::<ChainIdentity>(&json).ok(),
            Some(mainnet)
        );
    }
}
