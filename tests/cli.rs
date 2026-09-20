use std::{
    fs,
    path::{Path, PathBuf},
};

use assert_cmd::Command;
use predicates::prelude::*;

mod support;

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
        .stdout(predicate::str::contains("downer 0.5.0"));
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

/// KEI-81: an FFmpeg below the documented minimum names itself.
///
/// The fake reports 6.1.1 — Ubuntu 24.04's package, the version the issue was
/// filed against — so this runs anywhere, including a CI machine with no FFmpeg
/// at all. What is pinned is the wording: the version the user has, and the
/// minimum they need.
#[cfg(unix)]
#[test]
fn an_ffmpeg_below_the_minimum_reports_its_version_and_the_minimum() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg_reporting(temp.path(), true, "6.1.1-3ubuntu5");
    let output = temp.path().join("partial.mp4");

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/playlist.m3u8", "--output"])
        .arg(&output)
        .arg("--ffmpeg")
        .arg(&fake)
        .assert()
        // Exit 4 is "unavailable FFmpeg": too old to use is not a media failure.
        .code(4)
        .stderr(predicate::str::contains(
            "FFmpeg 6.1.1 is older than the minimum supported 7.1",
        ))
        // FFmpeg's own complaint is still the useful detail, so it is kept.
        .stderr(predicate::str::contains("network failure"));
}

/// The other half of the decision recorded in ADR-0006: an HLS download on an
/// old FFmpeg omits the two 7.1-only options rather than dying on them, and
/// says so, so the degradation is visible rather than silent.
#[cfg(unix)]
#[test]
fn an_old_ffmpeg_downloads_hls_without_the_71_only_options() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg_reporting(temp.path(), false, "6.1.1-3ubuntu5");
    let output_dir = temp.path().join("downloads");

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/stream.m3u8", "--dir"])
        .arg(&output_dir)
        .arg("--ffmpeg")
        .arg(&fake)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "FFmpeg 6.1.1 is older than the minimum supported 7.1",
        ));

    let received_args = recorded_args(temp.path());
    assert!(
        !received_args
            .iter()
            .any(|argument| argument == "-allowed_segment_extensions"),
        "the 7.1-only options are omitted on an older FFmpeg: {received_args:?}"
    );
    assert!(!received_args
        .iter()
        .any(|argument| argument == "-extension_picky"));
    assert!(received_args.contains(&"https://example.test/stream.m3u8".to_string()));
}

/// A supported FFmpeg keeps the options: they are what makes an HLS stream with
/// unusual segment extensions work at all from 7.1 onwards.
#[cfg(unix)]
#[test]
fn a_supported_ffmpeg_still_receives_the_segment_extension_options() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let output_dir = temp.path().join("downloads");

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/stream.m3u8", "--dir"])
        .arg(&output_dir)
        .arg("--ffmpeg")
        .arg(&fake)
        .assert()
        .success()
        .stderr(predicate::str::contains("older than the minimum").not());

    let received_args = recorded_args(temp.path());
    assert!(received_args
        .windows(2)
        .any(|pair| pair == ["-allowed_segment_extensions", "ALL"]));
    assert!(received_args
        .windows(2)
        .any(|pair| pair == ["-extension_picky", "0"]));
}

/// The default for an inferred filename is `rename`: a second download of the
/// same stream lands beside the first instead of failing, which is the defect
/// KEI-60 exists to fix.
#[cfg(unix)]
#[test]
fn an_inferred_filename_renames_rather_than_failing() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let output_dir = temp.path().join("downloads");
    let url = "https://example.test/hls/index.m3u8";

    for _ in 0..2 {
        Command::cargo_bin("downer")
            .unwrap()
            .args([url, "--dir"])
            .arg(&output_dir)
            .arg("--ffmpeg")
            .arg(&fake)
            .arg("--name")
            .arg("Lecture 3")
            .assert()
            .success();
    }

    assert_eq!(
        fs::read_to_string(output_dir.join("Lecture 3.mp4")).unwrap(),
        "fake media"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("Lecture 3_2.mp4")).unwrap(),
        "fake media"
    );
}

