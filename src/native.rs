use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{DownerError, DownerResult},
    ffmpeg::{FfmpegProgress, ProcessControl},
    output::{NamingHints, OnConflict},
    scraper::{hls_info_with_timeout, HlsInfo, ResolvedMedia},
    DownloadOptions,
};

const MAX_MESSAGE_BYTES: u32 = 1_048_576;

/// Wire contract version. See `docs/protocol.md` and
/// `docs/adr/0001-native-messaging-protocol.md`; the vocabulary is pinned by
/// `tests/fixtures/protocol.json`, which both test suites read.
pub const PROTOCOL_VERSION: u32 = 1;
/// Requests that omit `protocol_version` are treated as this legacy version and
/// still served, so an extension built before the handshake keeps working for
/// one release cycle.
const LEGACY_PROTOCOL_VERSION: u32 = 0;

const EVENT_HELLO: &str = "hello";
const EVENT_STATUS: &str = "status";
const EVENT_ACK: &str = "ack";
const EVENT_PROGRESS: &str = "progress";
const EVENT_LOG: &str = "log";
const EVENT_TERMINAL: &str = "terminal";
const EVENT_REJECTED: &str = "rejected";
const EVENT_CONTROL_ERROR: &str = "control-error";

const STATE_READY: &str = "ready";
const STATE_REJECTED: &str = "rejected";
const STATE_CONTROL_ERROR: &str = "control-error";

const ERROR_INVALID_REQUEST: &str = "invalid_request";
const ERROR_UNSUPPORTED_COMMAND: &str = "unsupported_command";
const ERROR_UNSUPPORTED_PROTOCOL_VERSION: &str = "unsupported_protocol_version";
const ERROR_DUPLICATE_JOB: &str = "duplicate_job";
/// A second `download` on a connection that is already running one. Distinct
/// from `duplicate_job`, which is the same job named twice: this is a
/// *different* job arriving at a host that serves one. See
/// `docs/adr/0013-one-download-per-host-process.md`.
const ERROR_HOST_BUSY: &str = "host_busy";
const ERROR_TASK_NOT_ACTIVE: &str = "task_not_active";
const ERROR_INVALID_HLS_INFO: &str = "invalid_hls_info";
const ERROR_DOWNLOAD_FAILED: &str = "download_failed";
const ERROR_CONTROL_FAILED: &str = "control_failed";
const ERROR_CANCELLED: &str = "cancelled";
/// A download that died at the resume, before FFmpeg produced any further
/// output. Separate from `download_failed` because the remedy differs: the
/// input is fine, the connections it was holding are not. See
/// `docs/adr/0012-control-semantics.md`.
const ERROR_RESUME_FAILED: &str = "resume_failed";

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
type SharedOutput = Arc<Mutex<io::Stdout>>;
/// The one job this host process may be running.
///
/// A single slot, not a map: the extension opens a native port per download
/// (`background.js::nativeDownload`) and this process serves exactly that one.
/// A map here would describe a multiplexing host that nothing on either side
/// implements. `job_id` still travels on the wire, so multiplexing could be
/// reintroduced without a protocol break — but the code no longer claims to
/// support it already. See `docs/adr/0013-one-download-per-host-process.md`.
type ActiveJob = Arc<Mutex<Option<(String, ActiveTask)>>>;

#[derive(Clone, Debug)]
struct ActiveTask {
    control: ProcessControl,
    hls_info: Arc<Mutex<Option<HlsInfo>>>,
    progress: Arc<Mutex<Option<FfmpegProgress>>>,
    /// Where this download will write, once the name has been inferred.
    ///
    /// Recorded so a job that ends badly can still say where its part-written
    /// file is: `download_resolved` returns the path only on success, and by
    /// the time an error surfaces the name it chose is gone. See KEI-86.
    target: Arc<Mutex<Option<PathBuf>>>,
}

