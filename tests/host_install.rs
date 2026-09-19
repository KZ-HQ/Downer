//! Black-box tests for `downer install-host` and `downer uninstall-host`.
//!
//! The acceptance criterion these stand in for — "the extension connects with
//! the repository gone" — is a whole-machine claim no test can make without
//! taking over the developer's real Firefox profile. What is testable is
//! everything the claim rests on: the manifest Firefox reads, the launcher it
//! runs, and whether that launcher actually starts the host protocol. Each test
//! runs the real binary under a temporary `HOME`, so nothing here touches the
//! machine's own registration.

#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::Value;
use tempfile::TempDir;

/// A throwaway `HOME` with every directory the installer resolves pointed
/// inside it.
///
/// The XDG variables are cleared rather than left alone: a developer machine
/// that sets `XDG_DATA_HOME` would otherwise have this test write into their
/// real data directory, which is exactly what the temporary home is for.
struct Home {
    directory: TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            directory: TempDir::new().expect("temporary home"),
        }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn downer(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin("downer"));
        command
            .env("HOME", self.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_BIN_HOME")
            .env_remove("DOWNER_FFMPEG")
            // Without this, a binary built into a Cargo home inside the real
            // `HOME` would be judged already-stable and never copied.
            .env("CARGO_HOME", self.path().join("cargo-home"));
        command
    }

    fn install(&self, arguments: &[&str]) -> String {
        let output = self
            .downer()
            .arg("install-host")
            .args(arguments)
            .output()
            .expect("install-host runs");
        assert!(
            output.status.success(),
            "install-host failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn manifest_path(&self) -> PathBuf {
        let directory = if cfg!(target_os = "macos") {
            self.path()
                .join("Library")
                .join("Application Support")
                .join("Mozilla")
                .join("NativeMessagingHosts")
        } else {
            self.path().join(".mozilla").join("native-messaging-hosts")
        };
        directory.join("com.downer.native.json")
    }

    fn manifest(&self) -> Value {
        let contents = fs::read_to_string(self.manifest_path()).expect("manifest was written");
        serde_json::from_str(&contents).expect("manifest is JSON")
    }

    fn data_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.path().join("Library").join("Application Support")
        } else {
            self.path().join(".local").join("share")
        }
    }

    fn launcher_path(&self) -> PathBuf {
        self.data_dir().join("downer").join("com.downer.native.sh")
    }

    fn binary_path(&self) -> PathBuf {
        self.data_dir().join("downer").join("bin").join("downer")
    }

    /// macOS has one `Application Support` directory for both data and config,
    /// which is what `dirs` reports and therefore where the host writes; only
    /// Linux splits them.
    fn config_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.data_dir()
        } else {
            self.path().join(".config")
        }
    }

    fn config_path(&self) -> PathBuf {
        self.config_dir().join("downer").join("config.json")
    }
}

/// An executable file that is not FFmpeg but is a path to one as far as the
/// installer can tell. It never runs here; only its existence is checked.
fn fake_executable(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").expect("fake executable written");
    set_executable(&path);
    path
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[test]
fn install_writes_a_manifest_firefox_can_use() {
    let home = Home::new();
    home.install(&[]);

    let manifest = home.manifest();
    assert_eq!(manifest["name"], "com.downer.native");
    assert_eq!(manifest["type"], "stdio");
    assert_eq!(manifest["allowed_extensions"][0], extension_id());
    assert_eq!(
        manifest["path"],
        home.launcher_path().to_string_lossy().as_ref(),
        "the manifest points at the launcher"
    );
}

/// The extension ID in the manifest is the extension's own, not a copy that can
/// drift. A mismatch means Firefox refuses the connection with a message that
/// names neither side, so it is worth a test of its own.
#[test]
fn the_manifest_allows_exactly_the_shipped_extension() {
    let extension: Value = serde_json::from_str(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("extension")
                .join("manifest.json"),
        )
        .expect("extension manifest is readable"),
    )
    .expect("extension manifest is JSON");
    assert_eq!(
        extension["browser_specific_settings"]["gecko"]["id"],
        extension_id(),
        "extension/manifest.json and downer::host::EXTENSION_ID must agree"
    );
}

fn extension_id() -> &'static str {
    downer::host::EXTENSION_ID
}

/// The whole point of the change: what Firefox launches must not live in the
/// checkout, so the binary is copied out of `target/` and the launcher runs the
/// copy.
#[test]
fn install_copies_the_binary_out_of_the_build_directory() {
    let home = Home::new();
    let report = home.install(&[]);

    let binary = home.binary_path();
    assert!(binary.is_file(), "{} exists", binary.display());
    assert!(is_executable(&binary), "the installed binary is executable");
    assert!(
        report.contains(&binary.to_string_lossy().to_string()),
        "the report names the installed binary: {report}"
    );

    let launcher = fs::read_to_string(home.launcher_path()).expect("launcher was written");
    assert!(
        launcher.contains(&format!("exec '{}' --native-host", binary.display())),
        "the launcher runs the installed copy: {launcher}"
    );
}

