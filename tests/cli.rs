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
    let received = fs::read_to_string(temp.path().join("args")).unwrap();
    let received_args: Vec<&str> = received.lines().collect();
    let canonical_url = url::Url::parse(url).unwrap().to_string();
    assert!(received_args.contains(&canonical_url.as_str()));
    assert!(!received_args.contains(&"no"));
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
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf 'partial media' > \"$last\"\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\necho 'network failure' >&2\nexit 17\n"
    } else {
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nmkdir -p \"$(dirname \"$last\")\"\nprintf 'fake media' > \"$last\"\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n"
    };
    fs::write(&script, body).unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
    script
}
