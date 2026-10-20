use std::fmt;
use std::sync::Arc;

use napi::Error as NapiError;

use gix::Error as GixError;

use crate::output::{CommandOutput, ProcessStatus};

/// Upper bound on how many causes are attached to a thrown error, so pathological
/// source chains (or cycles) cannot blow up the FFI boundary.
const MAX_CAUSES: usize = 16;

/// Build a [`napi::Error`] from a domain label and a native error.
///
/// The message keeps the historical `"{what}: {e}"` shape, so existing consumers are unaffected.
/// The gix error tree is additionally attached as a standard JS `error.cause` chain: one
/// `Error` per cause, innermost last, skipping the root diagnostic (already in the message).
/// Causes carrying a gix [`gix::error::Class`]ification get a machine-readable
/// `[git: <Class>]` suffix (e.g. `[git: NotFound]`) appended to their message.
pub fn err(what: &str, e: impl std::error::Error + 'static) -> NapiError {
    let mut out = NapiError::from_reason(format!("{what}: {e}"));
    out.cause = build_cause(&e);
    out
}

/// Build a [`napi::Error`] from a plain message with no underlying native error.
pub fn err_str(what: &str, msg: impl std::fmt::Display) -> NapiError {
    NapiError::from_reason(format!("{what}: {msg}"))
}

/// Walk `error`'s tree, collecting concrete error references in traversal order.
///
/// A nested [`GixError`] boundary owns a full tree, visited in logical order by
/// [`GixError::iter_errors()`] (including native sources); plain errors are followed through
/// their standard `source()` chain.
fn collect_chain<'a>(
    error: &'a (dyn std::error::Error + 'static),
    out: &mut Vec<&'a (dyn std::error::Error + 'static)>,
    depth: usize,
) {
    if depth > MAX_CAUSES {
        return;
    }
    if let Some(tree) = error.downcast_ref::<GixError>() {
        out.extend(tree.iter_errors());
        return;
    }
    out.push(error);
    if let Some(source) = error.source() {
        collect_chain(source, out, depth + 1);
    }
}

