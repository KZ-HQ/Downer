use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::output::OnConflict;

/// Clap needs a plain function for the default; the value itself lives with the
/// options it configures.
fn downer_default_reconnect_delay() -> u32 {
    crate::ffmpeg::DEFAULT_RECONNECT_DELAY_MAX
}

#[derive(Debug, Parser)]
#[command(
    name = "downer",
    version,
    about = "Download one media URL using FFmpeg",
    long_about = "Download one media URL using FFmpeg. Supports direct media files, HLS playlists, and segmented streams.",
    // `downer URL [options]` is the interface (AGENTS.md), so the URL stays a
    // required positional and the subcommands are an alternative to it rather
    // than a layer above it: `downer install-host` needs no URL, and no option
    // of the download command applies to it.
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// HTTP(S) media URL to download.
    #[arg(value_name = "URL", required = true)]
    pub url: Option<String>,

    /// Write to this exact file path.
    #[arg(short, long, value_name = "PATH", conflicts_with = "dir")]
    pub output: Option<PathBuf>,

    /// Write to this directory, inferring a safe filename.
    #[arg(long, value_name = "DIRECTORY", conflicts_with = "output")]
    pub dir: Option<PathBuf>,

    /// Replace an existing output file. Shorthand for `--on-conflict overwrite`.
    #[arg(long)]
    pub overwrite: bool,

    /// What to do when the output file already exists.
    ///
    /// Defaults to `rename` for an inferred filename and `fail` for the exact
    /// path given by `--output`.
    #[arg(long, value_name = "POLICY", value_enum, conflicts_with = "overwrite")]
    pub on_conflict: Option<OnConflict>,

    /// Title to name the download after when the URL says nothing useful.
    ///
    /// Used only when the URL-derived filename stem is generic (`index`,
    /// `playlist`, `master`, `download`, `video`, `media`, or digits only).
    #[arg(long, value_name = "TITLE")]
    pub name: Option<String>,

    /// Which rendition of an HLS master playlist to download.
    ///
    /// `best` (the default), `worst`, a height such as `720p` or `1280x720`,
    /// or an exact variant URL. A rendition the playlist does not offer stops
    /// the download rather than quietly becoming another one. Listing what a
    /// playlist offers belongs to discovery output and is KEI-62's.
    #[arg(long, value_name = "SELECTOR")]
    pub rendition: Option<crate::scraper::RenditionChoice>,

    /// List what the URL offers and exit without downloading.
    ///
    /// Prints one numbered row per candidate — its kind and URL — and, under a
    /// master playlist, the renditions it declares. Exits 0 when something was
    /// found, so a script can tell "nothing to download" from "the listing
    /// worked".
    #[arg(long, conflicts_with_all = ["select", "media"])]
    pub list: bool,

    /// Print machine-readable JSON instead of prose.
    ///
    /// Applies to `--list` and to the result of a download (`path`, `engine`,
    /// `bytes`, `elapsed_ms`). The document is a stable interface carrying a
    /// `schema_version`; see `docs/adr/0020-json-output-is-a-cli-interface.md`.
    /// Failures stay on stderr with their exit code, which is the
    /// machine-readable form a failure already has (ADR-0018).
    #[arg(long)]
    pub json: bool,

    /// Download the Nth candidate rather than the first.
    ///
    /// Numbered as `--list` prints them, from 1. A number the source does not
    /// offer stops the run rather than falling back to the first.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub select: Option<u32>,

    /// Download this exact candidate URL rather than the first.
    ///
    /// It must be one the source offers — `--list` prints them. To download a
    /// URL on its own, pass it as the argument instead.
    #[arg(long, value_name = "URL", conflicts_with = "select")]
    pub media: Option<String>,

    /// FFmpeg executable to invoke (default: ffmpeg on PATH).
    #[arg(long, value_name = "PATH", default_value = "ffmpeg")]
    pub ffmpeg: PathBuf,

    /// User-Agent used while fetching source pages and media.
    #[arg(
        long,
        value_name = "STRING",
        default_value = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36"
    )]
    pub user_agent: String,

    /// Browser cookie header for pages or media requiring a session.
    ///
    /// The value becomes an argument of this process and of FFmpeg, so it is
    /// visible to other local processes and is recorded in shell history.
    /// Prefer `--cookie-file` or `DOWNER_COOKIE`.
    #[arg(long, value_name = "COOKIE", conflicts_with = "cookie_file")]
    pub cookie: Option<String>,

    /// Read the cookie header from this file instead of the command line.
    ///
    /// The file holds one `name=value; name=value` header line. Leading and
    /// trailing whitespace is ignored. Cannot be combined with `--cookie`.
    #[arg(long, value_name = "PATH")]
    pub cookie_file: Option<PathBuf>,

    /// Number of FFmpeg processing threads; omit to let FFmpeg choose.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub threads: Option<u16>,

    /// Seconds FFmpeg may spend backing off before giving up on a connection.
    ///
    /// Applies to HTTP(S) inputs, including every HLS segment. FFmpeg's own
    /// default is 120, which is longer than anyone watches a stalled progress
    /// bar.
    #[arg(long, value_name = "SECONDS", default_value_t = downer_default_reconnect_delay())]
    pub reconnect_delay_max: u32,

    /// Do not pass FFmpeg any reconnect options.
    ///
    /// For a server that behaves worse when a dropped request is retried, and
    /// for reproducing a failure that reconnection would paper over.
    #[arg(long)]
    pub no_reconnect: bool,

    /// How many times to restart a download that failed for a network reason.
    ///
    /// `0` turns job-level retry off. FFmpeg's own per-connection reconnection
    /// is separate and stays on unless `--no-reconnect` is given. Only failures
    /// classified as transient are retried: a 403 or a 404 is never retried,
    /// whatever this says.
    #[arg(long, value_name = "N", default_value_t = crate::DEFAULT_RETRIES)]
    pub retries: u32,

    /// Seconds a source page or playlist fetch may take in total.
    ///
    /// The connect timeout is derived from it, capped at ten seconds. It does
    /// not bound the download itself, which is FFmpeg's and has no deadline by
    /// design — a large file is not a hung one.
    #[arg(long, value_name = "SECONDS", default_value_t = crate::DEFAULT_TIMEOUT.as_secs())]
    pub timeout: u64,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Everything `downer` does other than download a URL.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Register this binary as Firefox's native messaging host.
    InstallHost(InstallHostArgs),
    /// Remove the native messaging host registration.
    UninstallHost(UninstallHostArgs),
    /// Check that everything a download needs is in place.
    Doctor(DoctorArgs),
}

