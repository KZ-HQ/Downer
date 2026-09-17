//! Does FFmpeg send a forwarded cookie to hosts it does not belong to?
//!
//! `src/scraper.rs::ffmpeg_headers` renders the cookie as a `Cookie:` line in
//! FFmpeg's `-headers` block. FFmpeg applies that block to *every* HTTP request
//! it makes for an input, so a redirect to another host, or an HLS segment on
//! another host, receives the media host's session cookie. `-cookies` takes
//! Set-Cookie syntax with `domain=` and `path=`, which FFmpeg's `http.c` matches
//! against each request's host — if that matching holds across redirects and
//! inside the HLS demuxer, it is the fix.
//!
//! # These tests have never been run against a real FFmpeg
//!
//! FFmpeg is deliberately not installed in CI (see `AGENTS.md`) and is not
//! available in the environment this file was written in, so every test below
//! that needs one **skips**. They are written to be run by someone who has
//! FFmpeg, and KEI-54's follow-up issue is gated on that run:
//!
//! ```sh
//! cargo test --test cookie_scope -- --nocapture
//! ```
//!
//! A skip prints a line beginning `SKIP:`. Nothing here fakes an FFmpeg: a fake
//! would only tell us what we asked FFmpeg to do, which is what the argv-
//! recording fakes in `tests/cli.rs` and `tests/native_host.rs` already prove.
//! Only a real FFmpeg on a real socket shows what reaches the wire.
//!
//! The two servers bind `localhost` and `127.0.0.1`. Both are loopback, but they
//! are different host *strings*, which is what FFmpeg's cookie matching compares
//! — so no DNS and no `/etc/hosts` entry is needed.

mod support;

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use support::{get, HeaderRecorder, Reply};

/// Never a real cookie. Tests grep for this; `AGENTS.md` forbids a real value.
const SENTINEL: &str = "downer_sentinel=KEI54-not-a-real-session";

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
             and re-run: cargo test --test cookie_scope -- --nocapture"
        );
    }
    found
}

/// Run FFmpeg over `url` with the given cookie-passing arguments and wait for it
/// to finish. The exit status is ignored on purpose: the fixture server serves
/// bytes that are not decodable media, so FFmpeg is expected to fail. What is
/// being measured is which requests it made and what it put on them.
fn run_ffmpeg(ffmpeg: &PathBuf, cookie_args: &[&str], url: &str, output: &PathBuf) {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    command.args(cookie_args);
    command.args(["-i", url, "-c", "copy", "-y"]);
    command.arg(output);
    let _ = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `-headers` with a raw `Cookie:` line: today's behaviour.
fn headers_args(cookie: &str) -> Vec<String> {
    vec!["-headers".to_string(), format!("Cookie: {cookie}\r\n")]
}

/// `-cookies` in Set-Cookie syntax, scoped to one host.
fn cookies_args(cookie: &str, domain: &str) -> Vec<String> {
    vec![
        "-cookies".to_string(),
        format!("{cookie}; path=/; domain={domain}"),
    ]
}

fn as_args(owned: &[String]) -> Vec<&str> {
    owned.iter().map(String::as_str).collect()
}

/// The fixture server itself, proven without FFmpeg so it runs everywhere —
/// including CI, where the FFmpeg tests below only skip.
#[test]
fn fixture_server_records_headers_and_serves_routes() {
    let origin = HeaderRecorder::start("localhost");
    let other = HeaderRecorder::start("127.0.0.1");
    other.route("/real.mp4", Reply::text("video/mp4", "not real media"));
    origin.route("/video.mp4", Reply::Redirect(other.url("/real.mp4")));
    origin.route("/plain", Reply::text("text/plain", "hello"));

    let (status, body) = get(&origin.url("/plain"), Some(SENTINEL));
    assert_eq!(status, 200);
    assert_eq!(body, "hello");

    let (status, _) = get(&origin.url("/video.mp4"), None);
    assert_eq!(status, 302, "the redirect route answers with a redirect");

    let (status, _) = get(&origin.url("/missing"), None);
    assert_eq!(status, 404, "an unrouted path is a 404");

    let recorded = origin.requests_for("/plain");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].method, "GET");
    assert_eq!(
        recorded[0].cookie(),
        Some(SENTINEL),
        "the server records the Cookie header it received"
    );
    assert!(
        origin.requests_for("/video.mp4")[0].cookie().is_none(),
        "a request sent without a cookie records none"
    );
    assert!(
        other.requests().is_empty(),
        "the second server is independent and saw nothing"
    );
}

