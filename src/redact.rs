//! URL redaction for text that leaves this process.
//!
//! FFmpeg's HLS demuxer logs one `Opening '<url>' for reading` line per segment
//! at `-loglevel info`, and those URLs routinely carry a signed token in their
//! query string. The native host forwards every stderr line to the extension as
//! a `log` event, so without this the token crosses the native messaging port
//! and lands in `storage.local`. This keeps the part of a URL that makes a log
//! useful — scheme, host, port, path — and drops the parts that carry
//! credentials.
//!
//! The identical rule is implemented in `extension/redact.js`, which runs again
//! on the receiving side before anything is persisted or displayed. Two layers,
//! because neither covers the other's ground; the reasoning is in
//! `docs/adr/0003-redact-urls-in-logs.md` (KEI-55).
//!
//! `tests/fixtures/redaction.json` is the shared case table. The tests below and
//! `tests/extension/redact.test.js` both read it, exactly as both suites read
//! `tests/fixtures/protocol.json` for the wire vocabulary, so the two
//! implementations cannot drift apart silently.

/// What replaces a query string. The query is *marked*, not deleted, so a reader
/// can tell "this URL had parameters" from "this URL had none" — which is often
/// the difference between a signed and an unsigned CDN path.
pub const QUERY_PLACEHOLDER: &str = "?…";
/// What replaces a fragment, which can carry the same token a query can.
pub const FRAGMENT_PLACEHOLDER: &str = "#…";
/// What replaces `user:password@` in an authority.
pub const USERINFO_PLACEHOLDER: &str = "…@";

/// Upper bound on one redacted line. A pathological line is a storage problem
/// rather than a secrecy one, but the two meet here: one line must not be able
/// to fill the extension's per-job byte budget by itself. Truncation happens
/// after redaction, so it can never leave half a token behind.
pub const MAX_LINE_CHARS: usize = 2000;
const TRUNCATION_MARK: char = '…';

/// Characters that end a URL because something else begins. Matching stops here
/// rather than at whitespace alone, so a quoted URL in an FFmpeg log line does
/// not swallow its closing quote.
const URL_TERMINATORS: &[char] = &['\'', '"', '<', '>', '\\', '`', '|', '^', '{', '}'];

/// Characters that end a sentence rather than a URL. FFmpeg and our own error
/// strings both write `could not open <url>.`, and absorbing that full stop into
/// the match would hide it inside the placeholder.
const TRAILING_PUNCTUATION: &[char] = &['.', ',', ';', ':', '!', ')', ']', '}', '>'];

/// Redact one URL. Returns the input unchanged when it has no authority, which
/// is how a bare `https://` mentioned in prose stays readable.
pub fn redact_url(url: &str) -> String {
    let Some(separator) = url.find("://") else {
        return url.to_string();
    };
    let (scheme, rest) = url.split_at(separator + 3);
    if rest.is_empty() {
        return url.to_string();
    }

    let query_at = rest.find('?');
    let fragment_at = rest.find('#');
    let cut = match (query_at, fragment_at) {
        (Some(query), Some(fragment)) => query.min(fragment),
        (Some(query), None) => query,
        (None, Some(fragment)) => fragment,
        (None, None) => rest.len(),
    };
    let head = &rest[..cut];
    if head.is_empty() {
        return url.to_string();
    }

    // Userinfo is only userinfo before the first `/`; an `@` inside a path is an
    // ordinary path character.
    let (authority, path) = match head.find('/') {
        Some(at) => head.split_at(at),
        None => (head, ""),
    };
    if authority.is_empty() {
        return url.to_string();
    }
    let authority = match authority.rfind('@') {
        Some(at) => format!("{USERINFO_PLACEHOLDER}{}", &authority[at + 1..]),
        None => authority.to_string(),
    };

    let mut redacted = format!("{scheme}{authority}{path}");
    if query_at.is_some() {
        redacted.push_str(QUERY_PLACEHOLDER);
    }
    if fragment_at.is_some() {
        redacted.push_str(FRAGMENT_PLACEHOLDER);
    }
    redacted
}

/// Redact every `http`/`https` URL in a line of arbitrary text, and bound its
/// length.
///
/// Only those two schemes, because they are the only ones `output.rs` accepts
/// and so the only ones this product can be downloading. A `key=value` pair
/// elsewhere in a line is not assumed to be a URL: FFmpeg's own progress
/// vocabulary (`q=-1.0`, `size=…`) is full of them.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = find_scheme(rest) {
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || URL_TERMINATORS.contains(&c))
            .unwrap_or(candidate.len());
        let matched = &candidate[..end];
        let trimmed = matched.trim_end_matches(TRAILING_PUNCTUATION);
        out.push_str(&redact_url(trimmed));
        out.push_str(&matched[trimmed.len()..]);
        rest = &candidate[end..];
    }
    out.push_str(rest);

    truncate(out)
}

