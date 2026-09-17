//! Does FFmpeg send a forwarded cookie to hosts it does not belong to?
//!
//! `src/scraper.rs::ffmpeg_headers` renders the cookie as a `Cookie:` line in
//! FFmpeg's `-headers` block. FFmpeg applies that block to *every* HTTP request
//! it makes for an input, so a redirect to another host, or an HLS segment on
//! another host, receives the media host's session cookie. `-cookies` takes
//! Set-Cookie syntax with `domain=` and `path=`, which FFmpeg's `http.c` matches
//! against each request's host — if that matching holds across redirects and
//! inside the HLS demuxer, it is the fix.
//!
//! # Result: domain-scoped `-cookies` is confirmed (FFmpeg 9.0.1, macOS/arm64)
//!
//! ```text
//! redirect/headers: media host received cookie = true,  redirect target = true
//! redirect/cookies: media host received cookie = true,  redirect target = false
//! hls/headers:      playlist host received cookie = true, segment host  = true
//! hls/cookies:      playlist host received cookie = true, segment host  = false
//! ```
//!
//! `-cookies` reaches the host it is scoped to and stays off both a redirect
//! target and a cross-host HLS segment server; the scoping does propagate into
//! the HLS demuxer, which was the uncertain part. `-headers` leaks in both
//! cases, as ADR-0002 describes.
//!
//! The default-port case is covered too, by
//! `ffmpeg_cookie_scope_on_a_default_port` binding port 80 so the URL states no
//! port (opt-in; it needs `sudo`):
//!
//! ```text
//! default-port/headers: cookie arrived = true
//! default-port/cookies (downer's rendering): cookie arrived = true
//! default-port/cookies with an explicit :80: cookie arrived = false
//! ```
//!
//! So `domain=` is the authority *as written*, in both directions. The one step
//! still taken on faith is the scheme: the observed default port is 80 over
//! http, and that `:443` over https behaves the same follows from `ff_url_join`
//! running before either default is applied, which is one code path. See
//! `cookies_args`.
//!
//! # Running them
//!
//! FFmpeg is deliberately not installed in CI (see `AGENTS.md`), so every test
//! below that needs one **skips** there, printing a line beginning `SKIP:`.
//! A green CI run is therefore not evidence; only a local run is.
//!
//! ```sh
//! cargo test --test cookie_scope -- --nocapture
//! ```
//!
//! Nothing here fakes an FFmpeg: a fake would only tell us what we asked FFmpeg
//! to do, which is what the argv-recording fakes in `tests/cli.rs` and
//! `tests/native_host.rs` already prove. Only a real FFmpeg on a real socket
//! shows what reaches the wire.
//!
//! The two servers bind `localhost` and `127.0.0.1`. Both are loopback, but they
//! are different host *strings*, which is what FFmpeg's cookie matching compares
//! — so no DNS and no `/etc/hosts` entry is needed.

mod support;

use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

use support::{get, HeaderRecorder, Reply};

/// Never a real cookie. Tests grep for this; `AGENTS.md` forbids a real value.
const SENTINEL: &str = "downer_sentinel=KEI54-not-a-real-session";

/// Locate a real FFmpeg, or explain the skip and return `None`.
fn real_ffmpeg(test: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DOWNER_FFMPEG") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let found = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|directory| directory.join("ffmpeg"))
        .find(|candidate| candidate.is_file());
    if found.is_none() {
        eprintln!(
            "SKIP: {test} needs a real FFmpeg. Install one, or point DOWNER_FFMPEG at it, \
             and re-run: cargo test --test cookie_scope -- --nocapture"
        );
    }
    found
}

