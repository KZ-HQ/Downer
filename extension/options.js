const outputElement = document.getElementById("output-dir");
const threadsElement = document.getElementById("ffmpeg-threads");
const statusElement = document.getElementById("status");
const filterElement = document.getElementById("job-filter");
const logsElement = document.getElementById("logs");
const jobs = new Map();

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
    option.textContent = `${job.state}: ${job.url}`;
    filterElement.append(option);
  }
  filterElement.value = jobs.has(selected) || selected === "all" ? selected : "all";
}

function renderLogs() {
  const selected = filterElement.value;
  const entries = [];
  for (const job of jobs.values()) {
    if (selected !== "all" && selected !== job.id) continue;
    for (const entry of job.logs || []) {
      entries.push({ ...entry, url: job.url });
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

browser.storage.local.get({ outputDir: "", ffmpegThreads: null }).then((settings) => {
  outputElement.value = settings.outputDir;
  threadsElement.value = Number.isInteger(settings.ffmpegThreads) ? settings.ffmpegThreads : "";
});

document.getElementById("save").addEventListener("click", async () => {
  const threads = threadsElement.value.trim();
  const ffmpegThreads = threads ? Number(threads) : null;
  if (threads && (!Number.isInteger(ffmpegThreads) || ffmpegThreads < 1)) {
    statusElement.textContent = "Threads must be a positive whole number or blank.";
    return;
  }
  await browser.storage.local.set({ outputDir: outputElement.value.trim(), ffmpegThreads });
  statusElement.textContent = "Saved.";
});

filterElement.addEventListener("change", renderLogs);
document.getElementById("clear-logs").addEventListener("click", async () => {
  await browser.runtime.sendMessage({ type: "clear-download-logs" });
  for (const job of jobs.values()) job.logs = [];
  renderLogs();
});

browser.runtime.onMessage.addListener((message) => {
  if (message?.type === "download-status") updateJob(message.job);
});

browser.runtime.sendMessage({ type: "get-download-statuses" }).then((response) => {
  for (const job of response?.jobs || []) updateJob(job);
});