/// `--on-conflict fail` restores the old behaviour for an inferred name, and
/// `--on-conflict overwrite` replaces in place without a second file appearing.
#[cfg(unix)]
#[test]
fn on_conflict_overrides_the_inferred_default() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let output_dir = temp.path().join("downloads");
    let url = "https://example.test/hls/index.m3u8";
    fs::create_dir_all(&output_dir).unwrap();
    fs::write(output_dir.join("Lecture 3.mp4"), "already here").unwrap();

    let attempt = |policy: &str| {
        Command::cargo_bin("downer")
            .unwrap()
            .args([url, "--dir"])
            .arg(&output_dir)
            .arg("--ffmpeg")
            .arg(&fake)
            .arg("--name")
            .arg("Lecture 3")
            .args(["--on-conflict", policy])
            .assert()
    };

    attempt("fail")
        .code(3)
        .stderr(predicate::str::contains("already exists"));
    assert_eq!(
        fs::read_to_string(output_dir.join("Lecture 3.mp4")).unwrap(),
        "already here",
        "fail must not touch the existing file"
    );
    assert!(!output_dir.join("Lecture 3_2.mp4").exists());

    attempt("overwrite").success();
    assert_eq!(
        fs::read_to_string(output_dir.join("Lecture 3.mp4")).unwrap(),
        "fake media"
    );
    assert!(
        !output_dir.join("Lecture 3_2.mp4").exists(),
        "overwrite replaces in place rather than renaming"
    );
}

/// An exact path is a place the user named, so a collision there stays an
/// error. `--on-conflict` can still override it, deliberately.
#[cfg(unix)]
#[test]
fn an_exact_output_path_still_fails_on_collision_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let output = temp.path().join("exact.mp4");
    fs::write(&output, "already here").unwrap();

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/hls/index.m3u8", "--output"])
        .arg(&output)
        .arg("--ffmpeg")
        .arg(&fake)
        .assert()
        .code(3)
        .stderr(predicate::str::contains("already exists"));
    assert_eq!(fs::read_to_string(&output).unwrap(), "already here");
    assert!(!temp.path().join("exact_2.mp4").exists());

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/hls/index.m3u8", "--output"])
        .arg(&output)
        .arg("--ffmpeg")
        .arg(&fake)
        .args(["--on-conflict", "rename"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&output).unwrap(), "already here");
    assert_eq!(
        fs::read_to_string(temp.path().join("exact_2.mp4")).unwrap(),
        "fake media"
    );
}

#[test]
fn overwrite_and_on_conflict_cannot_be_used_together() {
    Command::cargo_bin("downer")
        .unwrap()
        .args([
            "https://example.test/video.mp4",
            "--overwrite",
            "--on-conflict",
            "rename",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn an_unknown_conflict_policy_is_an_input_error() {
    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--on-conflict", "clobber"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid value"));
}

/// A download that fails before FFmpeg writes anything must leave nothing
/// behind: the reserved name is a placeholder this process created, not output.
#[cfg(unix)]
#[test]
fn a_download_that_writes_nothing_leaves_no_file_behind() {
    let temp = tempfile::tempdir().unwrap();
    let silent = fake_silent_ffmpeg(temp.path());
    let output_dir = temp.path().join("downloads");

    for _ in 0..2 {
        Command::cargo_bin("downer")
            .unwrap()
            .args(["https://example.test/hls/index.m3u8", "--dir"])
            .arg(&output_dir)
            .arg("--ffmpeg")
            .arg(&silent)
            .arg("--name")
            .arg("Lecture 3")
            .assert()
            .code(5);
    }

    assert_eq!(
        fs::read_dir(&output_dir).unwrap().count(),
        0,
        "a failed download leaves no residue: {:?}",
        fs::read_dir(&output_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    );

    // Two failures did not walk the name forward either, so a later success
    // still gets the name the user expects.
    let working = fake_ffmpeg(temp.path(), false);
    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/hls/index.m3u8", "--dir"])
        .arg(&output_dir)
        .arg("--ffmpeg")
        .arg(&working)
        .arg("--name")
        .arg("Lecture 3")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(output_dir.join("Lecture 3.mp4")).unwrap(),
        "fake media"
    );
}

/// The other half of the rule: bytes FFmpeg did write are diagnostic output and
/// survive the failure, exactly as `AGENTS.md` requires.
#[cfg(unix)]
#[test]
fn a_partially_written_download_is_still_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let failing = fake_ffmpeg(temp.path(), true);
    let output_dir = temp.path().join("downloads");

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/hls/index.m3u8", "--dir"])
        .arg(&output_dir)
        .arg("--ffmpeg")
        .arg(&failing)
        .arg("--name")
        .arg("Lecture 3")
        .assert()
        .code(5);

    assert_eq!(
        fs::read_to_string(output_dir.join("Lecture 3.mp4")).unwrap(),
        "partial media",
        "partial output is never mistaken for a reservation"
    );
}