#[derive(Debug, Deserialize)]
struct NativeRequest {
    command: String,
    #[serde(default)]
    protocol_version: Option<u32>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    output_dir: Option<PathBuf>,
    /// The playlist the extension fetched in the page's context.
    ///
    /// Sent instead of the extension parsing it: one implementation reads a
    /// playlist, and it is this one. Absent — the CLI, or a fetch the extension
    /// could not make — the host fetches for itself, as it always has. See
    /// `docs/adr/0011-one-playlist-parser.md`.
    #[serde(default)]
    playlist_text: Option<String>,
    /// The FFmpeg the user chose on the Settings page.
    ///
    /// Firefox launches the host with a minimal environment, so `DOWNER_FFMPEG`
    /// cannot reach it and the user needs some way to say which FFmpeg to use.
    /// Absent, the host discovers one as it always has. This does not widen who
    /// can run what: the host already runs an FFmpeg named by its own config
    /// file, and only the pinned extension ID can talk to it at all.
    #[serde(default)]
    ffmpeg: Option<PathBuf>,
    #[serde(default)]
    overwrite: bool,
    /// What to do when the inferred output path is taken. Absent means
    /// [`OnConflict::Rename`]: the host only ever infers a name into
    /// `output_dir`, never an exact path, so renaming beside an existing file
    /// is the safe default even for a client that predates this field. An
    /// unrecognised value fails the frame rather than being ignored — silently
    /// misreading a collision policy could cost a user a file.
    #[serde(default)]
    on_conflict: Option<OnConflict>,
    /// The source page's title, used for naming only when the URL-derived stem
    /// is generic. Bounded and sanitised by `output::sanitize_title`; never
    /// logged or echoed in any event.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    threads: Option<u16>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    total_segments: Option<u64>,
    #[serde(default)]
    total_duration_ms: Option<u64>,
    /// Keep the partly written file when the download is **cancelled**.
    ///
    /// Absent means delete it: a cancel is the user saying they do not want
    /// this file, and a half-written video left in their downloads folder is
    /// litter they did not ask for. A client that wants the fragment for
    /// diagnostics asks for it. Failures are not cancels — those keep their
    /// partial file either way. See `docs/adr/0012-control-semantics.md`.
    #[serde(default)]
    keep_partial: bool,
}

#[derive(Debug, Serialize)]
struct Capabilities {
    /// Pause and resume use Unix process signals, so they are unavailable elsewhere.
    pause_resume: bool,
    hls_info: bool,
}

#[derive(Debug, Serialize)]
struct NativeResponse {
    protocol_version: u32,
    #[serde(rename = "type")]
    event_type: &'static str,
    ok: bool,
    path: Option<PathBuf>,
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_version: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Capabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<crate::diagnostics::Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_segments: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_segments: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<f64>,
    /// How far into the media FFmpeg has got, in milliseconds.
    ///
    /// Reported whenever FFmpeg has said, whether or not a segment total is
    /// known. It is the only evidence a download with no playlist metadata has
    /// that anything is happening at all, and it was being computed and thrown
    /// away. `percent` still requires a total: a numerator is informative, an
    /// invented fraction is not. See KEI-86.
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    /// Why the segment total is unavailable, when it is.
    ///
    /// Carried on a `progress` event because the download is still running: a
    /// probe that could not read the playlist is not a job state, and never
    /// terminates anything. See KEI-86.
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

impl Default for NativeResponse {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            event_type: EVENT_PROGRESS,
            ok: true,
            path: None,
            error: None,
            error_code: None,
            host_version: None,
            capabilities: None,
            status: None,
            job_id: None,
            state: None,
            completed_segments: None,
            total_segments: None,
            percent: None,
            elapsed_ms: None,
            metadata_error: None,
            request_id: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct NativeLogResponse {
    protocol_version: u32,
    #[serde(rename = "type")]
    event_type: &'static str,
    ok: bool,
    job_id: String,
    state: String,
    log: String,
}

pub fn run_stdio() -> DownerResult<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let output = Arc::new(Mutex::new(io::stdout()));
    let job = Arc::new(Mutex::new(None));