/// `--dev` is what `make extension-install` uses: the developer's rebuilt
/// binary must be the one Firefox runs, with no copy to go stale.
#[test]
fn dev_install_references_the_running_binary() {
    let home = Home::new();
    home.install(&["--dev"]);

    let running = assert_cmd::cargo::cargo_bin("downer");
    let launcher = fs::read_to_string(home.launcher_path()).expect("launcher was written");
    assert!(
        launcher.contains(&format!("exec '{}' --native-host", running.display())),
        "the launcher runs the build directory binary: {launcher}"
    );
    assert!(!home.binary_path().exists(), "a dev install copies nothing");
    assert!(
        home.manifest()["description"]
            .as_str()
            .expect("description is a string")
            .contains("development"),
        "the manifest says which kind of registration this is"
    );
}

/// The launcher is the only part of the chain this suite can execute, and it is
/// the part most easily got wrong: Firefox appends its own arguments, and the
/// host must still start. An empty stdin is a closed port, which the host
/// treats as a clean shutdown.
#[test]
fn the_launcher_starts_the_native_host() {
    let home = Home::new();
    home.install(&[]);

    let status = Command::new(home.launcher_path())
        .arg(home.manifest_path())
        .arg(extension_id())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("launcher runs");
    assert!(status.success(), "the launcher exits cleanly: {status}");
}

/// `make extension` installs on every run, so a second install must not fail,
/// duplicate anything, or change what the first one wrote.
#[test]
fn installing_twice_leaves_the_same_registration() {
    let home = Home::new();
    home.install(&[]);
    let first_manifest = fs::read_to_string(home.manifest_path()).expect("manifest");
    let first_launcher = fs::read_to_string(home.launcher_path()).expect("launcher");

    home.install(&[]);
    assert_eq!(
        first_manifest,
        fs::read_to_string(home.manifest_path()).expect("manifest")
    );
    assert_eq!(
        first_launcher,
        fs::read_to_string(home.launcher_path()).expect("launcher")
    );
}

#[test]
fn install_records_the_ffmpeg_path_for_the_host() {
    let home = Home::new();
    let tools = TempDir::new().expect("tools directory");
    let ffmpeg = fake_executable(tools.path(), "ffmpeg");

    home.install(&["--ffmpeg", ffmpeg.to_str().expect("utf-8 path")]);

    let config: Value =
        serde_json::from_str(&fs::read_to_string(home.config_path()).expect("config was written"))
            .expect("config is JSON");
    assert_eq!(
        config["ffmpeg"],
        ffmpeg
            .canonicalize()
            .expect("canonical path")
            .to_string_lossy()
            .as_ref()
    );
}

/// A path that is not there is a typo, and writing it down would turn it into a
/// download failure much later, with FFmpeg named as the culprit.
#[test]
fn install_refuses_an_ffmpeg_that_is_not_there() {
    let home = Home::new();
    let output = home
        .downer()
        .args(["install-host", "--ffmpeg", "/nonexistent/ffmpeg"])
        .output()
        .expect("install-host runs");

    assert_eq!(output.status.code(), Some(downer::INVALID_INPUT_EXIT));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("/nonexistent/ffmpeg"), "{stderr}");
    assert!(
        !home.manifest_path().exists(),
        "nothing is registered when the arguments are wrong"
    );
}

/// A bare name would be resolved against the environment Firefox hands the
/// host, which is the minimal environment the config file exists to work
/// around. Accepting it would record a setting that silently does nothing.
#[test]
fn install_refuses_a_bare_ffmpeg_name() {
    let home = Home::new();
    let output = home
        .downer()
        .args(["install-host", "--ffmpeg", "ffmpeg"])
        .output()
        .expect("install-host runs");

    assert_eq!(output.status.code(), Some(downer::INVALID_INPUT_EXIT));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--ffmpeg"),
        "the error says which argument is wrong"
    );
}

#[test]
fn uninstall_removes_the_registration_and_the_config() {
    let home = Home::new();
    let tools = TempDir::new().expect("tools directory");
    let ffmpeg = fake_executable(tools.path(), "ffmpeg");
    home.install(&["--ffmpeg", ffmpeg.to_str().expect("utf-8 path")]);

    let output = home
        .downer()
        .arg("uninstall-host")
        .output()
        .expect("uninstall-host runs");
    assert!(output.status.success());

    assert!(!home.manifest_path().exists(), "manifest is gone");
    assert!(!home.launcher_path().exists(), "launcher is gone");
    assert!(!home.config_path().exists(), "config is gone");
    assert!(
        home.binary_path().exists(),
        "the binary stays unless --binary asks for it"
    );
}

#[test]
fn uninstall_can_remove_the_installed_binary_and_repeats_cleanly() {
    let home = Home::new();
    home.install(&[]);

    let first = home
        .downer()
        .args(["uninstall-host", "--binary"])
        .output()
        .expect("uninstall-host runs");
    assert!(first.status.success());
    assert!(!home.binary_path().exists(), "the binary is gone");

    let second = home
        .downer()
        .args(["uninstall-host", "--binary"])
        .output()
        .expect("uninstall-host runs");
    assert!(
        second.status.success(),
        "a second uninstall is not an error"
    );
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("nothing to remove"),
        "it says it found nothing"
    );
}

/// `downer URL [options]` is the interface (AGENTS.md); adding subcommands must
/// not have moved the download behind one.
#[test]
fn a_url_is_still_the_default_command() {
    let output = Command::new(assert_cmd::cargo::cargo_bin("downer"))
        .arg("not-a-url")
        .output()
        .expect("downer runs");
    assert_eq!(
        output.status.code(),
        Some(downer::INVALID_INPUT_EXIT),
        "a bad URL is still a URL error, not an unknown subcommand: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
