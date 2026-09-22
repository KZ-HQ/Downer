use std::{
    collections::HashMap,
    ffi::OsString,
    fmt,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{ChildStderr, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

use crate::error::{DownerError, DownerResult};

/// The oldest FFmpeg this project supports, as documented in `README.md`.
///
/// The two HLS options in [`FfmpegInvocation`] exist only from here onwards, which
/// is what makes an older FFmpeg worth naming rather than letting it fail as an
/// argument-parsing error.
pub const MINIMUM_FFMPEG: FfmpegVersion = FfmpegVersion {
    major: 7,
    minor: 1,
    patch: None,
};

/// The HLS options that loosen FFmpeg's segment-extension checking. They are
/// dropped for an FFmpeg that predates them; see
/// `docs/adr/0006-ffmpeg-version-detection.md`.
const SEGMENT_EXTENSION_OPTIONS: [&str; 2] = ["-allowed_segment_extensions", "-extension_picky"];

/// A version as FFmpeg reports it on the first line of `ffmpeg -version`.
///
/// Only `major` and `minor` take part in the comparison; `patch` is kept so the
/// diagnostic can name the build the user actually has (`6.1.1`) rather than a
/// truncation of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: Option<u32>,
}

impl FfmpegVersion {
    /// Whether this version is at least [`MINIMUM_FFMPEG`].
    pub fn meets_minimum(&self) -> bool {
        (self.major, self.minor) >= (MINIMUM_FFMPEG.major, MINIMUM_FFMPEG.minor)
    }
}

impl fmt::Display for FfmpegVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)?;
        match self.patch {
            Some(patch) => write!(f, ".{patch}"),
            None => Ok(()),
        }
    }
}

/// The version of the FFmpeg at `program`, detected once per executable per
/// process.
///
/// `None` means "could not tell": FFmpeg would not run, or reported something
/// this cannot parse, such as a git-snapshot build. An undetectable version is
/// never treated as too old — the documented contract is that FFmpeg is 7.1 or
/// newer, and a failed probe is no reason to change how a download is built.
pub fn version(program: &Path) -> Option<FfmpegVersion> {
    static PROBED: OnceLock<Mutex<HashMap<PathBuf, Option<FfmpegVersion>>>> = OnceLock::new();
    let probed = PROBED.get_or_init(|| Mutex::new(HashMap::new()));
    // A poisoned cache is not worth failing a download over: probe again.
    let Ok(mut probed) = probed.lock() else {
        return probe_version(program);
    };
    *probed
        .entry(program.to_path_buf())
        .or_insert_with(|| probe_version(program))
}

/// Run `ffmpeg -version` and read the version off its first line.
fn probe_version(program: &Path) -> Option<FfmpegVersion> {
    let output = Command::new(program)
        .arg("-version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_version(stdout.lines().next()?)
}

/// Parse `ffmpeg version 6.1.1-3ubuntu5 Copyright (c) …` into `6.1.1`.
///
/// Distribution builds suffix the version (`6.1.1-3ubuntu5`) and some builds
/// prefix it (`n4.4.1`), so the numeric prefix of the token after `version` is
/// what counts. A snapshot build names no release (`N-109755-g1b9f9c1a3d`) and
/// yields `None`.
fn parse_version(line: &str) -> Option<FfmpegVersion> {
    let token = line
        .split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)?;
    let token = token
        .strip_prefix('n')
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(token);
    let numeric: String = token
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = numeric.split('.').filter(|part| !part.is_empty());
    Some(FfmpegVersion {
        major: parts.next()?.parse().ok()?,
        minor: parts.next().and_then(|part| part.parse().ok()).unwrap_or(0),
        patch: parts.next().and_then(|part| part.parse().ok()),
    })
}

/// How FFmpeg should report progress.
///
/// This is the only thing that differs between a run in a terminal and one
/// driven by the native host, and it is a mode rather than a set of flags so
/// that the two spellings cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reporting {
    /// A terminal reading FFmpeg's own `-stats` line on stderr.
    Cli,
    /// A caller reading machine-readable progress from stdout.
    Controlled { stats_period: f32 },
}

/// Seconds between `-progress` reports in [`Reporting::Controlled`].
pub const DEFAULT_STATS_PERIOD: f32 = 0.5;

impl Reporting {
    /// [`Reporting::Controlled`] at the default period.
    pub const CONTROLLED: Self = Self::Controlled {
        stats_period: DEFAULT_STATS_PERIOD,
    };
}

/// How hard FFmpeg should try to re-establish a dropped HTTP connection.
///
/// A value rather than a set of flags, for the reason
/// `docs/adr/0007-structured-ffmpeg-command-model.md` gives: the caller says
/// what it wants, and [`FfmpegInvocation::to_args`] is the only thing that
/// knows the spelling. `None` on the invocation means the options are not
/// passed at all, which is what `--no-reconnect` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    /// Seconds FFmpeg may spend backing off before it gives up on one
    /// connection. Its own default is 120, which is far longer than anyone
    /// waits at a progress bar.
    pub delay_max: u32,
}

/// The HTTP statuses worth reconnecting on.
///
/// Deliberately **not** `4xx,5xx`. A 403 or a 404 will not become a 200 by
/// being asked again, so reconnecting on the whole 4xx range turns a dead link
/// into repeated requests against someone else's server. The two exceptions are
/// the two that mean "later": 408 Request Timeout and 429 Too Many Requests.
/// See `docs/adr/0023-surviving-a-transient-failure.md`.
pub const RECONNECT_HTTP_STATUSES: &str = "5xx,408,429";

