//! Black-box tests for the Firefox native messaging host (`downer --native-host`).
//!
//! Each test starts the real binary, speaks the length-prefixed JSON protocol over
//! stdio, and points the host at a fake FFmpeg through `DOWNER_FFMPEG`, so the
//! observable protocol (field names, state strings, event order) is pinned without
//! touching the network or a real FFmpeg.
//!
//! The contract these tests pin is written down in `docs/protocol.md` and decided
//! in `docs/adr/0001-native-messaging-protocol.md`. The wire vocabulary lives in
//! `tests/fixtures/protocol.json`, which the extension's Node tests read too, so
//! neither implementation can rename a term without the other noticing. A failing
//! test here is a protocol decision to make deliberately, not a test to adjust.

#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

/// The shared wire vocabulary, read by this suite and by
/// `tests/extension/task-protocol.test.js`.
const PROTOCOL_FIXTURE: &str = include_str!("fixtures/protocol.json");

fn protocol() -> Value {
    serde_json::from_str(PROTOCOL_FIXTURE).expect("tests/fixtures/protocol.json is valid JSON")
}

fn protocol_strings(pointer: &str) -> Vec<String> {
    protocol()
        .pointer(pointer)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{pointer} is an array in tests/fixtures/protocol.json"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("protocol vocabulary entries are strings")
                .to_string()
        })
        .collect()
}

/// Every host response carries the protocol version and an explicit event type.
fn assert_envelope(event: &Value, expected_type: &str) {
    assert_eq!(
        event["protocol_version"],
        protocol()["protocol_version"],
        "{event}"
    );
    assert_eq!(event["type"], json!(expected_type), "{event}");
    assert!(
        protocol_strings("/event_types").contains(&expected_type.to_string()),
        "{expected_type} is listed in tests/fixtures/protocol.json"
    );
}

/// A running native host plus the framing helpers used to talk to it.
struct NativeHost {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Value>,
}

impl NativeHost {
    fn start(ffmpeg: &Path) -> Self {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("downer"))
            .arg("--native-host")
            .env("DOWNER_FFMPEG", ffmpeg)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("native host starts");
        let stdin = child.stdin.take().expect("native host stdin is piped");
        let mut stdout = child.stdout.take().expect("native host stdout is piped");
        let (sender, events) = mpsc::channel();
        thread::spawn(move || {
            while let Some(payload) = read_message(&mut stdout) {
                let message =
                    serde_json::from_slice::<Value>(&payload).expect("host writes valid JSON");
                if sender.send(message).is_err() {
                    return;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            events,
        }
    }

    fn send(&mut self, request: &Value) {
        let payload = serde_json::to_vec(request).expect("request serializes");
        let stdin = self.stdin.as_mut().expect("stdin is still open");
        write_frame(stdin, &payload);
    }

    /// Send a raw frame, so malformed and oversized messages can be exercised.
    fn send_raw(&mut self, length: u32, payload: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin is still open");
        stdin
            .write_all(&length.to_le_bytes())
            .expect("length write");
        stdin.write_all(payload).expect("payload write");
        stdin.flush().expect("frame flush");
    }

    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn next_event(&self) -> Value {
        match self.events.recv_timeout(EVENT_TIMEOUT) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for a native host event"),
            Err(RecvTimeoutError::Disconnected) => panic!("native host closed stdout unexpectedly"),
        }
    }

    /// Consume events until one matches, returning it together with everything skipped.
    fn wait_for(&self, matches: impl Fn(&Value) -> bool) -> (Value, Vec<Value>) {
        let mut skipped = Vec::new();
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            let event = self.next_event();
            if matches(&event) {
                return (event, skipped);
            }
            skipped.push(event);
        }
        panic!("no matching native host event arrived; saw {skipped:#?}");
    }

    fn wait_for_state(&self, state: &str) -> (Value, Vec<Value>) {
        self.wait_for(|event| event["state"] == state)
    }

