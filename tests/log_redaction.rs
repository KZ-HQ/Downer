//! Does redaction hold against what FFmpeg *actually* writes to stderr?
//!
//! `src/redact.rs` is tested against `tests/fixtures/redaction.json`, a case
//! table written by hand. That proves the rule is implemented consistently in
//! both languages, but it cannot prove the cases resemble FFmpeg's real output —
//! a table of invented lines only tests what we imagined FFmpeg logs. This
//! drives a real FFmpeg at a real socket and redacts what comes back, which is
//! the same reasoning `tests/cookie_scope.rs` applies to cookies.
//!
//! # Result: confirmed against FFmpeg 6.1.1 (Ubuntu, x86_64, 2026-09-17)
//!
//! ```text
//! [hls @ 0x…] Opening 'http://127.0.0.1:PORT/seg0.ts?…' for reading
//! [hls @ 0x…] Error when loading first segment 'http://127.0.0.1:PORT/seg0.ts?…'
//! Error opening input file http://127.0.0.1:PORT/stream.m3u8.
//! ```
//!
//! Two findings the hand-written table had not anticipated:
//!
//! * The `Opening '<url>' for reading` line is emitted at `-loglevel info`, which
//!   is the level `src/ffmpeg.rs` uses for a download with progress — so the
//!   leak this issue describes is reachable on the real download path, not only
//!   at `verbose`.
//! * It is **not the only** token-bearing line. `Error when loading first
//!   segment '<url>'` carries the same URL, and on the direct-file path
//!   `Error opening input file <url>.` does too — with a trailing full stop that
//!   must not be absorbed into the placeholder. Both go into the terminal error
//!   as well, because `DownerError::FfmpegFailed` embeds the stderr tail.
//!
//! # Running them
//!
//! These skip loudly without a real FFmpeg, as `tests/cookie_scope.rs` does, so
//! a green CI run is not evidence on its own:
//!
//! ```sh
//! cargo test --test log_redaction -- --nocapture
//! ```
//!
//! # FFmpeg version
//!
//! `README.md` documents 7.1 as the minimum supported version, and
//! `src/ffmpeg.rs` passes `-allowed_segment_extensions` and `-extension_picky`
//! for `.m3u8` inputs, which exist only from 7.1. This file therefore invokes
//! FFmpeg **without** those options, so the redaction check also runs on an
//! older FFmpeg that is otherwise unsupported. What is under test here is the
//! shape of FFmpeg's log lines, which those options do not affect.
//!
//! Since KEI-81 the download path omits those two options on an FFmpeg below
//! 7.1 rather than dying on them, so this file's invocation is no longer the
//! *only* way to reach an older FFmpeg. It is still the right one here: driving
//! FFmpeg directly keeps this test about FFmpeg's output and nothing else, and
//! it holds on a supported FFmpeg too, where the download path does pass them.

mod support;

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use downer::redact::redact_text;
use support::{HeaderRecorder, Reply};

/// Never a real token. Tests grep for this; `AGENTS.md` forbids a real value.
const SENTINEL: &str = "KEI55-not-a-real-signed-token";

/// Locate a real FFmpeg, or explain the skip and return `None`.
fn real_ffmpeg(test: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DOWNER_FFMPEG") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let found = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|directory| directory.join("ffmpeg"))
        .find(|candidate| candidate.is_file());
    if found.is_none() {
        eprintln!(
            "SKIP: {test} needs a real FFmpeg. Install one, or point DOWNER_FFMPEG at it, \
             and re-run: cargo test --test log_redaction -- --nocapture"
        );
    }
    found
}

/// Run FFmpeg over `url` at the loglevel the download path uses, and return what
/// it wrote to stderr.
///
/// The exit status is ignored on purpose: the fixture serves bytes that are not
/// decodable media, so FFmpeg is expected to fail. What is being measured is
/// what it *said*, and a failing run says more than a succeeding one.
fn ffmpeg_stderr(ffmpeg: &PathBuf, url: &str, output: &PathBuf) -> String {
    let result = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "info", "-nostats", "-nostdin"])
        .args(["-i", url, "-c", "copy", "-y"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match result {
        Ok(output) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Err(error) => panic!("FFmpeg could not be started: {error}"),
    }
}

/// A playlist whose one segment URL carries a token in its query, as a signed
/// CDN's would.
fn signed_playlist(segment_url: &str) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n\
         #EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.0,\n{segment_url}\n#EXT-X-ENDLIST\n"
    )
}