/// The default for [`Reconnect::delay_max`], in seconds.
pub const DEFAULT_RECONNECT_DELAY_MAX: u32 = 30;

impl Default for Reconnect {
    fn default() -> Self {
        Self {
            delay_max: DEFAULT_RECONNECT_DELAY_MAX,
        }
    }
}

/// Whether FFmpeg would fetch this input over HTTP, and so whether the
/// reconnect options mean anything for it. They are HTTP-protocol options; on a
/// local path FFmpeg rejects nothing but gains nothing either.
fn is_http_input(input: &str) -> bool {
    let lowered = input.trim_start().to_ascii_lowercase();
    lowered.starts_with("http://") || lowered.starts_with("https://")
}

/// One FFmpeg download, expressed as what it is rather than as argv.
///
/// [`FfmpegInvocation::to_args`] is the only place argument order is decided,
/// so no caller has to know where an option lands. The output path is a field
/// rather than something recovered from the last argv element.
#[derive(Debug, Clone)]
pub struct FfmpegInvocation {
    pub program: PathBuf,
    /// The media URL, passed to FFmpeg as a single argument so a shell can
    /// never see it; see `docs/adr/0002-cookie-scoping-and-argv-exposure.md`.
    pub input: String,
    /// A second input carrying the audio, when the HLS master declares it
    /// outside the video variant (`#EXT-X-MEDIA:TYPE=AUDIO` with a `URI`).
    ///
    /// Present, the two are muxed with explicit stream mapping. Absent, this is
    /// the single-input invocation it has always been. Nothing else in this
    /// project has a second input, so this is deliberately one optional audio
    /// input rather than a general list: the shape says what it is for. See
    /// `docs/adr/0014-pair-a-rendition-with-its-audio.md`.
    pub audio_input: Option<String>,
    /// The `-headers` block, applied to every request for this input.
    pub headers: Option<String>,
    /// The newline-delimited Set-Cookie syntax FFmpeg takes as `-cookies`,
    /// built by `scraper::ffmpeg_cookies`. Separate from `headers` because
    /// FFmpeg scopes `-cookies` per request host while applying `-headers` to
    /// every request.
    pub cookies: Option<String>,
    /// How hard to try to re-establish a dropped connection, or `None` to pass
    /// no reconnect options at all. Applied to HTTP(S) inputs only.
    pub reconnect: Option<Reconnect>,
    /// Whether to loosen FFmpeg's segment-extension checking. The caller
    /// decides, from the input *and* from whether this FFmpeg has the options
    /// at all — they exist only from [`MINIMUM_FFMPEG`]; see
    /// `docs/adr/0006-ffmpeg-version-detection.md`.
    pub hls_lenient: bool,
    pub threads: Option<u16>,
    pub overwrite: bool,
    pub output: PathBuf,
    pub reporting: Reporting,
}

#[derive(Clone, Debug)]
pub struct ProcessControl {
    child: Arc<Mutex<Option<std::process::Child>>>,
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    /// Set when a resume signals a live FFmpeg, cleared as soon as FFmpeg
    /// reports a later output timestamp. While it is set, FFmpeg has been told
    /// to continue but has not yet shown that it can: a failure in that window
    /// is a resume failure, not an ordinary one. See
    /// `docs/adr/0012-control-semantics.md`.
    resume_pending: Arc<AtomicBool>,
    /// Whether the cancel came from the user or from the host shutting down.
    ///
    /// Both stop FFmpeg the same way; they mean opposite things about the file.
    /// A requested cancel is the user saying they do not want it. A shutdown —
    /// EOF on the native port, which is what closing Firefox looks like — is
    /// nobody saying anything, and the extension already promises that such a
    /// job keeps what it had ("Interrupted by browser restart; partial file
    /// kept"). See `docs/adr/0012-control-semantics.md`.
    cancelled_for_shutdown: Arc<AtomicBool>,
    /// The latest `out_time_ms` FFmpeg has reported, and the value it stood at
    /// when the last resume was issued. Elapsed output is the only evidence
    /// that reaches this layer that segments are arriving again; the clock is
    /// not, because a paused process keeps no wall-clock promises.
    out_time_ms: Arc<AtomicU64>,
    resume_out_time_ms: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy)]
pub struct FfmpegProgress {
    pub out_time_ms: Option<u64>,
    pub finished: bool,
}

impl ProcessControl {
    pub fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            resume_pending: Arc::new(AtomicBool::new(false)),
            cancelled_for_shutdown: Arc::new(AtomicBool::new(false)),
            out_time_ms: Arc::new(AtomicU64::new(0)),
            resume_out_time_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Stop the download because the user asked. The part-written file is the
    /// caller's to dispose of; see `DownloadOptions::keep_partial`.
    pub fn cancel(&self) -> Result<(), String> {
        self.stop()
    }

    /// Stop the download because the host is going away, not because anyone
    /// asked. Identical to [`ProcessControl::cancel`] except that the
    /// part-written file is always kept.
    pub fn cancel_for_shutdown(&self) -> Result<(), String> {
        self.cancelled_for_shutdown.store(true, Ordering::SeqCst);
        self.stop()
    }

    /// Whether this download was cancelled by a request rather than by the host
    /// shutting down. The only cancel whose part-written file is up for
    /// deletion.
    pub fn cancelled_by_request(&self) -> bool {
        self.is_cancelled() && !self.cancelled_for_shutdown.load(Ordering::SeqCst)
    }

