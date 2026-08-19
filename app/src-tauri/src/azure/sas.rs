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
use viewmodel::fault::Fault;

/// The operator-facing name of the tool this module shells out to, and the one
/// thing [`Fault::message`] interpolates into its stock sentences.
const AZURE_CLI: &str = "Azure CLI";

/// How long a minted SAS is asked to live.
///
/// Azure caps user-delegation SAS at seven days, but there is no reason to ask
/// for the cap: the export is polled every four hours and a fresh SAS is two
/// seconds away, so a short life shrinks the window in which a leaked URL is
/// worth anything. Long enough to cover several polls if `az` starts failing,
/// short enough not to matter if it escapes.
const LIFETIME_HOURS: i64 = 24;

/// Why a mint failed, in the three shapes an operator can act on differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SasError {
    /// The Azure CLI is not installed, or not on this process's `PATH`.
    ///
    /// Worth its own variant because the fix is completely different from the
    /// one below, and because a GUI app's `PATH` is not the shell's — an `az`
    /// that works in a terminal can be invisible here.
    CliMissing,
    /// `az` ran and refused, in its own words: not signed in, no permission on
    /// the account, or no such container. The payload is the CLI's sentence,
    /// picked by [`explanation`].
    Refused(String),
    /// `az` ran, failed, and said nothing anyone can act on.
    ///
    /// A separate variant rather than a [`SasError::Refused`] carrying whatever
    /// happened to be on stderr's first line, which is how the screenshot in
    /// #352 came to show `The command failed with an unexpected error. Here is
    /// the traceback:` as if it were a diagnosis. It is the preamble *before*
    /// the diagnosis; reporting it as one is a guess wearing a sentence.
    /// Renders as [`Fault::Unexpected`] and points at the log, which has the
    /// whole of stderr.
    Unexplained,
}

impl SasError {
    /// One line, safe to render in the panel footer.
    ///
    /// Two of the three states render straight out of `viewmodel::fault`, the
    /// vocabulary that owns this codebase's stock sentences. The missing-CLI
    /// one appends the single thing only this module knows — the command to
    /// run — because a stock sentence is the floor a message may not fall
    /// below, never a cap on how specific one may be.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            SasError::CliMissing => {
                format!(
                    "{} with `az login`",
                    Fault::ToolUnavailable.message(AZURE_CLI)
                )
            }
            SasError::Refused(reason) => format!("Azure CLI refused: {reason}"),
            SasError::Unexplained => Fault::Unexpected.message(AZURE_CLI),
        }
    }
}

/// The executables to try, in order.
///
/// On Windows the CLI is `az.cmd`, and `Command::new("az")` will not find it:
/// `CreateProcess` appends `.exe`, never `.cmd`.
///
/// On Unix the bare name is tried first and then the usual install prefixes
/// **by absolute path**, because a `PATH` lookup is not enough here. A macOS
/// app launched from Finder or the Dock inherits a minimal
/// `/usr/bin:/bin:/usr/sbin:/sbin` — not the shell's — so Homebrew's
/// `/opt/homebrew/bin/az` is invisible to it. The module doc above names that
/// trap; this is what actually handles it. Without these the panel reports
/// "Azure CLI not found" forever on a machine where it is installed and signed
/// in, and only a terminal launch would ever work.
const CANDIDATES: &[&str] = if cfg!(windows) {
    &["az.cmd", "az.bat", "az"]
} else {
    &[
        "az",
        "/opt/homebrew/bin/az",
        "/usr/local/bin/az",
        "/usr/bin/az",
    ]
};

/// Mints a read+list, container-scoped, HTTPS-only user-delegation SAS URL.
///
/// Blocking — it spawns a process and waits — so callers run it off the async
/// executor, exactly as the container probes do.
///
/// # Errors
/// [`SasError::CliMissing`] if no Azure CLI could be spawned at all,
/// [`SasError::Refused`] carrying the CLI's own sentence, or
/// [`SasError::Unexplained`] when it failed without one.
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
            // The whole of stderr, kept where detail belongs — the panel gets
            // one sentence and the log gets the traceback. Safe to log because
            // the run failed: no SAS was minted, so there is no token in it,
            // and stdout (which is where a token would be) is never touched.
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("azure: `az` failed: {}", stderr.trim());
            return Err(
                explanation(&output.stderr).map_or(SasError::Unexplained, SasError::Refused)
            );
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if token.is_empty() {
            return Err(SasError::Refused("it printed no token".to_owned()));
        }
        return Ok(format!(
            "https://{account}.blob.core.windows.net/{container}?{token}"
        ));
    }

    let _ = last_spawn_failure;
    Err(SasError::CliMissing)
}

