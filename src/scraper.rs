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

pub fn hls_info(
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
) -> Option<HlsInfo> {
    hls_info_with_timeout(url, user_agent, referer, cookie, Duration::from_secs(8))
}

pub fn hls_info_with_timeout(
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
    timeout: Duration,
) -> Option<HlsInfo> {
    let client = Client::builder()
        .redirect(Policy::limited(10))
        .connect_timeout(timeout.min(Duration::from_secs(4)))
        .timeout(timeout)
        .build()
        .ok()?;
    if let Some(info) = hls_info_from_playlist(&client, url, user_agent, referer, cookie, None) {
        return Some(info);
    }

    let parent = url.join("../playlist.m3u8").ok()?;
    if parent == *url {
        return None;
    }
    hls_info_from_playlist(&client, &parent, user_agent, referer, cookie, Some(url))
}

fn hls_info_from_playlist(
    client: &Client,
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
    preferred_variant: Option<&Url>,
) -> Option<HlsInfo> {
    let (playlist, base_url) = fetch_hls_playlist(client, url, user_agent, referer, cookie)?;
    let playlist = if playlist
        .lines()
        .any(|line| line.trim().starts_with("#EXT-X-STREAM-INF:"))
    {
        let variant = select_variant(&playlist, &base_url, preferred_variant)?;
        fetch_hls_playlist(client, &variant, user_agent, referer, cookie)
            .or_else(|| fetch_hls_playlist(client, &variant, user_agent, Some(&base_url), cookie))?
            .0
    } else {
        playlist
    };

    parse_hls_info(&playlist)
}

fn fetch_hls_playlist(
    client: &Client,
    url: &Url,
    user_agent: &str,
    referer: Option<&Url>,
    cookie: Option<&str>,
) -> Option<(String, Url)> {
    let user_agent = HeaderValue::from_str(user_agent).ok()?;
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
    let response = request.send().ok()?;
    let base_url = response.url().clone();
    response
        .error_for_status()
        .ok()?
        .text()
        .ok()
        .map(|text| (text, base_url))
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

fn is_playlist_url(url: &Url) -> bool {
    let value = url.as_str().to_ascii_lowercase();
    value.contains(".m3u8") || value.contains(".mpd")
}

fn is_media_url(url: &Url) -> bool {
    let value = url.as_str().to_ascii_lowercase();
    [
        ".m3u8", ".mpd", ".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi", ".flv", ".ts", ".mpeg",
        ".mpg", ".ogg", ".ogv", ".3gp",
    ]
    .iter()
    .any(|extension| value.contains(extension))
}

#[cfg(test)]
mod tests {
    use super::*;

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
