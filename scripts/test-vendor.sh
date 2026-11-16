#!/bin/zsh
# Runs the unit tests of the vendored crates (vendor/gpui, vendor/taffy) with the features the app
# builds them with. They sit outside the workspace, so each is tested as a package of its own,
# pinned to the workspace's lockfile, patched as the app patches it, in the workspace's target dir.
# Usage: scripts/test-vendor.sh [gpui|taffy] [-- <test filter and harness args>]
set -e
root=${0:A:h:h}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
jobs=${JOBS:-2}

only=""
if [[ $# -gt 0 && $1 != "--" ]]; then
  only=$1
  shift
fi
[[ ${1:-} == "--" ]] && shift
harness=("$@")
(( ${#harness} )) || harness=(--test-threads=2)

# Each crate takes the workspace's lockfile for the run, so it builds the versions the app ships.
locks=()
cleanup() { for lock in $locks; do rm -f $lock; done; locks=() }
trap cleanup EXIT
trap 'cleanup; exit 1' INT TERM HUP PIPE

run() {
  local crate=$1; shift
  local lock=$root/vendor/$crate/Cargo.lock
  cp $root/Cargo.lock $lock
  locks+=($lock)
  echo "== vendor/$crate"
  cargo test -j $jobs --manifest-path $root/vendor/$crate/Cargo.toml --tests "$@" -- $harness
}

if [[ -z $only || $only == taffy ]]; then
  run taffy
fi
if [[ -z $only || $only == gpui ]]; then
  # The app's features (font-kit, runtime_shaders) and the vendored taffy and xattr, as Cargo.toml patches them.
  run gpui --no-default-features --features test-support,font-kit,runtime_shaders \
    --config "patch.crates-io.taffy.path=\"$root/vendor/taffy\"" \
    --config "patch.crates-io.xattr.path=\"$root/vendor/xattr\""
fi
