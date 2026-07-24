//! Bounded subprocess execution for compiler-test harnesses.
//!
//! Every child is placed in its own process group on Unix. Deadlines and
//! explicit cancellation terminate that group and reap the direct child, while
//! stdout and stderr are drained concurrently into fixed-size buffers. A
//! capture limit is a hard failure, never a successful result with silently
//! incomplete output.

use std::fmt;
use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_CAPTURE_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(1);
const MAX_TIMEOUT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_CAPTURE_LIMIT: usize = 1024 * 1024 * 1024;
const MAX_KILL_GRACE: Duration = Duration::from_secs(60);

/// Harness command categories with independently configurable deadlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Compile,
    Run,
    Tool,
    Project,
}

impl CommandClass {
    fn timeout_env(self) -> &'static str {
        match self {
            Self::Compile => "BENCCH_COMPILE_TIMEOUT_SECS",
            Self::Run => "BENCCH_RUN_TIMEOUT_SECS",
            Self::Tool => "BENCCH_TOOL_TIMEOUT_SECS",
            Self::Project => "BENCCH_PROJECT_TIMEOUT_SECS",
        }
    }

    fn default_timeout(self) -> Duration {
        match self {
            Self::Compile => Duration::from_secs(120),
            Self::Run => Duration::from_secs(30),
            Self::Tool => Duration::from_secs(60),
            Self::Project => Duration::from_secs(30 * 60),
        }
    }
}

/// Resource limits for one managed command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLimits {
    pub timeout: Duration,
    pub kill_grace: Duration,
    /// Maximum retained bytes for each of stdout and stderr.
    pub capture_limit: usize,
}

impl CommandLimits {
    pub fn for_class(class: CommandClass) -> Result<Self, String> {
        let timeout = duration_from_env(class.timeout_env(), class.default_timeout(), MAX_TIMEOUT)?;
        let kill_grace =
            duration_millis_from_env("BENCCH_KILL_GRACE_MS", DEFAULT_KILL_GRACE, MAX_KILL_GRACE)?;
        let capture_limit = usize_from_env(
            "BENCCH_OUTPUT_LIMIT_BYTES",
            DEFAULT_CAPTURE_LIMIT,
            MAX_CAPTURE_LIMIT,
        )?;
        Ok(Self {
            timeout,
            kill_grace,
            capture_limit,
        })
    }

    pub fn validate(self) -> Result<Self, String> {
        if self.timeout.is_zero() {
            return Err("managed command timeout must be greater than zero".into());
        }
        if self.timeout > MAX_TIMEOUT {
            return Err(format!(
                "managed command timeout {:?} exceeds the maximum {:?}",
                self.timeout, MAX_TIMEOUT
            ));
        }
        if self.kill_grace.is_zero() {
            return Err("managed command kill grace must be greater than zero".into());
        }
        if self.kill_grace > MAX_KILL_GRACE {
            return Err(format!(
                "managed command kill grace {:?} exceeds the maximum {:?}",
                self.kill_grace, MAX_KILL_GRACE
            ));
        }
        if self.capture_limit == 0 {
            return Err("managed command capture limit must be greater than zero".into());
        }
        if self.capture_limit > MAX_CAPTURE_LIMIT {
            return Err(format!(
                "managed command capture limit {} exceeds the maximum {}",
                self.capture_limit, MAX_CAPTURE_LIMIT
            ));
        }
        Ok(self)
    }
}

fn duration_from_env(key: &str, default: Duration, maximum: Duration) -> Result<Duration, String> {
    let Some(raw) = std::env::var_os(key) else {
        return Ok(default);
    };
    let raw = raw
        .to_str()
        .ok_or_else(|| format!("{key} is not valid UTF-8"))?;
    let seconds = raw
        .parse::<u64>()
        .map_err(|_| format!("{key} must be a positive integer number of seconds, got '{raw}'"))?;
    let value = Duration::from_secs(seconds);
    if value.is_zero() || value > maximum {
        return Err(format!(
            "{key} must be between 1 and {} seconds, got '{raw}'",
            maximum.as_secs()
        ));
    }
    Ok(value)
}

