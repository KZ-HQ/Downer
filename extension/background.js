const NATIVE_HOST = "com.downer.native";
const MAX_SAVED_JOBS = 20;
const MAX_LOG_LINES = 500;
const jobs = new Map();
const nativeTasks = new Map();
/**
 * Jobs begun since this background script loaded. Persisted jobs restored from
 * `storage.local` are deliberately not in here, so the popup can tell a live
 * download from history left by an earlier session (see extension/job-view.js).
 */
const sessionJobs = new Set();
let storageWrite = Promise.resolve();

const {
  matchingVariant,
  highestBandwidthVariant,
  parentPlaylistUrl,
  parseHlsInfo
} = DownerHls;

const jobsReady = browser.storage.local.get({ downloadJobs: [] }).then((stored) => {
  for (const job of stored.downloadJobs || []) {
    if (job?.id) jobs.set(job.id, job);
  }
});

async function cookieHeader(url) {
  const cookies = await browser.cookies.getAll({ url });
  return cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("; ");
}

async function fetchPlaylist(url, sourceUrl) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 8000);
  try {
    const response = await fetch(url, {
      credentials: "include",
      cache: "no-store",
      headers: { Accept: "application/vnd.apple.mpegurl, application/x-mpegURL, */*" },
      referrer: sourceUrl || undefined,
      referrerPolicy: "unsafe-url",
      signal: controller.signal
    });
    if (!response.ok) throw new Error("HTTP " + response.status);
    return { text: await response.text(), url: response.url || url };
  } finally {
    clearTimeout(timeout);
  }
}

async function fetchPlaylistFromPage(tabId, url, referrer) {
  if (tabId === undefined || tabId === null) throw new Error("No source tab is available");
  return browser.tabs.sendMessage(tabId, { type: "fetch-playlist", url, referrer });
}

async function fetchPlaylistForSession(tabId, url, referrer, sourceUrl) {
  try {
    return await fetchPlaylistFromPage(tabId, url, referrer);
  } catch (_) {
    return fetchPlaylist(url, referrer || sourceUrl);
  }
}

async function hlsSegmentInfo(url, sourceUrl, tabId) {
  let firstError;
  try {
    let playlist = await fetchPlaylistForSession(tabId, url, sourceUrl, sourceUrl);
    const variant = highestBandwidthVariant(playlist.text, playlist.url);
    if (variant) {
      playlist = await fetchPlaylistForSession(tabId, variant, playlist.url, sourceUrl);
    }
    const info = parseHlsInfo(playlist.text);
    if (info) return info;
  } catch (error) {
    firstError = error;
  }

  const parentUrl = parentPlaylistUrl(url);
  if (parentUrl && parentUrl !== url) {
    try {
      const master = await fetchPlaylistForSession(tabId, parentUrl, sourceUrl, sourceUrl);
      const variant = matchingVariant(master.text, master.url, url);
      const playlist = variant
        ? await fetchPlaylistForSession(tabId, variant, master.url, sourceUrl)
        : master;
      const info = parseHlsInfo(playlist.text);
      if (info) return info;
    } catch (error) {
      firstError = firstError || error;
    }
  }

  if (firstError) throw firstError;
  throw new Error("No HLS segments found in the playlist");
}

function nativeDownload(request, jobId) {
  const port = browser.runtime.connectNative(NATIVE_HOST);
  const channel = new DownerTaskProtocol.NativeTaskChannel(
    port,
    jobId,
    (response) => handleNativeEvent(jobId, response),
    () => browser.runtime.lastError?.message
  );
  nativeTasks.set(jobId, channel);
  const completion = channel.start(request).finally(() => nativeTasks.delete(jobId));
  return { channel, completion };
}

function handleNativeEvent(jobId, response) {
  if (typeof response?.log === "string" && response.log.trim()) {
    appendJobLog(jobId, response.log);
  }
  const progress = {};
  if (response?.completed_segments !== undefined) {
    progress.completedSegments = response.completed_segments;
  }
  if (response?.total_segments !== undefined) {
    progress.totalSegments = response.total_segments;
  }
  if (response?.percent !== undefined) progress.percent = response.percent;
  if (response?.state === "paused") {
    updateJob(jobId, { state: "paused", controlError: null, ...progress });
  } else if (["starting", "preparing"].includes(response?.state)) {
    updateJob(jobId, { state: response.state, controlError: null, ...progress });
  } else if (response?.state === "downloading") {
    updateJob(jobId, {
      state: "downloading",
      controlError: null,
      metadataError: progress.totalSegments ? null : jobs.get(jobId)?.metadataError,
      ...progress
    });
  } else if (response?.state === "cancelling") {
    updateJob(jobId, { state: "cancelling", controlError: null, ...progress });
  } else if (response?.state === "control-error") {
    updateJob(jobId, { controlError: response.error || "Could not control download.", ...progress });
  } else if (Object.keys(progress).length) {
    updateJob(jobId, progress);
  }
}

