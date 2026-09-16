use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "downer",
    version,
    about = "Download one media URL using FFmpeg",
    long_about = "Download one media URL using FFmpeg. Supports direct media files, HLS playlists, and segmented streams."
)]
pub struct Cli {
    /// HTTP(S) media URL to download.
    #[arg(value_name = "URL")]
    pub url: String,

    /// Write to this exact file path.
    #[arg(short, long, value_name = "PATH", conflicts_with = "dir")]
    pub output: Option<PathBuf>,

    /// Write to this directory, inferring a safe filename.
    #[arg(long, value_name = "DIRECTORY", conflicts_with = "output")]
    pub dir: Option<PathBuf>,

    /// Replace an existing output file.
    #[arg(long)]
    pub overwrite: bool,

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

    /// Optional browser cookie header for pages or media requiring a session.
    #[arg(long, value_name = "COOKIE")]
    pub cookie: Option<String>,

    /// Number of FFmpeg processing threads; omit to let FFmpeg choose.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub threads: Option<u16>,
}