    /// Assert no terminal event arrives within the window (used by the pause test).
    fn expect_no_terminal_event(&self, window: Duration) {
        let deadline = Instant::now() + window;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(remaining) {
                Ok(event) => assert!(
                    !is_terminal(&event),
                    "unexpected terminal event while paused: {event}"
                ),
                Err(RecvTimeoutError::Timeout) => return,
                Err(RecvTimeoutError::Disconnected) => panic!("native host exited while paused"),
            }
        }
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("native host status") {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "native host did not exit within {timeout:?}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_terminal(event: &Value) -> bool {
    matches!(
        event["state"].as_str(),
        Some("completed") | Some("failed") | Some("cancelled")
    )
}

fn write_frame(output: &mut impl Write, payload: &[u8]) {
    let length = u32::try_from(payload.len()).expect("payload fits a native frame");
    output
        .write_all(&length.to_le_bytes())
        .expect("length write");
    output.write_all(payload).expect("payload write");
    output.flush().expect("frame flush");
}

fn read_message(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut length = [0_u8; 4];
    if input.read_exact(&mut length).is_err() {
        return None;
    }
    let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
    input.read_exact(&mut payload).ok()?;
    Some(payload)
}

/// Knobs for the generated fake FFmpeg.
struct FakeFfmpeg {
    /// Number of `progress=continue` updates emitted before finishing.
    progress_updates: u32,
    /// Seconds slept after each update, so a run can be paused or cancelled.
    sleep_seconds: f32,
    /// Bytes written to the output path (the last argv element).
    content: &'static str,
    /// Line written to stderr, surfaced by the host as a log event and error text.
    stderr_line: Option<&'static str>,
    exit_code: i32,
}

impl Default for FakeFfmpeg {
    fn default() -> Self {
        Self {
            progress_updates: 2,
            sleep_seconds: 0.05,
            content: "fake media",
            stderr_line: None,
            exit_code: 0,
        }
    }
}

impl FakeFfmpeg {
    /// Write the fake into `directory` and return its path. The fake records argv in
    /// `<directory>/args`, NUL-separated so header arguments containing CRLF survive
    /// intact, and touches `<directory>/finished` only if it runs to the end.
    fn install(&self, directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let stderr_line = match self.stderr_line {
            Some(line) => format!("echo '{line}' >&2\n"),
            None => String::new(),
        };
        let script = format!(
            r#"#!/bin/sh
dir="$(dirname "$0")"
last=""
for arg in "$@"; do last="$arg"; done
printf '%s\0' "$@" > "$dir/args"
mkdir -p "$(dirname "$last")"
printf '%s' '{content}' > "$last"
i=0
while [ "$i" -lt {updates} ]; do
  i=$((i + 1))
  echo "out_time_us=$((i * 1000000))"
  echo "progress=continue"
  sleep {sleep}
done
echo "progress=end"
{stderr_line}: > "$dir/finished"
exit {exit_code}
"#,
            content = self.content,
            updates = self.progress_updates,
            sleep = self.sleep_seconds,
            stderr_line = stderr_line,
            exit_code = self.exit_code,
        );
        let path = directory.join("fake-ffmpeg");
        fs::write(&path, script).expect("fake FFmpeg is written");
        let mut permissions = fs::metadata(&path).expect("fake metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("fake is executable");
        path
    }
}

fn download_request(url: &str, output_dir: &Path) -> Value {
    json!({
        "command": "download",
        "protocol_version": 1,
        "url": url,
        "source_url": "https://example.test/watch/123",
        "output_dir": output_dir,
        "overwrite": false,
        "cookie": "session=secret-value",
        "user_agent": "Mozilla/5.0 (native host test)",
    })
}

fn recorded_args(directory: &Path) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(recorded) = fs::read(directory.join("args")) {
            return String::from_utf8(recorded)
                .expect("arguments are UTF-8")
                .split('\0')
                .filter(|argument| !argument.is_empty())
                .map(str::to_string)
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "fake FFmpeg never recorded its arguments"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn successful_download_reports_starting_progress_and_completed() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-success");
    host.send(&request);

    let starting = host.next_event();
    assert_envelope(&starting, "progress");
    assert_eq!(starting["ok"], json!(true));
    assert_eq!(starting["job_id"], json!("job-success"));
    assert_eq!(starting["state"], json!("starting"));

    let (downloading, _) = host.wait_for_state("downloading");
    assert_envelope(&downloading, "progress");
    assert_eq!(downloading["ok"], json!(true));
    assert_eq!(downloading["job_id"], json!("job-success"));

    let (completed, _) = host.wait_for_state("completed");
    assert_envelope(&completed, "terminal");
    assert_eq!(completed["ok"], json!(true));
    assert_eq!(completed["job_id"], json!("job-success"));
    let path = PathBuf::from(
        completed["path"]
            .as_str()
            .expect("completed carries a path"),
    );
    assert_eq!(path, output_dir.join("video.mp4"));
    assert_eq!(fs::read_to_string(path).unwrap(), "fake media");
}

#[test]
fn download_forwards_referer_user_agent_and_cookie_headers_once() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let output_dir = temp.path().join("downloads");
    let url = "https://example.test/video%20name.mp4?arg=$HOME;echo no";
    let mut host = NativeHost::start(&ffmpeg);
    host.send(&download_request(url, &output_dir));
    host.wait_for_state("completed");

