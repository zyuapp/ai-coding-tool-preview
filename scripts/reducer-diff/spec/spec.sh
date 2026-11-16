#!/bin/zsh
# Runs the TypeScript app's own test suite with its reducer, deriveView and mobile projection each
# checked against act_core on every call, and lists where they differ (the tests still see TS's answers).
# Usage: scripts/reducer-diff/spec/spec.sh [vitest paths]   (default: application, domain, contracts, relay)
# TS_SRC names the TypeScript checkout (default ../ai-coding-tool beside this one); SPEC_DIR its scratch copy.
set -e
here=${0:A:h}
root=${here:h:h:h}
src=${TS_SRC:-${root:h}/ai-coding-tool}
spec=${SPEC_DIR:-${TMPDIR:-/tmp}/act-spec}
mkdir -p $spec
rsync -a --delete --exclude node_modules --exclude dist --exclude release --exclude ios --exclude native --exclude vendor --exclude .git $src/ $spec/ts/
[[ -d $spec/ts/node_modules ]] || cp -cR $src/node_modules $spec/ts/node_modules
python3 $here/patch.py $spec/ts
CARGO_INCREMENTAL=0 cargo build -j 2 --quiet -p act_core --example reduce_lines --example view_lines --manifest-path $root/Cargo.toml
CARGO_INCREMENTAL=0 cargo build -j 2 --quiet -p act_host --example host_lines --manifest-path $root/Cargo.toml
cp $root/target/debug/examples/reduce_lines $root/target/debug/examples/view_lines $root/target/debug/examples/host_lines $spec/
log=$spec/divergences.jsonl
: > $log
[[ $# -eq 0 ]] && set -- tests/application tests/domain tests/contracts tests/relay
(cd $spec/ts && ACT_RUST=diff ACT_RUST_BIN=$spec/reduce_lines ACT_VIEW_BIN=$spec/view_lines ACT_HOST_BIN=$spec/host_lines ACT_RUST_LOG=$log FC_RUNS=${FC_RUNS:-60} npx vitest run "$@" --reporter=dot > $spec/run.txt 2>&1) || true
grep 'Tests ' $spec/run.txt || tail -5 $spec/run.txt
node $here/summary.cjs $log