/// Run FFmpeg over `url` with the given cookie-passing arguments and return what
/// it wrote to stderr.
///
/// The exit status is ignored on purpose: the fixture server serves bytes that
/// are not decodable media, so FFmpeg is expected to fail. What is being
/// measured is which requests it made and what it put on them.
///
/// stderr is captured rather than discarded because it is the only place FFmpeg
/// explains itself. When a cookie reaches nobody at all, the difference between
/// "FFmpeg cannot scope cookies" and "this argument was spelled wrong" is
/// usually a line in here.
fn run_ffmpeg(ffmpeg: &PathBuf, cookie_args: &[&str], url: &str, output: &PathBuf) -> String {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "verbose", "-nostdin"]);
    command.args(cookie_args);
    command.args(["-i", url, "-c", "copy", "-y"]);
    command.arg(output);
    match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Err(error) => format!("(FFmpeg could not be started: {error})"),
    }
}

/// The tail of FFmpeg's stderr, for a failure message.
fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(12);
    lines[start..].join("\n    ")
}

/// `-headers` with a raw `Cookie:` line: today's behaviour.
fn headers_args(cookie: &str) -> Vec<String> {
    vec!["-headers".to_string(), format!("Cookie: {cookie}\r\n")]
}

/// `-cookies` exactly as downer renders it for `media_url`.
///
/// Built by calling `scraper::ffmpeg_cookies` rather than by hand, so these
/// tests check the shipped rendering against a real FFmpeg. A hand-written
/// string would only prove that *some* spelling scopes correctly, which is the
/// weaker claim and the one that already misled this file once.
///
/// The rule it implements: `domain=` is the URL's authority as written, with a
/// port only when the URL states one. See `scraper::cookie_domain`.
fn cookies_args(cookie: &str, media_url: &str) -> Vec<String> {
    let url = url::Url::parse(media_url).expect("fixture URLs parse");
    let rendered = downer::scraper::ffmpeg_cookies(&url, Some(cookie))
        .expect("downer renders a -cookies value for a media URL with a cookie");
    vec!["-cookies".to_string(), rendered]
}

fn as_args(owned: &[String]) -> Vec<&str> {
    owned.iter().map(String::as_str).collect()
}

/// The fixture server itself, proven without FFmpeg so it runs everywhere —
/// including CI, where the FFmpeg tests below only skip.
#[test]
fn fixture_server_records_headers_and_serves_routes() {
    let origin = HeaderRecorder::start("localhost");
    let other = HeaderRecorder::start("127.0.0.1");
    other.route("/real.mp4", Reply::text("video/mp4", "not real media"));
    origin.route("/video.mp4", Reply::Redirect(other.url("/real.mp4")));
    origin.route("/plain", Reply::text("text/plain", "hello"));

    let (status, body) = get(&origin.url("/plain"), Some(SENTINEL));
    assert_eq!(status, 200);
    assert_eq!(body, "hello");

    let (status, _) = get(&origin.url("/video.mp4"), None);
    assert_eq!(status, 302, "the redirect route answers with a redirect");

    let (status, _) = get(&origin.url("/missing"), None);
    assert_eq!(status, 404, "an unrouted path is a 404");

    let recorded = origin.requests_for("/plain");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].method, "GET");
    assert_eq!(
        recorded[0].cookie(),
        Some(SENTINEL),
        "the server records the Cookie header it received"
    );
    assert!(
        origin.requests_for("/video.mp4")[0].cookie().is_none(),
        "a request sent without a cookie records none"
    );
    assert!(
        other.requests().is_empty(),
        "the second server is independent and saw nothing"
    );
}

