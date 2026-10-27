use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex};

use napi::bindgen_prelude::{Buffer, JsError, ToNapiValue};
use napi::sys;
use napi::{Error as NapiError, Status};
use napi_derive::napi;

use crate::output::{CommandOutput, ProcessStatus};
use crate::process::{ExecutionFailure, ProcessStatusJs};

/// Coarse, stable grouping for an error. Mirrors [`Error::category`].
///
/// Exposed to JavaScript as a string-valued enum so callers can branch on the
/// source of a failure (prerequisites, command, parsing, …) without parsing the
/// message.
#[derive(Clone, Copy)]
#[napi(string_enum)]
pub enum ErrorCategory {
    /// Git binary missing or too old.
    Prerequisites,
    /// A spawned git command failed, was cancelled, or timed out.
    Command,
    /// Output could not be parsed into a typed value.
    Parsing,
    /// Invalid configuration was supplied to a builder.
    Config,
    /// A path was not a git repository, or a repository operation failed.
    Repository,
    /// An I/O error occurred while running git.
    Io,
    /// Any other error.
    Custom,
}

/// Result type for git-spawn operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Result type returned by every entry point.
///
/// Mirrors `napi::Result<T>`: napi 3.x only accepts `Result<T, napi::Error>` as a
/// `#[napi]` return, so the error payload is always `napi::Error` (carrying
/// `message` + `status`). The structured [`RepositoryError`] fields are still
/// built internally by `err`/`err_repo` and converted into the `napi::Error`
/// via `From<RepositoryError> for napi::Error`.
pub type RepositoryResult<T> = napi::Result<T>;

/// Structured, machine-readable view of a thrown error.
///
/// The value that actually crosses the FFI boundary is a native JS `Error`
/// (so `error instanceof Error` holds and the historical `message` contract is
/// preserved); this type's **non-secret** fields are attached as own properties:
///
/// - `category`: one of [`ErrorCategory`].
/// - `kind`: which internal arm (or `"Gix"`/`"Napi"`) produced the error.
/// - `exitCode`: command exit code, when applicable (never the command line).
/// - `status`: the JS-facing [`ProcessStatusJs`], when applicable.
/// - `suggestion`: a safe, human-readable recovery hint, when available.
/// - `message`: the historical `"{context}: {diagnostic}"` shape, with secrets
///   redacted by the [`Error`] `Display`/`Debug` impls.
///
/// Secret-bearing data (`command` line, captured `stdout`/`stderr`, and the gix
/// error tree) is kept on this struct **only for internal/test use** and is
/// deliberately never serialized to JS.
pub struct RepositoryError {
    /// napi status used for the underlying JS `Error`.
    pub status: Status,
    /// Historical `"{context}: {diagnostic}"` message.
    pub message: String,
    /// Coarse grouping.
    pub category: ErrorCategory,
    /// Which internal arm (or `"Gix"`/`"Napi"`) produced the error.
    pub kind: String,
    /// git subsystem classification from gix, if applicable (internal only).
    pub class: Option<String>,
    /// Captured details for command failures (internal only; never serialized).
    pub command: Option<CommandFailure>,
    /// The gix error tree (internal only; never serialized to avoid leaking
    /// repository paths, remote URLs, or verbose diagnostics).
    pub cause: Option<Box<NapiError>>,
    /// Safe, human-readable recovery hint. Never contains paths, URLs, or
    /// command lines.
    pub suggestion: Option<String>,
}

/// Captured details about a failed (or interrupted) git command.
#[napi(object, js_name = "CommandFailure")]
pub struct CommandFailure {
    /// The command line that was run.
    pub command: String,
    /// Exit code; `-1` when no code is available.
    pub exit_code: i32,
    /// Exact process termination status.
    pub status: ProcessStatusJs,
    /// Captured stdout bytes.
    pub stdout: Buffer,
    /// Captured stderr bytes.
    pub stderr: Buffer,
}

fn status_js(status: Option<ProcessStatus>) -> ProcessStatusJs {
    match status {
        Some(status) => status.into(),
        None => ProcessStatusJs {
            kind: "unknown".to_string(),
            code: None,
            signal: None,
            core_dumped: None,
        },
    }
}

fn command_failure(command: String, output: &CommandOutput) -> CommandFailure {
    CommandFailure {
        command,
        exit_code: output.exit_code,
        status: status_js(Some(output.status)),
        stdout: Buffer::from(output.stdout.clone()),
        stderr: Buffer::from(output.stderr.clone()),
    }
}

