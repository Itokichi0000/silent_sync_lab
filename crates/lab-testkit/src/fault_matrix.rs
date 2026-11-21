//! Fault injection matrix (design spec Phase 5, cases T10–T12).
//!
//! Every fault is injected into what a backend *receives* — the bytes a fake
//! Bitcoin Core REST server or a fake BlindBit Oracle answers with, or the
//! batch a source hands to the engine — and the real adapters and the real
//! engine are run on it. For each fault two things are observed:
//!
//! * **alone**: a scan through the faulty source only;
//! * **cross-check**: `verify::cross_check` of the faulty source against an
//!   honest full-block source.
//!
//! From the two, a verdict is derived: refused (fail-closed), no effect,
//! silent alone but shown by a cross-check, or not detected by either. The
//! verdicts are asserted in `crates/silent-sync/tests/fault_matrix.rs` and
//! rendered into `docs/FAULT_INJECTION.md`.
//!
//! The honest baseline is the basic fixture (8 blocks, 7 outputs for a wallet
//! knowing label 1).

use crate::{
    fake_http::{encode_spent_outputs, fixture_blocks, FakeCoreRest, FakeOracle},
    fixture_gen::{self, LABEL_USED},
    test_source::Tamper,
};
use silent_sync::{
    bitcoin::{
        consensus::encode::{deserialize, serialize},
        Block, ScriptBuf, Witness,
    },
    engine::{scan_range, ScanError},
    network::{ChainIdentity, Network},
    protocol::ReceiveScanner,
    source::{
        bitcoin_core::CoreRestSource,
        blindbit::BlindbitHttpSource,
        fixture::{FixtureFile, FixtureSource},
        local_index::{IndexPolicy, LocalTweakIndex},
        ScanBatch, ScanSource, SourceError,
    },
    verify::{cross_check, Explanation},
};
use std::collections::BTreeSet;

const TIP: u32 = 8;

/// Which kind of source the fault is injected into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Full blocks and spent outputs from a Bitcoin Core REST server.
    FullBlocks,
    /// A tweak index from a BlindBit Oracle, anchored to an honest node's headers.
    TweakIndex,
    /// The batch any source returns to the engine.
    Batch,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::FullBlocks => "full blocks (Bitcoin Core REST)",
            Target::TweakIndex => "tweak index (BlindBit Oracle HTTP)",
            Target::Batch => "any source (batch handed to the engine)",
        }
    }
}

/// What a scan through the faulty source alone did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alone {
    /// The scan failed; nothing was reported as scanned.
    Refused { class: &'static str },
    /// The scan succeeded with exactly the honest result.
    HonestResult,
    /// The scan succeeded and reported a result that is not the honest one.
    WrongResult {
        /// Honest detections that are absent.
        missing: usize,
        /// Detections that do not exist on the chain.
        invented: usize,
        /// Detections of the right output with a wrong value or label.
        altered: usize,
    },
}

/// What a cross-check against an honest full-block source did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrossChecked {
    Refused {
        class: &'static str,
    },
    Identical,
    /// Differences were reported; their explanation kinds, deduplicated.
    Differences {
        explanations: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The faulty source alone already fails the scan.
    Refused,
    /// The result is the honest one.
    NoEffect,
    /// Alone, a wrong result is reported as a success; a cross-check against
    /// an honest full-block source reports the difference (or fails).
    SilentAloneShownByCrossCheck,
    /// A wrong result is reported and the cross-check does not show it.
    NotDetected,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Refused => "refused (fail-closed)",
            Verdict::NoEffect => "no effect",
            Verdict::SilentAloneShownByCrossCheck => "NOT detected alone; shown by a cross-check",
            Verdict::NotDetected => "NOT detected (neither alone nor by a later cross-check)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: &'static str,
    /// Test case of the design spec this row belongs to.
    pub case: &'static str,
    pub target: Target,
    pub fault: &'static str,
    pub alone: Alone,
    pub cross_check: CrossChecked,
    pub verdict: Verdict,
}

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(Network::Regtest)
}

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).expect("scanner")
}

type Key = (u32, String, u32);

fn class_of_source(e: &SourceError) -> &'static str {
    match e {
        SourceError::InvalidRange { .. } => "invalid range",
        SourceError::RangeUnavailable { .. } => "range unavailable",
        SourceError::InvalidData(_) => "invalid data",
        SourceError::Unavailable(_) => "source unavailable",
        SourceError::Network(_) => "network refused",
    }
}

