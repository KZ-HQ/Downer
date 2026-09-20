//! KEI-86: a playlist the probe cannot read says *why*.
//!
//! The diagnosis existed in `scraper::resolve_media` and nowhere else, so the
//! segment-count probe answered `None` for every cause alike and the extension
//! reported "no segments found" for a challenge page. These tests drive the
//! real probe against a local server shaped like each cause — no live site, no
//! network beyond loopback.

mod support;

use std::time::Duration;

use downer::scraper::{hls_info_with_timeout, PlaylistProblem};
use support::{HeaderRecorder, Reply};
use url::Url;

const AGENT: &str = "downer tests";
const TIMEOUT: Duration = Duration::from_secs(4);

fn probe(server: &HeaderRecorder, path: &str) -> Result<downer::scraper::HlsInfo, PlaylistProblem> {
    let url = Url::parse(&server.url(path)).expect("fixture URL parses");
    hls_info_with_timeout(&url, AGENT, None, None, TIMEOUT)
}

/// The shape observed on 2026-09-19: a `403` carrying Cloudflare's own marker.
/// The same test `resolve_media` makes, now made on the path that used to throw
/// the answer away.
#[test]
fn a_challenge_by_header_is_named_as_one() {
    let server = HeaderRecorder::start("127.0.0.1");
    server.route(
        "/media/index.m3u8",
        Reply::Status {
            code: 403,
            reason: "Forbidden",
            headers: vec![("cf-mitigated", "challenge".to_string())],
            content_type: "text/html",
            body: "<!DOCTYPE html><title>Attention Required! | Cloudflare</title>".to_string(),
        },
    );

    assert_eq!(
        probe(&server, "/media/index.m3u8"),
        Err(PlaylistProblem::Challenged)
    );
    let message = PlaylistProblem::Challenged.message();
    assert!(
        message.contains("Cloudflare challenge"),
        "and it says so in the words the CLI already uses: {message}"
    );
}

/// The other shape: a challenge served as `200` with an HTML body, which never
/// reaches a status check at all.
#[test]
fn a_challenge_served_as_200_is_still_named_as_one() {
    let server = HeaderRecorder::start("127.0.0.1");
    server.route(
        "/media/index.m3u8",
        Reply::text(
            "text/html",
            "<!DOCTYPE html>\n<title>Attention Required! | Cloudflare</title>\n<body>…</body>",
        ),
    );
    // The `../playlist.m3u8` fallback is tried and 404s; the problem reported is
    // the first one, about the URL the user actually asked for.
    assert_eq!(
        probe(&server, "/media/index.m3u8"),
        Err(PlaylistProblem::Challenged)
    );
}

/// HTML that is not a challenge — a login wall, an error page, a site that
/// serves its app shell for anything it does not recognise.
#[test]
fn a_body_that_is_not_a_playlist_says_that_and_not_no_segments() {
    let server = HeaderRecorder::start("127.0.0.1");
    server.route(
        "/media/index.m3u8",
        Reply::text("text/html", "<!DOCTYPE html><title>Sign in</title>"),
    );
    assert_eq!(
        probe(&server, "/media/index.m3u8"),
        Err(PlaylistProblem::NotAPlaylist)
    );
    assert!(PlaylistProblem::NotAPlaylist
        .message()
        .contains("not a playlist"));
}

/// A real playlist that genuinely has no segments. The one case the old message
/// was right about, and it must stay distinguishable from the three it was not.
#[test]
fn an_empty_playlist_still_reports_no_segments() {
    let server = HeaderRecorder::start("127.0.0.1");
    server.route(
        "/media/index.m3u8",
        Reply::text(
            "application/vnd.apple.mpegurl",
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-ENDLIST\n",
        ),
    );
    assert_eq!(
        probe(&server, "/media/index.m3u8"),
        Err(PlaylistProblem::NoSegments)
    );
    assert!(PlaylistProblem::NoSegments
        .message()
        .contains("no segments"));
}

/// An error status that is not a challenge names the status, so "403 from the
/// CDN" and "500 from the CDN" are not the same report.
#[test]
fn an_error_status_is_reported_with_the_status() {
    let server = HeaderRecorder::start("127.0.0.1");
    // No route: the server answers 404.
    let problem = probe(&server, "/media/missing.m3u8").expect_err("404 is not a playlist");
    let PlaylistProblem::Unreachable(reason) = problem else {
        panic!("expected Unreachable, got {problem:?}");
    };
    assert!(reason.contains("404"), "{reason}");
}

/// A server that is not there at all. The message must not carry the URL: that
/// is what `docs/adr/0003-redact-urls-in-logs.md` keeps off this wire, and
/// `reqwest`'s own `Display` includes it.
#[test]
fn an_unreachable_server_says_so_without_quoting_the_url() {
    let url = Url::parse("http://127.0.0.1:9/media/index.m3u8").expect("URL parses");
    let problem = hls_info_with_timeout(&url, AGENT, None, None, Duration::from_secs(2))
        .expect_err("port 9 refuses");
    let PlaylistProblem::Unreachable(reason) = problem else {
        panic!("expected Unreachable, got {problem:?}");
    };
    assert!(
        !reason.contains("127.0.0.1") && !reason.contains("media/index.m3u8"),
        "the reason must not quote the URL: {reason}"
    );
    assert!(reason.contains("could not be reached"), "{reason}");
}

/// The path that must keep working: a real playlist still yields its totals.
#[test]
fn a_real_playlist_still_reports_its_totals() {
    let server = HeaderRecorder::start("127.0.0.1");
    server.route(
        "/media/index.m3u8",
        Reply::text(
            "application/vnd.apple.mpegurl",
            "#EXTM3U\n#EXTINF:4.0,\nseg0.ts\n#EXTINF:2.5,\nseg1.ts\n#EXT-X-ENDLIST\n",
        ),
    );
    let info = probe(&server, "/media/index.m3u8").expect("a playlist with segments");
    assert_eq!(info.total_segments, 2);
    assert_eq!(info.total_duration_ms, 6500);
}
