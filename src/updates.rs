use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const UPDATE_FEED_URL: &str =
    "https://github.com/madpin/static-stream/releases/latest/download/latest.json";
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/madpin/static-stream/releases/download/";
const BUNDLE_ID: &str = "com.madpin.staticstream";
const APP_BUNDLE_NAME: &str = "Static Stream.app";
const UPDATE_ARCHIVE_LIMIT_BYTES: &str = "268435456";
const INSTALLER_MODE: &str = "--apply-update";

const TEAM_IDENTIFIER_PREFIX: &str = match option_env!("STATIC_STREAM_TEAM_PREFIX") {
    Some(prefix) => prefix,
    None => "",
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub pub_date: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug)]
pub struct StagedUpdate {
    source_bundle: PathBuf,
    target_bundle: PathBuf,
    working_directory: PathBuf,
}

pub fn check_for_update(current_version: &str) -> anyhow::Result<Option<UpdateManifest>> {
    let output = Command::new("/usr/bin/curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--max-filesize",
            "1048576",
            "--user-agent",
            concat!("Static-Stream/", env!("CARGO_PKG_VERSION")),
            UPDATE_FEED_URL,
        ])
        .output()
        .context("could not start the update request")?;
    require_success(&output, "update request")?;
    let manifest: UpdateManifest =
        serde_json::from_slice(&output.stdout).context("the update feed is invalid")?;
    manifest.validate()?;

    let current = Version::parse(current_version).context("the current app version is invalid")?;
    let available = Version::parse(&manifest.version).context("the update version is invalid")?;
    Ok((available > current).then_some(manifest))
}

pub fn installation_support() -> anyhow::Result<PathBuf> {
    let expected_team =
        expected_team_id().context("automatic installation requires an Apple-team-signed build")?;
    let bundle = current_app_bundle().context("run Static Stream from its macOS app bundle")?;
    let actual_team = team_identifier(&bundle)?;
    if actual_team != expected_team {
        bail!("the running app is signed by Apple team {actual_team}, expected {expected_team}");
    }
    let parent = bundle
        .parent()
        .context("the running app has no installation directory")?;
    let writable = Command::new("/usr/bin/test")
        .arg("-w")
        .arg(parent)
        .status()
        .context("could not inspect the installation directory")?
        .success();
    if !writable {
        bail!("the app installation directory is not writable");
    }
    Ok(bundle)
}