    fn stop(&self) -> Result<(), String> {
        self.cancelled.store(true, Ordering::SeqCst);
        let mut child = self
            .child
            .lock()
            .map_err(|_| "FFmpeg process control is unavailable".to_string())?;
        if let Some(child) = child.as_mut() {
            child
                .kill()
                .map_err(|error| format!("could not cancel FFmpeg: {error}"))?;
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("download is already cancelled".to_string());
        }
        self.paused.store(true, Ordering::SeqCst);
        self.signal_if_running(unix_signal::STOP)
    }

    pub fn resume(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("download is already cancelled".to_string());
        }
        self.paused.store(false, Ordering::SeqCst);
        // Record the mark *before* signalling, so progress that arrives in the
        // race between the two still counts as progress after the resume.
        self.resume_out_time_ms
            .store(self.out_time_ms.load(Ordering::SeqCst), Ordering::SeqCst);
        self.resume_pending.store(true, Ordering::SeqCst);
        let result = self.signal_if_running(unix_signal::CONT);
        if result.is_err() {
            // Nothing was resumed, so nothing is owed proof that it recovered.
            self.resume_pending.store(false, Ordering::SeqCst);
        }
        result
    }

    /// Record what FFmpeg last reported, and clear a pending resume once the
    /// output timestamp has moved past where it stood when resume was issued.
    ///
    /// Called by the executor for every progress report, so no caller can
    /// forget it.
    ///
    /// Measured against FFmpeg 6.1.1 with `-progress` at a 0.5 s stats period:
    /// a process under `SIGSTOP` writes no progress block at all (4 blocks
    /// before the stop, 4 after three seconds stopped, 10 three seconds after
    /// `SIGCONT`), and one blocked on a stalled HTTP input writes none either.
    /// So silence is the ordinary signal. The comparison still demands that the
    /// timestamp *advance* rather than merely arrive, because a block that
    /// repeats a timestamp says the same thing silence does.
    pub fn note_progress(&self, out_time_ms: Option<u64>) {
        let Some(out_time_ms) = out_time_ms else {
            return;
        };
        self.out_time_ms.fetch_max(out_time_ms, Ordering::SeqCst);
        if out_time_ms > self.resume_out_time_ms.load(Ordering::SeqCst) {
            self.resume_pending.store(false, Ordering::SeqCst);
        }
    }

    /// Whether FFmpeg was resumed and has not yet produced any further output.
    ///
    /// A download that ends in this state failed *at the resume*: while a
    /// process is stopped its sockets sit idle, and CDNs close them and expire
    /// signed segment URLs long before the user comes back. Worth naming
    /// separately because the remedy differs — retrying works, where retrying
    /// a genuinely broken input does not.
    pub fn resume_pending(&self) -> bool {
        self.resume_pending.load(Ordering::SeqCst)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn attach(&self, child: std::process::Child) -> Result<(), String> {
        let mut slot = self
            .child
            .lock()
            .map_err(|_| "FFmpeg process control is unavailable".to_string())?;
        *slot = Some(child);
        if self.is_cancelled() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        } else if self.is_paused() {
            if let Some(child) = slot.as_ref() {
                signal_process(child.id(), unix_signal::STOP)?;
            }
        }
        Ok(())
    }

    fn detach(&self) {
        if let Ok(mut child) = self.child.lock() {
            *child = None;
        }
    }

    fn signal_if_running(&self, signal: i32) -> Result<(), String> {
        let child = self
            .child
            .lock()
            .map_err(|_| "FFmpeg process control is unavailable".to_string())?;
        match child.as_ref() {
            Some(child) => signal_process(child.id(), signal),
            None => Ok(()),
        }
    }
}

/// Send a signal to the running FFmpeg.
///
/// There is no non-Unix arm here, and that is the point of
/// `docs/adr/0021-windows-is-unsupported.md`: pause and resume are SIGSTOP and
/// SIGCONT, ADR-0012 promises them, and a build that could not deliver them
/// would be promising something it cannot do. The crate refuses to compile off
/// Unix instead, so this is the only implementation there is.
fn signal_process(pid: u32, signal: i32) -> Result<(), String> {
    // SAFETY: pid is obtained from the live child process we spawned.
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "could not control FFmpeg: {}",
            std::io::Error::last_os_error()
        ))
    }
}

impl Default for ProcessControl {
    fn default() -> Self {
        Self::new()
    }
}

mod unix_signal {
    pub const CONT: i32 = libc::SIGCONT;
    pub const STOP: i32 = libc::SIGSTOP;
}

impl FfmpegInvocation {
    /// A plain download of `input` to `output`: no headers, no cookies, no
    /// leniency, no threading, reporting to a terminal.
    pub fn new(program: PathBuf, input: &str, output: PathBuf, overwrite: bool) -> Self {
        Self {
            program,
            input: input.to_string(),
            audio_input: None,
            headers: None,
            cookies: None,
            // On by default: a download that dies on one dropped connection is
            // the defect KEI-66 exists to remove, and the options cost nothing
            // when nothing goes wrong.
            reconnect: Some(Reconnect::default()),
            hls_lenient: false,
            threads: None,
            overwrite,
            output,
            reporting: Reporting::Cli,
        }
    }

