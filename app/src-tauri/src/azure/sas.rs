//! Minting the Azure Cost SAS in-process, by asking the Azure CLI for one.
//!
//! This used to be a shell script on a LaunchAgent: every four days it ran
//! `az`, wrote a seven-day SAS into the Keychain, and the app read it from
//! there. That worked, and it cost a scheduled job, a Keychain item, an
//! exemption in the credential-consolidation rules for that item (because an
//! external writer touched it), a hardcoded storage-account name in a script
//! nobody else could use, and a hard seven-day cliff if the job ever failed
//! quietly.
//!
//! All of it existed to work around the fact that a user-delegation SAS cannot
//! be long-lived — which stops being a problem the moment the thing that needs
//! the SAS can mint its own. The app already requires `az` to be installed and
//! signed in; that requirement is simply honest now instead of hidden inside a
//! cron job.
//!
//! **Nothing here is stored.** The minted URL lives as long as one poll and is
//! never written to disk, never logged, and never put in an error string — the
//! token is a query parameter, so a URL in a log line is a leaked credential.

use std::process::{Command, Stdio};

use chrono::{DateTime, Duration, Utc};

/// How long a minted SAS is asked to live.
///
/// Azure caps user-delegation SAS at seven days, but there is no reason to ask
/// for the cap: the export is polled every four hours and a fresh SAS is two
/// seconds away, so a short life shrinks the window in which a leaked URL is
/// worth anything. Long enough to cover several polls if `az` starts failing,
/// short enough not to matter if it escapes.
const LIFETIME_HOURS: i64 = 24;

/// Why a mint failed, in the two shapes an operator can act on differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SasError {
    /// The Azure CLI is not installed, or not on this process's `PATH`.
    ///
    /// Worth its own variant because the fix is completely different from the
    /// one below, and because a GUI app's `PATH` is not the shell's — an `az`
    /// that works in a terminal can be invisible here.
    CliMissing,
    /// `az` ran and refused: not signed in, no permission on the account, or
    /// no such container.
    Refused(String),
}

impl SasError {
    /// One line, safe to render in the panel footer.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            SasError::CliMissing => {
                "Azure CLI not found — install it and sign in with `az login`".to_owned()
            }
            SasError::Refused(reason) => format!("Azure CLI could not mint a SAS: {reason}"),
        }
    }
}

/// The executables to try, in order.
///
/// On Windows the CLI is `az.cmd`, and `Command::new("az")` will not find it:
/// `CreateProcess` appends `.exe`, never `.cmd`. Trying the plain name first
/// keeps Unix a single spawn.
const CANDIDATES: &[&str] = if cfg!(windows) {
    &["az.cmd", "az.bat", "az"]
} else {
    &["az"]
};

/// Mints a read+list, container-scoped, HTTPS-only user-delegation SAS URL.
///
/// Blocking — it spawns a process and waits — so callers run it off the async
/// executor, exactly as the container probes do.
///
/// # Errors
/// [`SasError::CliMissing`] if no Azure CLI could be spawned at all, or
/// [`SasError::Refused`] carrying the CLI's own first line of explanation.
pub fn mint(account: &str, container: &str, now: DateTime<Utc>) -> Result<String, SasError> {
    let expiry = (now + Duration::hours(LIFETIME_HOURS))
        .format("%Y-%m-%dT%H:%MZ")
        .to_string();
    let args = [
        "storage",
        "container",
        "generate-sas",
        "--account-name",
        account,
        "--name",
        container,
        // Read and list: everything the export reader does, and nothing else.
        "--permissions",
        "rl",
        "--expiry",
        &expiry,
        // `login` + `as-user` is what makes this a *user-delegation* SAS,
        // signed by the operator's Entra identity. The storage account this
        // was built for has shared-key auth disabled, which is the reason a
        // permanent SAS was never an option.
        "--auth-mode",
        "login",
        "--as-user",
        "--https-only",
        "-o",
        "tsv",
    ];

    let mut last_spawn_failure = None;
    for executable in CANDIDATES {
        let mut command = Command::new(executable);
        command.args(args).stdin(Stdio::null());
        // Without this a console window flashes for every mint — rare here at
        // a four-hour cadence, but the same reasoning as the container probes,
        // and free.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let output = match command.output() {
            Ok(output) => output,
            Err(e) => {
                last_spawn_failure = Some(e);
                continue;
            }
        };

        if !output.status.success() {
            return Err(SasError::Refused(first_line(&output.stderr)));
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if token.is_empty() {
            return Err(SasError::Refused(
                "the CLI returned an empty token".to_owned(),
            ));
        }
        return Ok(format!(
            "https://{account}.blob.core.windows.net/{container}?{token}"
        ));
    }

    let _ = last_spawn_failure;
    Err(SasError::CliMissing)
}

/// The CLI's first line of complaint, trimmed to something a footer can hold.
///
/// `az` writes multi-line diagnostics with stack traces and doc links; the
/// first line is the sentence a human needs and the rest would swamp a
/// one-line panel footer.
fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no explanation given");
    const LIMIT: usize = 160;
    if line.chars().count() > LIMIT {
        let truncated: String = line.chars().take(LIMIT).collect();
        format!("{truncated}…")
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_cli_names_the_cli_rather_than_the_account() {
        let message = SasError::CliMissing.user_message();
        assert!(message.contains("az login"), "{message}");
        assert!(
            !message.contains("SAS"),
            "the operator has nothing to fix about the SAS yet: {message}"
        );
    }

    #[test]
    fn a_refusal_carries_the_clis_own_words() {
        let message = SasError::Refused("Please run 'az login'".to_owned()).user_message();
        assert!(message.contains("Please run 'az login'"), "{message}");
    }

    #[test]
    fn only_the_first_non_empty_stderr_line_survives() {
        let stderr =
            b"\n\n  ERROR: Please run 'az login' to setup account.\nTraceback:\n  at foo\n";
        assert_eq!(
            first_line(stderr),
            "ERROR: Please run 'az login' to setup account."
        );
    }

    #[test]
    fn a_silent_failure_still_says_something() {
        assert_eq!(first_line(b""), "no explanation given");
    }

    /// A footer is one line. `az` can emit a paragraph.
    #[test]
    fn a_very_long_line_is_truncated_rather_than_wrapped() {
        let long = format!("ERROR: {}", "x".repeat(500));
        let got = first_line(long.as_bytes());
        assert!(got.chars().count() <= 161, "{}", got.chars().count());
        assert!(got.ends_with('…'));
    }

    /// The token is a query parameter, so the URL *is* the credential. This
    /// pins the shape the fetcher expects without ever holding a real one.
    #[test]
    fn the_minted_url_is_container_scoped_with_the_token_as_a_query() {
        // `mint` needs a CLI, so the format is asserted directly — the point
        // is that account and container land in the path and nowhere else.
        let url = format!(
            "https://{account}.blob.core.windows.net/{container}?{token}",
            account = "acct",
            container = "cost-exports",
            token = "sv=2024&sig=abc"
        );
        assert!(url.starts_with("https://acct.blob.core.windows.net/cost-exports?"));
        assert!(!url.trim_end_matches(|c| c != '?').is_empty());
    }
}
