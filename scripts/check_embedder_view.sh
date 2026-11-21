#!/usr/bin/env bash
# Checks the libraries the way an embedder gets them. The workspace's own
# tests cannot do this: they enable every feature of `silent-sync`.
#
#   1. probes: what is (not) reachable with default features and with
#      `low-level`, and a run of the default-feature library;
#   2. the probes' lock file pins the same versions as the workspace's;
#   3. documentation builds without warnings with default and all features;
#   4. the CLI built on its own (its real feature set) finds the fixture's
#      payments;
#   5. the offline examples run and give the expected result.
#
# Usage: scripts/check_embedder_view.sh        (from the repository root)
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== probes (default features, low-level)"
for probe in probe-default-features probe-low-level; do
  cargo test --manifest-path probes/Cargo.toml --locked --quiet -p "$probe"
done

echo "== probes/Cargo.lock agrees with Cargo.lock"
python3 -I - <<'PY'
import re, sys
def versions(path):
    found = {}
    for name, version in re.findall(r'\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"', open(path).read()):
        found.setdefault(name, set()).add(version)
    return found
root, probes = versions("Cargo.lock"), versions("probes/Cargo.lock")
ours = {"silent-sync", "probe-default-features", "probe-low-level"}
bad = [(n, sorted(v), sorted(root.get(n, []))) for n, v in probes.items() if n not in ours and v != root.get(n)]
if bad:
    sys.exit(f"probes/Cargo.lock differs from Cargo.lock: {bad}")
print(f"{len(probes) - len(ours & set(probes))} packages, same versions")
PY

echo "== documentation (default features, all features)"
RUSTDOCFLAGS="-D warnings" cargo doc -p silent-sync --no-deps --locked --quiet
RUSTDOCFLAGS="-D warnings" cargo doc -p silent-sync --no-deps --locked --quiet --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p silent-sync-sqlite --no-deps --locked --quiet

echo "== CLI built on its own"
target="${CARGO_TARGET_DIR:-target}/cli-alone"
cargo build -p lab-cli --locked --quiet --target-dir "$target"
out="$("$target/debug/silent-sync" scan --backend fixture \
  --fixture fixtures/local/regtest_basic_v1.json --label 1 2>/dev/null)"
grep -q "^detected_received_outputs: 7$" <<<"$out" || { echo "$out"; echo "unexpected scan result" >&2; exit 1; }
grep -q "\[full_block\]" <<<"$out"
out="$("$target/debug/silent-sync" verify --backend local-index \
  --fixture fixtures/local/regtest_basic_v1.json --reference fixture --label 1 2>/dev/null)"
grep -q "identical" <<<"$out" || { echo "$out"; echo "unexpected verify result" >&2; exit 1; }

echo "== examples"
out="$(cargo run --quiet --locked -p silent-sync --example cross_check_index \
  --features fixture,local-index -- fixtures/local/regtest_basic_v1.json)"
grep -q "^identical: true$" <<<"$out" || { echo "$out"; echo "unexpected example output" >&2; exit 1; }
out="$(cargo run --quiet --locked -p silent-sync --example custom_store \
  --features fixture -- fixtures/local/regtest_basic_v1.json)"
[ "$(grep -c "evidence FullBlockWithPrevouts" <<<"$out")" = 7 ] || { echo "$out"; echo "unexpected example output" >&2; exit 1; }
out="$(cargo run --quiet --locked -p silent-sync --example audit_and_add_label \
  --features fixture,local-index -- fixtures/local/regtest_basic_v1.json)"
grep -q "^audit against full blocks: identical: true (7 agreed, 0 only in the store, 0 only in the blocks)$" <<<"$out" \
  && grep -q "^rescan with labels \[0, 1, 5\]: 7 found again, 1 added$" <<<"$out" \
  && grep -q "^audit after the rescan: identical: true (8 agreed)$" <<<"$out" \
  || { echo "$out"; echo "unexpected example output" >&2; exit 1; }
echo "ok"
