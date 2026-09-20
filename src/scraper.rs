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
    Master {
        /// The rendition a download should use, absent when resolving would be
        /// unsafe — a master carrying audio as a separate rendition loses it if
        /// only the video variant is taken (ADR-0010).
        variant: Option<Url>,
    },
    /// Lists segments: how many, and how long altogether.
    Media(HlsInfo),
    /// Not a playlist this can use — no variants and no segments. A challenge
    /// page and an empty playlist both land here; the caller says which it was
    /// from what it fetched.
    Unusable,
}

/// Read a playlist that somebody else fetched.
///
/// `base_url` is the URL the text came from, after redirects, because variant
/// URIs are relative to it.
pub fn parse_playlist(text: &str, base_url: &Url) -> Playlist {
    if is_master_playlist(text) {
        return Playlist::Master {
            variant: if declares_separate_renditions(text) {
                None
            } else {
                select_variant(text, base_url, None)
            },
        };
    }
    match parse_hls_info(text) {
        Some(info) => Playlist::Media(info),
        None => Playlist::Unusable,
    }
}

/// The media playlist an HLS input actually means.
///
/// `Some` only when `url` is a **master** playlist: the chosen variant's URL,
/// by the same highest-bandwidth rule the segment count uses, so the rendition
/// downloaded is the rendition counted.
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
) -> Option<Url> {
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
        Playlist::Master { variant } => variant,
        Playlist::Media(_) | Playlist::Unusable => None,
    }
}

/// Does this master serve any rendition as its own playlist, outside the
/// variant streams?
///
/// `#EXT-X-MEDIA` with a `URI` is how a master declares audio (or subtitles)
/// carried separately from the video, which a `#EXT-X-STREAM-INF` then
/// references by group. Resolving such a master to its video variant would hand
/// FFmpeg the video alone and **silently lose the audio** — measured: the master
/// yields video+audio, the variant alone yields video only.
///
/// `select_variant` reads only `#EXT-X-STREAM-INF`, so it cannot express "this
/// one plus that audio". Rather than guess, this declines to resolve and the
/// master is used as before: the download is wasteful, which is this
/// optimisation's own problem, instead of wrong, which would be a new one. See
/// `docs/adr/0010-resolve-hls-master-playlists.md`.
fn declares_separate_renditions(playlist: &str) -> bool {
    playlist.lines().any(|line| {
        let line = line.trim();
        line.starts_with("#EXT-X-MEDIA:") && line.contains("URI=")
    })
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
        let variant = select_variant(&playlist, &base_url, preferred_variant)
            .ok_or(PlaylistProblem::NoSegments)?;
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

fn select_variant(playlist: &str, base_url: &Url, preferred_variant: Option<&Url>) -> Option<Url> {
    let mut bandwidth = 0_u64;
    let mut selected = None;
    let mut preferred = None;
    for line in playlist.lines() {
        let line = line.trim();
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            bandwidth = attributes
                .split(',')
                .find_map(|attribute| attribute.strip_prefix("BANDWIDTH="))
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
        } else if !line.starts_with('#') && !line.is_empty() {
            let candidate = base_url.join(line).ok()?;
            if preferred_variant.is_some_and(|variant| variant == &candidate) {
                preferred = Some(candidate.clone());
            }
            if selected
                .as_ref()
                .is_none_or(|(_, current)| bandwidth > *current)
            {
                selected = Some((candidate, bandwidth));
            }
        }
    }
    preferred.or_else(|| selected.map(|(url, _)| url))
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

/// Substring, not suffix: query strings and CDN path segments routinely follow
/// the extension, and a suffix test would miss every signed URL.
fn has_extension(url: &Url, extensions: &[&str]) -> bool {
    let value = url.as_str().to_ascii_lowercase();
    extensions.iter().any(|extension| value.contains(extension))
}

fn is_playlist_url(url: &Url) -> bool {
    has_extension(url, &PLAYLIST_EXTENSIONS)
}

fn is_media_url(url: &Url) -> bool {
    has_extension(url, &MEDIA_EXTENSIONS)
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

    #[test]
    fn hls_variant_selection_tolerates_indentation() {
        let playlist = MASTER_PLAYLIST;
        let base = Url::parse("https://example.test/master.m3u8").unwrap();
        assert_eq!(
            select_variant(playlist, &base, None).unwrap().as_str(),
            "https://example.test/high/video.m3u8"
        );
        let preferred = Url::parse("https://example.test/low/video.m3u8").unwrap();
        assert_eq!(
            select_variant(playlist, &base, Some(&preferred))
                .unwrap()
                .as_str(),
            preferred.as_str()
        );
    }
}
