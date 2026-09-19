/**
 * The bounds on persisted FFmpeg logs, and where they are stored.
 *
 * Logs live under one `downloadLogs:<jobId>` key per job rather than inside the
 * job records in `downloadJobs`. That split is the point: saving a progress
 * update used to rewrite every log line of every job, because they shared one
 * storage key (KEI-55).
 *
 * Pure and importable from Node, with the same `module.exports` guard as
 * `extension/task-protocol.js`, so the caps can be tested without a browser.
 */
var DownerJobLogs = (() => {
  const LOG_KEY_PREFIX = "downloadLogs:";

  /** The line cap AGENTS.md pins. Unchanged by KEI-55. */
  const MAX_LOG_LINES = 500;

  /**
   * A second cap, on bytes, because 500 lines is not a bound on size: one line
   * can be arbitrarily long. 128 KiB per job over at most `MAX_SAVED_JOBS` jobs
   * bounds the whole log history at ~2.5 MiB, which is what stops a long HLS
   * download from filling `storage.local` on its own.
   */
  const MAX_LOG_BYTES = 128 * 1024;

  /**
   * UTF-8 byte length, counted rather than measured, so this file stays free of
   * both DOM and Node APIs and can run identically in either. `TextEncoder`
   * would do, but a log cap is not worth a dependency on which globals happen
   * to exist.
   */
  function utf8Bytes(text) {
    if (typeof text !== "string") return 0;
    let bytes = 0;
    for (let index = 0; index < text.length; index += 1) {
      const code = text.charCodeAt(index);
      if (code < 0x80) bytes += 1;
      else if (code < 0x800) bytes += 2;
      else if (code >= 0xd800 && code <= 0xdbff) {
        // A surrogate pair is one 4-byte character; skip its low half.
        bytes += 4;
        index += 1;
      } else bytes += 3;
    }
    return bytes;
  }

  /** What one stored entry costs: its text plus its JSON envelope and timestamp. */
  function entryBytes(entry) {
    return utf8Bytes(entry?.text) + 32;
  }

  /**
   * Apply both caps, oldest first. The newest entry is always kept even if it
   * alone exceeds the byte cap — dropping the line that just arrived would make
   * a pathological line silently invisible, where keeping it merely makes the
   * next append drop it. `extension/redact.js` truncates individual lines to
   * `MAX_LINE_CHARS` before they get here, so "alone exceeds the cap" is already
   * a bounded amount of damage.
   */
  function trimLogEntries(entries, { maxLines = MAX_LOG_LINES, maxBytes = MAX_LOG_BYTES } = {}) {
    const kept = (entries || []).filter(Boolean).slice(-maxLines);
    let total = kept.reduce((sum, entry) => sum + entryBytes(entry), 0);
    let first = 0;
    while (first < kept.length - 1 && total > maxBytes) {
      total -= entryBytes(kept[first]);
      first += 1;
    }
    return first === 0 ? kept : kept.slice(first);
  }

  function logStorageKey(jobId) {
    return `${LOG_KEY_PREFIX}${jobId}`;
  }

  /** The job id a log key belongs to, or `null` if the key is not a log key. */
  function jobIdFromLogKey(key) {
    return typeof key === "string" && key.startsWith(LOG_KEY_PREFIX)
      ? key.slice(LOG_KEY_PREFIX.length)
      : null;
  }

  return {
    LOG_KEY_PREFIX,
    MAX_LOG_LINES,
    MAX_LOG_BYTES,
    utf8Bytes,
    entryBytes,
    trimLogEntries,
    logStorageKey,
    jobIdFromLogKey
  };
})();

if (typeof module !== "undefined") module.exports = DownerJobLogs;