    loop {
        let Some(payload) = read_message(&mut input).map_err(DownerError::NativeIo)? else {
            cancel_for_shutdown(&job);
            return Ok(());
        };
        let request = match serde_json::from_slice::<NativeRequest>(&payload) {
            Ok(request) => request,
            Err(error) => {
                // A malformed frame carries no request_id and no job_id, so it is
                // rejected without touching any running job.
                let response = rejected(
                    ERROR_INVALID_REQUEST,
                    format!("invalid native request: {error}"),
                    None,
                    None,
                );
                send_response(&output, &response).map_err(DownerError::NativeIo)?;
                continue;
            }
        };
        let version = request.protocol_version.unwrap_or(LEGACY_PROTOCOL_VERSION);
        if version != PROTOCOL_VERSION && version != LEGACY_PROTOCOL_VERSION {
            let response = rejected(
                ERROR_UNSUPPORTED_PROTOCOL_VERSION,
                format!(
                    "unsupported protocol version {version}; this host speaks version {PROTOCOL_VERSION}"
                ),
                request.request_id,
                None,
            );
            send_response(&output, &response).map_err(DownerError::NativeIo)?;
            continue;
        }
        if request.command == "download" {
            start_download(request, output.clone(), job.clone()).map_err(DownerError::NativeIo)?;
            continue;
        }
        let response = match request.command.as_str() {
            "hello" => hello_response(request.request_id),
            "status" => status_response(
                request.output_dir.as_deref(),
                request.ffmpeg.as_deref(),
                request.request_id,
            ),
            "pause" | "resume" | "cancel" => control_download(request, &job),
            "hls-info" => update_hls_info(request, &job),
            command => rejected(
                ERROR_UNSUPPORTED_COMMAND,
                format!("unsupported native command: {command}"),
                request.request_id,
                None,
            ),
        };
        send_response(&output, &response).map_err(DownerError::NativeIo)?;
    }
}

/// Answer the connection handshake. Cheap by design: the extension sends this on
/// every connect because a host process is started per download.
fn hello_response(request_id: Option<String>) -> NativeResponse {
    NativeResponse {
        event_type: EVENT_HELLO,
        ok: true,
        state: Some(STATE_READY.to_string()),
        host_version: Some(env!("CARGO_PKG_VERSION")),
        capabilities: Some(Capabilities {
            pause_resume: cfg!(unix),
            hls_info: true,
        }),
        request_id,
        ..NativeResponse::default()
    }
}

/// Answer the setup-diagnostics request.
///
/// Unlike `hello` this does I/O — it runs `ffmpeg -version` and writes a probe
/// file into `output_dir` — which is exactly why it is a separate command and
/// not part of the handshake: the handshake is on the path of every download,
/// and this is on the path of a button the user pressed.
///
/// `output_dir` is the extension's configured download directory. Absent, the
/// directory check is skipped rather than guessed at.
fn status_response(
    output_dir: Option<&Path>,
    ffmpeg: Option<&Path>,
    request_id: Option<String>,
) -> NativeResponse {
    // Resolve the directory exactly as `download` does, so the check reports on
    // the directory a download would really use. Most users configure none, and
    // checking nothing in the commonest case would make the panel's promise
    // that downloads "can be written where you asked" untrue by default.
    let directory = output_dir
        .map(Path::to_path_buf)
        .or_else(crate::output::default_download_dir);
    let report = crate::diagnostics::run(directory.as_deref(), ffmpeg);
    NativeResponse {
        event_type: EVENT_STATUS,
        ok: true,
        state: Some(STATE_READY.to_string()),
        host_version: Some(env!("CARGO_PKG_VERSION")),
        capabilities: Some(Capabilities {
            pause_resume: cfg!(unix),
            hls_info: true,
        }),
        status: Some(report),
        request_id,
        ..NativeResponse::default()
    }
}

/// A request the host refused to act on. `rejected` is deliberately **not** a
/// job state: the client must never treat it as terminal, so a malformed or
/// unsupported message can no longer end a running job's channel.
fn rejected(
    error_code: &'static str,
    error: String,
    request_id: Option<String>,
    job_id: Option<String>,
) -> NativeResponse {
    NativeResponse {
        event_type: EVENT_REJECTED,
        ok: false,
        error: Some(error),
        error_code: Some(error_code),
        job_id,
        state: Some(STATE_REJECTED.to_string()),
        request_id,
        ..NativeResponse::default()
    }
}

fn start_download(request: NativeRequest, output: SharedOutput, job: ActiveJob) -> io::Result<()> {
    let job_id = request.job_id.clone().unwrap_or_else(next_job_id);
    let initial_info = match (request.total_segments, request.total_duration_ms) {
        (Some(total_segments), Some(total_duration_ms)) if total_segments > 0 => Some(HlsInfo {
            total_segments,
            total_duration_ms,
        }),
        _ => None,
    };
    let task = ActiveTask {
        control: ProcessControl::new(),
        hls_info: Arc::new(Mutex::new(initial_info)),
        progress: Arc::new(Mutex::new(None)),
        target: Arc::new(Mutex::new(None)),
    };
    let mut active = job
        .lock()
        .map_err(|_| io::Error::other("download task registry is unavailable"))?;
    // Rejected rather than failed, either way: the job already running here must
    // not be terminated by a second `download` arriving on its connection.
    if let Some((running, _)) = active.as_ref() {
        let (error_code, error) = if running == &job_id {
            // The same job named twice.
            (
                ERROR_DUPLICATE_JOB,
                format!("download task already exists: {job_id}"),
            )
        } else {
            // A different job, at a host that serves one. The extension opens a
            // port per download and never does this; a client that did would
            // otherwise have got a second download sharing one process, which
            // is the model this host does not implement (ADR-0013).
            (
                ERROR_HOST_BUSY,
                format!("this host is already running download {running}"),
            )
        };
        return send_response(
            &output,
            &rejected(error_code, error, request.request_id, Some(job_id)),
        );
    }
    *active = Some((job_id.clone(), task.clone()));
    drop(active);

    if let Err(error) = send_response(
        &output,
        &NativeResponse {
            event_type: EVENT_PROGRESS,
            ok: true,
            job_id: Some(job_id.clone()),
            state: Some("starting".to_string()),
            total_segments: initial_info.map(|info| info.total_segments),
            percent: initial_info.map(|_| 0.0),
            request_id: request.request_id.clone(),
            ..NativeResponse::default()
        },
    ) {
        clear_job(&job, &job_id);
        return Err(error);
    }

    start_hls_preflight(&request, &job_id, &task, output.clone(), job.clone());

    let worker_job_id = job_id.clone();
    let worker_task = task.clone();
    thread::spawn(move || {
        let response = if worker_task.control.is_cancelled() {
            cancelled_response(&worker_job_id, &worker_task)
        } else {
            match download(request, &worker_task, output.clone(), &worker_job_id) {
                Ok(_path) if worker_task.control.is_cancelled() => {
                    cancelled_response(&worker_job_id, &worker_task)
                }
                Ok(path) => completed_response(&worker_job_id, path, &worker_task),
                Err(_error) if worker_task.control.is_cancelled() => {
                    cancelled_response(&worker_job_id, &worker_task)
                }
                Err(error) => NativeResponse {
                    event_type: EVENT_TERMINAL,
                    ok: false,
                    // A failure keeps its part-written file (ADR-0012), so the
                    // path is the difference between "you have a fragment at X"
                    // and a user hunting their downloads folder for it.
                    path: target_path(&worker_task),
                    // `DownerError::FfmpegFailed` embeds FFmpeg's stderr tail,
                    // which is exactly the text the `log` events are redacted
                    // for. The terminal error takes the same treatment, or a
                    // token would simply move from the log to the failure
                    // message the popup shows and persists.
                    error: Some(crate::redact::redact_text(&error.to_string())),
                    // Still `failed`, still one terminal event: only the code
                    // and the advice change. A resume failure is a failure.
                    error_code: Some(if worker_task.control.resume_pending() {
                        ERROR_RESUME_FAILED
                    } else {
                        ERROR_DOWNLOAD_FAILED
                    }),
                    job_id: Some(worker_job_id.clone()),
                    state: Some("failed".to_string()),
                    ..NativeResponse::default()
                },
            }
        };
        let _ = send_response(&output, &response);
        clear_job(&job, &worker_job_id);
    });

    Ok(())
}

fn completed_response(job_id: &str, path: PathBuf, task: &ActiveTask) -> NativeResponse {
    let info = task.hls_info.lock().ok().and_then(|info| *info);
    let progress = task.progress.lock().ok().and_then(|progress| *progress);
    let mut response = progress_response(
        job_id,
        info,
        Some(FfmpegProgress {
            out_time_ms: progress.and_then(|value| value.out_time_ms),
            finished: true,
        }),
        "completed",
        None,
    );
    response.event_type = EVENT_TERMINAL;
    response.path = Some(path);
    response
}

fn start_hls_preflight(
    request: &NativeRequest,
    job_id: &str,
    task: &ActiveTask,
    output: SharedOutput,
    job: ActiveJob,
) {
    if task.hls_info.lock().ok().and_then(|info| *info).is_some()
        || !request.url.to_ascii_lowercase().contains(".m3u8")
    {
        return;
    }
    let Ok(url) = crate::output::validate_url(&request.url) else {
        return;
    };
    // The extension fetched this in the page's context. When it is a media
    // playlist the totals are already in hand, so no fetch is needed at all —
    // and no second implementation has to agree about what they are.
    if let Some(text) = request.playlist_text.as_deref() {
        if let crate::scraper::Playlist::Media(info) = crate::scraper::parse_playlist(text, &url) {
            publish_hls_info(job_id, info, task, &output);
            return;
        }
    }
    let referer = request
        .source_url
        .as_deref()
        .and_then(|url| crate::output::validate_url(url).ok());
    let user_agent = request
        .user_agent
        .clone()
        .unwrap_or_else(|| "Mozilla/5.0 (Firefox; downer native host)".to_string());
    let cookie = request.cookie.clone();
    let job_id = job_id.to_string();
    let task = task.clone();
    thread::spawn(move || {
        let info = hls_info_with_timeout(
            &url,
            &user_agent,
            referer.as_ref(),
            cookie.as_deref(),
            Duration::from_secs(8),
        );
        let still_active = job
            .lock()
            .ok()
            .is_some_and(|active| active.as_ref().is_some_and(|(id, _)| id == &job_id));
        if !still_active {
            return;
        }
        match info {
            Ok(info) => publish_hls_info(&job_id, info, &task, &output),
            // A probe that fails says why, instead of leaving the row on
            // "waiting for playlist metadata" with no account of what went
            // wrong. The download itself is unaffected: FFmpeg fetches the
            // playlist for itself, in a session this probe does not have.
            Err(problem) => publish_metadata_problem(&job_id, &problem, &task, &output),
        }
    });
}

/// Tell the extension why the segment total is unavailable.
///
/// A `progress` event, not an error: the download is still running and this is
/// not a job state. The message is the one `resolve_media` would give for the
/// same cause, so the CLI and the extension say the same thing.
fn publish_metadata_problem(
    job_id: &str,
    problem: &crate::scraper::PlaylistProblem,
    task: &ActiveTask,
    output: &SharedOutput,
) {
    let progress = task.progress.lock().ok().and_then(|progress| *progress);
    let state = if task.control.is_paused() {
        "paused"
    } else {
        "downloading"
    };
    let mut response = progress_response(job_id, None, progress, state, None);
    response.metadata_error = Some(crate::redact::redact_text(&problem.message()));
    let _ = send_response(output, &response);
}

/// Record playlist totals on the task and tell the extension.
///
/// Shared by both ways they arrive: parsed from the text the extension sent, or
/// fetched by the host when it did not.
fn publish_hls_info(job_id: &str, info: HlsInfo, task: &ActiveTask, output: &SharedOutput) {
    if let Ok(mut current) = task.hls_info.lock() {
        *current = Some(info);
    }
    let progress = task.progress.lock().ok().and_then(|progress| *progress);
    let state = if task.control.is_paused() {
        "paused"
    } else {
        "downloading"
    };
    let _ = send_response(
        output,
        &progress_response(job_id, Some(info), progress, state, None),
    );
}

fn control_download(request: NativeRequest, job: &ActiveJob) -> NativeResponse {
    let job_id = request.job_id.unwrap_or_default();
    let task = running_task(job, &job_id);
    let Some(task) = task else {
        return control_error(
            job_id,
            request.request_id,
            ERROR_TASK_NOT_ACTIVE,
            "download task is not active",
        );
    };
    let result = match request.command.as_str() {
        "pause" => task.control.pause().map(|()| "paused"),
        "resume" => task.control.resume().map(|()| "downloading"),
        "cancel" => task.control.cancel().map(|()| "cancelling"),
        _ => Err("unsupported task control".to_string()),
    };
    match result {
        Ok(state) => NativeResponse {
            event_type: EVENT_ACK,
            ok: true,
            job_id: Some(job_id),
            state: Some(state.to_string()),
            request_id: request.request_id,
            ..NativeResponse::default()
        },
        Err(error) => control_error(job_id, request.request_id, ERROR_CONTROL_FAILED, &error),
    }
}

fn update_hls_info(request: NativeRequest, job: &ActiveJob) -> NativeResponse {
    let job_id = request.job_id.unwrap_or_default();
    let task = running_task(job, &job_id);
    let Some(task) = task else {
        return control_error(
            job_id,
            request.request_id,
            ERROR_TASK_NOT_ACTIVE,
            "download task is not active",
        );
    };
    let info = match (request.total_segments, request.total_duration_ms) {
        (Some(total_segments), Some(total_duration_ms)) if total_segments > 0 => HlsInfo {
            total_segments,
            total_duration_ms,
        },
        _ => {
            return control_error(
                job_id,
                request.request_id,
                ERROR_INVALID_HLS_INFO,
                "invalid HLS segment information",
            )
        }
    };
    if let Ok(mut current) = task.hls_info.lock() {
        *current = Some(info);
    } else {
        return control_error(
            job_id,
            request.request_id,
            ERROR_CONTROL_FAILED,
            "HLS progress state is unavailable",
        );
    }
    let progress = task.progress.lock().ok().and_then(|progress| *progress);
    progress_response(
        &job_id,
        Some(info),
        progress,
        if task.control.is_paused() {
            "paused"
        } else {
            "downloading"
        },
        request.request_id,
    )
}

/// A control command the host understood but could not apply. Like `rejected`,
/// `control-error` is not a job state and never terminates a job.
fn control_error(
    job_id: String,
    request_id: Option<String>,
    error_code: &'static str,
    error: &str,
) -> NativeResponse {
    NativeResponse {
        event_type: EVENT_CONTROL_ERROR,
        ok: false,
        error: Some(error.to_string()),
        error_code: Some(error_code),
        job_id: Some(job_id),
        state: Some(STATE_CONTROL_ERROR.to_string()),
        request_id,
        ..NativeResponse::default()
    }
}

fn cancelled_response(job_id: &str, task: &ActiveTask) -> NativeResponse {
    NativeResponse {
        event_type: EVENT_TERMINAL,
        ok: false,
        // Where the file was, whether or not it is still there: with
        // `keep_partial` off it has just been deleted, and naming it is still
        // the honest answer to "what happened to my download".
        path: target_path(task),
        error: Some("download cancelled".to_string()),
        error_code: Some(ERROR_CANCELLED),
        job_id: Some(job_id.to_string()),
        state: Some("cancelled".to_string()),
        ..NativeResponse::default()
    }
}

/// Stop the running job because the port closed — Firefox quitting, the
/// extension reloading, a crash. Not a cancel anyone asked for, so the
/// part-written file is kept whatever `keep_partial` says: the extension
/// reconciles such a job to `interrupted` on its next start and tells the user
/// the file is still there.
fn cancel_for_shutdown(job: &ActiveJob) {
    if let Ok(active) = job.lock() {
        if let Some((_, task)) = active.as_ref() {
            let _ = task.control.cancel_for_shutdown();
        }
    }
}

/// The running task, if it is the one named. A control command for any other
/// job is answered `task_not_active`, exactly as before: this host has one job
/// and does not know about anyone else's.
fn running_task(job: &ActiveJob, job_id: &str) -> Option<ActiveTask> {
    job.lock().ok().and_then(|active| {
        active
            .as_ref()
            .filter(|(id, _)| id == job_id)
            .map(|(_, task)| task.clone())
    })
}

/// Release the slot once a job has ended, leaving the process idle until the
/// port closes. Named rather than inlined because it happens on every terminal
/// path and each one must do it.
fn clear_job(job: &ActiveJob, job_id: &str) {
    if let Ok(mut active) = job.lock() {
        if active.as_ref().is_some_and(|(id, _)| id == job_id) {
            *active = None;
        }
    }
}

fn send_response<T: Serialize>(output: &SharedOutput, response: &T) -> io::Result<()> {
    let mut output = output
        .lock()
        .map_err(|_| io::Error::other("native output is unavailable"))?;
    write_message(&mut *output, response)
}

fn next_job_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let sequence = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    format!("native-{timestamp}-{sequence}")
}

