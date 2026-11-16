#!/usr/bin/env bash
# Usage: scripts/cargo-cache.sh key | vendor
# key: prints what a cached target/ depends on: Cargo.lock without the versions of local crates, which
#   every release bumps, and the vendored crates' tree.
# vendor: Cargo judges path crates by modification time, which a checkout resets, so when target/ was
#   built from the same vendored tree their files are dated back to keep that build. Run it after the
#   cache is restored and before building.
set -euo pipefail
cd "$(dirname "$0")/.."
tree=$(git rev-parse HEAD:vendor)
case ${1:-} in
  key)
    lock=$(awk 'BEGIN { RS = ""; ORS = "\n\n" } !/\nsource = / { gsub(/\nversion = "[^"]*"/, "") } { print }' Cargo.lock | shasum -a 256 | cut -c1-16)
    echo "$lock-${tree:0:16}"
    ;;
  vendor)
    if [[ -f target/vendor-tree && $(<target/vendor-tree) == "$tree" ]]; then
      git ls-files -z vendor | xargs -0 touch -t 200001010000
    fi
    mkdir -p target
    echo "$tree" > target/vendor-tree
    ;;
  *)
    echo "Usage: scripts/cargo-cache.sh key | vendor" >&2
    exit 2
    ;;
esac