/// An FFmpeg that cannot even be started is the same case: nothing was written,
/// so nothing should be left.
#[cfg(unix)]
#[test]
fn a_missing_ffmpeg_leaves_no_file_behind() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("downloads");

    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/hls/index.m3u8", "--dir"])
        .arg(&output_dir)
        .arg("--ffmpeg")
        .arg(temp.path().join("missing-ffmpeg"))
        .arg("--name")
        .arg("Lecture 3")
        .assert()
        .code(4);

    assert_eq!(fs::read_dir(&output_dir).unwrap().count(), 0);
}

/// The version the fakes claim unless a test asks for another one. Anything at
/// or above the documented minimum keeps them on the supported path.
#[cfg(unix)]
const FAKE_FFMPEG_VERSION: &str = "9.0.1";

/// Answer `-version` the way FFmpeg does and exit before doing anything else.
///
/// Every download probes the version now, so a fake that did not answer would
/// be asked to "download" to a file called `-version`.
#[cfg(unix)]
fn version_prelude(version: &str) -> String {
    format!(
        "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then\n  \
         echo 'ffmpeg version {version} Copyright (c) 2000-2026 the FFmpeg developers'\n  \
         exit 0\nfi\n"
    )
}

/// An FFmpeg that fails without writing to the output path at all.
#[cfg(unix)]
fn fake_silent_ffmpeg(directory: &Path) -> PathBuf {
    let script = directory.join("fake-silent-ffmpeg");
    let body = format!(
        "{}echo 'could not open input' >&2\nexit 1\n",
        version_prelude(FAKE_FFMPEG_VERSION)
    );
    install_fake(&script, &body)
}

#[cfg(unix)]
fn fake_ffmpeg(directory: &Path, fail: bool) -> PathBuf {
    fake_ffmpeg_reporting(directory, fail, FAKE_FFMPEG_VERSION)
}

/// A fake that claims `version`, so a test can drive the too-old path without a
/// real old FFmpeg.
#[cfg(unix)]
fn fake_ffmpeg_reporting(directory: &Path, fail: bool, version: &str) -> PathBuf {
    let script = directory.join(if fail {
        "fake-fail-ffmpeg"
    } else {
        "fake-ffmpeg"
    });
    let body = if fail {
        "last=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf 'partial media' > \"$last\"\nprintf '%s\\0' \"$@\" > \"$(dirname \"$0\")/args\"\necho 'network failure' >&2\nexit 17\n"
    } else {
        "last=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nmkdir -p \"$(dirname \"$last\")\"\nprintf 'fake media' > \"$last\"\nprintf '%s\\0' \"$@\" > \"$(dirname \"$0\")/args\"\n"
    };
    install_fake(&script, &format!("{}{body}", version_prelude(version)))
}

