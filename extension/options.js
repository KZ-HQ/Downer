/* global browser, DownerRedact, DownerJobLogs */

const { redactText } = DownerRedact;
const { trimLogEntries } = DownerJobLogs;

const outputElement = document.getElementById("output-dir");
const threadsElement = document.getElementById("ffmpeg-threads");
const conflictElement = document.getElementById("on-conflict");
const statusElement = document.getElementById("status");
const filterElement = document.getElementById("job-filter");
const logsElement = document.getElementById("logs");
const jobs = new Map();
/**
 * Logs are held here rather than on the job record: the background script keeps
 * them under their own `downloadLogs:<jobId>` storage key and sends them as
 * their own `download-log` message, so that a burst of FFmpeg output does not
 * rewrite or re-send every job (KEI-55).
 */
const logsByJob = new Map();

function renderFilter() {
  const selected = filterElement.value;
  filterElement.textContent = "";
  const all = document.createElement("option");
  all.value = "all";
  all.textContent = "All downloads";
  filterElement.append(all);
  for (const job of jobs.values()) {
    const option = document.createElement("option");
    option.value = job.id;
    option.textContent = `${job.state}: ${redactText(job.url)}`;
    filterElement.append(option);
  }
  filterElement.value = jobs.has(selected) || selected === "all" ? selected : "all";
}

function renderLogs() {
  const selected = filterElement.value;
  const entries = [];
  for (const job of jobs.values()) {
    if (selected !== "all" && selected !== job.id) continue;
    // The URL heading a log line is redacted at display as well as in the line
    // itself: the job's own `url` is stored whole, because the popup matches
    // jobs to page media by it and a re-download needs the real thing.
    const url = redactText(job.url);
    for (const entry of logsByJob.get(job.id) || []) {
      entries.push({ ...entry, url });
    }
  }
  entries.sort((left, right) => left.at - right.at);
  logsElement.textContent = entries.length
    ? entries.map((entry) => `[${new Date(entry.at).toLocaleTimeString()}] ${entry.url}\n${entry.text}`).join("\n")
    : "No FFmpeg logs yet.";
  logsElement.scrollTop = logsElement.scrollHeight;
}

function updateJob(job) {
  if (!job?.id) return;
  jobs.set(job.id, job);
  renderFilter();
  renderLogs();
}

/** One coalesced batch of log lines for one job. */
function appendLogs(jobId, entries) {
  if (!jobId || !Array.isArray(entries) || !entries.length) return;
  logsByJob.set(jobId, trimLogEntries([...(logsByJob.get(jobId) || []), ...entries]));
  renderLogs();
}

/** Kept identical to `default_on_conflict` in `tests/fixtures/protocol.json`. */
const DEFAULT_ON_CONFLICT = "rename";

browser.storage.local.get({
  outputDir: "",
  ffmpegThreads: null,
  onConflict: DEFAULT_ON_CONFLICT
}).then((settings) => {
  outputElement.value = settings.outputDir;
  threadsElement.value = Number.isInteger(settings.ffmpegThreads) ? settings.ffmpegThreads : "";
  // An unknown stored value falls back rather than being offered: the host
  // refuses a policy it does not understand.
  conflictElement.value = Array.from(conflictElement.options).some(
    (option) => option.value === settings.onConflict
  )
    ? settings.onConflict
    : DEFAULT_ON_CONFLICT;
});

document.getElementById("save").addEventListener("click", async () => {
  const threads = threadsElement.value.trim();
  const ffmpegThreads = threads ? Number(threads) : null;
  if (threads && (!Number.isInteger(ffmpegThreads) || ffmpegThreads < 1)) {
    statusElement.textContent = "Threads must be a positive whole number or blank.";
    return;
  }
  await browser.storage.local.set({
    outputDir: outputElement.value.trim(),
    ffmpegThreads,
    onConflict: conflictElement.value
  });
  statusElement.textContent = "Saved.";
});

filterElement.addEventListener("change", renderLogs);
document.getElementById("clear-logs").addEventListener("click", async () => {
  await browser.runtime.sendMessage({ type: "clear-download-logs" });
  logsByJob.clear();
  renderLogs();
});

browser.runtime.onMessage.addListener((message) => {
  if (message?.type === "download-status") updateJob(message.job);
  if (message?.type === "download-log") appendLogs(message.jobId, message.entries);
  if (message?.type === "download-logs-cleared") {
    logsByJob.clear();
    renderLogs();
  }
});

browser.runtime.sendMessage({ type: "get-download-statuses" }).then((response) => {
  for (const job of response?.jobs || []) updateJob(job);
  return browser.runtime.sendMessage({ type: "get-download-logs" });
}).then((response) => {
  for (const [jobId, entries] of Object.entries(response?.logs || {})) {
    logsByJob.set(jobId, entries);
  }
  renderLogs();
});
