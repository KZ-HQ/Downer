pub mod cli;
pub mod diagnostics;
pub mod discovery;
pub mod error;
pub mod failure;
pub mod ffmpeg;
pub mod host;
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
use scraper::{RenditionChoice, ResolvedMedia};

/// Environment variable holding a cookie header, used when neither `--cookie`
/// nor `--cookie-file` is supplied.
pub const COOKIE_ENV: &str = "DOWNER_COOKIE";

pub const INVALID_INPUT_EXIT: i32 = 2;
pub const OUTPUT_EXIT: i32 = 3;
pub const FFMPEG_UNAVAILABLE_EXIT: i32 = 4;
pub const MEDIA_FAILURE_EXIT: i32 = 5;
/// `downer doctor` found a failing check. Distinct from
/// [`FFMPEG_UNAVAILABLE_EXIT`] because a setup failure is not always FFmpeg's:
/// an unregistered host or an unwritable directory fail here too.
pub const SETUP_EXIT: i32 = 6;

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
    /// The playlist the caller already fetched, when it had the session to do
    /// so. Supplying it skips the host's own fetch entirely — the extension
    /// fetches in the page's context, which is the one context a challenged CDN
    /// answers (KEI-87). Absent, the host fetches for itself as before. See
    /// `docs/adr/0011-one-playlist-parser.md`.
    pub playlist_text: Option<String>,
    /// The rendition the caller chose, when they chose one.
    ///
    /// Absent means the default rule decides — highest bandwidth, exactly as
    /// before — so an unchosen download is unchanged (KEI-89's criterion).
    /// Present and not declared by the master is an error, not a fallback; see
    /// [`DownerError::VariantNotOffered`].
    pub rendition: Option<scraper::RenditionChoice>,
    /// Keep the partly written file when a download is **cancelled**.
    ///
    /// Off by default: a cancel is the user saying they do not want this file,
    /// so leaving a broken one in their downloads makes cleaning up their
    /// problem. A *failed* download keeps its partial either way — that one is
    /// the evidence for why it failed, and deleting it is irreversible at the
    /// worst possible moment. See `docs/adr/0012-control-semantics.md`.
    pub keep_partial: bool,
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
/// A rendition choice in the words the user used, for the error that says it
/// was not on offer.
fn describe_rendition(choice: &RenditionChoice) -> String {
    match choice {
        RenditionChoice::Best => "best".to_string(),
        RenditionChoice::Worst => "worst".to_string(),
        RenditionChoice::Height(height) => format!("{height}p"),
        RenditionChoice::Exact(url) => url.to_string(),
    }
}

