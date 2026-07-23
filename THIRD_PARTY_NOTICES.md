# Third-Party Notices

## Apple NullAudio Sample

`platform/macos/audio-driver/StaticStreamAudio.c` is adapted from Apple's "Creating an Audio Server
Driver Plug-in" sample, retrieved from Apple Developer Documentation in July 2026.

The upstream sample is Copyright 2024 Apple Inc. and is distributed under the permissive license in
`platform/macos/audio-driver/LICENSE.apple.txt`. Static Stream changes its identity, device
properties, build packaging, and I/O implementation to provide a lock-free output-to-input
loopback.

## Rust Dependencies

Rust dependency names and exact versions are recorded in `Cargo.lock`. Their license metadata is
available through `cargo metadata` and the respective package sources.
