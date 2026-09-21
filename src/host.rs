//! Installing and removing the Firefox native messaging host.
//!
//! Firefox finds a native host through a manifest in a well-known per-user
//! directory whose `path` must stay valid for as long as the extension is
//! installed. The first implementation pointed that path at a shell script
//! inside the repository checkout, so moving the checkout, deleting it, or
//! running `cargo clean` silently broke every download. This module makes the
//! registration independent of the checkout: it copies the running binary to a
//! stable per-user location (unless asked to reference it where it is), writes
//! a launcher beside it, and points the manifest at the launcher.
//!
//! The launcher exists because Firefox invokes the manifest's `path` with its
//! own arguments — the manifest path and the extension ID — and never with
//! `--native-host`. Rather than guess at the meaning of Firefox's argv, the
//! launcher supplies the flag, so the host is still entered through exactly the
//! documented command line (`docs/adr/0001-native-messaging-protocol.md`).
//!
//! See `docs/adr/0008-relocatable-native-host-installation.md`.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::{DownerError, DownerResult};

/// The native messaging host name. `extension/background.js` connects to it.
pub const HOST_NAME: &str = "com.downer.native";

/// The extension allowed to talk to the host.
///
/// Read from `browser_specific_settings.gecko.id` in
/// `extension/manifest.json` by `build.rs`, so the manifest Firefox reads is
/// the only place the ID is written and the two cannot drift (KEI-58).
/// `tests/host_install.rs` still asserts the agreement, which now checks the
/// build wiring rather than two hand-typed strings.
pub const EXTENSION_ID: &str = env!("DOWNER_EXTENSION_ID");

/// The platforms Downer distinguishes.
///
/// A value rather than a `cfg!` chain, because the unsupported arm has to be
/// reachable from a supported one. The crate refuses to build at all on a
/// non-Unix target (`src/lib.rs`), so a `cfg`-gated refusal could never be
/// exercised by a test — the only build that could run it is the build that
/// does not exist. Naming the platform makes the refusal ordinary code.
///
/// See `docs/adr/0021-windows-is-unsupported.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    /// A target that builds but that Downer does not support: a Unix that is
    /// neither macOS nor Linux, where only Firefox's registration directory is
    /// unknown. Carries the name `std::env::consts::OS` gives it.
    Unsupported(&'static str),
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Unsupported(std::env::consts::OS)
        }
    }

    /// How the `status` response and `downer doctor` name it. Never translated:
    /// `docs/protocol.md` lists these as the values of `platform`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::Unsupported(_) => "unsupported",
        }
    }

    pub const fn is_supported(self) -> bool {
        !matches!(self, Self::Unsupported(_))
    }
}

/// What every surface says about a platform Downer does not support.
///
/// One string, because the installer and `downer doctor` are the two places a
/// user meets this and a second wording would read as a second problem.
pub fn unsupported_platform_message(os: &str) -> String {
    format!(
        "unsupported platform: {os}. Downer supports macOS and Linux; \
         see docs/adr/0021-windows-is-unsupported.md"
    )
}

/// Where installation puts things, resolved from the environment.
///
/// Every path is derived once, here, so tests can drive a whole install under a
/// temporary `HOME` and assert on the result.
#[derive(Debug, Clone)]
pub struct HostPaths {
    /// The Firefox native messaging manifest.
    pub manifest: PathBuf,
    /// The shell launcher the manifest points at.
    pub launcher: PathBuf,
    /// Where an installed copy of the binary lives.
    pub binary: PathBuf,
    /// The host's own configuration file.
    pub config: PathBuf,
}

impl HostPaths {
    pub fn resolve() -> DownerResult<Self> {
        let home = dirs::home_dir().ok_or_else(|| {
            DownerError::Host("no home directory: set HOME and try again".to_string())
        })?;
        let data = dirs::data_dir().ok_or_else(|| {
            DownerError::Host("no data directory: set HOME and try again".to_string())
        })?;
        let config = dirs::config_dir().ok_or_else(|| {
            DownerError::Host("no config directory: set HOME and try again".to_string())
        })?;
        Ok(Self {
            manifest: manifest_dir(&home, Platform::current())?.join(format!("{HOST_NAME}.json")),
            launcher: data.join("downer").join(format!("{HOST_NAME}.sh")),
            binary: data.join("downer").join("bin").join("downer"),
            config: config.join("downer").join("config.json"),
        })
    }
}

