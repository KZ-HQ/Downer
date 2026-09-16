const MEDIA_EXTENSIONS = [
  ".m3u8", ".mpd", ".mp4", ".webm", ".mov", ".m4v", ".mkv", ".avi",
  ".flv", ".ts", ".mpeg", ".mpg", ".ogg", ".ogv", ".3gp"
];

function isMediaUrl(value) {
  const lower = value.toLowerCase();
  return MEDIA_EXTENSIONS.some((extension) => lower.includes(extension));
}

function addCandidate(candidates, value) {
  if (!value || value.startsWith("blob:") || value.startsWith("data:")) {
    return;
  }
  try {
    const url = new URL(value, window.location.href);
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
}

function scanMedia() {
  const candidates = [];
  const elements = document.querySelectorAll("video, video source, source, [src], [data-src], [data-video], [data-hls], [data-url]");
  for (const element of elements) {
    for (const attribute of ["src", "data-src", "data-video", "data-hls", "data-url"]) {
      addCandidate(candidates, element.getAttribute(attribute));
    }
  }

  for (const entry of performance.getEntriesByType("resource")) {
    addCandidate(candidates, entry.name);
  }

  const pageText = document.documentElement?.outerHTML || "";
  const absoluteUrls = pageText.match(/https?:\/\/[^"'\s<>]+/gi) || [];
  for (const value of absoluteUrls) {
    addCandidate(candidates, value.replaceAll("\\/", "/"));
  }

  candidates.sort((left, right) => {
    if (left.type === right.type) return 0;
    return left.type === "hls" ? -1 : 1;
  });
  return candidates;
}

async function fetchPlaylist(url, referrer) {
  const response = await fetch(url, {
    credentials: "include",
    cache: "no-store",
    headers: { Accept: "application/vnd.apple.mpegurl, application/x-mpegURL, */*" },
    referrer: referrer || window.location.href,
    referrerPolicy: "unsafe-url"
  });
  if (!response.ok) throw new Error("HTTP " + response.status);
  return { text: await response.text(), url: response.url || url };
}

browser.runtime.onMessage.addListener((message) => {
  if (message?.type === "fetch-playlist") {
    return fetchPlaylist(message.url, message.referrer);
  }
  if (message?.type !== "scan-media") {
    return undefined;
  }
  return Promise.resolve({
    sourceUrl: window.location.href,
    title: document.title,
    candidates: scanMedia()
  });
});