#[cfg(unix)]
fn install_fake(script: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    fs::write(script, body).unwrap();
    let mut permissions = fs::metadata(script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(script, permissions).unwrap();
    script.to_path_buf()
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

/// The value of `option` the fake FFmpeg was given, if any.
#[cfg(unix)]
fn recorded_option(directory: &Path, option: &str) -> Option<String> {
    let args = recorded_args(directory);
    args.iter()
        .position(|argument| argument == option)
        .map(|index| args[index + 1].clone())
}

/// The `-headers` block the fake FFmpeg was given, if any.
#[cfg(unix)]
fn recorded_headers(directory: &Path) -> Option<String> {
    recorded_option(directory, "-headers")
}

/// The `-cookies` value the fake FFmpeg was given, if any. Cookies live here
/// rather than in `-headers` so FFmpeg scopes them to the media host; see
/// `docs/adr/0002-cookie-scoping-and-argv-exposure.md`.
#[cfg(unix)]
fn recorded_cookies(directory: &Path) -> Option<String> {
    recorded_option(directory, "-cookies")
}

/// What `--dir` downloads render as a `domain=`: the URL's authority. The test
/// URL states no port, so no `:443` appears.
#[cfg(unix)]
const SCOPED: &str = "; path=/; domain=example.test";

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

    let cookies = recorded_cookies(temp.path()).expect("cookies were rendered");
    assert_eq!(
        cookies,
        format!("{SENTINEL}{SCOPED}"),
        "the trailing newline is trimmed, not forwarded"
    );
    assert!(
        !recorded_headers(temp.path())
            .unwrap_or_default()
            .contains("Cookie"),
        "the cookie is scoped through -cookies, never the -headers block"
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

    assert_eq!(
        recorded_cookies(temp.path()).expect("cookies were rendered"),
        format!("{SENTINEL}{SCOPED}")
    );
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

    let cookies = recorded_cookies(temp.path()).expect("cookies were rendered");
    assert_eq!(cookies, format!("{SENTINEL}{SCOPED}"));
    assert!(!cookies.contains("from-the-environment"));
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

    let cookies = recorded_cookies(temp.path()).expect("cookies were rendered");
    assert_eq!(cookies, format!("{SENTINEL}{SCOPED}"));
    assert!(!cookies.contains("from-the-environment"));
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

    assert!(
        recorded_cookies(temp.path()).is_none(),
        "a whitespace-only file is no cookie, so no -cookies argument at all"
    );
    assert!(!recorded_headers(temp.path())
        .unwrap_or_default()
        .contains("Cookie"));
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

/// A cookie must not be able to widen its own scope.
///
/// `-cookies` entries are newline-delimited and each carries `; domain=...`, so
/// a value smuggling a newline or a `;` could otherwise open an entry aimed at a
/// host of its choosing — the exact leak this scoping exists to close. The
/// `-headers` block is still checked too, since it is built by concatenation.
#[cfg(unix)]
#[test]
fn a_cookie_cannot_widen_its_own_scope() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let cookie_file = temp.path().join("cookies.txt");
    fs::write(
        &cookie_file,
        format!("{SENTINEL}\r\nevil=1; domain=attacker.test\r\nX-Injected: yes"),
    )
    .unwrap();

    download(temp.path(), &fake)
        .arg("--cookie-file")
        .arg(&cookie_file)
        .assert()
        .success();

    let cookies = recorded_cookies(temp.path()).expect("cookies were rendered");
    assert!(
        !cookies.contains("attacker.test"),
        "a cookie chose its own domain: {cookies:?}"
    );
    assert_eq!(
        cookies.lines().count(),
        1,
        "one entry, so no smuggled second scope: {cookies:?}"
    );
    assert_eq!(
        cookies.matches("domain=").count(),
        1,
        "one domain= per entry, so the scope cannot be overridden: {cookies:?}"
    );
    assert!(cookies.ends_with(SCOPED), "{cookies:?}");

    let headers = recorded_headers(temp.path()).expect("headers were rendered");
    let lines: Vec<&str> = headers
        .split("\r\n")
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "only User-Agent reaches FFmpeg: {lines:?}");
    assert!(lines[0].starts_with("User-Agent: "), "{lines:?}");
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

/// KEI-86: an old FFmpeg keeps KEI-81's headline, and its detail is cleaned up
/// like any other failure's.
///
/// The headline is this project's own, not FFmpeg's: an old FFmpeg that fails
/// names itself first, whatever it failed at. What changes is the wall of text
/// after it — FFmpeg's configuration chatter is dropped, and the cause is no
/// longer preceded by it.
#[cfg(unix)]
#[test]
fn an_old_ffmpeg_keeps_its_headline_and_loses_the_chatter() {
    let temp = tempfile::tempdir().unwrap();
    let fake = install_fake(
        &temp.path().join("fake-noisy-ffmpeg"),
        &format!(
            "{}last=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf 'partial media' > \"$last\"\n             echo 'ffmpeg stats and -progress period set to 0.5.' >&2\n             echo '[tcp @ 0x1] Connection to tcp://127.0.0.1:8081 failed: Connection refused' >&2\n             echo 'Error opening input files: Invalid data found when processing input' >&2\n             exit 17\n",
            version_prelude("6.1.1-3ubuntu5")
        ),
    );
    let output = temp.path().join("partial.mp4");

    let result = Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/playlist.m3u8", "--output"])
        .arg(&output)
        .arg("--ffmpeg")
        .arg(&fake)
        .output()
        .expect("downer runs");
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert!(
        stderr.contains("error: FFmpeg 6.1.1 is older than the minimum supported 7.1"),
        "KEI-81's wording is unchanged: {stderr}"
    );
    assert!(
        !stderr.contains("supported 7.1:\nffmpeg stats and -progress"),
        "the configuration chatter no longer opens the detail: {stderr}"
    );
    assert!(
        stderr.contains("Connection refused"),
        "and the cause is still there: {stderr}"
    );
}

/// KEI-90: `doctor` says whose download directory it is reporting.
///
/// The command line writes to the working directory; the extension writes to
/// the desktop's download folder. `doctor` is what people run to debug the
/// *extension*, so reporting one number with no label invites reading it as the
/// other.
#[cfg(unix)]
#[test]
fn doctor_distinguishes_the_command_lines_directory_from_the_extensions() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let fake = fake_ffmpeg(temp.path(), false);

    let output = Command::cargo_bin("downer")
        .unwrap()
        .arg("doctor")
        .arg("--ffmpeg")
        .arg(&fake)
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("doctor runs");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("note: this is the command line's directory"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&home.join("Downloads").display().to_string()),
        "and it names the extension's, resolved the same way a download does: {stdout}"
    );
}