/// Across a redirect from the media host to another host, does the cookie
/// follow?
///
/// Confirms domain-scoped `-cookies` when it passes. Refutes it — with a message
/// naming the host that received the cookie — when it fails.
#[test]
fn ffmpeg_cookie_scope_across_a_redirect() {
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_cookie_scope_across_a_redirect") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();

    for (mode, scoped) in [("headers", false), ("cookies", true)] {
        let origin = HeaderRecorder::start("localhost");
        let elsewhere = HeaderRecorder::start("127.0.0.1");
        elsewhere.route("/real.mp4", Reply::text("video/mp4", "not real media"));
        origin.route("/video.mp4", Reply::Redirect(elsewhere.url("/real.mp4")));

        let args = if scoped {
            cookies_args(SENTINEL, &origin.url("/video.mp4"))
        } else {
            headers_args(SENTINEL)
        };
        let stderr = run_ffmpeg(
            &ffmpeg,
            &as_args(&args),
            &origin.url("/video.mp4"),
            &temp.path().join(format!("redirect-{mode}.mp4")),
        );

        let at_origin = origin.requests_for("/video.mp4");
        let at_elsewhere = elsewhere.requests_for("/real.mp4");
        assert!(
            !at_origin.is_empty(),
            "[{mode}] FFmpeg never reached the media host; the fixture server or the \
             arguments are wrong, not FFmpeg's cookie handling"
        );
        assert!(
            !at_elsewhere.is_empty(),
            "[{mode}] FFmpeg did not follow the redirect, so this says nothing about \
             cross-host cookies"
        );

        let origin_got = at_origin.iter().any(|request| request.cookie().is_some());
        let elsewhere_got = at_elsewhere
            .iter()
            .any(|request| request.cookie().is_some());
        eprintln!(
            "redirect/{mode}: media host received cookie = {origin_got}, \
             redirect target received cookie = {elsewhere_got}"
        );

        assert!(
            origin_got,
            "[{mode}] the media host received no cookie at all. For `cookies` this \
             refutes domain-scoped -cookies *as spelled here* — but a cookie that \
             reaches nobody usually means FFmpeg discarded it while parsing or \
             matching, not that it cannot scope. Run the spelling probe before \
             concluding anything:\n    \
             DOWNER_COOKIE_MATRIX=1 cargo test --test cookie_scope \
             ffmpeg_cookies_option_spelling_matrix -- --nocapture\n  \
             FFmpeg said:\n    {}",
            stderr_tail(&stderr)
        );
        if scoped {
            assert!(
                !elsewhere_got,
                "[cookies] the redirect target {} received the cookie. Domain-scoped \
                 -cookies is REFUTED for redirects; scoping must be done another way.",
                elsewhere.host()
            );
        } else {
            assert!(
                elsewhere_got,
                "[headers] the redirect target did not receive the cookie. This FFmpeg \
                 does not leak through -headers the way ADR-0002 records; revisit the ADR."
            );
        }
    }
}

/// The HLS case: a playlist on the media host whose segment lives on another
/// host. The demuxer opens the segment as a separate HTTP request, and the
/// question is whether the cookie rides along.
#[test]
fn ffmpeg_cookie_scope_for_a_cross_host_hls_segment() {
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_cookie_scope_for_a_cross_host_hls_segment") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();

    for (mode, scoped) in [("headers", false), ("cookies", true)] {
        let origin = HeaderRecorder::start("localhost");
        let cdn = HeaderRecorder::start("127.0.0.1");
        cdn.route("/segment.ts", Reply::text("video/mp2t", "not real media"));
        origin.route(
            "/stream.m3u8",
            Reply::text(
                "application/vnd.apple.mpegurl",
                format!(
                    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n\
                     #EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.0,\n{}\n#EXT-X-ENDLIST\n",
                    cdn.url("/segment.ts")
                ),
            ),
        );

        let args = if scoped {
            cookies_args(SENTINEL, &origin.url("/stream.m3u8"))
        } else {
            headers_args(SENTINEL)
        };
        let stderr = run_ffmpeg(
            &ffmpeg,
            &as_args(&args),
            &origin.url("/stream.m3u8"),
            &temp.path().join(format!("hls-{mode}.mp4")),
        );

        let at_origin = origin.requests_for("/stream.m3u8");
        let at_cdn = cdn.requests_for("/segment.ts");
        assert!(
            !at_origin.is_empty(),
            "[{mode}] FFmpeg never fetched the playlist"
        );
        assert!(
            !at_cdn.is_empty(),
            "[{mode}] FFmpeg never fetched the cross-host segment, so this says nothing \
             about cross-host cookies"
        );

        let playlist_got = at_origin.iter().any(|request| request.cookie().is_some());
        let segment_got = at_cdn.iter().any(|request| request.cookie().is_some());
        eprintln!(
            "hls/{mode}: playlist host received cookie = {playlist_got}, \
             segment host received cookie = {segment_got}"
        );

        assert!(
            playlist_got,
            "[{mode}] the playlist host received no cookie. For `cookies` this \
             refutes domain-scoped -cookies *as spelled here*; see the spelling \
             probe before concluding anything:\n    \
             DOWNER_COOKIE_MATRIX=1 cargo test --test cookie_scope \
             ffmpeg_cookies_option_spelling_matrix -- --nocapture\n  \
             FFmpeg said:\n    {}",
            stderr_tail(&stderr)
        );
        if scoped {
            assert!(
                !segment_got,
                "[cookies] the cross-host segment server {} received the cookie. \
                 Domain-scoped -cookies does NOT propagate scoping into the HLS demuxer; \
                 the native scheduler (KEI-68/KEI-70) is the only fix for HLS.",
                cdn.host()
            );
        } else {
            assert!(
                segment_got,
                "[headers] the cross-host segment server did not receive the cookie. \
                 This FFmpeg does not leak through -headers the way ADR-0002 records; \
                 revisit the ADR."
            );
        }
    }
}

