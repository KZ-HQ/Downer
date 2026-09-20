use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use crate::error::{DownerError, DownerResult};

/// The longest a page title may contribute to a filename.
///
/// Titles are user data that reaches the disk, so the contribution is bounded
/// rather than trusted: a title is only ever consulted when the URL says
/// nothing useful, and never contributes more than this many characters.
const MAX_TITLE_CHARS: usize = 80;

/// How many `_n` candidates [`OnConflict::Rename`] tries before giving up.
/// A directory holding this many same-named downloads is a user problem, not a
/// loop to run forever.
const MAX_RENAME_ATTEMPTS: u32 = 1_000;

/// Stems that identify the stream rather than the content. A URL ending in one
/// of these says nothing a user would recognise, so the name becomes
/// [`DEFAULT_STEM`], or the caller's title when one was supplied. Numeric-only
/// stems (`1080.m3u8`, `42.mp4`) count too.
const GENERIC_STEMS: [&str; 6] = ["index", "playlist", "master", "download", "video", "media"];

/// What a download is called when neither the URL nor the caller says anything
/// useful. Predictable beats clever: see ADR-0005.
const DEFAULT_STEM: &str = "video";

/// What to do when the resolved output path is already taken.
///
/// See `docs/adr/0004-output-naming-and-collision-policy.md`. The default is
/// [`OnConflict::Rename`] for an inferred name and [`OnConflict::Fail`] for an
/// exact `--output` path; the caller decides which, this type only says what
/// each one does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Refuse to touch an existing file.
    Fail,
    /// Write beside it as `name_2.ext`, `name_3.ext`, and so on.
    #[default]
    Rename,
    /// Replace it.
    Overwrite,
}

/// Naming material the caller knows and the URL does not.
///
/// A title is **opt-in**: the CLI supplies one only for `--name`, and the
/// extension only when its "name downloads after the page title" setting is on.
/// Absent — which is the default — a generically named download is
/// [`DEFAULT_STEM`]. Even when present a title is advisory: it is consulted
/// only when the URL-derived stem is generic, and never widens what may be
/// written, see [`sanitize_title`].
#[derive(Debug, Clone, Default)]
pub struct NamingHints {
    pub title: Option<String>,
}

impl NamingHints {
    pub fn new(title: Option<String>) -> Self {
        Self { title }
    }
}

/// An output path that has been resolved against the collision policy.
///
/// `overwrite` is what FFmpeg is told. Under [`OnConflict::Rename`] the path
/// was reserved by this process (see [`resolve_conflict`]), so replacing that
/// reservation is both safe and required, and `reserved` is set so
/// [`release_reservation`] can take the empty file back if the download never
/// writes to it.
#[derive(Debug, Clone)]
pub struct OutputTarget {
    pub path: PathBuf,
    pub overwrite: bool,
    pub reserved: bool,
}

/// Accept only network media URLs that FFmpeg can open directly.
pub fn validate_url(raw: &str) -> DownerResult<Url> {
    let url = Url::parse(raw).map_err(|error| DownerError::InvalidUrl(error.to_string()))?;
    match url.scheme() {
        "http" | "https"
            if has_nonempty_authority(raw)
                && url.host_str().is_some_and(|host| !host.is_empty()) =>
        {
            Ok(url)
        }
        "http" | "https" => Err(DownerError::InvalidUrl("URL has no host".to_string())),
        scheme => Err(DownerError::InvalidUrl(format!(
            "unsupported scheme {scheme:?}; use http:// or https://"
        ))),
    }
}

fn has_nonempty_authority(raw: &str) -> bool {
    raw.split_once("://")
        .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
        .is_some_and(|authority| !authority.is_empty())
}

