/* global DownerJobView, DownerJobState */

const { RENDER_STALE, renderableJobs } = DownerJobView;
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
const helpElement = document.getElementById("help");
const downloadStatusElement = document.getElementById("download-status");
const rowsByJobId = new Map();
const rowsByUrl = new Map();

function showStatus(message) {
  statusElement.textContent = message;
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
    downloadStatusElement.textContent =
      "Retry starts the download again from the beginning. The part-written file was kept.";
  } else if (job.state === "failed") {
    showStatus(job.error || "Download failed.");
    downloadStatusElement.textContent = "Download failed. You can retry. The part-written file was kept.";
  } else if (job.state === "cancelled") {
    showStatus("Download cancelled.");
    // What actually happened to the fragment, from the policy this job ran
    // under — not from the setting as it stands now.
    downloadStatusElement.textContent = job.keepPartial
      ? "The part-written file was kept."
      : "The part-written file was deleted.";
  } else if (job.state === "paused") {
    showStatus("Download paused.");
    // Said here rather than in the docs only: this is the moment the user is
    // deciding how long to leave it.
    downloadStatusElement.textContent =
      "Resume or cancel this download. A long pause can break the connection.";
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

function addMediaRow(candidate, sourceUrl, tabId, title) {
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
  label.textContent = `${candidate.type.toUpperCase()}: ${candidate.url}`;
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
  task.append(label, meta, controls);
  item.append(task);
  listElement.append(item);
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
    showStatus(`${candidates.length} media URL${candidates.length === 1 ? "" : "s"} found.`);
    helpElement.textContent = "HLS playlists are listed first when available.";
    candidates.forEach((candidate) => addMediaRow(candidate, result.sourceUrl, tab.id, result.title));
    await restoreDownloadStatuses();
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