/// Which spelling of `-cookies`, if any, makes FFmpeg send the cookie to the
/// host it is scoped to?
///
/// `ffmpeg_cookie_scope_across_a_redirect` established that
/// `name=value; path=/; domain=<host>` reaches *nobody* — not even the media
/// host. A cookie that reaches nobody was discarded during parsing or matching,
/// which is a different finding from "FFmpeg cannot scope cookies", and the two
/// lead to opposite decisions in KEI-78. This probe tells them apart by trying
/// the plausible spellings against one host and reporting what arrived.
///
/// It is a diagnostic, not a check: it asserts nothing and cannot fail. Run it
/// explicitly, since it is only interesting when something is already wrong:
///
/// ```sh
/// DOWNER_COOKIE_MATRIX=1 cargo test --test cookie_scope \
///     ffmpeg_cookies_option_spelling_matrix -- --nocapture
/// ```
#[test]
fn ffmpeg_cookies_option_spelling_matrix() {
    if std::env::var_os("DOWNER_COOKIE_MATRIX").is_none() {
        eprintln!(
            "SKIP: ffmpeg_cookies_option_spelling_matrix is a diagnostic. Run it with \
             DOWNER_COOKIE_MATRIX=1 when the -cookies scope tests report that the media \
             host received no cookie."
        );
        return;
    }
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_cookies_option_spelling_matrix") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();

    eprintln!("\n=== -cookies spelling probe ===");
    eprintln!("FFmpeg: {}", ffmpeg.display());
    let version = Command::new(&ffmpeg)
        .arg("-version")
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    eprintln!("{version}");
    eprintln!(
        "\nEach row sends one cookie to a single-host server and reports whether the\n\
         server saw a Cookie header. `-headers` is the control: it must say yes.\n"
    );
    eprintln!("{:<52}  COOKIE ARRIVED", "SPELLING");
    eprintln!("{}", "-".repeat(70));

    // Each probe is built from the live server, because some spellings need the
    // port and the port is only known once it is bound.
    type Build = fn(&str, u16) -> (String, Vec<String>);
    let probes: Vec<Build> = vec![
        |_host, _port| {
            (
                "-headers (control, today's behaviour)".to_string(),
                vec!["-headers".to_string(), format!("Cookie: {SENTINEL}\r\n")],
            )
        },
        |host, _port| {
            (
                format!("-cookies  …; path=/; domain={host}"),
                vec![
                    "-cookies".to_string(),
                    format!("{SENTINEL}; path=/; domain={host}"),
                ],
            )
        },
        |host, port| {
            (
                format!("-cookies  …; path=/; domain={host}:{port}"),
                vec![
                    "-cookies".to_string(),
                    format!("{SENTINEL}; path=/; domain={host}:{port}"),
                ],
            )
        },
        |host, _port| {
            (
                format!("-cookies  …; path=/; domain=.{host}"),
                vec![
                    "-cookies".to_string(),
                    format!("{SENTINEL}; path=/; domain=.{host}"),
                ],
            )
        },
        |host, _port| {
            (
                format!("-cookies  …; domain={host}; path=/  (reordered)"),
                vec![
                    "-cookies".to_string(),
                    format!("{SENTINEL}; domain={host}; path=/"),
                ],
            )
        },
        |_host, _port| {
            (
                "-cookies  …; path=/   (no domain)".to_string(),
                vec!["-cookies".to_string(), format!("{SENTINEL}; path=/")],
            )
        },
        |_host, _port| {
            (
                "-cookies  …   (bare name=value)".to_string(),
                vec!["-cookies".to_string(), SENTINEL.to_string()],
            )
        },
        |host, _port| {
            (
                format!("-cookies  …; path=/; domain={host}; expires=…"),
                vec![
                    "-cookies".to_string(),
                    format!(
                        "{SENTINEL}; path=/; domain={host}; \
                         expires=Wed, 01 Jan 2031 00:00:00 GMT"
                    ),
                ],
            )
        },
        // Two entries, newline-delimited: one spelled with the port and one
        // without. FFmpeg matches `domain=` against the authority as written,
        // so exactly one of these can ever match and the other is skipped.
        // Emitting both is immune to the with-port/without-port distinction and
        // to a future FFmpeg changing which one it uses.
        |host, port| {
            (
                "-cookies  both spellings, matching one LAST".to_string(),
                vec![
                    "-cookies".to_string(),
                    format!(
                        "{SENTINEL}; path=/; domain={host}\n\
                         {SENTINEL}; path=/; domain={host}:{port}"
                    ),
                ],
            )
        },
        // Same two entries, order swapped. This is the row that matters for
        // the fallback: with an explicit port the matching entry is the one
        // carrying the port, so listing it FIRST here mimics the production
        // case, where the URL states no port and `domain=host` is the matching
        // entry and comes first. If FFmpeg kept only the last entry of a given
        // cookie name, the form would silently send nothing in production while
        // looking fine in this fixture.
        |host, port| {
            (
                "-cookies  both spellings, matching one FIRST".to_string(),
                vec![
                    "-cookies".to_string(),
                    format!(
                        "{SENTINEL}; path=/; domain={host}:{port}\n\
                         {SENTINEL}; path=/; domain={host}"
                    ),
                ],
            )
        },
    ];

    let mut any_scoped_worked = false;
    for (index, build) in probes.iter().enumerate() {
        let server = HeaderRecorder::start("localhost");
        server.route("/video.mp4", Reply::text("video/mp4", "not real media"));
        let (label, args) = build(server.host(), server.port());

        let stderr = run_ffmpeg(
            &ffmpeg,
            &as_args(&args),
            &server.url("/video.mp4"),
            &temp.path().join(format!("probe-{index}.mp4")),
        );

        let requests = server.requests_for("/video.mp4");
        let verdict = if requests.is_empty() {
            "— (FFmpeg never made the request)"
        } else if requests.iter().any(|request| request.cookie().is_some()) {
            if index > 0 {
                any_scoped_worked = true;
            }
            "YES"
        } else {
            "no"
        };
        eprintln!("{label:<52}  {verdict}");

        // Only the first failing scoped spelling needs its stderr shown; more
        // than that is noise.
        if index == 1 && verdict == "no" {
            eprintln!(
                "\n  FFmpeg's own account of the failing case:\n    {}\n",
                stderr_tail(&stderr)
            );
        }
    }

    eprintln!("{}", "-".repeat(70));
    if any_scoped_worked {
        eprintln!("A spelling works. KEI-78 should adopt it; ADR-0002's approach stands.\n");
    } else {
        eprintln!(
            "No -cookies spelling reached even its own host. Domain-scoped -cookies is\n\
             refuted for this FFmpeg, and KEI-78 needs a different approach — most\n\
             likely per-request scoping done by the native host (KEI-68/KEI-70).\n"
        );
    }
}