/// Where a download goes when the client named no directory.
///
/// The browser extension leaves this empty by default, so this is the **normal
/// path**, not an edge case.
///
/// `dirs::download_dir()` is the right answer wherever the desktop has one. On
/// macOS it always resolves. On Linux it reads XDG user-directory configuration
/// (`~/.config/user-dirs.dirs`) and returns `None` without it — absent on
/// minimal installs, containers, and any system without `xdg-user-dirs`. This
/// was not hypothetical: the KEI-59 integration test had to write that file by
/// hand before a default directory could be found at all, and `downer doctor`
/// on such a machine reported the process's working directory as the place
/// downloads go.
///
/// `$HOME/Downloads` is the fallback, created on first use like any other
/// output directory. It is what Firefox itself uses on Linux without XDG, so a
/// user's downloads land beside their browser's rather than somewhere this
/// project invented.
///
/// `None` only when there is no home directory either. On Unix `dirs::home_dir`
/// reads `$HOME` and falls back to the passwd entry, so this is rare — measured:
/// a host started with `HOME` unset still resolved `/root/Downloads`. It is
/// nonetheless the one case that cannot be decided, and the caller refuses
/// rather than guessing. The previous guess was `.`, the host process's working
/// directory, inherited from however Firefox was started (`/` from a desktop
/// launcher), so a download could land somewhere the user did not choose and
/// would not think to look. See KEI-90.
pub fn default_download_dir() -> Option<PathBuf> {
    resolve_default_download_dir(dirs::download_dir(), dirs::home_dir())
}

/// The rule itself, with its two inputs passed in.
///
/// Split out so the `None` case can be tested at all: it depends on the
/// developer's machine having no XDG configuration *and* no home directory,
/// which is not something a test can arrange.
fn resolve_default_download_dir(
    desktop: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    desktop.or_else(|| home.map(|home| home.join("Downloads")))
}

/// What to tell a client that has no configured directory and no `$HOME`.
pub const NO_DEFAULT_DIRECTORY: &str =
    "no download directory is configured and no default could be found; \
     set one in the extension's Settings page, or pass --dir";

pub fn resolve_output_path(
    url: &Url,
    output: Option<&Path>,
    dir: Option<&Path>,
    current_dir: &Path,
    hints: &NamingHints,
) -> DownerResult<PathBuf> {
    if let Some(path) = output {
        if path.as_os_str().is_empty() {
            return Err(DownerError::OutputPath("path is empty".to_string()));
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(DownerError::OutputIo)?;
        }
        return Ok(path.to_path_buf());
    }

    let directory = dir.unwrap_or(current_dir);
    if directory.as_os_str().is_empty() {
        return Err(DownerError::OutputPath("directory is empty".to_string()));
    }
    let filename = infer_filename_with_hints(url, hints);
    if let Err(error) = fs::create_dir_all(directory) {
        return Err(DownerError::OutputIo(error));
    }
    Ok(directory.join(filename))
}

pub fn check_output_path(path: &Path, overwrite: bool) -> DownerResult<()> {
    if path.is_dir() {
        return Err(DownerError::OutputDirectory(path.to_path_buf()));
    }
    match fs::metadata(path) {
        Ok(_) if !overwrite => Err(DownerError::OutputExists(path.to_path_buf())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DownerError::OutputIo(error)),
    }
}

/// Resolve `path` against `policy`, returning the path to write and whether
/// FFmpeg may replace what is there.
///
/// [`OnConflict::Rename`] reserves its choice by creating the file exclusively.
/// Probing with `metadata` and then handing the free name to FFmpeg would let
/// two hosts started at once — one download per process is the model — pick the
/// same ` (2)` and have one silently clobber the other. The reservation closes
/// that window at the cost of a zero-byte file if the download then fails
/// before FFmpeg writes anything, which the "preserve partial output" rule says
/// to leave in place anyway.
pub fn resolve_conflict(path: PathBuf, policy: OnConflict) -> DownerResult<OutputTarget> {
    if path.is_dir() {
        return Err(DownerError::OutputDirectory(path));
    }
    match policy {
        OnConflict::Overwrite => Ok(OutputTarget {
            path,
            overwrite: true,
            reserved: false,
        }),
        OnConflict::Fail => {
            check_output_path(&path, false)?;
            Ok(OutputTarget {
                path,
                overwrite: false,
                reserved: false,
            })
        }
        OnConflict::Rename => reserve_unused_path(path),
    }
}