    /// Render argv.
    ///
    /// This is the single source of argument order. Everything FFmpeg treats
    /// as an input option is emitted before `-i`, and the output path stays
    /// last, but nothing outside this function depends on either fact.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args = vec![OsString::from("-hide_banner"), OsString::from("-loglevel")];
        match self.reporting {
            Reporting::Cli => {
                args.push(OsString::from("error"));
                args.push(OsString::from("-stats"));
            }
            // `-progress` needs a log level that carries what it reports, and
            // `-nostats` keeps the human-readable line off stderr so the
            // machine-readable stream on stdout is the only progress source.
            Reporting::Controlled { stats_period } => {
                args.push(OsString::from("info"));
                args.push(OsString::from("-nostats"));
                args.push(OsString::from("-stats_period"));
                args.push(OsString::from(stats_period.to_string()));
                args.push(OsString::from("-progress"));
                args.push(OsString::from("pipe:1"));
            }
        }
        // `-allowed_segment_extensions`, `-headers` and `-cookies` are *input*
        // options: FFmpeg applies each to the next `-i` only. They are
        // therefore emitted once per input, or the audio playlist would be
        // fetched without the session that the video playlist needed.
        for input in [Some(&self.input), self.audio_input.as_ref()]
            .into_iter()
            .flatten()
        {
            // HTTP(S) only, and per input: the HLS demuxer honours these for
            // every segment it opens, which is what makes one 503 survivable.
            if let Some(reconnect) = self.reconnect.filter(|_| is_http_input(input)) {
                args.extend([
                    OsString::from("-reconnect"),
                    OsString::from("1"),
                    OsString::from("-reconnect_streamed"),
                    OsString::from("1"),
                    OsString::from("-reconnect_on_network_error"),
                    OsString::from("1"),
                    OsString::from("-reconnect_on_http_error"),
                    OsString::from(RECONNECT_HTTP_STATUSES),
                    OsString::from("-reconnect_delay_max"),
                    OsString::from(reconnect.delay_max.to_string()),
                ]);
            }
            if self.hls_lenient {
                args.push(OsString::from(SEGMENT_EXTENSION_OPTIONS[0]));
                args.push(OsString::from("ALL"));
                args.push(OsString::from(SEGMENT_EXTENSION_OPTIONS[1]));
                args.push(OsString::from("0"));
            }
            if let Some(headers) = &self.headers {
                args.push(OsString::from("-headers"));
                args.push(OsString::from(headers));
            }
            if let Some(cookies) = &self.cookies {
                args.push(OsString::from("-cookies"));
                args.push(OsString::from(cookies));
            }
            args.push(OsString::from("-i"));
            args.push(OsString::from(input));
        }
        if self.audio_input.is_some() {
            // Only with two inputs, and only then is it safe: input 0 is a
            // *media* playlist with one video stream, so `0:v:0` is
            // unambiguous. KEI-89 measured that the same option against a
            // *master* silently picks the first variant, which was the lowest.
            args.extend([
                OsString::from("-map"),
                OsString::from("0:v:0"),
                OsString::from("-map"),
                OsString::from("1:a:0"),
            ]);
        }
        args.extend([OsString::from("-c"), OsString::from("copy")]);
        if let Some(threads) = self.threads {
            args.push(OsString::from("-threads"));
            args.push(OsString::from(threads.to_string()));
        }
        args.push(OsString::from(if self.overwrite { "-y" } else { "-n" }));
        args.push(self.output.clone().into_os_string());
        args
    }
}

/// What replaces the output path in a logged command line.
///
/// The output *filename* is the page title when title naming is on, and
/// `AGENTS.md` allows a title into the output path and nowhere else — "never
/// let it into a log, an error, or any event but the output path". So the name
/// is dropped and the directory is logged separately, which is the half that
/// answers the question a log is asked ("where was it trying to write?").
pub const LOGGED_OUTPUT: &str = "<output>";

impl FfmpegInvocation {
    /// Render this invocation as a command line safe to write to a log file.
    ///
    /// Derived from [`FfmpegInvocation::to_args`] rather than rebuilt, so the
    /// logged command cannot drift from the executed one — the single-renderer
    /// rule of `docs/adr/0007-structured-ffmpeg-command-model.md` applies to
    /// this reading of it too.
    ///
    /// Three substitutions, each because the value is something a log must
    /// never carry:
    ///
    /// * `-cookies` becomes a **count**. Whether cookies were forwarded is the
    ///   first question in every protected-media report; which cookies they
    ///   were is never anyone's business.
    /// * `-headers` becomes its **field names**. `User-Agent` and `Referer` are
    ///   not secret today, but a log that names them survives a future header
    ///   that is.
    /// * The output path becomes [`LOGGED_OUTPUT`], for the reason above.
    ///
    /// URLs keep their scheme, host and path and lose their query, but that
    /// happens in `hostlog`, which redacts every value it writes.
    pub fn to_log_args(&self) -> Vec<String> {
        let output = self.output.clone().into_os_string();
        let mut rendered = Vec::new();
        let mut replacement: Option<String> = None;
        for argument in self.to_args() {
            if let Some(value) = replacement.take() {
                rendered.push(value);
                continue;
            }
            if argument == output {
                rendered.push(LOGGED_OUTPUT.to_string());
                continue;
            }
            let text = argument.to_string_lossy().into_owned();
            replacement = match text.as_str() {
                "-cookies" => Some(summarize_cookies(self.cookies.as_deref())),
                "-headers" => Some(summarize_headers(self.headers.as_deref())),
                _ => None,
            };
            rendered.push(text);
        }
        rendered
    }
}