fn download(
    request: NativeRequest,
    task: &ActiveTask,
    output: SharedOutput,
    job_id: &str,
) -> DownerResult<PathBuf> {
    let url = crate::output::validate_url(&request.url)?;
    let referer = request
        .source_url
        .as_deref()
        .map(crate::output::validate_url)
        .transpose()?;
    let user_agent = request
        .user_agent
        .unwrap_or_else(|| "Mozilla/5.0 (Firefox; downer native host)".to_string());
    let media = ResolvedMedia {
        url,
        referer,
        user_agent: user_agent.clone(),
    };
    // Refused rather than guessed at. The old fallback was `.` — the host
    // process's working directory, inherited from however Firefox was started —
    // so a download could land somewhere the user never chose. See KEI-90.
    let dir = request
        .output_dir
        .or_else(crate::output::default_download_dir)
        .ok_or_else(|| DownerError::OutputPath(crate::output::NO_DEFAULT_DIRECTORY.to_string()))?;
    let options = DownloadOptions {
        output: None,
        dir: Some(dir),
        overwrite: request.overwrite,
        on_conflict: request.on_conflict,
        naming: NamingHints::new(request.title),
        ffmpeg: request.ffmpeg.clone().unwrap_or_else(ffmpeg_path),
        user_agent,
        cookie: request.cookie,
        threads: request.threads,
        quiet: true,
        playlist_text: request.playlist_text.clone(),
        keep_partial: request.keep_partial,
    };
    let info = task.hls_info.lock().ok().and_then(|info| *info);
    let state = if task.control.is_paused() {
        "paused"
    } else {
        "downloading"
    };
    let _ = send_response(&output, &progress_response(job_id, info, None, state, None));
    let progress_output = output.clone();
    let progress_job_id = job_id.to_string();
    let progress_task = task.clone();
    let progress = move |value: FfmpegProgress| {
        if let Ok(mut current) = progress_task.progress.lock() {
            *current = Some(value);
        }
        let info = progress_task.hls_info.lock().ok().and_then(|info| *info);
        let _ = send_response(
            &progress_output,
            &progress_response(
                &progress_job_id,
                info,
                Some(value),
                if progress_task.control.is_paused() {
                    "paused"
                } else {
                    "downloading"
                },
                None,
            ),
        );
    };
    let log_output = output.clone();
    let log_job_id = job_id.to_string();
    let log = move |line: String| {
        let _ = send_response(
            &log_output,
            &NativeLogResponse {
                protocol_version: PROTOCOL_VERSION,
                event_type: EVENT_LOG,
                ok: true,
                job_id: log_job_id.clone(),
                state: "downloading".to_string(),
                // Redacted here, on the host, so a signed segment URL never
                // crosses the native messaging port. The extension redacts
                // again before it persists or displays the line; see
                // docs/adr/0003-redact-urls-in-logs.md for why both.
                log: crate::redact::redact_text(&line),
            },
        );
    };
    // The job error names an old FFmpeg on its own, but a download that still
    // succeeds would say nothing at all, so the warning also goes out as a log
    // line — the channel the Settings console already shows and persists.
    if let Some(version) = crate::unsupported_ffmpeg(&options.ffmpeg) {
        log(crate::outdated_ffmpeg_warning(version));
    }
    let target = task.target.clone();
    crate::download_resolved(
        media,
        &options,
        crate::Hooks::controlled(&task.control, progress, log).reporting_target(
            move |path: &Path| {
                if let Ok(mut current) = target.lock() {
                    *current = Some(path.to_path_buf());
                }
            },
        ),
    )
}

