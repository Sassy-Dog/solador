//! Reading one macOS keychain generic password, **through `/usr/bin/security`**.
//!
//! This is how orca reads Claude Code's item:
//! `security find-generic-password -s <service> -a <account> -w`, where exit 44
//! or "could not be found" means absent
//! (`main/macos-keychain/generic-password.ts:10-22,78-104`).
//!
//! The command-line tool is used rather than linking Security.framework, and
//! that choice is about prompts:
//!
//! - **Security.framework would prompt for Solador's binary.** Its "Always
//!   Allow" would then add Solador to the item's access list, which modifies
//!   another program's credential item.
//! - **`security` probably reads without a prompt.** Claude Code is understood
//!   to write its item through `security` too, which puts that tool on the
//!   item's access list. That is not verifiable from here without reading a
//!   real keychain, which no test does.
//!
//! If macOS prompts anyway, the operator has to be able to answer. So the
//! timeout is a minute rather than orca's 3 seconds, which would kill the
//! process under an open dialog.
//!
//! Nothing here runs unless the operator turned the Claude source on. The
//! gate is in the shell; this module is only the mechanism. Tests drive
//! [`SecurityCli`] against stub scripts and never against the real tool.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::secret::SecretText;

/// The tool, by absolute path, so a `security` earlier on `PATH` cannot stand
/// in for it and be handed the item.
pub const SECURITY_PROGRAM: &str = "/usr/bin/security";

/// Long enough for a person to answer a keychain prompt.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// `errSecItemNotFound`, as `security` reports it in its exit status.
const NOT_FOUND_STATUS: i32 = 44;

/// Reads one generic password. A trait so the credential discovery above it is
/// testable with fakes, and so no test can reach a real keychain by accident.
pub trait KeychainReader {
    /// Returns `Ok(None)` when no such item exists.
    ///
    /// # Errors
    /// [`KeychainError`] when the keychain was asked and did not answer: the
    /// prompt was refused, the read timed out, or the tool would not run.
    fn generic_password(
        &self,
        service: &str,
        account: Option<&str>,
    ) -> Result<Option<SecretText>, KeychainError>;
}

/// Why the keychain did not answer. Carries no output: `security`'s stdout
/// is the secret, and its stderr is not worth the risk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeychainError {
    #[error("could not run the keychain tool: {0}")]
    Spawn(String),
    #[error("the keychain did not answer in time")]
    TimedOut,
    #[error("the keychain refused (exit {0:?})")]
    Refused(Option<i32>),
    #[error("the keychain item is empty")]
    Empty,
}

/// [`KeychainReader`] over the `security` command-line tool.
#[derive(Debug, Clone)]
pub struct SecurityCli {
    program: PathBuf,
    timeout: Duration,
}

impl Default for SecurityCli {
    fn default() -> Self {
        SecurityCli {
            program: PathBuf::from(SECURITY_PROGRAM),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl SecurityCli {
    /// A reader over another program, so tests (and only tests) can point it
    /// at a stub.
    #[must_use]
    pub fn with_program(program: &Path, timeout: Duration) -> Self {
        SecurityCli {
            program: program.to_path_buf(),
            timeout,
        }
    }
}

impl KeychainReader for SecurityCli {
    fn generic_password(
        &self,
        service: &str,
        account: Option<&str>,
    ) -> Result<Option<SecretText>, KeychainError> {
        let mut command = Command::new(&self.program);
        command.args(["find-generic-password", "-s", service]);
        if let Some(account) = account {
            command.args(["-a", account]);
        }
        command
            .arg("-w")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| KeychainError::Spawn(e.kind().to_string()))?;

        // Both pipes drained on their own threads, so a chatty child cannot
        // block on a full pipe while this thread waits for it to exit.
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let out = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let err = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out.join();
                    let _ = err.join();
                    return Err(KeychainError::TimedOut);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(KeychainError::Spawn(e.kind().to_string())),
            }
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();

        if !status.success() {
            let not_found = status.code() == Some(NOT_FOUND_STATUS)
                || String::from_utf8_lossy(&stderr)
                    .to_ascii_lowercase()
                    .contains("could not be found");
            return if not_found {
                Ok(None)
            } else {
                Err(KeychainError::Refused(status.code()))
            };
        }
        let text = String::from_utf8(stdout).map_err(|_| KeychainError::Empty)?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(KeychainError::Empty);
        }
        Ok(Some(SecretText::new(trimmed.to_owned())))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for `security`: records its arguments, then behaves as told.
    fn stub(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("security");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\n{body}\n",
            dir.join("argv").display()
        );
        std::fs::write(&path, script).expect("write stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn argv(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("argv"))
            .expect("argv")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn reader(program: &Path) -> SecurityCli {
        SecurityCli::with_program(program, Duration::from_secs(5))
    }

    #[test]
    fn the_item_is_asked_for_by_service_and_account_and_returned_trimmed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(dir.path(), "printf '{\"a\":1}\\n'; exit 0");
        let secret = reader(&program)
            .generic_password("Claude Code-credentials", Some("alice"))
            .expect("read")
            .expect("present");
        assert_eq!(secret.expose(), r#"{"a":1}"#);
        assert_eq!(
            argv(dir.path()),
            [
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-a",
                "alice",
                "-w"
            ]
        );
    }

    #[test]
    fn no_account_leaves_the_flag_off_rather_than_sending_an_empty_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(dir.path(), "echo secret");
        reader(&program)
            .generic_password("svc", None)
            .expect("read")
            .expect("present");
        assert_eq!(
            argv(dir.path()),
            ["find-generic-password", "-s", "svc", "-w"]
        );
    }

    /// orca's two spellings of "absent": exit 44, or the message.
    #[test]
    fn a_missing_item_is_none_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let by_status = stub(dir.path(), "exit 44");
        assert!(reader(&by_status)
            .generic_password("svc", None)
            .expect("read")
            .is_none());
        let by_message = stub(
            dir.path(),
            "echo 'security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.' >&2; exit 1",
        );
        assert!(reader(&by_message)
            .generic_password("svc", None)
            .expect("read")
            .is_none());
    }

    /// A refused prompt is a failure to report, never an absent item. Read as
    /// absent, it would tell the operator they are not signed in.
    #[test]
    fn a_refusal_is_an_error_and_carries_none_of_the_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(
            dir.path(),
            "echo 'LEAKED' ; echo 'User canceled the operation.' >&2; exit 128",
        );
        let err = reader(&program)
            .generic_password("svc", None)
            .expect_err("refused");
        assert_eq!(err, KeychainError::Refused(Some(128)));
        assert!(!format!("{err} {err:?}").contains("LEAKED"));
    }

    #[test]
    fn an_empty_item_is_an_error_not_an_empty_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(dir.path(), "echo '   '");
        assert_eq!(
            reader(&program)
                .generic_password("svc", None)
                .expect_err("empty"),
            KeychainError::Empty
        );
    }

    #[test]
    fn a_read_that_never_returns_times_out_and_is_killed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stub(dir.path(), "exec sleep 30");
        let started = Instant::now();
        let err = SecurityCli::with_program(&program, Duration::from_millis(300))
            .generic_password("svc", None)
            .expect_err("timed out");
        assert_eq!(err, KeychainError::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_tool_that_is_not_there_is_a_spawn_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = reader(&dir.path().join("missing"))
            .generic_password("svc", None)
            .expect_err("missing");
        assert!(matches!(err, KeychainError::Spawn(_)), "{err:?}");
    }
}
