use regex::Regex;
use reqwest::{
    blocking::Client,
    header::{HeaderValue, ACCEPT, ACCEPT_LANGUAGE, COOKIE, USER_AGENT},
    redirect::Policy,
};
use std::time::Duration;
use url::Url;

use crate::{
    error::{DownerError, DownerResult},
    output::validate_url,
};

#[derive(Debug, Clone)]
pub struct ResolvedMedia {
    pub url: Url,
    pub referer: Option<Url>,
    pub user_agent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsInfo {
    pub total_segments: u64,
    pub total_duration_ms: u64,
}

/// Why a playlist could not be read for its segment totals.
///
/// The probe used to answer `None` for every one of these, which is how a
/// challenge page came to be reported as "no segments found": the diagnosis
/// existed in [`resolve_media`] and nowhere else. They are four different
/// problems with four different remedies, so they are four values. See KEI-86.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistProblem {
    /// A challenge page stood in for the playlist. The media is there; a
    /// browser session is what is missing.
    Challenged,
    /// The request failed, or the server answered with an error status.
    Unreachable(String),
    /// Something came back, and it is not a playlist.
    NotAPlaylist,
    /// A playlist, with no segments in it.
    NoSegments,
}

impl PlaylistProblem {
    /// What to tell the user. Deliberately the same words
    /// [`resolve_media`] already uses for a challenge, because that is the
    /// message they may have seen from the CLI.
    pub fn message(&self) -> String {
        match self {
            Self::Challenged => CHALLENGE_MESSAGE.to_string(),
            Self::Unreachable(reason) => format!("the playlist could not be fetched: {reason}"),
            Self::NotAPlaylist => {
                "the URL answered with something that is not a playlist".to_string()
            }
            Self::NoSegments => "the playlist lists no segments".to_string(),
        }
    }
}

/// The one wording for a challenge, shared by the source-page path and the
/// playlist probe so a user cannot get two different accounts of one problem.
pub const CHALLENGE_MESSAGE: &str =
    "Cloudflare challenge; provide a browser session cookie with --cookie or use a direct media URL";

pub fn hls_info(
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
) -> Result<HlsInfo, PlaylistProblem> {
    hls_info_with_timeout(url, user_agent, referer, cookie, Duration::from_secs(8))
}

pub fn hls_info_with_timeout(
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
    timeout: Duration,
) -> Result<HlsInfo, PlaylistProblem> {
    let client = Client::builder()
        .redirect(Policy::limited(10))
        .connect_timeout(timeout.min(Duration::from_secs(4)))
        .timeout(timeout)
        .build()
        .map_err(|error| PlaylistProblem::Unreachable(error.to_string()))?;
    let problem = match hls_info_from_playlist(&client, url, user_agent, referer, cookie, None) {
        Ok(info) => return Ok(info),
        Err(problem) => problem,
    };

    // The `../playlist.m3u8` fallback, unchanged. If it does not help, the
    // problem reported is the *first* one: the URL the user asked about is the
    // one they need an explanation for, not a sibling this probe guessed at.
    let Ok(parent) = url.join("../playlist.m3u8") else {
        return Err(problem);
    };
    if parent == *url {
        return Err(problem);
    }
    hls_info_from_playlist(&client, &parent, user_agent, referer, cookie, Some(url))
        .map_err(|_| problem)
}

/// What a playlist turned out to be, once parsed.
///
/// This is the single entry point for reading an HLS playlist. The extension
/// fetches playlists — only the page's context carries the session a challenged
/// CDN demands — and hands the text here rather than parsing it itself, so one
/// implementation decides what a playlist means. See
/// `docs/adr/0011-one-playlist-parser.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playlist {
    /// Lists renditions rather than segments.
    Master(MasterPlaylist),
    /// Lists segments: how many, and how long altogether.
    Media(HlsInfo),
    /// Not a playlist this can use — no variants and no segments. A challenge
    /// page and an empty playlist both land here; the caller says which it was
    /// from what it fetched.
    Unusable,
}

/// One `#EXT-X-STREAM-INF` variant stream.
///
/// KEI-61 keeps the whole list rather than only the winner. `select_variant`
/// used to fold the parse and the choice into one pass and return a single URL,
/// so a picker had nothing to offer; the choice is now made over this list by
/// [`MasterPlaylist::choose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendition {
    pub url: Url,
    /// `BANDWIDTH`, or `0` when the master omits it. Required by the HLS
    /// specification, so `0` means a malformed master rather than a free one.
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    /// The `AUDIO` group this variant names, when its audio is carried outside
    /// the variant stream.
    pub audio_group: Option<String>,
}

impl Rendition {
    /// Pixels, for ordering. `None` when the master declares no `RESOLUTION`.
    fn pixels(&self) -> Option<u64> {
        Some(u64::from(self.width?) * u64::from(self.height?))
    }
}

/// One `#EXT-X-MEDIA:TYPE=AUDIO` rendition that names a `URI`.
///
/// Only audio. A `SUBTITLES` rendition is deliberately not modelled: measured
/// on FFmpeg 9.0.2, mapping a WebVTT rendition into an MP4 fails outright
/// (`Could not find tag for codec webvtt`), and a master's own subtitle
/// rendition is not written either — so it is neither usable nor a loss. See
/// `docs/adr/0014-pair-a-rendition-with-its-audio.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRendition {
    pub url: Url,
    pub group_id: String,
    pub name: Option<String>,
    pub language: Option<String>,
    pub default: bool,
}

/// What a download should hand FFmpeg for one chosen rendition.
///
/// `audio` is `Some` only when the master carries the audio outside the video
/// variant, in which case both must be given to FFmpeg together or the audio is
/// silently lost — the defect ADR-0010 guarded against by declining, and which
/// ADR-0014 fixes by pairing instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantChoice {
    pub video: Url,
    pub audio: Option<Url>,
}

/// Which rendition a caller wants, when they want a particular one.
///
/// The extension names an exact URL, because the popup listed them and knows
/// them. A person at a terminal does not, so the CLI's `--rendition` also
/// takes the words and numbers they would actually type. Both land here so one
/// rule resolves them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenditionChoice {
    /// The highest bandwidth — what an unchosen download already gets.
    Best,
    /// The lowest bandwidth the master declares.
    Worst,
    /// A declared `RESOLUTION` height, as in `720p`.
    Height(u32),
    /// An exact variant URL, as the popup and the protocol carry it.
    Exact(Url),
}

