use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{ChildStderr, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use crate::error::{DownerError, DownerResult};

#[derive(Debug, Clone)]
pub struct FfmpegCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

#[derive(Clone, Debug)]
pub struct ProcessControl {
    child: Arc<Mutex<Option<std::process::Child>>>,
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy)]
pub struct FfmpegProgress {
    pub out_time_ms: Option<u64>,
    pub finished: bool,
}

impl ProcessControl {
    pub fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) -> Result<(), String> {
        self.cancelled.store(true, Ordering::SeqCst);
        let mut child = self
            .child
            .lock()
            .map_err(|_| "FFmpeg process control is unavailable".to_string())?;
        if let Some(child) = child.as_mut() {
            child
                .kill()
                .map_err(|error| format!("could not cancel FFmpeg: {error}"))?;
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("download is already cancelled".to_string());
        }
        self.paused.store(true, Ordering::SeqCst);
        self.signal_if_running(unix_signal::STOP)
    }

    pub fn resume(&self) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("download is already cancelled".to_string());
        }
        self.paused.store(false, Ordering::SeqCst);
        self.signal_if_running(unix_signal::CONT)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn attach(&self, child: std::process::Child) -> Result<(), String> {
        let mut slot = self
            .child
            .lock()
            .map_err(|_| "FFmpeg process control is unavailable".to_string())?;
        *slot = Some(child);
        if self.is_cancelled() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        } else if self.is_paused() {
            if let Some(child) = slot.as_ref() {
                signal_process(child.id(), unix_signal::STOP)?;
            }
        }
        Ok(())
    }

    fn detach(&self) {
        if let Ok(mut child) = self.child.lock() {
            *child = None;
        }
    }

    #[cfg(unix)]
    fn signal_if_running(&self, signal: i32) -> Result<(), String> {
        let child = self
            .child
            .lock()
            .map_err(|_| "FFmpeg process control is unavailable".to_string())?;
        match child.as_ref() {
            Some(child) => signal_process(child.id(), signal),
            None => Ok(()),
        }
    }

    #[cfg(not(unix))]
    fn signal_if_running(&self, _signal: i32) -> Result<(), String> {
        Err("pause and resume are supported only on Unix platforms".to_string())
    }
}

#[cfg(unix)]
fn signal_process(pid: u32, signal: i32) -> Result<(), String> {
    // SAFETY: pid is obtained from the live child process we spawned.
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "could not control FFmpeg: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn signal_process(_pid: u32, _signal: i32) -> Result<(), String> {
    Err("pause and resume are supported only on Unix platforms".to_string())
}

impl Default for ProcessControl {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
mod unix_signal {
    pub const CONT: i32 = libc::SIGCONT;
    pub const STOP: i32 = libc::SIGSTOP;
}

#[cfg(not(unix))]
mod unix_signal {
    pub const CONT: i32 = 0;
    pub const STOP: i32 = 0;
}

impl FfmpegCommand {
    pub fn new(program: PathBuf, url: &str, output: PathBuf, overwrite: bool) -> Self {
        Self::new_with_headers(program, url, output, overwrite, None)
    }

    pub fn new_with_headers(
        program: PathBuf,
        url: &str,
        output: PathBuf,
        overwrite: bool,
        headers: Option<&str>,
    ) -> Self {
        Self::new_with_headers_and_threads(program, url, output, overwrite, headers, None, None)
    }

