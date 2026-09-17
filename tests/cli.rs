use std::{
    fs,
    path::{Path, PathBuf},
};

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_and_version_are_available() {
    Command::cargo_bin("downer")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Download one media URL"));
    Command::cargo_bin("downer")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("downer 0.3.0"));
}

#[test]
fn invalid_url_has_stable_exit_code() {
    Command::cargo_bin("downer")
        .unwrap()
        .args([
            "ftp://example.test/video.mp4",
            "--ffmpeg",
            "definitely-not-used",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unsupported scheme"));
}

#[test]
fn output_and_dir_cannot_be_used_together() {
    Command::cargo_bin("downer")
        .unwrap()
        .args([
            "https://example.test/video.mp4",
            "--output",
            "video.mp4",
            "--dir",
            "downloads",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn existing_output_is_rejected_before_ffmpeg_is_started() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("existing.mp4");
    fs::write(&output, "keep me").unwrap();
    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--output"])
        .arg(&output)
        .arg("--ffmpeg")
        .arg(directory.path().join("missing-ffmpeg"))
        .assert()
        .code(3)
        .stderr(predicate::str::contains("already exists"));
    assert_eq!(fs::read_to_string(output).unwrap(), "keep me");
}

#[test]
fn missing_ffmpeg_has_stable_exit_code() {
    let directory = tempfile::tempdir().unwrap();
    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--dir"])
        .arg(directory.path())
        .arg("--ffmpeg")
        .arg(directory.path().join("missing-ffmpeg"))
        .assert()
        .code(4)
        .stderr(predicate::str::contains("FFmpeg executable is unavailable"));
}

#[cfg(unix)]
#[test]
fn fake_ffmpeg_can_succeed_and_receives_url_as_one_argument() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let output_dir = temp.path().join("downloads");
    let url = "https://example.test/video%20name.mp4?arg=$HOME;echo no";

    Command::cargo_bin("downer")
        .unwrap()
        .args([url, "--dir"])
        .arg(&output_dir)
        .arg("--ffmpeg")
        .arg(&fake)
        .assert()
        .success()
        .stdout(predicate::str::contains("Download complete"));

    let output = output_dir.join("video name.mp4");
    assert_eq!(fs::read_to_string(output).unwrap(), "fake media");
    let received_args = recorded_args(temp.path());
    let canonical_url = url::Url::parse(url).unwrap().to_string();
    assert!(received_args.contains(&canonical_url));
    assert!(!received_args.iter().any(|argument| argument == "no"));
}

#[cfg(unix)]
#[test]
fn fake_ffmpeg_failure_is_nonzero_and_preserves_partial_file() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), true);
    let output = temp.path().join("partial.mp4");

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--output"])
        .arg(&output)
        .arg("--ffmpeg")
        .arg(&fake)
        .assert()
        .code(5)
        .stderr(predicate::str::contains("network failure"));

    assert_eq!(fs::read_to_string(output).unwrap(), "partial media");
}

#[cfg(unix)]
fn fake_ffmpeg(directory: &Path, fail: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = directory.join(if fail {
        "fake-fail-ffmpeg"
    } else {
        "fake-ffmpeg"
    });
    let body = if fail {
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf 'partial media' > \"$last\"\nprintf '%s\\0' \"$@\" > \"$(dirname \"$0\")/args\"\necho 'network failure' >&2\nexit 17\n"
    } else {
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nmkdir -p \"$(dirname \"$last\")\"\nprintf 'fake media' > \"$last\"\nprintf '%s\\0' \"$@\" > \"$(dirname \"$0\")/args\"\n"
    };
    fs::write(&script, body).unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}

#[cfg(unix)]
fn recorded_args(directory: &Path) -> Vec<String> {
    String::from_utf8(fs::read(directory.join("args")).expect("fake FFmpeg recorded its arguments"))
        .expect("arguments are UTF-8")
        .split('\0')
        .filter(|argument| !argument.is_empty())
        .map(str::to_string)
        .collect()
}