/// The byte offset of the next `http://` or `https://`, matched without regard
/// to the case of the scheme.
fn find_scheme(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    match (lower.find("http://"), lower.find("https://")) {
        (Some(http), Some(https)) => Some(http.min(https)),
        (Some(http), None) => Some(http),
        (None, Some(https)) => Some(https),
        (None, None) => None,
    }
}

fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_LINE_CHARS {
        return text;
    }
    let mut out: String = text.chars().take(MAX_LINE_CHARS - 1).collect();
    out.push(TRUNCATION_MARK);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIXTURE: &str = include_str!("../tests/fixtures/redaction.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("tests/fixtures/redaction.json is valid JSON")
    }

    /// The shared case table, run against this implementation. The same table is
    /// run against `extension/redact.js` by `tests/extension/redact.test.js`; a
    /// case that passes on one side and fails on the other is exactly the drift
    /// this file is meant to make impossible.
    #[test]
    fn redaction_matches_the_shared_case_table() {
        let fixture = fixture();
        let cases = fixture["cases"]
            .as_array()
            .expect("cases is an array in tests/fixtures/redaction.json");
        assert!(!cases.is_empty(), "the shared case table is not empty");
        for case in cases {
            let name = case["name"].as_str().expect("every case is named");
            let input = case["input"].as_str().expect("every case has an input");
            let expected = case["expected"]
                .as_str()
                .expect("every case has an expected");
            assert_eq!(redact_text(input), expected, "case: {name}");
        }
    }

    /// The placeholders are part of the shared contract, not an implementation
    /// detail: the extension renders them and a reader learns to recognise them.
    #[test]
    fn the_placeholders_match_the_shared_case_table() {
        let fixture = fixture();
        assert_eq!(
            fixture["query_placeholder"].as_str(),
            Some(QUERY_PLACEHOLDER)
        );
        assert_eq!(
            fixture["fragment_placeholder"].as_str(),
            Some(FRAGMENT_PLACEHOLDER)
        );
        assert_eq!(
            fixture["userinfo_placeholder"].as_str(),
            Some(USERINFO_PLACEHOLDER)
        );
        assert_eq!(
            fixture["max_line_chars"].as_u64(),
            Some(MAX_LINE_CHARS as u64)
        );
    }

    /// The property that matters, stated independently of the case table: no
    /// sentinel from any case survives redaction of any case's input.
    #[test]
    fn no_sentinel_survives_redaction() {
        let fixture = fixture();
        let sentinels: Vec<String> = fixture["sentinels"]
            .as_array()
            .expect("sentinels is an array")
            .iter()
            .map(|value| value.as_str().expect("sentinels are strings").to_string())
            .collect();
        assert!(!sentinels.is_empty(), "there is at least one sentinel");
        for case in fixture["cases"].as_array().expect("cases is an array") {
            let input = case["input"].as_str().expect("every case has an input");
            // One case deliberately carries a scheme this product never
            // downloads, to pin that redaction keys off the URL grammar rather
            // than off the presence of a token-shaped string.
            if input.starts_with("ftp://") {
                continue;
            }
            let redacted = redact_text(input);
            for sentinel in &sentinels {
                assert!(
                    !redacted.contains(sentinel.as_str()),
                    "a sentinel survived redaction in case {:?}",
                    case["name"]
                );
            }
        }
    }

    #[test]
    fn a_very_long_line_is_truncated_after_redaction() {
        let line = format!("https://cdn.example.test/{}?token=x", "a".repeat(4000));
        let redacted = redact_text(&line);
        assert_eq!(redacted.chars().count(), MAX_LINE_CHARS);
        assert!(redacted.ends_with(TRUNCATION_MARK));
        assert!(
            !redacted.contains("token=x"),
            "redaction ran before truncation"
        );
    }

    #[test]
    fn a_line_with_no_url_is_returned_unchanged() {
        let line = "frame= 1234 fps= 60 q=-1.0 size=  12345kB bitrate=1234.5kbits/s";
        assert_eq!(redact_text(line), line);
    }

    #[test]
    fn a_multibyte_line_is_not_split_mid_character() {
        let line = format!(
            "日本語 {} https://cdn.example.test/v.ts?t=x",
            "あ".repeat(4000)
        );
        let redacted = redact_text(&line);
        assert_eq!(redacted.chars().count(), MAX_LINE_CHARS);
    }
}