/// Firefox's per-user native messaging directory.
///
/// Windows keeps this registration in the registry rather than a directory, but
/// a Windows build never gets this far: the crate refuses to compile there
/// (`src/lib.rs`). What reaches the refusal below is a Unix that is neither
/// macOS nor Linux — a FreeBSD, say — where everything else works and only this
/// directory is unknown. That is the degrading half of
/// `docs/adr/0021-windows-is-unsupported.md`.
fn manifest_dir(home: &Path, platform: Platform) -> DownerResult<PathBuf> {
    match platform {
        Platform::MacOs => Ok(home
            .join("Library")
            .join("Application Support")
            .join("Mozilla")
            .join("NativeMessagingHosts")),
        Platform::Linux => Ok(home.join(".mozilla").join("native-messaging-hosts")),
        Platform::Unsupported(os) => Err(DownerError::Host(unsupported_platform_message(os))),
    }
}

/// What `downer install-host` was asked to do.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Register the running binary where it is instead of copying it.
    pub link: bool,
    /// Label the manifest as a development registration.
    pub dev: bool,
    /// Record this FFmpeg in the host configuration.
    pub ffmpeg: Option<PathBuf>,
}

/// What an install actually did, so the caller can print it.
#[derive(Debug, Clone)]
pub struct InstallReport {
    pub manifest: PathBuf,
    pub launcher: PathBuf,
    /// The binary the launcher runs, whether copied or referenced.
    pub binary: PathBuf,
    /// True when the binary was copied to the stable location.
    pub copied: bool,
    pub dev: bool,
    /// The FFmpeg written to the configuration, if any.
    pub ffmpeg: Option<PathBuf>,
}

/// What `downer uninstall-host` was asked to do.
#[derive(Debug, Clone, Default)]
pub struct UninstallOptions {
    /// Also delete the copied binary, not just the registration.
    pub binary: bool,
}

/// What an uninstall removed. Paths that were already absent are not listed.
#[derive(Debug, Clone, Default)]
pub struct UninstallReport {
    pub removed: Vec<PathBuf>,
}

/// The native host manifest, exactly as Firefox reads it.
#[derive(Debug, Serialize, Deserialize)]
struct HostManifest {
    name: String,
    description: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    allowed_extensions: Vec<String>,
}

/// Register this binary as the native host.
///
/// Idempotent: running it twice leaves the same three files with the same
/// contents, which matters because `make extension-install` runs on every
/// `make extension`.
pub fn install(options: &InstallOptions) -> DownerResult<InstallReport> {
    let paths = HostPaths::resolve()?;
    let current = std::env::current_exe()
        .map_err(|error| DownerError::Host(format!("cannot locate this binary: {error}")))?;

    let ffmpeg = match options.ffmpeg.as_deref() {
        Some(path) => Some(validated_ffmpeg(path)?),
        None => None,
    };

    let link = options.link || options.dev;
    let (binary, copied) = if link || is_stable_location(&current, &paths) {
        (current, false)
    } else {
        copy_binary(&current, &paths.binary)?;
        (paths.binary.clone(), true)
    };

    write_launcher(&paths.launcher, &binary)?;
    write_manifest(&paths.manifest, &paths.launcher, options.dev)?;
    if let Some(ffmpeg) = ffmpeg.as_deref() {
        write_config(&paths.config, &HostConfig::with_ffmpeg(ffmpeg))?;
    }

    Ok(InstallReport {
        manifest: paths.manifest,
        launcher: paths.launcher,
        binary,
        copied,
        dev: options.dev,
        ffmpeg,
    })
}

/// Undo an install.
///
/// Missing files are not an error: a partial installation, or a second
/// uninstall, should both end with nothing registered and exit zero.
pub fn uninstall(options: &UninstallOptions) -> DownerResult<UninstallReport> {
    let paths = HostPaths::resolve()?;
    let mut removed = Vec::new();
    let mut targets = vec![
        paths.manifest.clone(),
        paths.launcher.clone(),
        paths.config.clone(),
    ];
    if options.binary {
        targets.push(paths.binary.clone());
    }
    for target in targets {
        match fs::remove_file(&target) {
            Ok(()) => removed.push(target),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(DownerError::Host(format!(
                    "could not remove {}: {error}",
                    target.display()
                )))
            }
        }
    }
    Ok(UninstallReport { removed })
}