/// With `--dir` there is nothing to confuse: the user named the directory.
#[cfg(unix)]
#[test]
fn doctor_with_an_explicit_directory_does_not_add_the_note() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);

    let output = Command::cargo_bin("downer")
        .unwrap()
        .arg("doctor")
        .arg("--dir")
        .arg(temp.path())
        .arg("--ffmpeg")
        .arg(&fake)
        .output()
        .expect("doctor runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("note: this is the command line's"),
        "{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Discovery, selection and JSON output (KEI-62)
//
// These drive the real binary against a loopback HTTP server, because only
// `http(s)://` URLs are accepted — a `file://` fixture cannot reach the source
// page path at all. The server is `tests/support`, already serving the variant
// and cookie-scope suites.
// ---------------------------------------------------------------------------

/// A page offering a master playlist and a plain file, in that order.
#[cfg(unix)]
fn source_page(server: &support::HeaderRecorder) -> String {
    format!(
        r#"<html><body>
             <video data-src="{}"></video>
             <source src="{}">
           </body></html>"#,
        server.url("/media/master.m3u8"),
        server.url("/media/trailer.mp4")
    )
}

#[cfg(unix)]
fn master_playlist(server: &support::HeaderRecorder) -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=400000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\"\n{}\n\
         #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n{}\n",
        server.url("/media/low.m3u8"),
        server.url("/media/high.m3u8")
    )
}

/// A server serving the page above, its playlist, and nothing else.
#[cfg(unix)]
fn discovery_server() -> support::HeaderRecorder {
    let server = support::HeaderRecorder::start("127.0.0.1");
    server.route(
        "/watch",
        support::Reply::text("text/html", source_page(&server)),
    );
    server.route(
        "/media/master.m3u8",
        support::Reply::text("application/vnd.apple.mpegurl", master_playlist(&server)),
    );
    server
}

