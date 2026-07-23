# Releases And Automatic Updates

Static Stream publishes macOS releases through
[GitHub Releases](https://github.com/madpin/static-stream/releases). A release contains:

- `Static-Stream-VERSION-macos-universal.dmg` for installation;
- `Static-Stream-VERSION-macos-universal.zip` for the in-app updater;
- `latest.json`, the version, URL, publication date, and SHA-256 update manifest.

Both app artifacts contain Apple Silicon and Intel executables. The workflow signs the host, helper
tools, bundled audio driver, and camera extension with a Developer ID Application certificate,
enables hardened runtime, notarizes the app and DMG with Apple, and staples the notarization tickets.

## Development Artifacts

Every successful commit to `main` creates a visible GitHub pre-release tagged
`development-VERSION-SHORT_SHA` and uploads a
**Static-Stream-macOS-universal-development** artifact to its GitHub Actions CI run. Both contain a
universal DMG, updater-shaped ZIP, and `SHA256SUMS-development.txt`; GitHub retains the Actions
artifact for 30 days while the pre-release remains in release history. This provides a downloadable
build for every commit before Apple distribution credentials are configured.

Development artifacts are ad-hoc signed. They can test the window, menu bar, keyboard controls,
audio engine, bundled microphone installation, clips, voice effects, and unsigned camera test.
They cannot activate Static Camera, pass Gatekeeper like a notarized release, or install themselves
through the updater. Development builds are marked as pre-releases and never publish `latest.json`,
so installed production builds will not mistake one for a release.

## One-Time Apple Setup

Distribution requires an active Apple Developer Program membership.

1. Create a **Developer ID Application** certificate and export it with its private key from
   Keychain Access as a password-protected `.p12`.
2. Register the host identifier `com.madpin.staticstream` with the System Extension capability.
3. Register the camera extension identifier `com.madpin.staticstream.camera`.
4. Configure the shared app group `TEAMID.group.com.madpin.staticstream`.
5. Create Developer ID provisioning profiles for the host and camera extension whose entitlements
   match [the signing guide](camera-signing.md).
6. Create an app-specific password for the Apple ID used by `notarytool`.

Apple documents the distribution requirements in
[Developer ID](https://developer.apple.com/developer-id/) and
[Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## GitHub Actions Secrets

Configure these repository secrets:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64-encoded Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | Password used when exporting the `.p12` |
| `APPLE_TEAM_ID` | Ten-character Apple Developer team identifier |
| `APP_PROVISIONING_PROFILE` | Base64-encoded host Developer ID profile |
| `CAMERA_PROVISIONING_PROFILE` | Base64-encoded camera-extension Developer ID profile |
| `APPLE_ID` | Apple ID used for notarization |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password for that Apple ID |

On macOS, encode and upload the files without writing their Base64 values to shell history:

```sh
base64 -i StaticStreamDeveloperID.p12 | gh secret set APPLE_CERTIFICATE
gh secret set APPLE_CERTIFICATE_PASSWORD
gh secret set APPLE_TEAM_ID
base64 -i StaticStreamApp.provisionprofile | gh secret set APP_PROVISIONING_PROFILE
base64 -i StaticStreamCamera.provisionprofile | gh secret set CAMERA_PROVISIONING_PROFILE
gh secret set APPLE_ID
gh secret set APPLE_APP_SPECIFIC_PASSWORD
```

The release workflow deliberately fails when any secret is absent. It never falls back to an
ad-hoc public build.

## Publish A Release

The Git tag must exactly match the version in `Cargo.toml`.

```sh
# After changing Cargo.toml, Cargo.lock, and CHANGELOG.md:
./scripts/check.sh
git tag v0.1.9
git push origin main v0.1.9
```

Pushing `v0.1.9` starts `.github/workflows/release.yml`. The workflow validates the tag, tests the
code, builds the universal bundle, signs and notarizes it, packages the DMG and ZIP, generates
`latest.json`, and creates the GitHub Release. It can also be rerun from **Actions > Release > Run
workflow** by selecting an existing tag.

Do not move a published version tag to different code. The updater uses semantic versions and a
release version identifies one immutable signed build.

## Update Experience

With **Check automatically** enabled, the app requests:

```text
https://github.com/madpin/static-stream/releases/latest/download/latest.json
```

five seconds after launch. Checks have short connection and total timeouts and do not block audio,
camera, menu, or window work. The user can also choose **Check now** in the window or **Check for
updates...** in the menu-bar menu.

Static Stream presents an available update but does not install it silently. **Install & restart**
downloads the universal ZIP and verifies:

1. the manifest is semantic and points into this repository's GitHub Release downloads;
2. the ZIP matches the manifest SHA-256;
3. the archive contains `Static Stream.app` with bundle ID `com.madpin.staticstream`;
4. the app version matches the manifest;
5. `codesign --verify --deep --strict` accepts the complete bundle;
6. its Apple Team ID matches the Team ID compiled into the running app;
7. Gatekeeper accepts the notarized app.

After verification, a detached copy of the app binary waits for Static Stream to exit, copies the
new bundle beside the installed one, preserves the current bundle as a temporary backup, activates
the new bundle with an atomic rename, and reopens it. An activation failure restores the backup.

## Development Builds

Ad-hoc and `cargo run` builds can exercise update checks and all update UI states, but automatic
installation is disabled. This is intentional: they have no stable Apple Team ID with which to
authenticate a downloaded replacement.

Run the normal local build and inspect its version:

```sh
./scripts/build-macos.sh
/usr/libexec/PlistBuddy \
  -c 'Print :CFBundleShortVersionString' \
  "dist/Static Stream.app/Contents/Info.plist"
```

For a release-equivalent local build, provide the same signing inputs used by CI and set
`RELEASE_SIGNING=1`. This mode requires a Developer ID Application identity and both provisioning
profiles, requests secure timestamps, and enables hardened runtime.

## Recovery

If an update check fails, the installed app continues running and records the error in **Activity**.
Network errors do not affect media routing.

If replacement cannot start, Static Stream leaves the installed app untouched. If the final rename
fails after preserving the existing bundle, it restores that bundle. A failure after the controller
has exited is reported by the detached helper on stderr; relaunch the existing app normally and
inspect the installation directory for a hidden `.Static Stream.app.previous` backup only if the
automatic rollback itself was interrupted.