fn reserve_unused_path(path: PathBuf) -> DownerResult<OutputTarget> {
    for attempt in 1..=MAX_RENAME_ATTEMPTS {
        let candidate = if attempt == 1 {
            path.clone()
        } else {
            numbered_variant(&path, attempt)
        };
        if candidate.is_dir() {
            continue;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => {
                return Ok(OutputTarget {
                    // The name is ours, so FFmpeg is told it may replace the
                    // empty file this reservation just created.
                    path: candidate,
                    overwrite: true,
                    reserved: true,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(DownerError::OutputIo(error)),
        }
    }
    Err(DownerError::OutputExists(path))
}

/// Take back a reservation the download never used.
///
/// A reservation is a placeholder this process created to claim a name, not
/// output. If the download then fails, leaving it behind would be residue: an
/// empty file the user did not ask for, which also pushes the next attempt onto
/// ` (2)`. So a failed download releases it.
///
/// The one thing this must never do is delete real output. "Preserve partial
/// output and diagnostic files after download failures" is the rule, and a
/// failure after FFmpeg has written some bytes is exactly the case it protects.
/// So the file is removed only while it is still **empty** — the state the
/// reservation created it in. The moment FFmpeg writes a byte it stops being a
/// reservation and is kept, and a path we did not reserve is never touched.
pub fn release_reservation(target: &OutputTarget) {
    if !target.reserved {
        return;
    }
    let is_empty = fs::metadata(&target.path).is_ok_and(|metadata| metadata.len() == 0);
    if is_empty {
        // Best effort: failing to tidy up must not replace the download's own
        // error, which is the one the user needs to read.
        let _ = fs::remove_file(&target.path);
    }
}

/// `video.mp4` and 2 become `video_2.mp4`; the suffix goes before the
/// extension so the file still opens in the application that handles it.
fn numbered_variant(path: &Path, attempt: u32) -> PathBuf {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.to_path_buf();
    };
    let numbered = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => {
            format!("{stem}_{attempt}.{extension}")
        }
        _ => format!("{name}_{attempt}"),
    };
    path.with_file_name(numbered)
}

/// Infer a filename, replacing a generic URL stem with the caller's title when
/// there is one and with [`DEFAULT_STEM`] when there is not.
///
/// The URL always wins when it says something: a distinctive path segment is a
/// better name than anything we could invent, and direct-file downloads are
/// untouched. Only the `index.m3u8` / `playlist.m3u8` case reaches the rest of
/// this, and there the answer is `video.mp4` unless a title was explicitly
/// supplied — ADR-0005 records why a predictable default won over a derived one.
pub fn infer_filename_with_hints(url: &Url, hints: &NamingHints) -> String {
    let inferred = infer_filename(url);
    let (stem, extension) = match inferred.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => return inferred,
    };
    if !is_generic_stem(stem) {
        return inferred;
    }
    let replacement = hints
        .title
        .as_deref()
        .and_then(sanitize_title)
        .unwrap_or_else(|| DEFAULT_STEM.to_string());
    format!("{replacement}.{extension}")
}

/// Whether a stem names the stream rather than its content.
pub fn is_generic_stem(stem: &str) -> bool {
    let stem = stem.trim().to_ascii_lowercase();
    if stem.is_empty() {
        return true;
    }
    GENERIC_STEMS.contains(&stem.as_str())
        || stem.chars().all(|character| character.is_ascii_digit())
}

/// Reduce a page title to something safe and bounded to put on disk.
///
/// A title is user data — it can name an account or a document — and a filename
/// is a place that data reaches the disk and the extension's persisted job
/// records. So the contribution is bounded, not trusted: whitespace (including
/// the newlines a title can carry) collapses to single spaces, the same
/// `sanitize_filename` rules that guard URL-derived names apply unchanged, and
/// the result is cut to `MAX_TITLE_CHARS` characters on a character boundary.
/// Returns `None` when nothing usable survives, which sends the caller back to
/// the URL-derived name.
pub fn sanitize_title(candidate: &str) -> Option<String> {
    let collapsed = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    let sanitized = sanitize_filename(&bounded);
    (!sanitized.is_empty()).then_some(sanitized)
}

