#!/bin/zsh
set -euo pipefail

ROOT="${0:A:h:h}"
DIST="$ROOT/dist"
APP="$DIST/Static Stream.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
EXTENSIONS="$CONTENTS/Library/SystemExtensions"
CAMERA_BUNDLE="$EXTENSIONS/com.madpin.staticstream.camera.systemextension"
CAMERA_CONTENTS="$CAMERA_BUNDLE/Contents"
CAMERA_MACOS="$CAMERA_CONTENTS/MacOS"
AUDIO_BUNDLE="$RESOURCES/StaticStreamAudio.driver"
AUDIO_CONTENTS="$AUDIO_BUNDLE/Contents"
AUDIO_MACOS="$AUDIO_CONTENTS/MacOS"
IDENTITY="${SIGNING_IDENTITY:-}"
TEAM_PREFIX="${TEAM_IDENTIFIER_PREFIX:-}"
APP_PROFILE="${APP_PROVISIONING_PROFILE:-}"
CAMERA_PROFILE="${CAMERA_PROVISIONING_PROFILE:-}"
UNIVERSAL="${UNIVERSAL:-1}"
RELEASE_SIGNING="${RELEASE_SIGNING:-0}"
VERSION="$(
    sed -nE 's/^version = "([^"]+)"$/\1/p' "$ROOT/Cargo.toml" \
        | head -1
)"
[[ -n "$VERSION" ]] || { print -u2 "Could not read the package version from Cargo.toml"; exit 1; }
VERSION_PARTS=(${(s:.:)${VERSION%%-*}})
[[ "${#VERSION_PARTS[@]}" == "3" ]] \
    || { print -u2 "The package version must have major.minor.patch components"; exit 1; }
BUILD_NUMBER="${STATIC_STREAM_BUILD_NUMBER:-$((VERSION_PARTS[1] * 1000000 + VERSION_PARTS[2] * 1000 + VERSION_PARTS[3]))}"

if [[ -z "$IDENTITY" ]]; then
    APPLE_IDENTITY_LINE="$(
        security find-identity -v -p codesigning 2>/dev/null \
            | grep -E '"(Apple Development|Developer ID Application|Apple Distribution):' \
            | head -1 \
            || true
    )"
    if [[ -n "$APPLE_IDENTITY_LINE" ]]; then
        IDENTITY="${APPLE_IDENTITY_LINE#*\"}"
        IDENTITY="${IDENTITY%%\"*}"
    fi
fi

if [[ -n "$IDENTITY" && "$IDENTITY" != "-" && -z "$TEAM_PREFIX" ]] \
    && [[ "$IDENTITY" =~ '\(([A-Z0-9]{10})\)$' ]]; then
    TEAM_PREFIX="${match[1]}"
fi

IDENTITY="${IDENTITY:--}"

if [[ -n "$TEAM_PREFIX" && "$TEAM_PREFIX" != *"." ]]; then
    TEAM_PREFIX="${TEAM_PREFIX}."
fi

if [[ "$RELEASE_SIGNING" == "1" ]]; then
    [[ "$IDENTITY" == "Developer ID Application:"* ]] \
        || { print -u2 "Release builds require a Developer ID Application signing identity"; exit 1; }
    [[ -n "$TEAM_PREFIX" ]] \
        || { print -u2 "Release builds require TEAM_IDENTIFIER_PREFIX"; exit 1; }
    [[ -n "$APP_PROFILE" && -f "$APP_PROFILE" ]] \
        || { print -u2 "Release builds require APP_PROVISIONING_PROFILE"; exit 1; }
    [[ -n "$CAMERA_PROFILE" && -f "$CAMERA_PROFILE" ]] \
        || { print -u2 "Release builds require CAMERA_PROVISIONING_PROFILE"; exit 1; }
fi

CODESIGN_ARGS=(--force --sign "$IDENTITY")
if [[ "$RELEASE_SIGNING" == "1" ]]; then
    CODESIGN_ARGS+=(--options runtime --timestamp)
