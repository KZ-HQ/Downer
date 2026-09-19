//! KEI-89: a master playlist resolves to one rendition before FFmpeg sees it.
//!
//! Handing FFmpeg a master playlist makes it open *every* variant stream and
//! keep only the best, so the rest is downloaded and discarded. Measured on
//! FFmpeg 9.0.1 against a two-rendition master: 10 requests, both renditions
//! fetched, versus 5 for the variant alone. See
//! `docs/adr/0010-resolve-hls-master-playlists.md`.
//!
//! What these tests pin is the decisive step — the URL FFmpeg is given — using
//! a fake FFmpeg that records its arguments. The downstream half, that a
//! variant URL fetches only that rendition, is FFmpeg's own behaviour and was
//! measured rather than asserted here: the recording server serves text, so it
//! cannot serve the real segments a genuine download would need.

#![cfg(unix)]

mod support;

use std::{fs, path::Path, path::PathBuf};

use downer::{output::NamingHints, scraper::ResolvedMedia, DownloadOptions, Hooks};
use support::{HeaderRecorder, Reply};

const USER_AGENT: &str = "downer-test/1.0";
const FAKE_VERSION: &str = "9.0.1";

fn master_playlist(low: &str, high: &str) -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=320x180\n{low}\n\
         #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=1280x720\n{high}\n"
    )
}

fn media_playlist(origin: &HeaderRecorder) -> String {
    let mut lines = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n");
    for index in 0..4 {
        lines.push_str("#EXTINF:2.0,\n");
        lines.push_str(&origin.url(&format!("/media/high/segment{index}.ts")));
        lines.push('\n');
    }
    lines.push_str("#EXT-X-ENDLIST\n");
    lines
}

/// Run a download against a fake FFmpeg and return the argv it received.
fn argv_for(url: &str) -> Vec<String> {
    let temp = tempfile::tempdir().expect("a temporary directory");
    let fake = install_recording_fake(temp.path());
    let output = temp.path().join("out.mp4");
    let media = ResolvedMedia {
        url: url::Url::parse(url).expect("a valid URL"),
        referer: None,
        user_agent: USER_AGENT.to_string(),
    };
    let options = DownloadOptions {
        output: Some(output),
        dir: None,
        overwrite: false,
        on_conflict: None,
        naming: NamingHints::new(None),
        ffmpeg: fake,
        user_agent: USER_AGENT.to_string(),
        cookie: None,
        threads: None,
        quiet: true,
    };
    downer::download_resolved(media, &options, Hooks::default()).expect("the fake download runs");
    recorded(temp.path())
}

/// The `-i` value out of a recorded argv.
fn input_of(args: &[String]) -> String {
    let at = args
        .iter()
        .position(|argument| argument == "-i")
        .expect("argv has an input");
    args[at + 1].clone()
}

#[test]
fn a_master_playlist_resolves_to_the_highest_rendition() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let low = origin.url("/media/low.m3u8");
    let high = origin.url("/media/high.m3u8");
    origin.route(
        "/media/master.m3u8",
        Reply::text(
            "application/vnd.apple.mpegurl",
            master_playlist(&low, &high),
        ),
    );

    let args = argv_for(&origin.url("/media/master.m3u8"));
    assert_eq!(
        input_of(&args),
        high,
        "FFmpeg is given the chosen rendition, not the master: {args:?}"
    );

    // One request, and it is the master. The master lists every rendition in
    // one file, so resolving does not grow with the number of them, and no
    // variant playlist or segment is fetched to make the choice.
    let requests = origin.requests();
    assert_eq!(
        requests.len(),
        1,
        "resolving costs exactly one request: {:?}",
        requests.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    assert_eq!(requests[0].path, "/media/master.m3u8");
}

#[test]
fn a_media_playlist_is_passed_through_unchanged() {
    let origin = HeaderRecorder::start("127.0.0.1");
    origin.route(
        "/media/video.m3u8",
        Reply::text("application/vnd.apple.mpegurl", media_playlist(&origin)),
    );

    let url = origin.url("/media/video.m3u8");
    let args = argv_for(&url);
    assert_eq!(
        input_of(&args),
        url,
        "a playlist with no renditions is used as given: {args:?}"
    );
}

/// The safety property: resolution is best-effort.
///
/// A playlist the host cannot fetch or parse must not break a download that
/// would otherwise work — a challenged CDN answers the host with HTML while
/// FFmpeg, carrying the page's session, may still succeed (KEI-87). Falling
/// back costs the bandwidth this resolution would have saved, and nothing else.
#[test]
fn an_unreadable_playlist_falls_back_to_the_url_as_given() {
    let origin = HeaderRecorder::start("127.0.0.1");
    // What a challenge looks like: a 200 that is not a playlist at all.
    origin.route(
        "/media/challenged.m3u8",
        Reply::text(
            "text/html",
            "<!DOCTYPE html><title>Attention Required!</title>",
        ),
    );

    let url = origin.url("/media/challenged.m3u8");
    let args = argv_for(&url);
    assert_eq!(
        input_of(&args),
        url,
        "an unparseable playlist falls back rather than failing: {args:?}"
    );

    // A route that does not exist at all is the other failure shape.
    let missing = origin.url("/media/absent.m3u8");
    assert_eq!(input_of(&argv_for(&missing)), missing);
}

fn install_recording_fake(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = directory.join("fake-ffmpeg");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then\n  \
         echo 'ffmpeg version {FAKE_VERSION} Copyright (c) 2000-2026 the FFmpeg developers'\n  \
         exit 0\nfi\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\n\
         mkdir -p \"$(dirname \"$last\")\"\nprintf 'fake media' > \"$last\"\n\
         printf '%s\\0' \"$@\" > \"$(dirname \"$0\")/args\"\n"
    );
    fs::write(&script, body).expect("the fake is written");
    let mut permissions = fs::metadata(&script)
        .expect("the fake exists")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("the fake is executable");
    script
}

fn recorded(directory: &Path) -> Vec<String> {
    String::from_utf8(fs::read(directory.join("args")).expect("the fake recorded its arguments"))
        .expect("arguments are UTF-8")
        .split('\0')
        .filter(|argument| !argument.is_empty())
        .map(str::to_string)
        .collect()
}
