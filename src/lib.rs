pub mod cli;
pub mod error;
pub mod ffmpeg;
pub mod native;
pub mod output;
pub mod scraper;

use std::path::Path;
use std::path::PathBuf;

use cli::Cli;
use error::{DownerError, DownerResult};
use ffmpeg::{
    execute, execute_controlled_with_progress, FfmpegCommand, FfmpegProgress, ProcessControl,
};
use output::{check_output_path, resolve_output_path};
use scraper::ResolvedMedia;

pub const INVALID_INPUT_EXIT: i32 = 2;
pub const OUTPUT_EXIT: i32 = 3;
pub const FFMPEG_UNAVAILABLE_EXIT: i32 = 4;
pub const MEDIA_FAILURE_EXIT: i32 = 5;

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub output: Option<PathBuf>,
    pub dir: Option<PathBuf>,
    pub overwrite: bool,
    pub ffmpeg: PathBuf,
    pub user_agent: String,
    pub cookie: Option<String>,
    pub threads: Option<u16>,
    pub quiet: bool,
}

/// Run one download and return an error that the binary can map to a stable exit code.
pub fn run(cli: Cli) -> DownerResult<()> {
    let media = scraper::resolve_media(&cli.url, &cli.user_agent, cli.cookie.as_deref())?;
    let options = DownloadOptions {
        output: cli.output,
        dir: cli.dir,
        overwrite: cli.overwrite,
        ffmpeg: cli.ffmpeg,
        user_agent: cli.user_agent,
        cookie: cli.cookie,
        threads: cli.threads,
        quiet: false,
    };
    download_resolved(media, &options).map(|_| ())
}

pub fn download_resolved(media: ResolvedMedia, options: &DownloadOptions) -> DownerResult<PathBuf> {
    download_resolved_with_executor(media, options, execute)
}

pub fn download_resolved_controlled(
    media: ResolvedMedia,
    options: &DownloadOptions,
    control: &ProcessControl,
) -> DownerResult<PathBuf> {
    download_resolved_controlled_with_progress(media, options, control, |_| {})
}

pub fn download_resolved_controlled_with_progress<F>(
    media: ResolvedMedia,
    options: &DownloadOptions,
    control: &ProcessControl,
    progress: F,
) -> DownerResult<PathBuf>
where
    F: Fn(FfmpegProgress) + Send + Sync + 'static,
{
    download_resolved_with_executor(media, options, |command| {
        execute_controlled_with_progress(command, control, progress)
    })
}

pub fn download_resolved_controlled_with_progress_and_logs<F, L>(
    media: ResolvedMedia,
    options: &DownloadOptions,
    control: &ProcessControl,
    progress: F,
    log: L,
) -> DownerResult<PathBuf>
where
    F: Fn(FfmpegProgress) + Send + Sync + 'static,
    L: Fn(String) + Send + Sync + 'static,
{
    download_resolved_with_executor(media, options, |command| {
        ffmpeg::execute_controlled_with_progress_and_logs(command, control, progress, log)
    })
}

fn download_resolved_with_executor(
    media: ResolvedMedia,
    options: &DownloadOptions,
    execute: impl FnOnce(&FfmpegCommand) -> DownerResult<PathBuf>,
) -> DownerResult<PathBuf> {
    let url = &media.url;
    let destination = resolve_output_path(
        url,
        options.output.as_deref(),
        options.dir.as_deref(),
        Path::new("."),
    )?;
    check_output_path(&destination, options.overwrite)?;

    if !options.quiet {
        if let Some(referer) = &media.referer {
            println!("Resolved media URL from {}", referer);
        }
        println!("Downloading {}", url.as_str());
        println!("Destination: {}", destination.display());
    }

    let headers = scraper::ffmpeg_headers(
        media.referer.as_ref(),
        &options.user_agent,
        options.cookie.as_deref(),
    );
    let command = FfmpegCommand::new_with_headers_and_threads(
        options.ffmpeg.clone(),
        url.as_str(),
        destination,
        options.overwrite,
        headers.as_deref(),
        options.threads,
    );
    let destination = execute(&command)?;
    if !options.quiet {
        println!("Download complete: {}", destination.display());
    }
    Ok(destination)
}

pub fn exit_code(error: &DownerError) -> i32 {
    match error {
        DownerError::InvalidUrl(_) => INVALID_INPUT_EXIT,
        DownerError::OutputExists(_)
        | DownerError::OutputPath(_)
        | DownerError::OutputDirectory(_) => OUTPUT_EXIT,
        DownerError::FfmpegUnavailable(_) => FFMPEG_UNAVAILABLE_EXIT,
        DownerError::FfmpegFailed { .. }
        | DownerError::SourceFetchFailed { .. }
        | DownerError::MediaNotFound(_) => MEDIA_FAILURE_EXIT,
        DownerError::OutputIo(_) => OUTPUT_EXIT,
        DownerError::NativeIo(_) => 1,
    }
}
