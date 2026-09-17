use std::{
    collections::HashMap,
    io::{self, Read, Write},
    path::PathBuf,
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
const ERROR_TASK_NOT_ACTIVE: &str = "task_not_active";
const ERROR_INVALID_HLS_INFO: &str = "invalid_hls_info";
const ERROR_DOWNLOAD_FAILED: &str = "download_failed";
const ERROR_CONTROL_FAILED: &str = "control_failed";
const ERROR_CANCELLED: &str = "cancelled";

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
type SharedOutput = Arc<Mutex<io::Stdout>>;
type ActiveTasks = Arc<Mutex<HashMap<String, ActiveTask>>>;

#[derive(Clone, Debug)]
struct ActiveTask {
    control: ProcessControl,
    hls_info: Arc<Mutex<Option<HlsInfo>>>,
    progress: Arc<Mutex<Option<FfmpegProgress>>>,
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
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_segments: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_segments: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<f64>,
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
            job_id: None,
            state: None,
            completed_segments: None,
            total_segments: None,
            percent: None,
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
    let tasks = Arc::new(Mutex::new(HashMap::new()));

    loop {
        let Some(payload) = read_message(&mut input).map_err(DownerError::NativeIo)? else {
            cancel_all(&tasks);
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
            start_download(request, output.clone(), tasks.clone())
                .map_err(DownerError::NativeIo)?;
            continue;
        }
        let response = match request.command.as_str() {
            "hello" => hello_response(request.request_id),
            "pause" | "resume" | "cancel" => control_download(request, &tasks),
            "hls-info" => update_hls_info(request, &tasks),
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

fn start_download(
    request: NativeRequest,
    output: SharedOutput,
    tasks: ActiveTasks,
) -> io::Result<()> {
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
    };
    let mut active = tasks
        .lock()
        .map_err(|_| io::Error::other("download task registry is unavailable"))?;
    if active.contains_key(&job_id) {
        // Rejected rather than failed: the job named here is already running and
        // must not be terminated by a duplicate start.
        return send_response(
            &output,
            &rejected(
                ERROR_DUPLICATE_JOB,
                format!("download task already exists: {job_id}"),
                request.request_id,
                Some(job_id),
            ),
        );
    }
    active.insert(job_id.clone(), task.clone());
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
        if let Ok(mut active) = tasks.lock() {
            active.remove(&job_id);
        }
        return Err(error);
    }

    start_hls_preflight(&request, &job_id, &task, output.clone(), tasks.clone());

    let worker_job_id = job_id.clone();
    let worker_task = task.clone();
    thread::spawn(move || {
        let response = if worker_task.control.is_cancelled() {
            cancelled_response(&worker_job_id)
        } else {
            match download(request, &worker_task, output.clone(), &worker_job_id) {
                Ok(_path) if worker_task.control.is_cancelled() => {
                    cancelled_response(&worker_job_id)
                }
                Ok(path) => completed_response(&worker_job_id, path, &worker_task),
                Err(_error) if worker_task.control.is_cancelled() => {
                    cancelled_response(&worker_job_id)
                }
                Err(error) => NativeResponse {
                    event_type: EVENT_TERMINAL,
                    ok: false,
                    // `DownerError::FfmpegFailed` embeds FFmpeg's stderr tail,
                    // which is exactly the text the `log` events are redacted
                    // for. The terminal error takes the same treatment, or a
                    // token would simply move from the log to the failure
                    // message the popup shows and persists.
                    error: Some(crate::redact::redact_text(&error.to_string())),
                    error_code: Some(ERROR_DOWNLOAD_FAILED),
                    job_id: Some(worker_job_id.clone()),
                    state: Some("failed".to_string()),
                    ..NativeResponse::default()
                },
            }
        };
        let _ = send_response(&output, &response);
        if let Ok(mut active) = tasks.lock() {
            active.remove(&worker_job_id);
        }
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
    tasks: ActiveTasks,
) {
    if task.hls_info.lock().ok().and_then(|info| *info).is_some()
        || !request.url.to_ascii_lowercase().contains(".m3u8")
    {
        return;
    }
    let Ok(url) = crate::output::validate_url(&request.url) else {
        return;
    };
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
        let Some(info) = info else {
            return;
        };
        let still_active = tasks
            .lock()
            .ok()
            .is_some_and(|active| active.contains_key(&job_id));
        if !still_active {
            return;
        }
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
            &output,
            &progress_response(&job_id, Some(info), progress, state, None),
        );
    });
}

fn control_download(request: NativeRequest, tasks: &ActiveTasks) -> NativeResponse {
    let job_id = request.job_id.unwrap_or_default();
    let task = tasks
        .lock()
        .ok()
        .and_then(|active| active.get(&job_id).cloned());
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

fn update_hls_info(request: NativeRequest, tasks: &ActiveTasks) -> NativeResponse {
    let job_id = request.job_id.unwrap_or_default();
    let task = tasks
        .lock()
        .ok()
        .and_then(|active| active.get(&job_id).cloned());
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

fn cancelled_response(job_id: &str) -> NativeResponse {
    NativeResponse {
        event_type: EVENT_TERMINAL,
        ok: false,
        error: Some("download cancelled".to_string()),
        error_code: Some(ERROR_CANCELLED),
        job_id: Some(job_id.to_string()),
        state: Some("cancelled".to_string()),
        ..NativeResponse::default()
    }
}

fn cancel_all(tasks: &ActiveTasks) {
    if let Ok(active) = tasks.lock() {
        for task in active.values() {
            let _ = task.control.cancel();
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
    let source_host = referer
        .as_ref()
        .unwrap_or(&url)
        .host_str()
        .map(str::to_string);
    let media = ResolvedMedia {
        url,
        referer,
        user_agent: user_agent.clone(),
    };
    let options = DownloadOptions {
        output: None,
        dir: Some(
            request
                .output_dir
                .or_else(dirs::download_dir)
                .unwrap_or_else(|| PathBuf::from(".")),
        ),
        overwrite: request.overwrite,
        on_conflict: request.on_conflict,
        naming: NamingHints::new(request.title, source_host),
        ffmpeg: ffmpeg_path(),
        user_agent,
        cookie: request.cookie,
        threads: request.threads,
        quiet: true,
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
    crate::download_resolved_controlled_with_progress_and_logs(
        media,
        &options,
        &task.control,
        progress,
        log,
    )
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
        request_id,
        ..NativeResponse::default()
    }
}

fn ffmpeg_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DOWNER_FFMPEG") {
        return PathBuf::from(path);
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
