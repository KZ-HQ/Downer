const NATIVE_HOST = "com.downer.native";
const MAX_SAVED_JOBS = 20;

/**
 * The collision policies the native host understands, and the default.
 *
 * Kept identical to `on_conflict_policies` and `default_on_conflict` in
 * `tests/fixtures/protocol.json`, which both test suites read. `rename` is the
 * default because the host always infers a filename into the output directory —
 * it is never given an exact path — so a second download of the same stream
 * lands beside the first instead of failing (KEI-60).
 */
const ON_CONFLICT_POLICIES = ["fail", "rename", "overwrite"];
const DEFAULT_ON_CONFLICT = "rename";

/**
 * Whether a download is named after the page title.
 *
 * Off by default (KEI-84): a generically named playlist becomes `video.mp4`,
 * which is predictable, rather than something derived from whatever the page
 * happened to put in its `<title>`. When it is off the extension simply does
 * not send `title`, so the opt-in needs no protocol field of its own and an
 * older host sees exactly what it saw before.
 */
const DEFAULT_NAME_FROM_TITLE = false;

/**
 * How long storage writes and log broadcasts are coalesced for.
 *
 * FFmpeg's HLS demuxer logs one line per segment at `-loglevel info`, so a
 * 2,000-segment stream used to cause thousands of full-state `storage.local`
 * writes and as many popup re-renders, one per line (KEI-55). Everything within
 * a window is now written and broadcast once. Terminal states bypass the window
 * entirely, so a finished job is durable immediately and the popup never waits
 * to hear that a download has ended.
 */
const SAVE_DEBOUNCE_MS = 300;

const jobs = new Map();
/** Log entries per job, persisted under their own key rather than in the job. */
const logsByJob = new Map();
const nativeTasks = new Map();
/**
 * Jobs begun since this background script loaded. Persisted jobs restored from
 * `storage.local` are deliberately not in here, so the popup can tell a live
 * download from history left by an earlier session (see extension/job-view.js).
 */
const sessionJobs = new Set();
let storageWrite = Promise.resolve();

/** Coalescing state: what the next flush has to write and broadcast. */
let flushTimer = null;
let jobsDirty = false;
const dirtyLogJobs = new Set();
let pendingLogBroadcasts = new Map();

const { canTransition, isTerminal, reconcileRestoredJobs } = DownerJobState;

const { redactText, redactFields } = DownerRedact;

const { trimLogEntries, logStorageKey, jobIdFromLogKey } = DownerJobLogs;

/** The job fields that can carry a URL, and so must be redacted before storage. */
const REDACTED_JOB_FIELDS = ["error", "controlError", "metadataError"];

const {
  matchingVariant,
  highestBandwidthVariant,
  parentPlaylistUrl,
  parseHlsInfo
} = DownerHls;

/**
 * Restore persisted jobs, reconciling any that were still active when the
 * browser closed. Native ports do not survive a restart and `nativeTasks` starts
 * empty, so such a job has no process behind it; leaving it as `downloading`
 * left the popup showing "Downloading…" forever with controls that could only
 * answer "Download task is no longer active." Reconciliation is persisted, so a
 * record is only ever reconciled once.
 */
