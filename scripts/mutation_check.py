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
    ("core: mainnet chain name accepted", C, '"main" => Err(NetworkError::MainnetRefused.into()),', '"main" => Ok(Network::Regtest),'),
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
    ("oracle: mainnet name accepted", O, '"main" | "mainnet" => Err(NetworkError::MainnetRefused.into()),', '"main" | "mainnet" => Ok(Network::Regtest),'),
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
    ("verify: candidate batch not checked", V, "check_batch(&candidate_id, &candidate_batch.blocks, next, last)?;", "let _ = (&candidate_id, check_batch);"),
    ("verify: reference batch not checked", V, "check_batch(&reference_id, &reference_batch.blocks, next, last)?;", "let _ = (&reference_id, check_batch);"),
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
    ("audit: reference batch not checked", A, "check_batch(&reference_id, &batch.blocks, next, chunk_last)?;", "let _ = check_batch;"),
    ("audit: reference chunks not linked", A, "if first_block.block.prev_hash != prev.hash {", "if false {"),
    ("audit: duplicates only checked per block", A, "                &mut seen,\n            )?;", "                {\n                    seen.clear();\n                    &mut seen\n                },\n            )?;"),
    ("audit: detection of another block accepted", A, "if let Some(d) = stored.iter().find(|d| d.block != stored_block) {", "if let Some(d) = stored.iter().find(|_| false) {"),
    ("audit: stored block hash not compared", A, "let same_block = stored_block == r_block.block;", "let same_block = true;"),
    ("audit: detections outside the stored blocks accepted", A, "if !(meta.birthday..=checkpoint.height).contains(&d.block.height) {", "if false {"),
    ("audit: output listed twice accepted", A, "if !listed.insert(key(&d)) {", "if !listed.insert(key(&d)) && false {"),
    ("audit: served evidence kind not checked", A, "if r_block.evidence.kind() != report.reference_evidence {", "if false {"),
    ("audit: value differences ignored", A, "} else if s.value_sats != r.value_sats || s.label != r.label {", "} else if s.label != r.label {"),
    ("audit: label differences ignored", A, "} else if s.value_sats != r.value_sats || s.label != r.label {", "} else if s.value_sats != r.value_sats {"),
    ("audit: identical despite different block hashes", A, "report.identical = report.differences.is_empty() && report.block_hash_mismatches.is_empty();", "report.identical = report.differences.is_empty();"),
    ("audit: identical despite differences", A, "report.identical = report.differences.is_empty() && report.block_hash_mismatches.is_empty();", "report.identical = report.block_hash_mismatches.is_empty();"),
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
    ("sync: batch not re-checked", S, "check_batch(backend_id, &batch.blocks, next, last)?;", "let _ = check_batch;"),
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
    ("store: duplicate outputs accepted", T, "if !seen.insert(outpoint) || already_stored(outpoint) {", "if (!seen.insert(outpoint) || already_stored(outpoint)) && false {"),
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
    ("sqlite: negative vout wrapped", Q, 'vout: u32::try_from(vout).map_err(|_| corrupt("vout"))?,', "vout: vout as u32,"),
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
for name, path, old, new in muts:
    full = os.path.join(root, path)
    src = open(full).read()
    if src.count(old) != 1:
        print(f"SETUP-ERROR  {name}: pattern count {src.count(old)}"); survived.append(name + " (setup)"); continue
    open(full, "w").write(src.replace(old, new))
    r = subprocess.run(["cargo", "test", "--locked", "-q", "--workspace", "--", "--skip", "k_max"], cwd=root, env=env, capture_output=True, text=True, timeout=1800)
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