#[derive(Debug, Parser)]
pub struct DoctorArgs {
    /// Check this FFmpeg instead of the one the native host would choose.
    #[arg(long, value_name = "PATH")]
    pub ffmpeg: Option<PathBuf>,

    /// Also check that this directory can be written to.
    ///
    /// Omitted, the directory check is skipped rather than run against a guess:
    /// a guess that passes says nothing about the directory downloads use.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct InstallHostArgs {
    /// Record this FFmpeg for the host to use.
    ///
    /// Firefox launches the native host with a minimal environment, so
    /// `DOWNER_FFMPEG` is not available to it. The path is written to the host
    /// configuration file and used unless `DOWNER_FFMPEG` overrides it.
    #[arg(long, value_name = "PATH")]
    pub ffmpeg: Option<PathBuf>,

    /// Register the running binary where it is instead of copying it.
    ///
    /// The registration then breaks if that binary is moved or deleted, which
    /// is what `--dev` accepts deliberately.
    #[arg(long)]
    pub link: bool,

    /// Register a development build: implies `--link` and labels the manifest.
    #[arg(long)]
    pub dev: bool,
}

#[derive(Debug, Parser)]
pub struct UninstallHostArgs {
    /// Also delete the copied binary, not just the registration.
    #[arg(long)]
    pub binary: bool,
}
