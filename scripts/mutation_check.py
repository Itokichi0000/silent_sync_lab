#!/usr/bin/env python3
"""Mutation check for the validation code in silent-sync.

Each entry disables exactly one check. For every entry the script applies the
change to a *scratch copy* of the repository, runs the fast test suite and
reports whether some test failed ("killed") or none did ("SURVIVED"). A
survivor means a check exists that no test would miss if it were removed.

Usage (never run it against the working tree itself):

    rm -rf /tmp/ssl-mut && mkdir -p /tmp/ssl-mut/repo
    git ls-files -co --exclude-standard | tar cf - -T - | tar xf - -C /tmp/ssl-mut/repo
    python3 scripts/mutation_check.py /tmp/ssl-mut/repo
    python3 scripts/mutation_check.py /tmp/ssl-mut/repo "sync:,bench:"   # only these groups

The slow K_max tests are skipped (`--skip k_max`); the K_max correction is
covered by `crates/silent-sync/tests/bip352_vectors.rs` instead. Exit status is
non-zero if anything survived. The patterns are tied to the current source
text: when a pattern no longer matches, the entry is reported as SETUP-ERROR
and must be updated together with the code.
"""
import os
import subprocess
import sys

if len(sys.argv) not in (2, 3) or not os.path.isfile(os.path.join(sys.argv[1], "Cargo.toml")):
    sys.exit(__doc__)
# Optional second argument: comma-separated prefixes; only entries whose name
# starts with one of them are run (to re-check a part after changing it).
only = [p for p in sys.argv[2].split(",") if p] if len(sys.argv) == 3 else []
root = os.path.abspath(sys.argv[1])
if os.path.isdir(os.path.join(root, ".git")):
    sys.exit("refusing to mutate a git working tree; use a scratch copy (see usage)")