/// Infer a filename without allowing URL path syntax to escape the destination directory.
pub fn infer_filename(url: &Url) -> String {
    let path_candidate = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned())
        .unwrap_or_default();
    let candidate = if path_candidate.is_empty() {
        url.query_pairs()
            .find(|(key, _)| matches!(key.as_ref(), "filename" | "file" | "name" | "title"))
            .map(|(_, value)| value.into_owned())
            .unwrap_or(path_candidate)
    } else {
        path_candidate
    };

    let mut filename = sanitize_filename(&candidate);
    if filename.is_empty() {
        filename = DEFAULT_STEM.to_string();
    }

    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("m3u8") => {
            filename.truncate(filename.len() - 5);
            filename.push_str(".mp4");
        }
        Some(_) => {}
        None => filename.push_str(".mp4"),
    }
    filename
}

fn sanitize_filename(candidate: &str) -> String {
    let mut sanitized = String::with_capacity(candidate.len());
    for character in candidate.chars() {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            sanitized.push('_');
        } else {
            sanitized.push(character);
        }
    }

    let sanitized = sanitized
        .trim()
        .trim_matches(|character| matches!(character, '.' | ' '))
        .to_string();
    if sanitized == "." || sanitized == ".." || is_windows_device_name(&sanitized) {
        String::new()
    } else {
        sanitized
    }
}

