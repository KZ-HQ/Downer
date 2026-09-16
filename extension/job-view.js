/**
 * Decides which persisted download jobs the popup should render.
 *
 * Pure and importable from Node (same `module.exports` guard as
 * `extension/hls.js`), because `extension/popup.js` touches the DOM and the
 * `browser` global at load and cannot be imported as-is.
 *
 * This is a **rendering** decision, not a job state. Nothing here is written
 * back to `storage.local`, and no job state is added or renamed: the extension
 * job state machine is owned by KEI-56, and this module deliberately reads the
 * existing states rather than inventing a set of its own.
 */
var DownerJobView = (() => {
  /** The states in which a job has finished. Mirrors the native protocol's. */
  const TERMINAL_STATES = new Set(["completed", "failed", "cancelled"]);

  /**
   * How the popup should treat one persisted job:
   *
   * - `live`   — started in this browser session for media listed on this page.
   *              Renders into its row and may set the headline status.
   * - `stale`  — persisted while still running, but the session that ran it is
   *              gone, so no native task exists. Renders into its row as not
   *              running, and never sets the headline.
   * - `history`— everything else: finished in an earlier session, or about a
   *              page the user is not looking at. Not rendered at all.
   */
  const RENDER_LIVE = "live";
  const RENDER_STALE = "stale";
  const RENDER_HISTORY = "history";

  /**
   * @param {object[]} jobs          persisted jobs, from `get-download-statuses`
   * @param {string[]} candidateUrls media URLs listed on the page being viewed
   * @param {string[]} sessionJobIds jobs begun in this background session
   */
  function classifyPersistedJobs({ jobs = [], candidateUrls = [], sessionJobIds = [] } = {}) {
    const onThisPage = new Set(candidateUrls);
    const thisSession = new Set(sessionJobIds);

    return jobs
      .filter((job) => job && job.id)
      .map((job) => {
        if (!job.url || !onThisPage.has(job.url)) {
          // The defect this fixes: such a job used to fall through the popup's
          // row lookup and still set the headline status for an unrelated page.
          return { job, render: RENDER_HISTORY, reason: "not-on-this-page" };
        }
        if (thisSession.has(job.id)) {
          return { job, render: RENDER_LIVE, reason: "this-session" };
        }
        if (!TERMINAL_STATES.has(job.state)) {
          // Non-terminal but not running: the browser closed mid-download and
          // `runDownload` never resumes. Showing it as live would wire Pause and
          // Cancel to a native task that no longer exists.
          return { job, render: RENDER_STALE, reason: "interrupted" };
        }
        return { job, render: RENDER_HISTORY, reason: "earlier-session" };
      });
  }

  /** The jobs to render, in the order the popup should apply them. */
  function renderableJobs(input) {
    return classifyPersistedJobs(input).filter(
      (decision) => decision.render !== RENDER_HISTORY
    );
  }

  return {
    TERMINAL_STATES,
    RENDER_LIVE,
    RENDER_STALE,
    RENDER_HISTORY,
    classifyPersistedJobs,
    renderableJobs
  };
})();

if (typeof module !== "undefined") module.exports = DownerJobView;