env = dict(
    os.environ,
    CARGO_TARGET_DIR=os.path.join(os.path.dirname(root), "mut-target"),
    RUST_BACKTRACE="0",
)
B = "crates/silent-sync/src/source/block_check.rs"
C = "crates/silent-sync/src/source/bitcoin_core.rs"
H = "crates/silent-sync/src/source/http.rs"
O = "crates/silent-sync/src/source/blindbit.rs"
L = "crates/silent-sync/src/source/local_index.rs"
V = "crates/silent-sync/src/verify.rs"
A = "crates/silent-sync/src/audit.rs"
R = "crates/silent-sync/src/rescan.rs"
E = "crates/silent-sync/src/engine.rs"
P = "crates/silent-sync/src/protocol.rs"
N = "crates/silent-sync/src/network.rs"
S = "crates/silent-sync/src/sync.rs"
T = "crates/silent-sync/src/store.rs"
Q = "crates/silent-sync-sqlite/src/lib.rs"
M = "crates/lab-cli/src/main.rs"
K = "crates/lab-cli/src/bench.rs"
SP = "crates/silent-sync/src/spend.rs"
W = "crates/silent-wallet/src/send.rs"
WH = "crates/silent-wallet/src/holdings.rs"
WR = "crates/silent-wallet/src/rpc.rs"
WK = "crates/silent-wallet/src/keys.rs"
WP = "crates/silent-wallet/src/pending.rs"
WC = "crates/wallet-cli/src/main.rs"
CL = "crates/silent-wallet/src/client.rs"
# Entries with a fifth element are run with those cargo arguments instead of
# the whole workspace: checks that only exist, or only matter, when some
# crate in the build has turned on a `mainnet` feature.
SDK_MAINNET = ["-p", "silent-sync", "--features", "mainnet,core-rest,blindbit-http,fixture,local-index,low-level,spend"]
UNIFIED = ["--features", "silent-sync/mainnet"]
muts = [
    # --- block structure checks shared by the fixture and Core backends ---
    ("pow check removed", B, "    header.validate_pow(target).map_err(|e| e.to_string())", "    Ok(header.block_hash())"),
    ("pow: canonical bits check removed", B, "if target.to_compact_lossy() != bits {", "if false {"),
    ("pow: network limit check removed", B, "if target > limit {", "if false {"),
    ("bip34 mismatch accepted", B, "Some(h) if h == height => {}", "Some(_) => {}\n        #[allow(unreachable_patterns)]\n        Some(h) if h == height => {}"),
    ("bip34 missing accepted", B, 'None => return Err("coinbase does not commit to a height".to_string()),', "None => {}"),
    ("coinbase-first (early) removed", B, "if !block.txdata.first().is_some_and(|tx| tx.is_coinbase()) {", "if false {"),
    ("coinbase position (loop) removed", B, "if tx.is_coinbase() != is_first {", "if false {"),
    ("duplicate txid in block removed", B, "if !seen_txids.insert(txid) {", "if !seen_txids.insert(txid) && false {"),
    ("link of block 1 to genesis removed", B, "if block.header.prev_blockhash != parent {", "if block.header.prev_blockhash != parent && height != 1 {"),
    ("parent link removed", B, "if block.header.prev_blockhash != parent {", "if false {"),
    ("requested-hash check removed", B, "if hash != expected {", "if false {"),
    ("witness commitment: mismatch accepted", B, "if expected.as_byte_array() != committed {", "if false {"),
    ("witness commitment: reserved value not required", B, "Some(items) if items.len() == 1 && items[0].len() == 32 => items[0].clone(),", "Some(items) if items.len() == 1 && items[0].len() == 32 => items[0].clone(),\n        Some(items) if items.is_empty() => return Ok(()),"),
    ("witness without commitment accepted", B, "return if has_witness {", "return if has_witness && false {"),
    ("merkle root check removed", B, "if !block.check_merkle_root() {", "if false {"),
    ("prevout list count unchecked", B, "if prevouts.len() != block.txdata.len() {", "if false {"),
    ("prevout count per tx unchecked", B, "if tx_prevouts.len() != expected_prevouts {", "if false {"),
    ("witness commitment: first match used instead of last", B, "        .iter()\n        .rev()\n        .map(|o| o.script_pubkey.as_bytes())", "        .iter()\n        .map(|o| o.script_pubkey.as_bytes())"),
    ("witness commitment: longer scripts ignored", B, "script.len() >= 38 && script[..6] == MAGIC", "script.len() == 38 && script[..6] == MAGIC"),
    ("witness commitment: extra reserved items accepted", B, "Some(items) if items.len() == 1 && items[0].len() == 32 => items[0].clone(),", "Some(items) if !items.is_empty() && items[0].len() == 32 => items[0].clone(),"),
    ("prevouts not cross-checked within the range", B, "if *real != supplied {", "if false {"),
    # --- loopback HTTP client shared by the network backends ---
    ("http: non-loopback host accepted", H, "if !ip.is_loopback() {", "if false {"),
    ("http: port not validated", H, "if !valid_port {", "if false {"),
    ("http: credentials echoed in refusal", H, "let shown = if url.contains('@') {", "let shown = if false {"),
    ("http: redirects followed", H, ".with_max_redirects(0)", ".with_max_redirects(5)"),
    ("http: no timeout", H, ".with_timeout(self.timeout_secs)", ".with_timeout(3_600)"),
    ("http: header size unbounded", H, "            .with_max_headers_size(MAX_HEADER_BYTES)\n", ""),
    ("http: status line unbounded", H, "            .with_max_status_line_length(MAX_HEADER_BYTES)\n", ""),
    ("http: any error status read as not found", H, "            (404, _) => Ok(None),", "            (_, _) if true => Ok(None),"),
    ("http: non-200 success statuses accepted", H, "            (200, body) => Ok(body),", "            (status, body) if status < 400 => Ok(body),"),
    ("http: response size unbounded", H, "if body.len() as u64 > self.max_body_bytes {", "if false {"),
    # --- Bitcoin Core REST backend ---
    ("core: mainnet chain name accepted", C, '"main" => Ok(crate::network::mainnet()?),', '"main" => Ok(Network::Regtest),'),
    ("core: node's block 0 not compared with genesis", C, "ChainIdentity::verified(network, genesis)?;", "let _ = genesis;"),
    ("core: header chain links unchecked", C, "if header.prev_blockhash != parent {", "if false {"),
    ("core: header proof of work unchecked", C, "let hash = check_proof_of_work(header, network)", "let hash = Ok::<_, String>(header.block_hash())"),
    ("core: header response need not start at the anchor", C, "Some(first) if first.block_hash() == top => {}", "Some(_) if true => {}"),
    ("core: stale header anchor kept after a reorg", C, "            anchored.truncate(low + 1);", "            let _ = low;"),
    ("core: reorg fork point not bisected", C, "                let mid = low + (high - low) / 2;", "                let mid = high - 1;"),
    ("core: trailing bytes after block accepted", C, "if !cursor.is_empty() {\n                return Err(at(", "if false {\n                return Err(at("),
    ("core: trailing bytes after spent outputs accepted", C, "if !cursor.is_empty() {\n        return Err(format!(", "if false {\n        return Err(format!("),
    ("core: spent-output transaction count unbounded", C, "if count > MAX_TXS_PER_BLOCK {", "if false {"),
    ("core: range beyond tip not refused", C, "if from < 1 || to > tip {", "if false {"),
    ("core: chain switch at tip() unnoticed", C, "if network_from_core_chain(&info.chain)? != self.chain.network() {", "if false {"),
    ("core: tip not bound to the header chain", C, "if tip.hash != info.bestblockhash {", "if false {"),
    ("core: block not checked against requested hash", C, "hash: Some(hash),", "hash: None,"),
    # --- BlindBit Oracle backend ---
    ("oracle: mainnet name accepted", O, '"main" | "mainnet" => Ok(crate::network::mainnet()?),', '"main" | "mainnet" => Ok(Network::Regtest),'),
    ("oracle: network not compared with header source", O, "if network != source.headers.chain().network() {", "if false {"),
    ("oracle: tweaks_only accepted", O, "if info.tweaks_only {", "if false {"),
    ("oracle: block hash not compared with header source", O, "if hash != expected.hash {", "if false {"),
    ("oracle: range beyond the index not refused", O, "if from < 1 || to > indexed_to {", "if false {"),
    # --- tweak-index scanning and cross-check ---
    ("index: duplicate output accepted", P, "if let Some(dup) = indexed.outputs.iter().find(|o| !seen.insert(o.vout)) {", "if let Some(dup) = indexed.outputs.iter().find(|o| !seen.insert(o.vout) && false) {"),
    ("index: duplicate transaction accepted", E, "if !seen_txids.insert(entry.txid) {", "if !seen_txids.insert(entry.txid) && false {"),
    ("verify: chain mismatch unchecked", V, "if source.chain() != expected_chain {", "if false {"),
    ("verify: block hash differences ignored", V, "let same_block: bool = same_hash(c_block.block, r_block.block);", "let same_block: bool = true;"),
    ("verify: detail differences ignored", V, "} else if c.value_sats != r.value_sats || c.label != r.label {", "} else if false {"),
    # --- scanner wrapper ---
    ("missing-witness check removed", P, ".position(|(txin, prevout)| witness_is_missing(txin, &prevout.script_pubkey))", ".position(|(txin, prevout)| false && witness_is_missing(txin, &prevout.script_pubkey))"),
    ("missing-witness: p2sh-wrapped dropped", P, "return redeem.is_p2wpkh() || redeem.is_p2wsh();", "return false;"),
    ("missing-witness: any p2sh flagged", P, "if let (Some(Ok(Instruction::PushBytes(push))), None) =\n            (instructions.next(), instructions.next())", "if let (Some(Ok(Instruction::PushBytes(push))), _) =\n            (instructions.next(), instructions.next())"),
    ("segwit>1 rule removed", P, "if spends_unknown_segwit {", "if false && spends_unknown_segwit {"),
    ("prevout count check removed", P, "if prevouts.len() != tx.input.len() {", "if false {"),
    # --- engine ---
    ("engine: empty block accepted", E, "if txs.is_empty() {", "if false {"),
    ("engine: coinbase position unchecked", E, "if entry.tx.is_coinbase() != (position == 0) {", "if false {"),
    ("engine: duplicate txid unchecked", E, "if !seen_txids.insert(txid) {", "if !seen_txids.insert(txid) && false {"),
    ("engine: self-parent unchecked", E, "if block.hash == block.prev_hash {", "if false {"),
    ("engine: hash link unchecked", E, "if block.prev_hash != prev.hash {", "if false {"),
    ("engine: chain mismatch unchecked", E, "if chain != expected_chain {", "if false {"),
    # --- network identity ---
    ("signet mapped to testnet", N, "Network::Signet => BitcoinNetwork::Signet,", "Network::Signet => BitcoinNetwork::Testnet,"),
    ("chain identity deserialize unverified", N, "Self::verified(raw.network, raw.genesis_hash)", "Ok(Self { network: raw.network, genesis_hash: raw.genesis_hash })"),
    ("mainnet name accepted as regtest", N, '"regtest" => Ok(Network::Regtest),', '"regtest" | "mainnet" => Ok(Network::Regtest),'),
    # --- tweak-index evidence: bounds in the engine, local index, cross-check ---
    ("index: entry without outputs accepted", E, "if entry.outputs.is_empty() {", "if false {"),
    ("index: impossible amount accepted", E, ".find(|o| o.value_sats > max_value)", ".find(|_| false)"),
    ("index: output count unbounded", E, "if outputs > MAX_INDEXED_OUTPUTS_PER_BLOCK {", "if false {"),
    ("index: detections not ordered by output index", E, "                    })?;\n                    matches.sort_by_key(|m| m.outpoint.vout);", "                    })?;"),
    ("local index: non-taproot outputs listed", L, "                    .filter(|(_, o)| o.script_pubkey.is_p2tr())\n", ""),
    ("local index: policy not part of the backend id", L, "IndexPolicy::IgnoreSegwitVersionRule => LOCAL_INDEX_LENIENT_BACKEND_ID,", "IndexPolicy::IgnoreSegwitVersionRule => LOCAL_INDEX_BACKEND_ID,"),
    ("verify: candidate batch not checked", V, "        check_batch(\n            &candidate_id,\n            &candidate_batch.blocks,\n            next,\n            last,\n            candidate.capabilities().evidence_kind,\n        )?;", "        let _ = (&candidate_id, check_batch);"),
    ("verify: reference batch not checked", V, "        check_batch(\n            &reference_id,\n            &reference_batch.blocks,\n            next,\n            last,\n            reference.capabilities().evidence_kind,\n        )?;", "        let _ = (&reference_id, check_batch);"),
    ("verify: candidate chunks not linked", V, "previous_c = linked(&candidate_id, previous_c, &candidate_batch.blocks)?;", "previous_c = linked(&candidate_id, None, &candidate_batch.blocks)?;"),
    ("verify: reference chunks not linked", V, "previous_r = linked(&reference_id, previous_r, &reference_batch.blocks)?;", "previous_r = linked(&reference_id, None, &reference_batch.blocks)?;"),
    ("verify: duplicates only checked per block", V, "let found_c = scan_one(&candidate_id, c_block, &mut seen_c)?;", "let found_c = scan_one(&candidate_id, c_block, {\n                seen_c.clear();\n                &mut seen_c\n            })?;"),
    ("verify: identical despite different block hashes", V, "report.identical = report.differences.is_empty() && report.block_hash_mismatches.is_empty();", "report.identical = report.differences.is_empty();"),
    ("oracle: tweaks_only may be absent", O, "    // Required: an answer without it is not from an oracle this adapter knows.\n    tweaks_only: bool,", "    #[serde(default)]\n    tweaks_only: bool,"),
    ("oracle: error text from /info unbounded", O, 'SourceError::InvalidData(format!("oracle /info: {}", printable(&e.to_string())))', 'SourceError::InvalidData(format!("oracle /info: {}", e))'),
    ("core: error text from chaininfo unbounded", C, "super::http::printable(&e.to_string())", "e.to_string()"),
    ("core: range lookup validates per block", C, "    fn block_refs(&self, from: u32, to: u32) -> Result<Vec<BlockRef>, SourceError> {\n        self.verified_refs(from, to)\n    }", "    fn block_refs(&self, from: u32, to: u32) -> Result<Vec<BlockRef>, SourceError> {\n        (from..=to).map(|height| self.block_ref(height)).collect()\n    }"),
    # --- persistent sync ---
    # --- audit of a store against a reference ---
    ("audit: binding unchecked", A, "if let Some(d) = binding_difference(&meta, reference.chain(), None, scanner, true) {", "if let Some(d) = binding_difference(&meta, reference.chain(), None, scanner, true).filter(|_| false) {"),
    ("audit: empty range accepted", A, "if first > last || first < meta.birthday || last > checkpoint.height {", "if first < meta.birthday || last > checkpoint.height {"),
    ("audit: range below the birthday accepted", A, "if first > last || first < meta.birthday || last > checkpoint.height {", "if first > last || last > checkpoint.height {"),
    ("audit: range above the checkpoint accepted", A, "if first > last || first < meta.birthday || last > checkpoint.height {", "if first > last || first < meta.birthday {"),
    ("audit: reference batch not checked", A, "        check_batch(\n            &reference_id,\n            &batch.blocks,\n            next,\n            chunk_last,\n            report.reference_evidence,\n        )?;", "        let _ = check_batch;"),
    ("audit: reference chunks not linked", A, "if first_block.block.prev_hash != prev.hash {", "if false {"),
    ("audit: duplicates only checked per block", A, "                &mut seen,\n            )?;", "                {\n                    seen.clear();\n                    &mut seen\n                },\n            )?;"),
    ("audit: detection of another block accepted", A, "if let Some(d) = stored.iter().find(|d| d.block != stored_block) {", "if let Some(d) = stored.iter().find(|_| false) {"),
    ("audit: stored block hash not compared", A, "let same_block = stored_block == r_block.block;", "let same_block = true;"),
    ("audit: detections outside the stored blocks accepted", A, "if !(meta.birthday..=checkpoint.height).contains(&d.block.height) {", "if false {"),
    ("audit: output listed twice accepted", A, "if !listed.insert(key(&d)) {", "if !listed.insert(key(&d)) && false {"),
    ("engine: evidence of another kind than declared accepted", E, "    if let Some(other) = blocks.iter().find(|b| b.evidence.kind() != declared) {", "    if let Some(other) = blocks.iter().find(|_| false) {"),
    ("audit: value differences ignored", A, "} else if s.value_sats != r.value_sats || s.label != r.label {", "} else if s.label != r.label {"),
    ("audit: label differences ignored", A, "} else if s.value_sats != r.value_sats || s.label != r.label {", "} else if s.value_sats != r.value_sats {"),
    ("audit: identical despite different block hashes", A, "\n        && report.block_hash_mismatches.is_empty()", ""),
    ("audit: identical despite differences", A, "    report.identical = report.differences.is_empty()\n        && ", "    report.identical = "),
    # --- scanning a store's range again into a fresh store ---
    ("rescan: binding unchecked", R, "        scanner,\n        false,\n    ) {\n        return Err(RescanError::BindingMismatch {", "        scanner,\n        false,\n    )\n    .filter(|_| false)\n    {\n        return Err(RescanError::BindingMismatch {"),
    ("rescan: another backend accepted", R, "        Some(source.backend_id()),\n        scanner,\n        false,", "        None,\n        scanner,\n        false,"),
    ("rescan: removed labels accepted", R, "if !meta.labels.iter().all(|m| labels.contains(m)) {", "if false {"),
    ("rescan: fresh store of another binding accepted", R, "Some(existing) if existing == fresh_meta => {}", "Some(_existing) if true => {}"),
    ("rescan: unfinished rescan accepted", R, "if !checkpoint.is_some_and(|cp| cp.height >= previous.height) {", "if false {"),
    ("rescan: checkpoint on another chain accepted", R, "if fresh.block_at(previous.height).map_err(of_fresh)? != Some(previous) {", "if false {"),
    ("rescan: lost detections accepted", R, "if !not_reproduced.is_empty() {", "if false {"),
    ("rescan: altered detections accepted", R, ".filter(|d| by_outpoint.get(&OutPoint::new(d.txid, d.vout)) != Some(d))", ".filter(|d| !by_outpoint.contains_key(&OutPoint::new(d.txid, d.vout)))"),
    ("rescan: fresh store keeps the old scanner version", R, "        scanner_version: SCANNER_VERSION.to_string(),\n        ..meta.clone()", "        ..meta.clone()"),
    # --- spends of detected outputs ---
    ("store: spend in a block that is not appended accepted", T, "        if !blocks.contains(&s.spent_in) {", "        if false {"),
    ("store: output spent by its own transaction accepted", T, "        if s.spending_txid == s.txid {", "        if false {"),
    ("store: spend of an undetected output accepted", T, "            .or_else(|| stored_height(outpoint));", "            .or(Some(0));"),
    ("store: spend before the output exists accepted", T, "            Some(height) if height > s.spent_in.height => {", "            Some(height) if false && height > s.spent_in.height => {"),
    ("store: output spent twice in one call accepted", T, "        if !spent.insert(outpoint) || already_spent(outpoint) {", "        if !spent.insert(outpoint) && false || already_spent(outpoint) {"),
    ("store: output spent twice across calls accepted", T, "        if !spent.insert(outpoint) || already_spent(outpoint) {", "        if !spent.insert(outpoint) {"),
    ("engine: spends of detected outputs not recognised", E, "                if is_detected(&input.previous_output) {", "                if false {"),
    ("sync: spends not recorded", S, "        store.append(&blocks, &detections, &spends)?;", "        store.append(&blocks, &detections, &[])?;"),
    ("sync: a chunk's own detections not checked for spends", S, "        detected.extend(detections.iter().map(|d| OutPoint::new(d.txid, d.vout)));\n", ""),
    ("sync: stored detections not checked for spends", S, "        Some(_) => store\n            .detections()?\n            .iter()\n            .map(|d| OutPoint::new(d.txid, d.vout))\n            .collect(),", "        Some(_) => BTreeSet::new(),"),
    ("sync: reverted spends not reported", S, "    spends_reverted.retain(|s| s.spent_in.height > fork_height);", "    spends_reverted.clear();"),
    ("audit: spends not compared", A, "            if report.spends_compared && same_block {", "            if false {"),
    ("audit: spends by different transactions called equal", A, "                        (Some(s), Some(r)) if s == r => {", "                        (Some(s), Some(r)) if s.txid == r.txid => {"),
    ("audit: spend of an output the store does not list accepted", A, "        if !listed.contains(&outpoint)\n            || ", "        if "),
    ("audit: spend listed twice accepted", A, "\n            || !spent_once.insert(outpoint)", ""),
    ("audit: identical despite spend differences", A, "\n        && report.spend_differences.is_empty();", ";"),
    ("audit: spends of index-detected outputs compared", A, "        .filter(|d| d.provenance.evidence_kind == EvidenceKind::FullBlockWithPrevouts)\n        .map(key)", "        .map(key)"),
    ("rescan: lost spends accepted", R, "    if !spends_lost.is_empty() {", "    if false {"),
    ("sqlite: stray spends not detected on open", Q, "        if stray_spends != 0 {", "        if false {"),
    ("sqlite replace: lost spends accepted", Q, "if let Some(lost) = old.spends()?.iter().find(|s| !spent.contains(s)) {", "if let Some(lost) = old.spends()?.iter().find(|_| false) {"),
    # --- what a wallet needs to spend (feature `spend`) ---
    ("spend: detection resting on an index accepted", SP, "    if detection.provenance.evidence_kind != EvidenceKind::FullBlockWithPrevouts {", "    if false {"),
    ("spend: another network accepted", SP, "    if source.chain().network() != detection.network {", "    if false {"),
    ("spend: another block at the height accepted", SP, "    if block.block != detection.block {", "    if false {"),
    ("spend: amount not compared", SP, ".filter(|m| m.amount.to_sat() == detection.value_sats && m.label == detection.label)", ".filter(|m| m.label == detection.label)"),
    ("spend: label not compared", SP, ".filter(|m| m.amount.to_sat() == detection.value_sats && m.label == detection.label)", ".filter(|m| m.amount.to_sat() == detection.value_sats)"),
    # --- wallet ---
    # Not listed, because no fast test can make them fail: in the signing
    # module, the derived key against the input's script, the signature
    # verification and the two fee re-checks (they guard against the
    # libraries being wrong); in the CLI, everything that is only reached
    # with funds on a real node (the typed confirmation, the "caught up"
    # requirement, asking the node first, recording before sending). The
    # node-backed tests in crates/wallet-cli/tests/regtest_wallet.rs cover
    # the latter.
    ("wallet: payment below the relay minimum accepted", W, "        if payment.amount_sats < dust {", "        if false {"),
    ("wallet: zero fee rate accepted", W, "    if fee_rate == 0 || fee_rate > limits.max_fee_rate {", "    if fee_rate > limits.max_fee_rate {"),
    ("wallet: fee rate above the limit accepted", W, "    if fee_rate == 0 || fee_rate > limits.max_fee_rate {", "    if fee_rate == 0 {"),
    ("wallet: fee above the limit accepted", W, "    if fee_sats > limits.max_fee_sats {", "    if false {"),
    ("wallet: input limit ignored", W, "            if selected.len() == limits.max_inputs {", "            if false {"),
    ("wallet: payment without enough inputs built", W, "        Ok((!selected.is_empty() && gathered >= needed).then_some(selected))", "        Ok((!selected.is_empty()).then_some(selected))"),
    ("wallet: address of another network accepted", W, "            .require_network(network.to_bitcoin())\n            .map_err(|_| SendError::Destination(format!(\"not a {network} address\")))?;", "            .assume_checked();"),
    ("wallet: amount above the total supply accepted", W, "    let add = |a: u64, b: u64| a.checked_add(b).filter(|sum| *sum <= max_money);", "    let add = |a: u64, b: u64| a.checked_add(b).filter(|_| true);"),
    ("wallet: the same output taken twice", W, "        if !taken.insert(outpoint) {", "        if !taken.insert(outpoint) && false {"),
    ("wallet: outputs worth less than their fee spent", W, "        if candidate.value_sats <= cost_of_adding {", "        if false {"),
    ("wallet: fee large next to the payment accepted", W, "    if u128::from(fee_sats) * 100 > u128::from(total) * u128::from(limits.max_fee_percent) {", "    if false {"),
    ("wallet: change of exactly the relay minimum dropped", W, "        .filter(|left| *left >= dust)\n", "        .filter(|left| *left > dust)\n"),
    ("wallet: change the wallet cannot find is sent", W, "        if !detected {", "        if false {"),
    ("wallet: change label handed out as an address", WK, "            Some(0) => Err(ProtocolError::Address(\n                \"label 0 is the change label; it is not handed out\".to_string(),\n            )),\n", ""),
    ("wallet: refused credentials not recognised", WR, "        if status == 401 || status == 403 {", "        if false {"),
    ("wallet: error answer of the node ignored", WR, "        if !answer[\"error\"].is_null() {", "        if false {"),
    ("wallet: answer under a failure status accepted", WR, "        if status != 200 {", "        if false {"),
    ("wallet: verdict of the node on a transaction ignored", WR, "        if verdict[\"allowed\"].as_bool() == Some(true) {", "        if true {"),
    ("wallet: transaction id reported by the node not compared", WR, "        if reported != transaction.compute_txid() {", "        if false {"),
    ("wallet: cookie file that is not a cookie accepted", WR, "        if !contents.contains(':') || contents.contains(['\\n', '\\r']) || contents.len() > 512 {", "        if false {"),
    ("wallet: node on another chain accepted", WR, "        if reported == genesis {", "        if true {"),
    ("wallet: lost answer treated as not sent", WR, "                    | RpcError::WrongChain\n            )", "                    | RpcError::WrongChain\n                    | RpcError::NoAnswer(_)\n                    | RpcError::Malformed(_)\n            )"),
    ("wallet: confirmed transaction forgotten at once", WP, "                    if checkpoint.saturating_sub(height) >= KEEP_CONFIRMED_FOR {", "                    if true {"),
    ("wallet: transaction of an abandoned block not put back on hold", WP, "                (Some(_), None) => {\n                    t.confirmed_at = None;", "                (Some(_), None) => {"),
    ("wallet: input spent by another transaction not noticed", WP, "            if recorded.iter().any(|s| s.spending_txid != t.txid) {", "            if false {"),
    ("wallet cli: store for another network than the node's created", WC, "    if source.chain().network() != network {", "    if false {"),
    ("wallet: index claims counted as unspent", WH, "        if output.provenance.evidence_kind != EvidenceKind::FullBlockWithPrevouts {", "        if false {"),
    ("wallet: spent outputs counted as unspent", WH, "        if spent.contains(&outpoint) {", "        if false {"),
    ("wallet: inputs of pending transactions counted as unspent", WH, "        if waiting.contains(&outpoint) {", "        if false {"),
    ("wallet: outputs without enough confirmations counted", WH, "        } else if confirmations >= min_confirmations {", "        } else if true {"),
    ("wallet: RPC to a non-loopback address accepted", WR, ".filter(|address| address.ip().is_loopback() && *address.ip() != Ipv4Addr::UNSPECIFIED)", ".filter(|_| true)"),
    ("wallet: seed of any length accepted", WK, "        if !(16..=64).contains(&seed.len()) {", "        if false {"),
    ("wallet cli: store of another seed accepted", WC, "    if wallet.scanner().wallet_id() != meta.wallet_id {", "    if false {"),
    # --- mainnet: off unless asked for, per crate ---
    ("mainnet: named without the feature", N, "    Err(NetworkError::MainnetRefused)\n}", "    Ok(Network::Regtest)\n}"),
    ("mainnet: regtest's proof of work limit used", B, "Network::Mainnet => Target::MAX_ATTAINABLE_MAINNET,", "Network::Mainnet => Target::MAX_ATTAINABLE_REGTEST,", SDK_MAINNET),
    ("mainnet: lab CLI accepts it when the SDK can name it", M, "    if network.is_mainnet() {", "    if false {", ["-p", "lab-cli"] + UNIFIED),
    ("mainnet: wallet made without the wallet's feature", WK, "        if network.is_mainnet() && !MAINNET_ENABLED {", "        if false {", ["-p", "silent-wallet"] + UNIFIED),
    ("mainnet: wallet CLI accepts it without its feature", WC, "    if network.is_mainnet() && !MAINNET_ENABLED {", "    if false {", ["-p", "wallet-cli"] + UNIFIED),
    ("mainnet: test network keys derived", WK, "        let coin = if network.is_mainnet() { 0 } else { 1 };", "        let coin = 1;", ["-p", "silent-wallet", "--features", "mainnet"]),
    ("mainnet: wallet CLI has no default amount limit", WC, "        .is_mainnet()\n        .then_some(MAINNET_DEFAULT_MAX_AMOUNT_SATS))", "        .is_mainnet()\n        .then_some(u64::MAX))", ["-p", "wallet-cli", "--features", "mainnet"]),
    # Not listed, because no fast test can reach them (they need a mainnet
    # node with blocks, or a confirmed send): the mainnet confirmation phrase
    # of the wallet CLI, and the client's default amount limit on mainnet
    # (covered by the node test `a_mainnet_wallet_opens_on_a_mainnet_node`).
    # --- the one-stop client ---
    ("client: amount limit ignored", CL, "            if amount_sats > limit_sats {", "            if false {"),
    ("client: node on another network accepted", CL, "        if source.chain().network() != network {", "        if false {"),
    ("client: store of another phrase opened", CL, "            if wallet.scanner().wallet_id() != meta.wallet_id {", "            if false {"),
    ("client: labels of the store not compared", CL, "            if !asked.is_empty() && asked != stored {", "            if false {"),
    ("client: label the store does not scan handed out", CL, "            if !self.meta.labels.contains(&m) {", "            if false {"),
    ("client: phrase of another length accepted", CL, "        if !matches!(phrase.word_count(), 12 | 24) {", "        if false {"),
    ("client: entropy of another size accepted", CL, "        if !matches!(entropy.len(), 16 | 32) {", "        if false {"),
    # Not listed (node tests only, `receive_and_send_between_two_wallets_on_a_node`):
    # the stale-transaction check of `send`, and the pending record written
    # before a transaction leaves.
    ("privacy: lock time left at zero", W, "    let lock_time = lock_time_like_core(tip.height)?;", "    let lock_time = LockTime::ZERO;"),
    ("privacy: lock time above the tip", W, "    let mut height = tip_height;", "    let mut height = tip_height + 1;"),
    ("privacy: outputs left in the order built (change last)", W, "    shuffle(&mut place)?;\n", ""),
    ("privacy: inputs left in the wallet's order", W, "    shuffle(&mut selected)?;\n", ""),
    ("privacy: labels not kept apart", W, "            .find(|(label, _)| *label == candidate.label)", "            .find(|_| true)"),
    ("privacy: spending labels together not reported", W, "    let mixes_labels = selected.iter().any(|d| d.label != selected[0].label);", "    let mixes_labels = false;"),
    ("privacy: no search for a payment without change", W, "const EXACT_SEARCH_TRIES: u32 = 100_000;", "const EXACT_SEARCH_TRIES: u32 = 0;"),
    ("privacy: change made although inputs were chosen to leave none", W, "        .filter(|_| !exact);", "        .filter(|_| true);"),
    ("privacy: fee limits ignored when leaving no change", W, "                    if extra <= cost_of_change && fee_within_limits(sum.saturating_sub(total)) =>", "                    if extra <= cost_of_change =>"),
    ("privacy: output held back still offered", "crates/silent-wallet/src/holdings.rs", "        } else if pending.held_back().contains(&outpoint) {", "        } else if false {"),
    ("wallet: source on another network signed for", W, "    if source.chain().network() != network {", "    if false {"),
    ("wallet: output of another network signed for", W, "    if let Some(foreign) = candidates.iter().find(|d| d.network != network) {", "    if let Some(foreign) = candidates.iter().find(|_| false) {"),
    ("wallet: transaction the node already has reported as not sent", WR, "        !self.means_already_with_node()\n            && matches!(", "        matches!("),
    ("client: change label in the configuration refused", CL, "            asked.retain(|&m| m != 0);\n", ""),
    ("wallet cli: amount limit ignored", WC, "        if args.amount > limit {", "        if false {"),
    ("sync: zero chunk size accepted", S, "if options.chunk_size == 0 {", "if false {"),
    ("sync: binding chain unchecked", S, "if meta.chain != chain {", "if false {"),
    ("sync: binding wallet unchecked", S, "if meta.wallet_id != wallet_id {", "if false {"),
    ("sync: binding backend unchecked", S, "if meta.backend_id != backend_id {", "if false {"),
    ("sync: binding labels unchecked", S, "if meta.labels != labels {", "if false {"),
    ("sync: binding scanner version unchecked", S, "if meta.scanner_version != SCANNER_VERSION {", "if false {"),
    ("sync: reorg never detected", S, "    if on_chain(checkpoint.height, checkpoint)? {\n        return Ok(None);", "    if true {\n        return Ok(None);"),
    ("sync: lagging source reported as a competing chain", S, "        return Err(if merely_behind {", "        return Err(if false {"),
    ("sync: source below the birthday reported as a competing chain", S, "            || source_tip.height < meta.birthday;", "            || false;"),
    ("sync: shorter competing chain reported as lagging", S, "        return Err(if merely_behind {", "        return Err(if true {"),
    ("sync: shorter competing chain followed", S, "    if checkpoint.height > source_tip.height {\n        // The source's chain ends below", "    if false {\n        // The source's chain ends below"),
    ("sync: birthday 0 accepted", T, "    let meta = store.meta()?.ok_or(StoreError::NotInitialized)?;\n    if meta.birthday == 0 {", "    let meta = store.meta()?.ok_or(StoreError::NotInitialized)?;\n    if false {"),
    ("sync: target above tip accepted", S, "Some(target) if target > source_tip.height => {", "Some(target) if false && target > source_tip.height => {"),
    ("sync: chunk not linked to checkpoint", S, "if first.block.prev_hash != cp.hash {", "if false {"),
    ("sync: batch not re-checked", S, "        check_batch(\n            backend_id,\n            &batch.blocks,\n            next,\n            last,\n            source.capabilities().evidence_kind,\n        )?;", "        let _ = check_batch;"),
    ("sync: rollback before a failure not reported", S, "let changed = report.reorg.is_some() || report.blocks_scanned > 0;", "let changed = report.blocks_scanned > 0;"),
    ("sync: commits before a failure not reported", S, "let changed = report.reorg.is_some() || report.blocks_scanned > 0;", "let changed = report.reorg.is_some();"),
    ("sync: tip on another block reported as caught up", S, ".is_some_and(|cp| cp.height == source_tip.height && cp.hash == source_tip.hash);", ".is_some_and(|cp| cp.height == source_tip.height);"),
    ("sync: birthday above tip not named", S, "if meta.birthday > report.source_tip.height {", "if false {"),
    ("sync: target below tip not named", S, "return Ok(if target < report.source_tip.height {", "return Ok(if false {"),
    ("sync: max blocks ignored", S, ".is_some_and(|max| report.blocks_scanned >= max)", ".is_some_and(|_| false)"),
    ("sync: checkpoint not advanced after a commit", S, "            report.checkpoint = Some(*last);\n", ""),
    ("sync: progress callback not called", S, "on_commit(*last);", "let _ = (last, &on_commit);"),
    ("sync: reorg depth limit ignored", S, "Some(limit) if depth > limit => Err(", "Some(limit) if false && depth > limit => Err("),
    ("sync: reorg exactly at the limit refused", S, "Some(limit) if depth > limit => Err(", "Some(limit) if depth >= limit => Err("),
    ("sync: reorg depth unchecked while walking down", S, "        too_deep(height)?;\n", ""),
    ("sync: reorg below the birthday not counted", S, "            too_deep(meta.birthday - 1)?;\n", ""),
    ("sync: no reorg depth limit by default", S, "max_reorg_depth: Some(DEFAULT_MAX_REORG_DEPTH),", "max_reorg_depth: None,"),
    # --- store invariants ---
    ("store: append need not extend checkpoint", T, "if u64::from(first.height) != u64::from(cp.height) + 1 || first.prev_hash != cp.hash {", "if false {"),
    ("store: first block need not be birthday", T, "if first.height != meta.birthday {", "if false {"),
    ("store: blocks in a call need not link", T, "if u64::from(b.height) != u64::from(a.height) + 1 || b.prev_hash != a.hash {", "if false {"),
    ("store: detection network unchecked", T, "if d.network != meta.chain.network() {", "if false {"),
    ("store: detection block membership unchecked", T, "if !blocks.contains(&d.block) {", "if false {"),
    ("store: duplicate outputs accepted", T, "if !seen.insert(outpoint) || stored_height(outpoint).is_some() {", "if (!seen.insert(outpoint) || stored_height(outpoint).is_some()) && false {"),
    ("store: empty append accepted", T, '.ok_or_else(|| invariant("append of zero blocks".to_string()))?;', '.copied().unwrap_or(BlockRef { height: meta.birthday, hash: meta.chain.genesis_hash(), prev_hash: meta.chain.genesis_hash() });\n    if blocks.is_empty() { return Ok(()); }'),
    ("store: self-parent block accepted", T, "if b.hash == b.prev_hash {", "if false {"),
    ("store: block hash repeated within a call accepted", T, "if !hashes.insert(b.hash) || block_hash_stored(b.hash) {", "if (!hashes.insert(b.hash) && false) || block_hash_stored(b.hash) {"),
    ("store: stored block hash accepted again", T, "if !hashes.insert(b.hash) || block_hash_stored(b.hash) {", "if !hashes.insert(b.hash) {"),
    ("store: unrepresentable value accepted", T, "if d.value_sats > MAX_STORABLE_SATS {", "if false {"),
    ("store: detection from another backend accepted", T, "if d.provenance.backend_id != meta.backend_id {", "if false {"),
    ("store: order ignores the txid", T, "detections.sort_by_key(|d| (d.block.height, d.txid, d.vout));", "detections.sort_by_key(|d| (d.block.height, d.vout));"),
    ("store: order by txid hex text", T, "detections.sort_by_key(|d| (d.block.height, d.txid, d.vout));", "detections.sort_by_key(|d| (d.block.height, d.txid.to_string(), d.vout));"),
    ("store: memory store lists detections uninitialized", T, "        if self.meta.is_none() {\n            return Err(StoreError::NotInitialized);\n        }\n        Ok(self.detections.clone())", "        Ok(self.detections.clone())"),
    # --- SQLite store ---
    ("sqlite: existing file overwritten", Q, "    options.write(true).create_new(true);", "    options.write(true).create(true);"),
    ("sqlite: schema version unchecked", Q, "Some(v) if v == SCHEMA_VERSION.to_string() => Ok(()),", "_ if true => Ok(()),"),
    ("sqlite: double initialize accepted", Q, "if read_meta(&tx)?.is_some() {", "if false {"),
    ("sqlite: zero birthday accepted", Q, "if meta.birthday == 0 {", "if false {"),
    ("sqlite: file permissions not restricted", Q, "        options.mode(0o600);", "        options.mode(0o644);"),
    ("sqlite: order by txid hex text", Q, "    sort_detections(&mut detections);", "    detections.sort_by_key(|d| (d.block.height, d.txid.to_string(), d.vout));\n    let _ = sort_detections;"),
    ("sqlite: integrity not checked on open", Q, "        store.check_integrity()?;\n", ""),
    ("sqlite: chain need not start at the birthday", Q, "if min != i64::from(meta.birthday) || max - min + 1 != count {", "if max - min + 1 != count {"),
    ("sqlite: gap in the stored chain accepted", Q, "if min != i64::from(meta.birthday) || max - min + 1 != count {", "if min != i64::from(meta.birthday) {"),
    ("sqlite: broken hash link accepted", Q, "if broken != 0 || self_parent != 0 {", "if self_parent != 0 {"),
    ("sqlite: self-parent block accepted on open", Q, "if broken != 0 || self_parent != 0 {", "if broken != 0 {"),
    ("sqlite: detections without their block accepted", Q, "if orphans != 0 {", "if false {"),
    ("sqlite: detections of another backend accepted", Q, "if foreign != 0 {", "if false {"),
    ("sqlite: blocks without a binding accepted", Q, "return if count == 0 {", "return if true {"),
    ("sqlite: binding with birthday 0 read", Q, "if meta.as_ref().is_some_and(|m| m.birthday == 0) {", "if false {"),
    ("sqlite: unreadable binding treated as none", Q, '        .map(|j| serde_json::from_str(&j).map_err(|_| corrupt("binding")))\n        .transpose()?;', "        .and_then(|j| serde_json::from_str(&j).ok());"),
    ("sqlite: append does not take the write lock up front", Q, "            .transaction_with_behavior(TransactionBehavior::Immediate)\n            .map_err(backend)?;\n        let meta = read_meta(&tx)?;\n        let checkpoint = read_checkpoint(&tx)?;", "            .transaction_with_behavior(TransactionBehavior::Deferred)\n            .map_err(backend)?;\n        let meta = read_meta(&tx)?;\n        let checkpoint = read_checkpoint(&tx)?;"),
    ("sqlite: no busy timeout", Q, "conn.busy_timeout(Duration::from_secs(10))", "conn.busy_timeout(Duration::from_secs(0))"),
    ("sqlite: stored block hash not pre-checked", Q, "            if exists.is_some() {\n                stored_hashes.insert(b.hash);", "            if false {\n                stored_hashes.insert(b.hash);"),
    ("sqlite: negative vout wrapped", Q, '                    vout: u32::try_from(vout).map_err(|_| corrupt("vout"))?,', "                    vout: vout as u32,"),
    ("sqlite: negative value wrapped", Q, 'value_sats: u64::try_from(value).map_err(|_| corrupt("value"))?,', "value_sats: value as u64,"),
    ("sqlite: negative label wrapped", Q, '                        .map(|m| u32::try_from(m).map_err(|_| corrupt("label")))\n                        .transpose()?,', "                        .map(|m| m as u32),"),
    ("sqlite replace: chain not compared", Q, "            &old_meta.chain,\n", "            &new_meta.chain,\n"),
    ("sqlite replace: receiver not compared", Q, "            &old_meta.wallet_id,\n", "            &new_meta.wallet_id,\n"),
    ("sqlite replace: backend not compared", Q, "            &old_meta.backend_id,\n", "            &new_meta.backend_id,\n"),
    ("sqlite replace: birthday not compared", Q, "            old_meta.birthday,\n", "            new_meta.birthday,\n"),
    ("sqlite replace: labels not compared", Q, "if !old_meta.labels.iter().all(|m| new_meta.labels.contains(m)) {", "if false {"),
    ("sqlite replace: checkpoint not compared", Q, "if new.block_at(checkpoint.height)? != Some(checkpoint) {", "if false {"),
    ("sqlite replace: lost detections accepted", Q, "if let Some(lost) = old.detections()?.iter().find(|d| !held.contains(d)) {", "if let Some(lost) = old.detections()?.iter().find(|_| false) {"),
    # The owner check of a rescan file has a test that needs the right to give
    # a file away (root); it is not listed here because the test skips that
    # part otherwise.
    ("sqlite: rescan file that is a link accepted", Q, "    if !found.file_type().is_file() {", "    if false {"),
    ("sqlite: rescan file accessible to others accepted", Q, "        if found.mode() & 0o077 != 0 {", "        if false {"),
    ("sqlite replace: rescan file not checked", Q, "    check_private_file_of_the_store_owner(fresh, current)?;\n    {\n        let old", "    {\n        let old"),
    # --- benchmark records ---
    ("bench: restore mismatch not flagged", K, "mode_check_passed: original.is_none_or(|held| held == detections),", "mode_check_passed: true,"),
    ("bench: failed runs counted in the statistics", K, "                .filter(|r| r.error.is_none())\n", "                .filter(|_| true)\n"),
    ("bench: differing runs not counted", K, ".filter(|r| r.verification_status == Verification::DiffersFromReference)", ".filter(|_| false)"),
    ("bench: cpu time read from the wrong fields", K, ".skip(11);", ".skip(9);"),
    ("bench: results of other builds merged", K, "if environment != base {", "if false {"),
    ("bench: error text recorded instead of its class", K, "record.error = Some(e.class.to_string());", "record.error = Some(e.detail.clone());"),
    ("bench: same-result column always yes", K, "if s.mismatches == 0 && s.errors == 0 {", "if true {"),
    ("cli: bench succeeds despite failed or differing runs", M, "if failed > 0 || differing > 0 {", "if false {"),
    # --- CLI ---
    ("cli: interrupted sync hides what it changed", M, "            show(&partial)?;\n", ""),
    ("cli: control characters in the birthday basis accepted", M, "if birthday_basis.chars().any(char::is_control) || birthday_basis.chars().count() > 500 {", "if birthday_basis.chars().count() > 500 {"),
    ("cli: unbounded birthday basis accepted", M, "if birthday_basis.chars().any(char::is_control) || birthday_basis.chars().count() > 500 {", "if birthday_basis.chars().any(char::is_control) {"),
    ("cli: source chain not compared with --network", M, "    if source.chain() != expected {", "    if false {"),
    ("cli: store network unchecked", M, "if meta.chain.network() != network {", "if false {"),
    ("cli: empty birthday basis accepted", M, "if birthday_basis.trim().is_empty() {", "if false {"),
    ("cli: reorg depth limit not passed on", M, "        .with_max_reorg_depth(args.max_reorg_depth.0);", "        ;"),
    ("cli: add-label with nothing to add proceeds", M, "    if add.is_some() && labels == meta.labels {", "    if false {"),
    ("cli: add-label block limit not passed on", M, "                .with_max_blocks(self.max_blocks)\n", ""),
    ("cli: reorg limit `none` read as a number", M, "        return Ok(ReorgLimit(None));", "        return Ok(ReorgLimit(Some(100)));"),
    ("cli: audit succeeds despite differences", M, "    if audit.identical {\n        Ok(())", "    if true {\n        Ok(())"),
    ("cli: audit first height not passed on", M, "        .with_first_height(args.from)\n", ""),
    ("cli: audit last height not passed on", M, "        .with_last_height(args.to);", "        ;"),
    ("cli: verify succeeds despite differences", M, "    if report.identical {\n        Ok(())", "    if true {\n        Ok(())"),
    ("cli: doctor succeeds with an unusable backend", M, "    if usable {\n        Ok(())", "    if true {\n        Ok(())"),
]
survived = []
if only:
    muts = [m for m in muts if any(m[0].startswith(prefix) for prefix in only)]
    print(f"running {len(muts)} selected entries: {only}")
for name, path, old, new, *scope in muts:
    scope = scope[0] if scope else ["--workspace"]
    full = os.path.join(root, path)
    src = open(full).read()
    if src.count(old) != 1:
        print(f"SETUP-ERROR  {name}: pattern count {src.count(old)}"); survived.append(name + " (setup)"); continue
    open(full, "w").write(src.replace(old, new))
    r = subprocess.run(["cargo", "test", "--locked", "-q", *scope, "--", "--skip", "k_max"], cwd=root, env=env, capture_output=True, text=True, timeout=1800)
    open(full, "w").write(src)
    out = r.stdout + r.stderr
    if r.returncode == 0:
        print(f"SURVIVED     {name}"); survived.append(name)
    elif "error[" in out or "could not compile" in out:
        print(f"BUILD-ERROR  {name}"); survived.append(name + " (build)")
    else:
        failed = sorted(
            line.strip()
            for line in out.splitlines()
            if line.startswith("    ") and " " not in line.strip() and line.strip()
        )
        print(f"killed       {name}  <- {', '.join(failed)[:160]}")
print("\nSURVIVORS:", survived or "none")
sys.exit(1 if survived else 0)