/// `<2 cookies>` — how many were forwarded, never which.
fn summarize_cookies(cookies: Option<&str>) -> String {
    let count = cookies
        .map(|value| value.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0);
    match count {
        1 => "<1 cookie>".to_string(),
        other => format!("<{other} cookies>"),
    }
}

/// `<User-Agent,Referer>` — which headers were sent, never their values.
fn summarize_headers(headers: Option<&str>) -> String {
    let names: Vec<&str> = headers
        .map(|value| {
            value
                .lines()
                .filter_map(|line| line.split(':').next())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        return "<no headers>".to_string();
    }
    format!("<{}>", names.join(","))
}

/// Record a spawn in the host's log file.
///
/// A no-op in the CLI, which never initialises a logger (ADR-0022) — so this
/// sits at the spawn itself rather than in the host, and every FFmpeg run the
/// host makes is logged without the host having to remember to.
fn log_spawn(invocation: &FfmpegInvocation) {
    crate::hostlog::info(
        "ffmpeg.spawn",
        &[
            ("program", &invocation.program.display().to_string()),
            ("args", &invocation.to_log_args().join(" ")),
            // The directory, never the filename: the name can be a page title.
            (
                "output_dir",
                &invocation
                    .output
                    .parent()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_default(),
            ),
        ],
    );
}

pub fn execute(invocation: &FfmpegInvocation) -> DownerResult<PathBuf> {
    log_spawn(invocation);
    let mut child = Command::new(&invocation.program)
        .args(invocation.to_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) {
                DownerError::FfmpegUnavailable(invocation.program.clone())
            } else {
                DownerError::FfmpegFailed {
                    status: None,
                    stderr: error.to_string(),
                }
            }
        })?;

    let stderr = child
        .stderr
        .take()
        .map(capture_and_display)
        .unwrap_or_else(|| Ok(Vec::new()));
    let status = child.wait().map_err(|error| DownerError::FfmpegFailed {
        status: None,
        stderr: error.to_string(),
    })?;
    let stderr = stderr
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|error| error.to_string());

    if status.success() {
        Ok(invocation.output.clone())
    } else {
        Err(DownerError::FfmpegFailed {
            status: status.code(),
            stderr,
        })
    }
}

pub fn execute_controlled(
    invocation: &FfmpegInvocation,
    control: &ProcessControl,
) -> DownerResult<PathBuf> {
    execute_controlled_with_progress(invocation, control, |_| {})
}

pub fn execute_controlled_with_progress<F>(
    invocation: &FfmpegInvocation,
    control: &ProcessControl,
    progress: F,
) -> DownerResult<PathBuf>
where
    F: Fn(FfmpegProgress) + Send + Sync + 'static,
{
    execute_controlled_with_progress_and_logs(invocation, control, progress, |_| {})
}

pub fn execute_controlled_with_progress_and_logs<F, L>(
    invocation: &FfmpegInvocation,
    control: &ProcessControl,
    progress: F,
    log: L,
) -> DownerResult<PathBuf>
where
    F: Fn(FfmpegProgress) + Send + Sync + 'static,
    L: Fn(String) + Send + Sync + 'static,
{
    log_spawn(invocation);
    let mut child = Command::new(&invocation.program)
        .args(invocation.to_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) {
                DownerError::FfmpegUnavailable(invocation.program.clone())
            } else {
                DownerError::FfmpegFailed {
                    status: None,
                    stderr: error.to_string(),
                }
            }
        })?;

    let progress_output = child.stdout.take();
    let stderr = child.stderr.take();
    control
        .attach(child)
        .map_err(|error| DownerError::FfmpegFailed {
            status: None,
            stderr: error,
        })?;
    // Every progress report passes through the control first, so the resume
    // tracking cannot be forgotten by a caller that supplies its own callback.
    let progress_control = control.clone();
    let progress = move |value: FfmpegProgress| {
        progress_control.note_progress(value.out_time_ms);
        progress(value);
    };
    let progress_thread =
        progress_output.map(|output| thread::spawn(|| capture_progress(output, progress)));
    let stderr_thread =
        stderr.map(|stderr| thread::spawn(|| capture_and_display_with_callback(stderr, log)));

    let status = loop {
        let status = control
            .child
            .lock()
            .map_err(|_| DownerError::FfmpegFailed {
                status: None,
                stderr: "FFmpeg process control is unavailable".to_string(),
            })?
            .as_mut()
            .and_then(|child| child.try_wait().transpose())
            .transpose()
            .map_err(|error| DownerError::FfmpegFailed {
                status: None,
                stderr: error.to_string(),
            })?;
        if let Some(status) = status {
            break status;
        }
        thread::sleep(Duration::from_millis(100));
    };

    let progress_error = match progress_thread {
        Some(thread) => match thread.join() {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(_) => Some("FFmpeg progress thread panicked".to_string()),
        },
        None => None,
    };
    let mut stderr = match stderr_thread {
        Some(thread) => match thread.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("FFmpeg output thread panicked")),
        },
        None => Ok(Vec::new()),
    }
    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    .unwrap_or_else(|error| error.to_string());
    if let Some(error) = progress_error {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&error);
    }
    control.detach();

    if status.success() {
        Ok(invocation.output.clone())
    } else {
        Err(DownerError::FfmpegFailed {
            status: status.code(),
            stderr,
        })
    }
}