fn class_of(e: &ScanError) -> &'static str {
    match e {
        ScanError::ChainMismatch { .. } => "chain mismatch",
        ScanError::InvalidRange { .. } => "invalid range",
        ScanError::Source(e) => class_of_source(e),
        ScanError::InconsistentBatch { .. } => "inconsistent batch",
        ScanError::Transaction { .. } => "transaction error",
    }
}

/// Honest result of the baseline: (height, txid, vout) → (value, label).
fn honest() -> Vec<(Key, (u64, Option<u32>))> {
    let source = FixtureSource::from_file(fixture_gen::generate()).expect("fixture");
    scan_range(&source, &scanner(), regtest(), 1, TIP)
        .expect("honest scan")
        .detected_received_outputs
        .iter()
        .map(|d| {
            (
                (d.block.height, d.txid.to_string(), d.vout),
                (d.value_sats, d.label),
            )
        })
        .collect()
}

/// Runs the two observations on a faulty source. `opened` is the result of
/// connecting to it (connecting can already be refused).
fn observe(opened: Result<Box<dyn ScanSource>, SourceError>) -> (Alone, CrossChecked) {
    let source = match opened {
        Ok(source) => source,
        Err(e) => {
            let class = class_of_source(&e);
            return (Alone::Refused { class }, CrossChecked::Refused { class });
        }
    };
    let truth = honest();

    // Alone: up to the height the source itself reports as its tip.
    let alone = match source
        .tip()
        .map_err(ScanError::Source)
        .and_then(|tip| scan_range(source.as_ref(), &scanner(), regtest(), 1, tip.height))
    {
        Err(e) => Alone::Refused {
            class: class_of(&e),
        },
        Ok(report) => {
            let found: Vec<(Key, (u64, Option<u32>))> = report
                .detected_received_outputs
                .iter()
                .map(|d| {
                    (
                        (d.block.height, d.txid.to_string(), d.vout),
                        (d.value_sats, d.label),
                    )
                })
                .collect();
            let found_keys: BTreeSet<&Key> = found.iter().map(|(k, _)| k).collect();
            let true_keys: BTreeSet<&Key> = truth.iter().map(|(k, _)| k).collect();
            let missing = true_keys.difference(&found_keys).count();
            let invented = found_keys.difference(&true_keys).count();
            let altered = found
                .iter()
                .filter(|(k, v)| truth.iter().any(|(tk, tv)| tk == k && tv != v))
                .count();
            if missing + invented + altered == 0 {
                Alone::HonestResult
            } else {
                Alone::WrongResult {
                    missing,
                    invented,
                    altered,
                }
            }
        }
    };

    // Cross-check: over the honest chain's whole range.
    let reference = FixtureSource::from_file(fixture_gen::generate()).expect("fixture");
    let cross = match cross_check(source.as_ref(), &reference, &scanner(), regtest(), 1, TIP) {
        Err(e) => CrossChecked::Refused {
            class: class_of(&e),
        },
        Ok(report) if report.identical => CrossChecked::Identical,
        Ok(report) => {
            let kinds: BTreeSet<String> = report
                .differences
                .iter()
                .map(|d| match &d.explanation {
                    Explanation::Unexplained { .. } => "unexplained".to_string(),
                    other => serde_json::to_value(other)
                        .ok()
                        .and_then(|v| v["kind"].as_str().map(str::to_string))
                        .unwrap_or_default(),
                })
                .collect();
            let mut explanations: Vec<String> = kinds.into_iter().collect();
            if !report.block_hash_mismatches.is_empty() && explanations.is_empty() {
                explanations.push("block_hash_differs".to_string());
            }
            CrossChecked::Differences { explanations }
        }
    };
    (alone, cross)
}

fn verdict(alone: &Alone, cross: &CrossChecked) -> Verdict {
    match (alone, cross) {
        (Alone::Refused { .. }, _) => Verdict::Refused,
        (Alone::HonestResult, CrossChecked::Identical) => Verdict::NoEffect,
        // An honest result for the source's own (shorter) range, while the
        // cross-check over the full range fails or differs.
        (Alone::HonestResult, _) | (Alone::WrongResult { .. }, CrossChecked::Refused { .. }) => {
            Verdict::SilentAloneShownByCrossCheck
        }
        (Alone::WrongResult { .. }, CrossChecked::Differences { .. }) => {
            Verdict::SilentAloneShownByCrossCheck
        }
        // The cross-check asks the source again: a source that lied before
        // and answers honestly now is not exposed by it.
        (Alone::WrongResult { .. }, CrossChecked::Identical) => Verdict::NotDetected,
    }
}

