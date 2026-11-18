#!/usr/bin/env bash
# Usage: scripts/cargo-cache.sh key | fresh
# key: prints what a cached target/ depends on: Cargo.lock without the versions of local crates, which
#   every release bumps, the trees of the sources judged by modification time, the build profiles,
#   and the packaging scripts, which choose what is built with which features.
# fresh: Cargo (for path crates) and the native helper builds judge sources by modification time,
#   which a checkout resets, so each such tree that target/ was built from unchanged is dated back to
#   keep that build. A target/ built with other profiles is emptied, as none of it is used again.
#   Run it after the cache is restored and before building.
set -euo pipefail
cd "$(dirname "$0")/.."
# Each tree with the file in target/ that records which version of it target/ was built from.
trees=("vendor:target/vendor-tree" "crates/devices/native:target/native-tree")
profiles=$(awk '/^\[/ { keep = /^\[profile/ } keep' Cargo.toml | shasum -a 256 | cut -c1-16)
case ${1:-} in
  key)
    lock=$(awk 'BEGIN { RS = ""; ORS = "\n\n" } !/\nsource = / { gsub(/\nversion = "[^"]*"/, "") } { print }' Cargo.lock | shasum -a 256 | cut -c1-16)
    sources=$({ for entry in "${trees[@]}"; do git rev-parse "HEAD:${entry%%:*}"; done; git rev-parse HEAD:scripts/package-mac.sh HEAD:scripts/package-linux.sh; echo "$profiles"; } | shasum -a 256 | cut -c1-16)
    echo "$lock-$sources"
    ;;
  fresh)
    mkdir -p target
    if [[ ! -f target/profiles || $(<target/profiles) != "$profiles" ]]; then
      rm -rf target/debug target/release
    fi
    echo "$profiles" > target/profiles
    for entry in "${trees[@]}"; do
      path=${entry%%:*} marker=${entry#*:}
      tree=$(git rev-parse "HEAD:$path")
      if [[ -f $marker && $(<"$marker") == "$tree" ]]; then
        git ls-files -z "$path" | xargs -0 touch -t 200001010000
      fi
      echo "$tree" > "$marker"
    done
    ;;
  *)
    echo "Usage: scripts/cargo-cache.sh key | fresh" >&2
    exit 2
    ;;
esac