fn duration_millis_from_env(
    key: &str,
    default: Duration,
    maximum: Duration,
) -> Result<Duration, String> {
    let Some(raw) = std::env::var_os(key) else {
        return Ok(default);
    };
    let raw = raw
        .to_str()
        .ok_or_else(|| format!("{key} is not valid UTF-8"))?;
    let milliseconds = raw.parse::<u64>().map_err(|_| {
        format!("{key} must be a positive integer number of milliseconds, got '{raw}'")
    })?;
    let value = Duration::from_millis(milliseconds);
    if value.is_zero() || value > maximum {
        return Err(format!(
            "{key} must be between 1 and {} milliseconds, got '{raw}'",
            maximum.as_millis()
        ));
    }
    Ok(value)
}

fn usize_from_env(key: &str, default: usize, maximum: usize) -> Result<usize, String> {
    let Some(raw) = std::env::var_os(key) else {
        return Ok(default);
    };
    let raw = raw
        .to_str()
        .ok_or_else(|| format!("{key} is not valid UTF-8"))?;
    let value = raw
        .parse::<usize>()
        .map_err(|_| format!("{key} must be a positive integer byte count, got '{raw}'"))?;
    if value == 0 || value > maximum {
        return Err(format!(
            "{key} must be between 1 and {maximum} bytes, got '{raw}'"
        ));
    }
    Ok(value)
}

/// Cooperative cancellation handle for a managed command.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedStream {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedStreams {
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
}

#[derive(Debug)]
pub struct ManagedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum ManagedCommandError {
    Configuration(String),
    Spawn(io::Error),
    Monitor {
        error: io::Error,
        captured: CapturedStreams,
    },
    TimedOut {
        timeout: Duration,
        captured: CapturedStreams,
    },
    Cancelled {
        captured: CapturedStreams,
    },
    OutputLimitExceeded {
        status: Option<ExitStatus>,
        limit: usize,
        captured: CapturedStreams,
    },
    CaptureIncomplete {
        status: ExitStatus,
        detail: String,
        captured: CapturedStreams,
    },
}

impl ManagedCommandError {
    pub fn captured(&self) -> Option<&CapturedStreams> {
        match self {
            Self::Monitor { captured, .. }
            | Self::TimedOut { captured, .. }
            | Self::Cancelled { captured }
            | Self::OutputLimitExceeded { captured, .. }
            | Self::CaptureIncomplete { captured, .. } => Some(captured),
            Self::Configuration(_) | Self::Spawn(_) => None,
        }
    }
}

impl fmt::Display for ManagedCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(detail) => {
                write!(formatter, "managed command configuration error: {detail}")
            }
            Self::Spawn(error) => write!(formatter, "cannot spawn managed command: {error}"),
            Self::Monitor { error, captured } => {
                write!(formatter, "cannot monitor managed command: {error}")?;
                write_captured_streams(formatter, captured)
            }
            Self::TimedOut { timeout, captured } => {
                write!(
                    formatter,
                    "managed command timed out after {} ms",
                    timeout.as_millis()
                )?;
                write_captured_streams(formatter, captured)
            }
            Self::Cancelled { captured } => {
                write!(formatter, "managed command was cancelled")?;
                write_captured_streams(formatter, captured)
            }
            Self::OutputLimitExceeded {
                status,
                limit,
                captured,
            } => {
                write!(
                    formatter,
                    "managed command exceeded the {limit}-byte per-stream output limit"
                )?;
                if let Some(status) = status {
                    write!(formatter, " (status {status})")?;
                }
                write_captured_streams(formatter, captured)
            }
            Self::CaptureIncomplete {
                status,
                detail,
                captured,
            } => {
                write!(
                    formatter,
                    "managed command output capture did not close after status {status}: {detail}"
                )?;
                write_captured_streams(formatter, captured)
            }
        }
    }
}

impl std::error::Error for ManagedCommandError {}

fn write_captured_streams(
    formatter: &mut fmt::Formatter<'_>,
    captured: &CapturedStreams,
) -> fmt::Result {
    write_captured_stream(formatter, "stdout", &captured.stdout)?;
    write_captured_stream(formatter, "stderr", &captured.stderr)
}

fn write_captured_stream(
    formatter: &mut fmt::Formatter<'_>,
    label: &str,
    captured: &CapturedStream,
) -> fmt::Result {
    if captured.bytes.is_empty() && !captured.truncated && captured.complete {
        return Ok(());
    }
    write!(
        formatter,
        "\n{label}{}{}:\n{}",
        if captured.truncated {
            " [TRUNCATED]"
        } else {
            ""
        },
        if captured.complete {
            ""
        } else {
            " [INCOMPLETE]"
        },
        String::from_utf8_lossy(&captured.bytes)
    )
}