    let args = recorded_args(temp.path());
    let headers_positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.as_str() == "-headers")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        headers_positions.len(),
        1,
        "one -headers argument: {args:?}"
    );
    let headers = &args[headers_positions[0] + 1];
    assert!(headers.contains("User-Agent: Mozilla/5.0 (native host test)"));
    assert!(headers.contains("Referer: https://example.test/watch/123"));
    assert!(headers.contains("Cookie: session=secret-value"));

    // The URL reaches FFmpeg as exactly one argv element, never through a shell.
    let canonical = url::Url::parse(url).unwrap().to_string();
    assert_eq!(
        args.iter()
            .filter(|argument| *argument == &canonical)
            .count(),
        1,
        "URL is a single argv element: {args:?}"
    );
    assert!(!args.iter().any(|argument| argument == "no"));
}

#[test]
fn failing_ffmpeg_reports_failed_state_and_keeps_the_partial_file() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        content: "partial media",
        stderr_line: Some("network failure"),
        exit_code: 17,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-failure");
    host.send(&request);

    let (failed, skipped) = host.wait_for_state("failed");
    assert_envelope(&failed, "terminal");
    assert_eq!(failed["ok"], json!(false));
    assert_eq!(failed["error_code"], json!("download_failed"));
    assert_eq!(failed["job_id"], json!("job-failure"));
    let error = failed["error"].as_str().expect("failure carries an error");
    assert!(error.contains("network failure"), "error text: {error}");

    // FFmpeg stderr is streamed as log events on the same channel, tagged with an
    // explicit type rather than recognised by the presence of a `log` field.
    let log_event = skipped
        .iter()
        .find(|event| {
            event["log"]
                .as_str()
                .is_some_and(|line| line.contains("network failure"))
        })
        .unwrap_or_else(|| panic!("expected a log event, saw {skipped:#?}"));
    assert_envelope(log_event, "log");
    assert_eq!(
        fs::read_to_string(output_dir.join("video.mp4")).unwrap(),
        "partial media"
    );
}