/// The path this job resolved to, if it got that far.
fn target_path(task: &ActiveTask) -> Option<PathBuf> {
    task.target.lock().ok().and_then(|path| path.clone())
}

fn progress_response(
    job_id: &str,
    info: Option<HlsInfo>,
    progress: Option<FfmpegProgress>,
    state: &str,
    request_id: Option<String>,
) -> NativeResponse {
    let (completed_segments, total_segments, percent) = match (info, progress) {
        (Some(info), Some(progress)) if progress.finished => (
            Some(info.total_segments),
            Some(info.total_segments),
            Some(100.0),
        ),
        (Some(info), Some(progress)) => {
            let completed = progress
                .out_time_ms
                .filter(|_| info.total_duration_ms > 0)
                .map(|elapsed| {
                    (elapsed.saturating_mul(info.total_segments) / info.total_duration_ms)
                        .min(info.total_segments)
                })
                .unwrap_or(0);
            let percent = if info.total_segments == 0 {
                0.0
            } else {
                completed as f64 * 100.0 / info.total_segments as f64
            };
            (Some(completed), Some(info.total_segments), Some(percent))
        }
        (Some(info), None) => (Some(0), Some(info.total_segments), Some(0.0)),
        // No playlist totals. Segment counts and a percentage are genuinely
        // unknowable, but elapsed output is not, and it is carried below.
        (None, _) => (None, None, None),
    };
    NativeResponse {
        event_type: EVENT_PROGRESS,
        ok: true,
        job_id: Some(job_id.to_string()),
        state: Some(state.to_string()),
        completed_segments,
        total_segments,
        percent,
        elapsed_ms: progress.and_then(|progress| progress.out_time_ms),
        request_id,
        ..NativeResponse::default()
    }
}

/// Which FFmpeg this host runs.
///
/// `DOWNER_FFMPEG` still wins, because it is the override every test and every
/// script already uses. Then the path recorded by `downer install-host
/// --ffmpeg`: Firefox launches the host with a minimal environment, so a user
/// whose FFmpeg is somewhere unusual has no environment variable to set and
/// needs the installer to have written it down (see
/// `docs/adr/0008-relocatable-native-host-installation.md`). Only then the
/// Homebrew locations and a bare `ffmpeg` resolved against whatever PATH the
/// browser happened to pass down.
pub fn ffmpeg_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DOWNER_FFMPEG") {
        return PathBuf::from(path);
    }
    if let Some(path) = crate::host::configured_ffmpeg() {
        return path;
    }
    for path in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("ffmpeg")
}

fn read_message(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0_u8; 4];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_le_bytes(length);
    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native message exceeds 1 MiB",
        ));
    }
    let mut payload = vec![0_u8; length as usize];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn write_message<T: Serialize>(output: &mut impl Write, response: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "native response is too large"))?;
    output.write_all(&length.to_le_bytes())?;
    output.write_all(&payload)?;
    output.flush()
}