/// Across a redirect from the media host to another host, does the cookie
/// follow?
///
/// Confirms domain-scoped `-cookies` when it passes. Refutes it — with a message
/// naming the host that received the cookie — when it fails.
#[test]
fn ffmpeg_cookie_scope_across_a_redirect() {
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_cookie_scope_across_a_redirect") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();

    for (mode, scoped) in [("headers", false), ("cookies", true)] {
        let origin = HeaderRecorder::start("localhost");
        let elsewhere = HeaderRecorder::start("127.0.0.1");
        elsewhere.route("/real.mp4", Reply::text("video/mp4", "not real media"));
        origin.route("/video.mp4", Reply::Redirect(elsewhere.url("/real.mp4")));

        let args = if scoped {
            cookies_args(SENTINEL, origin.host())
        } else {
            headers_args(SENTINEL)
        };
        run_ffmpeg(
            &ffmpeg,
            &as_args(&args),
            &origin.url("/video.mp4"),
            &temp.path().join(format!("redirect-{mode}.mp4")),
        );

        let at_origin = origin.requests_for("/video.mp4");
        let at_elsewhere = elsewhere.requests_for("/real.mp4");
        assert!(
            !at_origin.is_empty(),
            "[{mode}] FFmpeg never reached the media host; the fixture server or the \
             arguments are wrong, not FFmpeg's cookie handling"
        );
        assert!(
            !at_elsewhere.is_empty(),
            "[{mode}] FFmpeg did not follow the redirect, so this says nothing about \
             cross-host cookies"
        );

        let origin_got = at_origin.iter().any(|request| request.cookie().is_some());
        let elsewhere_got = at_elsewhere
            .iter()
            .any(|request| request.cookie().is_some());
        eprintln!(
            "redirect/{mode}: media host received cookie = {origin_got}, \
             redirect target received cookie = {elsewhere_got}"
        );

        assert!(
            origin_got,
            "[{mode}] the media host received no cookie at all. For `cookies` this \
             refutes domain-scoped -cookies: FFmpeg did not match the cookie to its \
             own domain, so switching to -cookies would break protected downloads."
        );
        if scoped {
            assert!(
                !elsewhere_got,
                "[cookies] the redirect target {} received the cookie. Domain-scoped \
                 -cookies is REFUTED for redirects; scoping must be done another way.",
                elsewhere.host()
            );
        } else {
            assert!(
                elsewhere_got,
                "[headers] the redirect target did not receive the cookie. This FFmpeg \
                 does not leak through -headers the way ADR-0002 records; revisit the ADR."
            );
        }
    }
}

/// The HLS case: a playlist on the media host whose segment lives on another
/// host. The demuxer opens the segment as a separate HTTP request, and the
/// question is whether the cookie rides along.
#[test]
fn ffmpeg_cookie_scope_for_a_cross_host_hls_segment() {
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_cookie_scope_for_a_cross_host_hls_segment") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();

    for (mode, scoped) in [("headers", false), ("cookies", true)] {
        let origin = HeaderRecorder::start("localhost");
        let cdn = HeaderRecorder::start("127.0.0.1");
        cdn.route("/segment.ts", Reply::text("video/mp2t", "not real media"));
        origin.route(
            "/stream.m3u8",
            Reply::text(
                "application/vnd.apple.mpegurl",
                format!(
                    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n\
                     #EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.0,\n{}\n#EXT-X-ENDLIST\n",
                    cdn.url("/segment.ts")
                ),
            ),
        );

        let args = if scoped {
            cookies_args(SENTINEL, origin.host())
        } else {
            headers_args(SENTINEL)
        };
        run_ffmpeg(
            &ffmpeg,
            &as_args(&args),
            &origin.url("/stream.m3u8"),
            &temp.path().join(format!("hls-{mode}.mp4")),
        );

        let at_origin = origin.requests_for("/stream.m3u8");
        let at_cdn = cdn.requests_for("/segment.ts");
        assert!(
            !at_origin.is_empty(),
            "[{mode}] FFmpeg never fetched the playlist"
        );
        assert!(
            !at_cdn.is_empty(),
            "[{mode}] FFmpeg never fetched the cross-host segment, so this says nothing \
             about cross-host cookies"
        );

        let playlist_got = at_origin.iter().any(|request| request.cookie().is_some());
        let segment_got = at_cdn.iter().any(|request| request.cookie().is_some());
        eprintln!(
            "hls/{mode}: playlist host received cookie = {playlist_got}, \
             segment host received cookie = {segment_got}"
        );

        assert!(
            playlist_got,
            "[{mode}] the playlist host received no cookie. For `cookies` this refutes \
             domain-scoped -cookies: a protected playlist would stop downloading."
        );
        if scoped {
            assert!(
                !segment_got,
                "[cookies] the cross-host segment server {} received the cookie. \
                 Domain-scoped -cookies does NOT propagate scoping into the HLS demuxer; \
                 the native scheduler (KEI-68/KEI-70) is the only fix for HLS.",
                cdn.host()
            );
        } else {
            assert!(
                segment_got,
                "[headers] the cross-host segment server did not receive the cookie. \
                 This FFmpeg does not leak through -headers the way ADR-0002 records; \
                 revisit the ADR."
            );
        }
    }
}