// ---------------------------------------------------------------------------
// Fault definitions
// ---------------------------------------------------------------------------

fn fixture() -> FixtureFile {
    fixture_gen::generate()
}

fn core(node: &FakeCoreRest) -> Result<Box<dyn ScanSource>, SourceError> {
    Ok(Box::new(CoreRestSource::connect(&node.url())?))
}

/// A fault applied to a fake Bitcoin Core REST server.
fn core_fault(apply: &dyn Fn(&mut FakeCoreRest)) -> (Alone, CrossChecked) {
    let mut node = FakeCoreRest::serve(&fixture());
    apply(&mut node);
    observe(core(&node))
}

/// A fault applied to a fake oracle; headers come from an honest fake node.
fn oracle_fault(apply: &dyn Fn(&FakeOracle, &FakeCoreRest)) -> (Alone, CrossChecked) {
    let fixture = fixture();
    let node = FakeCoreRest::serve(&fixture);
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture).expect("fixture"),
        IndexPolicy::Bip352,
    )
    .fetch_range(1, TIP)
    .expect("index")
    .blocks;
    let oracle = FakeOracle::serve("regtest", &index);
    apply(&oracle, &node);
    let opened = CoreRestSource::connect(&node.url())
        .and_then(|headers| BlindbitHttpSource::connect(&oracle.url(), headers))
        .map(|s| Box::new(s) as Box<dyn ScanSource>);
    observe(opened)
}

/// The services behind fault I07 (a paying entry replayed in a later block),
/// kept alive for tests that need the faulty source itself.
pub struct ReplayingOracle {
    _node: FakeCoreRest,
    _oracle: FakeOracle,
    pub source: BlindbitHttpSource<CoreRestSource>,
}

impl ReplayingOracle {
    pub fn start() -> Self {
        let fixture = fixture();
        let node = FakeCoreRest::serve(&fixture);
        let index = LocalTweakIndex::new(
            FixtureSource::from_file(fixture).expect("fixture"),
            IndexPolicy::Bip352,
        )
        .fetch_range(1, TIP)
        .expect("index")
        .blocks;
        let oracle = FakeOracle::serve("regtest", &index);
        replay_payment(&oracle);
        let source = CoreRestSource::connect(&node.url())
            .and_then(|headers| BlindbitHttpSource::connect(&oracle.url(), headers))
            .expect("connect");
        Self {
            _node: node,
            _oracle: oracle,
            source,
        }
    }
}

fn replay_payment(oracle: &FakeOracle) {
    let source: serde_json::Value =
        serde_json::from_slice(&oracle.http.get(&FakeOracle::block_path(PAYMENT_HEIGHT)))
            .expect("oracle answer");
    let copy = source["index"][0].clone();
    edit_oracle_block(oracle, 5, &|v| entries(v).push(copy.clone()));
}

/// Rewrites the oracle's JSON answer for one height.
fn edit_oracle_block(oracle: &FakeOracle, height: u32, edit: &dyn Fn(&mut serde_json::Value)) {
    let path = FakeOracle::block_path(height);
    let mut value: serde_json::Value =
        serde_json::from_slice(&oracle.http.get(&path)).expect("oracle answer");
    edit(&mut value);
    oracle.http.set(&path, value.to_string().into_bytes());
}

/// A fault applied to the batches of an otherwise honest full-block source.
fn batch_fault(tamper: impl Fn(&mut ScanBatch) + 'static) -> (Alone, CrossChecked) {
    let source = FixtureSource::from_file(fixture()).expect("fixture");
    observe(Ok(Box::new(Tamper::new(source, tamper))))
}

/// Height and index-entry position of the fixture's first payment (block 2
/// holds exactly one scannable transaction: the payment).
const PAYMENT_HEIGHT: u32 = 2;

fn entries(v: &mut serde_json::Value) -> &mut Vec<serde_json::Value> {
    v["index"].as_array_mut().expect("index entries")
}