#[test]
fn ffmpeg_logs_a_signed_segment_url_and_redaction_removes_it() {
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_logs_a_signed_segment_url_and_redaction_removes_it")
    else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let origin = HeaderRecorder::start("127.0.0.1");
    let segment = format!("{}?token={SENTINEL}&e=1790000000", origin.url("/seg0.ts"));
    origin.route("/seg0.ts", Reply::text("video/mp2t", "not real media"));
    origin.route(
        "/stream.m3u8",
        Reply::text("application/vnd.apple.mpegurl", signed_playlist(&segment)),
    );

    let stderr = ffmpeg_stderr(
        &ffmpeg,
        &origin.url("/stream.m3u8"),
        &temp.path().join("out.mp4"),
    );

    // The premise. Without this the rest of the test proves nothing: it would be
    // redacting output that never carried a token in the first place.
    assert!(
        stderr.contains(SENTINEL),
        "FFmpeg did not echo the segment URL, so this says nothing about redaction. \
         FFmpeg said:\n{stderr}"
    );
    assert!(
        stderr.contains("for reading"),
        "expected the HLS demuxer's `Opening '<url>' for reading` line at -loglevel info. \
         FFmpeg said:\n{stderr}"
    );
    // `support`'s recorder splits the query off before it records a path — the
    // same discipline `tests/fixtures/protected_site.py` applies to its own log,
    // so the fixture cannot leak what it is here to catch. That the request
    // arrived at all is what matters: FFmpeg really fetched the segment whose
    // URL it logged.
    assert!(
        !origin.requests_for("/seg0.ts").is_empty(),
        "FFmpeg never fetched the segment, so its log line was never about a real request"
    );

    // The property. Every line FFmpeg really wrote, redacted, carries no token.
    for line in stderr.lines() {
        let redacted = redact_text(line);
        assert!(
            !redacted.contains(SENTINEL),
            "a real FFmpeg line survived redaction with its token intact:\n  {redacted}"
        );
    }

    // And the redacted line is still worth reading: host, port and path survive.
    let opening = stderr
        .lines()
        .find(|line| line.contains("for reading"))
        .map(redact_text)
        .expect("the Opening line was found above");
    eprintln!("real FFmpeg, redacted: {opening}");
    assert!(
        opening.contains(&format!("http://{}/seg0.ts?…", origin.authority())),
        "redaction kept too little of a real line: {opening}"
    );
}

#[test]
fn ffmpeg_logs_a_direct_url_with_a_trailing_stop_and_redaction_removes_it() {
    // The direct-file path leaks by a different line —
    // `Error opening input file <url>.` — whose trailing full stop belongs to
    // the sentence, not the URL. Absorbing it would hide it in the placeholder.
    let Some(ffmpeg) =
        real_ffmpeg("ffmpeg_logs_a_direct_url_with_a_trailing_stop_and_redaction_removes_it")
    else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let origin = HeaderRecorder::start("127.0.0.1");
    origin.route("/v.mp4", Reply::text("video/mp4", "not real media"));
    let url = format!("{}?token={SENTINEL}&e=1790000000", origin.url("/v.mp4"));

    let stderr = ffmpeg_stderr(&ffmpeg, &url, &temp.path().join("out.mp4"));

    assert!(
        stderr.contains(SENTINEL),
        "FFmpeg did not echo the input URL. FFmpeg said:\n{stderr}"
    );
    for line in stderr.lines() {
        assert!(
            !redact_text(line).contains(SENTINEL),
            "a real FFmpeg line survived redaction with its token intact:\n  {}",
            redact_text(line)
        );
    }

    let reported = stderr
        .lines()
        .find(|line| line.contains("Error opening input file"))
        .map(redact_text)
        .expect("FFmpeg names the input file it could not open");
    eprintln!("real FFmpeg, redacted: {reported}");
    assert!(
        reported.ends_with("?…."),
        "the sentence's full stop was absorbed into the placeholder: {reported}"
    );
}