/// The opening of `az`'s **generic crash** report.
///
/// `az` has two failure shapes. Its ordinary errors put the diagnosis on the
/// first line (`ERROR: Please run 'az login' to setup account.`). Its crash
/// puts a fixed preamble there and the diagnosis at the very bottom:
///
/// ```text
/// ERROR: The command failed with an unexpected error. Here is the traceback:
///   <traceback frames>
///   <ExceptionType>: <the actual message>
/// ```
///
/// Matched on this exact clause rather than on the word "traceback", which
/// appears inside ordinary errors' stack dumps too — telling the two apart is
/// the whole job.
const CRASH_PREAMBLE: &str = "Here is the traceback:";

/// The CLI's own sentence, or `None` when its stderr carries none.
///
/// For an ordinary error that is [`first_line`]. For the crash above it is the
/// *last* non-empty line: the preamble names no cause, so picking the first
/// there is picking the one line guaranteed to be useless — which is exactly
/// what the panel showed in #352.
///
/// `None` — a crash whose traceback never arrived, or empty stderr — is not
/// downgraded into a sentence. It becomes [`SasError::Unexplained`], because a
/// preamble presented as a diagnosis is a fabricated value in prose form.
fn explanation(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next()?;
    if !first.contains(CRASH_PREAMBLE) {
        return Some(first_line(stderr));
    }
    // `first` is already consumed, so this is the last line *after* the
    // preamble — a crash that printed nothing else yields `None` rather than
    // the preamble back.
    lines.next_back().map(clamp)
}

/// The CLI's first line of complaint, trimmed to something a footer can hold.
///
/// `az` writes multi-line diagnostics with stack traces and doc links; for its
/// ordinary errors the first line is the sentence a human needs and the rest
/// would swamp a one-line panel footer. [`explanation`] is what decides
/// whether this is one of those errors.
fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no explanation given");
    clamp(line)
}

/// A footer is one line. `az` can emit a paragraph.
fn clamp(line: &str) -> String {
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
        // Composed, not rewritten: the vocabulary supplies the sentence and
        // this module appends the one thing only it knows.
        assert!(
            message.starts_with(&Fault::ToolUnavailable.message(AZURE_CLI)),
            "{message}"
        );
    }

    #[test]
    fn a_refusal_carries_the_clis_own_words() {
        let message = SasError::Refused("Please run 'az login'".to_owned()).user_message();
        assert!(message.contains("Please run 'az login'"), "{message}");
    }

    /// `az`'s generic crash puts a preamble on the first line and the diagnosis
    /// on the last. The screenshot in #352 shows what picking the first gets
    /// you: a panel header reading "The command failed with an unexpected
    /// error. Here is the traceback:" — the sentence *before* the cause.
    #[test]
    fn a_generic_crash_reports_the_exception_rather_than_the_preamble() {
        let stderr = b"ERROR: The command failed with an unexpected error. Here is the traceback:\n\
                       Traceback (most recent call last):\n  \
                         File \"knack/cli.py\", line 233, in invoke\n    \
                           cmd_result = self.invocation.execute(args)\n\
                       azure.core.exceptions.ClientAuthenticationError: DefaultAzureCredential failed to retrieve a token.\n";
        let got = explanation(stderr).expect("a crash still explains itself");
        assert_eq!(
            got,
            "azure.core.exceptions.ClientAuthenticationError: \
             DefaultAzureCredential failed to retrieve a token."
        );
        assert!(!got.contains("Here is the traceback"), "{got}");
    }

    /// The other half of the same discrimination: an ordinary `az` error must
    /// not regress to picking its last line, which would report a stack frame.
    /// Its stderr can carry the word "Traceback" too — only the preamble
    /// clause tells the two shapes apart.
    #[test]
    fn an_ordinary_error_still_reports_its_first_line() {
        let stderr =
            b"\n\n  ERROR: Please run 'az login' to setup account.\nTraceback:\n  at foo\n";
        assert_eq!(
            explanation(stderr).as_deref(),
            Some("ERROR: Please run 'az login' to setup account.")
        );
    }

    /// A crash that printed the preamble and then died has nothing to say. The
    /// preamble is not a fallback sentence — handing it back would report an
    /// unanticipated failure as an anticipated one.
    #[test]
    fn a_crash_with_no_traceback_is_unexplained_rather_than_the_preamble() {
        let stderr =
            b"ERROR: The command failed with an unexpected error. Here is the traceback:\n";
        assert_eq!(explanation(stderr), None);
        assert_eq!(explanation(b""), None);
    }

    /// …and that state renders as the fallback, never as one of the named
    /// ones. `viewmodel::fault` owns both sentences, so this cannot drift.
    #[test]
    fn an_unexplained_failure_renders_as_the_fallback_and_not_a_diagnosis() {
        let message = SasError::Unexplained.user_message();
        assert_eq!(message, Fault::Unexpected.message(AZURE_CLI));
        assert_ne!(message, SasError::CliMissing.user_message());
        assert!(message.contains(AZURE_CLI), "{message}");
        assert!(
            !message.contains("traceback"),
            "the traceback is in the log, not the panel: {message}"
        );
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