const jobsReady = browser.storage.local.get(null).then((stored) => {
  const previous = stored?.downloadJobs || [];
  const restored = reconcileRestoredJobs(stored?.downloadJobs);
  let changed = restored.some((job, index) => job !== previous[index]);

  const migrated = new Map();
  for (const job of restored) {
    // KEI-55 moved logs out of the job record. A record written before that
    // still carries them inline, so they are lifted into this session's log
    // store and redacted on the way — which also scrubs lines persisted before
    // redaction existed, rather than leaving them until "Clear logs" is
    // pressed. Both halves of the move are written back below, so it is a
    // one-time migration and not a re-read on every start.
    const { logs, ...record } = job;
    const redacted = redactFields(record, REDACTED_JOB_FIELDS);
    jobs.set(record.id, redacted);
    if (redacted !== record) changed = true;
    if (Array.isArray(logs)) {
      changed = true;
      if (logs.length) {
        migrated.set(record.id, trimLogEntries(logs.map(redactLogEntry)));
      }
    }
  }

  // Logs already under their own key, plus any key left behind by a job that
  // has since fallen off the end of the 20-job history.
  const orphanKeys = [];
  for (const [key, value] of Object.entries(stored || {})) {
    const jobId = jobIdFromLogKey(key);
    if (jobId === null) continue;
    if (!jobs.has(jobId)) {
      orphanKeys.push(key);
    } else if (!migrated.has(jobId) && Array.isArray(value)) {
      logsByJob.set(jobId, trimLogEntries(value));
    }
  }

  for (const [jobId, entries] of migrated) {
    logsByJob.set(jobId, entries);
    dirtyLogJobs.add(jobId);
  }
  if (changed) jobsDirty = true;
  const removal = orphanKeys.length ? removeStorageKeys(orphanKeys) : null;
  const write = changed || dirtyLogJobs.size ? flush() : null;
  return Promise.all([removal, write]).then(() => undefined);
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

/**
 * Connect, complete the `hello` handshake from `docs/protocol.md`, and only then
 * start the download. An incompatible host is refused here, so the job fails with
 * an explanation instead of hanging on a protocol the host does not speak.
 */
async function nativeDownload(request, jobId) {
  const port = browser.runtime.connectNative(NATIVE_HOST);
  const channel = new DownerTaskProtocol.NativeTaskChannel(
    port,
    jobId,
    (response) => handleNativeEvent(jobId, response),
    () => browser.runtime.lastError?.message
  );
  nativeTasks.set(jobId, channel);
  let compatibility;
  try {
    compatibility = DownerTaskProtocol.protocolCompatibility(await channel.hello());
  } catch (error) {
    compatibility = { ok: false, error: error.message || String(error) };
  }
  if (!compatibility.ok) {
    nativeTasks.delete(jobId);
    channel.close(compatibility.error);
    throw new Error(compatibility.error);
  }
  const completion = channel.start(request).finally(() => nativeTasks.delete(jobId));
  return { channel, completion };
}

function handleNativeEvent(jobId, response) {
  if (typeof response?.log === "string" && response.log.trim()) {
    appendJobLog(jobId, response.log);
    // A `log` event carries `state: "downloading"` (docs/protocol.md) and
    // nothing else, so falling through would broadcast a job-state change per
    // line — which is what re-rendered the popup thousands of times on a long
    // HLS download. The job is already `downloading` by the time logs start:
    // the host sends a `progress` event with that state before it runs FFmpeg.
    return;
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
  } else if (["control-error", "rejected"].includes(response?.state)) {
    // Connection-level states describe one request, never the job, so they only
    // surface an explanation and never change the job's state.
    updateJob(jobId, { controlError: response.error || "Could not control download.", ...progress });
  } else if (Object.keys(progress).length) {
    updateJob(jobId, progress);
  }
}

/** Redact one entry's text, whatever shape the entry arrived in. */
function redactLogEntry(entry) {
  return { at: entry?.at || Date.now(), text: redactText(entry?.text ?? "") };
}

/**
 * Record one line of FFmpeg stderr against a job.
 *
 * This no longer goes through `updateJob`: a log line is not a job-state change,
 * and routing it through one meant every line broadcast a full job record to the
 * popup and scheduled a write of all jobs. The line is redacted here as well as
 * on the host (see `extension/redact.js`), then queued for the next flush.
 */
function appendJobLog(jobId, line) {
  if (!jobs.has(jobId)) return;
  const entry = redactLogEntry({ at: Date.now(), text: line });
  logsByJob.set(jobId, trimLogEntries([...(logsByJob.get(jobId) || []), entry]));
  dirtyLogJobs.add(jobId);
  const pending = pendingLogBroadcasts.get(jobId) || [];
  pending.push(entry);
  pendingLogBroadcasts.set(jobId, pending);
  scheduleFlush();
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

/** The job records that are persisted, newest `MAX_SAVED_JOBS` only. */
function savedJobs() {
  return Array.from(jobs.values()).slice(-MAX_SAVED_JOBS);
}

/** Serialise storage work, so a later write cannot overtake an earlier one. */
function writeStorage(work) {
  storageWrite = storageWrite.catch(() => undefined).then(work);
  return storageWrite;
}

function removeStorageKeys(keys) {
  return writeStorage(() => browser.storage.local.remove(keys)).catch(() => undefined);
}

/**
 * Ask for a flush. Within a window every caller joins the same one, so a burst
 * of log lines and progress updates costs a single `storage.local.set`.
 * `immediate` is for terminal states, where durability must not wait.
 */
function scheduleFlush({ immediate = false } = {}) {
  if (immediate) {
    if (flushTimer) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    return flush();
  }
  if (flushTimer) return storageWrite;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    void flush();
  }, SAVE_DEBOUNCE_MS);
  return storageWrite;
}

/**
 * Write everything that changed since the last flush, in one `set`, and send the
 * coalesced log broadcasts.
 *
 * Jobs and logs are separate keys, so a progress update writes `downloadJobs`
 * alone and a run of log lines writes only the key of the job they belong to.
 */
function flush() {
  const values = {};
  let dropped = [];
  if (jobsDirty) {
    jobsDirty = false;
    const saved = savedJobs();
    values.downloadJobs = saved;
    // A job that has fallen off the end of the history takes its log key with
    // it; otherwise the split keys would grow without bound.
    const savedIds = new Set(saved.map((job) => job.id));
    dropped = [...logsByJob.keys()].filter((jobId) => !savedIds.has(jobId));
    for (const jobId of dropped) {
      logsByJob.delete(jobId);
      dirtyLogJobs.delete(jobId);
      pendingLogBroadcasts.delete(jobId);
    }
  }
  for (const jobId of dirtyLogJobs) values[logStorageKey(jobId)] = logsByJob.get(jobId) || [];
  dirtyLogJobs.clear();

  const batches = pendingLogBroadcasts;
  pendingLogBroadcasts = new Map();
  for (const [jobId, entries] of batches) broadcastLogs(jobId, entries);

  if (dropped.length) void removeStorageKeys(dropped.map(logStorageKey));
  if (!Object.keys(values).length) return storageWrite;
  return writeStorage(() => browser.storage.local.set(values));
}

function broadcast(job) {
  browser.runtime.sendMessage({ type: "download-status", job }).catch(() => undefined);
}

/**
 * Log lines go out as their own message, carrying a batch rather than a job.
 * The Settings page re-renders once per batch instead of once per line, and the
 * popup — which listens only for `download-status` — is not woken at all.
 */
function broadcastLogs(jobId, entries) {
  if (!entries.length) return;
  browser.runtime
    .sendMessage({ type: "download-log", jobId, entries })
    .catch(() => undefined);
}

/**
 * Apply changes to a job. A change of `state` must be a legal transition: a
 * terminal job stays terminal, so a late or duplicated native event cannot
 * revive a download that has already finished. Changes that carry no `state`
 * (progress, logs, control errors) are always applied.
 */
function updateJob(jobId, changes) {
  const current = jobs.get(jobId);
  if (changes.state !== undefined && !canTransition(current?.state, changes.state)) {
    console.warn(
      `Downer: ignoring ${current?.state ?? "(new)"} -> ${changes.state} for job ${jobId}`
    );
    const { state: _ignored, ...rest } = changes;
    if (!Object.keys(rest).length) return current;
    changes = rest;
  }
  const job = redactFields({ ...current, ...changes, id: jobId }, REDACTED_JOB_FIELDS);
  jobs.set(jobId, job);
  // State changes are rare and the popup must see them at once, so they are
  // broadcast immediately. Only the storage write is coalesced.
  broadcast(job);
  jobsDirty = true;
  void scheduleFlush({ immediate: isTerminal(job.state) });
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
    const settings = await browser.storage.local.get({
      outputDir: "",
      ffmpegThreads: null,
      onConflict: DEFAULT_ON_CONFLICT,
      nameFromTitle: DEFAULT_NAME_FROM_TITLE
    });
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
    const native = await nativeDownload({
      command: "download",
      url: message.url,
      source_url: message.sourceUrl,
      output_dir: settings.outputDir || null,
      cookie: cookie || null,
      user_agent: navigator.userAgent,
      // Omitted entirely unless the user opted in: absent means "name it
      // video.<ext>", which is the default policy rather than a fallback.
      title:
        settings.nameFromTitle === true &&
        typeof message.title === "string" &&
        message.title.trim()
          ? message.title.trim()
          : null,
      on_conflict: ON_CONFLICT_POLICIES.includes(settings.onConflict)
        ? settings.onConflict
        : DEFAULT_ON_CONFLICT,
      // Superseded by `on_conflict`, still sent so an older host that predates
      // that field keeps its current, strict behaviour rather than silently
      // replacing a file.
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
    startedAt: Date.now()
  };
  jobs.set(jobId, job);
  logsByJob.set(jobId, []);
  sessionJobs.add(jobId);
  broadcast(job);
  jobsDirty = true;
  void scheduleFlush();
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
  if (message?.type === "get-download-logs") {
    return jobsReady.then(() => ({
      logs: Object.fromEntries(
        savedJobs().map((job) => [job.id, logsByJob.get(job.id) || []])
      )
    }));
  }
  if (message?.type === "clear-download-logs") {
    return jobsReady.then(async () => {
      const keys = [...logsByJob.keys()].map(logStorageKey);
      logsByJob.clear();
      dirtyLogJobs.clear();
      pendingLogBroadcasts = new Map();
      for (const job of jobs.values()) logsByJob.set(job.id, []);
      if (keys.length) await removeStorageKeys(keys);
      browser.runtime.sendMessage({ type: "download-logs-cleared" }).catch(() => undefined);
      return { ok: true };
    });
  }
  return undefined;
});