fn is_windows_device_name(filename: &str) -> bool {
    let stem = filename.split('.').next().unwrap_or_default();
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_http_and_https_only() {
        assert!(validate_url("https://example.test/video.mp4").is_ok());
        assert!(validate_url("http://example.test/live.m3u8").is_ok());
        assert!(validate_url("ftp://example.test/video.mp4").is_err());
        assert!(validate_url("https:///video.mp4").is_err());
        assert!(validate_url("https:example.test/video.mp4").is_err());
    }

    #[test]
    fn decodes_and_sanitizes_url_filename() {
        let url = Url::parse("https://example.test/a/My%20video%3Ffinal.mp4?token=x").unwrap();
        assert_eq!(infer_filename(&url), "My video_final.mp4");
    }

    #[test]
    fn uses_mp4_for_missing_or_playlist_names() {
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/").unwrap()),
            "video.mp4"
        );
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/stream.m3u8").unwrap()),
            "stream.mp4"
        );
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/live").unwrap()),
            "live.mp4"
        );
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/?filename=from%20query.ts").unwrap()),
            "from query.ts"
        );
    }

    #[test]
    fn avoids_device_and_traversal_names() {
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/CON").unwrap()),
            "video.mp4"
        );
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/evil%2Fname.mp4").unwrap()),
            "evil_name.mp4"
        );
    }

    fn hints(title: Option<&str>) -> NamingHints {
        NamingHints::new(title.map(str::to_string))
    }

    #[test]
    fn detects_generic_stems() {
        for stem in ["index", "playlist", "master", "download", "video", "media"] {
            assert!(is_generic_stem(stem), "{stem} should be generic");
            assert!(
                is_generic_stem(&stem.to_ascii_uppercase()),
                "{stem} should be generic regardless of case"
            );
        }
        // Numeric-only stems name a rendition or a shard, not the content.
        assert!(is_generic_stem("1080"));
        assert!(is_generic_stem("42"));
        assert!(is_generic_stem(""));

        assert!(!is_generic_stem("lecture"));
        assert!(!is_generic_stem("indexing"));
        assert!(!is_generic_stem("master class"));
        assert!(!is_generic_stem("1080p"));
        assert!(!is_generic_stem("episode-2"));
    }

    #[test]
    fn a_generic_stem_defaults_to_video_with_no_title() {
        // The default, and the case that matters: nothing was opted into, so
        // the name is predictable rather than derived.
        for generic in [
            "https://example.test/hls/index.m3u8",
            "https://example.test/playlist.m3u8",
            "https://example.test/master.m3u8",
            "https://example.test/media/1080.mp4",
            "https://example.test/",
        ] {
            let url = Url::parse(generic).unwrap();
            assert_eq!(
                infer_filename_with_hints(&url, &hints(None)),
                "video.mp4",
                "{generic}"
            );
        }

        // The source host is deliberately not a fallback any more: it only ever
        // fired when no title was given, which is now the common case, and
        // `example.test.mp4` is not what "default" should mean.
        assert_eq!(
            infer_filename_with_hints(
                &Url::parse("https://example.test/playlist.m3u8").unwrap(),
                &hints(None)
            ),
            "video.mp4"
        );
    }

    #[test]
    fn a_supplied_title_replaces_a_generic_stem_and_nothing_else() {
        let generic = Url::parse("https://example.test/hls/index.m3u8").unwrap();
        assert_eq!(
            infer_filename_with_hints(&generic, &hints(Some("Lecture 3"))),
            "Lecture 3.mp4"
        );

        // A distinctive URL segment beats an opted-in title, so direct-file
        // downloads keep naming themselves exactly as they always have.
        let distinctive = Url::parse("https://example.test/lecture-three.mp4").unwrap();
        assert_eq!(
            infer_filename_with_hints(&distinctive, &hints(Some("Some Unrelated Page Title"))),
            "lecture-three.mp4"
        );

        // A title that sanitises away is the same as no title at all.
        assert_eq!(
            infer_filename_with_hints(&generic, &hints(Some("   ...   "))),
            "video.mp4"
        );
    }

    #[test]
    fn sanitizes_and_bounds_a_title() {
        // Path syntax cannot escape the destination directory.
        assert_eq!(
            sanitize_title("evil/../name").as_deref(),
            Some("evil_.._name")
        );
        // Newlines and runs of whitespace collapse rather than becoming
        // underscores, so a multi-line title still reads as a sentence.
        assert_eq!(
            sanitize_title("My\n\tGreat   Video").as_deref(),
            Some("My Great Video")
        );
        assert_eq!(
            sanitize_title("Report: Q3 <final>").as_deref(),
            Some("Report_ Q3 _final_")
        );

        // A title is user data reaching the disk, so its contribution is bounded.
        let long = "a".repeat(500);
        let bounded = sanitize_title(&long).expect("a long title still yields a name");
        assert_eq!(bounded.chars().count(), MAX_TITLE_CHARS);

        // Truncation lands on a character boundary, never inside a code point.
        let wide = "\u{1f3ac}".repeat(200);
        let bounded = sanitize_title(&wide).expect("a wide title still yields a name");
        assert_eq!(bounded.chars().count(), MAX_TITLE_CHARS);

        assert_eq!(sanitize_title(""), None);
        assert_eq!(sanitize_title("   "), None);
        assert_eq!(sanitize_title(".."), None);
        assert_eq!(sanitize_title("CON"), None);
    }

    #[test]
    fn rename_walks_the_numbered_sequence_and_reserves_its_choice() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("video.mp4");

        let first = resolve_conflict(base.clone(), OnConflict::Rename).unwrap();
        assert_eq!(first.path, base);
        // The reservation exists, so the next caller cannot pick the same name.
        assert!(first.path.exists());
        assert!(first.overwrite);

        let second = resolve_conflict(base.clone(), OnConflict::Rename).unwrap();
        assert_eq!(second.path, directory.path().join("video_2.mp4"));

        let third = resolve_conflict(base.clone(), OnConflict::Rename).unwrap();
        assert_eq!(third.path, directory.path().join("video_3.mp4"));

        // A gap is filled rather than skipped: the lowest free number wins.
        fs::remove_file(directory.path().join("video_2.mp4")).unwrap();
        let fourth = resolve_conflict(base, OnConflict::Rename).unwrap();
        assert_eq!(fourth.path, directory.path().join("video_2.mp4"));
    }

    #[test]
    fn a_failed_download_leaves_no_reservation_behind() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("video.mp4");

        let target = resolve_conflict(base.clone(), OnConflict::Rename).unwrap();
        assert!(target.path.exists(), "the reservation was created");
        release_reservation(&target);
        assert!(
            !target.path.exists(),
            "an unused reservation is residue and must not survive"
        );

        // And the name is free again, so a retry does not start at "_2".
        let retry = resolve_conflict(base.clone(), OnConflict::Rename).unwrap();
        assert_eq!(retry.path, base);
    }

    #[test]
    fn releasing_never_deletes_output_ffmpeg_actually_wrote() {
        let directory = tempfile::tempdir().unwrap();
        let target =
            resolve_conflict(directory.path().join("video.mp4"), OnConflict::Rename).unwrap();
        // The moment FFmpeg writes a byte the file stops being a reservation:
        // "preserve partial output after failures" takes over from here.
        fs::write(&target.path, b"partial media").unwrap();

        release_reservation(&target);
        assert_eq!(fs::read_to_string(&target.path).unwrap(), "partial media");
    }

    #[test]
    fn releasing_never_touches_a_path_we_did_not_reserve() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.mp4");
        fs::write(&path, b"keep me").unwrap();

        // `overwrite` and `fail` never reserve, so an empty file at the target
        // belongs to the user and must survive a failed download.
        let overwriting = resolve_conflict(path.clone(), OnConflict::Overwrite).unwrap();
        assert!(!overwriting.reserved);
        fs::write(&path, b"").unwrap();
        release_reservation(&overwriting);
        assert!(path.exists(), "an empty file we did not create is not ours");
    }

    #[test]
    fn rename_keeps_the_extension_last() {
        let directory = tempfile::tempdir().unwrap();
        let dotless = directory.path().join("noextension");
        fs::write(&dotless, b"old").unwrap();
        let target = resolve_conflict(dotless, OnConflict::Rename).unwrap();
        assert_eq!(target.path, directory.path().join("noextension_2"));

        let dotfile = directory.path().join(".hidden");
        fs::write(&dotfile, b"old").unwrap();
        let target = resolve_conflict(dotfile, OnConflict::Rename).unwrap();
        assert_eq!(target.path, directory.path().join(".hidden_2"));
    }

    #[test]
    fn fail_and_overwrite_policies_leave_the_file_alone_or_replace_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.mp4");
        fs::write(&path, b"keep me").unwrap();

        assert!(matches!(
            resolve_conflict(path.clone(), OnConflict::Fail),
            Err(DownerError::OutputExists(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");

        let target = resolve_conflict(path.clone(), OnConflict::Overwrite).unwrap();
        assert_eq!(target.path, path);
        assert!(target.overwrite);
        // Deciding the policy must not itself touch the file; only FFmpeg writes.
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
    }

    #[test]
    fn no_policy_writes_over_a_directory() {
        let directory = tempfile::tempdir().unwrap();
        let occupied = directory.path().join("taken");
        fs::create_dir(&occupied).unwrap();
        for policy in [OnConflict::Fail, OnConflict::Rename, OnConflict::Overwrite] {
            assert!(
                matches!(
                    resolve_conflict(occupied.clone(), policy),
                    Err(DownerError::OutputDirectory(_))
                ),
                "{policy:?} must refuse a directory"
            );
        }
    }

    /// KEI-90: the three cases, including the one a developer's machine cannot
    /// produce.
    #[test]
    fn the_default_download_directory_prefers_the_desktops_answer() {
        // Whatever the desktop says, even when it is not named "Downloads" —
        // a localised XDG entry is the user's real folder.
        assert_eq!(
            resolve_default_download_dir(
                Some(PathBuf::from("/home/me/Téléchargements")),
                Some(PathBuf::from("/home/me"))
            ),
            Some(PathBuf::from("/home/me/Téléchargements"))
        );
        // No XDG configuration: beside the browser's own default, not `.`.
        assert_eq!(
            resolve_default_download_dir(None, Some(PathBuf::from("/home/me"))),
            Some(PathBuf::from("/home/me/Downloads"))
        );
        // Neither: undecidable, and the caller refuses instead of guessing.
        assert_eq!(resolve_default_download_dir(None, None), None);
    }

    #[test]
    fn refuses_existing_output_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.mp4");
        fs::write(&path, b"old").unwrap();
        assert!(matches!(
            check_output_path(&path, false),
            Err(DownerError::OutputExists(_))
        ));
        assert!(check_output_path(&path, true).is_ok());
    }
}
