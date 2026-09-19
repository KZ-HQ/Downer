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
    path::Path,
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

    let mut checks = vec![
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

const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "unsupported"
};

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
fn output_directory_check(directory: &Path) -> Check {
    const NAME: &str = "output_directory";
    const TITLE: &str = "Download directory writable";

    if !directory.is_dir() {
        return Check::fail(
            NAME,
            TITLE,
            format!("{} is not a directory", directory.display()),
            "Choose an existing directory on the Settings page, or create this one.".to_string(),
        );
    }

    let probe = directory.join(format!(".downer-write-check-{}", probe_suffix()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Check::pass(NAME, TITLE, directory.display().to_string())
        }
        Err(error) => Check::fail(
            NAME,
            TITLE,
            format!("{} cannot be written to: {error}", directory.display()),
            "Choose a directory you can write to on the Settings page, or fix its permissions."
                .to_string(),
        ),
    }
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

    #[test]
    fn a_missing_directory_fails_with_something_to_do() {
        let temp = tempfile::tempdir().unwrap();
        let check = output_directory_check(&temp.path().join("not-created"));
        assert_eq!(check.outcome, Outcome::Fail);
        assert!(check.detail.contains("not a directory"));
        assert!(check.remedy.is_some(), "a failure always says what to do");
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
