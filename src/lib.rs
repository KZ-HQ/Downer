pub mod cli;
pub mod error;
pub mod ffmpeg;
pub mod native;
pub mod output;
pub mod redact;
pub mod scraper;

use std::path::Path;
use std::path::PathBuf;

use cli::Cli;
use error::{DownerError, DownerResult};
use ffmpeg::{execute, FfmpegInvocation, FfmpegProgress, ProcessControl, Reporting};
use output::{release_reservation, resolve_conflict, resolve_output_path, NamingHints, OnConflict};
use scraper::ResolvedMedia;

/// Environment variable holding a cookie header, used when neither `--cookie`
/// nor `--cookie-file` is supplied.
pub const COOKIE_ENV: &str = "DOWNER_COOKIE";

pub const INVALID_INPUT_EXIT: i32 = 2;
pub const OUTPUT_EXIT: i32 = 3;
pub const FFMPEG_UNAVAILABLE_EXIT: i32 = 4;
pub const MEDIA_FAILURE_EXIT: i32 = 5;

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub output: Option<PathBuf>,
    pub dir: Option<PathBuf>,
    /// Shorthand for [`OnConflict::Overwrite`], kept because the CLI flag and
    /// the protocol's `overwrite` field both predate `on_conflict`.
    pub overwrite: bool,
    /// What to do when the resolved output path is taken. `None` means "decide
    /// from the shape of the request": `Fail` for the exact path given by
    /// `--output`, `Rename` for an inferred filename.
    pub on_conflict: Option<OnConflict>,
    /// Naming material the URL does not carry: a page title and its host.
    pub naming: NamingHints,
    pub ffmpeg: PathBuf,
    pub user_agent: String,
    pub cookie: Option<String>,
    pub threads: Option<u16>,
    pub quiet: bool,
}

impl DownloadOptions {
    /// The collision policy actually in force.
    ///
    /// An explicit `on_conflict` always wins. Otherwise `--overwrite` still
    /// means overwrite, and the default depends on what was asked for: an exact
    /// `--output` path is a place the user named, so a collision there is an
    /// error, while an inferred filename is ours to choose and renames beside
    /// the existing file.
    pub fn conflict_policy(&self) -> OnConflict {
        match (self.on_conflict, self.overwrite, self.output.is_some()) {
            (Some(policy), _, _) => policy,
            (None, true, _) => OnConflict::Overwrite,
            (None, false, true) => OnConflict::Fail,
            (None, false, false) => OnConflict::Rename,
        }
    }
}

