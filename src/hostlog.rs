//! The native host's own log file.
//!
//! Firefox launches the native host with its stderr wired to the Browser
//! Console, which almost nobody opens, and nothing at all survives the process
//! exiting. So the failures that matter most — the host not starting, FFmpeg
//! not being found, a manifest pointing at a binary that moved — leave no trace
//! anywhere a user can attach to a bug report. This module gives the host one
//! bounded file on disk instead.
//!
//! Three properties are load-bearing, and each is a decision rather than an
//! implementation detail. They are recorded in
//! `docs/adr/0022-a-bounded-redacted-host-log-file.md`.
//!
//! **It cannot grow without bound.** Two files of [`DEFAULT_MAX_BYTES`] each, so
//! the worst case is fixed and small. A log that can fill a disk is a bug
//! report nobody wants and an outage somebody gets.
//!
//! **It cannot leak a secret.** Every value written passes through
//! [`crate::redact`], the same rule the protocol's `log` events use, so a signed
//! segment URL loses its query. That covers URLs and nothing else, so the two
//! things that are *not* URLs are kept away from here entirely rather than
//! filtered on the way in: a cookie never reaches this module (FFmpeg's argv is
//! rendered by [`crate::ffmpeg::FfmpegInvocation::to_log_args`], which replaces
//! the values), and neither does a page title, because the output *filename* is
//! the title when title naming is on and `AGENTS.md` forbids a title reaching a
//! log.
//!
//! **It cannot break a download.** Every failure here is swallowed. A full
//! disk, a read-only home directory or a file someone deleted mid-run must not
//! turn a working download into a failed one — the log exists to explain
//! failures, not to cause them.

use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::host::Platform;

/// The log file's name. The rotated one is this plus `.1`.
pub const FILE_NAME: &str = "host.log";

/// How large one file may get before it is rotated.
///
/// Two files of this size is the whole budget: 2 MiB total, which holds a long
/// HLS download's stderr several times over and still fits in a bug report.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

/// The environment variable that overrides the configured level.
///
/// Firefox launches the host with a minimal environment, so this is not how a
/// user turns on debug logging for the extension — the config file is (see
/// [`crate::host::HostConfig`]). It is here for a terminal and for tests.
pub const LEVEL_ENV: &str = "DOWNER_LOG";

/// How much to write.
///
/// Ordered least to most, so a message is written when its level is at most the
/// configured one. `Off` writes nothing, including no file: a user who turns
/// logging off should not find a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    Off,
    Error,
    #[default]
    Info,
    Debug,
}

impl Level {
    /// Parse a configured or environment value. Unrecognised text is `None`, so
    /// a typo falls back to the default rather than silently turning logging
    /// off — the failure mode of a typo must not be silence.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "error" => Some(Self::Error),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Error => "ERROR",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Info => "info",
            Self::Debug => "debug",
        })
    }
}

/// Where the log file lives, for a given platform and its directories.
///
/// Split out from [`default_path`] so both layouts can be asserted from
/// whichever platform the tests are running on — the same reason
/// [`Platform`] is a value at all (ADR-0021).
///
/// macOS has a documented place for this and users know to look there.
/// Elsewhere it is the XDG state directory, not the data directory: a log is
/// exactly what XDG means by state — it survives restarts, it is not precious,
/// and losing it costs nothing.
fn path_in(platform: Platform, home: &Path, state: &Path) -> PathBuf {
    match platform {
        Platform::MacOs => home
            .join("Library")
            .join("Logs")
            .join("downer")
            .join(FILE_NAME),
        Platform::Linux | Platform::Unsupported(_) => {
            state.join("downer").join("logs").join(FILE_NAME)
        }
    }
}

/// Where the log file lives on this machine.
///
/// `None` only when no home directory can be resolved at all, which is the same
/// condition that stops the host being installed in the first place.
pub fn default_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    // `state_dir` is XDG-only and `None` on macOS, which takes the other branch
    // anyway. `data_local_dir` is the fallback for a Unix without XDG state.
    let state = dirs::state_dir().or_else(dirs::data_local_dir)?;
    Some(path_in(Platform::current(), &home, &state))
}

/// The level this host should log at: `DOWNER_LOG` if it names one, otherwise
/// the host configuration, otherwise [`Level::Info`].
pub fn configured_level() -> Level {
    if let Ok(value) = std::env::var(LEVEL_ENV) {
        if let Some(level) = Level::parse(&value) {
            return level;
        }
    }
    crate::host::load_config().log_level()
}

