/**
 * The extension's download job state machine: the one definition of what states
 * a job can be in, which transitions are legal, and which states are terminal.
 *
 * Pure and importable from Node (same `module.exports` guard as
 * `extension/task-protocol.js`), so every transition can be tested directly.
 *
 * Two vocabularies meet here, and the distinction matters:
 *
 * - **Wire states** come from the native host and are specified in
 *   `docs/protocol.md`. `tests/fixtures/protocol.json` pins them, and both test
 *   suites read that file.
 * - **Extension-only states** never appear on the wire. `preparing` is set while
 *   the background script gathers cookies and playlist metadata before
 *   connecting; `interrupted` is set by startup reconciliation. The host neither
 *   emits nor accepts either one.
 *
 * So the extension's terminal set is deliberately *wider* than the protocol's:
 * `interrupted` ends a job for the extension, but a native channel is only ever
 * settled by a wire terminal state (see `TERMINAL_STATES` in
 * `extension/task-protocol.js`, which derives from `WIRE_TERMINAL_STATES`).
 */
var DownerJobState = (() => {
  const WIRE_ACTIVE_STATES = ["starting", "downloading", "retrying", "paused", "cancelling"];
  const WIRE_TERMINAL_STATES = ["completed", "failed", "cancelled"];
  const EXTENSION_ACTIVE_STATES = ["preparing"];
  const EXTENSION_TERMINAL_STATES = ["interrupted"];

  const ACTIVE = new Set([...WIRE_ACTIVE_STATES, ...EXTENSION_ACTIVE_STATES]);
  const TERMINAL = new Set([...WIRE_TERMINAL_STATES, ...EXTENSION_TERMINAL_STATES]);
  const WIRE_TERMINAL = new Set(WIRE_TERMINAL_STATES);
  const ALL = new Set([...ACTIVE, ...TERMINAL]);

  /** The message a reconciled job carries, and what the popup shows for it. */
  const INTERRUPTED_ERROR = "Interrupted by browser restart; partial file kept.";

  /**
   * Allowed transitions, matching what `background.js` actually produces:
   * `beginDownload` opens at `starting`, `runDownload` moves to `preparing`
   * while it gathers cookies and metadata, and the host's first progress event
   * returns to `starting`. Every active state may be interrupted, because
   * reconciliation can run at any point. Terminal states are final — a retry
   * creates a new job with a new id rather than reviving this one.
   */
  const TRANSITIONS = {
    starting: ["preparing", "downloading", "retrying", "paused", "cancelling", ...WIRE_TERMINAL_STATES, "interrupted"],
    preparing: ["starting", "downloading", "retrying", "paused", "cancelling", ...WIRE_TERMINAL_STATES, "interrupted"],
    downloading: ["retrying", "paused", "cancelling", ...WIRE_TERMINAL_STATES, "interrupted"],
    // A retry goes back to `downloading` when the next attempt starts, or
    // straight to a terminal state when the attempts run out. It is active, not
    // terminal: the job has not finished, it is between attempts (ADR-0023).
    retrying: ["downloading", "paused", "cancelling", ...WIRE_TERMINAL_STATES, "interrupted"],
    paused: ["downloading", "retrying", "cancelling", ...WIRE_TERMINAL_STATES, "interrupted"],
    cancelling: [...WIRE_TERMINAL_STATES, "interrupted"],
    completed: [],
    failed: [],
    cancelled: [],
    interrupted: []
  };

  /** States a job may open in. */
  const INITIAL_STATES = new Set(["starting"]);

  function isState(state) {
    return typeof state === "string" && ALL.has(state);
  }

  function isTerminal(state) {
    return TERMINAL.has(state);
  }

  function isActive(state) {
    return ACTIVE.has(state);
  }

  /**
   * Whether a job may move from `from` to `to`. `from` being absent means the
   * job is being created. An unknown state is never a legal destination, so a
   * typo cannot invent a state at runtime.
   */
  function canTransition(from, to) {
    if (!isState(to)) return false;
    if (from === undefined || from === null) return INITIAL_STATES.has(to);
    if (!isState(from)) return false;
    if (from === to) return true;
    return TRANSITIONS[from].includes(to);
  }

  // Presentation predicates. The popup asks these instead of repeating state
  // lists, so adding a state cannot leave one `includes([...])` behind.

  /** The row shows a progress bar and a segment count. */
  function showsProgress(state) {
    return isActive(state) || state === "completed";
  }

  /** The job is running or could resume running, so its row is not idle. */
  function isBusy(state) {
    return isActive(state);
  }

  function canPause(state) {
    return ["starting", "preparing", "downloading"].includes(state);
  }

  function canResume(state) {
    return state === "paused";
  }

  function canCancel(state) {
    return canPause(state) || state === "paused";
  }

  /**
   * Reconcile jobs restored from storage against reality. Native ports do not
   * survive a browser restart or an extension reload, and `nativeTasks` starts
   * empty, so a job stored in an active state has no process behind it and would
   * otherwise stay "Downloading…" forever with Pause and Cancel answering
   * "Download task is no longer active."
   *
   * Returns a new array; jobs already terminal are passed through untouched, so
   * this is safe to run on every startup and is backward compatible with records
   * written before `interrupted` existed.
   */
  function reconcileRestoredJobs(jobs, now = Date.now()) {
    return (jobs || [])
      .filter((job) => job && job.id)
      .map((job) => {
        if (isTerminal(job.state)) return job;
        // An unknown or missing state is also reconciled: it cannot be running.
        return {
          ...job,
          state: "interrupted",
          error: job.error || INTERRUPTED_ERROR,
          finishedAt: job.finishedAt || now
        };
      });
  }

  return {
    WIRE_ACTIVE_STATES,
    WIRE_TERMINAL_STATES,
    EXTENSION_ACTIVE_STATES,
    EXTENSION_TERMINAL_STATES,
    ACTIVE_STATES: ACTIVE,
    TERMINAL_STATES: TERMINAL,
    WIRE_TERMINAL_STATE_SET: WIRE_TERMINAL,
    ALL_STATES: ALL,
    INITIAL_STATES,
    TRANSITIONS,
    INTERRUPTED_ERROR,
    isState,
    isTerminal,
    isActive,
    canTransition,
    showsProgress,
    isBusy,
    canPause,
    canResume,
    canCancel,
    reconcileRestoredJobs
  };
})();

if (typeof module !== "undefined") module.exports = DownerJobState;
