//! What a URL offers, listed rather than silently reduced to one download.
//!
//! The CLI has always taken the first of `extract_media_urls` and downloaded
//! it. This is the rest of that list — printed by `--list`, chosen from by
//! `--select` and `--media`, and rendered as JSON by `--json` for a caller that
//! is a script rather than a person. See KEI-62.
//!
//! Nothing here decides what a playlist means or what counts as media: it
//! assembles what `scraper::resolve_source` and `scraper::fetch_playlist`
//! already answer. A second implementation of either is exactly what
//! `docs/adr/0011-one-playlist-parser.md` exists to prevent.

use serde::Serialize;
use url::Url;

use crate::{
    error::{DownerError, DownerResult},
    output::validate_url,
    scraper::{self, MediaKind, Playlist, SourceMedia},
};

/// The version of the `--json` documents this binary writes.
///
/// `--json` is a public interface, not debugging output: a script that reads
/// `path` out of a download result is entitled to keep working. Bumping this is
/// how an incompatible change announces itself, and
/// `docs/adr/0020-json-output-is-a-cli-interface.md` says what counts as one.
/// The name and shape mirror the native protocol's `protocol_version`, since
/// both answer the same question for their own surface.
pub const SCHEMA_VERSION: u32 = 1;

/// The engine that performed a download.
///
/// Constant today — ADR-0015 makes FFmpeg the only one — and reported anyway,
/// because KEI-71 puts a native HLS engine behind a feature flag and a script
/// that has been reading this field all along needs no change when it lands.
pub const ENGINE: &str = "ffmpeg";

/// One thing that could be downloaded.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// Its position in the list, 1-based, as `--list` prints it and `--select`
    /// takes it. 1-based because the number exists to be read off a printed
    /// list and typed back, not to index an array.
    pub index: usize,
    pub url: String,
    /// `hls`, `dash` or `file`, in the extension's words (`MediaKind::as_str`).
    pub kind: &'static str,
    /// DASH is classified but has never been put through FFmpeg end to end
    /// (KEI-61). Flagged so a script can decline it rather than discover that
    /// the hard way. Always present, including when false: a key a consumer can
    /// read unconditionally is worth more than the bytes it saves.
    pub experimental: bool,
    /// What a master playlist declares, best first. Absent for anything that is
    /// not a master: a media playlist and a plain file have nothing to choose
    /// between, and an empty array would suggest the choice existed and was
    /// empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renditions: Option<Vec<CandidateRendition>>,
}

/// One rendition of a master playlist.
///
/// The field names are `RenditionInfo`'s in `src/native.rs`, deliberately: the
/// popup and a shell script are looking at the same thing, and two vocabularies
/// for it would be two things to keep in step.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateRendition {
    pub url: String,
    pub bandwidth: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codecs: Option<String>,
    /// The paired audio, when the master carries it outside the variant. Said
    /// so a caller can tell the download will include it, not so it can be sent
    /// back: the rendition is re-derived at download time (ADR-0011, ADR-0014).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
    /// Whether this is the one an unchosen download takes.
    pub default: bool,
}

/// Everything a `--list` run found.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub schema_version: u32,
    /// Where the candidates were discovered: the source page after redirects,
    /// or the media URL itself when one was given directly.
    pub source: String,
    pub candidates: Vec<Candidate>,
}

/// What a `--json` download run reports when it succeeds.
///
/// There is no JSON *failure* document, on purpose. A failure already has a
/// machine-readable form — the exit code, which
/// `docs/adr/0018-stable-cli-exit-codes.md` makes a stable interface — and the
/// message belongs on stderr where a second contract cannot compete with it.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadReport {
    pub schema_version: u32,
    /// The media URL that was downloaded: the candidate selected, before any
    /// rendition within it was resolved.
    pub url: String,
    pub path: String,
    pub engine: &'static str,
    /// The finished file's size. Absent when it could not be read, which is the
    /// only honest answer for a number nobody measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Wall-clock time the download took. Named `elapsed_ms` rather than
    /// "duration" because the protocol already uses that word for this, and
    /// "duration" on a media tool reads as the media's own length — which is
    /// not what this is.
    pub elapsed_ms: u64,
}

/// Look at what `url` offers, enumerating the renditions of any master playlist
/// among the candidates.
///
/// Costs one request per playlist candidate on top of the source page.
/// `collapse_segments` has usually left exactly one, and the renditions are the
/// point of listing an HLS candidate at all — a row saying only "hls" tells the
/// user nothing they did not already type.
pub fn list(url: &str, user_agent: &str, cookie: Option<&str>) -> DownerResult<Listing> {
    let source = scraper::resolve_source(url, user_agent, cookie)?;
    let candidates = source
        .urls
        .iter()
        .enumerate()
        .map(|(position, candidate)| {
            let kind = scraper::media_kind(candidate)
                .expect("resolve_source only ever returns media URLs");
            Candidate {
                index: position + 1,
                url: candidate.to_string(),
                kind: kind.as_str(),
                experimental: kind == MediaKind::Dash,
                renditions: renditions_of(candidate, kind, &source, user_agent, cookie),
            }
        })
        .collect();
    Ok(Listing {
        schema_version: SCHEMA_VERSION,
        source: source.source().to_string(),
        candidates,
    })
}