pub fn run(cli: Cli) -> DownerResult<()> {
    if let Some(command) = cli.command {
        return run_command(command);
    }
    let cookie = resolve_cookie(&cli)?;
    // clap keeps the URL required unless a subcommand takes its place, so this
    // is unreachable with a parsed `Cli`; `expect` says so rather than inventing
    // an error case that no input can reach.
    let url = cli
        .url
        .as_deref()
        .expect("clap requires a URL unless a subcommand is given");

    if cli.list {
        let listing = discovery::list(url, &cli.user_agent, cookie.as_deref())?;
        if cli.json {
            println!("{}", discovery::to_json(&listing));
        } else {
            print!("{}", discovery::listing_text(&listing));
        }
        return Ok(());
    }

    // An unselected download resolves exactly as it always did, down to making
    // the same requests: `resolve_media` keeps the first candidate without the
    // caller ever seeing the list. Only a `--select` or `--media` needs the
    // whole list, so only those pay for assembling it.
    let media = match (cli.select, cli.media.as_deref()) {
        (None, None) => scraper::resolve_media(url, &cli.user_agent, cookie.as_deref())?,
        (select, media) => {
            let source = scraper::resolve_source(url, &cli.user_agent, cookie.as_deref())?;
            ResolvedMedia {
                url: discovery::select(&source, select.map(|n| n as usize), media)?,
                referer: source.referer,
                user_agent: cli.user_agent.clone(),
            }
        }
    };
    let chosen = media.url.clone();
    let json = cli.json;
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
        // `--json` puts one document on stdout and nothing else, so the running
        // commentary has to go. FFmpeg's own progress is unaffected: it goes to
        // stderr under `Reporting::Cli`.
        quiet: json,
        // The CLI has no browser session to fetch with, so the host fetches.
        playlist_text: None,
        rendition: cli.rendition,
        // Nothing can cancel a CLI download: Ctrl-C terminates the process and
        // no cleanup runs. The value is inert here rather than a flag that
        // would do nothing. See ADR-0012.
        keep_partial: true,
    };
    let started = std::time::Instant::now();
    let path = download_resolved(media, &options, Hooks::default())?;
    if json {
        println!(
            "{}",
            discovery::to_json(&discovery::DownloadReport {
                schema_version: discovery::SCHEMA_VERSION,
                url: chosen.to_string(),
                path: path.display().to_string(),
                engine: discovery::ENGINE,
                // Read from the finished file rather than counted as it was
                // written: FFmpeg owns the writing, and the file on disk is the
                // only number that is not an estimate.
                bytes: std::fs::metadata(&path).map(|meta| meta.len()).ok(),
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
        );
    }
    Ok(())
}

fn run_command(command: cli::Command) -> DownerResult<()> {
    match command {
        cli::Command::InstallHost(args) => {
            let report = host::install(&host::InstallOptions {
                link: args.link,
                dev: args.dev,
                ffmpeg: args.ffmpeg,
            })?;
            print_install_report(&report);
            Ok(())
        }
        cli::Command::Doctor(args) => run_doctor(&args),
        cli::Command::UninstallHost(args) => {
            let report = host::uninstall(&host::UninstallOptions {
                binary: args.binary,
            })?;
            if report.removed.is_empty() {
                println!("No native host registration found; nothing to remove.");
            } else {
                for path in &report.removed {
                    println!("Removed {}", path.display());
                }
            }
            Ok(())
        }
    }
}

fn print_install_report(report: &host::InstallReport) {
    println!("Registered native host: {}", report.manifest.display());
    if report.copied {
        println!("Installed binary: {}", report.binary.display());
    } else {
        println!("Using binary in place: {}", report.binary.display());
    }
    println!("Launcher: {}", report.launcher.display());
    if let Some(ffmpeg) = &report.ffmpeg {
        println!("FFmpeg: {}", ffmpeg.display());
    }
    if report.dev {
        println!(
            "Development registration: it stops working if {} is rebuilt elsewhere, moved, or deleted.",
            report.binary.display()
        );
    }
    // KEI-59 adds `downer doctor`; until then the extension's own connection is
    // the check, and saying so beats pointing at a command that does not exist.
    println!(
        "Verify from Firefox: open the Downer popup on a page with media and start a download."
    );
}

/// Render a diagnostics report to the terminal, and fail if anything failed.
///
/// The exit code is the point: a warning is not a failure (an old FFmpeg still
/// downloads — ADR-0006), so only a failing check makes this non-zero, which is
/// what lets the command be used in a script.
fn run_doctor(args: &cli::DoctorArgs) -> DownerResult<()> {
    // The CLI writes into the working directory when `--dir` is absent, so that
    // is what gets checked — the same rule the download itself follows.
    let directory = args.dir.clone().or_else(|| std::env::current_dir().ok());
    let report = diagnostics::run(directory.as_deref(), args.ffmpeg.as_deref());

    println!(
        "downer {} (protocol {}), {}",
        report.host_version, report.protocol_version, report.platform
    );
    println!();
    for check in &report.checks {
        println!("[{}] {}", check.outcome, check.title);
        println!("      {}", check.detail);
        if let Some(remedy) = &check.remedy {
            println!("      → {remedy}");
        }
        // Said against the check it is about, not merely after the last line
        // printed. `doctor` is the command people run to debug the *extension*,
        // and with no `--dir` the directory it checks is the command line's —
        // the working directory — not the one the extension uses. The two
        // differ by design (KEI-90); identical output for different answers
        // would be worse than a sentence.
        if check.name == "output_directory" && args.dir.is_none() {
            println!(
                "      note: this is the command line's directory. The extension uses {}",
                output::default_download_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "no default — set one on its Settings page".to_string())
            );
        }
    }
    println!();

    if report.outcome().is_failure() {
        return Err(DownerError::SetupCheckFailed);
    }
    Ok(())
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
    /// Called once, with the path this download will write, before FFmpeg
    /// starts.
    ///
    /// The caller cannot work this out for itself: it supplies a *directory*,
    /// and the filename is inferred here from the URL, the naming hints and the
    /// collision policy. Without this hook a failed download can only be
    /// reported as "it went wrong somewhere", because the one thing that says
    /// where the fragment is has already been consumed by the error path.
    pub on_target: Option<TargetReporter>,
}

