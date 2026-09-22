//! Turning FFmpeg's stderr into something a person can read.
//!
//! FFmpeg writes its diagnostics interleaved with progress chatter, and the
//! cause is rarely the first or the last line. A cross-host HLS download whose
//! segment origin was down produced this, in order:
//!
//! ```text
//! ffmpeg stats and -progress period set to 0.5.
//! [in#0 @ 0x…] Opening 'http://127.0.0.1:8081/media/segment0.ts' for reading
//! [tcp @ 0x…] Connection to tcp://127.0.0.1:8081 failed: Connection refused
//! [in#0 @ 0x…] Failed to open segment 0 of playlist 0
//! [in#0 @ 0x…] Segment 0 of playlist 0 failed too many times, skipping
//! …
//! Error opening input files: Invalid data found when processing input
//! ```
//!
//! The first line is FFmpeg reporting its own configuration. The last is a
//! consequence. `Connection refused`, in the middle, is the answer.
//!
//! This is not a parser for FFmpeg's output and does not try to be: it picks a
//! headline it recognises, and leaves everything else alone as detail.

/// Lines FFmpeg emits at `info` that describe what it is doing rather than what
/// went wrong. A controlled download must run at `info` because `-progress`
/// needs it (ADR-0007), so the chatter cannot be turned off at the source.
const CHATTER: &[&str] = &[
    "ffmpeg stats and -progress period set to",
    "for reading",
    "Press [q] to stop",
    "configuration:",
    "built with",
];

/// Phrases that name a cause rather than a consequence, most specific first.
/// Matched case-insensitively against each line.
const CAUSES: &[&str] = &[
    "connection refused",
    "connection timed out",
    "name or service not known",
    "temporary failure in name resolution",
    "server returned 4",
    "server returned 5",
    "403 forbidden",
    "404 not found",
    "401 unauthorized",
    "no such file or directory",
    "permission denied",
    "protocol not found",
    "no space left on device",
    "invalid data found",
];

/// Phrases that mean "the network misbehaved", so trying again might work.
///
/// Matched case-insensitively against FFmpeg's whole stderr. Kept separate from
/// [`CAUSES`] because the two answer different questions: `CAUSES` picks the
/// line worth showing a user, this decides whether to spend their time on
/// another attempt.
const TRANSIENT: &[&str] = &[
    "connection reset by peer",
    "connection timed out",
    "connection refused",
    "network is unreachable",
    "host is unreachable",
    "broken pipe",
    "no route to host",
    "server returned 5",
    "server returned 408",
    "server returned 429",
    // FFmpeg's own wording when a read dies part way through a stream, which is
    // exactly the dropped-connection case and says nothing more specific.
    "error in the pull function",
    "i/o error",
    "input/output error",
    "end of file",
    "timed out",
    "temporary failure in name resolution",
];

/// Phrases that settle the question the other way, whatever else is in the
/// text.
///
/// Checked **first**, because FFmpeg's stderr is a transcript rather than a
/// verdict: a run that ends on a 403 can still mention an earlier retried read,
/// and matching [`TRANSIENT`] against that would retry a request that is never
/// going to be allowed. Retrying a 403 or a 404 also hammers a server that has
/// already given its final answer.
const PERMANENT: &[&str] = &[
    "server returned 400",
    "server returned 401",
    "server returned 403",
    "server returned 404",
    "server returned 410",
    "403 forbidden",
    "404 not found",
    "401 unauthorized",
    "no space left on device",
    "permission denied",
    "invalid data found",
    "protocol not found",
    "no such file or directory",
    "unrecognized option",
    "option not found",
];

/// Is this FFmpeg failure worth another attempt?
///
/// Conservative by construction: a phrase must be recognised as transient *and*
/// nothing permanent may appear, so an unrecognised failure is not retried. The
/// cost of being wrong is asymmetric — a missed retry costs one manual click,
/// a wrong retry spends the user's time and someone else's bandwidth on a
/// request that has already been refused.
pub fn is_transient(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    if PERMANENT.iter().any(|phrase| text.contains(phrase)) {
        return false;
    }
    TRANSIENT.iter().any(|phrase| text.contains(phrase))
}

/// A failure split into the one line worth leading with, and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegFailure {
    pub headline: String,
    pub detail: String,
}

/// How much detail is worth sending to a popup sized for a sentence. The full
/// text still reaches the log console line by line, which is where a long tail
/// belongs.
pub const MAX_DETAIL_BYTES: usize = 2000;