pub fn stage_update(manifest: &UpdateManifest) -> anyhow::Result<StagedUpdate> {
    manifest.validate()?;
    let target_bundle = installation_support()?;
    let expected_team =
        expected_team_id().context("automatic installation requires Apple signing")?;
    let working_directory = update_working_directory()?;
    fs::create_dir_all(&working_directory).with_context(|| {
        format!(
            "could not create update directory {}",
            working_directory.display()
        )
    })?;

    let archive = working_directory.join("Static-Stream-update.zip");
    let result = (|| {
        let output = Command::new("/usr/bin/curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "10",
                "--max-time",
                "300",
                "--max-filesize",
                UPDATE_ARCHIVE_LIMIT_BYTES,
                "--user-agent",
                concat!("Static-Stream/", env!("CARGO_PKG_VERSION")),
                "--output",
            ])
            .arg(&archive)
            .arg(&manifest.url)
            .output()
            .context("could not start the update download")?;
        require_success(&output, "update download")?;

        let actual_checksum = sha256_file(&archive)?;
        if !actual_checksum.eq_ignore_ascii_case(&manifest.sha256) {
            bail!("the downloaded update checksum does not match the release");
        }

        let extracted = working_directory.join("extracted");
        fs::create_dir(&extracted).context("could not prepare the update extraction directory")?;
        let output = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(&archive)
            .arg(&extracted)
            .output()
            .context("could not start update extraction")?;
        require_success(&output, "update extraction")?;

        let source_bundle = extracted.join(APP_BUNDLE_NAME);
        verify_downloaded_bundle(&source_bundle, &manifest.version, expected_team)?;
        Ok(StagedUpdate {
            source_bundle,
            target_bundle,
            working_directory: working_directory.clone(),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&working_directory);
    }
    result
}

pub fn launch_installer(update: &StagedUpdate) -> anyhow::Result<()> {
    let current_executable = std::env::current_exe().context("could not locate Static Stream")?;
    let installer = update
        .working_directory
        .join("static-stream-update-installer");
    fs::copy(&current_executable, &installer).context("could not prepare the update installer")?;

    let mut permissions = fs::metadata(&installer)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(&installer, permissions)?;

    Command::new(&installer)
        .arg(INSTALLER_MODE)
        .arg(&update.source_bundle)
        .arg(&update.target_bundle)
        .arg(std::process::id().to_string())
        .arg(&update.working_directory)
        .spawn()
        .context("could not start the update installer")?;
    Ok(())
}

#[must_use]
pub fn run_installer_from_args() -> Option<anyhow::Result<()>> {
    let mut arguments = std::env::args_os();
    let _ = arguments.next();
    if arguments.next().as_deref() != Some(OsStr::new(INSTALLER_MODE)) {
        return None;
    }

    let result = (|| {
        let source_bundle = next_path(&mut arguments, "source app bundle")?;
        let target_bundle = next_path(&mut arguments, "target app bundle")?;
        let parent_pid = arguments
            .next()
            .context("missing parent process identifier")?
            .to_string_lossy()
            .parse::<u32>()
            .context("invalid parent process identifier")?;
        let working_directory = next_path(&mut arguments, "update working directory")?;
        if arguments.next().is_some() {
            bail!("unexpected update installer arguments");
        }
        apply_staged_update(
            &source_bundle,
            &target_bundle,
            parent_pid,
            &working_directory,
        )
    })();
    Some(result)
}

fn apply_staged_update(
    source_bundle: &Path,
    target_bundle: &Path,
    parent_pid: u32,
    working_directory: &Path,
) -> anyhow::Result<()> {
    wait_for_process_exit(parent_pid, Duration::from_secs(60))?;

    let parent = target_bundle
        .parent()
        .context("target app has no installation directory")?;
    let staged_bundle = parent.join(format!(".{APP_BUNDLE_NAME}.update-{parent_pid}"));
    let backup_bundle = parent.join(format!(".{APP_BUNDLE_NAME}.previous"));
    remove_if_present(&staged_bundle)?;
    remove_if_present(&backup_bundle)?;

    let output = Command::new("/usr/bin/ditto")
        .arg(source_bundle)
        .arg(&staged_bundle)
        .output()
        .context("could not copy the staged update")?;
    require_success(&output, "staged app copy")?;

    activate_staged_bundle(&staged_bundle, target_bundle, &backup_bundle)?;
    let _ = fs::remove_dir_all(&backup_bundle);
    let _ = fs::remove_dir_all(working_directory);
    Command::new("/usr/bin/open")
        .arg(target_bundle)
        .spawn()
        .context("the update installed, but Static Stream could not reopen")?;
    Ok(())
}

fn activate_staged_bundle(
    staged_bundle: &Path,
    target_bundle: &Path,
    backup_bundle: &Path,
) -> anyhow::Result<()> {
    fs::rename(target_bundle, backup_bundle).context("could not preserve the current app")?;
    if let Err(error) = fs::rename(staged_bundle, target_bundle) {
        let _ = fs::rename(backup_bundle, target_bundle);
        return Err(error).context("could not activate the staged app");
    }
    Ok(())
}

impl UpdateManifest {
    fn validate(&self) -> anyhow::Result<()> {
        Version::parse(&self.version).context("update version is not semantic")?;
        if self.pub_date.trim().is_empty() {
            bail!("update publication date is empty");
        }
        if !self.url.starts_with(RELEASE_DOWNLOAD_PREFIX) {
            bail!("update download is not hosted by the Static Stream GitHub repository");
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("update checksum is invalid");
        }
        Ok(())
    }
}

fn verify_downloaded_bundle(
    bundle: &Path,
    expected_version: &str,
    expected_team: &str,
) -> anyhow::Result<()> {
    if !bundle.is_dir() {
        bail!("the update archive does not contain {APP_BUNDLE_NAME}");
    }

    let output = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(bundle)
        .output()
        .context("could not verify the update signature")?;
    require_success(&output, "update code-signature verification")?;

    let bundle_id = bundle_plist_value(bundle, "CFBundleIdentifier")?;
    if bundle_id != BUNDLE_ID {
        bail!("the downloaded app has bundle identifier {bundle_id}");
    }
    let bundle_version = bundle_plist_value(bundle, "CFBundleShortVersionString")?;
    if bundle_version != expected_version {
        bail!("the downloaded app version is {bundle_version}, expected {expected_version}");
    }
    let team = team_identifier(bundle)?;
    if team != expected_team {
        bail!("the downloaded app is signed by Apple team {team}, expected {expected_team}");
    }

    let output = Command::new("/usr/sbin/spctl")
        .args(["--assess", "--type", "execute", "--verbose=2"])
        .arg(bundle)
        .output()
        .context("could not ask Gatekeeper to assess the update")?;
    require_success(&output, "Gatekeeper update assessment")
}

fn current_app_bundle() -> Option<PathBuf> {
    app_bundle_for_executable(&std::env::current_exe().ok()?)
}

fn app_bundle_for_executable(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

fn expected_team_id() -> Option<&'static str> {
    let team = TEAM_IDENTIFIER_PREFIX.trim_end_matches('.');
    (!team.is_empty()).then_some(team)
}

fn team_identifier(bundle: &Path) -> anyhow::Result<String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(bundle)
        .output()
        .context("could not inspect the app signature")?;
    require_success(&output, "app signature inspection")?;
    let details = String::from_utf8_lossy(&output.stderr);
    parse_team_identifier(&details).context("the app signature has no Apple team identifier")
}

