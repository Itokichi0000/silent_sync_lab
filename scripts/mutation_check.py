#!/usr/bin/env python3
"""Mutation check for the validation code in sync-core.

Each entry disables exactly one check. For every entry the script applies the
change to a *scratch copy* of the repository, runs the fast test suite and
reports whether some test failed ("killed") or none did ("SURVIVED"). A
survivor means a check exists that no test would miss if it were removed.

Usage (never run it against the working tree itself):

    rm -rf /tmp/ssl-mut && mkdir -p /tmp/ssl-mut/repo
    git ls-files -co --exclude-standard | tar cf - -T - | tar xf - -C /tmp/ssl-mut/repo
    python3 scripts/mutation_check.py /tmp/ssl-mut/repo

The slow K_max tests are skipped (`--skip k_max`); the K_max correction is
covered by `crates/sync-core/tests/bip352_vectors.rs` instead. Exit status is
non-zero if anything survived. The patterns are tied to the current source
text: when a pattern no longer matches, the entry is reported as SETUP-ERROR
and must be updated together with the code.
"""
import os
import subprocess
import sys

if len(sys.argv) != 2 or not os.path.isfile(os.path.join(sys.argv[1], "Cargo.toml")):
    sys.exit(__doc__)
root = os.path.abspath(sys.argv[1])
if os.path.isdir(os.path.join(root, ".git")):
    sys.exit("refusing to mutate a git working tree; use a scratch copy (see usage)")