function appendJobLog(jobId, line) {
  const job = jobs.get(jobId);
  if (!job) return;
  const logs = [...(job.logs || []), { at: Date.now(), text: line }].slice(-MAX_LOG_LINES);
  updateJob(jobId, { logs });
}

async function controlDownload(jobId, command) {
  if (!["pause", "resume", "cancel"].includes(command)) {
    return { ok: false, error: "Unsupported download control." };
  }
  const channel = nativeTasks.get(jobId);
  if (!channel) {
    return { ok: false, error: "Download task is no longer active." };
  }
  return channel.request(command);
}

function saveJobs() {
  storageWrite = storageWrite
    .catch(() => undefined)
    .then(() => browser.storage.local.set({
      downloadJobs: Array.from(jobs.values()).slice(-MAX_SAVED_JOBS)
    }));
  return storageWrite;
}

function broadcast(job) {
  browser.runtime.sendMessage({ type: "download-status", job }).catch(() => undefined);
}

function updateJob(jobId, changes) {
  const job = { ...jobs.get(jobId), ...changes, id: jobId };
  jobs.set(jobId, job);
  broadcast(job);
  void saveJobs();
  return job;
}

async function notify(job) {
  const completed = job.state === "completed";
  const title = completed ? "Downer download complete" : "Downer download failed";
  const cancelled = job.state === "cancelled";
  const message = completed
    ? `Saved to ${job.path}`
    : (cancelled ? "The download was cancelled." : (job.error || "FFmpeg could not download this media."));
  const notificationTitle = cancelled ? "Downer download cancelled" : title;
  try {
    await browser.notifications.create(`downer-${job.id}`, {
      type: "basic",
      title: notificationTitle,
      message
    });
  } catch (_) {
    // Notifications are supplementary; the popup still receives the status.
  }
}

async function runDownload(message, jobId) {
  try {
    updateJob(jobId, { state: "preparing" });
    const settings = await browser.storage.local.get({ outputDir: "", ffmpegThreads: null });
    const cookie = await cookieHeader(message.url);
    let playlistInfo = null;
    if (message.url.toLowerCase().includes(".m3u8")) {
      try {
        playlistInfo = await hlsSegmentInfo(message.url, message.sourceUrl, message.tabId);
      } catch (error) {
        updateJob(jobId, { metadataError: error.message || String(error) });
      }
      if (playlistInfo) {
        updateJob(jobId, {
          completedSegments: 0,
          totalSegments: playlistInfo.totalSegments,
          percent: 0,
          metadataError: null
        });
      }
    }
    const native = nativeDownload({
      command: "download",
      url: message.url,
      source_url: message.sourceUrl,
      output_dir: settings.outputDir || null,
      cookie: cookie || null,
      user_agent: navigator.userAgent,
      overwrite: false,
      threads: Number.isInteger(settings.ffmpegThreads) && settings.ffmpegThreads > 0
        ? settings.ffmpegThreads
        : null,
      total_segments: playlistInfo?.totalSegments,
      total_duration_ms: playlistInfo?.totalDurationMs
    }, jobId);
    const response = await native.completion;
    if (response?.state === "cancelled") {
      const job = updateJob(jobId, {
        state: "cancelled",
        error: response.error || "Download cancelled.",
        finishedAt: Date.now()
      });
      await notify(job);
    } else if (response?.ok) {
      const job = updateJob(jobId, {
        state: "completed",
        path: response.path,
        completedSegments: response.completed_segments ?? jobs.get(jobId)?.totalSegments,
        totalSegments: response.total_segments ?? jobs.get(jobId)?.totalSegments,
        percent: 100,
        finishedAt: Date.now()
      });
      await notify(job);
    } else {
      const job = updateJob(jobId, {
        state: "failed",
        error: response?.error || "Download failed.",
        finishedAt: Date.now()
      });
      await notify(job);
    }
  } catch (error) {
    const job = updateJob(jobId, {
      state: "failed",
      error: error.message || String(error),
      finishedAt: Date.now()
    });
    await notify(job);
  }
}

function beginDownload(message) {
  const jobId = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const job = {
    id: jobId,
    url: message.url,
    state: "starting",
    path: null,
    error: null,
    logs: [],
    startedAt: Date.now()
  };
  jobs.set(jobId, job);
  sessionJobs.add(jobId);
  broadcast(job);
  void saveJobs();
  void runDownload(message, jobId);
  return { ok: true, jobId, state: job.state, path: null, error: null };
}

browser.runtime.onMessage.addListener((message) => {
  if (message?.type === "download-media") {
    return Promise.resolve(beginDownload(message));
  }
  if (message?.type === "get-download-statuses") {
    return jobsReady.then(() => ({
      jobs: Array.from(jobs.values()).slice(-MAX_SAVED_JOBS),
      sessionJobIds: Array.from(sessionJobs)
    }));
  }
  if (message?.type === "control-download") {
    return controlDownload(message.jobId, message.command);
  }
  if (message?.type === "clear-download-logs") {
    for (const job of jobs.values()) updateJob(job.id, { logs: [] });
    return Promise.resolve({ ok: true });
  }
  return undefined;
});
