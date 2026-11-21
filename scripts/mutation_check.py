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
F = "crates/sync-core/src/source/fixture.rs"
E = "crates/sync-core/src/engine.rs"
P = "crates/sync-core/src/protocol.rs"
N = "crates/sync-core/src/network.rs"
muts = [
 ("pow check removed", F, "    block.header.validate_pow(target).map_err(|e| e.to_string())", "    Ok(block.block_hash())"),
 ("pow: canonical bits check removed", F, "if target.to_compact_lossy() != bits {", "if false {"),
 ("pow: network limit check removed", F, "if target > limit {", "if false {"),
 ("bip34 mismatch accepted", F, "Some(h) if h == height => {}", "Some(_) => {}\n        #[allow(unreachable_patterns)]\n        Some(h) if h == height => {}"),
 ("bip34 missing accepted", F, 'None => {\n            return Err(invalid(format!(\n                "block {height}: coinbase does not commit to a height"\n            )))\n        }', "None => {}"),
 ("fixture coinbase-first (early) removed", F, "if !block.txdata.first().is_some_and(|tx| tx.is_coinbase()) {", "if false {"),
 ("fixture coinbase position (loop) removed", F, "if tx.is_coinbase() != is_first {", "if false {"),
 ("fixture duplicate txid removed", F, "if !seen_txids.insert(txid) {", "if !seen_txids.insert(txid) && false {"),
 ("fixture genesis link removed", F, "if block.header.prev_blockhash != parent_hash {", "if block.header.prev_blockhash != parent_hash && height != 1 {"),
 ("witness commitment: mismatch accepted", F, "if expected.as_byte_array() != committed {", "if false {"),
 ("witness commitment: reserved value not required", F, "Some(items) if items.len() == 1 && items[0].len() == 32 => items[0].clone(),", "Some(items) if items.len() == 1 && items[0].len() == 32 => items[0].clone(),\n        Some(items) if items.is_empty() => return Ok(()),"),
 ("witness without commitment accepted", F, "return if has_witness {", "return if has_witness && false {"),
 ("merkle root check removed", F, "if !block.check_merkle_root() {", "if false {"),
 ("missing-witness check removed", P, ".position(|(txin, prevout)| witness_is_missing(txin, &prevout.script_pubkey))", ".position(|(txin, prevout)| false && witness_is_missing(txin, &prevout.script_pubkey))"),
 ("missing-witness: p2sh-wrapped dropped", P, "return redeem.is_p2wpkh() || redeem.is_p2wsh();", "return false;"),
 ("missing-witness: any p2sh flagged", P, "if let (Some(Ok(Instruction::PushBytes(push))), None) =\n            (instructions.next(), instructions.next())", "if let (Some(Ok(Instruction::PushBytes(push))), _) =\n            (instructions.next(), instructions.next())"),
 ("segwit>1 rule removed", P, "if spends_unknown_segwit {", "if false && spends_unknown_segwit {"),
 ("prevout count check removed", P, "if prevouts.len() != tx.input.len() {", "if false {"),
 ("engine: empty block accepted", E, "if txs.is_empty() {", "if false {"),
 ("engine: coinbase position unchecked", E, "if entry.tx.is_coinbase() != (position == 0) {", "if false {"),
 ("engine: duplicate txid unchecked", E, "if !seen_txids.insert(txid) {", "if !seen_txids.insert(txid) && false {"),
 ("engine: self-parent unchecked", E, "if block.hash == block.prev_hash {", "if false {"),
 ("engine: hash link unchecked", E, "if block.prev_hash != prev.hash {", "if false {"),
 ("engine: chain mismatch unchecked", E, "if chain != expected_chain {", "if false {"),
 ("signet mapped to testnet", N, "LabNetwork::Signet => Network::Signet,", "LabNetwork::Signet => Network::Testnet,"),
 ("chain identity deserialize unverified", N, "Self::verified(raw.network, raw.genesis_hash)", "Ok(Self { network: raw.network, genesis_hash: raw.genesis_hash })"),
 ("mainnet name accepted as regtest", N, '"regtest" => Ok(LabNetwork::Regtest),', '"regtest" | "mainnet" => Ok(LabNetwork::Regtest),'),
]
survived = []
for name, path, old, new in muts:
    full = os.path.join(root, path)
    src = open(full).read()
    if src.count(old) != 1:
        print(f"SETUP-ERROR  {name}: pattern count {src.count(old)}"); survived.append(name + " (setup)"); continue
    open(full, "w").write(src.replace(old, new))
    r = subprocess.run(["cargo", "test", "--locked", "-q", "--workspace", "--", "--skip", "k_max"], cwd=root, env=env, capture_output=True, text=True)
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