impl std::str::FromStr for RenditionChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("best") {
            return Ok(Self::Best);
        }
        if value.eq_ignore_ascii_case("worst") {
            return Ok(Self::Worst);
        }
        // `720p`, `720`, or `1280x720` — whichever the person read off the
        // player. Only the height is matched: it is what distinguishes the
        // renditions anybody chooses between.
        let height = value.trim_end_matches(['p', 'P']);
        let height = height.rsplit_once(['x', 'X']).map_or(height, |(_, h)| h);
        if let Ok(height) = height.parse::<u32>() {
            return Ok(Self::Height(height));
        }
        if let Ok(url) = Url::parse(value) {
            if matches!(url.scheme(), "http" | "https") {
                return Ok(Self::Exact(url));
            }
        }
        Err(format!(
            "expected best, worst, a height such as 720p, or an http(s) variant URL, got {value:?}"
        ))
    }
}

/// A parsed master playlist: every rendition it declares, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterPlaylist {
    /// Sorted by declared bandwidth, then by frame area — **never** by
    /// position. KEI-89 measured that `#EXT-X-STREAM-INF` order is the
    /// publisher's: in the master it found, the first variant was the lowest.
    pub renditions: Vec<Rendition>,
    pub audio: Vec<AudioRendition>,
}

impl MasterPlaylist {
    /// The rendition used when the caller chose none: the highest bandwidth,
    /// which is the rule `select_variant` has always applied, so an unchosen
    /// download is byte-for-byte what it was before (KEI-89's criterion).
    pub fn default_rendition(&self) -> Option<&Rendition> {
        self.renditions.first()
    }

    /// The rendition at `url`, if this master declares one.
    pub fn rendition(&self, url: &Url) -> Option<&Rendition> {
        self.renditions
            .iter()
            .find(|rendition| rendition.url == *url)
    }

    /// The audio to pair with `rendition`, when the master carries it
    /// separately.
    ///
    /// A variant names its group; within that group the `DEFAULT=YES`
    /// rendition wins, and failing that the first declared. That is what a
    /// player does, and — measured — what FFmpeg already picks when handed the
    /// master, so pairing changes which *requests* are made and not which audio
    /// lands in the file.
    pub fn audio_for(&self, rendition: &Rendition) -> Option<&AudioRendition> {
        let group = rendition.audio_group.as_deref()?;
        let mut in_group = self
            .audio
            .iter()
            .filter(|audio| audio.group_id == group)
            .peekable();
        let first = *in_group.peek()?;
        Some(
            self.audio
                .iter()
                .find(|audio| audio.group_id == group && audio.default)
                .unwrap_or(first),
        )
    }

    /// Resolve `requested` — or the default — into the inputs a download needs.
    ///
    /// `None` when this master declares no rendition at all, or when
    /// `requested` names one it does not declare. The second case is refused
    /// rather than quietly replaced with the default: a chosen quality that
    /// silently becomes another one is the illusion KEI-61 exists to remove.
    pub fn choose(&self, requested: Option<&RenditionChoice>) -> Option<VariantChoice> {
        let rendition = match requested {
            None | Some(RenditionChoice::Best) => self.default_rendition()?,
            // The list is sorted best-first, so the worst is the last.
            Some(RenditionChoice::Worst) => self.renditions.last()?,
            Some(RenditionChoice::Height(height)) => self
                .renditions
                .iter()
                .find(|rendition| rendition.height == Some(*height))?,
            Some(RenditionChoice::Exact(url)) => self.rendition(url)?,
        };
        Some(VariantChoice {
            video: rendition.url.clone(),
            audio: self.audio_for(rendition).map(|audio| audio.url.clone()),
        })
    }
}

/// Read a playlist that somebody else fetched.
///
/// `base_url` is the URL the text came from, after redirects, because variant
/// URIs are relative to it.
pub fn parse_playlist(text: &str, base_url: &Url) -> Playlist {
    if is_master_playlist(text) {
        return Playlist::Master(parse_master(text, base_url));
    }
    match parse_hls_info(text) {
        Some(info) => Playlist::Media(info),
        None => Playlist::Unusable,
    }
}

/// Read every `#EXT-X-STREAM-INF` variant and every audio `#EXT-X-MEDIA`.
fn parse_master(text: &str, base_url: &Url) -> MasterPlaylist {
    let mut renditions = Vec::new();
    let mut audio = Vec::new();
    let mut pending: Option<Vec<(String, String)>> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(attributes) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attributes = parse_attributes(attributes);
            if attribute(&attributes, "TYPE")
                .is_none_or(|value| !value.eq_ignore_ascii_case("AUDIO"))
            {
                continue;
            }
            let (Some(uri), Some(group_id)) = (
                attribute(&attributes, "URI"),
                attribute(&attributes, "GROUP-ID"),
            ) else {
                continue;
            };
            let Ok(url) = base_url.join(uri) else {
                continue;
            };
            audio.push(AudioRendition {
                url,
                group_id: group_id.to_string(),
                name: attribute(&attributes, "NAME").map(str::to_string),
                language: attribute(&attributes, "LANGUAGE").map(str::to_string),
                default: attribute(&attributes, "DEFAULT")
                    .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
            });
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(parse_attributes(attributes));
        } else if !line.starts_with('#') && !line.is_empty() {
            // A URI line belongs to the `#EXT-X-STREAM-INF` above it, and to
            // nothing if there was none.
            let Some(attributes) = pending.take() else {
                continue;
            };
            let Ok(url) = base_url.join(line) else {
                continue;
            };
            let (width, height) = attribute(&attributes, "RESOLUTION")
                .and_then(parse_resolution)
                .map_or((None, None), |(w, h)| (Some(w), Some(h)));
            renditions.push(Rendition {
                url,
                bandwidth: attribute(&attributes, "BANDWIDTH")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
                width,
                height,
                codecs: attribute(&attributes, "CODECS").map(str::to_string),
                audio_group: attribute(&attributes, "AUDIO").map(str::to_string),
            });
        }
    }

    // Best first, by what the master *declares*. Sorting is stable, so two
    // renditions a master describes identically keep its order; anything it
    // distinguishes is ordered by that, never by where it was written.
    renditions.sort_by(|left, right| {
        right
            .bandwidth
            .cmp(&left.bandwidth)
            .then(right.pixels().cmp(&left.pixels()))
    });
    MasterPlaylist { renditions, audio }
}

