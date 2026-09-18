#!/usr/bin/env python3
"""A local media origin that requires a session cookie.

Why this exists
---------------

`tests/cookie_scope.rs` drives FFmpeg directly at a loopback server, which
proves FFmpeg's cookie scoping but skips the extension entirely. The remaining
question is whether the *whole* path holds: Firefox's cookie jar →
`browser.cookies.getAll` → the native messaging port → the host → FFmpeg → a
server that actually refuses requests without the cookie.

Answering it needs a cookie-gated media URL, and a real one is hard to come by.
An unprotected URL proves nothing, because a silently dropped cookie still
downloads fine; most public media is either unprotected or protected by signed
URLs rather than cookies. This serves one locally instead.

It can also do something no real site can: show the leak being *closed*. Run two
instances on two host strings, point one's playlist at the other's segments, and
the cross-host download succeeds on `main` — where a raw `Cookie:` line in
`-headers` reaches every host FFmpeg touches — and fails once cookies are scoped
per host. That is a positive demonstration of the security property, not merely
the absence of a regression.

Usage
-----

Two instances, each knowing the other, is the interesting configuration::

    make fixture-site        # localhost:8080, peer 127.0.0.1:8081
    make fixture-site-peer   # 127.0.0.1:8081, peer localhost:8080

Then open http://localhost:8080/ in Firefox. The landing page sets the session
cookie and carries the media, so the extension discovers it by scanning, exactly
as it would on a real page.

`localhost` and `127.0.0.1` are one network but two host *strings*, which is
what cookie matching compares — so this needs no DNS and no `/etc/hosts` entry.

Each instance titles its landing page after its own host and port, and serves the
same playlist twice: once as `same-host.m3u8` and once as `index.m3u8`. That
second name is the point. `index` is on the generic-stem list in `src/output.rs`,
so a download of it must be named from the page title rather than the URL — and
since the two instances have different titles, the two origins produce two
different filenames from an identically named playlist. That is KEI-60's
acceptance criterion made reproducible; before the `index.m3u8` route existed,
every playlist here had a distinctive stem and a manual pass could look healthy
while never exercising title naming at all.

It also serves a *signed* playlist, whose segment URLs carry a token in their
query string, the way a real CDN's do. That is what makes KEI-55's redaction
checkable end to end: FFmpeg echoes `Opening '<url>' for reading` per segment at
`-loglevel info`, the host forwards each line, and the extension persists it — so
after downloading `/media/signed.m3u8`, the Settings page must show `?…` and
never the token. Nothing automated can make that check, because it needs a real
FFmpeg and a real browser.

Secrets
-------

The request log records whether a `Cookie` header arrived, never its value, and
prints request paths with their query string replaced by `?…` — the same rule
`src/redact.rs` and `extension/redact.js` apply. AGENTS.md forbids logging
cookie values, and a server whose whole purpose is handling them is the easiest
place to get that wrong. The signed token below is a sentinel with no secrecy to
lose, but logging it would still teach the wrong habit in the one file where it
matters most.

Dependencies
------------

Python standard library only, in keeping with a repository that ships an
extension with no dependencies and keeps its own list short. FFmpeg is used once
to generate the media, and is already a required runtime dependency.
"""

from __future__ import annotations

import argparse
import html
import shutil
import subprocess
import sys
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

# A sentinel, never anything resembling a real session. Tests and logs may name
# it; that is the point of choosing a value with no secrecy to lose.
COOKIE_NAME = "downer_fixture_session"
COOKIE_VALUE = "not-a-real-session"

# The query-string token the signed playlist carries. A sentinel, like the cookie
# value above: this exists to be looked for in logs, not to be kept out of them.
SIGNED_TOKEN = "fixture-signed-token-not-a-real-secret"
SIGNED_EXPIRY = "1790000000"

SEGMENT_COUNT = 4
SEGMENT_SECONDS = 2