fn parse_team_identifier(details: &str) -> Option<String> {
    details
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

fn bundle_plist_value(bundle: &Path, key: &str) -> anyhow::Result<String> {
    let plist = bundle.join("Contents/Info.plist");
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", &format!("Print :{key}")])
        .arg(plist)
        .output()
        .with_context(|| format!("could not inspect update property {key}"))?;
    require_success(&output, "update property inspection")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let file = File::open(path)
        .with_context(|| format!("could not open {} for verification", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1_024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn update_working_directory() -> anyhow::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("the system clock is before 1970")?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "static-stream-update-{}-{timestamp}",
        std::process::id()
    )))
}

fn next_path(
    arguments: &mut impl Iterator<Item = OsString>,
    label: &str,
) -> anyhow::Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .with_context(|| format!("missing {label}"))
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> anyhow::Result<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let running = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success());
        if !running {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("Static Stream did not exit before the update timeout")
}

fn remove_if_present(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("could not remove stale update {}", path.display()))
        }
    }
}

fn require_success(output: &Output, operation: &str) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        bail!("{operation} failed with {}", output.status);
    }
    bail!("{operation} failed: {detail}")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn manifest(version: &str) -> UpdateManifest {
        UpdateManifest {
            version: version.into(),
            notes: "Changes".into(),
            pub_date: "2026-07-23T20:00:00Z".into(),
            url: format!(
                "{RELEASE_DOWNLOAD_PREFIX}v{version}/Static-Stream-{version}-macos-universal.zip"
            ),
            sha256: "ab".repeat(32),
        }
    }

    #[test]
    fn newer_semantic_version_is_available() {
        let candidate = manifest("0.2.0");
        candidate.validate().unwrap();
        assert!(Version::parse(&candidate.version).unwrap() > Version::parse("0.1.9").unwrap());
    }

    #[test]
    fn manifest_rejects_an_untrusted_download_host() {
        let mut candidate = manifest("0.2.0");
        candidate.url = "https://example.com/Static-Stream.zip".into();
        assert!(
            candidate
                .validate()
                .unwrap_err()
                .to_string()
                .contains("GitHub repository")
        );
    }

    #[test]
    fn finds_app_bundle_above_executable() {
        assert_eq!(
            app_bundle_for_executable(Path::new(
                "/Applications/Static Stream.app/Contents/MacOS/static-stream"
            )),
            Some(PathBuf::from("/Applications/Static Stream.app"))
        );
        assert_eq!(
            app_bundle_for_executable(Path::new("/tmp/static-stream")),
            None
        );
    }

    #[test]
    fn parses_only_real_team_identifiers() {
        assert_eq!(
            parse_team_identifier("Executable=x\nTeamIdentifier=ABC123\n"),
            Some("ABC123".into())
        );
        assert_eq!(parse_team_identifier("TeamIdentifier=not set\n"), None);
    }

    #[test]
    fn hashes_update_archives() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.zip");
        File::create(&path)
            .unwrap()
            .write_all(b"static stream")
            .unwrap();

        assert_eq!(
            sha256_file(&path).unwrap(),
            "5a0c6f5e84a9f973f56488953f9bdfa8c6619d7a33bbdf78ae229aa524a72fff"
        );
    }

    #[test]
    fn failed_activation_restores_the_installed_bundle() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(APP_BUNDLE_NAME);
        let missing_staged = directory.path().join("missing.app");
        let backup = directory.path().join("backup.app");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("version"), "current").unwrap();

        assert!(
            activate_staged_bundle(&missing_staged, &target, &backup)
                .unwrap_err()
                .to_string()
                .contains("activate")
        );
        assert_eq!(
            fs::read_to_string(target.join("version")).unwrap(),
            "current"
        );
        assert!(!backup.exists());
    }

    #[test]
    fn successful_activation_preserves_a_removable_backup() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(APP_BUNDLE_NAME);
        let staged = directory.path().join("staged.app");
        let backup = directory.path().join("backup.app");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("version"), "current").unwrap();
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("version"), "new").unwrap();

        activate_staged_bundle(&staged, &target, &backup).unwrap();

        assert_eq!(fs::read_to_string(target.join("version")).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(backup.join("version")).unwrap(),
            "current"
        );
    }
}
