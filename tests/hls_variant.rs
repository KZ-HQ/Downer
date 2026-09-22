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
    argv_for_with_text(url, None)
}

/// As `argv_for`, with a playlist the caller already fetched.
fn argv_for_with_text(url: &str, playlist_text: Option<&str>) -> Vec<String> {
    argv_for_choice(url, playlist_text, None).expect("the fake download runs")
}

/// As `argv_for_with_text`, naming a rendition explicitly (KEI-61).
fn argv_for_choice(
    url: &str,
    playlist_text: Option<&str>,
    rendition: Option<&str>,
) -> Result<Vec<String>, downer::error::DownerError> {
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
        playlist_text: playlist_text.map(str::to_string),
        rendition: rendition.map(|raw| raw.parse().expect("a valid rendition selector")),
        keep_partial: true,
        reconnect: Some(downer::ffmpeg::Reconnect::default()),
        // No retry in an argv test: it asserts on what one invocation renders,
        // and a retried run would record the last attempt's arguments.
        retries: 0,
        timeout: downer::DEFAULT_TIMEOUT,
    };
    downer::download_resolved(media, &options, Hooks::default())?;
    Ok(recorded(temp.path()))
}

/// Every `-i` value out of a recorded argv, in order.
fn inputs_of(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(_, argument)| *argument == "-i")
        .filter_map(|(at, _)| args.get(at + 1).cloned())
        .collect()
}

/// Every `-map` value out of a recorded argv, in order.
fn maps_of(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(_, argument)| *argument == "-map")
        .filter_map(|(at, _)| args.get(at + 1).cloned())
        .collect()
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

/// A master whose audio is a separate rendition is resolved to **both**.
///
/// ADR-0010 declined here, because `select_variant` returned one URL and could
/// not say "this video plus that audio": handing FFmpeg the variant alone
/// silently lost the sound. ADR-0014 pairs them instead — measured on FFmpeg
/// 9.0.2, the variant plus the audio playlist with `-map 0:v:0 -map 1:a:0`
/// writes video and audio while fetching only the chosen rendition.
#[test]
fn a_master_with_separate_audio_resolves_to_the_variant_and_its_audio() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let video = origin.url("/media/v/video.m3u8");
    let audio = origin.url("/media/a/audio.m3u8");
    let master = format!(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,URI=\"{audio}\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aud\"\n{video}\n"
    );
    origin.route(
        "/media/with-audio.m3u8",
        Reply::text("application/vnd.apple.mpegurl", master),
    );

    let args = argv_for(&origin.url("/media/with-audio.m3u8"));
    assert_eq!(inputs_of(&args), [video, audio], "{args:?}");
    assert_eq!(
        maps_of(&args),
        ["0:v:0", "1:a:0"],
        "input 0 is a media playlist, so 0:v:0 is unambiguous: {args:?}"
    );
}

/// Every input option is repeated before every `-i`.
///
/// FFmpeg applies `-headers`, `-cookies` and the segment-extension options to
/// the *next* input only. Emitting them once would fetch the audio playlist
/// without the session the video playlist needed, which on a cookie-gated CDN
/// is a download that half works.
#[test]
fn each_input_carries_the_session_options() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let video = origin.url("/media/v/video.m3u8");
    let audio = origin.url("/media/a/audio.m3u8");
    let master = format!(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,URI=\"{audio}\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aud\"\n{video}\n"
    );
    origin.route(
        "/media/with-audio.m3u8",
        Reply::text("application/vnd.apple.mpegurl", master),
    );

    let args = argv_for(&origin.url("/media/with-audio.m3u8"));
    assert_eq!(
        args.iter()
            .filter(|argument| *argument == "-headers")
            .count(),
        2,
        "{args:?}"
    );
    assert_eq!(
        args.iter()
            .filter(|argument| *argument == "-allowed_segment_extensions")
            .count(),
        2,
        "{args:?}"
    );
}

/// The choice KEI-61 adds: a named rendition is the one downloaded, and it
/// still arrives with its audio.
#[test]
fn an_explicitly_chosen_rendition_is_the_one_downloaded() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let low = origin.url("/media/low/video.m3u8");
    let high = origin.url("/media/high/video.m3u8");
    let audio = origin.url("/media/a/audio.m3u8");
    let master = format!(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,URI=\"{audio}\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=320x180,AUDIO=\"aud\"\n{low}\n\
         #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720,AUDIO=\"aud\"\n{high}\n"
    );
    origin.route(
        "/media/master.m3u8",
        Reply::text("application/vnd.apple.mpegurl", master),
    );
    let url = origin.url("/media/master.m3u8");

    // Not choosing is unchanged: the highest bandwidth, as KEI-89 left it.
    let args = argv_for(&url);
    assert_eq!(inputs_of(&args), [high.clone(), audio.clone()], "{args:?}");

    // Choosing the low rendition downloads the low rendition — with sound,
    // which no route could reach before ADR-0014.
    let args = argv_for_choice(&url, None, Some(&low)).expect("the fake download runs");
    assert_eq!(inputs_of(&args), [low.clone(), audio], "{args:?}");
}