pub fn run(
    command: &mut Command,
    class: CommandClass,
) -> Result<ManagedOutput, ManagedCommandError> {
    let limits = CommandLimits::for_class(class).map_err(ManagedCommandError::Configuration)?;
    run_with_limits(command, limits, None)
}

pub fn run_cancellable(
    command: &mut Command,
    class: CommandClass,
    cancellation: &CancellationToken,
) -> Result<ManagedOutput, ManagedCommandError> {
    let limits = CommandLimits::for_class(class).map_err(ManagedCommandError::Configuration)?;
    run_with_limits(command, limits, Some(cancellation))
}

pub fn run_with_limits(
    command: &mut Command,
    limits: CommandLimits,
    cancellation: Option<&CancellationToken>,
) -> Result<ManagedOutput, ManagedCommandError> {
    let limits = limits
        .validate()
        .map_err(ManagedCommandError::Configuration)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(ManagedCommandError::Spawn)?;
    let process_group = child.id();
    let stdout = child
        .stdout
        .take()
        .expect("stdout is piped before the managed child is spawned");
    let stderr = child
        .stderr
        .take()
        .expect("stderr is piped before the managed child is spawned");
    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = start_reader(
        stdout,
        limits.capture_limit,
        Arc::clone(&output_limit_exceeded),
    );
    let stderr_reader = start_reader(
        stderr,
        limits.capture_limit,
        Arc::clone(&output_limit_exceeded),
    );

    enum StopReason {
        Exited(ExitStatus),
        TimedOut,
        Cancelled,
        OutputLimitExceeded,
        Monitor(io::Error),
    }

    let deadline = Instant::now() + limits.timeout;
    let reason = loop {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            break StopReason::Cancelled;
        }
        if output_limit_exceeded.load(Ordering::Acquire) {
            break StopReason::OutputLimitExceeded;
        }
        match child.try_wait() {
            Ok(Some(status)) => break StopReason::Exited(status),
            Ok(None) => {}
            Err(error) => break StopReason::Monitor(error),
        }
        let now = Instant::now();
        if now >= deadline {
            break StopReason::TimedOut;
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    };

    let (status, timed_out, cancelled, output_limited, mut monitor_error) = match reason {
        StopReason::Exited(status) => {
            let cleanup_error = kill_lingering_group(process_group).err();
            (Some(status), false, false, false, cleanup_error)
        }
        StopReason::TimedOut => {
            match terminate_and_reap(&mut child, process_group, limits.kill_grace) {
                Ok(status) => (Some(status), true, false, false, None),
                Err(error) => (None, true, false, false, Some(error)),
            }
        }
        StopReason::Cancelled => {
            match terminate_and_reap(&mut child, process_group, limits.kill_grace) {
                Ok(status) => (Some(status), false, true, false, None),
                Err(error) => (None, false, true, false, Some(error)),
            }
        }
        StopReason::OutputLimitExceeded => {
            match terminate_and_reap(&mut child, process_group, limits.kill_grace) {
                Ok(status) => (Some(status), false, false, true, None),
                Err(error) => (None, false, false, true, Some(error)),
            }
        }
        StopReason::Monitor(error) => {
            let cleanup_error =
                terminate_and_reap(&mut child, process_group, limits.kill_grace).err();
            (
                None,
                false,
                false,
                false,
                Some(cleanup_error.unwrap_or(error)),
            )
        }
    };

    let capture_deadline = Instant::now() + limits.kill_grace;
    let stdout_result = stdout_reader.finish(capture_deadline);
    let stderr_result = stderr_reader.finish(capture_deadline);
    let captured = CapturedStreams {
        stdout: stdout_result.stream,
        stderr: stderr_result.stream,
    };

    if timed_out {
        if let Some(error) = monitor_error.take() {
            return Err(ManagedCommandError::Monitor {
                error: io::Error::other(format!(
                    "command timed out after {} ms and process-tree cleanup failed: {error}",
                    limits.timeout.as_millis()
                )),
                captured,
            });
        }
        return Err(ManagedCommandError::TimedOut {
            timeout: limits.timeout,
            captured,
        });
    }
    if cancelled {
        if let Some(error) = monitor_error.take() {
            return Err(ManagedCommandError::Monitor {
                error: io::Error::other(format!(
                    "command was cancelled but process-tree cleanup failed: {error}"
                )),
                captured,
            });
        }
        return Err(ManagedCommandError::Cancelled { captured });
    }
    if output_limited {
        if let Some(error) = monitor_error.take() {
            return Err(ManagedCommandError::Monitor {
                error: io::Error::other(format!(
                    "command exceeded the output limit but process-tree cleanup failed: {error}"
                )),
                captured,
            });
        }
        return Err(ManagedCommandError::OutputLimitExceeded {
            status,
            limit: limits.capture_limit,
            captured,
        });
    }
    if let Some(error) = monitor_error {
        return Err(ManagedCommandError::Monitor { error, captured });
    }

    let status = status.expect("an exited managed command has an exit status");
    let mut capture_errors = Vec::new();
    if let Some(error) = &stdout_result.error {
        capture_errors.push(format!("stdout: {error}"));
    }
    if let Some(error) = &stderr_result.error {
        capture_errors.push(format!("stderr: {error}"));
    }
    if !capture_errors.is_empty() || !captured.stdout.complete || !captured.stderr.complete {
        if !captured.stdout.complete && stdout_result.error.is_none() {
            capture_errors.push("stdout: pipe remained open after process-tree cleanup".into());
        }
        if !captured.stderr.complete && stderr_result.error.is_none() {
            capture_errors.push("stderr: pipe remained open after process-tree cleanup".into());
        }
        return Err(ManagedCommandError::CaptureIncomplete {
            status,
            detail: capture_errors.join("; "),
            captured,
        });
    }
    if captured.stdout.truncated || captured.stderr.truncated {
        return Err(ManagedCommandError::OutputLimitExceeded {
            status: Some(status),
            limit: limits.capture_limit,
            captured,
        });
    }

    Ok(ManagedOutput {
        status,
        stdout: captured.stdout.bytes,
        stderr: captured.stderr.bytes,
    })
}

