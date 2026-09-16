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

pub fn ffmpeg_headers(
    referer: Option<&Url>,
    user_agent: &str,
    cookie: Option<&str>,
) -> Option<String> {
    let mut headers = format!("User-Agent: {user_agent}\r\n");
    if let Some(referer) = referer {
        headers.push_str(&format!("Referer: {referer}\r\n"));
    }
    if let Some(cookie) = cookie {
        headers.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    Some(headers)
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
        let headers = ffmpeg_headers(Some(&referer), "Test Agent", Some("session=abc")).unwrap();
        assert!(headers.contains("User-Agent: Test Agent\r\n"));
        assert!(headers.contains("Referer: https://example.test/watch/123\r\n"));
        assert!(headers.contains("Cookie: session=abc\r\n"));
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
