#!/usr/bin/env bash
# Downloads the Bitcoin Core release this project's end-to-end tests are run
# against and verifies it before unpacking.
#
#   scripts/fetch_bitcoind.sh <dest-dir>
#
# Prints the path of the bitcoind binary on the last line; use it as
# BITCOIND_EXE. <dest-dir> should be outside the repository (or git-ignored).
#
# Verification:
#   1. SHA-256 of the tarball against the value pinned below (always).
#   2. If VERIFY_SIGNATURES=1: the release's SHA256SUMS file against its
#      detached signatures, using the builder keys from a pinned commit of
#      bitcoin-core/guix.sigs, requiring MIN_GOOD_SIGNATURES distinct valid
#      signers (default 3). The keys are taken from GitHub over TLS, i.e.
#      their authenticity rests on that repository.
#
# The binary is always unpacked afresh from the verified tarball; anything
# already present in <dest-dir>/bitcoin-<version> is discarded.
#
# Only Linux x86_64 is pinned. For another platform, download and verify the
# release yourself and point BITCOIND_EXE at it.
set -euo pipefail

VERSION="32.1"
TARBALL="bitcoin-${VERSION}-x86_64-linux-gnu.tar.gz"
TARBALL_SHA256="61545c17c3120fb7412f896d248b11993af04ec8817aad3649c55b2afb044963"
BASE_URL="https://bitcoincore.org/bin/bitcoin-core-${VERSION}"

if [ $# -ne 1 ]; then
  echo "usage: $0 <dest-dir>" >&2
  exit 2
fi
if [ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]; then
  echo "only Linux x86_64 is pinned in this script" >&2
  exit 1
fi
DEST="$(mkdir -p "$1" && cd "$1" && pwd)"
cd "$DEST"

# Pinned commit of bitcoin-core/guix.sigs the builder keys are taken from.
GUIX_SIGS_REV="1326cef321a1218d49a9401364523a463908ecf0"

check_tarball() { echo "${TARBALL_SHA256}  ${TARBALL}" | sha256sum -c - >&2; }

if [ -f "$TARBALL" ] && ! check_tarball 2>/dev/null; then
  echo "existing $TARBALL does not match the pinned hash (partial download?); fetching again" >&2
  rm -f "$TARBALL"
fi
if [ ! -f "$TARBALL" ]; then
  curl -fsSL -o "$TARBALL.part" "$BASE_URL/$TARBALL"
  mv "$TARBALL.part" "$TARBALL"
fi
check_tarball

if [ "${VERIFY_SIGNATURES:-0}" = "1" ]; then
  curl -fsSL -o SHA256SUMS "$BASE_URL/SHA256SUMS"
  curl -fsSL -o SHA256SUMS.asc "$BASE_URL/SHA256SUMS.asc"
  if ! grep -q "^${TARBALL_SHA256}  ${TARBALL}\$" SHA256SUMS; then
    echo "the release's SHA256SUMS does not list the pinned hash for $TARBALL" >&2
    exit 1
  fi
  rm -rf guix.sigs gnupg
  git clone --quiet https://github.com/bitcoin-core/guix.sigs
  git -C guix.sigs checkout --quiet --detach "$GUIX_SIGS_REV"
  export GNUPGHOME="$DEST/gnupg"
  mkdir -p "$GNUPGHOME" && chmod 700 "$GNUPGHOME"
  gpg --quiet --import guix.sigs/builder-keys/*.gpg 2>/dev/null || true
  status="$(gpg --status-fd 1 --verify SHA256SUMS.asc SHA256SUMS 2>/dev/null || true)"
  # Count distinct signing keys, not signature packets: the same signature
  # repeated must not count twice. VALIDSIG's last field is the primary key.
  signers="$(printf '%s\n' "$status" | awk '$2 == "VALIDSIG" { print $NF }' | sort -u | grep -c . || true)"
  bad="$(printf '%s\n' "$status" | grep -c '^\[GNUPG:\] BADSIG' || true)"
  echo "signatures on SHA256SUMS: ${signers} distinct valid signers, ${bad} bad" >&2
  if [ "$bad" -ne 0 ] || [ "$signers" -lt "${MIN_GOOD_SIGNATURES:-3}" ]; then
    echo "signature verification failed" >&2
    exit 1
  fi
fi

# Always unpack afresh from the verified tarball: whatever was in the
# directory before (an interrupted extraction, a foreign file) is not trusted.
rm -rf "bitcoin-${VERSION}"
tar xzf "$TARBALL"
"$DEST/bitcoin-${VERSION}/bin/bitcoind" -version | head -n 1 >&2
echo "$DEST/bitcoin-${VERSION}/bin/bitcoind"
