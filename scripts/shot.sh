#!/bin/zsh
# Usage: scripts/shot.sh <process-name-or-pid> <out.png>
# Captures the first on-screen window owned by the named process.
set -e
dir=${0:a:h}
bin=/tmp/act-winid2
[ -x $bin ] || swiftc -O $dir/winid.swift -o $bin
id=$($bin "$1")
[ -n "$id" ] || { echo "no window for $1" >&2; exit 1; }
screencapture -x -o -l$id "$2"
