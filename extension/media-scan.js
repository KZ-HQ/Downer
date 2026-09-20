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
 * `src/scraper.rs::extract_media_urls`, and `mediaKind` the same rule as
 * `src/scraper.rs::media_kind`. The CLI and the extension must find the same
 * media on the same page; a difference between the two is a bug, not a
 * configuration. `tests/fixtures/media-extensions.json` pins both.
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

  /**
   * How good the evidence for a candidate is. Ordered worst-last, which is the
   * order the popup renders in.
   *
   * KEI-61: the popup used to show everything as one flat list, so a URL that
   * merely appeared in the page text ranked beside the stream the player was
   * actually playing.
   */
  const CONFIDENCE = ["observed", "declared", "inferred"];

  /** The part of a path up to and including its last `/`. */
  function directoryOf(url) {
    const path = new URL(url).pathname;
    const at = path.lastIndexOf("/");
    return at === -1 ? "/" : path.slice(0, at + 1);
  }

  /**
   * The final extension of the URL's **path**, lowercased, with its dot.
   *
   * Query and fragment excluded, last extension only. This replaces a substring
   * test over the whole URL, which matched `.mp4` in `?next=.mp4` and in
   * `poster.mp4.jpg`, and `.ts` in most of a modern site's TypeScript. Signed
   * URLs still match, because an extension lives in the path. Mirrors
   * `path_extension` in `src/scraper.rs`.
   */
  function pathExtension(url) {
    let pathname;
    try {
      pathname = new URL(url).pathname;
    } catch (_) {
      return null;
    }
    const filename = pathname.slice(pathname.lastIndexOf("/") + 1);
    if (!filename) return null;
    const at = filename.lastIndexOf(".");
    if (at === -1 || at === filename.length - 1) return null;
    return filename.slice(at).toLowerCase();
  }

  /**
   * Does this filename look like an HLS segment rather than a source file?
   *
   * `.ts` is both MPEG-TS and TypeScript, and on a modern site the TypeScript
   * is far more common. A packager names segments with an index — `seg-001.ts`,
   * `video32.ts` — where hand-written source does not: `main.ts`, `app.ts`. A
   * digit in the stem is the test. Mirrors `looks_like_segment` in
   * `src/scraper.rs`.
   */
  function looksLikeSegment(filename) {
    const at = filename.lastIndexOf(".");
    return at > 0 && /[0-9]/.test(filename.slice(0, at));
  }

  /** `"hls"`, `"dash"`, `"file"`, or `null` for anything that is not media. */
  function mediaKind(url) {
    const extension = pathExtension(url);
    if (!extension) return null;
    if (extension === ".m3u8") return "hls";
    if (extension === ".mpd") return "dash";
    if (extension === ".ts") {
      const pathname = new URL(url).pathname;
      const filename = pathname.slice(pathname.lastIndexOf("/") + 1);
      return looksLikeSegment(filename) ? "file" : null;
    }
    return MEDIA_EXTENSIONS.includes(extension) ? "file" : null;
  }

  /** Kept for callers that only ask "is this media at all?". */
  function isMediaUrl(value) {
    return mediaKind(value) !== null;
  }

  /**
   * Resolve one raw attribute or URL value against the page and record it if it
   * is http(s) media. Deduplicates by resolved URL, keeping the **best**
   * evidence: the same media reached through markup and through a `performance`
   * entry is one candidate, ranked as observed.
   */
  function addCandidate(candidates, value, baseUrl, confidence = "inferred") {
    if (!value || value.startsWith("blob:") || value.startsWith("data:")) {
      return candidates;
    }
    // Mirrors resolve_candidate in src/scraper.rs.
    const raw = value.trim().replace(/[;,]+$/, "");
    let url;
    try {
      url = new URL(raw, baseUrl);
    } catch (_) {
      return candidates; // Ignore malformed and non-URL attributes.
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") return candidates;
    const kind = mediaKind(url.href);
    if (!kind) return candidates;

    const existing = candidates.find((candidate) => candidate.url === url.href);
    if (existing) {
      if (CONFIDENCE.indexOf(confidence) < CONFIDENCE.indexOf(existing.confidence)) {
        existing.confidence = confidence;
      }
      return candidates;
    }
    candidates.push({
      url: url.href,
      kind,
      // `type` is the pre-KEI-61 field name, kept so a popup or test reading it
      // still works; `kind` is the one that distinguishes DASH.
      type: kind === "hls" ? "hls" : "video",
      confidence,
      experimental: kind === "dash"
    });
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
   * Drop segment URLs that belong to a playlist already in the list.
   *
   * A player fetches a playlist and then its segments, so `performance` reports
   * both and the popup used to show `seg-001.ts` beside the playlist that lists
   * it. "Belongs to" is by origin and directory prefix: a master at
   * `/master.m3u8` covers `/high/video1.ts`, and a playlist on another host
   * covers nothing. Mirrors `collapse_segments` in `src/scraper.rs`.
   */
  function collapseSegments(candidates) {
    const playlists = candidates
      .filter((candidate) => candidate.kind === "hls" || candidate.kind === "dash")
      .map((candidate) => {
        const url = new URL(candidate.url);
        return { origin: url.origin, folder: directoryOf(candidate.url) };
      });
    if (!playlists.length) return candidates;
    return candidates.filter((candidate) => {
      if (pathExtension(candidate.url) !== ".ts") return true;
      const url = new URL(candidate.url);
      const folder = directoryOf(candidate.url);
      return !playlists.some(
        (playlist) => playlist.origin === url.origin && folder.startsWith(playlist.folder)
      );
    });
  }

  /**
   * Collect media candidates from a page, best evidence and playlists first.
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
   * A `resourceUrls` entry is `observed` — the player loaded it. A DOM
   * attribute is `declared`. A bare match in the markup text is `inferred`, and
   * the popup collapses those under "Other candidates".
   */
  function collectCandidates({ attributeValues = [], resourceUrls = [], html = "", baseUrl } = {}) {
    let candidates = [];
    for (const value of attributeValues) addCandidate(candidates, value, baseUrl, "declared");
    for (const value of resourceUrls) addCandidate(candidates, value, baseUrl, "observed");

    const markup = normalizeMarkup(html);
    for (const match of markup.matchAll(ATTRIBUTE_PATTERN)) {
      addCandidate(candidates, match[1], baseUrl, "declared");
    }
    for (const match of markup.matchAll(ABSOLUTE_URL_PATTERN)) {
      addCandidate(candidates, match[0], baseUrl, "inferred");
    }

    candidates = collapseSegments(candidates);
    // Playlists first, as before; within that, the best evidence first. Never
    // by discovery order, which is an artefact of which pass ran.
    candidates.sort((left, right) => {
      const playlist = (candidate) => (candidate.kind === "hls" ? 0 : candidate.kind === "dash" ? 1 : 2);
      return (
        playlist(left) - playlist(right) ||
        CONFIDENCE.indexOf(left.confidence) - CONFIDENCE.indexOf(right.confidence)
      );
    });
    return candidates;
  }

  return {
    MEDIA_EXTENSIONS,
    MEDIA_ATTRIBUTES,
    MEDIA_SELECTOR,
    CONFIDENCE,
    mediaKind,
    isMediaUrl,
    addCandidate,
    collapseSegments,
    collectCandidates
  };
})();

if (typeof module !== "undefined") module.exports = DownerMediaScan;
