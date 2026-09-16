/*
 * HLS playlist helpers shared by the extension background worker.
 *
 * Loaded before background.js by manifest.json, and importable from Node for
 * tests through the same module.exports guard used by task-protocol.js.
 */

var DownerHls = (() => {
  function matchingVariant(text, baseUrl, targetUrl) {
    // Only a master playlist has variants. Without this guard the first segment
    // of a media playlist is mistaken for a variant and refetched as a playlist.
    if (!/^\s*#EXT-X-STREAM-INF:/m.test(text)) return null;
    let bandwidth = -1;
    let selected = null;
    let matching = null;
    let pendingBandwidth = 0;
    for (const rawLine of text.split(/\r?\n/)) {
      const line = rawLine.trim();
      if (line.startsWith("#EXT-X-STREAM-INF:")) {
        // BANDWIDTH may be the first attribute, right after the tag's colon, or
        // follow a comma. Anchoring on either avoids matching AVERAGE-BANDWIDTH.
        pendingBandwidth = Number(
          (line.match(/[:,]BANDWIDTH=(\d+)/) || [])[1] || 0
        );
      } else if (line && !line.startsWith("#")) {
        const candidate = new URL(line, baseUrl).href;
        if (targetUrl && candidate === new URL(targetUrl).href) matching = candidate;
        if (pendingBandwidth > bandwidth) {
          selected = candidate;
          bandwidth = pendingBandwidth;
        }
        pendingBandwidth = 0;
      }
    }
    return matching || selected;
  }

  function highestBandwidthVariant(text, baseUrl) {
    return matchingVariant(text, baseUrl, null);
  }

  function parentPlaylistUrl(url) {
    try {
      return new URL("../playlist.m3u8", url).href;
    } catch (_) {
      return null;
    }
  }

  function parseHlsInfo(text) {
    let totalSegments = 0;
    let totalDurationMs = 0;
    for (const rawLine of text.split(/\r?\n/)) {
      const line = rawLine.trim();
      if (!line.startsWith("#EXTINF:")) continue;
      const duration = Number(line.slice(8).split(",")[0]);
      if (!Number.isFinite(duration)) continue;
      totalSegments += 1;
      totalDurationMs += Math.round(duration * 1000);
    }
    return totalSegments ? { totalSegments, totalDurationMs } : null;
  }

  return {
    matchingVariant,
    highestBandwidthVariant,
    parentPlaylistUrl,
    parseHlsInfo
  };
})();

if (typeof module !== "undefined") module.exports = DownerHls;
