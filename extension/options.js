/* global browser, DownerRedact, DownerJobLogs */

const { redactText } = DownerRedact;
const { trimLogEntries } = DownerJobLogs;

const outputElement = document.getElementById("output-dir");
const ffmpegPathElement = document.getElementById("ffmpeg-path");
const threadsElement = document.getElementById("ffmpeg-threads");
const conflictElement = document.getElementById("on-conflict");
const titleNamingElement = document.getElementById("name-from-title");
const keepPartialElement = document.getElementById("keep-partial");
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
/** Kept identical to `DEFAULT_NAME_FROM_TITLE` in `background.js` (KEI-84). */
const DEFAULT_NAME_FROM_TITLE = false;
/** Kept identical to `default_keep_partial` in `tests/fixtures/protocol.json`. */
const DEFAULT_KEEP_PARTIAL = false;

browser.storage.local.get({
  outputDir: "",
  ffmpegPath: "",
  ffmpegThreads: null,
  onConflict: DEFAULT_ON_CONFLICT,
  nameFromTitle: DEFAULT_NAME_FROM_TITLE,
  keepPartial: DEFAULT_KEEP_PARTIAL
}).then((settings) => {
  titleNamingElement.checked = settings.nameFromTitle === true;
  keepPartialElement.checked = settings.keepPartial === true;
  outputElement.value = settings.outputDir;
  ffmpegPathElement.value = settings.ffmpegPath || "";
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
    ffmpegPath: ffmpegPathElement.value.trim(),
    ffmpegThreads,
    onConflict: conflictElement.value,
    nameFromTitle: titleNamingElement.checked,
    keepPartial: keepPartialElement.checked
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

/**
 * Render one setup check.
 *
 * The outcome vocabulary is the host's (`pass`, `warn`, `fail`) and is not
 * reinterpreted here: a warning is a real third state, not a soft failure. See
 * `src/diagnostics.rs`.
 */
function renderCheck(check) {
  const item = document.createElement("li");
  item.className = `check check-${check.outcome}`;

  const title = document.createElement("p");
  title.className = "check-title";
  title.textContent = `${OUTCOME_LABELS[check.outcome] || check.outcome} ${check.title}`;
  item.append(title);

  const detail = document.createElement("p");
  detail.className = "check-detail";
  detail.textContent = check.detail;
  item.append(detail);

  if (check.remedy) {
    const remedy = document.createElement("p");
    remedy.className = "check-remedy";
    remedy.textContent = check.remedy;
    item.append(remedy);
  }
  return item;
}

const OUTCOME_LABELS = { pass: "OK", warn: "Warning", fail: "Failed" };

function renderSetupReport(report) {
  resultsElement.textContent = "";

  const summary = document.createElement("p");
  summary.className = "check-summary";
  summary.textContent =
    `downer ${report.host_version} (protocol ${report.protocol_version}) on ${report.platform}`;
  resultsElement.append(summary);

  const list = document.createElement("ul");
  list.className = "checks";
  for (const check of report.checks || []) {
    list.append(renderCheck(check));
  }
  resultsElement.append(list);
}

/**
 * Turn a failure to reach or agree with the host into a rendered check.
 *
 * Three different problems arrive here as one exception, and they need three
 * different instructions:
 *
 * - no registration at all, which `downer install-host` fixes;
 * - a registration Firefox cannot execute;
 * - a host and extension built from different protocol versions, which is
 *   ordinary upgrade skew — the two ship separately, so rebuilding one without
 *   the other is easy — and which `install-host` alone does **not** fix.
 *
 * The host cannot report any of these: in the first two there is nothing to
 * ask, and in the third it either refused the handshake or answered a version
 * this extension will not talk to.
 */
function connectionCheck(message) {
  const text = String(message || "");

  // Either direction of mismatch: the host refused our version, or answered
  // with one we refuse. Both mean the two halves are from different builds.
  if (/protocol version/i.test(text)) {
    return {
      name: "protocol_version",
      title: "Native host and extension speak the same protocol",
      outcome: "fail",
      detail: text,
      remedy:
        "The host and the extension are from different builds. Update whichever is older: "
        + "rebuild and reinstall the host with `downer install-host`, or load a matching "
        + "extension build."
    };
  }
  if (/no such native application/i.test(text)) {
    return {
      name: "host_connection",
      title: "Native host reachable",
      outcome: "fail",
      detail: text,
      remedy: "Firefox has no registration for the native host. Install it with `downer install-host`."
    };
  }
  if (/permission denied|access/i.test(text)) {
    return {
      name: "host_connection",
      title: "Native host reachable",
      outcome: "fail",
      detail: text,
      remedy:
        "Firefox found the registration but could not run it. Re-run `downer install-host`, "
        + "and check the launcher is executable."
    };
  }
  return {
    name: "host_connection",
    title: "Native host reachable",
    outcome: "fail",
    detail: text,
    remedy: "Could not reach the native host. Install or re-register it with `downer install-host`."
  };
}

function renderSetupFailure(error) {
  resultsElement.textContent = "";
  resultsElement.append(
    renderCheck(connectionCheck(error && error.message ? error.message : error))
  );
}

const checkButton = document.getElementById("check-setup");
const resultsElement = document.getElementById("setup-results");

checkButton.addEventListener("click", async () => {
  checkButton.disabled = true;
  resultsElement.textContent = "Checking…";
  try {
    const response = await browser.runtime.sendMessage({
      type: "check-setup",
      outputDir: outputElement.value.trim() || null,
      ffmpegPath: ffmpegPathElement.value.trim() || null
    });
    if (!response || !response.ok) {
      throw new Error((response && response.error) || "The native host did not answer.");
    }
    renderSetupReport(response.status);
  } catch (error) {
    renderSetupFailure(error);
  } finally {
    checkButton.disabled = false;
  }
});
