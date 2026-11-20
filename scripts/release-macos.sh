#!/usr/bin/env bash
# Builds the macOS release into dist/:
#   BNK8-Configurator-<version>-macos-universal.dmg  signed, notarized, stapled app
#   bnk8-<version>-macos-universal.tar.gz             signed, notarized CLI + README + licenses
#   SHA256SUMS.txt
#
# Usage: scripts/release-macos.sh <version>          (version without the leading v)
#
# Environment:
#   SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)", or "-" for an ad-hoc
#                   signed smoke test that skips notarization and must not be published.
#   Notarization credentials, one of:
#     NOTARY_PROFILE                                        notarytool keychain profile (local)
#     APPLE_API_KEY_PATH, APPLE_API_KEY_ID, APPLE_API_ISSUER App Store Connect API key (CI)
#   TARGETS         Rust targets to merge (default: aarch64-apple-darwin x86_64-apple-darwin).
set -euo pipefail

version=${1:?usage: scripts/release-macos.sh <version>}
identity=${SIGN_IDENTITY:?set SIGN_IDENTITY to a Developer ID Application identity, or - for a smoke test}
read -r -a targets <<<"${TARGETS:-aarch64-apple-darwin x86_64-apple-darwin}"

app_name="BNK8 Configurator"
bundle_id="io.github.alexdibattista.bnk8"
min_macos="11.0"
icon_master="gui/assets/app-icon/macos-1024.png"  # from scripts/make-macos-icon.py

root=$(cd "$(dirname "$0")/.." && pwd)
dist="$root/dist"
work="$dist/work"
cd "$root"

die() { echo "error: $*" >&2; exit 1; }
step() { echo "==> $*"; }

# ---- preflight: everything that can fail before a long build ---------------

for manifest in Cargo.toml gui/Cargo.toml; do
    v=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$manifest" | head -1)
    [[ $v == "$version" ]] || die "$manifest has version $v; bump it to $version (see RELEASING.md)"
done

sysroot=$(rustc --print sysroot)
for t in "${targets[@]}"; do
    [[ -d $sysroot/lib/rustlib/$t ]] || die "Rust target $t is not installed: rustup target add $t"
done

notarize=true
notary_args=()
if [[ $identity == "-" ]]; then
    notarize=false
    echo "warning: ad-hoc signature, no notarization: this build must not be published" >&2
else
    security find-identity -v -p codesigning | grep -qF "\"$identity\"" ||
        die "signing identity not found in the keychain: $identity"
    if [[ -n ${NOTARY_PROFILE:-} ]]; then
        notary_args=(--keychain-profile "$NOTARY_PROFILE")
    elif [[ -n ${APPLE_API_KEY_PATH:-} ]]; then
        notary_args=(--key "$APPLE_API_KEY_PATH" --key-id "${APPLE_API_KEY_ID:?}" --issuer "${APPLE_API_ISSUER:?}")
    else
        die "set NOTARY_PROFILE, or APPLE_API_KEY_PATH + APPLE_API_KEY_ID + APPLE_API_ISSUER"
    fi
fi

if ((${#targets[@]} > 1)); then label="macos-universal"; else label="macos-${targets[0]%%-*}"; fi
dmg="$dist/BNK8-Configurator-$version-$label.dmg"
cli_dir="bnk8-$version-$label"
tarball="$dist/$cli_dir.tar.gz"

# ---- build -------------------------------------------------------------------

rm -rf "$dist"
mkdir -p "$work"
export MACOSX_DEPLOYMENT_TARGET=$min_macos
for t in "${targets[@]}"; do
    step "Building $t"
    cargo build --release --locked --target "$t" -p bnk8 -p bnk8-gui
done

merge() {
    local name=$1 inputs=()
    for t in "${targets[@]}"; do inputs+=("${CARGO_TARGET_DIR:-target}/$t/release/$name"); done
    lipo -create "${inputs[@]}" -output "$work/$name"
    echo "    $name: $(lipo -archs "$work/$name")"
}
step "Merging architectures"
merge bnk8
merge bnk8-gui

# ---- app bundle ----------------------------------------------------------------

step "Assembling $app_name.app"
app="$work/$app_name.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$work/bnk8-gui" "$app/Contents/MacOS/bnk8-gui"
cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>$app_name</string>
    <key>CFBundleExecutable</key><string>bnk8-gui</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleIdentifier</key><string>$bundle_id</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>$app_name</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key><string>$min_macos</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>© Alexander Di Battista. MIT OR Apache-2.0.</string>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

iconset="$work/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon_master" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$icon_master" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

# ---- sign ----------------------------------------------------------------------

sign_args=(--force --options runtime --sign "$identity")
$notarize && sign_args+=(--timestamp)
step "Signing"
codesign "${sign_args[@]}" --identifier "$bundle_id.cli" "$work/bnk8"
codesign "${sign_args[@]}" "$app"
codesign --verify --strict --verbose=2 "$work/bnk8"
codesign --verify --strict --deep --verbose=2 "$app"

notarize_file() {
    local file=$1 result="$work/notary.json" status id
    step "Notarizing $(basename "$file") (usually a few minutes)"
    xcrun notarytool submit "$file" "${notary_args[@]}" --wait --timeout 30m --output-format json >"$result"
    status=$(plutil -extract status raw -o - "$result")
    id=$(plutil -extract id raw -o - "$result")
    if [[ $status != Accepted ]]; then
        xcrun notarytool log "$id" "${notary_args[@]}" >&2 || true
        die "notarization of $(basename "$file") ended with status $status (submission $id)"
    fi
}

if $notarize; then
    # One submission covers the app and the CLI; the app then gets its ticket stapled.
    # The bare CLI binary cannot carry a stapled ticket: Gatekeeper looks it up online.
    stage="$work/notarize"
    mkdir -p "$stage"
    ditto "$app" "$stage/$app_name.app"
    ditto "$work/bnk8" "$stage/bnk8"
    ditto -c -k --keepParent "$stage" "$work/notarize.zip"
    notarize_file "$work/notarize.zip"
    xcrun stapler staple "$app"
fi

# ---- DMG -----------------------------------------------------------------------

step "Creating $(basename "$dmg")"
dmg_root="$work/dmg"
mkdir -p "$dmg_root"
ditto "$app" "$dmg_root/$app_name.app"
ln -s /Applications "$dmg_root/Applications"
hdiutil create -quiet -volname "$app_name" -srcfolder "$dmg_root" -fs HFS+ -format UDZO -ov "$dmg"
if $notarize; then
    codesign --force --timestamp --sign "$identity" "$dmg"
    notarize_file "$dmg"
    xcrun stapler staple "$dmg"
fi

# ---- CLI tarball ---------------------------------------------------------------

step "Creating $(basename "$tarball")"
mkdir -p "$work/$cli_dir"
ditto "$work/bnk8" "$work/$cli_dir/bnk8"
cp README.md LICENSE-MIT LICENSE-APACHE "$work/$cli_dir/"
tar -C "$work" -czf "$tarball" "$cli_dir"

# ---- verify and checksum ---------------------------------------------------------

if $notarize; then
    step "Verifying Gatekeeper acceptance"
    spctl --assess --type execute --verbose=2 "$app"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
    xcrun stapler validate "$dmg"
fi

(cd "$dist" && shasum -a 256 "$(basename "$dmg")" "$(basename "$tarball")" >SHA256SUMS.txt)
rm -rf "$work"
step "Done"
ls -lh "$dist"