else
    CODESIGN_ARGS+=(--timestamp=none)
fi

if [[ "$UNIVERSAL" == "1" ]]; then
    RUST_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
else
    case "$(uname -m)" in
        arm64) RUST_TARGETS=(aarch64-apple-darwin) ;;
        x86_64) RUST_TARGETS=(x86_64-apple-darwin) ;;
        *) print -u2 "Unsupported macOS architecture: $(uname -m)"; exit 1 ;;
    esac
fi

build_swift() {
    local output="$1"
    shift
    if [[ "$UNIVERSAL" == "1" ]]; then
        local arm_output="$DIST/${output:t}.arm64"
        local intel_output="$DIST/${output:t}.x86_64"
        swiftc -target arm64-apple-macos12.3 "$@" -o "$arm_output"
        swiftc -target x86_64-apple-macos12.3 "$@" -o "$intel_output"
        lipo -create "$arm_output" "$intel_output" -output "$output"
        rm -f "$arm_output" "$intel_output"
    else
        swiftc -target "$(uname -m)-apple-macos12.3" "$@" -o "$output"
    fi
}

cd "$ROOT"
for target in "${RUST_TARGETS[@]}"; do
    if ! rustup target list --installed | grep -qx "$target"; then
        print -u2 "Missing Rust target: $target"
        print -u2 "Install it with: rustup target add $target"
        exit 1
    fi
    STATIC_STREAM_TEAM_PREFIX="$TEAM_PREFIX" \
        cargo build --release --locked --target "$target"
done

rm -rf "$APP"
mkdir -p "$MACOS" "$CAMERA_MACOS" "$AUDIO_MACOS" "$RESOURCES/ThirdParty"
if [[ -n "$APP_PROFILE" ]]; then
    [[ -f "$APP_PROFILE" ]] || { print -u2 "App provisioning profile not found: $APP_PROFILE"; exit 1; }
    cp "$APP_PROFILE" "$CONTENTS/embedded.provisionprofile"
fi
if [[ -n "$CAMERA_PROFILE" ]]; then
    [[ -f "$CAMERA_PROFILE" ]] || { print -u2 "Camera provisioning profile not found: $CAMERA_PROFILE"; exit 1; }
    cp "$CAMERA_PROFILE" "$CAMERA_CONTENTS/embedded.provisionprofile"
fi
if [[ "$UNIVERSAL" == "1" ]]; then
    lipo -create \
        "$ROOT/target/aarch64-apple-darwin/release/static-stream" \
        "$ROOT/target/x86_64-apple-darwin/release/static-stream" \
        -output "$MACOS/static-stream"
else
    cp "$ROOT/target/${RUST_TARGETS[1]}/release/static-stream" "$MACOS/static-stream"
fi
cp "$ROOT/platform/macos/Info.plist" "$CONTENTS/Info.plist"
cp "$ROOT/platform/macos/audio-driver/Info.plist" "$AUDIO_CONTENTS/Info.plist"
cp "$ROOT/platform/macos/audio-driver/LICENSE.apple.txt" \
    "$RESOURCES/ThirdParty/Apple-AudioServerPlugIn-LICENSE.txt"
sed "s/__APP_GROUP__/${TEAM_PREFIX}group.com.madpin.staticstream/g" \
    "$ROOT/platform/macos/camera-extension/StaticStreamCameraProvider.swift" \
    > "$DIST/StaticStreamCameraProvider.swift"

build_swift "$MACOS/static-stream-activate" \
    -O \
    -parse-as-library \
    "$ROOT/platform/macos/ActivationHelper.swift" \
    -framework Foundation \
    -framework SystemExtensions

build_swift "$MACOS/static-stream-audio-install" \
    -O \
    -parse-as-library \
    "$ROOT/platform/macos/AudioDriverInstaller.swift" \
    -framework Foundation