fn capture_and_display(stderr: ChildStderr) -> std::io::Result<Vec<u8>> {
    capture_and_display_with_callback(stderr, |_| {})
}

fn capture_and_display_with_callback<L>(mut stderr: ChildStderr, log: L) -> std::io::Result<Vec<u8>>
where
    L: Fn(String),
{
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut pending = String::new();
    loop {
        let count = stderr.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let chunk = String::from_utf8_lossy(&buffer[..count]);
        eprint!("{chunk}");
        pending.push_str(&chunk);
        let mut start = 0;
        for (index, character) in pending.char_indices() {
            if !matches!(character, '\n' | '\r') {
                continue;
            }
            let line = pending[start..index].trim();
            if !line.is_empty() {
                log(line.to_string());
            }
            start = index + character.len_utf8();
        }
        if start > 0 {
            pending.drain(..start);
        }
        captured.extend_from_slice(&buffer[..count]);
    }
    let line = pending.trim();
    if !line.is_empty() {
        log(line.to_string());
    }
    Ok(captured)
}

fn capture_progress<R, F>(output: R, progress: F) -> std::io::Result<()>
where
    R: Read,
    F: Fn(FfmpegProgress),
{
    let mut reader = BufReader::new(output);
    let mut line = String::new();
    let mut out_time_ms = None;
    loop {
        line.clear();
        let count = reader.read_line(&mut line)?;
        if count == 0 {
            break;
        }
        if let Some(value) = line.strip_prefix("out_time_us=") {
            out_time_ms = value.trim().parse::<u64>().ok().map(|value| value / 1_000);
        } else if let Some(value) = line.strip_prefix("out_time_ms=") {
            out_time_ms = value.trim().parse::<u64>().ok().map(|value| value / 1_000);
        } else if line.trim() == "progress=continue" {
            progress(FfmpegProgress {
                out_time_ms,
                finished: false,
            });
        } else if line.trim() == "progress=end" {
            progress(FfmpegProgress {
                out_time_ms,
                finished: true,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The rendered argv for each shape the builder supports.
    ///
    /// `tests/ffmpeg_argv.rs` pins the same six cases against a real process;
    /// this covers the rendering itself, including the pieces that never reach
    /// a fake because they concern quoting.
    #[test]
    fn renders_each_option_without_shell_interpolation() {
        let injected = FfmpegInvocation::new(
            PathBuf::from("ffmpeg"),
            "https://example.test/video?a=$HOME;echo injected",
            PathBuf::from("a file;name.mp4"),
            false,
        );
        let args = strings(&injected);
        // Both arrive as single argv elements, so no shell ever splits them.
        assert!(args.contains(&"https://example.test/video?a=$HOME;echo injected".to_string()));
        assert!(args.contains(&"a file;name.mp4".to_string()));
        assert!(args.contains(&"-n".to_string()));
        assert!(!args.iter().any(|argument| argument == "sh"));

        let threaded = FfmpegInvocation {
            threads: Some(4),
            ..plain()
        };
        assert!(strings(&threaded)
            .windows(2)
            .any(|pair| pair == ["-threads", "4"]));

        let overwrite = FfmpegInvocation {
            overwrite: true,
            ..plain()
        };
        assert!(strings(&overwrite).iter().any(|argument| argument == "-y"));

        let with_headers = FfmpegInvocation {
            headers: Some("Referer: https://example.test/page\r\n".to_string()),
            ..plain()
        };
        let args = strings(&with_headers);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-headers", "Referer: https://example.test/page\r\n"]));
        assert!(
            !args.iter().any(|argument| argument == "-cookies"),
            "no -cookies argument when there are no cookies"
        );

        let with_cookies = FfmpegInvocation {
            cookies: Some("sid=downer-sentinel; path=/; domain=example.test".to_string()),
            ..plain()
        };
        let args = strings(&with_cookies);
        let cookies = args
            .iter()
            .position(|argument| argument == "-cookies")
            .expect("-cookies is passed");
        assert_eq!(
            args[cookies + 1],
            "sid=downer-sentinel; path=/; domain=example.test"
        );
        assert!(
            cookies < args.iter().position(|argument| argument == "-i").unwrap(),
            "-cookies is an input option: {args:?}"
        );
    }

    /// The leniency options are the caller's decision, not a URL substring
    /// match, so an old FFmpeg simply never has them rendered (ADR-0006).
    #[test]
    fn an_http_input_gets_the_reconnect_options_before_its_own_i() {
        let mut invocation = plain();
        invocation.reconnect = Some(Reconnect { delay_max: 45 });
        let args = strings(&invocation);

        let reconnect = args
            .iter()
            .position(|a| a == "-reconnect")
            .expect("present");
        let input = args.iter().position(|a| a == "-i").expect("present");
        assert!(
            reconnect < input,
            "input options precede their -i: {args:?}"
        );

        for pair in [
            ["-reconnect", "1"],
            ["-reconnect_streamed", "1"],
            ["-reconnect_on_network_error", "1"],
            ["-reconnect_on_http_error", RECONNECT_HTTP_STATUSES],
            ["-reconnect_delay_max", "45"],
        ] {
            assert!(
                args.windows(2).any(|window| window == pair),
                "{pair:?} missing from {args:?}"
            );
        }
    }

    /// The narrowing that matters: a 403 or a 404 is never reconnected on,
    /// because it will not become a 200 and retrying it hammers a server that
    /// has already answered. See ADR-0023.
    #[test]
    fn reconnect_covers_server_errors_and_the_two_retryable_client_ones() {
        assert_eq!(RECONNECT_HTTP_STATUSES, "5xx,408,429");
        assert!(!RECONNECT_HTTP_STATUSES.contains("4xx"));
    }

    #[test]
    fn no_reconnect_leaves_every_reconnect_option_off_the_command_line() {
        let mut invocation = plain();
        invocation.reconnect = None;
        let args = strings(&invocation).join(" ");
        assert!(!args.contains("-reconnect"), "{args}");
    }

    /// They are HTTP options; a local file gains nothing from them.
    #[test]
    fn a_local_input_gets_no_reconnect_options() {
        let mut invocation = plain();
        invocation.input = "/tmp/local.mp4".to_string();
        invocation.reconnect = Some(Reconnect::default());
        let args = strings(&invocation).join(" ");
        assert!(!args.contains("-reconnect"), "{args}");
    }

    /// Per input, like `-headers` and `-cookies`: the audio playlist is fetched
    /// over the same flaky network as the video one.
    #[test]
    fn a_paired_audio_input_gets_its_own_reconnect_options() {
        let mut invocation = plain();
        invocation.audio_input = Some("https://cdn.example.test/audio.m3u8".to_string());
        invocation.reconnect = Some(Reconnect::default());
        let args = strings(&invocation);
        let count = args.iter().filter(|a| *a == "-reconnect").count();
        assert_eq!(count, 2, "one per input: {args:?}");
    }

    #[test]
    fn http_inputs_are_recognised_by_scheme_only() {
        assert!(is_http_input("https://example.test/a.m3u8"));
        assert!(is_http_input("HTTP://example.test/a.m3u8"));
        assert!(!is_http_input("/var/tmp/a.mp4"));
        assert!(!is_http_input("file:///tmp/a.mp4"));
    }

    /// The logged command line carries no cookie, under any spelling.
    ///
    /// The sentinel is fake on purpose: `AGENTS.md` forbids a real cookie in a
    /// test, and a fake one proves the same thing — that the value never
    /// reaches the rendering, whatever it was.
    #[test]
    fn a_logged_command_line_counts_cookies_instead_of_carrying_them() {
        const SENTINEL: &str = "DOWNER-COOKIE-SENTINEL-2f8a";
        let mut invocation = plain();
        invocation.cookies = Some(format!(
            "session={SENTINEL}; path=/; domain=example.com\nother={SENTINEL}; path=/; domain=example.com"
        ));

        let logged = invocation.to_log_args().join(" ");
        assert!(!logged.contains(SENTINEL), "{logged}");
        assert!(!logged.contains("session="), "{logged}");
        assert!(logged.contains("-cookies <2 cookies>"), "{logged}");

        // And the executed command still carries the real thing, or the
        // download would break.
        let executed = strings(&invocation).join(" ");
        assert!(executed.contains(SENTINEL));
    }

    #[test]
    fn one_cookie_is_counted_in_the_singular_and_none_at_all_is_zero() {
        let mut invocation = plain();
        invocation.cookies = Some("session=x; path=/; domain=example.com".to_string());
        assert!(invocation.to_log_args().join(" ").contains("<1 cookie>"));
        assert_eq!(summarize_cookies(None), "<0 cookies>");
    }

    /// Headers are named, not quoted. A `Referer` tells you which page's
    /// session was in play; its value would not add anything a log may hold.
    #[test]
    fn a_logged_command_line_names_headers_without_their_values() {
        let mut invocation = plain();
        invocation.headers = Some(
            "User-Agent: Mozilla/5.0 (secret build 9)\r\nReferer: https://example.com/watch?id=42\r\n"
                .to_string(),
        );

        let logged = invocation.to_log_args().join(" ");
        assert!(logged.contains("-headers <User-Agent,Referer>"), "{logged}");
        assert!(!logged.contains("Mozilla"), "{logged}");
        assert!(!logged.contains("id=42"), "{logged}");
    }

    /// The output filename is the page title when title naming is on, and
    /// `AGENTS.md` keeps a title out of every log.
    #[test]
    fn a_logged_command_line_drops_the_output_filename() {
        let invocation = FfmpegInvocation::new(
            PathBuf::from("ffmpeg"),
            "https://example.com/video.m3u8",
            PathBuf::from("/home/someone/Downloads/Someone's Private Video Title.mp4"),
            false,
        );

        let logged = invocation.to_log_args().join(" ");
        assert!(!logged.contains("Private Video Title"), "{logged}");
        assert!(logged.ends_with(LOGGED_OUTPUT), "{logged}");
    }

    /// The logged line is the executed line, substitutions aside — so a reader
    /// debugging from the log is reading what actually ran.
    #[test]
    fn a_logged_command_line_matches_the_executed_one_position_for_position() {
        let mut invocation = plain();
        invocation.cookies = Some("session=x; path=/; domain=example.com".to_string());
        invocation.headers = Some("User-Agent: test\r\n".to_string());
        invocation.hls_lenient = true;
        invocation.threads = Some(4);

        let executed = strings(&invocation);
        let logged = invocation.to_log_args();
        assert_eq!(executed.len(), logged.len(), "{logged:?}");

        let substituted: Vec<usize> = executed
            .iter()
            .zip(&logged)
            .enumerate()
            .filter(|(_, (left, right))| left != right)
            .map(|(index, _)| index)
            .collect();
        // Exactly three: the cookie value, the header block, and the output.
        assert_eq!(substituted.len(), 3, "{substituted:?} in {logged:?}");
    }

    #[test]
    fn segment_extension_options_follow_the_lenient_flag() {
        let lenient = FfmpegInvocation {
            input: "https://example.test/playlist.m3u8".to_string(),
            audio_input: None,
            hls_lenient: true,
            ..plain()
        };
        let args = strings(&lenient);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-allowed_segment_extensions", "ALL"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-extension_picky", "0"]));

        // The same input with leniency off loses both options and both values,
        // and nothing else: the `0` that a value-matching filter would eat is
        // gone with its option, while the rest of the command is untouched.
        let strict = FfmpegInvocation {
            hls_lenient: false,
            ..lenient.clone()
        };
        let args = strings(&strict);
        assert!(!args
            .iter()
            .any(|argument| argument == "-allowed_segment_extensions"));
        assert!(!args.iter().any(|argument| argument == "ALL"));
        assert!(!args.iter().any(|argument| argument == "-extension_picky"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-i", "https://example.test/playlist.m3u8"]));
        assert_eq!(args.last().unwrap(), "video.mp4");
    }

    /// Controlled mode swaps the reporting flags and leaves everything else
    /// alone — the property the old index-based `splice` depended on argument
    /// order to achieve.
    #[test]
    fn controlled_reporting_changes_only_the_reporting_flags() {
        let cli = strings(&plain());
        let controlled = strings(&FfmpegInvocation {
            reporting: Reporting::CONTROLLED,
            ..plain()
        });

        assert!(cli.windows(2).any(|pair| pair == ["-loglevel", "error"]));
        assert!(cli.iter().any(|argument| argument == "-stats"));
        assert!(controlled
            .windows(2)
            .any(|pair| pair == ["-loglevel", "info"]));
        assert!(controlled
            .windows(2)
            .any(|pair| pair == ["-progress", "pipe:1"]));
        assert!(controlled
            .windows(2)
            .any(|pair| pair == ["-stats_period", "0.5"]));
        // `-stats` and `-nostats` are distinct arguments; match exactly.
        assert!(!controlled.iter().any(|argument| argument == "-stats"));
        assert_eq!(
            controlled
                .iter()
                .filter(|argument| *argument == "-loglevel")
                .count(),
            1,
            "one log level, not one overriding another: {controlled:?}"
        );

        // Everything after the reporting flags is identical.
        let tail = |args: &[String]| {
            let at = args.iter().position(|argument| argument == "-i").unwrap();
            args[at..].to_vec()
        };
        assert_eq!(tail(&cli), tail(&controlled));
    }

    /// A plain invocation the case tests vary one field of.
    fn plain() -> FfmpegInvocation {
        FfmpegInvocation::new(
            PathBuf::from("ffmpeg"),
            "https://example.test/video.mp4",
            PathBuf::from("video.mp4"),
            false,
        )
    }

    fn strings(invocation: &FfmpegInvocation) -> Vec<String> {
        invocation
            .to_args()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn reads_the_version_off_the_line_ffmpeg_actually_prints() {
        // The Ubuntu 24.04 build this issue was reported against, verbatim.
        let ubuntu = parse_version(
            "ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2023 the FFmpeg developers",
        )
        .expect("a distribution version parses");
        assert_eq!(ubuntu.to_string(), "6.1.1");
        assert!(!ubuntu.meets_minimum());

        let conda =
            parse_version("ffmpeg version 9.0.1 Copyright (c) 2000-2026 the FFmpeg developers")
                .expect("a release version parses");
        assert_eq!(conda.to_string(), "9.0.1");
        assert!(conda.meets_minimum());

        // The boundary itself, and the release just below it.
        assert!(parse_version("ffmpeg version 7.1 Copyright")
            .unwrap()
            .meets_minimum());
        assert!(!parse_version("ffmpeg version 7.0.2 Copyright")
            .unwrap()
            .meets_minimum());

        // Some builds prefix the tag with `n`.
        assert_eq!(
            parse_version("ffmpeg version n4.4.1 Copyright")
                .unwrap()
                .to_string(),
            "4.4.1"
        );

        // A snapshot build names no release. Undetectable is not "too old":
        // those builds are newer than 7.1, not older.
        assert!(parse_version("ffmpeg version N-109755-g1b9f9c1a3d Copyright").is_none());
        assert!(parse_version("not ffmpeg at all").is_none());

        assert_eq!(MINIMUM_FFMPEG.to_string(), "7.1");
    }

    #[test]
    fn parses_ffmpeg_progress_from_a_dedicated_stream() {
        let input = Cursor::new(
            b"out_time_us=1500000\nout_time_ms=1500000\nprogress=continue\nprogress=end\n",
        );
        let updates = Arc::new(Mutex::new(Vec::new()));
        let captured = updates.clone();
        capture_progress(input, move |progress| {
            captured.lock().unwrap().push(progress)
        })
        .unwrap();
        let updates = updates.lock().unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].out_time_ms, Some(1_500));
        assert!(!updates[0].finished);
        assert!(updates[1].finished);
    }

    #[test]
    fn pause_and_resume_can_be_queued_before_ffmpeg_starts() {
        let control = ProcessControl::new();
        control.pause().unwrap();
        assert!(control.is_paused());
        control.resume().unwrap();
        assert!(!control.is_paused());
        control.cancel().unwrap();
        assert!(control.is_cancelled());
    }
}