/// Resolve the cookie header from the three accepted sources, in order of
/// precedence: `--cookie`, then `--cookie-file`, then the `DOWNER_COOKIE`
/// environment variable.
///
/// `--cookie` and `--cookie-file` together is rejected by the argument parser
/// before this runs, so at most one of them is ever set.
///
/// Errors name the source, never the value: a cookie must not reach a log, an
/// error message, or a terminal. An empty or whitespace-only source is treated
/// as "no cookie" rather than as an empty header.
pub fn resolve_cookie(cli: &Cli) -> DownerResult<Option<String>> {
    if let Some(cookie) = &cli.cookie {
        return Ok(normalize_cookie(cookie));
    }
    if let Some(path) = &cli.cookie_file {
        let contents = std::fs::read_to_string(path)
            .map_err(|error| DownerError::CookieSource(format!("{}: {error}", path.display())))?;
        return Ok(normalize_cookie(&contents));
    }
    match std::env::var(COOKIE_ENV) {
        Ok(value) => Ok(normalize_cookie(&value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(DownerError::CookieSource(format!("{COOKIE_ENV}: {error}"))),
    }
}

/// Trim the surrounding whitespace a file or a shell inevitably adds, and treat
/// a blank source as absent. Control characters are removed by
/// [`scraper::ffmpeg_headers`] when the header block is built, so a cookie can
/// never inject a second header line.
fn normalize_cookie(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Run one download and return an error that the binary can map to a stable exit code.
pub fn run(cli: Cli) -> DownerResult<()> {
    let cookie = resolve_cookie(&cli)?;
    let media = scraper::resolve_media(&cli.url, &cli.user_agent, cookie.as_deref())?;
    let options = DownloadOptions {
        output: cli.output,
        dir: cli.dir,
        overwrite: cli.overwrite,
        on_conflict: cli.on_conflict,
        naming: NamingHints::new(cli.name),
        ffmpeg: cli.ffmpeg,
        user_agent: cli.user_agent,
        cookie,
        threads: cli.threads,
        quiet: false,
    };
    download_resolved(media, &options, Hooks::default()).map(|_| ())
}

/// Optional callbacks for a download.
///
/// All absent — [`Hooks::default`] — is the CLI's case: FFmpeg writes its own
/// progress to the terminal and nothing needs to interrupt it. Supplying a
/// [`ProcessControl`] is what makes a download controllable, and is required
/// before progress or log callbacks can fire, because both are read from a
/// process this owns.
#[derive(Default)]
pub struct Hooks<'a> {
    pub control: Option<&'a ProcessControl>,
    pub on_progress: Option<Box<dyn Fn(FfmpegProgress) + Send + Sync>>,
    pub on_log: Option<Box<dyn Fn(String) + Send + Sync>>,
}

impl<'a> Hooks<'a> {
    /// A controllable download reporting progress and FFmpeg's output.
    pub fn controlled<F, L>(control: &'a ProcessControl, on_progress: F, on_log: L) -> Self
    where
        F: Fn(FfmpegProgress) + Send + Sync + 'static,
        L: Fn(String) + Send + Sync + 'static,
    {
        Self {
            control: Some(control),
            on_progress: Some(Box::new(on_progress)),
            on_log: Some(Box::new(on_log)),
        }
    }
}

/// Download `media` to the path `options` resolves, and return where it landed.
///
/// The one download entry point: the CLI, the native host and the tests all
/// come through here, differing only in `hooks`.
pub fn download_resolved(
    media: ResolvedMedia,
    options: &DownloadOptions,
    hooks: Hooks<'_>,
) -> DownerResult<PathBuf> {
    let url = &media.url;
    let destination = resolve_output_path(
        url,
        options.output.as_deref(),
        options.dir.as_deref(),
        Path::new("."),
        &options.naming,
    )?;
    let target = resolve_conflict(destination, options.conflict_policy())?;
    let destination = target.path.clone();

    if !options.quiet {
        if let Some(referer) = &media.referer {
            println!("Resolved media URL from {}", referer);
        }
        println!("Downloading {}", url.as_str());
        println!("Destination: {}", destination.display());
    }

    // Both the CLI and the native host reach FFmpeg through here, so this is
    // the one place that has to know whether it is old enough to matter.
    let outdated = unsupported_ffmpeg(&options.ffmpeg);
    if let Some(version) = outdated {
        if !options.quiet {
            eprintln!("{}", outdated_ffmpeg_warning(version));
        }
    }

    let invocation = FfmpegInvocation {
        program: options.ffmpeg.clone(),
        input: url.as_str().to_string(),
        headers: scraper::ffmpeg_headers(media.referer.as_ref(), &options.user_agent),
        // Scoped to this URL's host, so a redirect target or a cross-host HLS
        // segment server never receives the media host's session.
        cookies: scraper::ffmpeg_cookies(url, options.cookie.as_deref()),
        // An HLS input needs the leniency options, but only an FFmpeg that has
        // them can be given them; see ADR-0006.
        hls_lenient: is_hls(url.as_str()) && outdated.is_none(),
        threads: options.threads,
        overwrite: target.overwrite,
        output: destination,
        reporting: match hooks.control {
            Some(_) => Reporting::CONTROLLED,
            None => Reporting::Cli,
        },
    };

    let result = match hooks.control {
        Some(control) => {
            let on_progress = hooks.on_progress;
            let on_log = hooks.on_log;
            ffmpeg::execute_controlled_with_progress_and_logs(
                &invocation,
                control,
                move |progress| {
                    if let Some(hook) = &on_progress {
                        hook(progress);
                    }
                },
                move |line| {
                    if let Some(hook) = &on_log {
                        hook(line);
                    }
                },
            )
        }
        None => execute(&invocation),
    };

    let destination = match result {
        Ok(destination) => destination,
        Err(error) => {
            // A name we reserved and never wrote to is residue, not output.
            // Anything FFmpeg did write is kept; `release_reservation` only
            // takes the file back while it is still empty.
            release_reservation(&target);
            return Err(match (outdated, error) {
                // An old FFmpeg that fails names itself, whatever it failed at.
                (Some(version), DownerError::FfmpegFailed { stderr, .. }) => {
                    DownerError::FfmpegTooOld {
                        version,
                        minimum: ffmpeg::MINIMUM_FFMPEG,
                        stderr,
                    }
                }
                (_, error) => error,
            });
        }
    };
    if !options.quiet {
        println!("Download complete: {}", destination.display());
    }
    Ok(destination)
}

/// Whether an input is an HLS playlist.
///
/// A substring match on the URL is what this has always been; the difference
/// is that the answer is now a field on the invocation rather than something
/// the argv builder rediscovers.
fn is_hls(url: &str) -> bool {
    url.to_ascii_lowercase().contains(".m3u8")
}

/// The version of `path`, when it is older than [`ffmpeg::MINIMUM_FFMPEG`].
///
/// `None` covers both "new enough" and "could not tell"; neither changes how a
/// download is built or reported.
pub fn unsupported_ffmpeg(path: &Path) -> Option<ffmpeg::FfmpegVersion> {
    ffmpeg::version(path).filter(|version| !version.meets_minimum())
}

/// The one wording for an FFmpeg below the minimum, so the CLI's warning, the
/// extension's log, and [`DownerError::FfmpegTooOld`] cannot drift apart.
pub fn outdated_ffmpeg_warning(version: ffmpeg::FfmpegVersion) -> String {
    format!(
        "Warning: FFmpeg {version} is older than the minimum supported {}. \
         HLS segment-extension options are being omitted; downloads may fail.",
        ffmpeg::MINIMUM_FFMPEG
    )
}

pub fn exit_code(error: &DownerError) -> i32 {
    match error {
        DownerError::InvalidUrl(_) | DownerError::CookieSource(_) => INVALID_INPUT_EXIT,
        DownerError::OutputExists(_)
        | DownerError::OutputPath(_)
        | DownerError::OutputDirectory(_) => OUTPUT_EXIT,
        // An FFmpeg too old to run this is unusable, not a media failure.
        DownerError::FfmpegUnavailable(_) | DownerError::FfmpegTooOld { .. } => {
            FFMPEG_UNAVAILABLE_EXIT
        }
        DownerError::FfmpegFailed { .. }
        | DownerError::SourceFetchFailed { .. }
        | DownerError::MediaNotFound(_) => MEDIA_FAILURE_EXIT,
        DownerError::OutputIo(_) => OUTPUT_EXIT,
        DownerError::NativeIo(_) => 1,
    }
}
