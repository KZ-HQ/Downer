//! Does pairing a rendition with its audio actually produce both streams?
//!
//! ADR-0010 declined to resolve a master that declares audio separately,
//! because handing FFmpeg the video variant alone silently loses the sound.
//! ADR-0014 pairs them instead. That claim is about what FFmpeg *does*, so a
//! fake FFmpeg cannot check it: `tests/hls_variant.rs` pins the argv, and this
//! pins what comes out the other end.
//!
//! # Result (FFmpeg 9.0.2, linux/x86_64)
//!
//! ```text
//! master (ADR-0010's behaviour)          11 segment requests   video 1280x720 + audio
//! the video variant alone                 4 segment requests   video only
//! variant + audio, -map 0:v:0 -map 1:a:0  9 segment requests   video 1280x720 + audio
//! the low variant + audio                 9 segment requests   video  320x180 + audio
//! ```
//!
//! The last row is the one that matters for a picker: a rendition that is not
//! the best, with sound. No route could reach it before.
//!
//! # Running it
//!
//! FFmpeg is deliberately not installed in CI (`AGENTS.md`), so this **skips**
//! there, printing a line beginning `SKIP:`. A green CI run is not evidence.
//!
//! ```sh
//! cargo test --test separate_audio -- --nocapture
//! ```

mod support;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use downer::{
    ffmpeg::FfmpegInvocation,
    scraper::{parse_playlist, MasterPlaylist, Playlist, RenditionChoice},
};
use support::{HeaderRecorder, Reply};
use url::Url;

/// The same discovery `tests/cookie_scope.rs` uses, and the same loud skip.
///
/// `make extension-ffmpeg` installs one into `/opt/downer-browser`; the HLS
/// options this project passes need 7.1 or newer, so a distribution FFmpeg on
/// PATH is often too old and the installed one is preferred when present.
fn real_ffmpeg(test: &str) -> Option<PathBuf> {
    let candidate = std::env::var_os("DOWNER_FFMPEG")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        // `make extension-ffmpeg` installs a current one here. Preferred over
        // PATH because a distribution FFmpeg is often older than this needs.
        .or_else(|| Some(PathBuf::from("/opt/downer-browser/bin/ffmpeg")).filter(|p| p.is_file()))
        .or_else(|| {
            std::env::var_os("PATH")
                .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .map(|directory| directory.join("ffmpeg"))
                .find(|candidate| candidate.is_file())
        });

    let Some(path) = candidate else {
        eprintln!(
            "SKIP: {test} needs a real FFmpeg {}+. Run `make extension-ffmpeg`, or point \
             DOWNER_FFMPEG at one, and re-run: cargo test --test separate_audio -- --nocapture",
            downer::ffmpeg::MINIMUM_FFMPEG
        );
        return None;
    };

    // Version-checked, unlike the other skipping suites, because this one
    // passes the HLS leniency options — and below 7.1 they do not exist, so an
    // older FFmpeg dies at argument parsing (KEI-81). That would read as a
    // failure of the behaviour under test rather than of the toolchain.
    if let Some(version) = downer::unsupported_ffmpeg(&path) {
        eprintln!(
            "SKIP: {test} needs FFmpeg {} or newer; {} is {version}. Run \
             `make extension-ffmpeg` and re-run with --nocapture",
            downer::ffmpeg::MINIMUM_FFMPEG,
            path.display()
        );
        return None;
    }
    Some(path)
}

fn run(program: &Path, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// One HLS rendition, packaged for real: a playlist plus its segment bytes.
struct Packaged {
    playlist: String,
    segments: Vec<(String, Vec<u8>)>,
}

/// Encode `source` and cut it into one-second HLS segments.
fn package(ffmpeg: &Path, directory: &Path, name: &str, source: &[&str]) -> Option<Packaged> {
    let input = directory.join(format!("{name}.mp4"));
    let mut encode: Vec<&str> = vec!["-y", "-loglevel", "error"];
    encode.extend_from_slice(source);
    let input_path = input.to_string_lossy().to_string();
    encode.push(&input_path);
    if !run(ffmpeg, &encode) {
        return None;
    }

    let playlist = directory.join(format!("{name}.m3u8"));
    let pattern = directory.join(format!("{name}-%d.ts"));
    if !run(
        ffmpeg,
        &[
            "-y",
            "-loglevel",
            "error",
            "-i",
            &input_path,
            "-c",
            "copy",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_playlist_type",
            "vod",
            "-hls_list_size",
            "0",
            "-hls_segment_filename",
            &pattern.to_string_lossy(),
            &playlist.to_string_lossy(),
        ],
    ) {
        return None;
    }

    let text = std::fs::read_to_string(&playlist).ok()?;
    let mut segments = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        segments.push((line.to_string(), std::fs::read(directory.join(line)).ok()?));
    }
    Some(Packaged {
        playlist: text,
        segments,
    })
}

/// Serve a packaged rendition under `prefix`, and answer with its playlist URL.
fn publish(origin: &HeaderRecorder, prefix: &str, packaged: &Packaged) -> String {
    for (name, bytes) in &packaged.segments {
        origin.route(
            &format!("{prefix}/{name}"),
            Reply::bytes("video/mp2t", bytes.clone()),
        );
    }
    origin.route(
        &format!("{prefix}/index.m3u8"),
        Reply::text("application/vnd.apple.mpegurl", packaged.playlist.clone()),
    );
    origin.url(&format!("{prefix}/index.m3u8"))
}

