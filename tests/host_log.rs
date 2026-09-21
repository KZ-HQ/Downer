//! The native host's log file, exercised through the real binary.
//!
//! These are KEI-64's acceptance criteria, and they are black-box on purpose.
//! `src/hostlog.rs` unit-tests the writer — rotation, levels, quoting — against
//! a `HostLog` it constructs directly. What those cannot show is that the host
//! *wires it up*: that starting the process and running a download actually
//! produces a file, in the place `downer doctor` names, with the events in it
//! and the secrets not.
//!
//! The sentinel test is the one that matters. It sends a cookie and a signed URL
//! through the whole path — native port, host, `FfmpegInvocation`, FFmpeg's argv
//! and stderr — and then greps the file for values that must not have survived.
//! Both sentinels are invented strings: `AGENTS.md` forbids a real cookie in a
//! test, and an invented one proves exactly the same thing, because the code
//! never inspects the value.

#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

/// Strings that must never reach the log file.
///
/// Invented, never a real credential. The cookie value stands in for a session
/// token; the query token stands in for a CDN signature.
const COOKIE_SENTINEL: &str = "DOWNER-COOKIE-SENTINEL-4b71f2";
const QUERY_SENTINEL: &str = "DOWNER-QUERY-SENTINEL-9c3ade";

const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

/// A host running under a temporary `HOME`, so its log lands somewhere this
/// test owns and the developer's real log is never touched.
struct LoggingHost {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Value>,
    home: PathBuf,
}

impl LoggingHost {
    fn start(home: &Path, ffmpeg: &Path, level: &str) -> Self {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("downer"))
            .arg("--native-host")
            .env("HOME", home)
            .env("DOWNER_FFMPEG", ffmpeg)
            .env("DOWNER_LOG", level)
            // Cleared so the log path is derived from `HOME` alone; otherwise
            // the developer's own XDG settings would decide where it lands.
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("native host starts");

        let stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let (sender, events) = mpsc::channel();
        thread::spawn(move || {
            while let Some(payload) = read_message(&mut stdout) {
                let Ok(message) = serde_json::from_slice::<Value>(&payload) else {
                    return;
                };
                if sender.send(message).is_err() {
                    return;
                }
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            events,
            home: home.to_path_buf(),
        }
    }

    /// Where the host writes on this platform, mirroring
    /// `hostlog::path_in`. Asserted against `downer doctor` below, so the two
    /// cannot drift.
    fn log_path(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home.join("Library/Logs/downer/host.log")
        } else {
            self.home.join(".local/state/downer/logs/host.log")
        }
    }

    fn send(&mut self, request: &Value) {
        let payload = serde_json::to_vec(request).expect("request serializes");
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin
            .write_all(&(payload.len() as u32).to_le_bytes())
            .expect("length write");
        stdin.write_all(&payload).expect("payload write");
        stdin.flush().expect("flush");
    }

    /// Wait for a terminal event, so the assertions run against a finished job.
    fn wait_for_terminal(&self) -> Value {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            let Ok(event) = self.events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if event.get("type").and_then(Value::as_str) == Some("terminal") {
                return event;
            }
        }
        panic!("no terminal event within {EVENT_TIMEOUT:?}");
    }

    fn shutdown(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }

    fn read_log(&self) -> String {
        // The host writes `host.stop` on its way out, so the file is only
        // certainly complete once the process has exited.
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }
}

impl Drop for LoggingHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
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

/// A fake FFmpeg that also echoes a signed URL to stderr, the way the real one
/// does for every HLS segment it opens.
fn install_fake_ffmpeg(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "-version" ]; then
  echo 'ffmpeg version 9.0.1 Copyright (c) 2000-2026 the FFmpeg developers'
  exit 0
fi
dir="$(dirname "$0")"
last=""
for arg in "$@"; do last="$arg"; done
printf '%s\0' "$@" > "$dir/args"
mkdir -p "$(dirname "$last")"
printf '%s' 'fake media' > "$last"
echo "[hls @ 0x1] Opening 'https://cdn.example.test/seg1.ts?token={QUERY_SENTINEL}' for reading" >&2
echo "out_time_us=1000000"
echo "progress=continue"
echo "progress=end"
exit 0
"#
    );
    let path = directory.join("fake-ffmpeg");
    fs::write(&path, script).expect("fake FFmpeg written");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("fake is executable");
    path
}

fn download_request(output_dir: &Path) -> Value {
    json!({
        "command": "download",
        "protocol_version": 1,
        "job_id": "job-log-1",
        "request_id": "req-log-1",
        "url": format!("https://cdn.example.test/master.m3u8?token={QUERY_SENTINEL}"),
        "source_url": "https://example.test/watch",
        "cookie": format!("session={COOKIE_SENTINEL}"),
        "output_dir": output_dir,
        "title": "A Very Private Page Title",
    })
}

