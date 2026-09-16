const statusElement = document.getElementById("status");
const listElement = document.getElementById("media-list");
const helpElement = document.getElementById("help");
const downloadStatusElement = document.getElementById("download-status");
const rowsByJobId = new Map();
const rowsByUrl = new Map();

function showStatus(message) {
  statusElement.textContent = message;
}

function renderDownloadStatus(job) {
  const row = rowsByJobId.get(job.id) || rowsByUrl.get(job.url);
  if (row) {
    rowsByJobId.set(job.id, row);
    const button = row.download;
    button.dataset.jobId = job.id;
    const showProgress = ["starting", "preparing", "downloading", "paused", "cancelling", "completed"].includes(job.state);
    row.progress.hidden = !showProgress;
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

    const active = ["starting", "preparing", "downloading", "paused", "cancelling"].includes(job.state);
    row.pause.hidden = !active || !["starting", "preparing", "downloading"].includes(job.state);
    row.resume.hidden = !active || job.state !== "paused";
    row.cancel.hidden = !active || job.state === "cancelling";
    row.pause.disabled = !["starting", "preparing", "downloading"].includes(job.state);
    row.resume.disabled = job.state !== "paused";
    row.cancel.disabled = !["starting", "preparing", "downloading", "paused"].includes(job.state);
  }

  if (job.state === "completed") {
    showStatus(`Download complete: ${job.path}`);
    downloadStatusElement.textContent = "The media file is ready.";
  } else if (job.state === "failed") {
    showStatus(job.error || "Download failed.");
    downloadStatusElement.textContent = "Download failed. You can retry.";
  } else if (job.state === "cancelled") {
    showStatus("Download cancelled.");
    downloadStatusElement.textContent = "The partial file was kept for diagnostics.";
  } else if (job.state === "paused") {
    showStatus("Download paused.");
    downloadStatusElement.textContent = "Resume or cancel this download.";
  } else if (job.state === "cancelling") {
    showStatus("Cancelling download…");
    downloadStatusElement.textContent = "FFmpeg is stopping; please wait.";
  } else {
    showStatus("Download started. FFmpeg is working…");
    downloadStatusElement.textContent = job.metadataError
      ? `Segment count unavailable: ${job.metadataError}`
      : "You can close this popup; Firefox will notify you when it finishes.";
  }
  if (job.controlError) downloadStatusElement.textContent = job.controlError;
}

function addMediaRow(candidate, sourceUrl, tabId) {
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
  const row = { download: button, pause, resume, cancel, count, progress };
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
        tabId
      });
      if (response?.ok && response.jobId) {
        button.dataset.jobId = response.jobId;
        rowsByJobId.set(response.jobId, row);
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

async function restoreDownloadStatuses() {
  try {
    const response = await browser.runtime.sendMessage({ type: "get-download-statuses" });
    for (const job of response?.jobs || []) renderDownloadStatus(job);
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
    candidates.forEach((candidate) => addMediaRow(candidate, result.sourceUrl, tab.id));
    await restoreDownloadStatuses();
  } catch (error) {
    showStatus("Could not inspect this page.");
    helpElement.textContent = error.message;
  }
}

scanActiveTab();
