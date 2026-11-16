#!/usr/bin/env bash
# Usage: scripts/release-feed.sh <version> <file>... > latest-<platform>.yml
# Writes the update feed `act_host::updates` reads for a release: the version, and each file's name,
# base64 SHA-512 and size. The updater fetches files by name from the release tagged v<version>, and
# takes the first .zip on macOS and the first .AppImage on Linux.
set -euo pipefail
[[ $# -ge 2 ]] || { echo "Usage: $0 <version> <file>..." >&2; exit 1; }
version=$1
shift
echo "version: $version"
echo "files:"
for file in "$@"; do
  [[ -f $file ]] || { echo "No such file: $file" >&2; exit 1; }
  echo "  - url: $(basename "$file")"
  echo "    sha512: $(openssl dgst -sha512 -binary "$file" | openssl base64 -A)"
  echo "    size: $(wc -c < "$file" | tr -d ' ')"
done
echo "releaseDate: '$(date -u +%Y-%m-%dT%H:%M:%S.000Z)'"
