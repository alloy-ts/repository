use std::borrow::Cow;
use std::fmt;
use std::process::{ExitStatus, Output};

/// How a child process ended. The exact status is kept even when no exit code exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// The process exited normally with this code.
    Exited {
        /// Exit code returned by the child.
        code: i32,
    },
    /// A Unix signal terminated the process.
    Signaled {
        /// Unix signal number.
        signal: i32,
        /// Whether the process produced a core dump.
        core_dumped: bool,
    },
    /// The platform did not report an exit code or signal.
    Unknown,
}

impl From<ExitStatus> for ProcessStatus {
    fn from(status: ExitStatus) -> Self {
        if let Some(code) = status.code() {
            return Self::Exited { code };
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Signaled {
                    signal,
                    core_dumped: status.core_dumped(),
                };
            }
        }
        Self::Unknown
    }
}

impl ProcessStatus {
    /// Classify an operating system exit status.
    #[must_use]
    pub fn from_exit_status(status: ExitStatus) -> Self {
        Self::from(status)
    }
}

/// Raw streams and termination details from a git command.
#[derive(Clone)]
pub struct CommandOutput {
    /// Captured stdout, preserved byte for byte.
    pub stdout: Vec<u8>,
    /// Captured stderr, preserved byte for byte.
    pub stderr: Vec<u8>,
    /// Exact process termination status.
    pub status: ProcessStatus,
    /// Exit code for compatibility; `-1` when no code is available.
    pub exit_code: i32,
    /// Whether the process exited successfully.
    pub success: bool,
}

impl CommandOutput {
    /// Preserve both streams and the exact status of a completed process.
    #[must_use]
    pub fn from_process_output(output: Output) -> Self {
        let status = ProcessStatus::from(output.status);
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();
        Self {
            stdout: output.stdout,
            stderr: output.stderr,
            status,
            exit_code,
            success,
        }
    }

    /// Read stdout without decoding.
    #[must_use]
    pub fn stdout_bytes(&self) -> &[u8] {
        &self.stdout
    }

    /// Read stderr without decoding.
    #[must_use]
    pub fn stderr_bytes(&self) -> &[u8] {
        &self.stderr
    }

    /// Decode stdout lossily, replacing invalid UTF-8 with U+FFFD.
    #[must_use]
    pub fn stdout_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    /// Decode stderr lossily, replacing invalid UTF-8 with U+FFFD.
    #[must_use]
    pub fn stderr_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }

    /// Split lossy stdout text into owned lines.
    #[must_use]
    pub fn stdout_lines(&self) -> Vec<String> {
        self.stdout_str().lines().map(ToOwned::to_owned).collect()
    }

    /// Split lossy stderr text into owned lines.
    #[must_use]
    pub fn stderr_lines(&self) -> Vec<String> {
        self.stderr_str().lines().map(ToOwned::to_owned).collect()
    }

    /// Decode stdout lossily and remove trailing whitespace.
    #[must_use]
    pub fn stdout_trimmed(&self) -> String {
        self.stdout_str().trim_end().to_owned()
    }
}

impl fmt::Debug for CommandOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandOutput")
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .field("status", &self.status)
            .field("exit_code", &self.exit_code)
            .field("success", &self.success)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_binary_streams_and_hides_them_from_debug() {
        let output = std::process::Command::new("git")
            .arg("--version")
            .output()
            .expect("git is available for crate tests");
        let status = ProcessStatus::from(output.status);
        let result = CommandOutput::from_process_output(output);
        assert_eq!(result.status, status);
        assert_eq!(result.exit_code, 0);
        assert!(result.success);

        let binary = CommandOutput {
            stdout: b"secret-out\xff".to_vec(),
            stderr: b"secret-err\xfe".to_vec(),
            status: ProcessStatus::Exited { code: 1 },
            exit_code: 1,
            success: false,
        };
        assert_eq!(binary.stdout_bytes(), b"secret-out\xff");
        assert_eq!(binary.stderr_bytes(), b"secret-err\xfe");
        assert!(binary.stdout_str().contains('\u{fffd}'));
        assert!(binary.stderr_str().contains('\u{fffd}'));
        assert_eq!(binary.stderr_lines(), vec!["secret-err\u{fffd}".to_owned()]);
        let debug = format!("{binary:?}");
        assert!(!debug.contains("secret-out"));
        assert!(!debug.contains("secret-err"));
    }

    #[cfg(unix)]
    #[test]
    fn identifies_signal_termination() {
        use std::os::unix::process::ExitStatusExt;
        let status = ExitStatus::from_raw(9);
        assert_eq!(
            ProcessStatus::from(status),
            ProcessStatus::Signaled {
                signal: 9,
                core_dumped: false,
            }
        );
    }
}