/// Split an HLS attribute list into pairs.
///
/// Splitting on `,` is not enough: `CODECS="avc1.64001f,mp4a.40.2"` carries one
/// inside quotes, and the naive split this replaces read that master's `AUDIO`
/// and `RESOLUTION` as nonsense. Quoted values are returned unquoted.
fn parse_attributes(line: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut quoted = false;
    let mut field = String::new();
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                field.push(character);
            }
            ',' if !quoted => {
                push_attribute(&mut pairs, &field);
                field.clear();
            }
            _ => field.push(character),
        }
    }
    push_attribute(&mut pairs, &field);
    pairs
}

fn push_attribute(pairs: &mut Vec<(String, String)>, field: &str) {
    let Some((name, value)) = field.trim().split_once('=') else {
        return;
    };
    pairs.push((
        name.trim().to_ascii_uppercase(),
        value.trim().trim_matches('"').to_string(),
    ));
}

fn attribute<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// `1280x720` as declared by `RESOLUTION`.
fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
}

/// The media playlist an HLS input actually means.
///
/// `Some` only when `url` is a **master** playlist: the chosen variant's URL,
/// by the same highest-bandwidth rule the segment count uses, so the rendition
/// downloaded is the rendition counted.
///
/// `requested` names a rendition explicitly. `None` takes the default, which
/// is the highest bandwidth as it has always been.
///
/// `None` means "use `url` as given" and covers every other case — a media
/// playlist, a playlist that could not be fetched, one that could not be
/// parsed. That is deliberate. Handing FFmpeg the original URL is exactly what
/// this project did before, so a failure here costs the bandwidth this
/// resolution would have saved and nothing else. A challenged CDN (KEI-87) or a
/// playlist shape nobody anticipated must not turn a working download into a
/// broken one.
///
/// Costs one request. A master playlist lists every variant in one text file,
/// so the count does not grow with the number of renditions, and no media is
/// fetched. See `docs/adr/0010-resolve-hls-master-playlists.md`.
pub fn resolve_variant(
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
    requested: Option<&RenditionChoice>,
) -> Option<VariantChoice> {
    let client = Client::builder()
        .redirect(Policy::limited(10))
        .connect_timeout(VARIANT_CONNECT_TIMEOUT)
        .timeout(VARIANT_TIMEOUT)
        .build()
        .ok()?;
    // A problem here costs the bandwidth this resolution would have saved and
    // nothing else — the download falls back to the URL as given — so it is
    // dropped rather than reported. The *segment count* probe reports its
    // problems, because there the user is owed an explanation for a blank row.
    let (playlist, base_url) =
        fetch_hls_playlist(&client, url, user_agent, referer, cookie).ok()?;
    match parse_playlist(&playlist, &base_url) {
        Playlist::Master(master) => master.choose(requested),
        Playlist::Media(_) | Playlist::Unusable => None,
    }
}

/// Whether a playlist lists variant streams rather than segments.
fn is_master_playlist(playlist: &str) -> bool {
    playlist
        .lines()
        .any(|line| line.trim().starts_with("#EXT-X-STREAM-INF:"))
}

/// Bounded so a slow or hanging playlist host delays a download by seconds
/// rather than stalling it: the fallback is the download we would have run
/// anyway.
const VARIANT_TIMEOUT: Duration = Duration::from_secs(8);
const VARIANT_CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

fn hls_info_from_playlist(
    client: &Client,
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
    preferred_variant: Option<&Url>,
) -> Result<HlsInfo, PlaylistProblem> {
    let (playlist, base_url) = fetch_hls_playlist(client, url, user_agent, referer, cookie)?;
    let playlist = if is_master_playlist(&playlist) {
        // The `../playlist.m3u8` fallback asks the parent master about the URL
        // the user gave: if it lists it, that is the rendition to count, and
        // otherwise the default one.
        let master = parse_master(&playlist, &base_url);
        let preferred = preferred_variant
            .filter(|url| master.rendition(url).is_some())
            .map(|url| RenditionChoice::Exact(url.clone()));
        let variant = master
            .choose(preferred.as_ref())
            .ok_or(PlaylistProblem::NoSegments)?
            .video;
        match fetch_hls_playlist(client, &variant, user_agent, referer, cookie) {
            Ok((text, _)) => text,
            // Retried with the master as Referer, as before.
            Err(problem) => {
                fetch_hls_playlist(client, &variant, user_agent, Some(&base_url), cookie)
                    .map_err(|_| problem)?
                    .0
            }
        }
    } else {
        playlist
    };

    parse_hls_info(&playlist).ok_or_else(|| classify_body(&playlist))
}

/// What a body that yielded no segments actually was.
///
/// Three outcomes the probe used to collapse into one `None`. The challenge
/// check by body text complements the header check in `fetch_hls_playlist`: a
/// challenge served as `200` never reaches that one.
fn classify_body(body: &str) -> PlaylistProblem {
    if !body.trim_start().starts_with("#EXTM3U") {
        if body.to_ascii_lowercase().contains("cloudflare") {
            return PlaylistProblem::Challenged;
        }
        return PlaylistProblem::NotAPlaylist;
    }
    PlaylistProblem::NoSegments
}

fn fetch_hls_playlist(
    client: &Client,
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
) -> Result<(String, Url), PlaylistProblem> {
    let user_agent = HeaderValue::from_str(user_agent)
        .map_err(|_| PlaylistProblem::Unreachable("invalid user agent".to_string()))?;
    let mut request = client
        .get(url.clone())
        .header(USER_AGENT, user_agent)
        .header(
            ACCEPT,
            "application/vnd.apple.mpegurl, application/x-mpegURL, */*",
        );
    if let Some(referer) = referer {
        request = request.header("Referer", referer.as_str());
    }
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    let response = request
        .send()
        .map_err(|error| PlaylistProblem::Unreachable(request_failure(&error)))?;
    let base_url = response.url().clone();
    let status = response.status();
    if !status.is_success() {
        // The same test `resolve_media` makes, in the path that used to throw
        // the answer away.
        if status.as_u16() == 403 && response.headers().get("cf-mitigated").is_some() {
            return Err(PlaylistProblem::Challenged);
        }
        return Err(PlaylistProblem::Unreachable(format!(
            "the server answered {status}"
        )));
    }
    let text = response
        .text()
        .map_err(|error| PlaylistProblem::Unreachable(request_failure(&error)))?;
    Ok((text, base_url))
}

