use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::output::OnConflict;

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
