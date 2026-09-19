//! KEI-52: the exact argv Downer hands FFmpeg, pinned for the six cases the
//! issue names.
//!
//! These run the real download path against a fake FFmpeg that records its
//! arguments, so what is pinned is what a process actually received rather
//! than what a builder intended. They exist to make the structured command
//! model's output diffable: a change in argument order or spelling shows up
//! here as a failing comparison, not as a subtly different download.

#![cfg(unix)]

use std::{fs, path::Path, path::PathBuf};

use downer::{
    ffmpeg::ProcessControl, output::NamingHints, scraper::ResolvedMedia, DownloadOptions,
};

const USER_AGENT: &str = "downer-test/1.0";
const FAKE_VERSION: &str = "9.0.1";

/// The `-headers` block every case carries, since a user agent is always sent.
fn user_agent_header() -> String {
    format!("User-Agent: {USER_AGENT}\r\n")
}

struct Case {
    url: &'static str,
    referer: Option<&'static str>,
    threads: Option<u16>,
    overwrite: bool,
    controlled: bool,
}

impl Case {
    /// A direct-file download with everything optional left off.
    fn direct() -> Self {
        Self {
            url: "https://example.test/video.mp4",
            referer: None,
            threads: None,
            overwrite: false,
            controlled: false,
        }
    }
}

/// Run one download against a recording fake and return the argv it received,
/// with the (temporary) output path replaced by `<OUTPUT>` so the expectation
/// can be written literally.
fn argv_for(case: Case) -> Vec<String> {
    let temp = tempfile::tempdir().expect("a temporary directory");
    let fake = install_recording_fake(temp.path());
    let output = temp.path().join("out.mp4");
    let media = ResolvedMedia {
        url: url::Url::parse(case.url).expect("a valid URL"),
        referer: case
            .referer
            .map(|referer| url::Url::parse(referer).expect("a valid referer")),
        user_agent: USER_AGENT.to_string(),
    };
    let options = DownloadOptions {
        output: Some(output.clone()),
        dir: None,
        overwrite: case.overwrite,
        on_conflict: None,
        naming: NamingHints::new(None),
        ffmpeg: fake,
        user_agent: USER_AGENT.to_string(),
        cookie: None,
        threads: case.threads,
        quiet: true,
    };

    let control = ProcessControl::new();
    let hooks = if case.controlled {
        downer::Hooks::controlled(&control, |_| {}, |_| {})
    } else {
        downer::Hooks::default()
    };
    downer::download_resolved(media, &options, hooks).expect("the fake download succeeds");

    recorded(temp.path())
        .into_iter()
        .map(|argument| {
            if Path::new(&argument) == output {
                "<OUTPUT>".to_string()
            } else {
                argument
            }
        })
        .collect()
}

#[test]
fn direct_file() {
    assert_eq!(
        argv_for(Case::direct()),
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-stats",
            "-headers",
            &user_agent_header(),
            "-i",
            "https://example.test/video.mp4",
            "-c",
            "copy",
            "-n",
            "<OUTPUT>",
        ]
    );
}

#[test]
fn hls_carries_the_segment_extension_options() {
    assert_eq!(
        argv_for(Case {
            url: "https://example.test/playlist.m3u8",
            ..Case::direct()
        }),
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-stats",
            "-allowed_segment_extensions",
            "ALL",
            "-extension_picky",
            "0",
            "-headers",
            &user_agent_header(),
            "-i",
            "https://example.test/playlist.m3u8",
            "-c",
            "copy",
            "-n",
            "<OUTPUT>",
        ]
    );
}

#[test]
fn a_referer_joins_the_headers_block() {
    assert_eq!(
        argv_for(Case {
            referer: Some("https://example.test/page"),
            ..Case::direct()
        }),
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-stats",
            "-headers",
            &format!(
                "{}Referer: https://example.test/page\r\n",
                user_agent_header()
            ),
            "-i",
            "https://example.test/video.mp4",
            "-c",
            "copy",
            "-n",
            "<OUTPUT>",
        ]
    );
}

#[test]
fn threads_are_an_output_option() {
    assert_eq!(
        argv_for(Case {
            threads: Some(4),
            ..Case::direct()
        }),
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-stats",
            "-headers",
            &user_agent_header(),
            "-i",
            "https://example.test/video.mp4",
            "-c",
            "copy",
            "-threads",
            "4",
            "-n",
            "<OUTPUT>",
        ]
    );
}

#[test]
fn overwrite_replaces_the_refusal_flag() {
    assert_eq!(
        argv_for(Case {
            overwrite: true,
            ..Case::direct()
        }),
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-stats",
            "-headers",
            &user_agent_header(),
            "-i",
            "https://example.test/video.mp4",
            "-c",
            "copy",
            "-y",
            "<OUTPUT>",
        ]
    );
}

/// Controlled mode, as the native host runs it.
///
/// This is the one case whose argv differs from the pre-refactor command, and
/// only by a removal: the old code kept `-loglevel error` from the builder and
/// spliced `-loglevel info` in after it, leaving FFmpeg to resolve the pair by
/// last-one-wins. The mode now renders one log level. Behaviour is unchanged —
/// `info` was, and is, what applies.
#[test]
fn controlled_mode_reports_progress_on_stdout() {
    assert_eq!(
        argv_for(Case {
            controlled: true,
            ..Case::direct()
        }),
        [
            "-hide_banner",
            "-loglevel",
            "info",
            "-nostats",
            "-stats_period",
            "0.5",
            "-progress",
            "pipe:1",
            "-headers",
            &user_agent_header(),
            "-i",
            "https://example.test/video.mp4",
            "-c",
            "copy",
            "-n",
            "<OUTPUT>",
        ]
    );
}

/// A fake FFmpeg that answers `-version`, writes the output file, and records
/// its arguments NUL-separated so header blocks containing CRLF survive.
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