/// The renditions `candidate` declares, when it is a master playlist.
///
/// `None` for everything else, including a playlist that could not be fetched
/// or read: an absent list says "nothing to choose between here", which is the
/// truth in all of those cases and is what the download would then do anyway.
fn renditions_of(
    candidate: &Url,
    kind: MediaKind,
    source: &SourceMedia,
    user_agent: &str,
    cookie: Option<&str>,
) -> Option<Vec<CandidateRendition>> {
    if kind != MediaKind::Hls {
        return None;
    }
    let Playlist::Master(master) =
        scraper::fetch_playlist(candidate, user_agent, source.referer.as_ref(), cookie)?
    else {
        return None;
    };
    let default = master
        .default_rendition()
        .map(|rendition| rendition.url.clone());
    Some(
        master
            .renditions
            .iter()
            .map(|rendition| CandidateRendition {
                url: rendition.url.to_string(),
                bandwidth: rendition.bandwidth,
                width: rendition.width,
                height: rendition.height,
                codecs: rendition.codecs.clone(),
                audio_url: master
                    .audio_for(rendition)
                    .map(|audio| audio.url.to_string()),
                default: default.as_ref() == Some(&rendition.url),
            })
            .collect(),
    )
}

/// Pick the candidate a `--select` or `--media` names, or the first when
/// neither was given.
///
/// A choice the source does not offer is **refused**, never replaced with the
/// first — the same rule `DownerError::VariantNotOffered` applies one level
/// down. Downloading something other than what was named is the failure mode
/// both exist to prevent, and a script cannot see that it happened.
pub fn select(
    source: &SourceMedia,
    select: Option<usize>,
    media: Option<&str>,
) -> DownerResult<Url> {
    if let Some(position) = select {
        // 1-based, and `checked_sub` rather than `- 1` because this is a public
        // function: clap rejects `--select 0` before it gets here, but a
        // library caller has made no such promise.
        return position
            .checked_sub(1)
            .and_then(|index| source.urls.get(index))
            .cloned()
            .ok_or_else(|| {
                DownerError::CandidateNotOffered(format!(
                    "--select {position}: this source offers {}; run --list to see them",
                    offer_count(source.urls.len())
                ))
            });
    }
    if let Some(raw) = media {
        // Parsed rather than compared as text, so a malformed or non-http(s)
        // `--media` is invalid input (exit 2) rather than a well-formed URL
        // that merely happens not to be on offer (exit 5).
        let wanted = validate_url(raw)?;
        return source
            .urls
            .iter()
            .find(|candidate| **candidate == wanted)
            .cloned()
            .ok_or_else(|| {
                DownerError::CandidateNotOffered(format!(
                    "--media {wanted}: this source does not offer it; run --list to see what it does"
                ))
            });
    }
    Ok(source.urls[0].clone())
}

fn offer_count(total: usize) -> String {
    match total {
        1 => "1 candidate".to_string(),
        other => format!("{other} candidates"),
    }
}

/// Render a listing for a person reading a terminal.
pub fn listing_text(listing: &Listing) -> String {
    let mut out = format!("Source: {}\n\n", listing.source);
    for candidate in &listing.candidates {
        let kind = if candidate.experimental {
            format!("{} (experimental)", candidate.kind)
        } else {
            candidate.kind.to_string()
        };
        out.push_str(&format!(
            "{:>3}  {:<5}  {}\n",
            candidate.index, kind, candidate.url
        ));
        for rendition in candidate.renditions.iter().flatten() {
            out.push_str(&format!("       {}\n", rendition_line(rendition)));
        }
    }
    out.push_str("\nDownload one with --select N, or name it with --media URL.\n");
    if listing
        .candidates
        .iter()
        .any(|candidate| candidate.renditions.iter().flatten().count() > 1)
    {
        out.push_str("Choose a rendition with --rendition (best, worst, a height such as 720p, or a variant URL).\n");
    }
    out
}

/// One rendition, labelled with what `--rendition` actually accepts.
///
/// The height is printed as `720p` rather than `1280x720` because that is the
/// word `--rendition` takes, so a line of this output can be typed straight
/// back in.
fn rendition_line(rendition: &CandidateRendition) -> String {
    let mut parts = Vec::new();
    match (rendition.height, rendition.width) {
        (Some(height), _) => parts.push(format!("{height}p")),
        (None, Some(width)) => parts.push(format!("{width}px wide")),
        (None, None) => parts.push("unknown size".to_string()),
    }
    if rendition.bandwidth > 0 {
        parts.push(format!("{} kbps", rendition.bandwidth / 1000));
    }
    if let Some(codecs) = &rendition.codecs {
        parts.push(codecs.clone());
    }
    if rendition.default {
        parts.push("(default)".to_string());
    }
    if rendition.audio_url.is_some() {
        parts.push("+ separate audio".to_string());
    }
    parts.join("  ")
}

/// Render any of the documents above.
///
/// Pretty-printed: `--list --json` is read by people at least as often as by
/// `jq`, and neither minds the whitespace.
pub fn to_json<T: Serialize>(value: &T) -> String {
    // These documents are strings, integers and booleans in named structs, so
    // there is no input that makes this fail. `expect` says that, rather than
    // inventing an error case and a exit code for something unreachable.
    serde_json::to_string_pretty(value).expect("discovery documents always serialize")
}
