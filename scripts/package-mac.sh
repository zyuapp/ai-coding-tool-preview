#!/bin/zsh
# Usage: scripts/package-mac.sh [out-dir]
# Builds "<app name>.app" (Apple silicon) into out-dir, default target/package: the release binary,
# its icon, the native helpers, the Cua Driver, the app plugin and the legal notices. Signs with
# $MAC_SIGN_IDENTITY (a Developer ID Application identity, with a secure timestamp), else ad hoc.
# Names and ids come from crates/core/src/identity.rs.
set -euo pipefail
root=${0:a:h:h}
out=${1:-$root/target/package}
identity() { sed -n "s/^pub const $1: [^=]* = \"\(.*\)\";$/\1/p" $root/crates/core/src/identity.rs }
app_name=$(identity APP_NAME)
bundle_id=$(identity BUNDLE_ID)
url_scheme=$(identity URL_SCHEME)
[[ -n $app_name && -n $bundle_id && -n $url_scheme ]] || { echo "Could not read the app's identity." >&2; exit 1 }
app="$out/$app_name.app"
sign_identity=${MAC_SIGN_IDENTITY:--}
sign_flags=(--force --sign $sign_identity --options runtime)
[[ $sign_identity == - ]] || sign_flags+=(--timestamp)
cua_version=0.53.0
cua_sha256=9ab4b8cff7f0a72804a0ae7c17c0f8dad40e7c7b032a9099929da5d5c6648c54

[[ $(uname -s) == Darwin ]] || { echo "package-mac.sh builds on macOS only." >&2; exit 1 }
version=$(sed -n 's/^version = "\(.*\)"/\1/p' $root/Cargo.toml | head -1)

echo "Building the release binary…"
cargo build --release -p ai-coding-tool --manifest-path $root/Cargo.toml

echo "Building the native helpers…"
zsh $root/crates/devices/scripts/build-dictation-helper.sh
simulator=$(ls -t $root/target/release/build/act_devices-*/out/aic-simulator 2>/dev/null | head -1 || true)

echo "Fetching the Cua Driver $cua_version…"
cua_dir=$root/target/cua-driver/$cua_version
if [[ ! -x $cua_dir/cua-driver ]]; then
  mkdir -p $cua_dir
  archive=$cua_dir/cua-driver.tar.gz
  curl -fsSL -o $archive "https://github.com/trycua/cua/releases/download/cua-driver-rs-v$cua_version/cua-driver-rs-$cua_version-darwin-arm64.tar.gz"
  [[ $(shasum -a 256 $archive | cut -d' ' -f1) == $cua_sha256 ]] || { echo "Cua Driver archive checksum mismatch." >&2; rm -f $archive; exit 1 }
  tar -xzf $archive -C $cua_dir --strip-components=1 "cua-driver-rs-$cua_version-darwin-arm64/cua-driver"
  rm -f $archive
  if [[ $(lipo -archs $cua_dir/cua-driver) != arm64 ]]; then
    lipo -thin arm64 $cua_dir/cua-driver -output $cua_dir/cua-driver.arm64
    mv $cua_dir/cua-driver.arm64 $cua_dir/cua-driver
  fi
  chmod 755 $cua_dir/cua-driver
fi

echo "Assembling $app…"
rm -rf $app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/native"
cp $root/target/release/ai-coding-tool "$app/Contents/MacOS/ai-coding-tool"
cp $cua_dir/cua-driver "$app/Contents/Resources/cua-driver"
cp -R $root/assets/app-plugin "$app/Contents/Resources/app-plugin"
cp -R $root/assets/legal "$app/Contents/Resources/legal"
rm -rf "$app/Contents/Resources/legal/linux"
[[ -f $root/target/native/aic-dictation ]] && cp $root/target/native/aic-dictation "$app/Contents/Resources/native/"
[[ -n $simulator ]] && cp $simulator "$app/Contents/Resources/native/"

# The icon: Icon Composer's file when Xcode can compile it, else the flat PNG.
icon_work=$(mktemp -d)
trap 'rm -rf $icon_work' EXIT
icon_plist=""
if xcrun -f actool >/dev/null 2>&1; then
  cp -R $root/assets/icon.icon $icon_work/AppIcon.icon
  mkdir -p $icon_work/out
  xcrun actool $icon_work/AppIcon.icon --compile $icon_work/out --app-icon AppIcon --enable-on-demand-resources NO \
    --development-region en --target-device mac --platform macosx --minimum-deployment-target 13.0 \
    --output-partial-info-plist $icon_work/partial.plist >/dev/null
  cp $icon_work/out/AppIcon.icns $icon_work/out/Assets.car "$app/Contents/Resources/"
  icon_plist="	<key>CFBundleIconName</key>
	<string>AppIcon</string>"
else
  iconset=$icon_work/AppIcon.iconset
  mkdir -p $iconset
  for size in 16 32 64 128 256 512; do
    sips -z $size $size $root/assets/icon.png --out $iconset/icon_${size}x${size}.png >/dev/null
    sips -z $((size * 2)) $((size * 2)) $root/assets/icon.png --out $iconset/icon_${size}x${size}@2x.png >/dev/null
  done
  iconutil -c icns $iconset -o "$app/Contents/Resources/AppIcon.icns"
fi

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>$app_name</string>
	<key>CFBundleExecutable</key>
	<string>ai-coding-tool</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
$icon_plist
	<key>CFBundleIdentifier</key>
	<string>$bundle_id</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$app_name</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>CFBundleURLTypes</key>
	<array>
		<dict>
			<key>CFBundleURLName</key>
			<string>$app_name project</string>
			<key>CFBundleURLSchemes</key>
			<array>
				<string>$url_scheme</string>
			</array>
		</dict>
	</array>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>$app_name listens while you dictate, and writes down what you say on this Mac.</string>
	<key>NSAppleEventsUsageDescription</key>
	<string>$app_name uses Automation only when a requested computer-use action needs to communicate with another app.</string>
	<key>NSScreenCaptureUsageDescription</key>
	<string>$app_name captures app windows so Claude can see and interact with interfaces you ask it to control.</string>
</dict>
</plist>
PLIST
printf 'APPL????' > "$app/Contents/PkgInfo"

echo "Signing with ${sign_identity/#-/an ad hoc signature}…"
entitlements=$icon_work/entitlements.plist
cat > $entitlements <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.cs.disable-library-validation</key>
	<true/>
	<key>com.apple.security.device.audio-input</key>
	<true/>
</dict>
</plist>
PLIST
for helper in "$app"/Contents/Resources/native/*(N); do
  codesign $sign_flags --entitlements $entitlements $helper
done
# The Cua Driver keeps its own signature, which its permissions are tied to.
codesign $sign_flags --entitlements $entitlements "$app"
codesign --verify --strict "$app"
echo "Built $app"