#[test]
fn cancel_acknowledges_then_reports_the_cancelled_terminal_state() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-cancel");
    host.send(&request);
    host.wait_for_state("downloading");

    host.send(&json!({
        "command": "cancel",
        "job_id": "job-cancel",
        "request_id": "job-cancel-1",
    }));
    let (ack, _) = host.wait_for_state("cancelling");
    assert_envelope(&ack, "ack");
    assert_eq!(ack["ok"], json!(true));
    assert_eq!(ack["request_id"], json!("job-cancel-1"));

    let (cancelled, _) = host.wait_for_state("cancelled");
    assert_envelope(&cancelled, "terminal");
    assert_eq!(cancelled["error_code"], json!("cancelled"));
    assert_eq!(cancelled["ok"], json!(false));
    assert_eq!(cancelled["job_id"], json!("job-cancel"));
    assert_eq!(cancelled["error"], json!("download cancelled"));
    assert!(
        !temp.path().join("finished").exists(),
        "cancel must kill FFmpeg before it finishes"
    );
}

#[test]
fn pause_stops_progress_and_resume_lets_the_download_complete() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 3,
        sleep_seconds: 0.5,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-pause");
    host.send(&request);
    host.wait_for(|event| event["state"] == "downloading" && event["ok"] == json!(true));

    host.send(&json!({
        "command": "pause",
        "job_id": "job-pause",
        "request_id": "job-pause-1",
    }));
    let (paused, _) = host.wait_for_state("paused");
    assert_eq!(paused["ok"], json!(true));
    assert_eq!(paused["request_id"], json!("job-pause-1"));
    host.expect_no_terminal_event(Duration::from_millis(1_200));

    host.send(&json!({
        "command": "resume",
        "job_id": "job-pause",
        "request_id": "job-pause-2",
    }));
    let (resumed, _) = host.wait_for(|event| {
        event["state"] == "downloading" && event["request_id"] == json!("job-pause-2")
    });
    assert_eq!(resumed["ok"], json!(true));

    let (completed, _) = host.wait_for_state("completed");
    assert_eq!(completed["ok"], json!(true));
}

#[test]
fn download_with_playlist_totals_reports_segment_progress() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-segments");
    request["total_segments"] = json!(10);
    request["total_duration_ms"] = json!(10_000);
    host.send(&request);

    let starting = host.next_event();
    assert_eq!(starting["state"], json!("starting"));
    assert_eq!(starting["total_segments"], json!(10));
    assert_eq!(starting["percent"], json!(0.0));

    // The fake reports one second of output against a ten-second, ten-segment playlist.
    let (progress, _) = host.wait_for(|event| {
        event["state"] == "downloading" && event["completed_segments"] == json!(1)
    });
    assert_eq!(progress["total_segments"], json!(10));
    assert_eq!(progress["percent"], json!(10.0));
}

#[test]
fn hls_info_updates_totals_for_an_active_download() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-hls");
    host.send(&request);
    // Wait for the single progress update (one second of output) before sending totals.
    host.wait_for(|event| event["state"] == "downloading" && event["percent"].is_null());

    host.send(&json!({
        "command": "hls-info",
        "job_id": "job-hls",
        "request_id": "job-hls-1",
        "total_segments": 4,
        "total_duration_ms": 8_000,
    }));
    let (updated, _) = host.wait_for(|event| event["request_id"] == json!("job-hls-1"));
    assert_eq!(updated["ok"], json!(true));
    assert_eq!(updated["state"], json!("downloading"));
    assert_eq!(updated["total_segments"], json!(4));
    assert_eq!(updated["completed_segments"], json!(0));
    assert_eq!(updated["percent"], json!(0.0));
}

#[test]
fn hls_info_without_segment_totals_is_a_control_error() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-bad-hls");
    host.send(&request);
    host.wait_for_state("downloading");

    host.send(&json!({
        "command": "hls-info",
        "job_id": "job-bad-hls",
        "request_id": "job-bad-hls-1",
        "total_segments": 0,
        "total_duration_ms": 0,
    }));
    let (error, _) = host.wait_for_state("control-error");
    assert_envelope(&error, "control-error");
    assert_eq!(error["ok"], json!(false));
    assert_eq!(error["error_code"], json!("invalid_hls_info"));
    assert_eq!(error["request_id"], json!("job-bad-hls-1"));
    assert_eq!(error["error"], json!("invalid HLS segment information"));
}

