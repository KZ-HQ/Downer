/* global DownerMediaScan */

// Detection lives in extension/media-scan.js so it can be tested from Node over
// HTML fixtures; this file is the DOM adapter that feeds it the live page.
const { MEDIA_ATTRIBUTES, MEDIA_SELECTOR, collectCandidates } = DownerMediaScan;

function scanMedia() {
  const attributeValues = [];
  for (const element of document.querySelectorAll(MEDIA_SELECTOR)) {
    for (const attribute of MEDIA_ATTRIBUTES) {
      const value = element.getAttribute(attribute);
      if (value) attributeValues.push(value);
    }
  }

  return collectCandidates({
    attributeValues,
    resourceUrls: performance.getEntriesByType("resource").map((entry) => entry.name),
    html: document.documentElement?.outerHTML || "",
    baseUrl: window.location.href
  });
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
