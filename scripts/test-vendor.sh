#!/bin/zsh
# Runs the unit tests of the vendored crates (vendor/taffy, vendor/zed/gpui and the current platform's
# gpui crates) with the features the app builds them with. They sit outside the workspace, so each
# is tested as a package of its own, pinned to the workspace's lockfile, patched as the app patches
# it, in the workspace's target dir. Their tests read fonts from vendor/assets.
# Usage: scripts/test-vendor.sh [taffy|gpui|platform] [-- <test filter and harness args>]
set -e
root=${0:A:h:h}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
jobs=${JOBS:-2}
# Upstream code: a lint a newer compiler adds must not fail its tests.
export RUSTFLAGS="${RUSTFLAGS:-} --cap-lints warn"

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

# The vendored taffy, as Cargo.toml patches it.
patches=(
  --config "patch.crates-io.taffy.path=\"$root/vendor/taffy\""
)

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
  # The app's features: font-kit, and runtime_shaders for the platform it draws with.
  run zed/gpui --no-default-features --features test-support,font-kit,gpui_platform/runtime_shaders $patches
fi
if [[ -z $only || $only == platform ]]; then
  if [[ $OSTYPE == darwin* ]]; then
    run zed/gpui_apple --features test-support,font-kit,runtime_shaders $patches
    run zed/gpui_macos --no-default-features --features font-kit,runtime_shaders $patches
  fi
fi