impl RepositoryError {
    fn from_message(e: NapiError) -> Self {
        let message = e.reason.clone();
        let cause = e.cause;
        Self {
            status: e.status,
            message,
            category: ErrorCategory::Custom,
            kind: "Custom".to_string(),
            class: None,
            command: None,
            cause,
            suggestion: None,
        }
    }

    /// Build the structured error for the internal [`Error`] enum.
    pub fn from_error(err: &Error) -> Self {
        let message = err.to_string();
        let (category, kind, command) = match err {
            Error::GitNotFound => (ErrorCategory::Prerequisites, "GitNotFound", None),
            Error::UnsupportedVersion { .. } => {
                (ErrorCategory::Prerequisites, "UnsupportedVersion", None)
            }
            Error::CommandFailed {
                command,
                exit_code,
                status,
                stdout,
                stderr,
            } => (
                ErrorCategory::Command,
                "CommandFailed",
                Some(CommandFailure {
                    command: command.clone(),
                    exit_code: *exit_code,
                    status: status_js(Some(*status)),
                    stdout: Buffer::from(stdout.clone()),
                    stderr: Buffer::from(stderr.clone()),
                }),
            ),
            Error::Execution { failure } => (
                ErrorCategory::Command,
                "Execution",
                Some(CommandFailure {
                    command: String::new(),
                    exit_code: failure
                        .status
                        .map(|s| match s {
                            ProcessStatus::Exited { code } => code,
                            ProcessStatus::Signaled { .. } => -1,
                            ProcessStatus::Unknown => -1,
                        })
                        .unwrap_or(-1),
                    status: status_js(failure.status),
                    stdout: Buffer::from(failure.stdout.clone()),
                    stderr: Buffer::from(failure.stderr.clone()),
                }),
            ),
            Error::ParseError { .. } => (ErrorCategory::Parsing, "ParseError", None),
            Error::InvalidConfig { .. } => (ErrorCategory::Config, "InvalidConfig", None),
            Error::NotARepository { .. } => (ErrorCategory::Repository, "NotARepository", None),
            Error::Io { .. } => (ErrorCategory::Io, "Io", None),
            Error::Timeout { .. } => (ErrorCategory::Command, "Timeout", None),
            Error::Custom { .. } => (ErrorCategory::Custom, "Custom", None),
        };
        Self {
            status: Status::GenericFailure,
            message,
            category,
            kind: kind.to_string(),
            class: None,
            command,
            cause: None,
            suggestion: suggestion_for(category),
        }
    }
}

/// Attach a non-secret, named property to a raw JS error object.
///
/// napi 3.x does not expose property setters on `JsError` or `Unknown`, so the
/// underlying `sys` binding is called directly.
unsafe fn set_err_prop<T: ToNapiValue>(
    env: sys::napi_env,
    obj: sys::napi_value,
    name: &str,
    value: T,
) -> napi::Result<()> {
    let key = std::ffi::CString::new(name)
        .map_err(|_| NapiError::new(Status::GenericFailure, "invalid error property name"))?;
    let prop = unsafe { T::to_napi_value(env, value)? };
    napi::check_status!(unsafe { sys::napi_set_named_property(env, obj, key.as_ptr(), prop) })?;
    Ok(())
}

impl ToNapiValue for RepositoryError {
    unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
        let js_err = JsError::from(NapiError::new(val.status, val.message));
        let value = unsafe { js_err.into_value(env) };
        if value.is_null() {
            return Err(NapiError::new(
                Status::GenericFailure,
                "failed to build error object",
            ));
        }
        // Only non-secret structured fields are attached; `command`
        // (with its stdout/stderr), `class`, and `cause` are intentionally
        // never serialized to JS.
        unsafe {
            set_err_prop(env, value, "category", val.category)?;
            set_err_prop(env, value, "kind", val.kind)?;
            if let Some(cmd) = val.command {
                set_err_prop(env, value, "exitCode", cmd.exit_code)?;
                set_err_prop(env, value, "status", cmd.status)?;
            }
            if let Some(suggestion) = val.suggestion {
                set_err_prop(env, value, "suggestion", suggestion)?;
            }
        }
        Ok(value)
    }
}

impl From<RepositoryError> for NapiError {
    fn from(e: RepositoryError) -> Self {
        // The internal `cause` (gix error tree) is intentionally dropped here so
        // it never reaches the JS `Error.cause` and leak repository paths/URLs.
        let mut err = NapiError::new(e.status, e.message);
        err.cause = None;
        err
    }
}

