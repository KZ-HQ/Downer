use std::{
    fs, io,
    path::{Path, PathBuf},
};

use percent_encoding::percent_decode_str;
use url::Url;

use crate::error::{DownerError, DownerResult};

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

pub fn resolve_output_path(
    url: &Url,
    output: Option<&Path>,
    dir: Option<&Path>,
    current_dir: &Path,
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
    let filename = infer_filename(url);
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
        filename = "download".to_string();
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
            "download.mp4"
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
            "download.mp4"
        );
        assert_eq!(
            infer_filename(&Url::parse("https://example.test/evil%2Fname.mp4").unwrap()),
            "evil_name.mp4"
        );
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
