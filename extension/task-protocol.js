/* global browser */

var DownerTaskProtocol = (() => {
  const TERMINAL_STATES = new Set(["completed", "failed", "cancelled"]);

  class NativeTaskChannel {
    constructor(port, jobId, onEvent, disconnectError) {
      this.port = port;
      this.jobId = jobId;
      this.onEvent = onEvent;
      this.disconnectError = disconnectError;
      this.pending = new Map();
      this.nextRequest = 1;
      this.settled = false;
      this.completion = new Promise((resolve, reject) => {
        this.resolveCompletion = resolve;
        this.rejectCompletion = reject;
      });
      port.onMessage.addListener((response) => this.handleMessage(response));
      port.onDisconnect.addListener(() => this.handleDisconnect());
    }

    start(request) {
      this.port.postMessage({ ...request, job_id: this.jobId });
      return this.completion;
    }

    request(command, payload = {}, timeoutMs = 5000) {
      if (this.settled) {
        return Promise.resolve({ ok: false, error: "Download task is no longer active." });
      }
      const requestId = `${this.jobId}-${this.nextRequest++}`;
      return new Promise((resolve) => {
        const timeout = setTimeout(() => {
          this.pending.delete(requestId);
          resolve({ ok: false, error: `Native host did not acknowledge ${command}.` });
        }, timeoutMs);
        this.pending.set(requestId, { resolve, timeout });
        try {
          this.port.postMessage({
            command,
            job_id: this.jobId,
            request_id: requestId,
            ...payload
          });
        } catch (error) {
          clearTimeout(timeout);
          this.pending.delete(requestId);
          resolve({ ok: false, error: error.message || "Could not control download." });
        }
      });
    }

    handleMessage(response) {
      if (response?.job_id && response.job_id !== this.jobId) return;
      if (response?.request_id) {
        const pending = this.pending.get(response.request_id);
        if (pending) {
          clearTimeout(pending.timeout);
          this.pending.delete(response.request_id);
          pending.resolve(response);
        }
      }
      this.onEvent(response || {});
      if (TERMINAL_STATES.has(response?.state)) this.finish(response);
    }

    handleDisconnect() {
      if (this.settled) return;
      const message = this.disconnectError?.() || "native host disconnected";
      this.fail(new Error(message));
    }

    finish(response) {
      if (this.settled) return;
      this.settled = true;
      this.rejectPending("Download task finished before the command was acknowledged.");
      this.resolveCompletion(response);
      this.port.disconnect();
    }

    fail(error) {
      if (this.settled) return;
      this.settled = true;
      this.rejectPending(error.message);
      this.rejectCompletion(error);
    }

    rejectPending(error) {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timeout);
        pending.resolve({ ok: false, error });
      }
      this.pending.clear();
    }
  }

  return { NativeTaskChannel, TERMINAL_STATES };
})();

if (typeof module !== "undefined") module.exports = DownerTaskProtocol;