build_swift "$MACOS/static-stream-probe" \
    -O \
    -parse-as-library \
    "$ROOT/platform/macos/DeviceProbe.swift" \
    -framework AVFoundation \
    -framework Foundation

build_swift "$MACOS/static-stream-camera-test" \
    -O \
    -parse-as-library \
    "$ROOT/platform/macos/camera-extension/FreezeFrameSelector.swift" \
    "$ROOT/platform/macos/CameraDevelopmentTest.swift" \
    -framework AVFoundation \
    -framework Foundation

build_swift "$CAMERA_MACOS/StaticStreamCameraExtension" \
    -O \
    "$ROOT/platform/macos/camera-extension/FreezeFrameSelector.swift" \
    "$DIST/StaticStreamCameraProvider.swift" \
    "$ROOT/platform/macos/camera-extension/main.swift" \
    -framework AVFoundation \
    -framework CoreMediaIO \
    -framework Foundation \
    -framework IOKit

clang \
    -std=c11 \
    -O2 \
    -Wall \
    -Wextra \
    -Werror \
    -arch arm64 \
    -arch x86_64 \
    -mmacosx-version-min=12.3 \
    -bundle \
    "$ROOT/platform/macos/audio-driver/StaticStreamAudio.c" \
    -framework CoreAudio \
    -framework CoreFoundation \
    -o "$AUDIO_MACOS/StaticStreamAudio"

sed "s/__TEAM_PREFIX__/$TEAM_PREFIX/g" \
    "$ROOT/platform/macos/camera-extension/Info.plist" \
    > "$CAMERA_CONTENTS/Info.plist"
sed "s/__TEAM_PREFIX__/$TEAM_PREFIX/g" \
    "$ROOT/platform/macos/camera-extension/StaticStreamCamera.entitlements" \
    > "$DIST/StaticStreamCamera.entitlements"
sed "s/__TEAM_PREFIX__/$TEAM_PREFIX/g" \
    "$ROOT/platform/macos/StaticStream.entitlements" \
    > "$DIST/StaticStream.entitlements"

for plist in \
    "$CONTENTS/Info.plist" \
    "$CAMERA_CONTENTS/Info.plist" \
    "$AUDIO_CONTENTS/Info.plist"; do
    plutil -replace CFBundleShortVersionString -string "$VERSION" "$plist"
    plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$plist"
done

plutil -lint \
    "$CONTENTS/Info.plist" \
    "$CAMERA_CONTENTS/Info.plist" \
    "$AUDIO_CONTENTS/Info.plist"
codesign "${CODESIGN_ARGS[@]}" "$MACOS/static-stream-activate"
codesign "${CODESIGN_ARGS[@]}" "$MACOS/static-stream-audio-install"
codesign "${CODESIGN_ARGS[@]}" "$MACOS/static-stream-probe"
codesign "${CODESIGN_ARGS[@]}" "$MACOS/static-stream-camera-test"
codesign "${CODESIGN_ARGS[@]}" "$AUDIO_BUNDLE"
if [[ "$IDENTITY" == "-" || -z "$TEAM_PREFIX" ]]; then
    codesign "${CODESIGN_ARGS[@]}" "$CAMERA_BUNDLE"
    codesign "${CODESIGN_ARGS[@]}" "$APP"
else
    codesign \
        "${CODESIGN_ARGS[@]}" \
        --entitlements "$DIST/StaticStreamCamera.entitlements" \
        "$CAMERA_BUNDLE"
    codesign \
        "${CODESIGN_ARGS[@]}" \
        --entitlements "$DIST/StaticStream.entitlements" \
        "$APP"
fi
codesign --verify --deep --strict --verbose=2 "$APP"

print "Built: $APP"
file "$MACOS/static-stream"
if [[ "$IDENTITY" == "-" || -z "$TEAM_PREFIX" ]]; then
    print "Camera activation needs an Apple Development or Developer ID Application certificate and TEAM_IDENTIFIER_PREFIX."
fi
