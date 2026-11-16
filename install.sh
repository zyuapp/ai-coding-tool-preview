#!/bin/sh
# Install the latest AI Coding Tool Preview release.
#
#   curl -fsSL https://raw.githubusercontent.com/zyuapp/ai-coding-tool-preview/main/install.sh | sh
#
# macOS gets the signed app in /Applications. Linux gets the AppImage in ~/Applications,
# which is the only Linux package that updates itself without asking for a password.
set -eu

REPO="zyuapp/ai-coding-tool-preview"
DOWNLOAD="https://github.com/$REPO/releases/latest/download"
APP="AI Coding Tool Preview"

fail() { printf '\nInstall failed: %s\n' "$1" >&2; exit 1; }
say() { printf '%s\n' "$1"; }

# Each release publishes one update feed per platform, naming its files and their checksums,
# so the installer never has to guess a filename.
manifest_for() {
  case "$1" in
    Darwin)
      if [ "$2" = "x86_64" ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = "1" ]; then
        set -- "$1" arm64
      fi
      [ "$2" = "arm64" ] || fail "macOS builds are Apple silicon only, and this Mac is $2."
      echo latest-mac.yml
      ;;
    Linux)
      case "$2" in
        x86_64 | amd64) echo latest-linux.yml ;;
        aarch64 | arm64) echo latest-linux-arm64.yml ;;
        *) fail "Linux builds cover x86_64 and arm64, and this machine is $2." ;;
      esac
      ;;
    *) fail "$1 is not supported yet." ;;
  esac
}

# The first file in the feed ending in $1, and its checksum, as "name sha512".
pick() {
  printf '%s\n' "$manifest" | awk -v suffix="$1" '
    /^ *- url: / { name = $3; gsub(/["\047]/, "", name); next }
    /^ *sha512: / && name != "" && substr(name, length(name) - length(suffix) + 1) == suffix { print name, $2; exit }
  '
}

verify() {
  command -v openssl >/dev/null 2>&1 || { say "Skipping the checksum because openssl is missing."; return; }
  [ "$(openssl dgst -sha512 -binary "$1" | openssl base64 -A)" = "$2" ] \
    || fail "The download did not match its checksum. Try again."
}

launch_linux() {
  [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] || return 0
  if command -v setsid >/dev/null 2>&1; then
    setsid "$1" >/dev/null 2>&1 &
  else
    "$1" >/dev/null 2>&1 &
  fi
}

# The AppImage carries its own FUSE library, but mounting still needs the kernel's FUSE device
# and a fusermount helper.
has_fuse() {
  [ -e /dev/fuse ] && { command -v fusermount3 >/dev/null 2>&1 || command -v fusermount >/dev/null 2>&1; }
}

command -v curl >/dev/null 2>&1 || fail "curl is required."

system="$(uname -s)"
manifest_name="$(manifest_for "$system" "$(uname -m)")"
manifest="$(curl -fsSL "$DOWNLOAD/$manifest_name")" || fail "Could not read the latest release ($manifest_name)."
version="$(printf '%s\n' "$manifest" | sed -n 's/^version: *//p' | head -n 1 | tr -d "\"'")"
if [ "$system" = Darwin ]; then picked="$(pick .zip)"; else picked="$(pick .AppImage)"; fi
asset="${picked% *}"
checksum="${picked#* }"
[ -n "$picked" ] && [ -n "$version" ] || fail "The release feed could not be read."

if [ "$system" = Darwin ]; then
  applications=/Applications
  [ -w "$applications" ] || applications="$HOME/Applications"
else
  applications="$HOME/Applications"
fi
mkdir -p "$applications"

# Stage inside the destination so the final move is a rename on the same filesystem.
staging="$applications/.aicodingtool-preview-install.$$"
rm -rf "$staging"
mkdir "$staging"
trap 'rm -rf "$staging"' EXIT INT TERM

say "Downloading $APP $version for $(uname -s) $(uname -m)."
curl -fL --retry 3 --progress-bar -o "$staging/$asset" "$DOWNLOAD/$asset" \
  || fail "The download did not finish."
verify "$staging/$asset" "$checksum"

if [ "$system" = Darwin ]; then
  if pgrep -f "/$APP.app/Contents/MacOS/" >/dev/null 2>&1; then
    fail "$APP is running. Quit it, then run this again."
  fi
  /usr/bin/ditto -x -k "$staging/$asset" "$staging/unpacked" || fail "The download could not be unpacked."
  bundle="$(cd "$staging/unpacked" && ls -d ./*.app 2>/dev/null | head -n 1 | sed 's|^\./||')"
  [ -n "$bundle" ] || fail "The download contained no app."
  target="$applications/$bundle"
  rm -rf "$target"
  mv "$staging/unpacked/$bundle" "$target"
  xattr -dr com.apple.quarantine "$target" 2>/dev/null || true
  say "Installed $target"
  open -a "$target"
else
  target="$applications/AICodingToolPreview.AppImage"
  chmod 755 "$staging/$asset"
  mv -f "$staging/$asset" "$target"
  say "Installed $target"
  if has_fuse; then
    launch_linux "$target"
    say "It adds itself to your app menu the first time it runs."
  else
    say "FUSE is not available, so start it with: $target --appimage-extract-and-run"
  fi
fi
