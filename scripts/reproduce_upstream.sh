#!/usr/bin/env bash
# Gate 0: reproduce the upstream bdk-sp test suite and README example at the
# commit this project pins.
#
#   scripts/reproduce_upstream.sh <work-dir>
#
# <work-dir> is created if needed and holds the upstream clone and the logs.
# It must be outside this repository.
#
# The upstream dev-dependencies download Bitcoin Core 25.0 and electrs at build
# time and verify their SHA-256. Behind a TLS-intercepting proxy that download
# fails; in that case fetch the two files yourself and export, before running:
#
#   BITCOIND_TARBALL_FILE=/path/to/bitcoin-25.0-x86_64-linux-gnu.tar.gz
#   ELECTRSD_DOWNLOAD_ENDPOINT=http://127.0.0.1:<port>   # directory with the electrs zip
#
# The build scripts still verify the hashes. Never disable TLS verification.
set -euo pipefail

BDK_SP_REV="bb373e566fa2a9ee037cb15de2cbe6e679aac7b0"

if [ $# -ne 1 ]; then
  echo "usage: $0 <work-dir>" >&2
  exit 2
fi
WORK="$(mkdir -p "$1" && cd "$1" && pwd)"
LOGS="$WORK/logs"
mkdir -p "$LOGS"

if [ ! -d "$WORK/bdk-sp/.git" ]; then
  git clone --quiet https://github.com/bitcoindevkit/bdk-sp "$WORK/bdk-sp"
fi
cd "$WORK/bdk-sp"
git checkout --quiet --detach "$BDK_SP_REV"
echo "bdk-sp at $(git rev-parse HEAD)"

run() { # run <log-name> <command...>
  local log="$LOGS/$1.log"
  shift
  echo "\$ $*" | tee "$log"
  if "$@" >>"$log" 2>&1; then
    echo "  ok      -> $log"
  else
    echo "  FAILED  -> $log" >&2
    return 1
  fi
}

run upstream_cargo_test cargo test --no-fail-fast --all-features
run upstream_cargo_test_slow cargo test --no-fail-fast --all-features -- --ignored slow::
run upstream_example_send_and_scan cargo run --example send_and_scan

grep -h "^test result" "$LOGS/upstream_cargo_test.log" "$LOGS/upstream_cargo_test_slow.log" \
  | grep -v " 0 passed; 0 failed; 0 ignored"
tail -n 4 "$LOGS/upstream_example_send_and_scan.log"