/// Read FFmpeg's stderr into a headline and the detail behind it.
pub fn summarize(stderr: &str) -> FfmpegFailure {
    let all: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let kept: Vec<&str> = all
        .iter()
        .copied()
        .filter(|line| !is_chatter(line))
        .collect();
    // Chatter is dropped only when something else survives. When FFmpeg said
    // nothing but chatter, the chatter is the whole of what is known, and an
    // empty report is worse than a noisy one.
    let lines = if kept.is_empty() { all } else { kept };

    let headline = pick_headline(&lines).unwrap_or_else(|| {
        // Nothing recognised. The last line is FFmpeg's own summary, which is
        // a better opening than its first line even when it is a consequence.
        lines
            .last()
            .map(|line| strip_context(line).to_string())
            .unwrap_or_else(|| "FFmpeg produced no diagnostic output".to_string())
    });

    FfmpegFailure {
        detail: bound(&lines.join("\n")),
        headline,
    }
}

fn is_chatter(line: &str) -> bool {
    CHATTER.iter().any(|noise| line.contains(noise))
}

fn pick_headline(lines: &[&str]) -> Option<String> {
    for cause in CAUSES {
        if let Some(line) = lines
            .iter()
            .find(|line| line.to_ascii_lowercase().contains(cause))
        {
            return Some(strip_context(line).to_string());
        }
    }
    None
}

/// Drop FFmpeg's `[tcp @ 0x7bff04c080]` component prefix.
///
/// The component is useful in a log and noise in a headline, and the pointer in
/// it differs between runs, which would make the same failure look like two.
fn strip_context(line: &str) -> &str {
    let Some(rest) = line.strip_prefix('[') else {
        return line;
    };
    match rest.split_once("] ") {
        Some((_, tail)) => tail,
        None => line,
    }
}