/// Runs every fault and returns the observations.
pub fn run() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut add = |id, case, target, fault, (alone, cross): (Alone, CrossChecked)| {
        let verdict = verdict(&alone, &cross);
        rows.push(Row {
            id,
            case,
            target,
            fault,
            alone,
            cross_check: cross,
            verdict,
        });
    };
    use Target::*;

    // ---- full blocks from a node ----
    add(
        "F01",
        "T11",
        FullBlocks,
        "a transaction is removed from a block (header unchanged)",
        core_fault(&|node| {
            let path = node.block_path(PAYMENT_HEIGHT);
            let mut block: Block = deserialize(&node.get(&path)).expect("block");
            block.txdata.remove(1);
            node.set(&path, serialize(&block));
            let mut prevouts = fixture_blocks(&fixture())[PAYMENT_HEIGHT as usize - 1]
                .1
                .clone();
            prevouts.remove(1);
            node.set(
                &node.spent_path(PAYMENT_HEIGHT),
                encode_spent_outputs(&prevouts),
            );
        }),
    );
    add(
        "F02",
        "T11",
        FullBlocks,
        "a block is replaced by a different valid block (re-sealed without the payment)",
        core_fault(&|node| {
            let path = node.block_path(PAYMENT_HEIGHT);
            let mut block: Block = deserialize(&node.get(&path)).expect("block");
            block.txdata.remove(1);
            fixture_gen::seal(&mut block);
            node.set(&path, serialize(&block));
            let mut prevouts = fixture_blocks(&fixture())[PAYMENT_HEIGHT as usize - 1]
                .1
                .clone();
            prevouts.remove(1);
            node.set(
                &node.spent_path(PAYMENT_HEIGHT),
                encode_spent_outputs(&prevouts),
            );
        }),
    );
    add(
        "F03",
        "T11",
        FullBlocks,
        "witnesses are stripped from a block (the block hash stays the same)",
        core_fault(&|node| {
            let path = node.block_path(PAYMENT_HEIGHT);
            let mut block: Block = deserialize(&node.get(&path)).expect("block");
            for tx in block.txdata.iter_mut().skip(1) {
                for txin in &mut tx.input {
                    txin.witness = Witness::new();
                }
            }
            node.set(&path, serialize(&block));
        }),
    );
    add(
        "F04",
        "T11",
        FullBlocks,
        "the spent-output list of a block lacks one transaction",
        core_fault(&|node| {
            let mut prevouts = fixture_blocks(&fixture())[PAYMENT_HEIGHT as usize - 1]
                .1
                .clone();
            prevouts.pop();
            node.set(
                &node.spent_path(PAYMENT_HEIGHT),
                encode_spent_outputs(&prevouts),
            );
        }),
    );
    add(
        "F05",
        "T11",
        FullBlocks,
        "spent outputs created before the scanned range are replaced by well-formed lies",
        core_fault(&|node| {
            let mut prevouts = fixture_blocks(&fixture())[PAYMENT_HEIGHT as usize - 1]
                .1
                .clone();
            for prevout in prevouts.iter_mut().flatten() {
                // A P2WSH script: the input no longer looks eligible.
                prevout.script_pubkey = ScriptBuf::from_hex(
                    "0020000000000000000000000000000000000000000000000000000000000000dead",
                )
                .expect("script");
            }
            node.set(
                &node.spent_path(PAYMENT_HEIGHT),
                encode_spent_outputs(&prevouts),
            );
        }),
    );
    add(
        "F06",
        "T12",
        FullBlocks,
        "the node reports a tip two blocks below its real chain (recent blocks hidden)",
        core_fault(&|node| {
            let hidden = TIP - 2;
            node.set_chain_info("regtest", hidden, node.hashes[hidden as usize]);
        }),
    );
    add(
        "F07",
        "T12",
        FullBlocks,
        "the node reports a tip height its chain does not reach",
        core_fault(&|node| node.set_chain_info("regtest", TIP + 5, node.hashes[TIP as usize])),
    );
    add(
        "F08",
        "T12",
        FullBlocks,
        "one block is not served (HTTP 404)",
        core_fault(&|node| node.remove(&node.block_path(5))),
    );
    add(
        "F09",
        "T12",
        FullBlocks,
        "the node stops answering (connection refused)",
        {
            let node = FakeCoreRest::serve(&fixture());
            let url = node.url();
            drop(node);
            observe(CoreRestSource::connect(&url).map(|s| Box::new(s) as Box<dyn ScanSource>))
        },
    );
    add(
        "F10",
        "T11",
        FullBlocks,
        "the node says it is on mainnet",
        core_fault(&|node| node.set_chain_info("main", TIP, node.hashes[TIP as usize])),
    );
    add(
        "F11",
        "T11",
        FullBlocks,
        "the node announces another network (signet) for the chain it serves",
        core_fault(&|node| node.set_chain_info("signet", TIP, node.hashes[TIP as usize])),
    );
    add(
        "F12",
        "T11",
        FullBlocks,
        "the node consistently serves another chain built on the same genesis block (another fork, or an impostor on the port)",
        {
            let node = FakeCoreRest::serve(&fixture_gen::generate_fork());
            observe(core(&node))
        },
    );

    // ---- tweak index from an oracle ----
    add(
        "I01",
        "T10",
        TweakIndex,
        "the index entry of a paying transaction is removed",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                entries(v).remove(0);
            })
        }),
    );
    add(
        "I02",
        "T10",
        TweakIndex,
        "the paying output is removed from its index entry",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                let outputs = entries(v)[0]["utxos"].as_array_mut().expect("outputs");
                outputs.retain(|o| o["vout"] != 0);
            })
        }),
    );
    add(
        "I03",
        "T10",
        TweakIndex,
        "the tweak of a paying transaction is replaced by another transaction's tweak",
        oracle_fault(&|oracle, _| {
            let other: serde_json::Value =
                serde_json::from_slice(&oracle.http.get(&FakeOracle::block_path(3)))
                    .expect("oracle answer");
            let tweak = other["index"][0]["tweak"].clone();
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                entries(v)[0]["tweak"] = tweak.clone();
            })
        }),
    );
    add(
        "I04",
        "T11",
        TweakIndex,
        "the amount of a paying output is altered",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                entries(v)[0]["utxos"][0]["amount"] = serde_json::json!(21_000_000);
            })
        }),
    );
    add(
        "I05",
        "T11",
        TweakIndex,
        "the output key of a paying output is altered",
        oracle_fault(&|oracle, _| {
            let other: serde_json::Value =
                serde_json::from_slice(&oracle.http.get(&FakeOracle::block_path(3)))
                    .expect("oracle answer");
            let key = other["index"][0]["utxos"][0]["pubkey"].clone();
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                entries(v)[0]["utxos"][0]["pubkey"] = key.clone();
            })
        }),
    );
    add(
        "I06",
        "T11",
        TweakIndex,
        "a paying entry is copied under a transaction id that is not in the block (fabricated payment)",
        oracle_fault(&|oracle, _| {
            let source: serde_json::Value = serde_json::from_slice(
                &oracle.http.get(&FakeOracle::block_path(PAYMENT_HEIGHT)),
            )
            .expect("oracle answer");
            let mut copy = source["index"][0].clone();
            copy["txid"] = serde_json::json!("ab".repeat(32));
            edit_oracle_block(oracle, 5, &|v| entries(v).push(copy.clone()));
        }),
    );
    add(
        "I07",
        "T11",
        TweakIndex,
        "a paying entry is replayed in a later block under its own transaction id",
        oracle_fault(&|oracle, _| replay_payment(oracle)),
    );
    add(
        "I08",
        "T11",
        TweakIndex,
        "an index entry is listed twice in the same block",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                let first = entries(v)[0].clone();
                entries(v).push(first);
            })
        }),
    );
    add(
        "I09",
        "T11",
        TweakIndex,
        "the entries of a block are served in reverse order",
        oracle_fault(&|oracle, _| edit_oracle_block(oracle, 3, &|v| entries(v).reverse())),
    );
    add(
        "I10",
        "T11",
        TweakIndex,
        "an entry for a transaction that pays someone else is removed",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, 3, &|v| {
                // Block 3 lists the receiver's payment first, then another receiver's.
                entries(v).pop();
            })
        }),
    );
    add(
        "I11",
        "T11",
        TweakIndex,
        "the oracle answers a height with the block hash of another chain",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, 4, &|v| {
                v["block_identifier"]["block_hash"] = serde_json::json!("11".repeat(32));
            })
        }),
    );
    add(
        "I12",
        "T12",
        TweakIndex,
        "the oracle has no data for a height below its claimed tip (HTTP 200, empty hash)",
        oracle_fault(&|oracle, _| {
            oracle.http.set(
                &FakeOracle::block_path(6),
                crate::fake_http::oracle_full_block_json(6, None, &[]),
            )
        }),
    );
    add(
        "I13",
        "T12",
        TweakIndex,
        "the oracle has indexed fewer blocks than the node has",
        oracle_fault(&|oracle, _| oracle.set_info("regtest", TIP - 2, false)),
    );
    add(
        "I14",
        "T12",
        TweakIndex,
        "the oracle stops answering block requests (HTTP 404)",
        oracle_fault(&|oracle, _| oracle.http.remove(&FakeOracle::block_path(4))),
    );
    add(
        "I15",
        "T11",
        TweakIndex,
        "the oracle says it indexes mainnet",
        oracle_fault(&|oracle, _| oracle.set_info("main", TIP, false)),
    );
    add(
        "I16",
        "T11",
        TweakIndex,
        "a tweak is not a point on the curve",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                entries(v)[0]["tweak"] = serde_json::json!(format!("02{}", "00".repeat(32)));
            })
        }),
    );
    add(
        "I17",
        "T11",
        TweakIndex,
        "the amount of a paying output is set above the total supply",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, PAYMENT_HEIGHT, &|v| {
                entries(v)[0]["utxos"][0]["amount"] = serde_json::json!(2_100_000_000_000_001u64);
            })
        }),
    );
    add(
        "I18",
        "T11",
        TweakIndex,
        "an index entry lists no outputs",
        oracle_fault(&|oracle, _| {
            edit_oracle_block(oracle, 3, &|v| {
                entries(v)[1]["utxos"] = serde_json::json!([]);
            })
        }),
    );
    add(
        "I19",
        "T10",
        TweakIndex,
        "the index omits a paying entry only the first time the block is requested (injected into a locally computed index)",
        {
            let index = LocalTweakIndex::new(
                FixtureSource::from_file(fixture()).expect("fixture"),
                IndexPolicy::Bip352,
            );
            let asked_before = std::cell::Cell::new(false);
            observe(Ok(Box::new(Tamper::new(
                index,
                move |batch: &mut ScanBatch| {
                    for block in &mut batch.blocks {
                        if block.block.height != PAYMENT_HEIGHT {
                            continue;
                        }
                        if let silent_sync::source::Evidence::TweakIndex(entries) =
                            &mut block.evidence
                        {
                            if !asked_before.replace(true) {
                                entries.clear();
                            }
                        }
                    }
                },
            ))))
        },
    );

    // ---- batches handed to the engine ----
    add(
        "B01",
        "T11",
        Batch,
        "the blocks of a response are in the wrong order",
        batch_fault(|batch| batch.blocks.reverse()),
    );
    add(
        "B02",
        "T11",
        Batch,
        "a block appears twice in a response",
        batch_fault(|batch| {
            if let Some(first) = batch.blocks.first().cloned() {
                batch.blocks.insert(0, first);
            }
        }),
    );
    add(
        "B03",
        "T11",
        Batch,
        "the last block of a response is missing (partial response)",
        batch_fault(|batch| {
            batch.blocks.pop();
        }),
    );
    add(
        "B04",
        "T11",
        Batch,
        "a block in the middle of a response is missing",
        batch_fault(|batch| {
            if batch.blocks.len() > 2 {
                batch.blocks.remove(1);
            }
        }),
    );
    rows
}

