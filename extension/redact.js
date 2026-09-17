/**
 * URL redaction, shared by everything in the extension that persists, broadcasts
 * or displays text that came from FFmpeg or from a failed fetch.
 *
 * FFmpeg's HLS demuxer logs one `Opening '<url>' for reading` line per segment at
 * `-loglevel info`, and those URLs routinely carry a signed token in their query
 * string. The host forwards each line as a `log` event and the extension used to
 * persist it verbatim until the user pressed "Clear logs". This keeps the part of
 * the URL that makes a log useful — scheme, host, port, path — and throws away
 * the parts that carry credentials.
 *
 * The same rule is implemented in `src/redact.rs`, which runs first, on the host,
 * before the log event is ever emitted. Two layers, because neither covers the
 * other's ground: the host never sees the error strings the extension makes from
 * its own playlist fetches, and the extension is not the only thing that will
 * read the host's output (KEI-64). `tests/fixtures/redaction.json` is the shared
 * case table both implementations are tested against, so they cannot drift.
 * The decision is recorded in `docs/adr/0003-redact-urls-in-logs.md`.
 *
 * Pure and importable from Node, with the same `module.exports` guard as
 * `extension/hls.js`.
 */
var DownerRedact = (() => {
  const QUERY_PLACEHOLDER = "?…";
  const FRAGMENT_PLACEHOLDER = "#…";
  const USERINFO_PLACEHOLDER = "…@";

  /**
   * A long line is a storage problem rather than a secrecy one, but the two meet
   * here: a single pathological line must not be able to fill the per-job byte
   * budget on its own. Truncation happens after redaction, so it can never leave
   * half a token behind.
   */
  const MAX_LINE_CHARS = 2000;
  const TRUNCATION_MARK = "…";

  /**
   * Only `http` and `https`, because those are the only schemes `output.rs`
   * accepts and therefore the only ones this product can be downloading. A
   * `key=value` pair elsewhere in a line is not assumed to be a URL — FFmpeg's
   * own progress vocabulary (`q=-1.0`, `size=…`) is full of them.
   */
  const URL_PATTERN = /https?:\/\/[^\s'"<>\\`|^{}]*/gi;

  /**
   * Characters that end a sentence rather than a URL. FFmpeg and our own error
   * strings both write things like `could not open <url>.`, and absorbing that
   * full stop into the match would put it inside the placeholder.
   */
  const TRAILING_PUNCTUATION = /[.,;:!)\]}>]+$/;

  /**
   * Redact one URL. Returns the input unchanged if it has no authority, which is
   * how a bare `https://` mentioned in prose stays readable.
   */
  function redactUrl(url) {
    const separator = url.indexOf("://");
    if (separator === -1) return url;
    const scheme = url.slice(0, separator + 3);
    const rest = url.slice(separator + 3);
    if (!rest) return url;

    const queryAt = rest.indexOf("?");
    const fragmentAt = rest.indexOf("#");
    let cut = rest.length;
    if (queryAt !== -1) cut = queryAt;
    if (fragmentAt !== -1 && fragmentAt < cut) cut = fragmentAt;
    let head = rest.slice(0, cut);
    if (!head) return url;

    // Userinfo is only userinfo before the first `/`; an `@` inside a path is an
    // ordinary path character.
    const pathAt = head.indexOf("/");
    const authority = pathAt === -1 ? head : head.slice(0, pathAt);
    const path = pathAt === -1 ? "" : head.slice(pathAt);
    if (!authority) return url;
    const at = authority.lastIndexOf("@");
    head = (at === -1 ? authority : USERINFO_PLACEHOLDER + authority.slice(at + 1)) + path;

    let tail = "";
    if (queryAt !== -1) tail += QUERY_PLACEHOLDER;
    if (fragmentAt !== -1) tail += FRAGMENT_PLACEHOLDER;
    return scheme + head + tail;
  }

  /**
   * Redact every URL in a line of arbitrary text, and bound its length.
   * Non-strings are returned as they arrived, so this can be applied to an
   * optional field without checking it first.
   */
  function redactText(text) {
    if (typeof text !== "string" || !text) return text;
    const redacted = text.replace(URL_PATTERN, (match) => {
      const trailing = TRAILING_PUNCTUATION.exec(match);
      const suffix = trailing ? trailing[0] : "";
      const url = suffix ? match.slice(0, -suffix.length) : match;
      return redactUrl(url) + suffix;
    });
    return redacted.length > MAX_LINE_CHARS
      ? redacted.slice(0, MAX_LINE_CHARS - TRUNCATION_MARK.length) + TRUNCATION_MARK
      : redacted;
  }

  /** Redact the string-valued fields of an object, leaving everything else. */
  function redactFields(record, fields) {
    if (!record) return record;
    const changed = {};
    for (const field of fields) {
      if (typeof record[field] !== "string") continue;
      const redacted = redactText(record[field]);
      if (redacted !== record[field]) changed[field] = redacted;
    }
    return Object.keys(changed).length ? { ...record, ...changed } : record;
  }

  return {
    QUERY_PLACEHOLDER,
    FRAGMENT_PLACEHOLDER,
    USERINFO_PLACEHOLDER,
    MAX_LINE_CHARS,
    redactUrl,
    redactText,
    redactFields
  };
})();

if (typeof module !== "undefined") module.exports = DownerRedact;
