//! The setup checks behind the `status` native command and `downer doctor`.
//!
//! Both surfaces run the same checks and render the same structure, so a
//! problem is described identically whether the user is looking at the Settings
//! page or a terminal. Nothing here decides *presentation*: a check carries a
//! machine-readable name, an outcome, what was found, and what to do about it,
//! and the caller lays that out.
//!
//! Every check is safe to run at any time: none of them writes anything that
//! outlives the check, and none of them blocks indefinitely.

use std::{
    fmt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::{ffmpeg, host};

/// How a single check turned out.
///
/// `Warn` exists because "works, but not as intended" is a real answer and
/// collapsing it into either `Pass` or `Fail` loses the reason the user came
/// looking. An FFmpeg below the supported minimum is the case in point: it
/// downloads (ADR-0006), so failing would be wrong, and it needs saying, so
/// passing would be wrong too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Warn,
    Fail,
}

impl Outcome {
    /// Whether this outcome should make `downer doctor` exit non-zero.
    ///
    /// A warning does not: the setup works, and an exit code that cried wolf
    /// would make the command useless in a script.
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Fail)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "ok",
            Self::Warn => "warning",
            Self::Fail => "failed",
        })
    }
}

/// One named thing that was checked.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// A stable identifier, safe to key UI or tests off. Never translated.
    pub name: &'static str,
    /// What was checked, for a human.
    pub title: &'static str,
    pub outcome: Outcome,
    /// What was found — the FFmpeg that resolved, the directory that could not
    /// be written to. Always present, including on a pass, because "which one?"
    /// is the question a diagnostics panel exists to answer.
    pub detail: String,
    /// What to do about it. `None` on a pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

impl Check {
    fn pass(name: &'static str, title: &'static str, detail: String) -> Self {
        Self {
            name,
            title,
            outcome: Outcome::Pass,
            detail,
            remedy: None,
        }
    }

    fn warn(name: &'static str, title: &'static str, detail: String, remedy: String) -> Self {
        Self {
            name,
            title,
            outcome: Outcome::Warn,
            detail,
            remedy: Some(remedy),
        }
    }

    fn fail(name: &'static str, title: &'static str, detail: String, remedy: String) -> Self {
        Self {
            name,
            title,
            outcome: Outcome::Fail,
            detail,
            remedy: Some(remedy),
        }
    }
}

/// Everything the `status` command and `downer doctor` report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub host_version: &'static str,
    pub protocol_version: u32,
    pub platform: &'static str,
    /// The FFmpeg that a download started right now would use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffmpeg_path: Option<String>,
    /// As FFmpeg reports it, when it could be run and parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffmpeg_version: Option<String>,
    /// Whether an HLS download would work: FFmpeg runs and is new enough.
    pub ffmpeg_ok: bool,
    pub checks: Vec<Check>,
}

impl Report {
    /// The worst outcome across all checks, which is the report's own verdict.
    pub fn outcome(&self) -> Outcome {
        if self
            .checks
            .iter()
            .any(|check| check.outcome == Outcome::Fail)
        {
            Outcome::Fail
        } else if self
            .checks
            .iter()
            .any(|check| check.outcome == Outcome::Warn)
        {
            Outcome::Warn
        } else {
            Outcome::Pass
        }
    }
}