#[test]
fn control_commands_for_an_unknown_job_report_control_error() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    for command in ["pause", "resume", "cancel", "hls-info"] {
        host.send(&json!({
            "command": command,
            "job_id": "missing-job",
            "request_id": format!("{command}-1"),
            "total_segments": 3,
            "total_duration_ms": 3_000,
        }));
        let event = host.next_event();
        assert_envelope(&event, "control-error");
        assert_eq!(event["ok"], json!(false), "{command}: {event}");
        assert_eq!(event["state"], json!("control-error"), "{command}: {event}");
        assert_eq!(
            event["error_code"],
            json!("task_not_active"),
            "{command}: {event}"
        );
        assert_eq!(event["job_id"], json!("missing-job"), "{command}: {event}");
        assert_eq!(
            event["request_id"],
            json!(format!("{command}-1")),
            "{command}: {event}"
        );
        assert_eq!(
            event["error"],
            json!("download task is not active"),
            "{command}: {event}"
        );
    }
}

#[test]
fn duplicate_job_ids_are_rejected_without_terminating_the_running_download() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-duplicate");
    host.send(&request);
    host.wait_for_state("downloading");

    request["request_id"] = json!("job-duplicate-2");
    host.send(&request);
    let (rejected, _) = host.wait_for_state("rejected");
    assert_envelope(&rejected, "rejected");
    assert_eq!(rejected["ok"], json!(false));
    assert_eq!(rejected["error_code"], json!("duplicate_job"));
    // The duplicate names the *running* job. Before the protocol was versioned
    // this was reported as `failed`, which terminated the client's view of a
    // download that was still running; `rejected` is never terminal.
    assert_eq!(rejected["job_id"], json!("job-duplicate"));
    assert_eq!(rejected["request_id"], json!("job-duplicate-2"));
    assert_eq!(
        rejected["error"],
        json!("download task already exists: job-duplicate")
    );

    // The original download is untouched and still cancellable.
    host.send(&json!({
        "command": "cancel",
        "protocol_version": 1,
        "job_id": "job-duplicate",
        "request_id": "job-duplicate-cancel",
    }));
    let (ack, _) = host.wait_for(|event| event["request_id"] == json!("job-duplicate-cancel"));
    assert_eq!(ack["ok"], json!(true), "{ack}");
    assert_eq!(ack["state"], json!("cancelling"), "{ack}");
}

#[test]
fn malformed_json_is_rejected_without_a_job_id() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    let payload = b"{not json";
    host.send_raw(payload.len() as u32, payload);
    let event = host.next_event();
    assert_envelope(&event, "rejected");
    assert_eq!(event["ok"], json!(false));
    assert_eq!(event["state"], json!("rejected"));
    assert_eq!(event["error_code"], json!("invalid_request"));
    assert!(event["job_id"].is_null(), "{event}");
    assert!(
        event["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("invalid native request")),
        "{event}"
    );

    // The host keeps serving after a bad message.
    host.send(&json!({
        "command": "pause",
        "protocol_version": 1,
        "job_id": "missing",
        "request_id": "after-bad-json",
    }));
    let event = host.next_event();
    assert_eq!(event["request_id"], json!("after-bad-json"));
}

#[test]
fn hello_reports_the_protocol_version_host_version_and_capabilities() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    host.send(&json!({
        "command": "hello",
        "protocol_version": 1,
        "request_id": "hello-1",
    }));
    let event = host.next_event();
    assert_envelope(&event, "hello");
    assert_eq!(event["ok"], json!(true));
    assert_eq!(event["state"], json!("ready"));
    assert_eq!(event["request_id"], json!("hello-1"));
    assert_eq!(event["host_version"], json!(env!("CARGO_PKG_VERSION")));
    // Pause and resume use Unix process signals; this suite is Unix-only.
    assert_eq!(event["capabilities"]["pause_resume"], json!(true));
    assert_eq!(event["capabilities"]["hls_info"], json!(true));
    for capability in protocol_strings("/capabilities") {
        assert!(
            event["capabilities"][&capability].is_boolean(),
            "capability {capability} is reported: {event}"
        );
    }
}