/// The streams `ffprobe` finds, as `codec_type` strings.
fn streams(ffmpeg: &Path, file: &Path) -> Vec<String> {
    let probe = ffmpeg.with_file_name("ffprobe");
    let probe = if probe.is_file() {
        probe
    } else {
        return vec![];
    };
    let output = Command::new(probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().trim_end_matches(',').to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Build the whole fixture: two video-only renditions and a separate audio
/// rendition, declared the way a real packager declares them.
fn build(
    ffmpeg: &Path,
    directory: &Path,
    origin: &HeaderRecorder,
) -> Option<(Url, String, String)> {
    let high = package(
        ffmpeg,
        directory,
        "high",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=1280x720:rate=15:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "15",
            "-an",
        ],
    )?;
    let low = package(
        ffmpeg,
        directory,
        "low",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x180:rate=15:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "15",
            "-an",
        ],
    )?;
    let audio = package(
        ffmpeg,
        directory,
        "audio",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:a",
            "aac",
            "-b:a",
            "64k",
            "-vn",
        ],
    )?;

    let high_url = publish(origin, "/high", &high);
    let low_url = publish(origin, "/low", &low);
    let audio_url = publish(origin, "/audio", &audio);

    // Lowest variant first, as KEI-89 found in the field.
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:4\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,LANGUAGE=\"en\",URI=\"{audio_url}\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=320x180,CODECS=\"avc1.42c00d,mp4a.40.2\",AUDIO=\"aud\"\n{low_url}\n\
         #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\",AUDIO=\"aud\"\n{high_url}\n"
    );
    origin.route(
        "/master.m3u8",
        Reply::text("application/vnd.apple.mpegurl", master),
    );
    Some((
        Url::parse(&origin.url("/master.m3u8")).expect("a valid URL"),
        high_url,
        low_url,
    ))
}

/// Download `choice` from the master and report the streams written.
fn download(
    ffmpeg: &Path,
    master: &MasterPlaylist,
    choice: Option<&RenditionChoice>,
    output: &Path,
) -> Vec<String> {
    let chosen = master.choose(choice).expect("the master offers it");
    let invocation = FfmpegInvocation {
        program: ffmpeg.to_path_buf(),
        input: chosen.video.to_string(),
        audio_input: chosen.audio.as_ref().map(ToString::to_string),
        headers: None,
        cookies: None,
        reconnect: None,
        hls_lenient: true,
        threads: None,
        overwrite: true,
        output: output.to_path_buf(),
        reporting: downer::ffmpeg::Reporting::Cli,
    };
    downer::ffmpeg::execute(&invocation).expect("the download runs");
    streams(ffmpeg, output)
}

#[test]
fn a_chosen_rendition_arrives_with_its_separately_declared_audio() {
    let Some(ffmpeg) = real_ffmpeg("a_chosen_rendition_arrives_with_its_separately_declared_audio")
    else {
        return;
    };
    let temp = tempfile::tempdir().expect("a temporary directory");
    let origin = HeaderRecorder::start("127.0.0.1");
    let Some((master_url, high_url, low_url)) = build(&ffmpeg, temp.path(), &origin) else {
        eprintln!("SKIP: this FFmpeg could not build the HLS fixture (needs libx264 and aac)");
        return;
    };

    let text = support::get(master_url.as_str(), None).1;
    let Playlist::Master(master) = parse_playlist(&text, &master_url) else {
        panic!("the fixture is a master playlist");
    };

    // 1. The defect ADR-0010 guarded against: the variant alone loses the audio.
    let alone = {
        let invocation = FfmpegInvocation {
            program: ffmpeg.clone(),
            input: high_url.clone(),
            audio_input: None,
            headers: None,
            cookies: None,
            reconnect: None,
            hls_lenient: true,
            threads: None,
            overwrite: true,
            output: temp.path().join("alone.mp4"),
            reporting: downer::ffmpeg::Reporting::Cli,
        };
        downer::ffmpeg::execute(&invocation).expect("the download runs");
        streams(&ffmpeg, &temp.path().join("alone.mp4"))
    };
    if alone.is_empty() {
        eprintln!("SKIP: no ffprobe beside this FFmpeg, so the streams cannot be read");
        return;
    }
    assert_eq!(alone, ["video"], "ADR-0010's measurement still holds");

    // 2. ADR-0014: pairing the same variant with its audio writes both.
    let paired = download(&ffmpeg, &master, None, &temp.path().join("paired.mp4"));
    assert_eq!(paired, ["video", "audio"], "the sound is not lost");

    // 3. The choice: a rendition that is not the best, still with sound.
    origin.clear();
    let chosen = download(
        &ffmpeg,
        &master,
        Some(&RenditionChoice::Worst),
        &temp.path().join("worst.mp4"),
    );
    assert_eq!(chosen, ["video", "audio"]);

    // And it fetched only the rendition asked for. This is the bandwidth claim
    // ADR-0010 made and ADR-0014 keeps: choosing does not cost the other one.
    let fetched = origin.requests();
    let segments = |prefix: &str| {
        fetched
            .iter()
            .filter(|request| request.path.starts_with(prefix) && request.path.ends_with(".ts"))
            .count()
    };
    assert!(segments("/low") > 0, "the chosen rendition was fetched");
    assert_eq!(
        segments("/high"),
        0,
        "the rendition that was not chosen was not fetched: {:?}",
        fetched.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    assert!(segments("/audio") > 0, "and its audio was");
    assert!(low_url.contains("/low/"), "sanity: the fixture is wired up");
}