/// Run every check.
///
/// `output_dir` is the directory downloads would land in. Callers pass the
/// directory their *own* download path would resolve to, defaults included —
/// the native host's default is the user's Downloads folder, the CLI's is the
/// working directory, and checking anything else would report on a directory
/// nobody uses. `None` is for the case where even that cannot be resolved, and
/// skips the check rather than inventing one.
///
/// `ffmpeg` overrides discovery. `None` reports the FFmpeg the native host
/// would pick, which is what the Settings panel is asking about; a caller that
/// names one is asking about that one instead.
pub fn run(output_dir: Option<&Path>, ffmpeg: Option<&Path>) -> Report {
    let ffmpeg_path = match ffmpeg {
        Some(path) => path.to_path_buf(),
        None => crate::native::ffmpeg_path(),
    };
    let version = ffmpeg::version(&ffmpeg_path);

    // The platform check comes first because it subsumes the others: on a
    // platform Downer does not support, "FFmpeg is fine" is true and useless.
    let mut checks = vec![
        platform_check(host::Platform::current()),
        host_registration_check(),
        ffmpeg_check(&ffmpeg_path, version),
    ];
    if let Some(directory) = output_dir {
        checks.push(output_directory_check(directory));
    }

    Report {
        host_version: env!("CARGO_PKG_VERSION"),
        protocol_version: crate::native::PROTOCOL_VERSION,
        platform: PLATFORM,
        ffmpeg_path: Some(ffmpeg_path.display().to_string()),
        ffmpeg_version: version.map(|version| version.to_string()),
        // An FFmpeg that could not be probed is not usable for a download: the
        // probe is `ffmpeg -version`, the cheapest thing it could be asked to
        // do. Below the minimum it still works (ADR-0006), so that stays ok.
        ffmpeg_ok: version.is_some(),
        checks,
    }
}

const PLATFORM: &str = host::Platform::current().name();

/// Is Downer running on a platform it supports?
///
/// Windows never reaches this: the crate does not compile there
/// (`docs/adr/0021-windows-is-unsupported.md`). What this catches is a Unix
/// that is neither macOS nor Linux, where downloads work but the native host
/// cannot be registered with Firefox — so it fails rather than warns. The
/// platform is a parameter so the failing branch can be asserted from a passing
/// one, which is the only way it can be tested at all.
fn platform_check(platform: host::Platform) -> Check {
    const NAME: &str = "platform";
    const TITLE: &str = "Supported platform";

    match platform {
        host::Platform::MacOs | host::Platform::Linux => {
            Check::pass(NAME, TITLE, platform.name().to_string())
        }
        host::Platform::Unsupported(os) => Check::fail(
            NAME,
            TITLE,
            host::unsupported_platform_message(os),
            "Downloads may work, but the native host cannot be registered with Firefox here. \
             Use the command line on this platform, or macOS or Linux for the extension."
                .to_string(),
        ),
    }
}

/// Is the native host registered with Firefox, and does the registration still
/// point at something that exists?
///
/// This is the check that explains "native host disconnected", which is the
/// error users actually hit and the one `browser.runtime.lastError` words
/// unhelpfully.
fn host_registration_check() -> Check {
    const NAME: &str = "host_registration";
    const TITLE: &str = "Native host registered with Firefox";

    let paths = match host::HostPaths::resolve() {
        Ok(paths) => paths,
        Err(error) => {
            return Check::fail(
                NAME,
                TITLE,
                error.to_string(),
                "Set HOME to a writable home directory and run `downer install-host`.".to_string(),
            )
        }
    };

    if !paths.manifest.is_file() {
        return Check::fail(
            NAME,
            TITLE,
            format!("no manifest at {}", paths.manifest.display()),
            "Run `downer install-host` to register the host with Firefox.".to_string(),
        );
    }

    // A manifest whose launcher is gone is worse than no manifest: Firefox
    // finds the registration, runs nothing, and reports a disconnect.
    if !paths.launcher.is_file() {
        return Check::fail(
            NAME,
            TITLE,
            format!(
                "{} registered, but its launcher {} is missing",
                paths.manifest.display(),
                paths.launcher.display()
            ),
            "Run `downer install-host` again to rewrite the launcher.".to_string(),
        );
    }

    Check::pass(NAME, TITLE, format!("{}", paths.manifest.display()))
}