/// A bounded, rotating log file.
///
/// Rotation is by size and keeps exactly one previous file, so the total on
/// disk is bounded by twice [`HostLog::max_bytes`]. The cap is a field rather
/// than a constant so a test can prove rotation with a handful of bytes instead
/// of writing a megabyte to do it.
pub struct HostLog {
    path: PathBuf,
    level: Level,
    max_bytes: u64,
    /// Opened on the first line written, not at construction: a host that logs
    /// nothing should not create a file, and `Off` should not create a
    /// directory.
    file: Option<fs::File>,
    written: u64,
}

impl HostLog {
    pub fn new(path: PathBuf, level: Level, max_bytes: u64) -> Self {
        Self {
            path,
            level,
            max_bytes,
            file: None,
            written: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// The rotated file's path: the live one plus `.1`.
    pub fn rotated_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".1");
        self.path.with_file_name(name)
    }

    /// Write one event, if `level` is enabled.
    ///
    /// Every failure is swallowed deliberately; see the module comment.
    pub fn write(&mut self, level: Level, event: &str, fields: &[(&str, &str)]) {
        if level > self.level || self.level == Level::Off {
            return;
        }
        let line = render(level, event, fields);
        let _ = self.write_line(&line);
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let bytes = line.len() as u64 + 1;
        // Rotate *before* writing a line that would cross the cap, so the cap is
        // never exceeded rather than merely noticed afterwards. A line larger
        // than the whole budget still gets written, into an empty file: dropping
        // it would lose the one event most worth having.
        if self.file.is_some() && self.written > 0 && self.written + bytes > self.max_bytes {
            self.rotate()?;
        }
        self.ensure_open()?;
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        self.written += bytes;
        Ok(())
    }

    fn ensure_open(&mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // Append to whatever a previous run left, so restarting the host does
        // not discard the evidence from the run that just failed — which is the
        // run somebody is asking about.
        self.written = file.metadata().map(|data| data.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        // A rename over the previous rotation is the whole retention policy:
        // one generation back, then gone.
        fs::rename(&self.path, self.rotated_path())?;
        self.written = 0;
        Ok(())
    }
}

/// Render one line: timestamp, level, event, then `key=value` pairs.
///
/// Every value is redacted and quoted here rather than at each call site, so a
/// new event cannot forget to do it.
fn render(level: Level, event: &str, fields: &[(&str, &str)]) -> String {
    let mut line = format!("{} {:<5} {event}", timestamp(), level.label());
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&quote(&crate::redact::redact_text(value)));
    }
    line
}

