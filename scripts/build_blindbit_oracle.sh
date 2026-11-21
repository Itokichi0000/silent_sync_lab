#!/usr/bin/env bash
# Builds BlindBit Oracle v2 at the commit this project was researched and
# tested against.
#
#   scripts/build_blindbit_oracle.sh <dest-dir>
#
# Needs Go >= 1.24.1 and network access for Go modules. Prints the path of the
# binary on the last line; use it as BLINDBIT_ORACLE_EXE. <dest-dir> should be
# outside the repository.
#
# The oracle is third-party software (MIT). It is only ever run by the lab's
# tests against a throwaway regtest node on loopback.
set -euo pipefail

ORACLE_REPO="https://github.com/setavenger/blindbit-oracle"
ORACLE_REV="f9ab7fd8bc35467c492324f743ba1b69049a2bb6"

if [ $# -ne 1 ]; then
  echo "usage: $0 <dest-dir>" >&2
  exit 2
fi
DEST="$(mkdir -p "$1" && cd "$1" && pwd)"
cd "$DEST"
if [ ! -d src/.git ]; then
  rm -rf src
  git clone --quiet "$ORACLE_REPO" src
fi
git -C src fetch --quiet origin
git -C src checkout --quiet --detach "$ORACLE_REV"
test "$(git -C src rev-parse HEAD)" = "$ORACLE_REV"
(cd src && go build -o "$DEST/blindbit-oracle" ./cmd/blindbit-oracle) >&2
"$DEST/blindbit-oracle" --version >&2 || true
echo "$DEST/blindbit-oracle"
