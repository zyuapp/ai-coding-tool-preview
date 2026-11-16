#!/bin/zsh
# Checks every command's field validation against the TypeScript app's: the TS window check judges each
# command with each field swapped for values from a pool, and act_core's view_input must agree.
# Usage: scripts/command-diff/run.sh   (TS_SRC names the TypeScript checkout; SPEC_DIR its scratch copy)
set -e
here=${0:A:h}
root=${here:h:h}
src=${TS_SRC:-${root:h}/ai-coding-tool}
spec=${SPEC_DIR:-${TMPDIR:-/tmp}/act-spec}
mkdir -p $spec
rsync -a --delete --exclude node_modules --exclude dist --exclude release --exclude ios --exclude native --exclude vendor --exclude .git $src/ $spec/ts/
[[ -d $spec/ts/node_modules ]] || cp -cR $src/node_modules $spec/ts/node_modules
mkdir -p $spec/ts/tests/command-diff
cp $here/commands.test.mts $spec/ts/tests/command-diff/
(cd $spec/ts && ACT_COMMANDS_OUT=$spec/commands.jsonl npx vitest run tests/command-diff/commands.test.mts --silent=false > $spec/commands.txt 2>&1) || { cat $spec/commands.txt; exit 1; }
grep -A40 "NO SAMPLE" $spec/commands.txt || true
CARGO_INCREMENTAL=0 cargo build -j 2 --quiet -p act_core --example command_check --manifest-path $root/Cargo.toml
$root/target/debug/examples/command_check < $spec/commands.jsonl