/// A rendition the master does not declare stops the download rather than
/// quietly becoming the default one.
#[test]
fn a_rendition_that_is_not_offered_is_refused() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let high = origin.url("/media/high/video.m3u8");
    let master =
        format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720\n{high}\n");
    origin.route(
        "/media/master.m3u8",
        Reply::text("application/vnd.apple.mpegurl", master),
    );

    let absent = origin.url("/media/4k/video.m3u8");
    let error = argv_for_choice(&origin.url("/media/master.m3u8"), None, Some(&absent))
        .expect_err("an unoffered rendition is refused");
    assert!(
        matches!(error, downer::error::DownerError::VariantNotOffered(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("does not offer that rendition"));
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

/// KEI-51: a playlist the extension already fetched is parsed, not refetched.
///
/// The extension has the page's session and the host does not, so the text it
/// fetched is the one that can be read at all on a challenged CDN (KEI-87).
/// Supplying it must also cost nothing: the recording server sees no request.
#[test]
fn a_supplied_master_playlist_is_used_without_any_fetch() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let low = origin.url("/media/low.m3u8");
    let high = origin.url("/media/high.m3u8");
    // Deliberately not routed: reaching for it over HTTP would 404, so a pass
    // here proves the text was read rather than the URL fetched.
    let url = origin.url("/media/master.m3u8");

    let args = argv_for_with_text(&url, Some(&master_playlist(&low, &high)));
    assert_eq!(
        input_of(&args),
        high,
        "the supplied master resolved to its highest rendition: {args:?}"
    );
    assert!(
        origin.requests().is_empty(),
        "supplying the text means no fetch at all: {:?}",
        origin
            .requests()
            .iter()
            .map(|r| &r.path)
            .collect::<Vec<_>>()
    );
}

/// A supplied media playlist is left as the input, and still not refetched.
#[test]
fn a_supplied_media_playlist_needs_no_fetch_either() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let url = origin.url("/media/video.m3u8");

    let args = argv_for_with_text(&url, Some(&media_playlist(&origin)));
    assert_eq!(input_of(&args), url);
    assert!(origin.requests().is_empty(), "no fetch was needed");
}

/// Text that is not a playlist falls back to the URL, as a failed fetch does.
#[test]
fn supplied_text_that_is_not_a_playlist_falls_back() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let url = origin.url("/media/challenged.m3u8");

    let args = argv_for_with_text(
        &url,
        Some("<!DOCTYPE html><title>Attention Required!</title>"),
    );
    assert_eq!(input_of(&args), url);
}

/// `--rendition` takes what a person would type, not only a URL they would
/// have to find. The extension names an exact URL because the popup listed
/// them; nobody at a terminal has that.
#[test]
fn a_rendition_selector_reads_the_words_a_person_would_type() {
    use downer::scraper::RenditionChoice;

    assert_eq!("best".parse(), Ok(RenditionChoice::Best));
    assert_eq!("WORST".parse(), Ok(RenditionChoice::Worst));
    assert_eq!("720p".parse(), Ok(RenditionChoice::Height(720)));
    assert_eq!("720".parse(), Ok(RenditionChoice::Height(720)));
    assert_eq!("1280x720".parse(), Ok(RenditionChoice::Height(720)));
    assert_eq!(
        "https://cdn.test/v/high.m3u8".parse(),
        Ok(RenditionChoice::Exact(
            url::Url::parse("https://cdn.test/v/high.m3u8").unwrap()
        ))
    );
    // Not a scheme a download may use, so not a URL this accepts.
    assert!("file:///etc/passwd".parse::<RenditionChoice>().is_err());
    assert!("medium".parse::<RenditionChoice>().is_err());
}

#[test]
fn a_rendition_selector_picks_by_height_and_by_worst() {
    let origin = HeaderRecorder::start("127.0.0.1");
    let low = origin.url("/media/low/video.m3u8");
    let high = origin.url("/media/high/video.m3u8");
    let master = format!(
        "#EXTM3U\n\
         #EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=320x180\n{low}\n\
         #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720\n{high}\n"
    );
    origin.route(
        "/media/master.m3u8",
        Reply::text("application/vnd.apple.mpegurl", master),
    );
    let url = origin.url("/media/master.m3u8");

    for (selector, expected) in [
        ("best", &high),
        ("worst", &low),
        ("720p", &high),
        ("180p", &low),
    ] {
        let args = argv_for_choice(&url, None, Some(selector)).expect("the fake download runs");
        assert_eq!(&input_of(&args), expected, "--rendition {selector}");
    }

    // A height the master does not declare is refused, like any other
    // rendition that is not on offer.
    let error = argv_for_choice(&url, None, Some("1440p")).expect_err("not offered");
    assert!(error.to_string().contains("1440p"), "{error}");
}