class Media:
    """The generated media, cached on disk between runs."""

    def __init__(self, directory: Path, ffmpeg: str) -> None:
        self.directory = directory
        self.ffmpeg = ffmpeg

    @property
    def video(self) -> Path:
        return self.directory / "video.mp4"

    def segment(self, index: int) -> Path:
        return self.directory / f"segment{index}.ts"

    def ensure(self) -> None:
        """Generate the media if it is not already there.

        Nothing is committed: real media has no place in the repository, and
        FFmpeg is a required runtime dependency anyway, so generating is cheaper
        than carrying binaries.
        """
        if self.video.exists() and self.segment(SEGMENT_COUNT - 1).exists():
            return
        if shutil.which(self.ffmpeg) is None and not Path(self.ffmpeg).is_file():
            sys.exit(
                f"error: {self.ffmpeg} is not available, and this fixture needs it once to\n"
                "       generate the media it serves. Install FFmpeg, or point --ffmpeg at it."
            )
        self.directory.mkdir(parents=True, exist_ok=True)
        duration = SEGMENT_COUNT * SEGMENT_SECONDS
        print(f"generating {duration}s of test media in {self.directory} ...", flush=True)
        self._run([
            "-f", "lavfi", "-i", f"testsrc=size=320x240:rate=15:duration={duration}",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", str(15 * SEGMENT_SECONDS),
            "-y", str(self.video),
        ])
        self._run([
            "-i", str(self.video), "-c", "copy",
            "-f", "segment", "-segment_time", str(SEGMENT_SECONDS),
            "-segment_format", "mpegts",
            "-y", str(self.directory / "segment%d.ts"),
        ])
        print("media ready", flush=True)

    def _run(self, arguments: list[str]) -> None:
        result = subprocess.run(
            [self.ffmpeg, "-hide_banner", "-loglevel", "error", *arguments],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            sys.exit(f"error: FFmpeg failed generating media:\n{result.stderr.strip()}")


class Site:
    """Configuration shared by every request."""

    def __init__(self, host: str, port: int, peer: str | None, media: Media) -> None:
        self.host = host
        self.port = port
        self.peer = peer.rstrip("/") if peer else None
        self.media = media

    @property
    def origin(self) -> str:
        return f"http://{self.host}:{self.port}"

    @property
    def peer_host(self) -> str | None:
        return urlsplit(self.peer).hostname if self.peer else None


def landing_page(site: Site) -> bytes:
    """A page the extension's own media scan can read.

    The media is reachable through `src` and `href` attributes rather than being
    pasted in by hand, so this exercises `extension/media-scan.js` too.
    """
    peer_section = (
        f"""
    <h2>Cross-host HLS</h2>
    <p>
      This playlist is served here, but its segments live on
      <code>{html.escape(site.peer_host or "")}</code> — a different host, which
      also requires a cookie it will never be sent once cookies are scoped.
      Downloading it succeeds on <code>main</code> and fails once
      <code>-cookies</code> replaces <code>-headers</code>. That failure is the
      leak being closed, not a bug.
    </p>
    <p><a href="/media/cross-host.m3u8">cross-host.m3u8</a></p>
"""
        if site.peer
        else """
    <h2>Cross-host HLS</h2>
    <p>Not available: start a second instance and pass <code>--peer</code>.</p>
"""
    )

    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Downer fixture — {html.escape(site.host)}:{site.port}</title>
  <style>
    body {{ font: 16px/1.5 system-ui, sans-serif; margin: 2rem auto; max-width: 44rem; }}
    code {{ background: #eee; padding: 0.1em 0.3em; border-radius: 3px; }}
    video {{ width: 100%; background: #000; }}
  </style>
</head>
<body>
  <h1>Downer fixture — {html.escape(site.host)}:{site.port}</h1>
  <p>
    Loading this page set a session cookie for <code>{html.escape(site.host)}</code>.
    Everything under <code>/media/</code> answers <code>403</code> without it, so a
    download only succeeds if the cookie travelled the whole way to FFmpeg.
  </p>

  <h2>Direct file</h2>
  <video src="/media/video.mp4" controls></video>

  <h2>Same-host HLS</h2>
  <p><a href="/media/same-host.m3u8">same-host.m3u8</a></p>

  <h2>Generically named HLS</h2>
  <p>
    The same playlist served as <code>index.m3u8</code>, which is what most of
    the web calls its playlists and is therefore the name <em>Downer</em> refuses
    to take a filename from. Downloading this must produce a file named after
    this page's title, not <code>index.mp4</code> — and because that title names
    this instance's host and port, the two fixture origins produce two different
    filenames from the same playlist name. That is KEI-60's acceptance criterion,
    reproducibly, without depending on what a live site happens to serve.
  </p>
  <p><a href="/media/index.m3u8">index.m3u8</a></p>

  <h2>Signed HLS</h2>
  <p>
    The segments in this playlist carry a token in their query string, as a real
    CDN's would. Download it, then open Downer's Settings page: every logged
    <code>Opening &hellip; for reading</code> line must end in
    <code>?&hellip;</code>, with no token anywhere. That is KEI-55's redaction,
    end to end — FFmpeg's real output, a real native port, and a real browser.
  </p>
  <p><a href="/media/signed.m3u8">signed.m3u8</a></p>
{peer_section}
  <h2>Redirect</h2>
  <p>
    <a href="/redirect/video.mp4">/redirect/video.mp4</a> answers a 302 to the
    other host, for the cross-host redirect case.
  </p>
</body>
</html>
""".encode()


def playlist(urls: list[str]) -> bytes:
    lines = ["#EXTM3U", "#EXT-X-VERSION:3", f"#EXT-X-TARGETDURATION:{SEGMENT_SECONDS}",
             "#EXT-X-MEDIA-SEQUENCE:0", "#EXT-X-PLAYLIST-TYPE:VOD"]
    for url in urls:
        lines.append(f"#EXTINF:{SEGMENT_SECONDS}.0,")
        lines.append(url)
    lines.append("#EXT-X-ENDLIST")
    return ("\n".join(lines) + "\n").encode()


def redact_path(path: str) -> str:
    """A request path with its query replaced, for the log.

    The same rule as `src/redact.rs` and `extension/redact.js`: keep the path,
    mark that there was a query, print none of it.
    """
    split = urlsplit(path)
    return f"{split.path}?…" if split.query else split.path


class Handler(BaseHTTPRequestHandler):
    site: Site  # set on the subclass created in serve()

    protocol_version = "HTTP/1.1"
    server_version = "downer-fixture"
    sys_version = ""

    # --- logging ------------------------------------------------------------

    def log_message(self, format: str, *args) -> None:  # noqa: A002 - base class name
        """Silence the default line; `respond` logs with the cookie state."""

    def log_request_outcome(self, status: int) -> None:
        cookie = "cookie" if self.has_cookie() else "NO COOKIE"
        print(
            f"{self.site.host}:{self.site.port}  {self.command} {redact_path(self.path)}"
            f"  -> {status}  [{cookie}]",
            flush=True,
        )

    # --- helpers ------------------------------------------------------------

    def has_cookie(self) -> bool:
        """Is our session cookie present?

        Only the name is compared and only a boolean is kept. The value is never
        read into a message, a log line, or an error.
        """
        header = self.headers.get("Cookie")
        if not header:
            return False
        return any(
            pair.strip().startswith(f"{COOKIE_NAME}=") for pair in header.split(";")
        )

    def respond(self, status: int, body: bytes = b"", content_type: str | None = None,
                extra_headers: list[tuple[str, str]] | None = None) -> None:
        self.send_response(status)
        if content_type:
            self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for name, value in extra_headers or []:
            self.send_header(name, value)
        self.end_headers()
        if self.command != "HEAD" and body:
            self.wfile.write(body)
        self.log_request_outcome(status)

    def has_signed_token(self, query: str) -> bool:
        """Is the signed playlist's token present? Compared, never logged."""
        return any(pair == f"token={SIGNED_TOKEN}" for pair in query.split("&"))

    def deny_token(self) -> None:
        self.respond(
            HTTPStatus.FORBIDDEN,
            b"403: this segment requires the signed token from /media/signed.m3u8.\n",
            "text/plain; charset=utf-8",
        )

    def deny(self) -> None:
        self.respond(
            HTTPStatus.FORBIDDEN,
            b"403: this media requires a session cookie. Load / first.\n",
            "text/plain; charset=utf-8",
        )

    def send_file(self, path: Path, content_type: str) -> None:
        if not path.is_file():
            self.respond(HTTPStatus.NOT_FOUND, b"404\n", "text/plain; charset=utf-8")
            return
        self.respond(HTTPStatus.OK, path.read_bytes(), content_type)

    # --- routing ------------------------------------------------------------

    def do_HEAD(self) -> None:  # noqa: N802 - base class name
        self.do_GET()

    def do_GET(self) -> None:  # noqa: N802 - base class name
        site = self.site
        path = urlsplit(self.path).path

        if path == "/":
            self.respond(
                HTTPStatus.OK,
                landing_page(site),
                "text/html; charset=utf-8",
                [("Set-Cookie", f"{COOKIE_NAME}={COOKIE_VALUE}; Path=/; SameSite=Lax")],
            )
            return

        if path == "/logout":
            self.respond(
                HTTPStatus.FOUND,
                b"",
                None,
                [
                    ("Set-Cookie", f"{COOKIE_NAME}=; Path=/; Max-Age=0"),
                    ("Location", "/"),
                ],
            )
            return

        # Everything below is gated.
        if not path.startswith(("/media/", "/redirect/")):
            self.respond(HTTPStatus.NOT_FOUND, b"404\n", "text/plain; charset=utf-8")
            return

        if not self.has_cookie():
            self.deny()
            return

        if path == "/redirect/video.mp4":
            target = f"{site.peer}/media/video.mp4" if site.peer else "/media/video.mp4"
            self.respond(HTTPStatus.FOUND, b"", None, [("Location", target)])
            return

        if path == "/media/video.mp4":
            self.send_file(site.media.video, "video/mp4")
            return

        if path == "/media/index.m3u8":
            # Deliberately the same playlist as `same-host.m3u8` under a generic
            # name. The stem is what matters: `index` is on the generic list in
            # `src/output.rs`, so naming falls through to the page title, which
            # is what KEI-60 exists to make happen. Serving only distinctively
            # named playlists here would let a manual pass look healthy while
            # never exercising that path at all.
            self.respond(
                HTTPStatus.OK,
                playlist([f"{site.origin}/media/segment{i}.ts" for i in range(SEGMENT_COUNT)]),
                "application/vnd.apple.mpegurl",
            )
            return

        if path == "/media/same-host.m3u8":
            self.respond(
                HTTPStatus.OK,
                playlist([f"{site.origin}/media/segment{i}.ts" for i in range(SEGMENT_COUNT)]),
                "application/vnd.apple.mpegurl",
            )
            return

        if path == "/media/cross-host.m3u8":
            if not site.peer:
                self.respond(
                    HTTPStatus.NOT_FOUND,
                    b"404: start a second instance and pass --peer\n",
                    "text/plain; charset=utf-8",
                )
                return
            self.respond(
                HTTPStatus.OK,
                playlist([f"{site.peer}/media/segment{i}.ts" for i in range(SEGMENT_COUNT)]),
                "application/vnd.apple.mpegurl",
            )
            return

        if path == "/media/signed.m3u8":
            # Segment URLs with a token in the query, the way a real CDN signs
            # them. Downloading this is what makes KEI-55's redaction checkable
            # end to end: FFmpeg logs each of these URLs, so the Settings page
            # must end up showing `?…` and never the token.
            self.respond(
                HTTPStatus.OK,
                playlist([
                    f"{site.origin}/media/segment{i}.ts"
                    f"?token={SIGNED_TOKEN}&e={SIGNED_EXPIRY}"
                    for i in range(SEGMENT_COUNT)
                ]),
                "application/vnd.apple.mpegurl",
            )
            return

        if path.startswith("/media/segment") and path.endswith(".ts"):
            index = path[len("/media/segment"):-len(".ts")]
            if index.isdigit():
                # A segment reached through the signed playlist must present the
                # token, so the query is load-bearing rather than decorative: a
                # redaction that removed it from the URL FFmpeg *requests*, and
                # not merely from the line it logs, would fail the download here.
                query = urlsplit(self.path).query
                if query and not self.has_signed_token(query):
                    self.deny_token()
                    return
                self.send_file(site.media.segment(int(index)), "video/mp2t")
                return

        self.respond(HTTPStatus.NOT_FOUND, b"404\n", "text/plain; charset=utf-8")


def serve(site: Site) -> None:
    handler = type("SiteHandler", (Handler,), {"site": site})
    server = ThreadingHTTPServer((site.host, site.port), handler)
    server.daemon_threads = True

    print(f"\n  {site.origin}/", flush=True)
    if site.peer:
        print(f"  peer: {site.peer}", flush=True)
    else:
        print("  peer: none (pass --peer for the cross-host cases)", flush=True)
    print("\n  Open the origin in Firefox to receive the session cookie, then use", flush=True)
    print("  the Downer toolbar button. Ctrl-C to stop.\n", flush=True)

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped", flush=True)
    finally:
        server.server_close()


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Serve cookie-gated media for end-to-end verification.",
        epilog="See the module docstring for what this is for and how to read the results.",
    )
    parser.add_argument("--host", default="localhost",
                        help="host to bind and to advertise in URLs (default: localhost)")
    parser.add_argument("--port", type=int, default=8080, help="port to bind (default: 8080)")
    parser.add_argument("--peer", default=None,
                        help="origin of the other instance, e.g. http://127.0.0.1:8081")
    parser.add_argument("--media-dir", type=Path,
                        default=Path(__file__).resolve().parent / "media",
                        help="where generated media is cached (git-ignored)")
    parser.add_argument("--ffmpeg", default="ffmpeg",
                        help="FFmpeg used once to generate the media (default: ffmpeg)")
    arguments = parser.parse_args()

    media = Media(arguments.media_dir, arguments.ffmpeg)
    media.ensure()
    serve(Site(arguments.host, arguments.port, arguments.peer, media))


if __name__ == "__main__":
    main()