/// A binary that is already somewhere durable is registered where it is.
///
/// `cargo install` puts it on the user's PATH and keeps it there across
/// rebuilds, and a previous install already put it at the stable location, so
/// copying either one would leave two binaries to keep in step.
fn is_stable_location(current: &Path, paths: &HostPaths) -> bool {
    if current == paths.binary {
        return true;
    }
    match (current.parent(), dirs::executable_dir(), cargo_bin_dir()) {
        (Some(parent), executable, cargo) => {
            Some(parent) == executable.as_deref() || Some(parent) == cargo.as_deref()
        }
        _ => false,
    }
}

fn cargo_bin_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(home).join("bin"));
    }
    dirs::home_dir().map(|home| home.join(".cargo").join("bin"))
}

fn copy_binary(current: &Path, target: &Path) -> DownerResult<()> {
    create_parent(target)?;
    // Copying onto a running executable fails with ETXTBSY on Linux, and the
    // binary being replaced may be the very host Firefox is speaking to, so
    // write beside it and rename into place.
    let temporary = target.with_extension("incoming");
    fs::copy(current, &temporary).map_err(|error| {
        DownerError::Host(format!(
            "could not copy {} to {}: {error}",
            current.display(),
            temporary.display()
        ))
    })?;
    set_executable(&temporary)?;
    fs::rename(&temporary, target).map_err(|error| {
        DownerError::Host(format!("could not install {}: {error}", target.display()))
    })
}

fn write_launcher(launcher: &Path, binary: &Path) -> DownerResult<()> {
    create_parent(launcher)?;
    let script = format!(
        "#!/bin/sh\n\
         # Written by `downer install-host`. Edits are lost on the next install.\n\
         #\n\
         # Firefox runs this with the manifest path and the extension ID, never\n\
         # with `--native-host`, so the flag that selects the host protocol is\n\
         # supplied here.\n\
         exec {} --native-host \"$@\"\n",
        shell_quote(&binary.to_string_lossy())
    );
    write_file(launcher, script.as_bytes())?;
    set_executable(launcher)
}

fn write_manifest(manifest: &Path, launcher: &Path, dev: bool) -> DownerResult<()> {
    create_parent(manifest)?;
    let description = if dev {
        "Downer FFmpeg native messaging host (development build)"
    } else {
        "Downer FFmpeg native messaging host"
    };
    let document = HostManifest {
        name: HOST_NAME.to_string(),
        description: description.to_string(),
        path: launcher.to_string_lossy().into_owned(),
        kind: "stdio".to_string(),
        allowed_extensions: vec![EXTENSION_ID.to_string()],
    };
    let mut payload = serde_json::to_vec_pretty(&document)
        .map_err(|error| DownerError::Host(format!("could not render the manifest: {error}")))?;
    payload.push(b'\n');
    write_file(manifest, &payload)
}

/// The host's persistent settings.
///
/// It exists for one thing today: Firefox launches the native host with a
/// minimal environment, so `DOWNER_FFMPEG` is impractical for a user whose
/// FFmpeg is somewhere unusual. JSON rather than TOML because the crate already
/// speaks JSON everywhere and a config file is not worth a dependency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg: Option<PathBuf>,
}

impl HostConfig {
    fn with_ffmpeg(ffmpeg: &Path) -> Self {
        Self {
            ffmpeg: Some(ffmpeg.to_path_buf()),
        }
    }
}

/// Read the host configuration, or the default when it is absent or unreadable.
///
/// A broken configuration file must not stop a download: discovery falls back
/// to the same search it used before the file existed.
pub fn load_config() -> HostConfig {
    let Ok(paths) = HostPaths::resolve() else {
        return HostConfig::default();
    };
    let Ok(contents) = fs::read_to_string(&paths.config) else {
        return HostConfig::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// The FFmpeg recorded at install time, if it is still there.
pub fn configured_ffmpeg() -> Option<PathBuf> {
    load_config().ffmpeg.filter(|path| path.is_file())
}

fn write_config(config_path: &Path, config: &HostConfig) -> DownerResult<()> {
    create_parent(config_path)?;
    let mut payload = serde_json::to_vec_pretty(config)
        .map_err(|error| DownerError::Host(format!("could not render the config: {error}")))?;
    payload.push(b'\n');
    write_file(config_path, &payload)
}

fn validated_ffmpeg(path: &Path) -> DownerResult<PathBuf> {
    let resolved = if path.components().count() > 1 || path.is_absolute() {
        path.to_path_buf()
    } else {
        // A bare name is only useful if it is on PATH when Firefox launches the
        // host, which is exactly the situation the config file exists to fix.
        return Err(DownerError::HostArgument(format!(
            "--ffmpeg needs a path to the executable, not the bare name `{}`",
            path.display()
        )));
    };
    if !resolved.is_file() {
        return Err(DownerError::HostArgument(format!(
            "no FFmpeg executable at {}",
            resolved.display()
        )));
    }
    resolved.canonicalize().map_err(|error| {
        DownerError::HostArgument(format!("could not resolve {}: {error}", resolved.display()))
    })
}

fn create_parent(path: &Path) -> DownerResult<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|error| {
        DownerError::Host(format!("could not create {}: {error}", parent.display()))
    })
}