#[derive(Debug, Default)]
struct CaptureBuffer {
    bytes: Vec<u8>,
    truncated: bool,
}

struct Reader {
    state: Arc<Mutex<CaptureBuffer>>,
    completion: mpsc::Receiver<io::Result<()>>,
}

struct ReaderResult {
    stream: CapturedStream,
    error: Option<String>,
}

impl Reader {
    fn finish(self, deadline: Instant) -> ReaderResult {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let completion = self.completion.recv_timeout(remaining);
        let (complete, error) = match completion {
            Ok(Ok(())) => (true, None),
            Ok(Err(error)) => (false, Some(error.to_string())),
            Err(mpsc::RecvTimeoutError::Timeout) => (false, None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                (false, Some("capture reader terminated unexpectedly".into()))
            }
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ReaderResult {
            stream: CapturedStream {
                bytes: std::mem::take(&mut state.bytes),
                truncated: state.truncated,
                complete,
            },
            error,
        }
    }
}

fn start_reader<R>(mut pipe: R, limit: usize, output_limit_exceeded: Arc<AtomicBool>) -> Reader
where
    R: Read + Send + 'static,
{
    let state = Arc::new(Mutex::new(CaptureBuffer {
        bytes: Vec::with_capacity(limit.min(64 * 1024)),
        truncated: false,
    }));
    let reader_state = Arc::clone(&state);
    let (sender, completion) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut chunk = [0u8; 64 * 1024];
        let result = loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(read) => {
                    let mut captured = reader_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let remaining = limit.saturating_sub(captured.bytes.len());
                    let retained = read.min(remaining);
                    captured.bytes.extend_from_slice(&chunk[..retained]);
                    if retained != read {
                        captured.truncated = true;
                        output_limit_exceeded.store(true, Ordering::Release);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(result);
    });
    Reader { state, completion }
}

fn terminate_and_reap(
    child: &mut std::process::Child,
    process_group: u32,
    grace: Duration,
) -> io::Result<ExitStatus> {
    let mut lifecycle_error = None;
    #[cfg(unix)]
    if let Err(error) = signal_process_group(process_group, SIGTERM) {
        lifecycle_error = Some(error);
        let _ = child.kill();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }

    let deadline = Instant::now() + grace;
    let mut status = None;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(exited)) => {
                status = Some(exited);
                break;
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(error) => {
                if lifecycle_error.is_none() {
                    lifecycle_error = Some(error);
                }
                break;
            }
        }
    }

    #[cfg(unix)]
    if let Err(error) = signal_process_group(process_group, SIGKILL) {
        if lifecycle_error.is_none() {
            lifecycle_error = Some(error);
        }
    }
    if status.is_none() {
        let _ = child.kill();
        status = Some(child.wait()?);
    }
    if let Some(error) = lifecycle_error {
        return Err(error);
    }
    status.ok_or_else(|| io::Error::other("managed command exited without a wait status"))
}

