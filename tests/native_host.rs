//! Black-box tests for the Firefox native messaging host (`downer --native-host`).
//!
//! Each test starts the real binary, speaks the length-prefixed JSON protocol over
//! stdio, and points the host at a fake FFmpeg through `DOWNER_FFMPEG`, so the
//! observable protocol (field names, state strings, event order) is pinned without
//! touching the network or a real FFmpeg.

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
    assert_eq!(starting["ok"], json!(true));
    assert_eq!(starting["job_id"], json!("job-success"));
    assert_eq!(starting["state"], json!("starting"));

    let (downloading, _) = host.wait_for_state("downloading");
    assert_eq!(downloading["ok"], json!(true));
    assert_eq!(downloading["job_id"], json!("job-success"));

    let (completed, _) = host.wait_for_state("completed");
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
    assert_eq!(failed["ok"], json!(false));
    assert_eq!(failed["job_id"], json!("job-failure"));
    let error = failed["error"].as_str().expect("failure carries an error");
    assert!(error.contains("network failure"), "error text: {error}");

    // FFmpeg stderr is streamed as log events on the same channel.
    assert!(
        skipped.iter().any(|event| event["log"]
            .as_str()
            .is_some_and(|line| line.contains("network failure"))),
        "expected a log event, saw {skipped:#?}"
    );
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
    assert_eq!(ack["ok"], json!(true));
    assert_eq!(ack["request_id"], json!("job-cancel-1"));

    let (cancelled, _) = host.wait_for_state("cancelled");
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
    assert_eq!(error["ok"], json!(false));
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
        assert_eq!(event["ok"], json!(false), "{command}: {event}");
        assert_eq!(event["state"], json!("control-error"), "{command}: {event}");
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
fn duplicate_job_ids_are_rejected_without_starting_a_second_download() {
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
    let (rejected, _) = host.wait_for_state("failed");
    assert_eq!(rejected["ok"], json!(false));
    assert_eq!(rejected["job_id"], json!("job-duplicate"));
    assert_eq!(rejected["request_id"], json!("job-duplicate-2"));
    assert_eq!(
        rejected["error"],
        json!("download task already exists: job-duplicate")
    );
}

#[test]
fn malformed_json_is_reported_without_a_job_id() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    let payload = b"{not json";
    host.send_raw(payload.len() as u32, payload);
    let event = host.next_event();
    assert_eq!(event["ok"], json!(false));
    assert_eq!(event["state"], json!("failed"));
    assert!(event["job_id"].is_null(), "{event}");
    assert!(
        event["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("invalid native request")),
        "{event}"
    );

    // The host keeps serving after a bad message.
    host.send(&json!({"command": "pause", "job_id": "missing", "request_id": "after-bad-json"}));
    let event = host.next_event();
    assert_eq!(event["request_id"], json!("after-bad-json"));
}

#[test]
fn unsupported_commands_fail_without_a_job_id() {
    let temp = tempfile::tempdir().unwrap();
    let ffmpeg = FakeFfmpeg::default().install(temp.path());
    let mut host = NativeHost::start(&ffmpeg);

    host.send(&json!({"command": "ping", "request_id": "ping-1"}));
    let event = host.next_event();
    assert_eq!(event["ok"], json!(false));
    assert_eq!(event["state"], json!("failed"));
    assert_eq!(event["error"], json!("unsupported native command: ping"));
    assert_eq!(event["request_id"], json!("ping-1"));
    // Pinned as current behaviour: the JS NativeTaskChannel treats any `failed`
    // state as terminal for the job owning the port, even without a `job_id`.
    // KEI-50 (protocol contract) decides whether this should change.
    assert!(event["job_id"].is_null(), "{event}");
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
