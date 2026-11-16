#!/usr/bin/env bash
# Usage: scripts/package-linux.sh [x86_64|aarch64] [out-dir]
# Builds <artifact name>-<version>-<arch>.tar.gz and .AppImage into out-dir (default target/package),
# with names and ids from crates/core/src/identity.rs.
# Run on Linux for the host's architecture (the AppImage needs appimagetool on PATH there); elsewhere
# it runs itself in a Docker container for the given architecture (default the host's), with 2 build
# jobs, and makes the AppImage in a second one: Alpine's appimagetool, with the AppImage runtime built
# from source.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
host_arch=$(uname -m | sed 's/arm64/aarch64/; s/amd64/x86_64/')
arch=${1:-$host_arch}
out=${2:-$root/target/package}
cua_version=0.52.0
ort_version=1.32.0
case $arch in
  x86_64) cua_arch=x86_64 cua_sha256=55c99ec4177170b1a440a5efd41e55bacae1e9fe257c96dcd9e5b81ef9085036 ort_arch=x64 ort_sha256=bb169b7e374731730d2ae406fb39de7be4b315f413a9132efe896261c31bb503 runtime_sha256=617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46 ;;
  aarch64) cua_arch=arm64 cua_sha256=5c7d0a36c0e368fb2847f226ca1435c60bfb79ac3968d9b08a94d82c97da880a ort_arch=aarch64 ort_sha256=81a46251dd90da4122f4b9ceb4433f836fbc79ee1eddffca7c09033af81a1939 runtime_sha256=9a72a744d221576c9347ec074243acc5c3c7568896b12714e0e82e9f0bf47900 ;;
  *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
esac
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
identity() { sed -n "s/^pub const $1: [^=]* = \"\(.*\)\";$/\1/p" "$root/crates/core/src/identity.rs"; }
app_name=$(identity APP_NAME)
app_id=$(identity BUNDLE_ID)
url_scheme=$(identity URL_SCHEME)
executable=$(identity LINUX_EXECUTABLE)
name=$(identity ARTIFACT_NAME)-$version-$arch
[[ -n $app_name && -n $app_id && -n $url_scheme && -n $executable ]] || { echo "Could not read the app's identity." >&2; exit 1; }
# AppImage/type2-runtime, built from this commit. runtime_sha256 is its Continuous release's prebuilt
# runtime, which appimagetool on PATH embeds. assets/legal/linux/APPIMAGE-RUNTIME.txt names this commit.
runtime_commit=fedfad51acc2860b2916a1eef32ba4a500ff31ef

if [[ $(uname -s) != Linux || $arch != "$host_arch" ]]; then
  platform=linux/$([[ $arch == x86_64 ]] && echo amd64 || echo arm64)
  image=act-linux-build:$arch
  docker build --platform "$platform" -t "$image" - <<'DOCKERFILE'
FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake pkg-config libasound2-dev libfontconfig-dev libfreetype-dev libssl-dev \
    libvulkan1 libwayland-dev libx11-xcb-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libzstd-dev libdbus-1-dev curl file xz-utils libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev \
 && rm -rf /var/lib/apt/lists/*
DOCKERFILE
  mkdir -p "$out"
  docker run --rm --platform "$platform" --cpus 2 --memory 8g \
    -e CARGO_BUILD_JOBS=2 -e CARGO_TARGET_DIR=/target \
    -v "$root":/src -v "$out":/out -v act-linux-cargo-$arch:/usr/local/cargo/registry -v act-linux-target-$arch:/target \
    -w /src "$image" bash scripts/package-linux.sh "$arch" /out

  appimage_image=act-linux-appimage:$arch
  docker build --platform "$platform" --build-arg runtime_commit=$runtime_commit -t "$appimage_image" - <<'DOCKERFILE'
FROM alpine:3.21 AS runtime
ARG runtime_commit
RUN apk add --no-cache bash alpine-sdk util-linux file autoconf automake libtool xz eudev-dev gettext-dev linux-headers \
    meson zstd-dev zstd-static zlib-dev zlib-static clang musl-dev mimalloc-dev git
RUN git clone https://github.com/AppImage/type2-runtime /type2-runtime && cd /type2-runtime && git checkout -q $runtime_commit \
 && bash scripts/common/install-dependencies.sh && bash scripts/build-runtime.sh \
 && find out -name 'runtime-*' ! -name '*.debug' -exec cp {} /runtime ';'
FROM alpine:edge
RUN echo https://dl-cdn.alpinelinux.org/alpine/edge/testing >> /etc/apk/repositories \
 && apk add --no-cache appimagetool squashfs-tools desktop-file-utils file
COPY --from=runtime /runtime /runtime
DOCKERFILE
  docker run --rm --platform "$platform" -v "$out":/out "$appimage_image" sh -euc "
    mkdir -p /AppDir/usr && tar -xzf /out/$name.tar.gz -C /AppDir/usr --strip-components=1
    ln -s usr/bin/$executable /AppDir/AppRun
    cp /AppDir/usr/share/applications/$app_id.desktop /AppDir/
    cp /AppDir/usr/share/icons/hicolor/512x512/apps/$app_id.png /AppDir/
    rm -f /out/$name.AppImage
    ARCH=$arch appimagetool --no-appstream --runtime-file /runtime /AppDir /out/$name.AppImage"
  echo "Built $out/$name.tar.gz and $out/$name.AppImage"
  exit 0
fi

target_dir=${CARGO_TARGET_DIR:-$root/target}

echo "Building the release binary and the dictation helper…"
cargo build --release -p ai-coding-tool --manifest-path "$root/Cargo.toml"
cargo build --release -p act_devices --features onnx --bin aic-dictation-onnx --manifest-path "$root/Cargo.toml"

echo "Fetching ONNX Runtime $ort_version…"
ort_dir=$target_dir/onnxruntime/$ort_version-$ort_arch
if [[ ! -f $ort_dir/libonnxruntime.so.1 ]]; then
  mkdir -p "$ort_dir"
  archive=$ort_dir/onnxruntime.tgz
  curl -fsSL -o "$archive" "https://github.com/microsoft/onnxruntime/releases/download/v$ort_version/onnxruntime-linux-$ort_arch-$ort_version.tgz"
  [[ $(sha256sum "$archive" | cut -d' ' -f1) == "$ort_sha256" ]] || { echo "ONNX Runtime archive checksum mismatch." >&2; rm -f "$archive"; exit 1; }
  tar -xzf "$archive" -C "$ort_dir" --strip-components=1 "onnxruntime-linux-$ort_arch-$ort_version/lib/libonnxruntime.so.$ort_version" "onnxruntime-linux-$ort_arch-$ort_version/LICENSE" "onnxruntime-linux-$ort_arch-$ort_version/ThirdPartyNotices.txt"
  mv "$ort_dir/lib/libonnxruntime.so.$ort_version" "$ort_dir/libonnxruntime.so.1"
  rmdir "$ort_dir/lib"
  rm -f "$archive"
fi

echo "Fetching the Cua Driver $cua_version…"
cua_dir=$target_dir/cua-driver/$cua_version-linux
if [[ ! -x $cua_dir/cua-driver ]]; then
  mkdir -p "$cua_dir"
  archive=$cua_dir/cua-driver.tar.gz
  curl -fsSL -o "$archive" "https://github.com/trycua/cua/releases/download/cua-driver-rs-v$cua_version/cua-driver-rs-$cua_version-linux-$cua_arch-binary.tar.gz"
  [[ $(sha256sum "$archive" | cut -d' ' -f1) == "$cua_sha256" ]] || { echo "Cua Driver archive checksum mismatch." >&2; rm -f "$archive"; exit 1; }
  tar -xzf "$archive" -C "$cua_dir" cua-driver
  rm -f "$archive"
  chmod 755 "$cua_dir/cua-driver"
fi

# The package's layout is the bundle's: the binary in bin, what it ships in the Resources beside it.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
appdir=$work/$name
mkdir -p "$appdir/usr/bin" "$appdir/usr/Resources/native" "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/512x512/apps"
install -m 755 "$target_dir/release/ai-coding-tool" "$appdir/usr/bin/$executable"
# The dictation helper loads ONNX Runtime from beside itself.
install -m 755 "$target_dir/release/aic-dictation-onnx" "$appdir/usr/Resources/native/aic-dictation-onnx"
install -m 644 "$ort_dir/libonnxruntime.so.1" "$appdir/usr/Resources/native/libonnxruntime.so.1"
install -m 755 "$cua_dir/cua-driver" "$appdir/usr/Resources/cua-driver"
cp -R "$root/assets/app-plugin" "$appdir/usr/Resources/app-plugin"
cp -R "$root/assets/legal" "$appdir/usr/Resources/legal"
mkdir -p "$appdir/usr/Resources/legal/onnxruntime"
cp "$ort_dir/LICENSE" "$ort_dir/ThirdPartyNotices.txt" "$appdir/usr/Resources/legal/onnxruntime/"
cp "$root/assets/icon.png" "$appdir/usr/share/icons/hicolor/512x512/apps/$app_id.png"
cat > "$appdir/usr/share/applications/$app_id.desktop" <<DESKTOP
[Desktop Entry]
Name=$app_name
Comment=A fast local desktop client for Claude and Codex coding tasks.
Exec=$executable %U
Terminal=false
Type=Application
Icon=$app_id
Categories=Development;
StartupWMClass=$app_id
MimeType=x-scheme-handler/$url_scheme;
DESKTOP

mkdir -p "$out"
echo "Writing $out/$name.tar.gz…"
tar -C "$appdir/usr" -czf "$out/$name.tar.gz" --transform "s,^\.,$name," .

if command -v appimagetool >/dev/null 2>&1; then
  ln -s "usr/bin/$executable" "$appdir/AppRun"
  cp "$appdir/usr/share/applications/$app_id.desktop" "$appdir/"
  cp "$root/assets/icon.png" "$appdir/$app_id.png"
  runtime=$work/runtime
  curl -fsSL -o "$runtime" "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$arch"
  [[ $(sha256sum "$runtime" | cut -d' ' -f1) == "$runtime_sha256" ]] || { echo "The AppImage runtime's Continuous release is no longer $runtime_commit; update runtime_commit, runtime_sha256 and assets/legal/linux/APPIMAGE-RUNTIME.txt." >&2; exit 1; }
  echo "Writing $out/$name.AppImage…"
  ARCH=$arch appimagetool --no-appstream --runtime-file "$runtime" "$appdir" "$out/$name.AppImage"
elif [[ $out != /out ]]; then
  echo "appimagetool is not on PATH; skipped the AppImage."
fi
echo "Built $out/$name.tar.gz"