/// Does `domain=<host>` — with no port — match a URL that states no port?
///
/// Every other test here uses the fixture server's ephemeral port, so the URL
/// always carries an explicit one and `domain=` always carries it too. A
/// production media URL is `https://cdn.example.test/video.m3u8`, whose port is
/// implicit, and `scraper::cookie_domain` therefore emits a bare host. That case
/// is read off FFmpeg's source — `ff_url_join(hoststr, ..., port, NULL)` runs
/// before the `port < 0` defaulting — but has never been observed, and getting
/// it wrong is silent: FFmpeg makes the request and just leaves the cookie off.
///
/// Binding port 80 makes the case reachable locally. It needs privileges, so it
/// is opt-in twice over and skips loudly otherwise:
///
/// ```sh
/// sudo DOWNER_COOKIE_PORT80=1 cargo test --test cookie_scope \
///     ffmpeg_cookie_scope_on_a_default_port -- --nocapture
/// ```
///
/// `sudo` runs the already-built test binary, so build first with a plain
/// `cargo test --no-run` if the sudo environment lacks cargo.
#[test]
fn ffmpeg_cookie_scope_on_a_default_port() {
    if std::env::var_os("DOWNER_COOKIE_PORT80").is_none() {
        eprintln!(
            "SKIP: ffmpeg_cookie_scope_on_a_default_port needs to bind port 80, which needs \
             privileges. Run: sudo DOWNER_COOKIE_PORT80=1 cargo test --test cookie_scope \
             ffmpeg_cookie_scope_on_a_default_port -- --nocapture"
        );
        return;
    }
    let Some(ffmpeg) = real_ffmpeg("ffmpeg_cookie_scope_on_a_default_port") else {
        return;
    };

    let server = match HeaderRecorder::try_start_on("localhost", 80) {
        Ok(server) => server,
        Err(error) => {
            eprintln!(
                "SKIP: could not bind localhost:80 ({}): {error}. Permission denied means this \
                 needs sudo; address in use means something else on this machine is serving \
                 port 80 and must be stopped first.",
                error.kind()
            );
            return;
        }
    };
    assert_eq!(server.port(), 80, "the test is meaningless on another port");
    server.route("/video.mp4", Reply::text("video/mp4", "not real media"));

    // No port in the URL: this is the shape a production media URL has.
    let media_url = format!("http://{}/video.mp4", server.host());
    let temp = tempfile::tempdir().unwrap();

    let arrived = |label: &str, args: Vec<String>| -> bool {
        server.clear();
        run_ffmpeg(
            &ffmpeg,
            &as_args(&args),
            &media_url,
            &temp.path().join(format!("port80-{label}.mp4")),
        );
        let requests = server.requests_for("/video.mp4");
        assert!(
            !requests.is_empty(),
            "[{label}] FFmpeg never reached the server, so this says nothing about cookies"
        );
        let arrived = requests.iter().any(|request| request.cookie().is_some());
        eprintln!("default-port/{label}: cookie arrived = {arrived}");
        arrived
    };

    assert!(
        arrived("headers", headers_args(SENTINEL)),
        "[headers] the control did not arrive, so the fixture is broken, not the cookie rule"
    );

    // What downer actually renders for this URL.
    let rendered = cookies_args(SENTINEL, &media_url);
    assert!(
        rendered[1].ends_with(&format!("domain={}", server.host())),
        "downer must render a bare host for a URL with no port: {:?}",
        rendered[1]
    );
    assert!(
        arrived("cookies (downer's rendering)", rendered),
        "domain=<host> did not match a URL that states no port. The rule in \
         scraper::cookie_domain is WRONG for production URLs, where the port is implicit: \
         every protected download would silently lose its cookie. ADR-0002 and KEI-78 both \
         need correcting."
    );

    // The converse, which makes the rule "the authority as written" rather than
    // "the authority, port optional".
    assert!(
        !arrived(
            "cookies with an explicit :80",
            vec![
                "-cookies".to_string(),
                format!("{SENTINEL}; path=/; domain={}:80", server.host()),
            ],
        ),
        "domain=<host>:80 matched a URL that states no port. Harmless in itself, but it means \
         FFmpeg's match string is not the authority as written, so re-read http.c before \
         trusting scraper::cookie_domain."
    );
}
