/**
 * Decides which media candidates the popup lists, and which it folds into
 * "Other candidates".
 *
 * Pure and importable from Node (same `module.exports` guard as
 * `extension/job-view.js`), because `extension/popup.js` touches the DOM and the
 * `browser` global at load and cannot be imported as-is.
 *
 * This is a **rendering** decision. Detection is `extension/media-scan.js`,
 * which is kept identical to `src/scraper.rs::extract_media_urls`; nothing here
 * changes what is found, only what the popup puts on its main list. The CLI's
 * `--list` deliberately keeps showing everything, because listing is what a
 * listing command is for. See ADR-0024.
 */
var DownerCandidateView = (() => {
  const MediaScan = typeof DownerMediaScan !== "undefined"
    ? DownerMediaScan
    : require("./media-scan.js");
  // Best first: "observed", "declared", "inferred". Read from the detection
  // module rather than restated, so the two cannot drift.
  const { CONFIDENCE } = MediaScan;

  /** A playlist is a stream, not one file among several; see `partition`. */
  function isPlaylist(candidate) {
    return candidate.kind === "hls" || candidate.kind === "dash";
  }

  function rank(candidate) {
    const at = CONFIDENCE.indexOf(candidate.confidence);
    return at === -1 ? CONFIDENCE.length : at;
  }

  /**
   * Split candidates into the ones the popup lists and the ones it collapses.
   *
   * **List only the best evidence there is.** If the page actually fetched
   * something, the things it merely mentions are weaker candidates and belong
   * under the disclosure triangle; if it fetched nothing, declared markup is the
   * best there is and gets the main list.
   *
   * This is KEI-98. A `<video>` with an `.mp4` and an `.ogg` `<source>` is one
   * video offered two ways, and the popup listed both as if the page held two
   * videos. Firefox fetched the `.mp4` (`observed`) and never touched the `.ogg`
   * (`declared`), so the evidence already says which one matters — no rule about
   * sibling elements is needed, and none is used. It generalises past
   * `<source>`: a preview clip or an ad left in the markup is demoted for the
   * same reason.
   *
   * Two deliberate limits:
   *
   * - **A playlist is never demoted.** `collectCandidates` already sorts
   *   playlists ahead of files unconditionally, and ranking a declared `.m3u8`
   *   below an observed preview `.mp4` would contradict that — the stream is the
   *   thing the user came for even when the player has not started fetching it.
   * - **Nothing is dropped.** A demoted candidate renders under "Other
   *   candidates", one click away, and is counted in the summary.
   *
   * When nothing has been observed — the popup opened before playback — the best
   * tier present is `declared`, so the split is exactly what it was before this
   * rule existed. The failure mode is "no change", not a wrong answer.
   *
   * @param {object[]} candidates from `media-scan.js::collectCandidates`
   * @returns {{listed: object[], collapsed: object[]}}
   */
  function partition(candidates = []) {
    if (!candidates.length) return { listed: [], collapsed: [] };

    const best = Math.min(...candidates.map(rank));
    const keep = (candidate) => isPlaylist(candidate) || rank(candidate) === best;

    const listed = candidates.filter(keep);
    const collapsed = candidates.filter((candidate) => !keep(candidate));
    return { listed, collapsed };
  }

  /**
   * How the "Other candidates" summary should describe what it hides.
   *
   * Before KEI-98 everything collapsed was a bare string match in the page text,
   * so the summary said so. Now a candidate can also be collapsed for being
   * declared in markup the page never fetched, and calling that "found in the
   * page text" would be wrong.
   */
  function collapsedSummary(collapsed) {
    const count = collapsed.length;
    const noun = `${count} other candidate${count === 1 ? "" : "s"}`;
    return collapsed.every((candidate) => candidate.confidence === "inferred")
      ? `${noun} found in the page text`
      : `${noun} this page did not load`;
  }

  return { isPlaylist, partition, collapsedSummary };
})();

if (typeof module !== "undefined") module.exports = DownerCandidateView;
