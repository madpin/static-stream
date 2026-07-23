# Static Stream Architecture

Static Stream keeps portable media logic in Rust and isolates macOS-only publication and UI work
behind small native components. The application does not download or depend on BlackHole, OBS, or
another virtual-device package.

## System Context

```text
                                          +----------------------+
Physical camera --> AVFoundation frames ->| Static Camera        |--> Meeting app
                                          | CMIO system extension|
Controller --> app-group Freeze state ----|                      |
                                          +----------------------+

Physical microphone --> CPAL input --> resampler --> voice DSP --+
                                                                 +--> mix --> Core Audio output
Decoded sound clip ----------------------------------------------+              |
                                                            v
                                              StaticStreamAudio.driver
                                                  output -> input
                                                            |
                                                            v
                                                        Meeting app

Decoded sound clip --> independent gain --> default speakers (optional)
```

## Bundle And Processes

`Static Stream.app` contains these executables:

| Component | Responsibility |
| --- | --- |
| `static-stream` | Rust controller, WebKit window, menu-bar item, hotkeys, audio engine |
| `static-stream-activate` | Requests camera system-extension activation/deactivation |
| `static-stream-audio-install` | Installs, replaces, or removes owned Core Audio bundles |
| `static-stream-probe` | Probes AVFoundation camera availability outside the controller |
| `static-stream-camera-test` | Runs the unsigned live/Freeze/resume selector test |
| `StaticStreamCameraExtension` | Publishes Static Camera through Core Media I/O |
| `StaticStreamAudio` | Publishes Static Microphone as a Core Audio AudioServerPlugIn |

The helper executables keep Swift framework details out of the portable Rust state machine and make
installer and probe results explicit process outputs.

## Controller

`src/state.rs` owns platform-neutral user actions and effects. `src/macos.rs` maps window IPC,
menu commands, and global shortcuts into the same action path. This prevents keyboard, menu, and GUI
controls from drifting into separate behavior.

The WebKit view in `assets/app.html` is a local resource with no network dependency. Rust sends a
serialized `GuiState`; JavaScript renders controls and sends small command messages back. The
Activity log is bounded to 250 in-memory events.

Update checks are the only network operation initiated by the controller. They run on a background
thread after startup or on request; WebKit never receives network access or downloads executable
content.

## Audio Path

`src/audio` uses CPAL for physical input and virtual-device output. Different device sample rates
are bridged by the streaming resampler. The voice processor transforms only physical-microphone
samples. The mixer then combines or replaces that signal with a decoded clip, applies the
virtual-microphone clip gain, and publishes a final level meter.

Voice presets use a preallocated dual-grain pitch shifter, one-pole filters, oscillators, and a
cubic saturator. Effect changes crossfade between two preallocated processing lanes. Intensity and
wet/dry mix use per-sample linear smoothing. Clean bypass is sample-exact. All voice settings travel
through the same bounded command queue as mute and clip gain; successful application returns a
status event to the Activity log.

The optional speaker path receives the same decoded clip through an independent gain. It never
receives physical-microphone samples.

Audio callbacks use preallocated buffers and bounded queues. The AudioServerPlugIn uses a fixed
lock-free ring to connect the Core Audio output stream to the input stream. Disk I/O, decoding, GUI
updates, and logging stay outside real-time callbacks.

Each callback computes local clip, physical-microphone, processed-voice, and final-output peaks,
then performs one atomic maximum update per meter. Output buffers larger than the preallocated
scratch space are processed in fixed chunks; the callback never resizes a vector.

## Clip Lifecycle And Progress

```text
button / shortcut
    -> PlayClip action
    -> background Symphonia decode
    -> audio-engine queue
    -> ClipStarted(name, duration_ms)
    -> direct event-loop status forwarding
    -> monotonic controller clock
    -> GuiState.clip_progress
    -> button background scaleX(progress)
    -> ClipFinished / ClipStopped / ClipError
```

