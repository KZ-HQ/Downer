# ADR-0002: Accept cookie exposure in FFmpeg's argv, and scope cookies at the host rather than in the wire protocol

* Status: Accepted
* Date: 2026-09-17
* Issue: [KEI-54](https://linear.app/kzhq/issue/KEI-54)

## Context

The cookie path — page session → extension → native host → FFmpeg → network —
is what makes protected downloads work and is also the product's main security
boundary. Two distinct problems sit on it.

**Cookies reach hosts they do not belong to.** `extension/background.js`
collects cookies with `browser.cookies.getAll({ url: media.url })`, so the *set*
of cookies is already scoped to the media URL. It sends them as one flat
`cookie` string, which `src/scraper.rs::ffmpeg_headers` renders as a raw
`Cookie:` line inside FFmpeg's `-headers` block. FFmpeg applies that block to
**every** HTTP request it makes for an input. A redirect from the media host to
another host carries the cookie along, and for HLS every segment and key
request does too, including ones on a different domain. The scoping the
extension did is discarded the moment the header block is built.

FFmpeg's HTTP protocol also accepts `-cookies`, which takes Set-Cookie syntax
with `domain=` and `path=` and is matched per request host in its `http.c`.
That is the obvious fix, provided the matching actually holds across a redirect
and inside the HLS demuxer.

**The cookie is visible in process arguments.** Whether it is passed as
`-headers` or as `-cookies`, the value becomes an element of FFmpeg's argv and
is readable by any process on the machine that can see the process list. For
the CLI, `--cookie` additionally puts it in shell history. FFmpeg has no
file-based input for headers or cookies, and no environment variable for them,
so nothing we pass to FFmpeg can avoid argv.

## Decision

### Accept the argv exposure, and shrink the surface around it

Cookie values remain visible in FFmpeg's argv. The alternative — routing media
requests through the native host so FFmpeg is never given a cookie at all — is a
rewrite of the download path, and the work is already specified elsewhere: the
native concurrent HLS scheduler (KEI-68 design, KEI-70 implementation) has the
host fetch segments itself. Duplicating that here would collide with it.

The threat model this accepts is a local attacker who can already list another
user's processes. On a single-user desktop, which is what Downer targets, such
an attacker has the browser profile — and therefore the cookies — by easier
means. The exposure is real but it is not the weakest link.

What is *not* accepted is putting the value anywhere it need not be:

* The CLI gains `--cookie-file PATH` and the `DOWNER_COOKIE` environment
  variable, so the value need never enter shell history. Precedence is
  `--cookie`, then `--cookie-file`, then `DOWNER_COOKIE`; `--cookie` together
  with `--cookie-file` is rejected by the argument parser with exit code 2.
  `--cookie` is kept, and its help text says why the other two are preferable.
* No cookie value appears in a log line, an error message, a host event, or a
  persisted job record. `tests/cli.rs` and `tests/native_host.rs` grep a
  sentinel through stdout, stderr, and every event the host emits.
* Header values are stripped of ASCII control characters before the `-headers`
  block is assembled. The block is built by concatenation, so a cookie or
  User-Agent carrying CRLF could previously append headers of its own choosing.

`tests/cli.rs::a_cookie_value_is_still_visible_in_ffmpeg_argv` pins the accepted
exposure as a decision rather than an oversight. When KEI-70 lands and FFmpeg
stops being handed cookies, that test should fail and be deleted, and this ADR
superseded.

### Scope cookies at the host, from the media URL — no protocol change

When domain-scoped `-cookies` is adopted, the host will derive the scope from
the media `url` it already receives, and render one `-cookies` entry per cookie
with `domain=` set to that URL's host.

The alternative considered was the one KEI-54 originally proposed: have the
extension send each cookie's `domain`, `path`, and `secure` attributes, which
`cookies.getAll` already returns. That is more faithful to cookie semantics — it
would honour a `.example.com` cookie across a same-site redirect, and the
`secure` flag over plain HTTP. It also costs a new field on the `download`
request, a `protocol_version` bump to 2, an ADR of its own (any protocol change
needs one, per ADR-0001 and AGENTS.md), and updates to `docs/protocol.md`,
`tests/fixtures/protocol.json`, and both test suites.

Host-side scoping buys most of the security benefit for none of that. Because
`getAll` was already filtered by the media URL, every cookie in the flat string
is by construction valid for the media host, so scoping them all to that host
is correct — and strictly tighter than today, where they are sent to every host
FFmpeg touches. If local testing shows that bluntness breaks a real site, the
structured field is the fallback, and it gets its own ADR then.

### The behaviour change itself is deferred until verified

**The `-headers` → `-cookies` switch is not in this change.** Nothing in this
repository can verify it: FFmpeg is deliberately not installed in CI
(see AGENTS.md), both test suites generate fake FFmpeg scripts, and the
environment KEI-54 was implemented in has no FFmpeg and no way to install one.
Switching the main download path to an option whose behaviour nobody has
observed would risk breaking every protected HLS download, and the failure
would surface only in a browser.

So what landed in KEI-54 is the apparatus to settle it: `tests/support/mod.rs`
is a dependency-free loopback HTTP server that records the headers it receives,
and `tests/cookie_scope.rs` drives a real FFmpeg against two of them — bound to
`localhost` and `127.0.0.1`, which are one network but two host strings —
through a redirect and through a cross-host HLS segment, with `-headers` and
with `-cookies`. Without a real FFmpeg those tests skip, loudly.

### Verified: FFmpeg 9.0.1, macOS/arm64, 2026-09-17

That run has since happened, on the reporter's machine:

```text
redirect/headers: media host received cookie = true,  redirect target = true
redirect/cookies: media host received cookie = true,  redirect target = false
hls/headers:      playlist host received cookie = true, segment host  = true
hls/cookies:      playlist host received cookie = true, segment host  = false
```

**Domain-scoped `-cookies` is confirmed.** The cookie reaches the host it is
scoped to and stays off both a redirect target and a cross-host HLS segment
server. The scoping propagates into the HLS demuxer, which was the part most in
doubt. `-headers` leaks in both cases, so the problem this ADR describes is
real and not theoretical.

#### The `domain=` value is the URL authority, not the hostname

The first run appeared to refute the whole approach — the cookie reached
*nobody*, not even its own host. That was a malformed argument, not a
limitation. FFmpeg's `get_cookies()` keeps a cookie only when its `domain=` is a
suffix of the string `http_open_cnx_internal()` builds with

```c
ff_url_join(hoststr, sizeof(hoststr), NULL, NULL, tmp_host, port, NULL);
```

and that call sits *before* the `if (port < 0) port = 443/80` defaulting. So
`port` is whatever `av_url_split()` found in the URL — `-1` when the URL states
none, for which `ff_url_join` appends nothing. The rule is the URL's authority
**exactly as written**:

| media URL | required `domain=` |
| --- | --- |
| `http://localhost:60254/v.mp4` | `localhost:60254` |
| `https://cdn.example.test/v.mp4` | `cdn.example.test` (no `:443`) |

In Rust that is `Url::port()`, which is `None` for a default port — not
`port_or_known_default()`.

**A mismatch is silent.** FFmpeg makes the request and simply omits the cookie,
with no warning even at `-loglevel verbose`. Getting this wrong in production
therefore breaks protected downloads with no diagnostic, which is the single
sharpest edge in adopting `-cookies`.

One residual: every run used an explicit, non-default port, because the fixture
server binds an ephemeral one. That the rule also holds for an implicit `:443`
is read off the source above, not observed.

KEI-78 should therefore emit **both** spellings as two newline-delimited
entries — `domain=host` and `domain=host:port`. `domain=` is matched against one
authority string, so at most one can ever match and the other is skipped, which
makes the form immune both to the default-port question and to a future FFmpeg
changing which string it builds. That form is **confirmed** by the same probe
run: FFmpeg accepts newline-delimited entries, and carrying a non-matching one
alongside does not stop the matching one being sent.

## Consequences

* A cookie can be supplied without entering shell history, and cannot inject a
  second header line. Neither property depended on FFmpeg semantics, so both
  ship now.
* Cookies still reach redirect targets and cross-host HLS segments. The fix is
  now verified rather than merely proposed, but it is KEI-78 that applies it;
  this ADR changed no cookie rendering.
* `tests/cookie_scope.rs` passes in CI without proving anything about FFmpeg,
  because it skips there. A green CI run is not evidence; only a local run is,
  and the result of one is recorded above.
* The wire protocol is unchanged and stays at version 1. The extension is
  unchanged by KEI-54.
* One gap is out of scope here: if FFmpeg itself echoes a token-bearing URL
  through its stderr, the host forwards that line as a log event and the
  extension persists it. Redacting FFmpeg's own output before persistence is
  KEI-55.
* KEI-52 (structured FFmpeg command builder) touches the same argv-building
  code. Nothing here restructures it — `ffmpeg_headers` keeps its signature and
  `FfmpegCommand` is untouched — so the two remain separable.
