#!/bin/zsh
set -euo pipefail

ROOT="${0:A:h:h}"
cd "$ROOT"

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets

swiftc \
    -typecheck \
    -parse-as-library \
    -target "$(uname -m)-apple-macos12.3" \
    platform/macos/ActivationHelper.swift \
    -framework Foundation \
    -framework SystemExtensions

swiftc \
    -typecheck \
    -parse-as-library \
    -target "$(uname -m)-apple-macos12.3" \
    platform/macos/AudioDriverInstaller.swift \
    -framework Foundation

swiftc \
    -typecheck \
    -parse-as-library \
    -target "$(uname -m)-apple-macos12.3" \
    platform/macos/DeviceProbe.swift \
    -framework AVFoundation \
    -framework Foundation

CAMERA_TEST="$(mktemp -t static-stream-camera-test.XXXXXX)"
DRIVER_TEST="$(mktemp -t static-stream-driver-test.XXXXXX)"
trap 'rm -f "$CAMERA_TEST" "$DRIVER_TEST"' EXIT
swiftc \
    -parse-as-library \
    -target "$(uname -m)-apple-macos12.3" \
    platform/macos/camera-extension/FreezeFrameSelector.swift \
    platform/macos/CameraDevelopmentTest.swift \
    -framework AVFoundation \
    -framework Foundation \
    -o "$CAMERA_TEST"
"$CAMERA_TEST"

swiftc \
    -typecheck \
    -target "$(uname -m)-apple-macos12.3" \
    platform/macos/camera-extension/FreezeFrameSelector.swift \
    platform/macos/camera-extension/StaticStreamCameraProvider.swift \
    platform/macos/camera-extension/main.swift \
    -framework AVFoundation \
    -framework CoreMediaIO \
    -framework Foundation \
    -framework IOKit

clang \
    -std=c11 \
    -fsyntax-only \
    -Wall \
    -Wextra \
    -Werror \
    platform/macos/audio-driver/StaticStreamAudio.c

clang \
    -std=c11 \
    -O2 \
    -Wall \
    -Wextra \
    -Werror \
    platform/macos/audio-driver/StaticStreamAudio.c \
    platform/macos/audio-driver/StaticStreamAudioTests.c \
    -framework CoreAudio \
    -framework CoreFoundation \
    -o "$DRIVER_TEST"
"$DRIVER_TEST"

plutil -lint \
    platform/macos/Info.plist \
    platform/macos/StaticStream.entitlements \
    platform/macos/audio-driver/Info.plist \
    platform/macos/camera-extension/Info.plist \
    platform/macos/camera-extension/StaticStreamCamera.entitlements