impl From<NapiError> for RepositoryError {
    fn from(e: NapiError) -> Self {
        let message = e.reason.clone();
        Self {
            status: e.status,
            message,
            category: ErrorCategory::Custom,
            kind: "Napi".to_string(),
            class: None,
            command: None,
            cause: e.cause,
            suggestion: None,
        }
    }
}

impl From<Error> for RepositoryError {
    fn from(e: Error) -> Self {
        RepositoryError::from_error(&e)
    }
}

impl From<std::io::Error> for RepositoryError {
    fn from(e: std::io::Error) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

impl From<gix::Error> for RepositoryError {
    fn from(e: gix::Error) -> Self {
        err_repo("gix", e)
    }
}

impl From<std::str::Utf8Error> for RepositoryError {
    fn from(e: std::str::Utf8Error) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

impl From<std::string::FromUtf8Error> for RepositoryError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

impl From<serde_json::Error> for RepositoryError {
    fn from(e: serde_json::Error) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

impl From<crate::crypto::CryptoError> for RepositoryError {
    fn from(e: crate::crypto::CryptoError) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

impl From<crate::vault::SafeError> for RepositoryError {
    fn from(e: crate::vault::SafeError) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

#[cfg(feature = "gpg")]
impl From<crate::pgp::PgpError> for RepositoryError {
    fn from(e: crate::pgp::PgpError) -> Self {
        Self::from_message(NapiError::from_reason(e.to_string()))
    }
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl fmt::Debug for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RepositoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|b| b as &(dyn std::error::Error + 'static))
    }
}

/// Build the internal [`RepositoryError`] from a domain label and a native error.
///
/// Kept separate from [`err`] so internal helpers that return
/// `napi::Result<T>` can produce a [`RepositoryError`] directly.
fn err_repo(what: &str, e: impl std::error::Error + 'static) -> RepositoryError {
    let mut out = RepositoryError::from_message(NapiError::from_reason(format!("{what}: {e}")));
    out.kind = "Gix".to_string();
    out.class = gix_class(&e);
    out.category = out
        .class
        .as_deref()
        .map(category_for_class)
        .unwrap_or(ErrorCategory::Custom);
    out.cause = None;
    out.suggestion = suggestion_for(out.category);
    out
}

/// Build a [`RepositoryError`] from a domain label and a native error.
///
/// The message keeps the historical `"{what}: {e}"` shape, so existing
/// consumers are unaffected. The gix error tree is additionally attached as a
/// standard JS `error.cause` chain: one `Error` per cause, innermost last,
/// skipping the root diagnostic (already in the message). Causes carrying a gix
/// [`gix::error::Class`]ification get a machine-readable `[git: <Class>]` suffix
/// appended to their message.
pub fn err(what: &str, e: impl std::error::Error + 'static) -> NapiError {
    err_repo(what, e).into()
}

/// Build the internal [`RepositoryError`] from a plain message with no underlying
/// native error.
fn err_str_repo(what: &str, msg: impl std::fmt::Display) -> RepositoryError {
    RepositoryError::from_message(NapiError::from_reason(format!("{what}: {msg}")))
}

/// Build a [`napi::Error`] from a plain message with no underlying native error.
pub fn err_str(what: &str, msg: impl std::fmt::Display) -> NapiError {
    err_str_repo(what, msg).into()
}

/// Extract the leading git subsystem classification (e.g. `"NotFound"`) for a
/// native error, if gix can classify it.
fn gix_class(e: &(dyn std::error::Error + 'static)) -> Option<String> {
    gix::error::classify(e)
        .next()
        .map(|err| format!("{:?}", err.class()))
}

/// Map a gix classification to a coarse [`ErrorCategory`].
fn category_for_class(class: &str) -> ErrorCategory {
    match class {
        "NotFound" | "ObjectNotFound" | "Repository" | "Reference" | "ReferenceMissing"
        | "ReferenceExists" | "Object" | "ObjectExists" | "Tree" | "Commit" | "Tag" | "Blob"
        | "Index" | "Odb" | "Pack" | "Revision" | "Worktree" | "Submodule" | "Mailmap"
        | "Ignore" => ErrorCategory::Repository,
        "Net" | "Ssh" | "Http" | "Ssl" | "Certificate" | "Authentication" => ErrorCategory::Command,
        "Config" | "Configuration" => ErrorCategory::Config,
        _ => ErrorCategory::Custom,
    }
}

/// Produce a safe, human-readable recovery hint for an error category.
///
/// Intentionally contains no repository paths, remote URLs, command lines, or
/// captured output, so it is safe to surface directly to JS consumers.
fn suggestion_for(category: ErrorCategory) -> Option<String> {
    match category {
        ErrorCategory::Prerequisites => Some(
            "Install Git and ensure it is on PATH, or upgrade to a supported version.".to_string(),
        ),
        ErrorCategory::Repository => {
            Some("Verify the target is a Git repository and that references exist.".to_string())
        }
        ErrorCategory::Config => Some("Check the supplied Git configuration.".to_string()),
        ErrorCategory::Parsing => Some(
            "The command output could not be parsed; review the operation parameters.".to_string(),
        ),
        ErrorCategory::Command => Some(
            "The git command failed or was interrupted; check the exit code and timeout."
                .to_string(),
        ),
        ErrorCategory::Io => Some("An I/O error occurred while running git.".to_string()),
        ErrorCategory::Custom => None,
    }
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

static NAMESPACE_CACHE: LazyLock<Mutex<HashMap<usize, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MMAP_THRESHOLD_CACHE: LazyLock<Mutex<HashMap<usize, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read the cached reference namespace for the repository identified by `key`.
pub fn cached_namespace_get(key: usize) -> Option<String> {
    let cache = NAMESPACE_CACHE.lock().ok()?;
    cache.get(&key).cloned().flatten()
}

/// Cache the reference namespace (or `None` to clear it) for the repository identified by `key`.
pub fn cached_namespace_set(key: usize, value: Option<String>) {
    if let Ok(mut cache) = NAMESPACE_CACHE.lock() {
        cache.insert(key, value);
    }
}

/// Read the cached packed-refs mmap threshold for the repository identified by `key`.
pub fn cached_mmap_threshold_get(key: usize) -> Option<u64> {
    let cache = MMAP_THRESHOLD_CACHE.lock().ok()?;
    cache.get(&key).copied()
}

/// Cache the packed-refs mmap threshold for the repository identified by `key`.
pub fn cached_mmap_threshold_set(key: usize, value: u64) {
    if let Ok(mut cache) = MMAP_THRESHOLD_CACHE.lock() {
        cache.insert(key, value);
    }
}

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
        failure: Box<ExecutionFailure>,
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

    /// A coarse category, as the JS-facing [`ErrorCategory`].
    #[must_use]
    pub fn error_category(&self) -> ErrorCategory {
        match self {
            Self::GitNotFound | Self::UnsupportedVersion { .. } => ErrorCategory::Prerequisites,
            Self::CommandFailed { .. } | Self::Execution { .. } | Self::Timeout { .. } => {
                ErrorCategory::Command
            }
            Self::ParseError { .. } => ErrorCategory::Parsing,
            Self::InvalidConfig { .. } => ErrorCategory::Config,
            Self::NotARepository { .. } => ErrorCategory::Repository,
            Self::Io { .. } => ErrorCategory::Io,
            Self::Custom { .. } => ErrorCategory::Custom,
        }
    }

    /// The machine-readable arm name that produced this error.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::GitNotFound => "GitNotFound",
            Self::UnsupportedVersion { .. } => "UnsupportedVersion",
            Self::CommandFailed { .. } => "CommandFailed",
            Self::Execution { .. } => "Execution",
            Self::ParseError { .. } => "ParseError",
            Self::InvalidConfig { .. } => "InvalidConfig",
            Self::NotARepository { .. } => "NotARepository",
            Self::Io { .. } => "Io",
            Self::Timeout { .. } => "Timeout",
            Self::Custom { .. } => "Custom",
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

/// Map a coarse [`ErrorCategory`] to an napi status code.
fn status_for_category(cat: ErrorCategory) -> Status {
    match cat {
        ErrorCategory::Config => Status::InvalidArg,
        _ => Status::GenericFailure,
    }
}

impl From<Error> for NapiError {
    fn from(e: Error) -> Self {
        let re: RepositoryError = e.into();
        NapiError::new(status_for_category(re.category), re.message)
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
    fn structured_error_preserves_history() {
        let error = RepositoryError::from_error(&Error::command_failed("git status", 1, "", ""));
        assert_eq!(error.kind, "CommandFailed");
        assert_eq!(error.category, ErrorCategory::Command);
        assert!(error.command.is_some());
    }
}