/// Does FFmpeg run, and is it new enough?
fn ffmpeg_check(path: &Path, version: Option<ffmpeg::FfmpegVersion>) -> Check {
    const NAME: &str = "ffmpeg";
    const TITLE: &str = "FFmpeg available";

    let Some(version) = version else {
        return Check::fail(
            NAME,
            TITLE,
            format!("{} could not be run", path.display()),
            "Install FFmpeg 7.1 or newer, then record it with \
             `downer install-host --ffmpeg /path/to/ffmpeg`."
                .to_string(),
        );
    };

    if !version.meets_minimum() {
        return Check::warn(
            NAME,
            TITLE,
            format!("{} is FFmpeg {version}", path.display()),
            format!(
                "FFmpeg {version} is older than the minimum supported {}. \
                 HLS downloads are attempted without the segment-extension \
                 options and may fail. Upgrade FFmpeg, then record it with \
                 `downer install-host --ffmpeg /path/to/ffmpeg`.",
                ffmpeg::MINIMUM_FFMPEG
            ),
        );
    }

    Check::pass(
        NAME,
        TITLE,
        format!("{} is FFmpeg {version}", path.display()),
    )
}

/// Can a download actually be written to the configured directory?
///
/// Existence and permission bits are not the question — whether a file can be
/// created is. This writes a uniquely named probe file and removes it, so it
/// answers the real question without disturbing anything already there.
///
/// A directory that does not exist **yet** is not a failure. A download creates
/// its output directory (`output::resolve_output_path` calls `create_dir_all`),
/// so reporting "not a directory" would tell the user something is wrong with a
/// setup that works — and on a Linux machine with no XDG configuration, where
/// the default is a `~/Downloads` nobody has created, that is the *ordinary*
/// case rather than a corner of it. The check walks up to the nearest existing
/// ancestor and asks the same question there, which is what decides whether the
/// download's `create_dir_all` will succeed. See KEI-90.
fn output_directory_check(directory: &Path) -> Check {
    const NAME: &str = "output_directory";
    const TITLE: &str = "Download directory writable";

    if !directory.exists() {
        let Some(existing) = nearest_existing_ancestor(directory) else {
            return Check::fail(
                NAME,
                TITLE,
                format!(
                    "{} does not exist and neither does any parent of it",
                    directory.display()
                ),
                "Choose an existing directory on the Settings page.".to_string(),
            );
        };
        if !existing.is_dir() {
            return Check::fail(
                NAME,
                TITLE,
                format!(
                    "{} cannot be created: {} is a file",
                    directory.display(),
                    existing.display()
                ),
                "Choose a directory whose parents are directories, on the Settings page."
                    .to_string(),
            );
        }
        return match writable(&existing) {
            Ok(()) => Check::pass(
                NAME,
                TITLE,
                format!("{} (will be created)", directory.display()),
            ),
            Err(error) => Check::fail(
                NAME,
                TITLE,
                format!(
                    "{} does not exist and cannot be created: {} is not writable ({error})",
                    directory.display(),
                    existing.display()
                ),
                "Choose a directory you can write to on the Settings page, or fix its permissions."
                    .to_string(),
            ),
        };
    }

    if !directory.is_dir() {
        return Check::fail(
            NAME,
            TITLE,
            format!("{} is not a directory", directory.display()),
            "Choose a directory rather than a file on the Settings page.".to_string(),
        );
    }

    match writable(directory) {
        Ok(()) => Check::pass(NAME, TITLE, directory.display().to_string()),
        Err(error) => Check::fail(
            NAME,
            TITLE,
            format!("{} cannot be written to: {error}", directory.display()),
            "Choose a directory you can write to on the Settings page, or fix its permissions."
                .to_string(),
        ),
    }
}

