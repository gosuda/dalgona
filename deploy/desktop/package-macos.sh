#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf 'desktop-package: %s\n' "$1" >&2
    exit 1
}

: "${DALGONA_MACOS_SIGN_IDENTITY:?desktop-package: set DALGONA_MACOS_SIGN_IDENTITY to a Developer ID Application identity}"
: "${DALGONA_MACOS_NOTARY_PROFILE:?desktop-package: set DALGONA_MACOS_NOTARY_PROFILE to an existing notarytool keychain profile}"
case "$DALGONA_MACOS_SIGN_IDENTITY" in
    'Developer ID Application: '*) ;;
    *) fail 'DALGONA_MACOS_SIGN_IDENTITY must name a Developer ID Application identity' ;;
esac
host=$(go env GOHOSTOS)
[ "$host" = darwin ] || fail 'the universal DMG must be built on macOS with Xcode'

version=${DALGONA_PACKAGE_VERSION:-}
if [ -z "$version" ]; then
    tag=$(git describe --tags --exact-match --match 'v[0-9]*' HEAD 2>/dev/null) || fail 'set DALGONA_PACKAGE_VERSION to major.minor.patch or build at an exact vMAJOR.MINOR.PATCH tag'
    version=${tag#v}
fi
[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail 'DALGONA_PACKAGE_VERSION must be major.minor.patch'

for tool in clang lipo notarytool stapler; do
    xcrun --find "$tool" >/dev/null || fail "Xcode must provide $tool"
done
for tool in codesign ditto hdiutil iconutil plutil sips; do
    command -v "$tool" >/dev/null || fail "macOS must provide $tool"
done
sdk=$(xcrun --sdk macosx --show-sdk-path)

mkdir -p dist
work=$(mktemp -d "$PWD/dist/.dalgona-macos.XXXXXXXX")
mount="$work/mount"
mounted=no
cleanup() {
    if [ "$mounted" = yes ]; then
        hdiutil detach "$mount" || return
    fi
    rm -rf -- "${work:?missing package work directory}"
}
trap cleanup EXIT

volume="$work/volume"
app="$volume/dalgona.app"
macos="$app/Contents/MacOS"
resources="$app/Contents/Resources"
plist="$app/Contents/Info.plist"
iconset="$work/dalgona.iconset"
mkdir -p "$macos" "$resources" "$iconset"
cp deploy/desktop/Info.plist "$plist"
plutil -replace CFBundleShortVersionString -string "$version" "$plist"
plutil -replace CFBundleVersion -string "$version" "$plist"
plutil -lint "$plist"

for arch in amd64 arm64; do
    GOOS=darwin GOARCH="$arch" CGO_ENABLED=1 CC=clang SDKROOT="$sdk" \
        go build -tags gui -trimpath -o "$work/dalgona-gui-$arch" ./cmd/dalgona-gui
    GOOS=darwin GOARCH="$arch" CGO_ENABLED=0 \
        go build -trimpath -o "$work/dalgona-$arch" ./cmd/dalgona
done
for program in dalgona-gui dalgona; do
    xcrun lipo -create -output "$macos/$program" "$work/$program-amd64" "$work/$program-arm64"
    chmod 0755 "$macos/$program"
done
for size in 16 32 64; do
    sips -z "$size" "$size" assets/brand/dalgona-8.png --out "$work/icon-$size.png" >/dev/null
done
cp "$work/icon-16.png" "$iconset/icon_16x16.png"
cp "$work/icon-32.png" "$iconset/icon_16x16@2x.png"
cp "$work/icon-32.png" "$iconset/icon_32x32.png"
cp "$work/icon-64.png" "$iconset/icon_32x32@2x.png"
cp assets/brand/dalgona-8.png "$iconset/icon_128x128.png"
cp assets/brand/dalgona-16.png "$iconset/icon_128x128@2x.png"
cp assets/brand/dalgona-16.png "$iconset/icon_256x256.png"
iconutil -c icns -o "$resources/dalgona.icns" "$iconset"
cp LICENSE.md THIRD_PARTY_NOTICES.md assets/brand/dalgona.svg "$resources/"
ln -s /Applications "$volume/Applications"

identity="$DALGONA_MACOS_SIGN_IDENTITY"
codesign --force --sign "$identity" --timestamp --options runtime --identifier com.gosuda.dalgona.daemon "$macos/dalgona"
codesign --force --sign "$identity" --timestamp --options runtime "$app"
codesign --verify --deep --strict "$app"

notarize() {
    local file="$1" result status
    if ! result=$(xcrun notarytool submit "$file" --keychain-profile "$DALGONA_MACOS_NOTARY_PROFILE" --wait --timeout 1h --output-format plist); then
        printf '%s\n' "$result" >&2
        fail "notarization failed for ${file##*/}"
    fi
    status=$(plutil -extract status raw -expect string - <<<"$result") || fail "notarization returned no status for ${file##*/}"
    if [ "$status" != Accepted ]; then
        printf '%s\n' "$result" >&2
        fail "notarization did not accept ${file##*/}: $status"
    fi
}

ditto -c -k --keepParent "$app" "$work/dalgona.zip"
notarize "$work/dalgona.zip"
xcrun stapler staple "$app"

name="dalgona-gui_${version}_universal.dmg"
dmg="$work/$name"
hdiutil create -srcfolder "$volume" -volname dalgona -format UDZO -o "$dmg"
codesign --sign "$identity" --timestamp --identifier com.gosuda.dalgona.dmg "$dmg"
notarize "$dmg"
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
codesign --verify --strict "$dmg"
hdiutil verify "$dmg"

mkdir "$mount"
hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$mount" "$dmg"
mounted=yes
app="$mount/dalgona.app"
codesign --verify --deep --strict "$app"
xcrun stapler validate "$app"
for program in dalgona-gui dalgona; do
    xcrun lipo "$app/Contents/MacOS/$program" -verify_arch x86_64 arm64
done
cmp LICENSE.md "$app/Contents/Resources/LICENSE.md"
cmp THIRD_PARTY_NOTICES.md "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"
cmp assets/brand/dalgona.svg "$app/Contents/Resources/dalgona.svg"
test -s "$app/Contents/Resources/dalgona.icns"
hdiutil detach "$mount"
mounted=no
mv -f "$dmg" "dist/$name"
printf 'desktop-package: dist/%s verified; clean-system install, launch, upgrade, rollback and uninstall did not run\n' "$name"