/// Told the path a download will write, once it has been inferred.
pub type TargetReporter = Box<dyn Fn(&Path) + Send + Sync>;

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
            on_target: None,
        }
    }

    /// Also report the path this download resolved to, before it starts.
    pub fn reporting_target<T>(mut self, on_target: T) -> Self
    where
        T: Fn(&Path) + Send + Sync + 'static,
    {
        self.on_target = Some(Box::new(on_target));
        self
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
    // Announced before FFmpeg runs, so a download that fails or is cancelled can
    // still say where its part-written file is. See KEI-86.
    if let Some(on_target) = hooks.on_target.as_ref() {
        on_target(&target.path);
    }

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

    // A master playlist lists renditions; handing one to FFmpeg makes it fetch
    // *every* rendition and write only the best, so the rest is downloaded and
    // discarded. Resolving to the variant we already count means one rendition
    // is fetched, and the same one lands on disk. Best-effort by design: an
    // unresolvable playlist falls back to the URL as given, which is what this
    // did before. See ADR-0010.
    let chosen = if !is_hls(url.as_str()) {
        None
    } else if let Some(text) = options.playlist_text.as_deref() {
        // Already fetched by whoever had the session; parsing it here is what
        // keeps one implementation of what a playlist means.
        match scraper::parse_playlist(text, url) {
            scraper::Playlist::Master(master) => master.choose(options.rendition.as_ref()),
            scraper::Playlist::Media(_) | scraper::Playlist::Unusable => None,
        }
    } else {
        scraper::resolve_variant(
            url,
            &options.user_agent,
            media.referer.as_ref(),
            options.cookie.as_deref(),
            options.rendition.as_ref(),
        )
    };
    // A rendition that was asked for and is not on offer stops the download.
    // Falling back to the default here would hand the user a different quality
    // from the one they picked without saying so.
    if let Some(requested) = &options.rendition {
        if chosen.is_none() {
            return Err(DownerError::VariantNotOffered(describe_rendition(
                requested,
            )));
        }
    }
    let input = chosen
        .as_ref()
        .map_or_else(|| url.clone(), |choice| choice.video.clone());
    // Only when the master carries audio outside the variant. Handing FFmpeg
    // the video alone there loses the sound — measured; see ADR-0014.
    let audio_input = chosen.as_ref().and_then(|choice| choice.audio.clone());
    if !options.quiet && input != *url {
        println!("Selected rendition: {input}");
        if let Some(audio) = &audio_input {
            println!("Selected audio rendition: {audio}");
        }
    }

    let invocation = FfmpegInvocation {
        program: options.ffmpeg.clone(),
        input: input.as_str().to_string(),
        audio_input: audio_input.as_ref().map(|url| url.as_str().to_string()),
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

    // Bound before the callbacks are moved into the executor below. Whether the
    // control was cancelled is what distinguishes "the user stopped this" from
    // "this went wrong", and the two get different treatment on failure.
    let control = hooks.control;
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
            // A cancelled download is one the user said they did not want, so
            // by default its partial goes too. A *failed* one keeps its
            // partial: that file is the evidence for the failure, and a
            // download that died at 90% may still be worth having. See
            // ADR-0012.
            // `cancelled_by_request`, not `is_cancelled`: a job stopped
            // because the native port closed was not cancelled by anyone, and
            // the extension already promises that such a job keeps its file.
            if !options.keep_partial && control.is_some_and(ProcessControl::cancelled_by_request) {
                remove_partial(&target.path);
            }
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

/// Delete a cancelled download's partly written file.
///
/// Best-effort on purpose. The download has already ended and the user has
/// already been told; a file that cannot be removed — permissions, a vanished
/// directory — is not worth turning a clean cancel into an error report.
fn remove_partial(path: &Path) {
    let _ = std::fs::remove_file(path);
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
        DownerError::InvalidUrl(_)
        | DownerError::CookieSource(_)
        | DownerError::HostArgument(_) => INVALID_INPUT_EXIT,
        // Beside `VariantNotOffered`, not with the input errors above: asking
        // for a candidate the source does not offer is the same failure as
        // asking for a rendition it does not declare, one level up. Splitting
        // them across two codes would make the number depend on which kind of
        // choice was named. No new code, so ADR-0018 stands unamended.
        DownerError::OutputExists(_)
        | DownerError::OutputPath(_)
        | DownerError::OutputDirectory(_) => OUTPUT_EXIT,
        // An FFmpeg too old to run this is unusable, not a media failure.
        DownerError::FfmpegUnavailable(_) | DownerError::FfmpegTooOld { .. } => {
            FFMPEG_UNAVAILABLE_EXIT
        }
        DownerError::FfmpegFailed { .. }
        | DownerError::SourceFetchFailed { .. }
        | DownerError::MediaNotFound(_)
        | DownerError::CandidateNotOffered(_)
        | DownerError::VariantNotOffered(_) => MEDIA_FAILURE_EXIT,
        DownerError::SetupCheckFailed => SETUP_EXIT,
        DownerError::OutputIo(_) => OUTPUT_EXIT,
        DownerError::NativeIo(_) | DownerError::Host(_) => 1,
    }
}