/// Convert the error tree into a nested chain of [`NapiError`]s for JS `error.cause`.
fn build_cause(error: &(dyn std::error::Error + 'static)) -> Option<Box<NapiError>> {
    let mut refs = Vec::new();
    collect_chain(error, &mut refs, 0);

    // Every entry gets a cause, including the root (whose diagnostic is also
    // in the message): single-node trees must still expose their classification.
    let mut messages: Vec<String> = Vec::with_capacity(refs.len());
    for entry in refs.iter().copied() {
        let mut message = entry.to_string();
        if message.is_empty() {
            continue;
        }
        if let Some(classification) = gix::error::classify(entry).next() {
            message.push_str(&format!(" [git: {:?}]", classification.class()));
        }
        if messages.last().is_some_and(|last| last == &message) {
            continue;
        }
        messages.push(message);
        if messages.len() >= MAX_CAUSES {
            break;
        }
    }

    let mut acc: Option<Box<NapiError>> = None;
    for message in messages.iter().rev() {
        let mut cause = Box::new(NapiError::from_reason(message.clone()));
        cause.cause = acc;
        acc = Some(cause);
    }
    acc
}

/// Parse a hex object id string into a [`gix::ObjectId`].
pub fn parse_oid(s: &str) -> napi::Result<gix::ObjectId> {
    gix::ObjectId::from_hex(s.as_bytes()).map_err(|e| err("parse_oid", e))
}

/// Obtain a cheap thread-local view of the repository for synchronous methods.
///
/// Re-applies the cached reference namespace and packed-refs mmap threshold, since
/// those live on the thread-local view and would otherwise be reset on every call.
pub fn thread_local(repo: &Arc<gix::ThreadSafeRepository>) -> gix::Repository {
    let mut r = repo.to_thread_local();
    let key = Arc::as_ptr(repo) as usize;
    if let Some(ns) = cached_namespace_get(key) {
        // The cached namespace was validated when set.
        let _ = r.set_namespace(&ns);
    }
    if let Some(bytes) = cached_mmap_threshold_get(key) {
        r.refs.set_packed_buffer_mmap_threshold(bytes);
    }
    r
}

thread_local! {
    /// Per-repository reference namespace cache. Each synchronous call gets a fresh
    /// thread-local view whose in-memory namespace would not otherwise persist across
    /// calls, so we mirror the last value here (keyed by the repo allocation).
    static NAMESPACE_CACHE: std::cell::RefCell<std::collections::HashMap<usize, Option<String>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Per-repository packed-refs mmap threshold, mirrored for the same reason.
    static MMAP_THRESHOLD_CACHE: std::cell::RefCell<std::collections::HashMap<usize, u64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Read the cached reference namespace for the repository identified by `key`.
pub fn cached_namespace_get(key: usize) -> Option<String> {
    NAMESPACE_CACHE.with(|m| m.borrow().get(&key).cloned().flatten())
}

/// Cache the reference namespace (or `None` to clear it) for the repository identified by `key`.
pub fn cached_namespace_set(key: usize, value: Option<String>) {
    NAMESPACE_CACHE.with(|m| {
        m.borrow_mut().insert(key, value);
    });
}

/// Read the cached packed-refs mmap threshold for the repository identified by `key`.
pub fn cached_mmap_threshold_get(key: usize) -> Option<u64> {
    MMAP_THRESHOLD_CACHE.with(|m| m.borrow().get(&key).copied())
}

/// Cache the packed-refs mmap threshold for the repository identified by `key`.
pub fn cached_mmap_threshold_set(key: usize, value: u64) {
    MMAP_THRESHOLD_CACHE.with(|m| {
        m.borrow_mut().insert(key, value);
    });
}

/// Result type for git-spawn operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for all git-spawn operations.
pub enum Error {
    /// Git binary not found in PATH.
    GitNotFound,
    /// Git version is below the minimum supported.
    UnsupportedVersion {
        /// Version reported by `git --version`.
        found: String,
        /// Minimum required version.
        minimum: String,
    },
    /// A git command exited unsuccessfully.
    CommandFailed {
        /// Full command line, available for explicit inspection only.
        command: String,
        /// Legacy exit code; `-1` if the child did not exit normally.
        exit_code: i32,
        /// Exact process termination status.
        status: ProcessStatus,
        /// Captured stdout bytes.
        stdout: Vec<u8>,
        /// Captured stderr bytes.
        stderr: Vec<u8>,
    },
    /// Execution was cancelled, timed out, or exceeded a capture limit.
    Execution {
        /// Partial output and cleanup details from the failed execution.
        failure: Box<crate::process::ExecutionFailure>,
    },
    /// Failed to parse git output into a typed value.
    ParseError {
        /// Description of the parse failure.
        message: String,
    },
    /// Invalid configuration supplied to a builder.
    InvalidConfig {
        /// Description of the misconfiguration.
        message: String,
    },
    /// Operation targeted a path that is not a git repository.
    NotARepository {
        /// Path that was expected to be a repo.
        path: String,
    },
    /// IO error while spawning or reading from a git process.
    Io {
        /// Human-readable message.
        message: String,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Legacy timeout error.
    Timeout {
        /// Configured timeout in seconds.
        timeout_seconds: u64,
    },
    /// Generic error with a custom message.
    Custom {
        /// Custom error message.
        message: String,
    },
}

impl Error {
    /// Create a synthetic command failure from an exit code and byte streams.
    /// For actual child processes, use [`Self::from_command_output`].
    pub fn command_failed(
        command: impl Into<String>,
        exit_code: i32,
        stdout: impl AsRef<[u8]>,
        stderr: impl AsRef<[u8]>,
    ) -> Self {
        Self::CommandFailed {
            command: command.into(),
            exit_code,
            status: ProcessStatus::Exited { code: exit_code },
            stdout: stdout.as_ref().to_vec(),
            stderr: stderr.as_ref().to_vec(),
        }
    }

    /// Preserve the complete output and termination status of a failed command.
    pub fn from_command_output(command: impl Into<String>, output: CommandOutput) -> Self {
        Self::CommandFailed {
            command: command.into(),
            exit_code: output.exit_code,
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }

    /// Create a parse error.
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::ParseError {
            message: message.into(),
        }
    }

    /// Create an invalid configuration error.
    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self::InvalidConfig {
            message: message.into(),
        }
    }

    /// Create a not-a-repository error.
    pub fn not_a_repository(path: impl Into<String>) -> Self {
        Self::NotARepository { path: path.into() }
    }

    /// Create a legacy timeout error.
    #[must_use]
    pub fn timeout(timeout_seconds: u64) -> Self {
        Self::Timeout { timeout_seconds }
    }

    /// Create a custom error.
    pub fn custom(message: impl Into<String>) -> Self {
        Self::Custom {
            message: message.into(),
        }
    }

    /// A coarse category useful for logging and metrics.
    #[must_use]
    pub fn category(&self) -> &'static str {
        match self {
            Self::GitNotFound | Self::UnsupportedVersion { .. } => "prerequisites",
            Self::CommandFailed { .. } | Self::Execution { .. } | Self::Timeout { .. } => "command",
            Self::ParseError { .. } => "parsing",
            Self::InvalidConfig { .. } => "config",
            Self::NotARepository { .. } => "repository",
            Self::Io { .. } => "io",
            Self::Custom { .. } => "custom",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitNotFound => f.write_str("git binary not found in PATH"),
            Self::UnsupportedVersion { .. } => f.write_str("unsupported git version"),
            Self::CommandFailed { status, .. } => write!(f, "git command failed ({status:?})"),
            Self::Execution { failure } => fmt::Display::fmt(failure, f),
            Self::ParseError { .. } => f.write_str("failed to parse git output"),
            Self::InvalidConfig { .. } => f.write_str("invalid configuration"),
            Self::NotARepository { .. } => f.write_str("not a git repository"),
            Self::Io { .. } => f.write_str("io error while running git"),
            Self::Timeout { timeout_seconds } => {
                write!(f, "operation timed out after {timeout_seconds} seconds")
            }
            Self::Custom { .. } => f.write_str("git operation failed"),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitNotFound => f.write_str("GitNotFound"),
            Self::UnsupportedVersion { .. } => f.write_str("UnsupportedVersion"),
            Self::CommandFailed {
                exit_code,
                status,
                stdout,
                stderr,
                ..
            } => f
                .debug_struct("CommandFailed")
                .field("exit_code", exit_code)
                .field("status", status)
                .field("stdout_len", &stdout.len())
                .field("stderr_len", &stderr.len())
                .finish(),
            Self::Execution { failure } => f
                .debug_struct("Execution")
                .field("failure", failure)
                .finish(),
            Self::ParseError { .. } => f.write_str("ParseError"),
            Self::InvalidConfig { .. } => f.write_str("InvalidConfig"),
            Self::NotARepository { .. } => f.write_str("NotARepository"),
            Self::Io { .. } => f.write_str("Io"),
            Self::Timeout { timeout_seconds } => f
                .debug_struct("Timeout")
                .field("timeout_seconds", timeout_seconds)
                .finish(),
            Self::Custom { .. } => f.write_str("Custom"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io {
            message: err.to_string(),
            source: err,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(Error::GitNotFound.category(), "prerequisites");
        assert_eq!(
            Error::command_failed("git status", 1, "", "").category(),
            "command"
        );
        assert_eq!(Error::parse_error("x").category(), "parsing");
        assert_eq!(Error::not_a_repository("/tmp").category(), "repository");
    }

    #[test]
    fn command_failure_preserves_bytes_and_hides_payloads() {
        let output = CommandOutput {
            stdout: b"private-stdout\xff".to_vec(),
            stderr: b"private-stderr\xfe".to_vec(),
            status: ProcessStatus::Signaled {
                signal: 9,
                core_dumped: false,
            },
            exit_code: -1,
            success: false,
        };
        let error = Error::from_command_output("secret-command", output);
        let Error::CommandFailed {
            command,
            exit_code,
            status,
            stdout,
            stderr,
        } = &error
        else {
            panic!("expected command failure");
        };
        assert_eq!(command, "secret-command");
        assert_eq!(*exit_code, -1);
        assert_eq!(
            *status,
            ProcessStatus::Signaled {
                signal: 9,
                core_dumped: false,
            }
        );
        assert_eq!(stdout, b"private-stdout\xff");
        assert_eq!(stderr, b"private-stderr\xfe");
        for rendered in [format!("{error}"), format!("{error:?}")] {
            assert!(!rendered.contains("secret-command"));
            assert!(!rendered.contains("private-stdout"));
            assert!(!rendered.contains("private-stderr"));
        }
    }

    #[test]
    fn all_other_errors_hide_messages() {
        let errors = [
            Error::parse_error("secret"),
            Error::invalid_config("secret"),
            Error::not_a_repository("secret"),
            Error::custom("secret"),
            Error::UnsupportedVersion {
                found: "secret".into(),
                minimum: "secret".into(),
            },
            std::io::Error::other("secret").into(),
        ];
        for error in errors {
            assert!(!format!("{error}").contains("secret"));
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    #[test]
    fn synthetic_command_failure_preserves_binary_stderr() {
        let error = Error::command_failed("private", 2, b"out\xff", b"err\xfe");
        let Error::CommandFailed {
            status,
            stdout,
            stderr,
            ..
        } = error
        else {
            panic!("expected command failure");
        };
        assert_eq!(status, ProcessStatus::Exited { code: 2 });
        assert_eq!(stdout, b"out\xff");
        assert_eq!(stderr, b"err\xfe");
    }
}