/// `--list` on a source page names every candidate and exits 0 having
/// downloaded nothing.
#[cfg(unix)]
#[test]
fn list_prints_candidates_and_downloads_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let server = discovery_server();

    let assert = Command::cargo_bin("downer")
        .unwrap()
        .args([&server.url("/watch"), "--list"])
        .arg("--ffmpeg")
        .arg(&fake)
        .env_remove("DOWNER_COOKIE")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("  1  hls"),
        "playlists come first:\n{stdout}"
    );
    assert!(stdout.contains("/media/master.m3u8"), "{stdout}");
    assert!(stdout.contains("  2  file"), "{stdout}");
    assert!(stdout.contains("/media/trailer.mp4"), "{stdout}");
    // The renditions under the master are the point of listing it: a row
    // saying only "hls" tells the user nothing they did not type.
    assert!(stdout.contains("720p"), "{stdout}");
    assert!(stdout.contains("360p"), "{stdout}");
    assert!(
        stdout.contains("(default)"),
        "the 720p line is the default pick:\n{stdout}"
    );
    // Exiting 0 is the contract; not downloading is the other half of it.
    assert!(
        !temp.path().join("args").exists(),
        "--list must not start FFmpeg"
    );
}

/// The JSON listing is the shape `docs/adr/0020-*` promises, key for key.
#[cfg(unix)]
#[test]
fn list_json_carries_the_documented_keys() {
    let server = discovery_server();
    let assert = Command::cargo_bin("downer")
        .unwrap()
        .args([&server.url("/watch"), "--list", "--json"])
        .env_remove("DOWNER_COOKIE")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let document: serde_json::Value =
        serde_json::from_str(&stdout).expect("--list --json writes JSON and nothing else");

    assert_eq!(
        keys(&document),
        vec!["candidates", "schema_version", "source"]
    );
    assert_eq!(document["schema_version"], 1);
    assert!(document["source"].as_str().unwrap().ends_with("/watch"));

    let candidates = document["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        keys(&candidates[0]),
        vec!["experimental", "index", "kind", "renditions", "url"]
    );
    assert_eq!(candidates[0]["index"], 1);
    assert_eq!(candidates[0]["kind"], "hls");
    assert_eq!(candidates[0]["experimental"], false);

    // A plain file has nothing to choose between, so it carries no `renditions`
    // key at all rather than an empty list suggesting an empty choice.
    assert_eq!(
        keys(&candidates[1]),
        vec!["experimental", "index", "kind", "url"]
    );
    assert_eq!(candidates[1]["kind"], "file");

    let renditions = candidates[0]["renditions"].as_array().unwrap();
    assert_eq!(renditions.len(), 2);
    assert_eq!(
        keys(&renditions[0]),
        vec!["bandwidth", "codecs", "default", "height", "url", "width"]
    );
    // Best first, never the master's own order — KEI-89 measured that the
    // publisher's order puts the lowest first.
    assert_eq!(renditions[0]["height"], 720);
    assert_eq!(renditions[0]["bandwidth"], 900_000);
    assert_eq!(renditions[0]["default"], true);
    assert_eq!(renditions[1]["height"], 360);
    assert_eq!(renditions[1]["default"], false);
}

/// The keys of a JSON object, sorted, so a test can assert the whole set rather
/// than the fields it happened to think of.
#[cfg(unix)]
fn keys(value: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = value
        .as_object()
        .expect("a JSON object")
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// `--select 2` downloads the second candidate rather than the first.
#[cfg(unix)]
#[test]
fn select_downloads_the_named_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let server = discovery_server();

    Command::cargo_bin("downer")
        .unwrap()
        .args([&server.url("/watch"), "--select", "2", "--dir"])
        .arg(temp.path().join("downloads"))
        .arg("--ffmpeg")
        .arg(&fake)
        .env_remove("DOWNER_COOKIE")
        .assert()
        .success();

    let args = recorded_args(temp.path());
    assert!(
        args.iter()
            .any(|argument| argument.ends_with("trailer.mp4")),
        "the second candidate reached FFmpeg: {args:?}"
    );
}