/// The `-headers` block the fake FFmpeg was given, if any.
#[cfg(unix)]
fn recorded_headers(directory: &Path) -> Option<String> {
    let args = recorded_args(directory);
    args.iter()
        .position(|argument| argument == "-headers")
        .map(|index| args[index + 1].clone())
}

// ---------------------------------------------------------------------------
// Cookie sources (KEI-54)
//
// Never a real cookie. `AGENTS.md` forbids one in a test; these grep for a
// sentinel instead.
// ---------------------------------------------------------------------------

#[cfg(unix)]
const SENTINEL: &str = "downer_sentinel=KEI54-not-a-real-session";

/// A download that always reaches FFmpeg, so the rendered headers can be read
/// back. `--dir` keeps the output inside the temp directory.
#[cfg(unix)]
fn download(temp: &Path, fake: &Path) -> Command {
    let mut command = Command::cargo_bin("downer").unwrap();
    command
        .args(["https://example.test/video.mp4", "--dir"])
        .arg(temp.join("downloads"))
        .arg("--ffmpeg")
        .arg(fake)
        .env_remove("DOWNER_COOKIE");
    command
}

#[test]
fn cookie_and_cookie_file_together_is_an_input_error() {
    Command::cargo_bin("downer")
        .unwrap()
        .args([
            "https://example.test/video.mp4",
            "--cookie",
            "downer_sentinel=unused",
            "--cookie-file",
            "cookies.txt",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[cfg(unix)]
#[test]
fn cookie_file_is_read_and_forwarded_to_ffmpeg() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let cookie_file = temp.path().join("cookies.txt");
    // A trailing newline is what a shell redirect or an editor leaves behind.
    fs::write(&cookie_file, format!("{SENTINEL}\n")).unwrap();

    download(temp.path(), &fake)
        .arg("--cookie-file")
        .arg(&cookie_file)
        .assert()
        .success();

    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    assert_eq!(
        headers.matches("Cookie: ").count(),
        1,
        "one Cookie line: {headers:?}"
    );
    assert!(
        headers.contains(&format!("Cookie: {SENTINEL}\r\n")),
        "the trailing newline is trimmed, not forwarded: {headers:?}"
    );
}

#[cfg(unix)]
#[test]
fn cookie_environment_variable_is_used_when_no_argument_is_given() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);

    download(temp.path(), &fake)
        .env("DOWNER_COOKIE", SENTINEL)
        .assert()
        .success();

    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    assert!(headers.contains(&format!("Cookie: {SENTINEL}\r\n")));
}

#[cfg(unix)]
#[test]
fn cookie_argument_takes_precedence_over_the_environment_variable() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);

    download(temp.path(), &fake)
        .env("DOWNER_COOKIE", "downer_sentinel=from-the-environment")
        .args(["--cookie", SENTINEL])
        .assert()
        .success();

    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    assert!(headers.contains(&format!("Cookie: {SENTINEL}\r\n")));
    assert!(!headers.contains("from-the-environment"));
}

#[cfg(unix)]
#[test]
fn cookie_file_takes_precedence_over_the_environment_variable() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let cookie_file = temp.path().join("cookies.txt");
    fs::write(&cookie_file, SENTINEL).unwrap();

    download(temp.path(), &fake)
        .env("DOWNER_COOKIE", "downer_sentinel=from-the-environment")
        .arg("--cookie-file")
        .arg(&cookie_file)
        .assert()
        .success();

    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    assert!(headers.contains(&format!("Cookie: {SENTINEL}\r\n")));
    assert!(!headers.contains("from-the-environment"));
}

#[cfg(unix)]
#[test]
fn a_blank_cookie_source_sends_no_cookie_header() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let cookie_file = temp.path().join("empty.txt");
    fs::write(&cookie_file, "   \n").unwrap();

    download(temp.path(), &fake)
        .arg("--cookie-file")
        .arg(&cookie_file)
        .assert()
        .success();

    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    assert!(
        !headers.contains("Cookie:"),
        "a whitespace-only file is no cookie, not an empty one: {headers:?}"
    );
}

