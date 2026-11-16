#!/bin/zsh
# Usage: scripts/profile.sh <dir>
# Copies the Electron dev profile's store and JSON files into <dir> for ACT_PROFILE_DIR.
# ELECTRON_PROFILE names the source profile (default: the Electron dev build's on macOS).
set -e
[[ -n ${1:-} ]] || { echo "Usage: scripts/profile.sh <dir>" >&2; exit 2; }
src=${ELECTRON_PROFILE:-"$HOME/Library/Application Support/AI Coding Tool Dev"}
[[ -d $src ]] || { echo "No profile at $src (set ELECTRON_PROFILE)." >&2; exit 1; }
mkdir -p "$1"
# A WAL database cannot be opened read-only without its shared-memory file, so copy it first.
stage=$(mktemp -d)
cp "$src"/tasks.v3.sqlite* "$stage"/
sqlite3 "$stage/tasks.v3.sqlite" ".backup '$1/tasks.v3.sqlite'"
rm -rf "$stage"
cp "$src"/*.json "$1"/ 2>/dev/null || true
# A copy must not take over the phone bridge's port and Tailscale Serve, or drive paired computers.
echo '{"version":1,"enabled":false}' > "$1/mobile.v1.json"
rm -f "$1/computers.v1.json" "$1/mobile-devices.v1.json"
# Its process ids belong to the Electron app; the copy's memory monitor would signal them.
rm -f "$1/session-processes.json"
# A copy has launched this build before, so its first launch runs no post-update engine upgrades.
sed -n 's/^version = "\(.*\)"/\1/p' "${0:A:h}/../Cargo.toml" | head -1 | tr -d '\n' > "$1/launched-version"