/// Cut the detail to [`MAX_DETAIL_BYTES`] on a line boundary, saying so.
fn bound(detail: &str) -> String {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail.to_string();
    }
    // Keep the *end*: the lines nearest the failure explain it, and a long
    // stderr is long because a segment message repeated.
    //
    // The cut is moved forward to a character boundary. FFmpeg's output is not
    // guaranteed ASCII — a path with an accent, a localised message — and
    // slicing a `str` mid-character panics, which would turn a download failure
    // into a crash while reporting it.
    let mut cut = detail.len() - MAX_DETAIL_BYTES;
    while cut < detail.len() && !detail.is_char_boundary(cut) {
        cut += 1;
    }
    let tail = &detail[cut..];
    let tail = match tail.find('\n') {
        Some(index) => &tail[index + 1..],
        None => tail,
    };
    format!("…earlier output omitted…\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported case, verbatim. The cause is in the middle; the first line
    /// is FFmpeg's configuration and the last is a consequence of the cause.
    #[test]
    fn leads_with_the_cause_not_the_configuration_or_the_consequence() {
        let stderr = "ffmpeg stats and -progress period set to 0.5.\n\
             [in#0 @ 0x7bff020000] Opening 'http://127.0.0.1:8081/media/segment0.ts' for reading\n\
             [tcp @ 0x7bff04c080] Connection to tcp://127.0.0.1:8081 failed: Connection refused\n\
             [in#0 @ 0x7bff020000] Failed to open segment 0 of playlist 0\n\
             Error opening input files: Invalid data found when processing input";
        let summary = summarize(stderr);
        assert_eq!(
            summary.headline,
            "Connection to tcp://127.0.0.1:8081 failed: Connection refused"
        );
        assert!(
            !summary.detail.contains("stats and -progress"),
            "configuration chatter is not detail: {}",
            summary.detail
        );
        assert!(
            !summary.detail.contains("for reading"),
            "nor is what FFmpeg was opening: {}",
            summary.detail
        );
        assert!(summary.detail.contains("Invalid data found"));
    }

    #[test]
    fn an_http_status_is_a_cause() {
        let summary = summarize(
            "ffmpeg stats and -progress period set to 0.5.\n\
             [hls @ 0x1] Server returned 404 Not Found\n\
             Error opening input files: Invalid data found when processing input",
        );
        assert_eq!(summary.headline, "Server returned 404 Not Found");
    }

    /// Nothing recognised: the last line beats the first, because FFmpeg's own
    /// summary is at the end.
    #[test]
    fn falls_back_to_the_last_line_rather_than_the_first() {
        let summary = summarize(
            "ffmpeg stats and -progress period set to 0.5.\n\
             [out#0 @ 0x1] Something entirely unanticipated\n\
             Conversion failed!",
        );
        assert_eq!(summary.headline, "Conversion failed!");
    }

    /// Chatter is only noise next to something better. Alone, it is the report.
    /// Without this the one line FFmpeg emitted could be filtered away, leaving
    /// a failure with no detail at all — which is how this rule was found.
    #[test]
    fn chatter_survives_when_it_is_all_there_is() {
        let summary = summarize("Opening https://cdn.example.test/hls/seg42.ts?… for reading");
        assert_eq!(
            summary.detail,
            "Opening https://cdn.example.test/hls/seg42.ts?… for reading"
        );
        assert_eq!(
            summary.headline,
            "Opening https://cdn.example.test/hls/seg42.ts?… for reading"
        );
    }

    /// FFmpeg's output is not guaranteed ASCII, and a byte-indexed cut through
    /// a multi-byte character panics. Reporting a failure must not be a way to
    /// crash.
    #[test]
    fn a_long_tail_with_multibyte_characters_does_not_panic() {
        // Sized so the byte-indexed cut lands *inside* a three-byte `…`
        // rather than beside one: with the trailing `!` removed the same
        // content cuts cleanly and proves nothing. Verified by construction —
        // the boundary is at `len - MAX_DETAIL_BYTES`, and the single extra
        // trailing byte shifts it one byte into the character.
        let noisy = format!("[in#0 @ 0x1] {}\n", "…".repeat(40)).repeat(60);
        let stderr = format!("{noisy}Error opening input files: Invalid data found!");
        assert!(
            !stderr.is_char_boundary(stderr.len() - MAX_DETAIL_BYTES),
            "this fixture only tests what it claims if the cut is mid-character"
        );

        let summary = summarize(&stderr);
        assert!(summary.detail.starts_with("…earlier output omitted…"));
        assert!(summary
            .detail
            .ends_with("Error opening input files: Invalid data found!"));
    }

    #[test]
    fn empty_output_says_so_rather_than_showing_nothing() {
        let summary = summarize("   \n\n");
        assert_eq!(summary.headline, "FFmpeg produced no diagnostic output");
        assert_eq!(summary.detail, "");
    }

    /// The component prefix carries a pointer that differs between runs, so two
    /// reports of one failure would not look alike.
    #[test]
    fn the_component_prefix_is_dropped_from_the_headline_only() {
        let summary = summarize("[tcp @ 0xdeadbeef] Connection refused");
        assert_eq!(summary.headline, "Connection refused");
        assert_eq!(summary.detail, "[tcp @ 0xdeadbeef] Connection refused");
    }

    #[test]
    fn a_long_tail_is_bounded_keeping_the_end() {
        let repeated =
            "[in#0 @ 0x1] Segment 0 of playlist 0 failed too many times, skipping\n".repeat(200);
        let stderr = format!("{repeated}Error opening input files: Invalid data found");
        let summary = summarize(&stderr);
        assert!(
            summary.detail.len() <= MAX_DETAIL_BYTES + 32,
            "{}",
            summary.detail.len()
        );
        assert!(summary.detail.starts_with("…earlier output omitted…"));
        assert!(
            summary
                .detail
                .ends_with("Error opening input files: Invalid data found"),
            "the end is what explains the failure"
        );
    }

    /// A dropped connection mid-stream is the case this whole issue exists for.
    #[test]
    fn a_dropped_connection_is_worth_retrying() {
        for stderr in [
            "[hls @ 0x1] Error in the pull function.\n[in#0] Error during demuxing: Input/output error",
            "[tcp @ 0x2] Connection reset by peer",
            "[http @ 0x3] Server returned 503 Service Unavailable",
            "[http @ 0x4] Server returned 429 Too Many Requests",
            "[tcp @ 0x5] Connection timed out",
        ] {
            assert!(is_transient(stderr), "should retry: {stderr}");
        }
    }

    /// The half that protects somebody else's server. A 403 will not become a
    /// 200, so retrying it spends the user's time and hammers a host that has
    /// already given its final answer.
    #[test]
    fn an_authorisation_or_missing_resource_failure_is_never_retried() {
        for stderr in [
            "[http @ 0x1] Server returned 403 Forbidden",
            "[http @ 0x2] Server returned 404 Not Found",
            "[http @ 0x3] Server returned 401 Unauthorized",
            "[out#0] Error opening output file: Permission denied",
            "No space left on device",
            "[in#0] Error opening input: Invalid data found when processing input",
        ] {
            assert!(!is_transient(stderr), "should not retry: {stderr}");
        }
    }

    /// FFmpeg's stderr is a transcript, not a verdict: a run that ends on a 403
    /// can still mention an earlier retried read. The permanent phrase has to
    /// win, or a forbidden URL would be requested three times.
    #[test]
    fn a_permanent_failure_wins_over_transient_noise_earlier_in_the_log() {
        let stderr = "[hls @ 0x1] Error in the pull function.\n                      [http @ 0x2] Server returned 403 Forbidden\n                      [in#0] Error during demuxing: Input/output error";
        assert!(
            !is_transient(stderr),
            "a 403 anywhere in the transcript settles it"
        );
    }

    /// Unrecognised means "do not retry": the classification is deliberately
    /// conservative, because a wrong retry costs more than a missed one.
    #[test]
    fn an_unrecognised_failure_is_not_retried() {
        assert!(!is_transient("[in#0] Something nobody has seen before"));
        assert!(!is_transient(""));
    }

    /// An FFmpeg too old for its options will be exactly as old next time.
    #[test]
    fn an_argument_parsing_failure_is_not_a_network_problem() {
        assert!(!is_transient(
            "Unrecognized option 'allowed_segment_extensions'.\nError splitting the argument list: Option not found"
        ));
    }
}