    /// `cookies` is the newline-delimited Set-Cookie syntax FFmpeg takes as
    /// `-cookies`, built by `scraper::ffmpeg_cookies`. It is passed separately
    /// from `headers` because FFmpeg scopes `-cookies` per request host while
    /// applying `-headers` to every request for the input.
    pub fn new_with_headers_and_threads(
        program: PathBuf,
        url: &str,
        output: PathBuf,
        overwrite: bool,
        headers: Option<&str>,
        cookies: Option<&str>,
        threads: Option<u16>,
    ) -> Self {
        let mut args = vec![
            OsString::from("-hide_banner"),
            OsString::from("-loglevel"),
            OsString::from("error"),
            OsString::from("-stats"),
        ];
        if url.to_ascii_lowercase().contains(".m3u8") {
            args.push(OsString::from("-allowed_segment_extensions"));
            args.push(OsString::from("ALL"));
            args.push(OsString::from("-extension_picky"));
            args.push(OsString::from("0"));
        }
        if let Some(headers) = headers {
            args.push(OsString::from("-headers"));
            args.push(OsString::from(headers));
        }
        if let Some(cookies) = cookies {
            args.push(OsString::from("-cookies"));
            args.push(OsString::from(cookies));
        }
        args.extend([
            OsString::from("-i"),
            OsString::from(url),
            OsString::from("-c"),
            OsString::from("copy"),
        ]);
        if let Some(threads) = threads {
            args.push(OsString::from("-threads"));
            args.push(OsString::from(threads.to_string()));
        }
        args.push(OsString::from(if overwrite { "-y" } else { "-n" }));
        args.push(output.into_os_string());
        Self { program, args }
    }
}

pub fn execute(command: &FfmpegCommand) -> DownerResult<PathBuf> {
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) {
                DownerError::FfmpegUnavailable(command.program.clone())
            } else {
                DownerError::FfmpegFailed {
                    status: None,
                    stderr: error.to_string(),
                }
            }
        })?;

    let stderr = child
        .stderr
        .take()
        .map(capture_and_display)
        .unwrap_or_else(|| Ok(Vec::new()));
    let status = child.wait().map_err(|error| DownerError::FfmpegFailed {
        status: None,
        stderr: error.to_string(),
    })?;
    let stderr = stderr
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|error| error.to_string());

    if status.success() {
        Ok(command_output_path(&command.args))
    } else {
        Err(DownerError::FfmpegFailed {
            status: status.code(),
            stderr,
        })
    }
}

pub fn execute_controlled(
    command: &FfmpegCommand,
    control: &ProcessControl,
) -> DownerResult<PathBuf> {
    execute_controlled_with_progress(command, control, |_| {})
}

pub fn execute_controlled_with_progress<F>(
    command: &FfmpegCommand,
    control: &ProcessControl,
    progress: F,
) -> DownerResult<PathBuf>
where
    F: Fn(FfmpegProgress) + Send + Sync + 'static,
{
    execute_controlled_with_progress_and_logs(command, control, progress, |_| {})
}

pub fn execute_controlled_with_progress_and_logs<F, L>(
    command: &FfmpegCommand,
    control: &ProcessControl,
    progress: F,
    log: L,
) -> DownerResult<PathBuf>
where
    F: Fn(FfmpegProgress) + Send + Sync + 'static,
    L: Fn(String) + Send + Sync + 'static,
{
    let mut args = command.args.clone();
    args.retain(|argument| argument != "-stats");
    args.splice(
        3..3,
        [
            OsString::from("-loglevel"),
            OsString::from("info"),
            OsString::from("-nostats"),
            OsString::from("-stats_period"),
            OsString::from("0.5"),
            OsString::from("-progress"),
            OsString::from("pipe:1"),
        ],
    );
    let mut child = Command::new(&command.program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) {
                DownerError::FfmpegUnavailable(command.program.clone())
            } else {
                DownerError::FfmpegFailed {
                    status: None,
                    stderr: error.to_string(),
                }
            }
        })?;

    let progress_output = child.stdout.take();
    let stderr = child.stderr.take();
    control
        .attach(child)
        .map_err(|error| DownerError::FfmpegFailed {
            status: None,
            stderr: error,
        })?;
    let progress_thread =
        progress_output.map(|output| thread::spawn(|| capture_progress(output, progress)));
    let stderr_thread =
        stderr.map(|stderr| thread::spawn(|| capture_and_display_with_callback(stderr, log)));

    let status = loop {
        let status = control
            .child
            .lock()
            .map_err(|_| DownerError::FfmpegFailed {
                status: None,
                stderr: "FFmpeg process control is unavailable".to_string(),
            })?
            .as_mut()
            .and_then(|child| child.try_wait().transpose())
            .transpose()
            .map_err(|error| DownerError::FfmpegFailed {
                status: None,
                stderr: error.to_string(),
            })?;
        if let Some(status) = status {
            break status;
        }
        thread::sleep(Duration::from_millis(100));
    };

    let progress_error = match progress_thread {
        Some(thread) => match thread.join() {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(_) => Some("FFmpeg progress thread panicked".to_string()),
        },
        None => None,
    };
    let mut stderr = match stderr_thread {
        Some(thread) => match thread.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("FFmpeg output thread panicked")),
        },
        None => Ok(Vec::new()),
    }
    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    .unwrap_or_else(|error| error.to_string());
    if let Some(error) = progress_error {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&error);
    }
    control.detach();

    if status.success() {
        Ok(command_output_path(&command.args))
    } else {
        Err(DownerError::FfmpegFailed {
            status: status.code(),
            stderr,
        })
    }
}

