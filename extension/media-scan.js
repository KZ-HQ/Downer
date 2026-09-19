/**
 * Media candidate detection for the content script.
 *
 * The scan itself is pure and importable from Node (same `module.exports` guard
 * as `extension/task-protocol.js`), so `tests/extension/media-scan.test.js` can drive it
 * over HTML fixtures. `extension/content.js` is the thin DOM adapter that feeds
 * it the live page; it touches the `browser` global at load and is not
 * importable.
 *
 * `MEDIA_ATTRIBUTES` is deliberately the same list as `attribute_pattern` in
 * `src/scraper.rs::extract_media_urls`. The CLI and the extension must find the
 * same media on the same page; a difference between the two lists is a bug, not
 * a configuration. Keep them in step.
 */
var DownerMediaScan = (() => {
  const MEDIA_EXTENSIONS = [
    ".m3u8", ".mpd", ".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi",
    ".flv", ".ts", ".mpeg", ".mpg", ".ogg", ".ogv", ".3gp"
  ];

  const MEDIA_ATTRIBUTES = [
    "src", "href", "data-src", "data-video", "data-hls", "data-url",
    "data-file", "file", "video_url"
  ];

  /** Selector for the DOM pass, derived from the attribute list above. */
  const MEDIA_SELECTOR = ["video", "video source", "source", "a[href]"]
    .concat(MEDIA_ATTRIBUTES.map((attribute) => `[${attribute}]`))
    .join(", ");

  const ATTRIBUTE_PATTERN = new RegExp(
    `(?:${MEDIA_ATTRIBUTES.join("|")})\\s*=\\s*["']([^"']+)["']`,
    "gi"
  );
  const ABSOLUTE_URL_PATTERN = /https?:\/\/[^"'\s<>]+/gi;

  function isMediaUrl(value) {
    const lower = value.toLowerCase();
    return MEDIA_EXTENSIONS.some((extension) => lower.includes(extension));
  }

  /**
   * Resolve one raw attribute or URL value against the page and record it if it
   * is http(s) media. Deduplicates by resolved URL, so the same media reached
   * through `src` and through `href` is listed once.
   */
  function addCandidate(candidates, value, baseUrl) {
    if (!value || value.startsWith("blob:") || value.startsWith("data:")) {
      return candidates;
    }
    // Mirrors resolve_candidate in src/scraper.rs.
    const raw = value.trim().replace(/[;,]+$/, "");
    try {
      const url = new URL(raw, baseUrl);
      if ((url.protocol === "http:" || url.protocol === "https:") && isMediaUrl(url.href)) {
        if (!candidates.some((candidate) => candidate.url === url.href)) {
          candidates.push({
            url: url.href,
            type: url.href.toLowerCase().includes(".m3u8") ? "hls" : "video"
          });
        }
      }
    } catch (_) {
      // Ignore malformed and non-URL attributes.
    }
    return candidates;
  }

  /** Undo the escaping commonly applied to URLs inside inline scripts. */
  function normalizeMarkup(html) {
    return html
      .replaceAll("\\\\", "\\")
      .replaceAll("\\/", "/")
      .replaceAll("\\u0026", "&")
      .replaceAll("&amp;", "&");
  }

  /**
   * Collect media candidates from a page, HLS playlists first.
   *
   * Every input is optional so this is testable without a DOM:
   * - `attributeValues`: raw attribute values read from the live DOM.
   * - `resourceUrls`: URLs the page actually fetched (`performance` entries).
   * - `html`: page markup, scanned for media attributes and absolute URLs.
   *
   * The markup pass resolves *relative* values against `baseUrl`, which is what
   * `<a href="movie.mp4">` needs: an anchor is not a fetched resource until it
   * is clicked, and the absolute-URL pass alone never saw it.
   *
   * KEI-61 introduces a `confidence` field; anything found only through `href`
   * or markup is weaker evidence than a resource the player actually loaded,
   * and this is where that rank would be assigned.
   */
  function collectCandidates({ attributeValues = [], resourceUrls = [], html = "", baseUrl } = {}) {
    const candidates = [];
    for (const value of attributeValues) addCandidate(candidates, value, baseUrl);
    for (const value of resourceUrls) addCandidate(candidates, value, baseUrl);

    const markup = normalizeMarkup(html);
    for (const match of markup.matchAll(ATTRIBUTE_PATTERN)) {
      addCandidate(candidates, match[1], baseUrl);
    }
    for (const match of markup.matchAll(ABSOLUTE_URL_PATTERN)) {
      addCandidate(candidates, match[0], baseUrl);
    }

    candidates.sort((left, right) => {
      if (left.type === right.type) return 0;
      return left.type === "hls" ? -1 : 1;
    });
    return candidates;
  }

  return {
    MEDIA_EXTENSIONS,
    MEDIA_ATTRIBUTES,
    MEDIA_SELECTOR,
    isMediaUrl,
    addCandidate,
    collectCandidates
  };
})();

if (typeof module !== "undefined") module.exports = DownerMediaScan;