/// Write a file atomically, so a failed install cannot leave a half-written
/// manifest that Firefox would then refuse to parse.
fn write_file(path: &Path, contents: &[u8]) -> DownerResult<()> {
    let temporary = path.with_extension("incoming");
    let write = || -> io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()
    };
    write().map_err(|error| {
        DownerError::Host(format!("could not write {}: {error}", path.display()))
    })?;
    fs::rename(&temporary, path)
        .map_err(|error| DownerError::Host(format!("could not write {}: {error}", path.display())))
}

/// Make the launcher executable.
///
/// Unconditionally Unix: Firefox executes the launcher directly, so the mode
/// bits are not decoration. There is no non-Unix arm because there is no
/// non-Unix build — see `docs/adr/0021-windows-is-unsupported.md`.
fn set_executable(path: &Path) -> DownerResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|error| {
        DownerError::Host(format!(
            "could not make {} executable: {error}",
            path.display()
        ))
    })
}

/// Quote a path for `/bin/sh`.
///
/// Single quotes take everything literally, so the only case to handle is a
/// single quote in the path itself: end the quoted run, emit an escaped quote,
/// and start a new one.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_literally() {
        assert_eq!(
            shell_quote("/usr/local/bin/downer"),
            "'/usr/local/bin/downer'"
        );
        assert_eq!(shell_quote("/tmp/a b/downer"), "'/tmp/a b/downer'");
        assert_eq!(shell_quote("/tmp/$HOME/downer"), "'/tmp/$HOME/downer'");
    }

    #[test]
    fn closes_and_reopens_quoting_around_a_quote() {
        assert_eq!(shell_quote("/tmp/it's/downer"), "'/tmp/it'\\''s/downer'");
    }

    /// The refusal a platform Downer does not support gets from the installer.
    ///
    /// Asserted from a supported platform by naming the unsupported one, which
    /// is the whole reason [`Platform`] is a value: on Windows this code cannot
    /// run, because on Windows the crate does not compile
    /// (`docs/adr/0021-windows-is-unsupported.md`).
    #[test]
    fn an_unsupported_platform_is_refused_by_name() {
        let error =
            manifest_dir(Path::new("/home/someone"), Platform::Unsupported("windows")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("unsupported platform"), "{message}");
        assert!(message.contains("windows"), "{message}");
        assert!(
            message.contains("macOS and Linux"),
            "the refusal must say what is supported: {message}"
        );
    }

    /// The two supported platforms resolve to the directory Firefox actually
    /// reads, whichever platform the test itself is running on.
    #[test]
    fn each_supported_platform_gets_its_own_firefox_directory() {
        let home = Path::new("/home/someone");
        let macos = manifest_dir(home, Platform::MacOs).unwrap();
        let linux = manifest_dir(home, Platform::Linux).unwrap();
        assert!(macos.ends_with("Mozilla/NativeMessagingHosts"), "{macos:?}");
        assert!(
            linux.ends_with(".mozilla/native-messaging-hosts"),
            "{linux:?}"
        );
        assert_ne!(macos, linux);
    }

    /// `is_supported` and `name` agree, whichever platform this is running on.
    ///
    /// Deliberately not "the current platform is macOS or Linux": a Unix that
    /// is neither still builds by design, and a test that went red there would
    /// contradict the decision it is supposed to be guarding.
    #[test]
    fn a_platform_is_named_unsupported_exactly_when_it_is_unsupported() {
        for platform in [
            Platform::MacOs,
            Platform::Linux,
            Platform::Unsupported("windows"),
            Platform::current(),
        ] {
            assert_eq!(
                platform.is_supported(),
                platform.name() != "unsupported",
                "{platform:?}"
            );
        }
    }

    #[test]
    fn a_bare_ffmpeg_name_is_refused() {
        let error = validated_ffmpeg(Path::new("ffmpeg")).unwrap_err();
        assert!(matches!(error, DownerError::HostArgument(_)), "{error:?}");
    }
}