#[test]
fn a_request_without_a_protocol_version_is_still_served_as_legacy() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    // An extension built before the handshake omits the field entirely. It keeps
    // working for one release cycle; see docs/protocol.md, "Versioning".
    host.send(&json!({"command": "hello", "request_id": "legacy-1"}));
    let event = host.next_event();
    assert_envelope(&event, "hello");
    assert_eq!(event["ok"], json!(true));
    assert_eq!(event["request_id"], json!("legacy-1"));
}

#[test]
fn a_request_with_an_unsupported_protocol_version_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    host.send(&json!({
        "command": "hello",
        "protocol_version": 99,
        "request_id": "future-1",
    }));
    let event = host.next_event();
    assert_envelope(&event, "rejected");
    assert_eq!(event["ok"], json!(false));
    assert_eq!(event["state"], json!("rejected"));
    assert_eq!(event["error_code"], json!("unsupported_protocol_version"));
    assert_eq!(event["request_id"], json!("future-1"));
    assert!(
        event["error"]
            .as_str()
            .is_some_and(|error| error.contains("99")),
        "the mismatch names the version it refused: {event}"
    );
}

#[test]
fn unsupported_commands_are_rejected_without_terminating_an_active_job() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-unsupported");
    host.send(&request);
    host.wait_for_state("downloading");

    host.send(&json!({
        "command": "ping",
        "protocol_version": 1,
        "request_id": "ping-1",
    }));
    let event = host.next_event();
    assert_envelope(&event, "rejected");
    assert_eq!(event["ok"], json!(false));
    assert_eq!(event["state"], json!("rejected"));
    assert_eq!(event["error_code"], json!("unsupported_command"));
    assert_eq!(event["error"], json!("unsupported native command: ping"));
    assert_eq!(event["request_id"], json!("ping-1"));
    // `rejected` is a connection state, not a job state: it carries no job_id and,
    // crucially, is not terminal. Before ADR-0001 this was `failed`, which the JS
    // NativeTaskChannel treated as terminal for the job owning the port.
    assert!(event["job_id"].is_null(), "{event}");
    assert!(
        !protocol_strings("/job_states/terminal").contains(&"rejected".to_string()),
        "rejected is not a terminal job state in tests/fixtures/protocol.json"
    );

    // The download the rejection did not belong to is still running and controllable.
    host.send(&json!({
        "command": "cancel",
        "protocol_version": 1,
        "job_id": "job-unsupported",
        "request_id": "job-unsupported-cancel",
    }));
    let (ack, _) = host.wait_for(|event| event["request_id"] == json!("job-unsupported-cancel"));
    assert_envelope(&ack, "ack");
    assert_eq!(ack["ok"], json!(true), "{ack}");
    let (cancelled, _) = host.wait_for_state("cancelled");
    assert_eq!(cancelled["job_id"], json!("job-unsupported"));
}

#[test]
fn the_shared_protocol_vocabulary_matches_what_the_host_emits() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let event_types = protocol_strings("/event_types");
    let mut states = protocol_strings("/job_states/active");
    states.extend(protocol_strings("/job_states/terminal"));
    states.extend(protocol_strings("/connection_states"));

    host.send(&json!({"command": "hello", "protocol_version": 1}));
    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-vocabulary");
    host.send(&request);

    let mut seen = Vec::new();
    let (terminal, mut skipped) = host.wait_for(is_terminal);
    skipped.push(terminal);
    for event in &skipped {
        let event_type = event["type"]
            .as_str()
            .unwrap_or_else(|| panic!("every response carries a type: {event}"));
        assert!(
            event_types.contains(&event_type.to_string()),
            "unlisted event type {event_type}: {event}"
        );
        let state = event["state"]
            .as_str()
            .unwrap_or_else(|| panic!("every response carries a state: {event}"));
        assert!(
            states.contains(&state.to_string()),
            "unlisted state {state}: {event}"
        );
        seen.push(event_type.to_string());
    }
    // `preparing` is listed as extension-only and must never reach the wire.
    assert!(
        !skipped.iter().any(|event| event["state"] == "preparing"),
        "the host never emits the extension-only preparing state"
    );
    for expected in ["hello", "progress", "terminal"] {
        assert!(seen.contains(&expected.to_string()), "saw {seen:?}");
    }
}