fn command_output_path(args: &[OsString]) -> PathBuf {
    args.last()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("output"))
}

fn capture_and_display(stderr: ChildStderr) -> std::io::Result<Vec<u8>> {
    capture_and_display_with_callback(stderr, |_| {})
}

fn capture_and_display_with_callback<L>(mut stderr: ChildStderr, log: L) -> std::io::Result<Vec<u8>>
where
    L: Fn(String),
{
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut pending = String::new();
    loop {
        let count = stderr.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let chunk = String::from_utf8_lossy(&buffer[..count]);
        eprint!("{chunk}");
        pending.push_str(&chunk);
        let mut start = 0;
        for (index, character) in pending.char_indices() {
            if !matches!(character, '\n' | '\r') {
                continue;
            }
            let line = pending[start..index].trim();
            if !line.is_empty() {
                log(line.to_string());
            }
            start = index + character.len_utf8();
        }
        if start > 0 {
            pending.drain(..start);
        }
        captured.extend_from_slice(&buffer[..count]);
    }
    let line = pending.trim();
    if !line.is_empty() {
        log(line.to_string());
    }
    Ok(captured)
}

fn capture_progress<R, F>(output: R, progress: F) -> std::io::Result<()>
where
    R: Read,
    F: Fn(FfmpegProgress),
{
    let mut reader = BufReader::new(output);
    let mut line = String::new();
    let mut out_time_ms = None;
    loop {
        line.clear();
        let count = reader.read_line(&mut line)?;
        if count == 0 {
            break;
        }
        if let Some(value) = line.strip_prefix("out_time_us=") {
            out_time_ms = value.trim().parse::<u64>().ok().map(|value| value / 1_000);
        } else if let Some(value) = line.strip_prefix("out_time_ms=") {
            out_time_ms = value.trim().parse::<u64>().ok().map(|value| value / 1_000);
        } else if line.trim() == "progress=continue" {
            progress(FfmpegProgress {
                out_time_ms,
                finished: false,
            });
        } else if line.trim() == "progress=end" {
            progress(FfmpegProgress {
                out_time_ms,
                finished: true,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn builds_arguments_without_shell_interpolation() {
        let command = FfmpegCommand::new(
            PathBuf::from("ffmpeg"),
            "https://example.test/video?a=$HOME;echo injected",
            PathBuf::from("a file;name.mp4"),
            false,
        );
        let args: Vec<String> = command
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"https://example.test/video?a=$HOME;echo injected".to_string()));
        assert!(args.contains(&"a file;name.mp4".to_string()));
        assert!(args.contains(&"-n".to_string()));
        assert!(!args.iter().any(|arg| arg == "sh"));

        let threaded = FfmpegCommand::new_with_headers_and_threads(
            PathBuf::from("ffmpeg"),
            "https://example.test/video.mp4",
            PathBuf::from("video.mp4"),
            false,
            None,
            None,
            Some(4),
        );
        let threaded_args: Vec<String> = threaded
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(threaded_args
            .windows(2)
            .any(|pair| pair == ["-threads", "4"]));

        let overwrite = FfmpegCommand::new(
            PathBuf::from("ffmpeg"),
            "https://example.test/video.mp4",
            PathBuf::from("video.mp4"),
            true,
        );
        assert!(overwrite.args.iter().any(|arg| arg == "-y"));

        let with_headers = FfmpegCommand::new_with_headers(
            PathBuf::from("ffmpeg"),
            "https://example.test/video.mp4",
            PathBuf::from("video.mp4"),
            false,
            Some("Referer: https://example.test/page\r\n"),
        );
        assert!(with_headers.args.iter().any(|arg| arg == "-headers"));
        assert!(with_headers
            .args
            .iter()
            .any(|arg| arg == "Referer: https://example.test/page\r\n"));
        assert!(
            !with_headers.args.iter().any(|arg| arg == "-cookies"),
            "no -cookies argument when there are no cookies"
        );

        let with_cookies = FfmpegCommand::new_with_headers_and_threads(
            PathBuf::from("ffmpeg"),
            "https://example.test/video.mp4",
            PathBuf::from("video.mp4"),
            false,
            None,
            Some("sid=downer-sentinel; path=/; domain=example.test"),
            None,
        );
        let cookie_args: Vec<String> = with_cookies
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let index = cookie_args
            .iter()
            .position(|arg| arg == "-cookies")
            .expect("-cookies is passed");
        assert_eq!(
            cookie_args[index + 1],
            "sid=downer-sentinel; path=/; domain=example.test"
        );
        assert!(
            index < cookie_args.iter().position(|arg| arg == "-i").unwrap(),
            "-cookies is an input option: {cookie_args:?}"
        );

        let hls = FfmpegCommand::new(
            PathBuf::from("ffmpeg"),
            "https://example.test/playlist.m3u8",
            PathBuf::from("video.mp4"),
            false,
        );
        assert!(hls
            .args
            .iter()
            .any(|arg| arg == "-allowed_segment_extensions"));
        assert!(hls.args.iter().any(|arg| arg == "ALL"));
        assert!(hls.args.iter().any(|arg| arg == "-extension_picky"));
        assert!(hls.args.iter().any(|arg| arg == "0"));

        let direct = FfmpegCommand::new(
            PathBuf::from("ffmpeg"),
            "https://example.test/video.mp4",
            PathBuf::from("video.mp4"),
            false,
        );
        assert!(!direct
            .args
            .iter()
            .any(|arg| arg == "-allowed_segment_extensions"));
    }

    #[test]
    fn parses_ffmpeg_progress_from_a_dedicated_stream() {
        let input = Cursor::new(
            b"out_time_us=1500000\nout_time_ms=1500000\nprogress=continue\nprogress=end\n",
        );
        let updates = Arc::new(Mutex::new(Vec::new()));
        let captured = updates.clone();
        capture_progress(input, move |progress| {
            captured.lock().unwrap().push(progress)
        })
        .unwrap();
        let updates = updates.lock().unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].out_time_ms, Some(1_500));
        assert!(!updates[0].finished);
        assert!(updates[1].finished);
    }

    #[cfg(unix)]
    #[test]
    fn pause_and_resume_can_be_queued_before_ffmpeg_starts() {
        let control = ProcessControl::new();
        control.pause().unwrap();
        assert!(control.is_paused());
        control.resume().unwrap();
        assert!(!control.is_paused());
        control.cancel().unwrap();
        assert!(control.is_cancelled());
    }
}