/// `--media URL` picks the same candidate by name.
#[cfg(unix)]
#[test]
fn media_downloads_the_named_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let server = discovery_server();

    Command::cargo_bin("downer")
        .unwrap()
        .args([
            &server.url("/watch"),
            "--media",
            &server.url("/media/trailer.mp4"),
            "--dir",
        ])
        .arg(temp.path().join("downloads"))
        .arg("--ffmpeg")
        .arg(&fake)
        .env_remove("DOWNER_COOKIE")
        .assert()
        .success();

    let args = recorded_args(temp.path());
    assert!(
        args.iter()
            .any(|argument| argument.ends_with("trailer.mp4")),
        "the named candidate reached FFmpeg: {args:?}"
    );
}

/// A `--select` past the end of the list is refused, never quietly replaced
/// with the first — the rule `VariantNotOffered` applies one level down, and it
/// shares that error's exit code.
#[cfg(unix)]
#[test]
fn a_candidate_that_is_not_offered_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);
    let server = discovery_server();

    Command::cargo_bin("downer")
        .unwrap()
        .args([&server.url("/watch"), "--select", "9"])
        .arg("--ffmpeg")
        .arg(&fake)
        .env_remove("DOWNER_COOKIE")
        .assert()
        .code(5)
        .stderr(predicate::str::contains("2 candidates"));

    Command::cargo_bin("downer")
        .unwrap()
        .args([
            &server.url("/watch"),
            "--media",
            "https://elsewhere.test/other.mp4",
        ])
        .arg("--ffmpeg")
        .arg(&fake)
        .env_remove("DOWNER_COOKIE")
        .assert()
        .code(5)
        .stderr(predicate::str::contains("does not offer it"));

    assert!(
        !temp.path().join("args").exists(),
        "a refused choice starts no download"
    );
}

/// A malformed `--media` is invalid input, not an unoffered candidate: the
/// remedy is to fix the argument, which is what exit 2 means (ADR-0018).
#[cfg(unix)]
#[test]
fn a_malformed_media_argument_is_an_input_error() {
    let server = discovery_server();
    Command::cargo_bin("downer")
        .unwrap()
        .args([&server.url("/watch"), "--media", "not-a-url"])
        .env_remove("DOWNER_COOKIE")
        .assert()
        .code(2);
}

/// `--list` and a selection together is a usage error rather than one silently
/// ignoring the other.
#[test]
fn list_and_select_cannot_be_used_together() {
    Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--list", "--select", "2"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// A `--json` download writes one document and no prose, so stdout parses.
#[cfg(unix)]
#[test]
fn json_download_result_carries_the_documented_keys() {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_ffmpeg(temp.path(), false);

    let assert = Command::cargo_bin("downer")
        .unwrap()
        .args(["https://example.test/video.mp4", "--json", "--dir"])
        .arg(temp.path().join("downloads"))
        .arg("--ffmpeg")
        .arg(&fake)
        .env_remove("DOWNER_COOKIE")
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let document: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json writes JSON and nothing else");

    assert_eq!(
        keys(&document),
        vec![
            "bytes",
            "elapsed_ms",
            "engine",
            "path",
            "schema_version",
            "url"
        ]
    );
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["engine"], "ffmpeg");
    assert_eq!(document["url"], "https://example.test/video.mp4");
    // The fake writes "fake media"; the size is read off the finished file.
    assert_eq!(document["bytes"], 10);
    assert!(document["path"].as_str().unwrap().ends_with("video.mp4"));
}

/// A `--list` run against a page offering nothing is a media failure, exactly
/// as a download of it would be: the exit code says "no media here", and
/// listing does not invent a success out of an empty list.
#[cfg(unix)]
#[test]
fn listing_a_page_with_no_media_fails_like_a_download() {
    let server = support::HeaderRecorder::start("127.0.0.1");
    server.route(
        "/empty",
        support::Reply::text("text/html", "<html><body>nothing here</body></html>"),
    );

    Command::cargo_bin("downer")
        .unwrap()
        .args([&server.url("/empty"), "--list"])
        .env_remove("DOWNER_COOKIE")
        .assert()
        .code(5)
        .stderr(predicate::str::contains("no supported m3u8"));
}
