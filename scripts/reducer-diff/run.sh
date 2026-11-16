#!/bin/zsh
# Differential fuzz of act_core::reduce against the TypeScript reducer.
# Usage: scripts/reducer-diff/run.sh [diff.cjs options, e.g. --seeds 1-50 --steps 400 --cover]
# TS_DIST names the built TypeScript app (default ../ai-coding-tool/dist beside this checkout).
set -e
here=${0:A:h}
root=${here:h:h}
dist=${TS_DIST:-${root:h}/ai-coding-tool/dist}
[[ -f $dist/main/application/workspace-reducer.js ]] || { echo "No built TypeScript reducer at $dist (set TS_DIST)."; exit 1; }
cargo build -j 2 --quiet -p act_core --example reduce_lines --manifest-path "$root/Cargo.toml"
export TRAILS=${TRAILS:-${TMPDIR:-/tmp}}
exec node "$here/diff.cjs" "$dist" "$root/target/debug/examples/reduce_lines" "$@"
