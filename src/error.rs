use std::{fmt, io, path::PathBuf};

use crate::ffmpeg::FfmpegVersion;

#[derive(Debug)]
pub enum DownerError {
    InvalidUrl(String),
    OutputExists(PathBuf),
    OutputPath(String),
    OutputDirectory(PathBuf),
    OutputIo(io::Error),
    SourceFetchFailed {
        status: Option<u16>,
        message: String,
    },
    MediaNotFound(String),
    /// A cookie could not be read from the requested source. The message names
    /// the source, never the value.
    CookieSource(String),
    NativeIo(io::Error),
    /// Installing or removing the native messaging host failed part way.
    Host(String),
    /// An argument to a host command named something unusable.
    HostArgument(String),
    FfmpegUnavailable(PathBuf),
    /// FFmpeg ran, but it is older than the minimum this project supports, and
    /// the download failed. Carried as its own error so the CLI and the
    /// extension both name the version and the minimum instead of surfacing
    /// whatever FFmpeg complained about.
    FfmpegTooOld {
        version: FfmpegVersion,
        minimum: FfmpegVersion,
        /// What FFmpeg itself reported, kept because an old FFmpeg can fail for
        /// ordinary reasons too and the detail is still the useful part.
        stderr: String,
    },
    FfmpegFailed {
        status: Option<i32>,
        stderr: String,
    },
    /// `downer doctor` found at least one failing check.
    ///
    /// It carries no message: the checks have already been printed, each with
    /// its own detail and remedy, and restating them as an error would say the
    /// same thing twice in a less useful shape.
    SetupCheckFailed,
}

pub type DownerResult<T> = Result<T, DownerError>;

impl fmt::Display for DownerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(message) => write!(f, "invalid URL: {message}"),
            Self::OutputExists(path) => write!(
                f,
                "output already exists: {} (use --on-conflict rename to write beside it, or --overwrite to replace it)",
                path.display()
            ),
            Self::OutputPath(message) => write!(f, "invalid output path: {message}"),
            Self::OutputDirectory(path) => {
                write!(f, "output path is a directory: {}", path.display())
            }
            Self::OutputIo(error) => write!(f, "could not prepare output: {error}"),
            Self::SourceFetchFailed { status, message } => {
                if let Some(status) = status {
                    write!(f, "could not fetch source page (HTTP {status}): {message}")
                } else {
                    write!(f, "could not fetch source page: {message}")
                }
            }
            Self::MediaNotFound(source) => write!(
                f,
                "no supported m3u8 or video media URL was found in source page: {source}"
            ),
            Self::CookieSource(message) => write!(f, "could not read cookie: {message}"),
            Self::NativeIo(error) => write!(f, "native messaging I/O error: {error}"),
            Self::Host(message) => write!(f, "native host installation failed: {message}"),
            Self::HostArgument(message) => write!(f, "{message}"),
            Self::FfmpegUnavailable(path) => write!(
                f,
                "FFmpeg executable is unavailable or cannot be started: {}",
                path.display()
            ),
            Self::FfmpegTooOld {
                version,
                minimum,
                stderr,
            } => {
                // The headline is this one, not FFmpeg's: an old FFmpeg that
                // fails names itself first, whatever it failed at. KEI-81 pins
                // this wording and `tests/cli.rs` asserts on it. The *detail*
                // gets the same treatment as any other failure — chatter
                // dropped, length bounded, lines kept — because it is the same
                // wall of text (KEI-86).
                write!(
                    f,
                    "FFmpeg {version} is older than the minimum supported {minimum}"
                )?;
                if stderr.trim().is_empty() {
                    Ok(())
                } else {
                    write!(f, ":\n{}", crate::failure::summarize(stderr).detail)
                }
            }
            Self::SetupCheckFailed => {
                write!(f, "setup checks failed; see the report above")
            }
            // Headline first, then the status, then the detail — in that order
            // because the first line is the one a user reads. FFmpeg's stderr
            // buries the cause in the middle and opens with its own
            // configuration; `failure::summarize` picks the cause out and
            // bounds the rest. See KEI-86.
            Self::FfmpegFailed { status, stderr } => {
                if stderr.trim().is_empty() {
                    return write!(f, "FFmpeg failed with status {}", display_status(*status));
                }
                let summary = crate::failure::summarize(stderr);
                write!(
                    f,
                    "{}\n\nFFmpeg failed with status {}:\n{}",
                    summary.headline,
                    display_status(*status),
                    summary.detail
                )
            }
        }
    }
}

fn display_status(status: Option<i32>) -> String {
    status
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

impl std::error::Error for DownerError {}