fn kill_lingering_group(process_group: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        signal_process_group(process_group, SIGKILL)
    }
    #[cfg(not(unix))]
    {
        let _ = process_group;
        Ok(())
    }
}

#[cfg(unix)]
const SIGKILL: i32 = 9;
#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(unix)]
const ESRCH: i32 = 3;

#[cfg(unix)]
fn signal_process_group(process_group: u32, signal: i32) -> io::Result<()> {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    let process_group = i32::try_from(process_group)
        .map_err(|_| io::Error::other("managed process-group id exceeds i32"))?;
    // Negative pid addresses the process group created by CommandExt::process_group.
    let result = unsafe { kill(-process_group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short_limits(capture_limit: usize) -> CommandLimits {
        CommandLimits {
            timeout: Duration::from_millis(150),
            kill_grace: Duration::from_millis(50),
            capture_limit,
        }
    }

    #[cfg(unix)]
    #[test]
    fn timeout_preserves_partial_output_and_kills_descendants() {
        let root = std::env::temp_dir().join(format!(
            "managed_process_timeout_{}_{}",
            std::process::id(),
            thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let pid_file = root.join("descendant.pid");
        let script = format!(
            "printf 'started\\n'; sleep 300 & printf '%s\\n' \"$!\" > '{}'; wait",
            pid_file.display()
        );

        let started = Instant::now();
        let error = run_with_limits(
            Command::new("/bin/sh").args(["-c", &script]),
            short_limits(1024),
            None,
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        let ManagedCommandError::TimedOut { captured, .. } = error else {
            panic!("expected timeout, got {error}");
        };
        assert_eq!(captured.stdout.bytes, b"started\n");
        assert!(captured.stdout.complete);
        assert!(captured.stderr.complete);

        let descendant = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert!(
            !process_exists(descendant),
            "descendant {descendant} survived process-group timeout cleanup"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn background_descendant_cannot_hold_capture_open() {
        let started = Instant::now();
        let output = run_with_limits(
            Command::new("/bin/sh").args(["-c", "sleep 300 & printf 'done\\n'"]),
            short_limits(1024),
            None,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(output.status.success());
        assert_eq!(output.stdout, b"done\n");
    }

    #[cfg(unix)]
    #[test]
    fn output_limit_is_an_explicit_hard_failure() {
        let limits = CommandLimits {
            timeout: Duration::from_secs(2),
            ..short_limits(8)
        };
        let started = Instant::now();
        let error = run_with_limits(
            Command::new("/bin/sh").args(["-c", "while :; do printf '0123456789abcdef'; done"]),
            limits,
            None,
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "output-limit cancellation waited for the command deadline"
        );
        let ManagedCommandError::OutputLimitExceeded {
            limit, captured, ..
        } = error
        else {
            panic!("expected output-limit failure, got {error}");
        };
        assert_eq!(limit, 8);
        assert_eq!(captured.stdout.bytes, b"01234567");
        assert!(captured.stdout.truncated);
        assert!(captured.stdout.complete);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_the_managed_process_group() {
        let cancellation = CancellationToken::default();
        let trigger = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            trigger.cancel();
        });
        let limits = CommandLimits {
            timeout: Duration::from_secs(2),
            ..short_limits(1024)
        };
        let error = run_with_limits(
            Command::new("/bin/sh").args(["-c", "printf 'ready\\n'; sleep 300 & wait"]),
            limits,
            Some(&cancellation),
        )
        .unwrap_err();
        canceller.join().unwrap();
        let ManagedCommandError::Cancelled { captured } = error else {
            panic!("expected cancellation, got {error}");
        };
        assert_eq!(captured.stdout.bytes, b"ready\n");
        assert!(captured.stdout.complete);
        assert!(captured.stderr.complete);
    }

    #[cfg(unix)]
    fn process_exists(pid: i32) -> bool {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let result = unsafe { kill(pid, 0) };
        if result == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() != Some(ESRCH)
    }
}