/// Write a uniquely named probe file and remove it.
fn writable(directory: &Path) -> std::io::Result<()> {
    let probe = directory.join(format!(".downer-write-check-{}", probe_suffix()));
    std::fs::write(&probe, b"")?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// The closest ancestor of `directory` that exists, including itself.
///
/// By existence, not by being a directory. An ancestor that exists as a *file*
/// blocks `create_dir_all` for everything under it, so skipping past it to the
/// next directory up would report a writable parent for a path that can never
/// be created.
fn nearest_existing_ancestor(directory: &Path) -> Option<PathBuf> {
    directory
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .map(Path::to_path_buf)
}

/// A suffix unlikely to collide with a concurrent check or a real file.
fn probe_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writable_directory_passes_and_leaves_nothing_behind() {
        let temp = tempfile::tempdir().unwrap();
        let check = output_directory_check(temp.path());
        assert_eq!(check.outcome, Outcome::Pass);
        assert!(check.remedy.is_none());
        // The probe file is the whole mechanism; leaving one behind would put
        // a dotfile in the user's downloads on every check.
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            0,
            "the write probe is removed"
        );
    }

    /// KEI-90 reverses this: a directory that does not exist *yet* used to fail.
    ///
    /// A download creates its output directory, so the old answer reported a
    /// problem with a setup that works — and once the default became
    /// `~/Downloads` on a machine with no XDG configuration, that became the
    /// ordinary case rather than a corner of it. What the check must still
    /// answer is whether the download's `create_dir_all` will succeed, which is
    /// a question about the nearest existing ancestor.
    #[test]
    fn a_directory_that_does_not_exist_yet_passes_and_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let check = output_directory_check(&temp.path().join("not-created"));
        assert_eq!(check.outcome, Outcome::Pass, "{check:?}");
        assert!(check.detail.contains("will be created"), "{check:?}");
    }

    /// Several levels deep, as `~/Downloads` is from `/`.
    #[test]
    fn a_directory_several_levels_from_anything_existing_still_passes() {
        let temp = tempfile::tempdir().unwrap();
        let check = output_directory_check(&temp.path().join("a").join("b").join("c"));
        assert_eq!(check.outcome, Outcome::Pass, "{check:?}");
    }

    /// The case that must still fail, and it must not be reached by walking
    /// *past* the thing in the way. A file blocks `create_dir_all` for
    /// everything beneath it however writable the directory above it is — and
    /// unlike a permission bit, root cannot ignore it, so this asserts on every
    /// machine rather than skipping.
    #[test]
    fn a_missing_directory_under_a_file_fails_rather_than_finding_a_parent() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("in-the-way");
        std::fs::write(&blocker, b"").unwrap();

        let check = output_directory_check(&blocker.join("downloads"));
        assert_eq!(check.outcome, Outcome::Fail, "{check:?}");
        assert!(check.detail.contains("is a file"), "{check:?}");
        assert!(check.remedy.is_some(), "a failure always says what to do");

        // And the claim the check makes is true.
        assert!(
            std::fs::create_dir_all(blocker.join("downloads")).is_err(),
            "the check must agree with what a download would find"
        );
    }

    /// An unwritable parent fails too, where permission bits can be felt.
    #[cfg(unix)]
    #[test]
    fn a_missing_directory_under_an_unwritable_parent_fails_with_something_to_do() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        let check = output_directory_check(&locked.join("downloads"));
        // Running as root defeats the permission bits, so this asserts the
        // distinction only where it can exist — the convention this file
        // already uses for `an_unwritable_directory_fails_…`.
        let root_ignores_permissions = std::fs::create_dir(locked.join("probe")).is_ok();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if root_ignores_permissions {
            return;
        }

        assert_eq!(check.outcome, Outcome::Fail, "{check:?}");
        assert!(check.detail.contains("cannot be created"), "{check:?}");
        assert!(check.remedy.is_some(), "a failure always says what to do");
    }

    /// A path that exists but is a file is still wrong, and says so distinctly.
    #[test]
    fn a_path_that_is_a_file_fails_as_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-a-directory");
        std::fs::write(&file, b"").unwrap();
        let check = output_directory_check(&file);
        assert_eq!(check.outcome, Outcome::Fail, "{check:?}");
        assert!(check.detail.contains("is not a directory"), "{check:?}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_fails_rather_than_passing_on_existence() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        let check = output_directory_check(&locked);
        // Running as root defeats the permission bits, so this asserts the
        // distinction only where it can exist.
        if std::fs::write(locked.join(".probe"), b"").is_ok() {
            let _ = std::fs::remove_file(locked.join(".probe"));
            return;
        }
        assert_eq!(check.outcome, Outcome::Fail);
        assert!(check.detail.contains("cannot be written to"));

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The unsupported-platform report, asserted from a supported platform.
    ///
    /// This is the doctor half of KEI-67's acceptance criterion. It names the
    /// platform rather than relying on `cfg`, because the platform the criterion
    /// is about — Windows — is one this test could never run on: the crate
    /// refuses to build there (`docs/adr/0021-windows-is-unsupported.md`).
    #[test]
    fn an_unsupported_platform_fails_the_report_and_says_what_is_supported() {
        let check = platform_check(host::Platform::Unsupported("windows"));
        assert_eq!(check.outcome, Outcome::Fail, "{check:?}");
        assert_eq!(check.name, "platform");
        assert!(check.detail.contains("windows"), "{check:?}");
        assert!(check.detail.contains("macOS and Linux"), "{check:?}");
        let remedy = check.remedy.expect("a failure always says what to do");
        assert!(remedy.contains("Firefox"), "{remedy}");
    }

    #[test]
    fn a_supported_platform_passes_and_names_itself() {
        for platform in [host::Platform::MacOs, host::Platform::Linux] {
            let check = platform_check(platform);
            assert_eq!(check.outcome, Outcome::Pass, "{check:?}");
            assert_eq!(check.detail, platform.name());
            assert!(check.remedy.is_none(), "a pass has nothing to remedy");
        }
    }

    /// The reported `platform` string is the one `docs/protocol.md` documents.
    #[test]
    fn the_reported_platform_is_a_documented_value() {
        assert!(
            matches!(PLATFORM, "macos" | "linux" | "unsupported"),
            "protocol.md lists the permitted values; got {PLATFORM}"
        );
    }

    #[test]
    fn an_old_ffmpeg_warns_rather_than_failing() {
        // ADR-0006: an FFmpeg below the minimum still downloads, so it must not
        // be reported as a failure — but it must be reported.
        let version = ffmpeg::FfmpegVersion {
            major: 6,
            minor: 1,
            patch: Some(1),
        };
        let check = ffmpeg_check(Path::new("/usr/bin/ffmpeg"), Some(version));
        assert_eq!(check.outcome, Outcome::Warn);
        assert!(check.detail.contains("6.1.1"));
        assert!(check.remedy.unwrap().contains("7.1"));
    }

    #[test]
    fn an_unrunnable_ffmpeg_fails() {
        let check = ffmpeg_check(Path::new("/nonexistent/ffmpeg"), None);
        assert_eq!(check.outcome, Outcome::Fail);
        assert!(check.remedy.unwrap().contains("install-host --ffmpeg"));
    }

    #[test]
    fn the_report_takes_the_worst_outcome() {
        let mut report = Report {
            host_version: "0.0.0",
            protocol_version: 1,
            platform: "linux",
            ffmpeg_path: None,
            ffmpeg_version: None,
            ffmpeg_ok: true,
            checks: vec![Check::pass("a", "A", "fine".to_string())],
        };
        assert_eq!(report.outcome(), Outcome::Pass);

        report
            .checks
            .push(Check::warn("b", "B", "odd".to_string(), "look".to_string()));
        assert_eq!(report.outcome(), Outcome::Warn);

        report.checks.push(Check::fail(
            "c",
            "C",
            "broken".to_string(),
            "fix".to_string(),
        ));
        assert_eq!(report.outcome(), Outcome::Fail);
        assert!(report.outcome().is_failure());
    }
}