/// A request failure in words, without the URL.
///
/// `reqwest`'s `Display` includes the URL it was fetching, which is exactly the
/// text `docs/adr/0003-redact-urls-in-logs.md` keeps off this wire. The cause
/// is what the user needs; the URL is one they already have.
fn request_failure(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "the request timed out".to_string();
    }
    if error.is_connect() {
        return "the server could not be reached".to_string();
    }
    if error.is_redirect() {
        return "too many redirects".to_string();
    }
    "the request failed".to_string()
}

fn parse_hls_info(playlist: &str) -> Option<HlsInfo> {
    let mut total_segments = 0_u64;
    let mut total_duration_ms = 0_u64;
    for line in playlist.lines() {
        let Some(duration) = line.trim().strip_prefix("#EXTINF:") else {
            continue;
        };
        let Some(duration) = duration
            .split(',')
            .next()
            .and_then(|value| value.parse::<f64>().ok())
        else {
            continue;
        };
        total_segments += 1;
        total_duration_ms += (duration * 1_000.0).round() as u64;
    }
    (total_segments > 0).then_some(HlsInfo {
        total_segments,
        total_duration_ms,
    })
}

pub fn resolve_media(
    raw: &str,
    user_agent: &str,
    cookie: Option<&str>,
) -> DownerResult<ResolvedMedia> {
    let input = validate_url(raw)?;
    if is_media_url(&input) {
        return Ok(ResolvedMedia {
            url: input,
            referer: None,
            user_agent: user_agent.to_string(),
        });
    }

    let client = Client::builder()
        .redirect(Policy::limited(10))
        .build()
        .map_err(|error| DownerError::SourceFetchFailed {
            status: None,
            message: error.to_string(),
        })?;
    let user_agent_header =
        HeaderValue::from_str(user_agent).map_err(|error| DownerError::SourceFetchFailed {
            status: None,
            message: format!("invalid User-Agent: {error}"),
        })?;
    let mut request = client
        .get(input.clone())
        .header(USER_AGENT, user_agent_header)
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }

    let response = request
        .send()
        .map_err(|error| DownerError::SourceFetchFailed {
            status: None,
            message: error.to_string(),
        })?;
    let status = response.status();
    let final_url = response.url().clone();
    if !status.is_success() {
        let message = if status.as_u16() == 403 && response.headers().get("cf-mitigated").is_some()
        {
            "Cloudflare challenge; provide a browser session cookie with --cookie or use a direct media URL".to_string()
        } else {
            format!("server denied the source page with {status}")
        };
        return Err(DownerError::SourceFetchFailed {
            status: Some(status.as_u16()),
            message,
        });
    }

    let html = response
        .text()
        .map_err(|error| DownerError::SourceFetchFailed {
            status: Some(status.as_u16()),
            message: error.to_string(),
        })?;
    let media_url = extract_media_urls(&html, &final_url)
        .into_iter()
        .next()
        .ok_or_else(|| DownerError::MediaNotFound(final_url.to_string()))?;

    Ok(ResolvedMedia {
        url: media_url,
        referer: Some(final_url),
        user_agent: user_agent.to_string(),
    })
}