/// Running a download produces a log file, with the events that explain the run.
#[test]
fn a_download_leaves_a_log_file_describing_what_happened() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let downloads = temp.path().join("downloads");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&downloads).unwrap();
    let ffmpeg = install_fake_ffmpeg(temp.path());

    let mut host = LoggingHost::start(&home, &ffmpeg, "debug");
    host.send(&json!({"command": "hello", "protocol_version": 1}));
    host.send(&download_request(&downloads));
    host.wait_for_terminal();
    host.shutdown();

    let log = host.read_log();
    assert!(!log.is_empty(), "the host wrote a log file");

    for event in [
        "host.start",
        "request.received",
        "ffmpeg.spawn",
        "job.terminal",
        "host.stop",
    ] {
        assert!(log.contains(event), "{event} is missing from:\n{log}");
    }
    // The command is named, so a reader can tell which request did what.
    assert!(log.contains("command=download"), "{log}");
    assert!(log.contains("job_id=job-log-1"), "{log}");
}

/// The acceptance criterion: no cookie value and no URL query reaches the file.
#[test]
fn no_cookie_value_or_query_string_reaches_the_log_file() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let downloads = temp.path().join("downloads");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&downloads).unwrap();
    let ffmpeg = install_fake_ffmpeg(temp.path());

    // `debug` deliberately: the loudest level is the one most likely to leak,
    // so the criterion is proven where it is hardest to satisfy.
    let mut host = LoggingHost::start(&home, &ffmpeg, "debug");
    host.send(&download_request(&downloads));
    host.wait_for_terminal();
    host.shutdown();

    let log = host.read_log();
    assert!(!log.is_empty(), "there is a log to check");

    assert!(
        !log.contains(COOKIE_SENTINEL),
        "a cookie value reached the log file:\n{log}"
    );
    assert!(
        !log.contains(QUERY_SENTINEL),
        "a URL query string reached the log file:\n{log}"
    );
    // Not merely absent by accident: the cookie was forwarded, and the log says
    // so by counting it.
    assert!(
        log.contains("<1 cookie>"),
        "the log should record that a cookie was sent, without its value:\n{log}"
    );
    // The fake's stderr carried the signed URL, so the redaction actually ran
    // over it rather than the line never arriving.
    assert!(
        log.contains("https://cdn.example.test/seg1.ts"),
        "the segment URL should survive, minus its query:\n{log}"
    );
}

/// A page title is user data. `AGENTS.md` allows it into the output path and
/// nowhere else, and the output filename *is* the title.
#[test]
fn a_page_title_never_reaches_the_log_file() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let downloads = temp.path().join("downloads");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&downloads).unwrap();
    let ffmpeg = install_fake_ffmpeg(temp.path());

    let mut host = LoggingHost::start(&home, &ffmpeg, "debug");
    host.send(&download_request(&downloads));
    host.wait_for_terminal();
    host.shutdown();

    let log = host.read_log();
    assert!(
        !log.contains("Very Private Page Title"),
        "a page title reached the log file:\n{log}"
    );
    // The directory is logged, though: "where was it writing?" is the question
    // a log exists to answer, and the directory answers it without the name.
    assert!(
        log.contains(&downloads.display().to_string()),
        "the output directory should be logged:\n{log}"
    );
}

/// Turning logging off leaves no file at all.
#[test]
fn logging_off_writes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let downloads = temp.path().join("downloads");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&downloads).unwrap();
    let ffmpeg = install_fake_ffmpeg(temp.path());

    let mut host = LoggingHost::start(&home, &ffmpeg, "off");
    host.send(&download_request(&downloads));
    host.wait_for_terminal();
    host.shutdown();

    assert!(
        !host.log_path().exists(),
        "logging off must leave no file behind"
    );
}

/// `downer doctor` names the same file the host writes.
///
/// The two derive the path independently — the host at start-up, the doctor
/// from `hostlog::default_path` — so this is the test that keeps the path a
/// user is told about pointing at the file a user needs.
#[test]
fn the_doctor_reports_the_path_the_host_actually_writes() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let downloads = temp.path().join("downloads");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&downloads).unwrap();
    let ffmpeg = install_fake_ffmpeg(temp.path());

    let mut host = LoggingHost::start(&home, &ffmpeg, "info");
    host.send(&json!({"command": "hello", "protocol_version": 1}));
    host.send(&download_request(&downloads));
    host.wait_for_terminal();
    host.shutdown();
    let written = host.log_path();
    assert!(written.is_file(), "the host wrote {}", written.display());

    let output = Command::new(assert_cmd::cargo::cargo_bin("downer"))
        .arg("doctor")
        .env("HOME", &home)
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("doctor runs");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Native host log:"),
        "doctor names the log file:\n{stdout}"
    );
    assert!(
        stdout.contains(&written.display().to_string()),
        "doctor names the file the host wrote ({}):\n{stdout}",
        written.display()
    );
}

/// The `status` response carries the path too, which is what the Settings
/// panel renders.
#[test]
fn the_status_response_carries_the_log_path() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let ffmpeg = install_fake_ffmpeg(temp.path());

    let mut host = LoggingHost::start(&home, &ffmpeg, "info");
    host.send(&json!({"command": "status", "protocol_version": 1, "request_id": "req-status"}));

    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut status = None;
    while Instant::now() < deadline && status.is_none() {
        if let Ok(event) = host.events.recv_timeout(Duration::from_millis(500)) {
            if event.get("type").and_then(Value::as_str) == Some("status") {
                status = Some(event);
            }
        }
    }
    let status = status.expect("a status response arrives");
    let log_path = status
        .pointer("/status/log_path")
        .and_then(Value::as_str)
        .expect("status carries log_path");

    assert_eq!(log_path, host.log_path().display().to_string());
    host.shutdown();
}