/// Quote a value so a multi-word one cannot look like several fields, and so a
/// newline in it cannot forge a second log line.
fn quote(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if !needs_quotes {
        return value.to_string();
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// An RFC 3339 UTC timestamp, to the second.
///
/// Hand-rolled rather than pulled in with a date crate: this is the only place
/// in the project that formats a wall-clock time, and a dependency for it would
/// weigh more than the twenty lines below. Seconds are enough — the question a
/// log answers here is "what happened before what", not "how long did it take",
/// which `-progress` already reports.
fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Days since the Unix epoch to a civil date, by Howard Hinnant's algorithm.
///
/// Shifts the era so that March is the first month, which is what makes the
/// leap day fall at the end of a year and removes every special case.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

static LOG: OnceLock<Mutex<HostLog>> = OnceLock::new();

/// Start logging for this process.
///
/// Called once, by the native host. The CLI deliberately does not call it: a
/// terminal already shows what the host cannot, and a `downer <url>` run that
/// silently began writing files under the user's home would be a surprise.
/// `downer doctor` still reports the path, because the path is what a user
/// needs to find the file the *host* wrote.
pub fn init(path: PathBuf, level: Level, max_bytes: u64) {
    let _ = LOG.set(Mutex::new(HostLog::new(path, level, max_bytes)));
}

/// Start logging at the configured level and place, if one can be resolved.
pub fn init_default() {
    let level = configured_level();
    if level == Level::Off {
        return;
    }
    if let Some(path) = default_path() {
        init(path, level, DEFAULT_MAX_BYTES);
    }
}

/// Write one event. A no-op when logging was never started, which is what makes
/// this safe to call from code shared with the CLI.
pub fn event(level: Level, event: &str, fields: &[(&str, &str)]) {
    let Some(log) = LOG.get() else {
        return;
    };
    // A poisoned lock means another thread panicked mid-write. Losing log lines
    // is the correct response; propagating the panic is not.
    if let Ok(mut log) = log.lock() {
        log.write(level, event, fields);
    }
}

/// Shorthand for the common levels, so call sites read as events rather than as
/// logging calls.
pub fn info(event: &str, fields: &[(&str, &str)]) {
    self::event(Level::Info, event, fields);
}

pub fn error(event: &str, fields: &[(&str, &str)]) {
    self::event(Level::Error, event, fields);
}

pub fn debug(event: &str, fields: &[(&str, &str)]) {
    self::event(Level::Debug, event, fields);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn the_macos_layout_is_the_one_apple_documents() {
        let path = path_in(
            Platform::MacOs,
            Path::new("/Users/someone"),
            Path::new("/unused"),
        );
        assert_eq!(
            path,
            Path::new("/Users/someone/Library/Logs/downer/host.log")
        );
    }

    #[test]
    fn elsewhere_the_log_goes_under_the_state_directory() {
        let path = path_in(
            Platform::Linux,
            Path::new("/home/someone"),
            Path::new("/home/someone/.local/state"),
        );
        assert_eq!(
            path,
            Path::new("/home/someone/.local/state/downer/logs/host.log")
        );
    }

    #[test]
    fn a_level_is_parsed_case_insensitively_and_a_typo_is_not_silence() {
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse(" info "), Some(Level::Info));
        assert_eq!(Level::parse("off"), Some(Level::Off));
        // The important one: an unrecognised value must not read as `Off`, or a
        // typo would turn logging off without saying so.
        assert_eq!(Level::parse("verbose"), None);
    }

    #[test]
    fn levels_order_from_quietest_to_loudest() {
        assert!(Level::Off < Level::Error);
        assert!(Level::Error < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert_eq!(Level::default(), Level::Info);
    }

    #[test]
    fn nothing_is_written_below_the_configured_level() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("host.log");
        let mut log = HostLog::new(path.clone(), Level::Info, DEFAULT_MAX_BYTES);

        log.write(Level::Debug, "ffmpeg.stderr", &[("line", "chatter")]);
        assert!(
            !path.exists(),
            "a filtered event must not even create a file"
        );

        log.write(Level::Info, "host.start", &[]);
        assert!(read(&path).contains("host.start"));
    }

    #[test]
    fn off_writes_nothing_at_all() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("host.log");
        let mut log = HostLog::new(path.clone(), Level::Off, DEFAULT_MAX_BYTES);
        log.write(Level::Error, "host.failed", &[("error", "boom")]);
        assert!(!path.exists(), "logging off must leave no file behind");
    }

    /// Rotation, proven with a cap small enough to reach in a test rather than
    /// by writing a megabyte to observe the real one.
    #[test]
    fn the_log_rotates_at_the_cap_and_keeps_exactly_one_generation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("host.log");
        let mut log = HostLog::new(path.clone(), Level::Info, 200);

        for index in 0..40 {
            log.write(Level::Info, "job.progress", &[("n", &index.to_string())]);
        }

        let rotated = log.rotated_path();
        assert!(path.is_file(), "the live file exists");
        assert!(rotated.is_file(), "exactly one generation is kept");
        assert!(
            !temp.path().join("host.log.2").exists(),
            "a second generation would mean the budget is not bounded"
        );

        let live = fs::metadata(&path).unwrap().len();
        assert!(live <= 200, "the live file stays under the cap: {live}");

        // And the bound that matters is the total, not one file.
        let total = live + fs::metadata(&rotated).unwrap().len();
        assert!(total <= 400, "two files of the cap, at most: {total}");

        // The newest events are the ones kept live.
        assert!(read(&path).contains("n=39"));
    }

    #[test]
    fn reopening_appends_rather_than_discarding_the_previous_run() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("host.log");

        let mut first = HostLog::new(path.clone(), Level::Info, DEFAULT_MAX_BYTES);
        first.write(Level::Info, "host.start", &[("run", "1")]);
        drop(first);

        let mut second = HostLog::new(path.clone(), Level::Info, DEFAULT_MAX_BYTES);
        second.write(Level::Info, "host.start", &[("run", "2")]);

        let contents = read(&path);
        assert!(contents.contains("run=1"), "{contents}");
        assert!(contents.contains("run=2"), "{contents}");
    }

    /// A value cannot forge a second line or a second field.
    #[test]
    fn a_value_with_a_newline_cannot_forge_a_log_line() {
        let line = render(
            Level::Info,
            "request.received",
            &[("command", "download\nINFO host.stop")],
        );
        assert_eq!(line.lines().count(), 1, "{line}");
        assert!(line.contains("\\n"), "{line}");
    }

    #[test]
    fn a_url_in_a_value_loses_its_query() {
        let line = render(
            Level::Info,
            "ffmpeg.spawn",
            &[("input", "https://cdn.example.com/v.m3u8?token=SECRET")],
        );
        assert!(!line.contains("SECRET"), "{line}");
        assert!(line.contains("https://cdn.example.com/v.m3u8"), "{line}");
    }

    #[test]
    fn a_timestamp_is_rfc3339_utc_to_the_second() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-', "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
    }

    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        // A leap day, and the day after it.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        // 2000 was a leap year; 1900 was not, which is the case a naive
        // "divisible by four" rule gets wrong.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_717), (2026, 9, 21));
    }
}