Progress begins at `ClipStarted`, not at the button click. This distinction keeps a slow decoder or
audio-device startup from appearing as audio already played. The controller derives progress from a
monotonic `Instant` and the decoded clip duration, clamps it to `[0, 1]`, and refreshes the GUI while
playback is active. JavaScript updates the existing button's CSS custom property instead of
rebuilding the clip list on each tick, preserving focus and keyboard behavior.

Audio statuses are delivered immediately by a blocking forwarding thread and tagged with the audio
engine generation, so a replaced engine cannot update current state. Continuous meters and progress
use a separate telemetry message rather than serializing devices, clips, setup text, and the
Activity log. Telemetry runs at 10 Hz only while the window is visible and audio is ready; hidden or
startup maintenance runs at 2 Hz without updating WebKit.

## Camera Path

The Core Media I/O extension captures a physical camera through AVFoundation and publishes frames as
**Static Camera**. A shared `FreezeFrameSelector<Frame>` implements the transition contract:

1. Live frames replace the held frame and pass through.
2. Entering Freeze returns the most recently held frame.
3. Further physical frames do not replace that held frame while frozen.
4. Returning to Live passes the new current frame and updates the held frame.

The extension instantiates this selector with `CVPixelBuffer`. The unsigned development helper
compiles the same source file with synthetic frame identifiers and verifies the transition sequence.
This tests the freeze decision without pretending that macOS has published an unsigned device.

The controller and extension exchange Freeze state through the team-prefixed app group
`TEAMID.group.com.madpin.staticstream`. The extension must therefore be built with the same team
prefix and provisioning configuration as the host.

## Installation And Upgrades

The audio installer writes the bundled driver to
`/Library/Audio/Plug-Ins/HAL/StaticStreamAudio.driver`, removes recognized older Static Stream
bundle names, and restarts Core Audio. It does not inspect or remove unrelated virtual audio
products.

The camera uses Apple's system-extension activation API and the stable identifier
`com.madpin.staticstream.camera`. macOS owns final approval, replacement, and removal.

### Application Updates

```text
GitHub latest.json
    -> semantic-version comparison
    -> GitHub release ZIP download
    -> SHA-256 verification
    -> extract to a unique temporary directory
    -> bundle ID, version, code-signature, Team ID, and Gatekeeper checks
    -> detached installer waits for the controller to exit
    -> copy, atomic rename, rollback on activation failure
    -> reopen Static Stream
```

`src/updates.rs` owns this path. The release manifest checksum detects transfer or storage
corruption. Authenticity does not depend on that manifest alone: the extracted bundle must carry a
valid Apple code signature from the same Team ID compiled into the running app, and Gatekeeper must
accept the notarized app. Download URLs are restricted to the Static Stream GitHub repository.

The installer stages a complete app beside the installed bundle before renaming anything. It first
renames the current bundle to a backup, activates the staged bundle, restores the backup if that
rename fails, and deletes the backup only after activation succeeds. Standard users need a writable
installation directory; Static Stream does not add a persistent privileged update service.

Release CI builds both macOS architectures, signs the nested code inside-out with hardened runtime,
notarizes and staples the app and DMG, and publishes the DMG, updater ZIP, and `latest.json` to a
tagged GitHub Release.

## Portability Boundary

Reusable components:

- action/effect state machine
- configuration and clip discovery
- Symphonia decoding
- voice effects, mixer, resampling, level metering, and clip lifecycle
- bounded event and audio transport

macOS-specific components:

- WebKit/AppKit shell and menu-bar item
- global shortcut registration
- Core Audio driver and privileged installer
- AVFoundation camera discovery
- Core Media I/O camera extension and system-extension activation

A Linux port should replace the publication layer with PipeWire and a video loopback endpoint. A
Windows port should use WASAPI for audio and Media Foundation for the virtual camera. Those adapters
must preserve the controller's existing action, status, and installer contracts rather than fork
the media engine.

## Security Model

Audio installation requires one administrator-approved write to the system HAL plug-in directory.
Camera publication requires an Apple team signature, matching entitlements, and user approval
because it crosses a system extension boundary. Static Stream does not work around these controls.

Configuration is stored under the user's application-support directory. Activity events are
in-memory only. Audio samples and camera frames are not written to disk.
