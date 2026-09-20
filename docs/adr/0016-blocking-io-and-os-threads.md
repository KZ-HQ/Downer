# ADR-0016: Blocking I/O on OS threads, and no async runtime in this project's own code

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-63](https://linear.app/kzhq/issue/KEI-63)

## Context

This decision predates the ADR directory and is reconstructed from the code
rather than from a fresh evaluation. It is worth recording because it is the
kind of choice a contributor reverses by accident: reaching for `tokio` to add
one concurrent fetch is a small diff that changes the shape of every function
it touches.

`Cargo.toml` takes `reqwest` with `default-features = false` and the `blocking`
feature. There is no `async fn` in `src/`, no executor, and no `#[tokio::main]`.
Concurrency is `std::thread::spawn` with `Arc<Mutex<…>>` for shared state —
four spawn sites in the whole project.

**`tokio` is nevertheless in `Cargo.lock`**, at 1.53.1. `reqwest::blocking`
drives a current-thread runtime internally and hands a synchronous API over it.
So the decision is not "no async runtime in the process". It is that no async
runtime appears in this project's own code, and no function here is written in
terms of one.

## What this project actually does concurrently

Everything the work is made of is a blocking wait on a child process or a
socket, and there are four of them:

| Thread | What it waits on | Where |
| --- | --- | --- |
| the download worker | one FFmpeg process, start to exit | `src/native.rs:568` |
| the HLS preflight probe | one playlist request, with an 8 s timeout | `src/native.rs:666` |
| progress capture | FFmpeg's `-progress` pipe | `src/ffmpeg.rs:609` |
| log capture | FFmpeg's stderr | `src/ffmpeg.rs:611` |

The native host's main thread does a fifth blocking wait, on Firefox's stdin
(`run_stdio`, `src/native.rs:296`), and the CLI does none of this at all: one
foreground download, two capture threads.

Four blocked threads cost four stacks. An async runtime earns its complexity
when the count is in the thousands and the memory is the constraint; ADR-0013
measured this host's entire idle footprint at ~3.5 MB, and it sits beside an
FFmpeg process that dwarfs it.

## Decision

**Blocking calls on OS threads. No `async` in `src/`.**

The reason is not that async would be slow. It is that async is *not available
for the part that matters*. This project's concurrency is dominated by waiting
on a child process — and a child process is not a future. Reaping it, reading
its pipes, signalling it for the pause and resume of ADR-0012: all of it is
`std::process` and `libc`, all of it blocking. An async runtime would leave that
half exactly as it is and convert only the playlist fetch, which is one request
with a timeout.

So the trade is a real cost against a benefit this workload cannot collect.
The cost is function colouring — every caller of an `async fn` becomes one, or
must block on a runtime handle — reached through a crate that pulls in an
executor, and error handling that has to distinguish a cancelled task from a
failed one. The benefit would be the ability to hold many more concurrent waits
than this project has any use for.

The rejected alternative was `reqwest`'s default async client with a runtime in
the host. It would have meant an executor in the process purely to serve the
scraper, with the FFmpeg supervision — the actual work — still on threads
beside it, and two concurrency models in one binary instead of one.

`Arc<Mutex<…>>` follows from the same place. The shared state is a handful of
cells written rarely and read on event boundaries: the active job slot, the
latest progress, the resolved output path, a lock around stdout so two threads
cannot interleave a frame. Nothing here is contended enough for a lock to be
the interesting part.

## Consequences

* A contributor adding a concurrent segment fetcher (KEI-68, KEI-69) must use
  a bounded thread pool, not an executor, or change this decision with a record
  that says so. `AGENTS.md` already requires that pool to be bounded and
  order-preserving; this is the reason it says "worker pool".
* `tokio` stays in the dependency tree via `reqwest::blocking` whatever this
  project does, so its presence in `Cargo.lock` is not evidence that the
  decision has been reversed. The evidence would be an `async fn` in `src/`.
* `reqwest` is taken with `default-features = false` and `rustls-tls`, so no
  system OpenSSL is needed and the MSRV in `Cargo.toml` stays a function of the
  pinned versions rather than of a C toolchain.
* Every blocking wait needs its own timeout, since no runtime imposes one. The
  preflight probe has an explicit 8 s bound; **FFmpeg does not**, which is the
  gap ADR-0012 names — a server that accepts a socket and never answers leaves
  the job sitting there.

## Unverified

* **No alternative was benchmarked.** No async variant of this host exists to
  compare against, and the argument here is about which model fits child-process
  supervision, not about throughput. The one number quoted (~3.5 MB idle) is
  ADR-0013's measurement of the whole process, not of thread stacks.
* **The thread count is from reading the code**, not from observing a running
  host under load. A download that fans out per segment would change it, which
  is exactly what KEI-68 proposes.
