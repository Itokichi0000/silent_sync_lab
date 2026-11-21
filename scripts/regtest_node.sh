#!/usr/bin/env bash
# Starts / stops a local Bitcoin Core REGTEST node for manual experiments with
# `silent-sync sync --backend core`.
#
#   BITCOIND_EXE=/path/to/bitcoind scripts/regtest_node.sh start
#   scripts/regtest_node.sh status
#   scripts/regtest_node.sh stop
#
# The node:
#   * runs regtest only (this script has no way to select another chain);
#   * binds RPC and REST to 127.0.0.1 only and does not listen for peers;
#   * keeps its data in ./regtest-data (git-ignored), including the RPC cookie.
#
# REST (`-rest=1`) is unauthenticated and read-only: any local process can read
# chain data from it. That is what the lab's Core backend uses; it never needs
# the RPC cookie.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATADIR="${REGTEST_DATADIR:-$ROOT/regtest-data}"
PORT="${REGTEST_PORT:-18443}"
: "${BITCOIND_EXE:=bitcoind}"
CLI_EXE="${BITCOIN_CLI_EXE:-$(dirname "$BITCOIND_EXE")/bitcoin-cli}"
cli() { "$CLI_EXE" -chain=regtest -datadir="$DATADIR" -rpcconnect=127.0.0.1 -rpcport="$PORT" "$@"; }

case "${1:-}" in
  start)
    mkdir -p "$DATADIR"
    "$BITCOIND_EXE" -chain=regtest -datadir="$DATADIR" -daemonwait \
      -server=1 -rest=1 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcport="$PORT" \
      -listen=0 -connect=0 -dnsseed=0 -fixedseeds=0 -natpmp=0 -fallbackfee=0.0001
    echo "regtest node started; REST at http://127.0.0.1:$PORT (height $(cli getblockcount))"
    ;;
  status)
    cli getblockchaininfo | grep -E '"(chain|blocks|bestblockhash)"'
    ;;
  stop)
    cli stop
    ;;
  *)
    echo "usage: $0 start|status|stop" >&2
    exit 2
    ;;
esac