fn alone_text(alone: &Alone) -> String {
    match alone {
        Alone::Refused { class } => format!("scan fails ({class})"),
        Alone::HonestResult => {
            "scan succeeds with the honest result for the range the source reports".to_string()
        }
        Alone::WrongResult {
            missing,
            invented,
            altered,
        } => {
            let mut parts = Vec::new();
            if *missing > 0 {
                parts.push(format!("{missing} payment(s) missing"));
            }
            if *invented > 0 {
                parts.push(format!("{invented} non-existent payment(s) reported"));
            }
            if *altered > 0 {
                parts.push(format!("{altered} payment(s) with a wrong amount"));
            }
            format!("**scan succeeds**: {}", parts.join(", "))
        }
    }
}

fn cross_text(cross: &CrossChecked) -> String {
    match cross {
        CrossChecked::Refused { class } => format!("fails ({class})"),
        CrossChecked::Identical => "identical".to_string(),
        CrossChecked::Differences { explanations } => {
            format!("differences: `{}`", explanations.join("`, `"))
        }
    }
}

/// The table of `docs/FAULT_INJECTION.md`.
pub fn render_markdown(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str("| ID | Case | Injected into | Fault | Faulty source alone | Cross-check against an honest full-block source | Verdict |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            row.id,
            row.case,
            row.target.as_str(),
            row.fault,
            alone_text(&row.alone),
            cross_text(&row.cross_check),
            row.verdict.as_str(),
        ));
    }
    out
}
