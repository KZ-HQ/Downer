/* global DownerJobView, DownerJobState, DownerCandidateView */

const { RENDER_STALE, renderableJobs } = DownerJobView;
// Which candidates reach the main list, and which are folded away; see
// extension/candidate-view.js.
const { partition: partitionCandidates, collapsedSummary } = DownerCandidateView;
// One definition of what each state means for the UI; see extension/job-state.js.
const {
  showsProgress,
  isBusy,
  canPause,
  canResume,
  canCancel,
  INTERRUPTED_ERROR
} = DownerJobState;

const statusElement = document.getElementById("status");
const listElement = document.getElementById("media-list");
const otherListElement = document.getElementById("other-media-list");
const otherCandidatesElement = document.getElementById("other-candidates");
const otherSummaryElement = document.getElementById("other-candidates-summary");
const helpElement = document.getElementById("help");
const downloadStatusElement = document.getElementById("download-status");
const rowsByJobId = new Map();
const rowsByUrl = new Map();

function showStatus(message) {
  statusElement.textContent = message;
}

/**
 * Elapsed media time as `h:mm:ss` or `m:ss`.
 *
 * This is how far into the *media* FFmpeg has got, not how long the download
 * has been running — the two differ, and the first is the one that means
 * progress.
 */
function formatElapsed(milliseconds) {
  const total = Math.max(0, Math.floor(milliseconds / 1000));
  const seconds = String(total % 60).padStart(2, "0");
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}`
    : `${minutes}:${seconds}`;
}

/**
 * Render one job's state. A job with no row on this page is not about what the
 * user is looking at, so it renders nothing at all — in particular it does not
 * set the headline status, which is the defect this guard closes.
 */
/**
 * The row a job should render into, or nothing if the job is not about media on
 * this page. A row is claimed by one job at a time: two jobs for the same URL no
 * longer fight over it, and an older job can never take a row back from a newer
 * one, which is how a stale job used to overwrite a live download's row.
 */
function rowFor(job) {
  const claimed = rowsByJobId.get(job.id);
  if (claimed) return claimed;
  const row = rowsByUrl.get(job.url);
  if (!row) return undefined;
  const ownerStartedAt = row.jobStartedAt;
  const startedAt = job.startedAt || 0;
  if (row.jobId && row.jobId !== job.id && startedAt < (ownerStartedAt || 0)) {
    return undefined;
  }
  return row;
}

function claimRow(row, job) {
  if (row.jobId && row.jobId !== job.id) rowsByJobId.delete(row.jobId);
  row.jobId = job.id;
  row.jobStartedAt = job.startedAt || 0;
  rowsByJobId.set(job.id, row);
}

function renderDownloadStatus(job) {
  const row = rowFor(job);
  if (!row) return;
  claimRow(row, job);
  const button = row.download;
  button.dataset.jobId = job.id;
  row.progress.hidden = !showsProgress(job.state);
  if (job.totalSegments) {
    const completed = job.state === "completed"
      ? job.totalSegments
      : Math.min(job.completedSegments || 0, job.totalSegments);
    row.count.textContent = String(completed) + " / " + String(job.totalSegments) + " segments";
    row.progress.max = 100;
    row.progress.value = job.state === "completed"
      ? 100
      : Math.max(0, Math.min(100, job.percent || 0));
  } else if (job.state === "completed") {
    row.count.textContent = "Complete";
    row.progress.max = 100;
    row.progress.value = 100;
  } else if (job.state === "cancelled") {
    row.count.textContent = "Cancelled";
    row.progress.removeAttribute("value");
  } else if (job.state === "failed") {
    row.count.textContent = "Failed";
    row.progress.removeAttribute("value");
  } else if (job.state === "interrupted") {
    row.count.textContent = "Interrupted — not running";
    row.progress.removeAttribute("value");
  } else if (job.elapsedMs > 0) {
    // No segment total, but FFmpeg has said how far into the media it is. The
    // bar stays indeterminate — a numerator without a denominator is not a
    // fraction — while the text advances, which is what distinguishes a
    // download that is working from one that is stuck (KEI-86).
    row.count.textContent = `${formatElapsed(job.elapsedMs)} downloaded`;
    row.progress.removeAttribute("value");
  } else {
    row.count.textContent = job.metadataError
      ? "Waiting for playlist metadata…"
      : "Waiting for FFmpeg progress…";
    row.progress.removeAttribute("value");
  }
  if (job.state === "completed") {
    button.disabled = true;
    button.textContent = "Downloaded";
  } else if (job.state === "failed") {
    button.disabled = false;
    button.textContent = "Retry";
  } else if (job.state === "cancelled") {
    button.disabled = false;
    button.textContent = "Download again";
  } else if (job.state === "interrupted") {
    button.disabled = false;
    button.textContent = "Download again";
  } else if (job.state === "preparing") {
    button.disabled = true;
    button.textContent = "Preparing…";
  } else if (job.state === "retrying") {
    button.disabled = true;
    button.textContent = "Reconnecting…";
  } else if (job.state === "paused") {
    button.disabled = true;
    button.textContent = "Paused";
  } else if (job.state === "cancelling") {
    button.disabled = true;
    button.textContent = "Cancelling…";
  } else {
    button.disabled = true;
    button.textContent = "Downloading…";
  }

  const busy = isBusy(job.state);
  // Pause and Resume rest on Unix process signals, which the host reports for
  // itself in the `hello` handshake. A host that cannot do it gets no buttons
  // rather than buttons that can only answer `control_failed`; Cancel works
  // everywhere. Absent capabilities mean a host too old to say, which is a host
  // that does support them — the field arrived long after the commands did.
  const canSignal = job.capabilities?.pause_resume !== false;
  row.pause.hidden = !busy || !canPause(job.state) || !canSignal;
  row.resume.hidden = !busy || !canResume(job.state) || !canSignal;
  row.cancel.hidden = !busy || !canCancel(job.state);
  row.pause.disabled = !canPause(job.state) || !canSignal;
  row.resume.disabled = !canResume(job.state) || !canSignal;
  row.cancel.disabled = !canCancel(job.state);

  if (job.state === "completed") {
    showStatus(`Download complete: ${job.path}`);
    downloadStatusElement.textContent = "The media file is ready.";
  } else if (job.state === "failed" && job.errorCode === "resume_failed") {
    // The input is fine; the connections FFmpeg was holding while stopped are
    // not. Saying so is the difference between "retry this" and "this will
    // never work" — see docs/adr/0012-control-semantics.md.
    showStatus("Could not resume: the connection was lost while paused.");
    downloadStatusElement.textContent = job.path
      ? `Retry starts the download again from the beginning. The part-written file is at ${job.path}.`
      : "Retry starts the download again from the beginning. The part-written file was kept.";
  } else if (job.state === "failed") {
    showStatus(job.error || "Download failed.");
    downloadStatusElement.textContent = job.path
      ? `Download failed. You can retry. The part-written file is at ${job.path}.`
      : "Download failed. You can retry. The part-written file was kept.";
  } else if (job.state === "cancelled") {
    showStatus("Download cancelled.");
    // What actually happened to the fragment, from the policy this job ran
    // under — not from the setting as it stands now.
    downloadStatusElement.textContent = job.keepPartial
      ? `The part-written file was kept${job.path ? ` at ${job.path}` : ""}.`
      : "The part-written file was deleted.";
  } else if (job.state === "paused") {
    showStatus("Download paused.");
    // Said here rather than in the docs only: this is the moment the user is
    // deciding how long to leave it.
    downloadStatusElement.textContent =
      "Resume or cancel this download. A long pause can break the connection.";
  } else if (job.state === "retrying") {
    // Named as reconnection rather than failure: nothing has gone wrong from
    // the user's point of view yet, and the job may well finish. Saying
    // "failed, retrying" would ask them to worry about something they cannot
    // act on.
    showStatus(
      job.attempt && job.maxAttempts
        ? `Connection lost. Reconnecting… (attempt ${job.attempt} of ${job.maxAttempts})`
        : "Connection lost. Reconnecting…"
    );
    downloadStatusElement.textContent =
      "The download restarts from the beginning of the file. Cancel if you would rather stop.";
  } else if (job.state === "cancelling") {
    showStatus("Cancelling download…");
    downloadStatusElement.textContent = "FFmpeg is stopping; please wait.";
  } else if (job.state === "interrupted") {
    showStatus(job.error || INTERRUPTED_ERROR);
    downloadStatusElement.textContent = "Start it again to download the rest.";
  } else {
    showStatus("Download started. FFmpeg is working…");
    downloadStatusElement.textContent = job.metadataError
      ? `Segment count unavailable: ${job.metadataError}`
      : "You can close this popup; Firefox will notify you when it finishes.";
  }
  if (job.controlError) downloadStatusElement.textContent = job.controlError;
}

/**
 * A bit rate a person can compare. `BANDWIDTH` is bits per second.
 */
function formatBitrate(bandwidth) {
  if (!bandwidth) return "";
  return bandwidth >= 1000000
    ? `${(bandwidth / 1000000).toFixed(1)} Mbps`
    : `${Math.round(bandwidth / 1000)} kbps`;
}

/**
 * What one rendition is called in the picker.
 *
 * Height first, because "1080p" is how people choose. The bit rate is the
 * tie-breaker between two renditions at one resolution, and the whole label
 * falls back to the URL when a master declares neither — a row with no words
 * on it would be a choice nobody can make.
 */
function renditionLabel(rendition) {
  const parts = [];
  if (rendition.height) parts.push(`${rendition.height}p`);
  else if (rendition.width) parts.push(`${rendition.width} wide`);
  const bitrate = formatBitrate(rendition.bandwidth);
  if (bitrate) parts.push(bitrate);
  return parts.join(" · ") || rendition.url;
}

/**
 * Ask the background script what a playlist offers and, when it offers a real
 * choice, build the picker.
 *
 * Silence is the fallback everywhere: a host too old to enumerate, a playlist
 * that could not be fetched, a media playlist, or a master with one rendition
 * all leave the row exactly as it was, and Download still works. A picker with
 * nothing to decide is the thing this issue exists to remove, so it is never
 * shown for the sake of showing something.
 */
async function offerRenditions(row, candidate, sourceUrl, tabId) {
  if (candidate.kind !== "hls" && candidate.type !== "hls") return;
  let info;
  try {
    info = await browser.runtime.sendMessage({
      type: "inspect-playlist",
      url: candidate.url,
      sourceUrl,
      tabId
    });
  } catch (_) {
    return;
  }
  const renditions = (info?.ok && info.renditions) || [];
  if (info?.kind !== "master" || renditions.length < 2) return;

  for (const rendition of renditions) {
    const option = document.createElement("option");
    option.value = rendition.url;
    option.textContent = renditionLabel(rendition);
    if (rendition.default) option.selected = true;
    row.select.append(option);
  }
  // The default is what an unchosen download already gets, so opening the
  // popup and pressing Download is unchanged (KEI-89's criterion).
  if (!renditions.some((rendition) => rendition.default)) {
    row.select.selectedIndex = 0;
  }
  const separateAudio = renditions.some((rendition) => rendition.audio_url);
  row.variantNote.textContent = separateAudio
    ? `${renditions.length} qualities · audio is a separate track and is included`
    : `${renditions.length} qualities`;
  row.variants.hidden = false;
}

function addMediaRow(candidate, sourceUrl, tabId, title, target = listElement) {
  const item = document.createElement("li");
  const task = document.createElement("div");
  task.className = "task";
  const label = document.createElement("span");
  label.className = "url";
  const meta = document.createElement("div");
  meta.className = "task-meta";
  const count = document.createElement("span");
  count.className = "segment-count";
  count.textContent = "Not started";
  const progress = document.createElement("progress");
  progress.className = "progress";
  progress.max = 100;
  progress.hidden = true;
  progress.removeAttribute("value");
  meta.append(count, progress);
  // `kind` distinguishes DASH, which used to be labelled VIDEO with no
  // indication that nothing had ever validated it against FFmpeg.
  const kind = (candidate.kind === "dash" ? "dash" : candidate.type || candidate.kind) || "file";
  label.textContent = `${(kind === "file" ? "video" : kind).toUpperCase()}: ${candidate.url}`;
  if (candidate.experimental) {
    const badge = document.createElement("span");
    badge.className = "experimental";
    badge.textContent = "experimental";
    label.append(badge);
  }
  // Hidden until the host says there is a choice; see `offerRenditions`.
  const variants = document.createElement("div");
  variants.className = "variants";
  variants.hidden = true;
  const variantLabel = document.createElement("label");
  variantLabel.textContent = "Quality ";
  const select = document.createElement("select");
  select.className = "variant-select";
  variantLabel.append(select);
  const variantNote = document.createElement("span");
  variantNote.className = "variant-note";
  variants.append(variantLabel, variantNote);
  const button = document.createElement("button");
  button.textContent = "Download";
  const controls = document.createElement("span");
  controls.className = "controls";
  const pause = document.createElement("button");
  pause.textContent = "Pause";
  const resume = document.createElement("button");
  resume.textContent = "Resume";
  const cancel = document.createElement("button");
  cancel.textContent = "Cancel";
  const row = {
    download: button,
    pause,
    resume,
    cancel,
    count,
    progress,
    variants,
    select,
    variantNote,
    jobId: null,
    jobStartedAt: 0
  };
  rowsByUrl.set(candidate.url, row);
  controls.append(button, pause, resume, cancel);
  pause.hidden = true;
  resume.hidden = true;
  cancel.hidden = true;

  async function sendControl(command) {
    try {
      const response = await browser.runtime.sendMessage({
        type: "control-download",
        jobId: button.dataset.jobId,
        command
      });
      if (!response?.ok) throw new Error(response?.error || "Could not control download.");
      if (command === "pause") {
        showStatus("Pausing download…");
        downloadStatusElement.textContent = "Waiting for FFmpeg to acknowledge pause.";
      } else if (command === "resume") {
        showStatus("Resuming download…");
        downloadStatusElement.textContent = "Waiting for FFmpeg to resume.";
      } else if (command === "cancel") {
        renderDownloadStatus({ id: button.dataset.jobId, url: candidate.url, state: "cancelling" });
      }
    } catch (error) {
      showStatus(error.message || "Could not control download.");
      downloadStatusElement.textContent = "The download is still running.";
    }
  }
  pause.addEventListener("click", () => sendControl("pause"));
  resume.addEventListener("click", () => sendControl("resume"));
  cancel.addEventListener("click", () => sendControl("cancel"));
  button.addEventListener("click", async () => {
    button.disabled = true;
    button.textContent = "Starting…";
    try {
      const response = await browser.runtime.sendMessage({
        type: "download-media",
        url: candidate.url,
        sourceUrl,
        // Only when a picker is showing. Absent, the host applies the same
        // rule as before, so the one-click path is untouched.
        variantUrl: variants.hidden ? null : select.value || null,
        // The page title names the file when the playlist URL is generic
        // (`index.m3u8` and friends); the host bounds and sanitises it.
        title,
        tabId
      });
      if (response?.ok && response.jobId) {
        button.dataset.jobId = response.jobId;
        claimRow(row, { id: response.jobId, startedAt: Date.now() });
        renderDownloadStatus({ ...response, id: response.jobId, url: candidate.url });
      } else {
        throw new Error(response?.error || "Download failed.");
      }
    } catch (error) {
      button.disabled = false;
      button.textContent = "Retry";
      showStatus(error.message || "Download failed");
      downloadStatusElement.textContent = "Download failed. You can retry.";
    }
  });
  task.append(label, variants, meta, controls);
  item.append(task);
  target.append(item);
  return row;
}

browser.runtime.onMessage.addListener((message) => {
  if (message?.type === "download-status" && message.job) {
    renderDownloadStatus(message.job);
  }
});

/**
 * Show a job that was still running when its session ended. No native task
 * exists for it, so the row must not offer Pause or Cancel and must not disable
 * Download: `controlDownload` would only answer "Download task is no longer
 * active." This is a popup-local presentation, not a job state — KEI-56 owns
 * reconciling such jobs in storage.
 */
/**
 * A job that is not running and was never reconciled into `interrupted` — a
 * record written before startup reconciliation existed. It renders into its row
 * as not running, and deliberately does not touch the headline, because unlike a
 * reconciled job it carries no trustworthy explanation to show.
 */
function renderInterruptedJob(job) {
  const row = rowsByUrl.get(job.url);
  if (!row) return;
  row.count.textContent = "Interrupted — not running";
  row.progress.hidden = true;
  row.progress.removeAttribute("value");
  row.download.disabled = false;
  row.download.textContent = "Download";
  // Deliberately leave `dataset.jobId` unset: nothing may be wired to a task
  // that no longer exists. The row is not claimed either, so a later live job
  // for this URL takes it cleanly.
  delete row.download.dataset.jobId;
  for (const control of [row.pause, row.resume, row.cancel]) {
    control.hidden = true;
    control.disabled = true;
  }
}

async function restoreDownloadStatuses() {
  try {
    const response = await browser.runtime.sendMessage({ type: "get-download-statuses" });
    const decisions = renderableJobs({
      jobs: response?.jobs || [],
      candidateUrls: [...rowsByUrl.keys()],
      sessionJobIds: response?.sessionJobIds || []
    });
    for (const { job, render } of decisions) {
      // A reconciled `interrupted` job is about media on this page and carries a
      // real explanation, so it renders in full — that is the message KEI-56
      // requires after a restart. An unreconciled one only claims its row.
      if (render === RENDER_STALE && job.state !== "interrupted") {
        renderInterruptedJob(job);
      } else {
        renderDownloadStatus(job);
      }
    }
  } catch (_) {
    // Status restoration is optional; a new download still works.
  }
}

async function scanActiveTab() {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id || !tab.url || !/^https?:/.test(tab.url)) {
    showStatus("This page cannot be scanned.");
    helpElement.textContent = "Firefox blocks extensions on some internal pages.";
    return;
  }
  try {
    const result = await browser.tabs.sendMessage(tab.id, { type: "scan-media" });
    const candidates = result?.candidates || [];
    if (!candidates.length) {
      showStatus("No media URL found.");
      return;
    }
    // List only the best evidence the page offers, and fold the rest under the
    // disclosure triangle. The rule and its limits are in `candidate-view.js`.
    const { listed, collapsed } = partitionCandidates(candidates);

    showStatus(`${listed.length} media URL${listed.length === 1 ? "" : "s"} found.`);
    helpElement.textContent = "HLS playlists are listed first when available.";
    const rows = listed.map((candidate) =>
      addMediaRow(candidate, result.sourceUrl, tab.id, result.title)
    );
    for (const candidate of collapsed) {
      addMediaRow(candidate, result.sourceUrl, tab.id, result.title, otherListElement);
    }
    if (collapsed.length) {
      otherSummaryElement.textContent = collapsedSummary(collapsed);
      otherCandidatesElement.hidden = false;
    }
    await restoreDownloadStatuses();
    // After the rows exist and their jobs are restored: enumerating asks the
    // host, which is slower than rendering and must not hold the list up.
    //
    // One at a time. Each call opens its own short-lived native connection,
    // the way `hello` and `status` do, and one host process per playlist at
    // once is a cost nobody has measured — a page normally has one playlist
    // after segments are collapsed into it, so serialising costs nothing in
    // the usual case and bounds the unusual one.
    for (const [index, candidate] of listed.entries()) {
      await offerRenditions(rows[index], candidate, result.sourceUrl, tab.id);
    }
  } catch (error) {
    showStatus("Could not inspect this page.");
    helpElement.textContent = error.message;
  }
}

scanActiveTab();

/**
 * Warn, compactly, when the setup has never been checked or last failed.
 *
 * A warning outcome is deliberately not surfaced here: it means downloads work
 * (an old FFmpeg still downloads — ADR-0006), and a permanent banner for a
 * working setup is the kind of notice people learn to ignore.
 */
async function renderSetupWarning() {
  const element = document.getElementById("setup-warning");
  if (!element) return;
  let check = null;
  try {
    ({ setupCheck: check } = await browser.storage.local.get({ setupCheck: null }));
  } catch {
    return;
  }
  if (check && check.outcome !== "fail") {
    element.hidden = true;
    return;
  }
  element.textContent = check
    ? "Setup check failed. Open Settings → Check setup."
    : "Setup has not been checked. Open Settings → Check setup.";
  element.hidden = false;
}

renderSetupWarning();