env = dict(
    os.environ,
    CARGO_TARGET_DIR=os.path.join(os.path.dirname(root), "mut-target"),
    RUST_BACKTRACE="0",
)
B = "crates/sync-core/src/source/block_check.rs"
C = "crates/sync-core/src/source/bitcoin_core.rs"
H = "crates/sync-core/src/source/http.rs"
O = "crates/sync-core/src/source/blindbit.rs"
V = "crates/sync-core/src/verify.rs"
E = "crates/sync-core/src/engine.rs"
P = "crates/sync-core/src/protocol.rs"
N = "crates/sync-core/src/network.rs"
S = "crates/sync-core/src/sync.rs"
T = "crates/sync-core/src/store.rs"
Q = "crates/lab-storage/src/lib.rs"
M = "crates/lab-cli/src/main.rs"
muts = [
    # --- block structure checks shared by the fixture and Core backends ---
    ("pow check removed", B, "    block.header.validate_pow(target).map_err(|e| e.to_string())", "    Ok(block.block_hash())"),
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
    ("http: non-200 success statuses accepted", H, "if status != 200 {", "if status >= 400 {"),
    ("http: response size unbounded", H, "if body.len() as u64 > self.max_body_bytes {", "if false {"),
    # --- Bitcoin Core REST backend ---
    ("core: mainnet chain name accepted", C, '"main" => Err(NetworkError::MainnetRefused.into()),', '"main" => Ok(LabNetwork::Regtest),'),
    ("core: node's block 0 not compared with genesis", C, "ChainIdentity::verified(network, genesis)?;", "let _ = genesis;"),
    ("core: header chain links unchecked", C, "if header.prev_blockhash != parent {", "if false {"),
    ("core: header proof of work unchecked", C, "let hash = check_proof_of_work(header, network)", "let hash = Ok::<_, String>(header.block_hash())"),
    ("core: header response need not start at the anchor", C, "Some(first) if first.block_hash() == top => {}", "Some(_) if true => {}"),
    ("core: stale header anchor kept after a reorg", C, "            anchored.pop();", "            break;"),
    ("core: trailing bytes after block accepted", C, "if !cursor.is_empty() {\n                return Err(at(", "if false {\n                return Err(at("),
    ("core: trailing bytes after spent outputs accepted", C, "if !cursor.is_empty() {\n        return Err(format!(", "if false {\n        return Err(format!("),
    ("core: spent-output transaction count unbounded", C, "if count > MAX_TXS_PER_BLOCK {", "if false {"),
    ("core: range beyond tip not refused", C, "if from < 1 || to > tip {", "if false {"),
    ("core: chain switch at tip() unnoticed", C, "if network_from_core_chain(&info.chain)? != self.chain.network() {", "if false {"),
    ("core: tip not bound to the header chain", C, "if tip.hash != info.bestblockhash {", "if false {"),
    ("core: block not checked against requested hash", C, "hash: Some(hash),", "hash: None,"),
    # --- BlindBit Oracle backend ---
    ("oracle: mainnet name accepted", O, '"main" | "mainnet" => Err(NetworkError::MainnetRefused.into()),', '"main" | "mainnet" => Ok(LabNetwork::Regtest),'),
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
    ("signet mapped to testnet", N, "LabNetwork::Signet => Network::Signet,", "LabNetwork::Signet => Network::Testnet,"),
    ("chain identity deserialize unverified", N, "Self::verified(raw.network, raw.genesis_hash)", "Ok(Self { network: raw.network, genesis_hash: raw.genesis_hash })"),
    ("mainnet name accepted as regtest", N, '"regtest" => Ok(LabNetwork::Regtest),', '"regtest" | "mainnet" => Ok(LabNetwork::Regtest),'),
    # --- persistent sync ---
    ("sync: zero chunk size accepted", S, "if options.chunk_size == 0 {", "if false {"),
    ("sync: binding chain unchecked", S, "if meta.chain != source.chain() {", "if false {"),
    ("sync: binding wallet unchecked", S, "if meta.wallet_id != wallet_id {", "if false {"),
    ("sync: binding backend unchecked", S, "if meta.backend_id != source.backend_id() {", "if false {"),
    ("sync: binding labels unchecked", S, "if meta.labels != labels {", "if false {"),
    ("sync: binding scanner version unchecked", S, "if meta.scanner_version != SCANNER_VERSION {", "if false {"),
    ("sync: reorg never detected", S, "if checkpoint.height <= source_tip.height && on_chain(checkpoint.height, checkpoint)? {", "if true {"),
    ("sync: lagging source treated as reorg", S, "if stored_at_tip.is_some_and(|b| b.hash == source_tip.hash)\n            || (source_tip.height < meta.birthday)", "if false"),
    ("sync: source below birthday treated as reorg", S, "|| (source_tip.height < meta.birthday)", "|| false"),
    ("sync: target above tip accepted", S, "Some(target) if target > source_tip.height => {", "Some(target) if false && target > source_tip.height => {"),
    ("sync: chunk not linked to checkpoint", S, "if first.block.prev_hash != cp.hash {", "if false {"),
    ("sync: batch not re-checked", S, "check_batch(&backend_id, &batch.blocks, next, last)?;", "let _ = check_batch;"),
    # --- store invariants ---
    ("store: append need not extend checkpoint", T, "if u64::from(first.height) != u64::from(cp.height) + 1 || first.prev_hash != cp.hash {", "if false {"),
    ("store: first block need not be birthday", T, "if first.height != meta.birthday {", "if false {"),
    ("store: blocks in a call need not link", T, "if u64::from(b.height) != u64::from(a.height) + 1 || b.prev_hash != a.hash {", "if false {"),
    ("store: detection network unchecked", T, "if d.network != meta.chain.network() {", "if false {"),
    ("store: detection block membership unchecked", T, "if !blocks.contains(&d.block) {", "if false {"),
    ("store: duplicate outputs accepted", T, "if !seen.insert(outpoint) || already_stored(outpoint) {", "if (!seen.insert(outpoint) || already_stored(outpoint)) && false {"),
    ("store: empty append accepted", T, '.ok_or_else(|| invariant("append of zero blocks".to_string()))?;', '.copied().unwrap_or(BlockRef { height: meta.birthday, hash: meta.chain.genesis_hash(), prev_hash: meta.chain.genesis_hash() });\n    if blocks.is_empty() { return Ok(()); }'),
    # --- SQLite store ---
    ("sqlite: existing file overwritten", Q, "if path.exists() {", "if false {"),
    ("sqlite: schema version unchecked", Q, "Some(v) if v == SCHEMA_VERSION.to_string() => Ok(()),", "_ if true => Ok(()),"),
    ("sqlite: double initialize accepted", Q, "if read_meta(&tx)?.is_some() {", "if false {"),
    ("sqlite: zero birthday accepted", Q, "if meta.birthday == 0 {", "if false {"),
    ("sqlite: file permissions not restricted", Q, "std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(backend)", "let _ = (path, std::fs::Permissions::from_mode(0o600));\n    Ok(())"),
    # --- CLI ---
    ("cli: store network unchecked", M, "if meta.chain.network() != network {", "if false {"),
    ("cli: empty birthday basis accepted", M, "if birthday_basis.trim().is_empty() {", "if false {"),
    ("cli: verify succeeds despite differences", M, "    if report.identical {\n        Ok(())", "    if true {\n        Ok(())"),
    ("cli: doctor succeeds with an unusable backend", M, "    if usable {\n        Ok(())", "    if true {\n        Ok(())"),
]
survived = []
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
