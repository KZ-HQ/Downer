/* global browser, DownerJobState */

/**
 * Client half of the native messaging contract specified in `docs/protocol.md`
 * and decided in `docs/adr/0001-native-messaging-protocol.md`. The wire
 * vocabulary is pinned by `tests/fixtures/protocol.json`, which this file's
 * Node tests and the Rust host's integration tests both read.
 */
var DownerTaskProtocol = (() => {
  const PROTOCOL_VERSION = 1;

  /**
   * States that end a job on the wire. Only these settle a channel, and only for
   * its own job. Derived from `extension/job-state.js` so there is one
   * definition, and deliberately the *wire* terminal set: the extension's own
   * `interrupted` state never appears on this channel.
   */
  const TERMINAL_STATES = typeof DownerJobState !== "undefined"
    ? DownerJobState.WIRE_TERMINAL_STATE_SET
    : require("./job-state.js").WIRE_TERMINAL_STATE_SET;
  /**
   * Connection-level states. They describe a request, not a job, and must never
   * terminate a channel — that is what let a malformed control message end a
   * running download before the protocol was versioned.
   */
  const CONNECTION_STATES = new Set(["ready", "rejected", "control-error"]);

  /**
   * Decide whether a `hello` response comes from a host this extension can talk
   * to. Pure, so the Node tests can cover it without a port.
   */
  function protocolCompatibility(hello) {
    if (!hello || hello.ok === false) {
      return {
        ok: false,
        error: hello?.error
          || "The Downer native host did not answer the protocol handshake."
      };
    }
    const version = hello.protocol_version;
    if (!Number.isInteger(version)) {
      return {
        ok: false,
        error: "The Downer native host is too old: it reports no protocol version. "
          + "Rebuild it with `make extension`."
      };
    }
    if (version !== PROTOCOL_VERSION) {
      return {
        ok: false,
        error: `The Downer native host speaks protocol version ${version}, but this `
          + `extension needs version ${PROTOCOL_VERSION}. Rebuild the native host with `
          + "`make extension`, or install a matching extension version."
      };
    }
    return {
      ok: true,
      hostVersion: hello.host_version || null,
      capabilities: hello.capabilities || {}
    };
  }

  class NativeTaskChannel {
    constructor(port, jobId, onEvent, disconnectError) {
      this.port = port;
      this.jobId = jobId;
      this.onEvent = onEvent;
      this.disconnectError = disconnectError;
      this.pending = new Map();
      this.nextRequest = 1;
      this.settled = false;
      this.startRequestId = null;
      this.completion = new Promise((resolve, reject) => {
        this.resolveCompletion = resolve;
        this.rejectCompletion = reject;
      });
      // A host that disconnects during the handshake rejects `completion` before
      // anything awaits it; mark it handled so that is not an unhandled rejection.
      this.completion.catch(() => undefined);
      port.onMessage.addListener((response) => this.handleMessage(response));
      port.onDisconnect.addListener(() => this.handleDisconnect());
    }

    /** Connection handshake. Sent before any download, per `docs/protocol.md`. */
    hello(timeoutMs = 5000) {
      return this.send("hello", {}, timeoutMs, false);
    }

    start(request) {
      this.startRequestId = `${this.jobId}-start`;
      this.port.postMessage({
        protocol_version: PROTOCOL_VERSION,
        ...request,
        job_id: this.jobId,
        request_id: this.startRequestId
      });
      return this.completion;
    }

    request(command, payload = {}, timeoutMs = 5000) {
      return this.send(command, payload, timeoutMs, true);
    }

    send(command, payload, timeoutMs, includeJobId) {
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
            protocol_version: PROTOCOL_VERSION,
            command,
            ...(includeJobId ? { job_id: this.jobId } : {}),
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
      const event = response || {};
      if (event.job_id && event.job_id !== this.jobId) return;
      if (event.request_id) {
        const pending = this.pending.get(event.request_id);
        if (pending) {
          clearTimeout(pending.timeout);
          this.pending.delete(event.request_id);
          pending.resolve(event);
        }
      }
      this.onEvent(event);
      if (event.type === "rejected" || event.state === "rejected") {
        // A rejection answers one request. It ends the job only when the request
        // it rejects is the one that would have started the job.
        if (event.request_id && event.request_id === this.startRequestId) {
          this.fail(new Error(event.error || "The native host rejected the download."));
        }
        return;
      }
      // A terminal state settles this channel only when it names this job, so an
      // unattributed failure can never end a download that is still running.
      if (TERMINAL_STATES.has(event.state) && event.job_id === this.jobId) this.finish(event);
    }

    handleDisconnect() {
      if (this.settled) return;
      const message = this.disconnectError?.() || "native host disconnected";
      this.fail(new Error(message));
    }

    /** Give up on a channel that never started a job (a failed handshake). */
    close(reason = "The native host connection was closed.") {
      this.fail(new Error(reason));
      try {
        this.port.disconnect();
      } catch (_) {
        // The port may already be gone; closing is best effort.
      }
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

  return {
    NativeTaskChannel,
    TERMINAL_STATES,
    CONNECTION_STATES,
    PROTOCOL_VERSION,
    protocolCompatibility
  };
})();

if (typeof module !== "undefined") module.exports = DownerTaskProtocol;
