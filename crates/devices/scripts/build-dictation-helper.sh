#!/bin/zsh
# Builds the macOS dictation helper (FluidAudio, Apple silicon only) into <workspace>/target/native,
# as aic-dictation and the source build's aic-dictation-dev. Elsewhere, or without Xcode, it does nothing.
set -e
dir=${0:a:h}
package=$dir/../native/dictation
out=$dir/../../../target/native
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || exit 0
swift=$(xcrun -f swift 2>/dev/null) || { echo "Skipping the dictation helper: Xcode is not installed." >&2; exit 0 }
newest=$(find $package/Sources $package/Package.swift -type f -exec stat -f %m {} + | sort -n | tail -1)
if [[ -f $out/aic-dictation && $(stat -f %m $out/aic-dictation) -ge $newest ]]; then
  [[ -f $out/aic-dictation-dev ]] || cp $out/aic-dictation $out/aic-dictation-dev
  exit 0
fi
scratch=$out/dictation-build
$swift build -c release --arch arm64 --package-path $package --scratch-path $scratch
bin=$($swift build -c release --arch arm64 --package-path $package --scratch-path $scratch --show-bin-path)
mkdir -p $out
cp $bin/aic-dictation $out/aic-dictation
cp $out/aic-dictation $out/aic-dictation-dev
