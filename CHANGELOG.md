# Changelog

## 0.1.9 - 2026-07-23

- Add automatic GitHub Release checks after launch and a manual check in the window and menu bar.
- Add a keyboard-accessible App updates section with version, status, release notes, and an
  explicit install-and-restart action.
- Verify updater archives by trusted repository URL and SHA-256 before validating the extracted
  bundle ID, version, Apple Team ID, code signature, and Gatekeeper assessment.
- Add detached app replacement with same-directory staging, rollback, cleanup, and restart.
- Add a tag-driven GitHub Actions release pipeline for universal Developer ID signing,
  notarization, stapling, DMG and updater ZIP packaging, and GitHub Release publication.
- Derive all host, camera-extension, and audio-driver versions from `Cargo.toml`.
- Document release secrets, publishing, updater behavior, architecture, and recovery.

## 0.1.8 - 2026-07-23

- Add native Clean, Deep, Robot, Anonymous, Radio, Alien, Tiny, and Demon voice presets.
- Add preallocated pitch, filter, modulation, saturation, smoothing, and preset-crossfade DSP.
- Add voice preset, intensity, and wet/dry controls to the main window.
- Add a processed-voice level between physical input and final Static Microphone output.
- Add direct voice-effect selection to the menu bar and `Option+Command+V` preset cycling.
- Persist voice settings and acknowledge applied presets in the Activity debug screen.
- Restore compatibility with the documented Rust 1.85 minimum by pinning the ring buffer and
  avoiding newer let-chain syntax.

## 0.1.7 - 2026-07-23

- Forward audio-engine status events directly to the UI instead of waiting for a 500 ms idle poll.
- Distinguish decoded-but-queued clips as **Starting** before playback is acknowledged.
- Update visible audio meters and clip progress at 10 Hz through a small telemetry payload.
- Stop WebKit telemetry updates while the control window is hidden.
- Aggregate level peaks once per audio callback instead of using an atomic operation per sample.
- Process oversized Core Audio buffers in fixed scratch chunks without callback-time allocation.
- Replace per-restart warning sleeper threads with an event-loop deadline.

## 0.1.6 - 2026-07-23

- Fill the active sound-clip button from left to right as acknowledged playback advances.
- Show the current playback percentage without recreating the focused clip button.
- Extract the camera freeze selector into shared extension and test-helper code.
- Add an unsigned camera development test to the Setup screen and Activity log.
- Add an in-app camera signing and installation guide.
- Support optional host-app and camera-extension provisioning profiles in macOS builds.
- Add illustrated usage, camera signing, development, and architecture documentation.

## 0.1.5 - 2026-07-23

- Increase the control-window height and keep the full setup section reachable.
- Add live clip, physical microphone, and virtual microphone level meters.
- Add independent clip-to-microphone and clip-to-speaker volume controls.
- Add optional clip monitoring through the default physical speakers.
- Add a dedicated sound-clip refresh action to the control window.
- Report audio initialization stages and slow-start recovery guidance while devices open asynchronously.
- Auto-detect an eligible Apple signing identity during macOS builds when one is installed.
- Prevent a short Core Audio property query from deadlocking later Static Microphone clients.

## 0.1.4 - 2026-07-23

- Show sound clip loading, playing, finished, stopped, and error state in the control window.
- Report mixer playback start, completion, replacement, and stop events in Activity.
- Prevent a stale MP3 decode from playing after Stop or after a newer clip request.
- Add deterministic MP3 decoding and mixer lifecycle regression tests.
- Route automatically only to Static Microphone; third-party loopbacks require explicit selection.
- Keep Static Microphone installation optional at build time and bundled inside the portable app.