pub fn extract_media_urls(html: &str, base: &Url) -> Vec<Url> {
    let normalized = html
        .replace("\\\\", "\\")
        .replace("\\/", "/")
        .replace("\\u0026", "&")
        .replace("&amp;", "&");
    let attribute_pattern = Regex::new(
        r#"(?is)(?:src|href|data-src|data-video|data-hls|data-url|data-file|file|video_url)\s*=\s*["']([^"']+)["']"#,
    )
    .expect("media attribute pattern is valid");
    let raw_url_pattern =
        Regex::new(r#"(?i)https?://[^"'\s<>]+"#).expect("media URL pattern is valid");

    let mut urls = Vec::new();
    for captures in attribute_pattern.captures_iter(&normalized) {
        if let Some(url) = captures
            .get(1)
            .and_then(|value| resolve_candidate(value.as_str(), base))
        {
            if is_media_url(&url) && !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    for captures in raw_url_pattern.captures_iter(&normalized) {
        if let Some(url) = captures
            .get(0)
            .and_then(|value| resolve_candidate(value.as_str(), base))
        {
            if is_media_url(&url) && !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    // A segment listed beside its own playlist is not a second thing to
    // download; see `collapse_segments`.
    collapse_segments(&mut urls);
    urls.sort_by_key(|url| if is_playlist_url(url) { 0 } else { 1 });
    urls
}

/// Build the CRLF-delimited header block FFmpeg takes as `-headers`.
///
/// Cookies are deliberately **not** here; see [`ffmpeg_cookies`]. FFmpeg applies
/// `-headers` to every request it makes for an input, so a `Cookie:` line in
/// this block reaches redirect targets and cross-host HLS segment servers too.
///
/// Every value is passed through [`header_value`] first. The block is assembled
/// by concatenation, so a value carrying its own CRLF would otherwise append
/// headers of the caller's choosing — a User-Agent arriving from a page or over
/// the native messaging port must not be able to do that.
pub fn ffmpeg_headers(referer: Option<&Url>, user_agent: &str) -> Option<String> {
    let mut headers = format!("User-Agent: {}\r\n", header_value(user_agent));
    if let Some(referer) = referer {
        headers.push_str(&format!("Referer: {}\r\n", header_value(referer.as_str())));
    }
    Some(headers)
}

/// Render a flat `name=value; name=value` cookie header as the newline-delimited
/// Set-Cookie syntax FFmpeg takes as `-cookies`, scoped to `url`'s host.
///
/// Unlike `-headers`, FFmpeg matches each `-cookies` entry against the host of
/// the request it is about to make, so a redirect target or a cross-host HLS
/// segment server gets nothing. Verified against FFmpeg 9.0.1; see
/// `tests/cookie_scope.rs` and ADR-0002.
///
/// The cookies are already correct for this host: the extension collects them
/// with `browser.cookies.getAll({ url: media.url })`, and the CLI is given a
/// header the user copied for this media. Scoping the lot to the media host is
/// therefore faithful, and strictly tighter than sending them everywhere.
pub fn ffmpeg_cookies(url: &Url, cookie: Option<&str>) -> Option<String> {
    let domain = cookie_domain(url)?;
    let entries: Vec<String> = cookie?
        .split(';')
        .map(str::trim)
        .filter(|pair| is_cookie_pair(pair))
        // `header_value` strips control characters, so a cookie value carrying a
        // newline cannot open an entry of its own with a domain it chose.
        .map(|pair| format!("{}; path=/; domain={domain}", header_value(pair)))
        .collect();
    (!entries.is_empty()).then(|| entries.join("\n"))
}

/// Attribute names from Set-Cookie syntax. They are meaningless in a `Cookie:`
/// request header, which is what this function is given, so a pair named after
/// one of them is either malformed or an injection attempt.
const COOKIE_ATTRIBUTES: [&str; 8] = [
    "domain",
    "expires",
    "httponly",
    "max-age",
    "partitioned",
    "path",
    "samesite",
    "secure",
];

/// Is `pair` a `name=value` cookie rather than a Set-Cookie attribute?
///
/// Entries are built by appending `; path=/; domain=...`, so a pair that is
/// itself named `domain` would put a second `domain=` in the entry — and a
/// cookie *value* containing `;` is enough to smuggle one in. A cookie value
/// may not contain `;` under RFC 6265 and no browser produces one, but the CLI
/// takes an arbitrary string, so the pairs are filtered rather than trusted.
fn is_cookie_pair(pair: &str) -> bool {
    let Some((name, _)) = pair.split_once('=') else {
        return false;
    };
    let name = name.trim();
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_ascii_control())
        && !COOKIE_ATTRIBUTES
            .iter()
            .any(|attribute| name.eq_ignore_ascii_case(attribute))
}

/// The `domain=` value FFmpeg will accept for requests to `url`: its authority
/// **exactly as written**.
///
/// FFmpeg keeps a cookie only when its `domain=` is a suffix of the string
/// `http_open_cnx_internal()` builds with
/// `ff_url_join(hoststr, ..., tmp_host, port, NULL)`, and that call sits *before*
/// the `if (port < 0) port = 443/80` defaulting. So `port` is whatever
/// `av_url_split()` found in the URL — negative when the URL states none, for
/// which `ff_url_join` appends nothing.
///
/// * `http://localhost:60254/v.mp4` → `localhost:60254`
/// * `https://cdn.example.test/v.mp4` → `cdn.example.test`, with no `:443`
///
/// Hence [`Url::port`], which is `None` for a default port, and never
/// `port_or_known_default`. Getting this wrong is silent: FFmpeg makes the
/// request and simply omits the cookie, at any log level.
fn cookie_domain(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Strip the ASCII control characters that would end a header line early, so a
/// value can only ever occupy the header it was placed in. Nothing is logged:
/// the rejected characters never belong in a real header value, and the value
/// itself may be a secret.
fn header_value(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_ascii_control())
        .collect()
}

fn resolve_candidate(raw: &str, base: &Url) -> Option<Url> {
    let raw = raw.trim().trim_end_matches([';', ',']);
    let url = if raw.starts_with("//") {
        Url::parse(&format!("{}:{raw}", base.scheme())).ok()?
    } else {
        base.join(raw).ok()?
    };
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// The extensions that mark a URL as media.
///
/// Mirrored by `MEDIA_EXTENSIONS` in `extension/media-scan.js`; both are
/// asserted against `tests/fixtures/media-extensions.json`, so the two cannot
/// drift without a test failing. See `docs/adr/0011-one-playlist-parser.md`.
pub const MEDIA_EXTENSIONS: [&str; 15] = [
    ".m3u8", ".mpd", ".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi", ".flv", ".ts", ".mpeg",
    ".mpg", ".ogg", ".ogv", ".3gp",
];

/// The subset of [`MEDIA_EXTENSIONS`] that names a playlist rather than a file.
pub const PLAYLIST_EXTENSIONS: [&str; 2] = [".m3u8", ".mpd"];

/// What a URL is, as far as its own shape can say.
///
/// Mirrored by `mediaKind` in `extension/media-scan.js`; the case table in
/// `tests/fixtures/media-extensions.json` is asserted by both suites, so the
/// two cannot drift (ADR-0011's arrangement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// An HLS playlist: a master or a media playlist, not yet distinguished.
    Hls,
    /// A DASH manifest. Listed, but not validated against FFmpeg — see KEI-61.
    Dash,
    /// A media file FFmpeg can be pointed at directly.
    File,
}

/// The final extension of the URL's **path**, lowercased, with its dot.
///
/// The query and fragment are excluded, and only the last extension counts.
/// This replaces a substring test over the whole URL, which matched `.mp4` in
/// `?next=.mp4` and in `poster.mp4.jpg`, and `.ts` in most of a modern site's
/// TypeScript. The signed URLs the old comment worried about still match:
/// `/video.m3u8?token=…` has the extension in its path, which is where an
/// extension lives. See KEI-61.
fn path_extension(url: &Url) -> Option<String> {
    let filename = url.path().rsplit('/').next()?;
    if filename.is_empty() {
        return None;
    }
    let (_, extension) = filename.rsplit_once('.')?;
    (!extension.is_empty()).then(|| format!(".{}", extension.to_ascii_lowercase()))
}

/// Does this filename look like an HLS segment rather than a source file?
///
/// `.ts` is both MPEG-TS and TypeScript, and on a modern site the TypeScript is
/// far more common. A segment is named by a packager and carries an index —
/// `seg-001.ts`, `video32.ts`, `media_1.ts` — where hand-written source does
/// not: `main.ts`, `app.ts`, `index.ts`. A digit in the stem is therefore the
/// test. It is a heuristic and is allowed to be: a `.ts` beside its playlist is
/// collapsed into it by [`collapse_segments`] anyway, so this decides only the
/// rare segment with no playlist in sight.
fn looks_like_segment(filename: &str) -> bool {
    filename
        .rsplit_once('.')
        .is_some_and(|(stem, _)| stem.chars().any(|character| character.is_ascii_digit()))
}

pub fn media_kind(url: &Url) -> Option<MediaKind> {
    let extension = path_extension(url)?;
    match extension.as_str() {
        ".m3u8" => Some(MediaKind::Hls),
        ".mpd" => Some(MediaKind::Dash),
        ".ts" => {
            let filename = url.path().rsplit('/').next().unwrap_or_default();
            looks_like_segment(filename).then_some(MediaKind::File)
        }
        other if MEDIA_EXTENSIONS.contains(&other) => Some(MediaKind::File),
        _ => None,
    }
}

fn is_playlist_url(url: &Url) -> bool {
    matches!(media_kind(url), Some(MediaKind::Hls | MediaKind::Dash))
}

fn is_media_url(url: &Url) -> bool {
    media_kind(url).is_some()
}

/// The part of a path up to and including its last `/`.
fn directory(url: &Url) -> String {
    let path = url.path();
    match path.rfind('/') {
        Some(index) => path[..=index].to_string(),
        None => "/".to_string(),
    }
}

/// Drop segment URLs that belong to a playlist already in the list.
///
/// A player fetches a playlist and then its segments, so `performance` reports
/// both and the popup used to show `seg-001.ts` beside the playlist that lists
/// it. A segment is not separately downloadable in any useful sense, so when
/// the playlist it sits under is present the segment is noise. "Under" is by
/// origin and directory prefix: a master at `/master.m3u8` covers
/// `/high/video1.ts`, and a playlist on another host covers nothing.
pub fn collapse_segments(urls: &mut Vec<Url>) {
    let playlists: Vec<(String, String)> = urls
        .iter()
        .filter(|url| is_playlist_url(url))
        .map(|url| (url.origin().ascii_serialization(), directory(url)))
        .collect();
    if playlists.is_empty() {
        return;
    }
    urls.retain(|url| {
        if path_extension(url).as_deref() != Some(".ts") {
            return true;
        }
        let origin = url.origin().ascii_serialization();
        let folder = directory(url);
        !playlists.iter().any(|(playlist_origin, playlist_folder)| {
            *playlist_origin == origin && folder.starts_with(playlist_folder)
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension lists on both sides come from one place.
    ///
    /// `extension/media-scan.js` keeps its own literal, because a content
    /// script cannot read a repository file at runtime. This is what stops the
    /// two from drifting: the shared vocabulary is the definition, and both
    /// suites check themselves against it. The Node half is in
    /// `tests/extension/media-scan.test.js`.
    #[test]
    fn media_extensions_match_the_shared_vocabulary() {
        const VOCABULARY: &str = include_str!("../tests/fixtures/media-extensions.json");
        let shared: serde_json::Value =
            serde_json::from_str(VOCABULARY).expect("the shared vocabulary is valid JSON");
        let listed = |key: &str| -> Vec<String> {
            shared[key]
                .as_array()
                .unwrap_or_else(|| panic!("{key} is an array"))
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .expect("an extension is a string")
                        .to_string()
                })
                .collect()
        };
        assert_eq!(listed("media_extensions"), MEDIA_EXTENSIONS.to_vec());
        assert_eq!(listed("playlist_extensions"), PLAYLIST_EXTENSIONS.to_vec());
    }

    #[test]
    fn extracts_relative_and_absolute_media_urls_with_playlists_first() {
        let base = Url::parse("https://example.test/watch/123").unwrap();
        let html = r#"
            <video data-src="/video.mp4"></video>
            <script>const stream = "https:\\/\\/cdn.example.test/live.m3u8?token=abc";</script>
            <a href="cover.jpg">cover</a>
        "#;
        let urls = extract_media_urls(html, &base);
        assert_eq!(
            urls[0].as_str(),
            "https://cdn.example.test/live.m3u8?token=abc"
        );
        assert!(urls
            .iter()
            .any(|url| url.as_str() == "https://example.test/video.mp4"));
        assert!(!urls.iter().any(|url| url.as_str().ends_with("cover.jpg")));
    }

    #[test]
    fn builds_ffmpeg_headers_for_discovered_media() {
        let referer = Url::parse("https://example.test/watch/123").unwrap();
        let headers = ffmpeg_headers(Some(&referer), "Test Agent").unwrap();
        assert!(headers.contains("User-Agent: Test Agent\r\n"));
        assert!(headers.contains("Referer: https://example.test/watch/123\r\n"));
        assert!(
            !headers.contains("Cookie"),
            "cookies go to -cookies, never the -headers block: {headers:?}"
        );
    }

    /// The `domain=` value is the URL's authority as written. A default port
    /// must not appear, and a stated port must. Getting this wrong makes FFmpeg
    /// drop the cookie silently, so it is pinned here rather than left to the
    /// FFmpeg-dependent tests in `tests/cookie_scope.rs`, which skip in CI.
    #[test]
    fn scopes_cookies_to_the_url_authority_as_written() {
        let implicit = Url::parse("https://cdn.example.test/video.m3u8").unwrap();
        assert_eq!(
            ffmpeg_cookies(&implicit, Some("sid=downer-sentinel")).unwrap(),
            "sid=downer-sentinel; path=/; domain=cdn.example.test",
            "a default port must not appear in domain="
        );

        let explicit = Url::parse("http://localhost:60254/video.mp4").unwrap();
        assert_eq!(
            ffmpeg_cookies(&explicit, Some("sid=downer-sentinel")).unwrap(),
            "sid=downer-sentinel; path=/; domain=localhost:60254",
            "a stated port must appear in domain="
        );

        // Even when it is the scheme's default, a stated port is part of the
        // authority FFmpeg matches against.
        let stated_default = Url::parse("https://cdn.example.test:443/video.mp4").unwrap();
        assert!(ffmpeg_cookies(&stated_default, Some("sid=downer-sentinel"))
            .unwrap()
            .ends_with("domain=cdn.example.test"));
    }

    /// One entry per cookie: FFmpeg parses `-cookies` as Set-Cookie lines, so a
    /// whole `a=1; b=2` header in one entry would make `b=2` an attribute.
    #[test]
    fn renders_one_cookies_entry_per_cookie() {
        let url = Url::parse("https://cdn.example.test/video.mp4").unwrap();
        let rendered = ffmpeg_cookies(&url, Some("a=1; b=2 ;; c=3")).unwrap();
        assert_eq!(
            rendered.lines().collect::<Vec<_>>(),
            [
                "a=1; path=/; domain=cdn.example.test",
                "b=2; path=/; domain=cdn.example.test",
                "c=3; path=/; domain=cdn.example.test",
            ]
        );

        assert!(ffmpeg_cookies(&url, None).is_none());
        assert!(
            ffmpeg_cookies(&url, Some("   ")).is_none(),
            "a header with no name=value pair renders no -cookies argument"
        );
    }

    /// Entries are newline-delimited, so a cookie value carrying a newline could
    /// otherwise open an entry of its own scoped to a domain it chose.
    #[test]
    fn a_cookie_value_cannot_open_an_entry_of_its_own() {
        let url = Url::parse("https://cdn.example.test/video.mp4").unwrap();
        let rendered = ffmpeg_cookies(
            &url,
            Some("sid=downer-sentinel\nevil=1; path=/; domain=attacker.test"),
        )
        .unwrap();
        assert_eq!(rendered.lines().count(), 1, "{rendered:?}");
        assert!(!rendered.contains("attacker.test"), "{rendered:?}");
        assert!(
            rendered.ends_with("domain=cdn.example.test"),
            "{rendered:?}"
        );
        assert_eq!(rendered.matches("domain=").count(), 1, "{rendered:?}");

        // The same smuggling without a newline: a `;` in a cookie value would
        // otherwise split into pairs named after Set-Cookie attributes.
        let semicolons =
            ffmpeg_cookies(&url, Some("sid=x; domain=attacker.test; path=/; secure")).unwrap();
        assert_eq!(
            semicolons, "sid=x; path=/; domain=cdn.example.test",
            "attribute pairs are dropped, not rendered as cookies"
        );
    }

    /// The header block is concatenated, so a value carrying CRLF would append
    /// headers chosen by whoever supplied it. Sentinel values only: a real
    /// cookie must never appear in a test.
    #[test]
    fn header_values_cannot_inject_extra_header_lines() {
        let headers = ffmpeg_headers(None, "Agent\r\nX-Injected-By-Agent: yes").unwrap();
        // FFmpeg splits the block on CRLF, so the line count is the property
        // that matters. The injected text survives inside the value it was
        // smuggled in, which is inert; it just never becomes a header of its own.
        let lines: Vec<&str> = headers
            .split("\r\n")
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.len(), 1, "one line per supplied header: {lines:?}");
        assert_eq!(
            lines[0], "User-Agent: AgentX-Injected-By-Agent: yes",
            "{lines:?}"
        );
    }

    #[test]
    fn direct_media_urls_skip_page_fetching() {
        let resolved =
            resolve_media("https://cdn.example.test/video.mp4", "Test Agent", None).unwrap();
        assert_eq!(resolved.url.as_str(), "https://cdn.example.test/video.mp4");
        assert!(resolved.referer.is_none());
    }

    /// HTML fixtures shared with the extension's `tests/extension/media-scan.test.js`.
    /// The CLI and the extension must find the same media on the same page; the
    /// attribute lists here and in `extension/media-scan.js` are kept identical,
    /// and these tests fail together if one side drifts.
    const ANCHOR_RELATIVE_PAGE: &str = include_str!("../tests/fixtures/pages/anchor-relative.html");
    const ANCHOR_NON_MEDIA_PAGE: &str =
        include_str!("../tests/fixtures/pages/anchor-non-media.html");
    const ANCHOR_AND_SRC_PAGE: &str = include_str!("../tests/fixtures/pages/anchor-and-src.html");
    const SCRAPER_ATTRIBUTES_PAGE: &str =
        include_str!("../tests/fixtures/pages/scraper-attributes.html");

    fn scanned(html: &str) -> Vec<String> {
        let base = Url::parse("https://example.test/files/index.html").unwrap();
        let mut urls: Vec<String> = extract_media_urls(html, &base)
            .iter()
            .map(|url| url.to_string())
            .collect();
        urls.sort();
        urls
    }

    #[test]
    fn resolves_relative_anchor_links_like_the_extension() {
        assert_eq!(
            scanned(ANCHOR_RELATIVE_PAGE),
            [
                "https://cdn.example.test/promo.mp4",
                "https://example.test/archive/talk.webm",
                "https://example.test/files/media/clip.m3u8",
                "https://example.test/files/movie.mp4",
            ]
        );
    }

    #[test]
    fn ignores_anchors_to_non_media_files() {
        assert!(scanned(ANCHOR_NON_MEDIA_PAGE).is_empty());
    }

    #[test]
    fn deduplicates_media_reached_by_both_src_and_href() {
        assert_eq!(
            scanned(ANCHOR_AND_SRC_PAGE),
            ["https://example.test/media/feature.mp4"]
        );
    }

    #[test]
    fn reads_every_attribute_the_extension_reads() {
        assert_eq!(
            scanned(SCRAPER_ATTRIBUTES_PAGE),
            [
                "https://example.test/a/eight.ogv",
                "https://example.test/a/five.mov",
                "https://example.test/a/four.mkv",
                "https://example.test/a/nine.mpd",
                "https://example.test/a/one.mp4",
                "https://example.test/a/seven.flv",
                "https://example.test/a/six.m4v",
                "https://example.test/a/three.m3u8",
                "https://example.test/a/two.webm",
            ]
        );
    }

    /// Playlist fixtures are shared with the extension's Node tests in
    /// `tests/extension/`, so both parsers stay pinned to the same inputs until
    /// they are consolidated.
    const SEGMENTS_PLAYLIST: &str = include_str!("../tests/fixtures/hls/segments.m3u8");
    const TOLERANT_PLAYLIST: &str = include_str!("../tests/fixtures/hls/segments-tolerant.m3u8");
    const MASTER_PLAYLIST: &str = include_str!("../tests/fixtures/hls/master.m3u8");

    #[test]
    fn counts_hls_segments_and_duration() {
        let playlist = SEGMENTS_PLAYLIST;
        assert_eq!(
            parse_hls_info(playlist),
            Some(HlsInfo {
                total_segments: 2,
                total_duration_ms: 10_506,
            })
        );
    }

    #[test]
    fn hls_metadata_tolerates_indentation_and_bad_duration_lines() {
        let playlist = TOLERANT_PLAYLIST;
        assert_eq!(
            parse_hls_info(playlist),
            Some(HlsInfo {
                total_segments: 2,
                total_duration_ms: 5_500,
            })
        );
    }

    const SEPARATE_AUDIO_MASTER: &str =
        include_str!("../tests/fixtures/hls/master-separate-audio.m3u8");
    const SUBTITLES_ONLY_MASTER: &str =
        include_str!("../tests/fixtures/hls/master-subtitles-only.m3u8");

    fn master_of(text: &str) -> MasterPlaylist {
        let base = Url::parse("https://example.test/master.m3u8").unwrap();
        match parse_playlist(text, &base) {
            Playlist::Master(master) => master,
            other => panic!("expected a master, got {other:?}"),
        }
    }

    #[test]
    fn hls_variant_selection_tolerates_indentation() {
        let master = master_of(MASTER_PLAYLIST);
        assert_eq!(
            master.choose(None).unwrap().video.as_str(),
            "https://example.test/high/video.m3u8"
        );
        let preferred = Url::parse("https://example.test/low/video.m3u8").unwrap();
        assert_eq!(
            master
                .choose(Some(&RenditionChoice::Exact(preferred.clone())))
                .unwrap()
                .video
                .as_str(),
            preferred.as_str()
        );
    }

    /// Publisher order is not quality order. KEI-89 measured a master whose
    /// first variant was its lowest, so position must never decide.
    #[test]
    fn renditions_are_ordered_by_what_the_master_declares_not_by_position() {
        let master = master_of(SEPARATE_AUDIO_MASTER);
        assert_eq!(
            master
                .renditions
                .iter()
                .map(|rendition| rendition.bandwidth)
                .collect::<Vec<_>>(),
            [900_000, 200_000],
            "the fixture lists the low rendition first, on purpose"
        );
        let best = master.default_rendition().unwrap();
        assert_eq!(best.width, Some(1280));
        assert_eq!(best.height, Some(720));
        assert_eq!(best.codecs.as_deref(), Some("avc1.64001f,mp4a.40.2"));
    }

    /// The comma inside `CODECS="…"` used to split the attribute list, which
    /// made every attribute after it unreadable — including `AUDIO`, the one
    /// that decides whether audio is carried separately at all.
    #[test]
    fn a_quoted_comma_does_not_split_the_attribute_list() {
        let master = master_of(SEPARATE_AUDIO_MASTER);
        let best = master.default_rendition().unwrap();
        assert_eq!(best.audio_group.as_deref(), Some("aud"));
    }

    /// The ADR-0014 pairing: a chosen rendition arrives with its audio, so
    /// FFmpeg gets both and the sound is not lost.
    #[test]
    fn a_rendition_is_paired_with_the_default_audio_of_its_group() {
        let master = master_of(SEPARATE_AUDIO_MASTER);
        let chosen = master.choose(None).unwrap();
        assert_eq!(
            chosen.video.as_str(),
            "https://example.test/high/video.m3u8"
        );
        assert_eq!(
            chosen.audio.as_ref().map(Url::as_str),
            Some("https://example.test/audio/en.m3u8"),
            "DEFAULT=YES wins within the group, not declaration order"
        );

        // And the low rendition, which no route could reach with sound before.
        let low = Url::parse("https://example.test/low/video.m3u8").unwrap();
        let chosen = master
            .choose(Some(&RenditionChoice::Exact(low.clone())))
            .unwrap();
        assert_eq!(chosen.video, low);
        assert_eq!(
            chosen.audio.as_ref().map(Url::as_str),
            Some("https://example.test/audio/en.m3u8")
        );
    }

    /// ADR-0014 narrows ADR-0010's guard. A master whose only separately
    /// declared rendition is subtitles resolves normally: measured, the variant
    /// alone writes exactly what the master writes, at half the segments.
    #[test]
    fn a_subtitles_only_master_resolves_without_a_paired_audio() {
        let master = master_of(SUBTITLES_ONLY_MASTER);
        assert!(
            master.audio.is_empty(),
            "subtitles are not modelled as audio"
        );
        let chosen = master.choose(None).unwrap();
        assert_eq!(
            chosen.video.as_str(),
            "https://example.test/high/video.m3u8"
        );
        assert_eq!(chosen.audio, None);
    }

    /// Naming a rendition the master does not declare is refused, not replaced
    /// with the default: a quality that silently becomes another one is the
    /// illusion KEI-61 removes.
    #[test]
    fn a_rendition_the_master_does_not_declare_is_not_silently_replaced() {
        let master = master_of(SEPARATE_AUDIO_MASTER);
        let absent = Url::parse("https://example.test/4k/video.m3u8").unwrap();
        assert_eq!(master.choose(Some(&RenditionChoice::Exact(absent))), None);
    }

    /// The detection table is the shared fixture's, so the Rust and JavaScript
    /// scanners cannot disagree about what counts as media.
    #[test]
    fn media_kinds_match_the_shared_case_table() {
        const VOCABULARY: &str = include_str!("../tests/fixtures/media-extensions.json");
        let shared: serde_json::Value = serde_json::from_str(VOCABULARY).unwrap();
        let cases = shared["detection_cases"]
            .as_array()
            .expect("detection_cases is an array");
        assert!(cases.len() >= 12, "the table is worth having");
        for case in cases {
            let raw = case["url"].as_str().expect("a case has a url");
            let expected = case["kind"].as_str();
            let url = Url::parse(raw).expect("a case URL parses");
            let actual = media_kind(&url).map(|kind| match kind {
                MediaKind::Hls => "hls",
                MediaKind::Dash => "dash",
                MediaKind::File => "file",
            });
            assert_eq!(
                actual,
                expected,
                "{raw}: {}",
                case["why"].as_str().unwrap_or_default()
            );
        }
    }

    /// A segment listed beside the playlist that lists it is not a second
    /// thing to download.
    #[test]
    fn segments_collapse_into_a_playlist_that_covers_them() {
        let mut urls = vec![
            Url::parse("https://cdn.test/v/master.m3u8").unwrap(),
            Url::parse("https://cdn.test/v/high/seg-001.ts").unwrap(),
            Url::parse("https://cdn.test/v/high/seg-002.ts").unwrap(),
            // Another host: this playlist covers nothing here.
            Url::parse("https://other.test/clip-01.ts").unwrap(),
        ];
        collapse_segments(&mut urls);
        assert_eq!(
            urls.iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "https://cdn.test/v/master.m3u8",
                "https://other.test/clip-01.ts"
            ]
        );

        // With no playlist in the list, a segment is all there is, so it stays.
        let mut alone = vec![Url::parse("https://cdn.test/v/seg-001.ts").unwrap()];
        collapse_segments(&mut alone);
        assert_eq!(alone.len(), 1);
    }
}