#[test]
fn an_unreadable_cookie_file_is_an_input_error() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--cookie-file"])
        .arg(temp.path().join("nothing-here.txt"))
        .arg("--ffmpeg")
        .arg(temp.path().join("missing-ffmpeg"))
        .env_remove("DOWNER_COOKIE")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("could not read cookie"))
        .stderr(predicate::str::contains("nothing-here.txt"));
}

/// A cookie carrying CRLF would otherwise append headers of its own choosing to
/// the `-headers` block, which is assembled by concatenation.
#[cfg(unix)]
#[test]
fn a_cookie_cannot_inject_extra_headers() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let cookie_file = temp.path().join("cookies.txt");
    fs::write(
        &cookie_file,
        format!("{SENTINEL}\r\nX-Injected: yes\r\nCookie: downer_sentinel=second"),
    )
    .unwrap();

    download(temp.path(), &fake)
        .arg("--cookie-file")
        .arg(&cookie_file)
        .assert()
        .success();

    // The CRLFs are what matter: the injected text survives as part of the
    // cookie's *value*, which is inert, but it must not become a header of its
    // own. FFmpeg splits the block on CRLF, so counting lines is the real test.
    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    let lines: Vec<&str> = headers
        .split("\r\n")
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "only User-Agent and Cookie reach FFmpeg: {lines:?}"
    );
    assert!(lines[0].starts_with("User-Agent: "), "{lines:?}");
    assert!(lines[1].starts_with("Cookie: "), "{lines:?}");
    assert!(
        !lines.iter().any(|line| line.starts_with("X-Injected:")),
        "injected header line survived: {lines:?}"
    );
}

/// Acceptance criterion: no cookie value in any log or error message. The
/// failure path is the interesting one, because it is where FFmpeg's own stderr
/// is copied into the error text.
#[cfg(unix)]
#[test]
fn a_cookie_value_never_reaches_stdout_or_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let cookie_file = temp.path().join("cookies.txt");
    fs::write(&cookie_file, SENTINEL).unwrap();

    for fail in [false, true] {
        let fake = fake_ffmpeg(temp.path(), fail);
        let output = Command::cargo_bin("downer")
            .unwrap()
            .args(["https://example.test/video.mp4", "--output"])
            .arg(temp.path().join(format!("out-{fail}.mp4")))
            .arg("--ffmpeg")
            .arg(&fake)
            .arg("--cookie-file")
            .arg(&cookie_file)
            .env_remove("DOWNER_COOKIE")
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stdout.contains(SENTINEL),
            "cookie reached stdout (ffmpeg failing: {fail})"
        );
        assert!(
            !stderr.contains(SENTINEL),
            "cookie reached stderr (ffmpeg failing: {fail})"
        );
    }
}

/// ADR-0002 accepts that the cookie stays visible in FFmpeg's argv, because
/// FFmpeg has no file-based header or cookie input. This pins that as a known,
/// decided exposure rather than an oversight: when the native HLS scheduler
/// (KEI-68/KEI-70) takes over fetching, FFmpeg stops being given cookies at all
/// and this test is the one that should fail and be deleted.
#[cfg(unix)]
#[test]
fn a_cookie_value_is_still_visible_in_ffmpeg_argv() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let cookie_file = temp.path().join("cookies.txt");
    fs::write(&cookie_file, SENTINEL).unwrap();

    download(temp.path(), &fake)
        .arg("--cookie-file")
        .arg(&cookie_file)
        .assert()
        .success();

    assert!(
        recorded_args(temp.path())
            .iter()
            .any(|argument| argument.contains(SENTINEL)),
        "ADR-0002 records this exposure; if it is gone, update the ADR"
    );
}