#[test]
fn the_documented_protocol_version_matches_the_shared_fixture() {
    // docs/protocol.md is the prose contract; this keeps its version header, the
    // shared fixture, and the host's own constant from drifting apart.
    let spec = include_str!("../docs/protocol.md");
    let version = protocol()["protocol_version"]
        .as_u64()
        .expect("protocol_version is an integer");
    assert!(
        spec.contains(&format!("Version **{version}**")),
        "docs/protocol.md declares version {version}"
    );
    assert!(
        std::path::Path::new("docs/adr/0001-native-messaging-protocol.md").exists(),
        "ADR-0001 is committed"
    );
}

#[test]
fn messages_larger_than_one_mebibyte_end_the_host_with_an_error() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    // Only the length prefix is needed: the host rejects it before reading a payload.
    host.send_raw(1_048_577, &[]);
    assert_eq!(host.wait_for_exit(Duration::from_secs(5)), 1);
}

#[test]
fn closing_stdin_cancels_active_downloads_and_exits_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        progress_updates: 1,
        sleep_seconds: 30.0,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-eof");
    host.send(&request);
    host.wait_for_state("downloading");

    host.close_stdin();
    assert_eq!(host.wait_for_exit(Duration::from_secs(5)), 0);
    assert!(
        !temp.path().join("finished").exists(),
        "EOF must kill the running FFmpeg"
    );
}

/// KEI-54 acceptance criterion: a cookie value must not appear in anything the
/// host emits — no progress event, no log event, no terminal error, and nothing
/// on stderr. The extension persists those events verbatim into `downloadJobs`,
/// so an event carrying the cookie would put it in `storage.local` too.
///
/// A sentinel, never a real cookie: `AGENTS.md` forbids one in a test.
///
/// This covers what the host controls. It does not cover FFmpeg echoing a
/// token-bearing URL back through its own stderr; redacting *that* before
/// persistence is KEI-55, and ADR-0002 records it as the remaining gap.
#[test]
fn no_host_event_carries_the_cookie_value() {
    const SENTINEL: &str = "downer_sentinel=KEI54-not-a-real-session";

    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg {
        content: "partial media",
        stderr_line: Some("Server returned 403 Forbidden"),
        exit_code: 17,
        ..FakeFfmpeg::default()
    }
    .install(temp.path());
    let output_dir = temp.path().join("downloads");
    let mut host = NativeHost::start(&ffmpeg);

    let mut request = download_request("https://example.test/video.mp4", &output_dir);
    request["job_id"] = json!("job-secrets");
    request["cookie"] = json!(SENTINEL);
    host.send(&request);

    let (terminal, earlier) = host.wait_for_state("failed");
    assert_envelope(&terminal, "terminal");

    // Everything the host said, including the events seen on the way here.
    for event in earlier.iter().chain(std::iter::once(&terminal)) {
        let rendered = serde_json::to_string(event).expect("event serializes");
        assert!(
            !rendered.contains(SENTINEL),
            "a host event carried the cookie value: {}",
            event["type"]
        );
    }

    // The cookie did reach FFmpeg, so this is a statement about what the host
    // reports, not about the cookie having been dropped on the floor.
    let args = recorded_args(temp.path());
    assert!(
        args.iter().any(|argument| argument.contains(SENTINEL)),
        "the cookie still reaches FFmpeg: {args:?}"
    );
}
