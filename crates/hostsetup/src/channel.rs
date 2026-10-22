//! The local channel between the command mode and the running app.
//!
//! One interface, two transports. On Unix it is a socket,
//! `<store dir>/run/cli.sock`; on Windows a named pipe whose name is derived
//! from the store directory. Either way only this user can reach it, the app
//! checks every peer's user, the command mode checks the app's (a request
//! carries a token), and nothing is ever handed back: anyone who can connect
//! could already edit `store.json` as this user, so the channel adds no new
//! principal, and no reply carries a token, so it adds no way to read one.
//!
//! The protocol is the same on both: one JSON line per connection, capped at
//! [`MAX_LINE`], answered by one line. [`handle`] is that exchange, over any
//! byte stream, so `protocol`, `pairing` and `server` never learn which
//! transport they are on.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
    BufReader as AsyncBufReader,
};

use crate::protocol::{Response, MAX_LINE};
use crate::server::{respond, Agent, HostBook, ServerInfo};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{bind, request, serve, socket_path, Listener, RUN_DIR, SOCKET_FILE};
#[cfg(windows)]
pub use windows::{bind, pipe_name, request, serve, Listener};

/// How long the server waits for a request line, and the command mode for a
/// reply. A reply can include a probe and a health check, each with its own
/// timeout, so the client waits longer than the server.
const SERVER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);
/// The pause after a failed `accept` before trying again.
const ACCEPT_RETRY: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum ChannelError {
    /// Nothing is serving the channel: Solador is not running (or is a build
    /// that predates the hand-off).
    NotRunning,
    /// The channel's directory belongs to someone else or is open to others,
    /// or its server is not running as this user.
    Insecure(String),
    /// Something already serves the channel: a second Solador instance, or on
    /// Windows a process that took the pipe's name first. The two cannot be
    /// told apart there, and both are refused the same way.
    AlreadyServed,
    #[cfg(unix)]
    TooLong(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // What the command knows is that nothing answered, not why. The
            // app being closed is the likely reason; an open one whose
            // hand-off failed to start is the other, and only a relaunch
            // retries that bind.
            ChannelError::NotRunning => f.write_str(
                "Solador isn't running. Open it (it holds your hosts and their tokens), then re-run. \
                 If it is already open, quit and reopen it: its command-line hand-off did not start.",
            ),
            #[cfg(unix)]
            ChannelError::Insecure(why) => write!(f, "refusing the hand-off socket: {why}"),
            #[cfg(windows)]
            ChannelError::Insecure(why) => write!(f, "refusing the hand-off pipe: {why}"),
            #[cfg(unix)]
            ChannelError::AlreadyServed => {
                f.write_str("another Solador instance already answers the hand-off socket")
            }
            #[cfg(windows)]
            ChannelError::AlreadyServed => f.write_str(
                "something already holds the hand-off pipe: another Solador instance, \
                 or another process that took its name first",
            ),
            #[cfg(unix)]
            ChannelError::TooLong(path) => write!(
                f,
                "the hand-off socket path is longer than this platform allows ({} bytes): {}",
                unix::SUN_PATH_MAX - 1,
                path.display()
            ),
            #[cfg(unix)]
            ChannelError::Io(error) => write!(f, "hand-off socket: {error}"),
            #[cfg(windows)]
            ChannelError::Io(error) => write!(f, "hand-off pipe: {error}"),
        }
    }
}

impl From<std::io::Error> for ChannelError {
    fn from(error: std::io::Error) -> Self {
        ChannelError::Io(error)
    }
}

/// The store directory both sides agree on, by the rule the app opens its
/// store with ([`store::Store::resolved_dir`]): `SOLADOR_STORE_DIR` when set,
/// so a scratch instance pairs with a scratch command, else the platform
/// default.
///
/// # Errors
/// When the platform has no per-user config directory.
pub fn store_dir() -> Result<PathBuf, String> {
    store::Store::resolved_dir().map_err(|error| error.to_string())
}

/// One request and one reply over `stream`, then the connection is dropped.
async fn handle<S, B, A>(
    stream: S,
    book: &B,
    agent: &A,
    info: &ServerInfo,
    adds: &tokio::sync::Mutex<()>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
    B: HostBook,
    A: Agent,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut line = String::new();
    let mut reader = AsyncBufReader::new(read.take(MAX_LINE as u64 + 1));
    let read = tokio::time::timeout(SERVER_READ_TIMEOUT, reader.read_line(&mut line)).await;
    let response = match read {
        // A probe of a stale socket connects and hangs up: nothing to answer.
        Ok(Ok(0)) => return,
        Ok(Ok(_)) if line.len() > MAX_LINE || !line.ends_with('\n') => {
            Response::refused("Request too long.")
        }
        Ok(Ok(_)) => respond(line.trim_end(), book, agent, info, adds).await,
        Ok(Err(_)) => Response::refused("Malformed request."),
        Err(_) => Response::refused("Timed out waiting for the request."),
    };
    let mut reply = serde_json::to_string(&response).expect("responses serialise");
    reply.push('\n');
    let _ = write.write_all(reply.as_bytes()).await;
}

/// Why a peer on the channel is not this user's. Constants, and nothing read
/// from the peer: they reach a log line.
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    OtherUser,
    Unreadable,
}

#[cfg(any(windows, test))]
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::OtherUser => "a process running as another user",
            Refusal::Unreadable => "a process whose user could not be read",
        })
    }
}

/// Whether a peer may be trusted: only a process whose token's user SID is
/// byte for byte this process's. Both directions use it: the app for each
/// client, the command mode for the app (a request carries a token). A SID
/// that could not be read, or is empty, is a refusal, never a match.
#[cfg(any(windows, test))]
fn admit_sid(peer: &std::io::Result<Vec<u8>>, me: &[u8]) -> Result<(), Refusal> {
    match peer {
        Ok(sid) if !sid.is_empty() && sid == me => Ok(()),
        Ok(_) => Err(Refusal::OtherUser),
        Err(_) => Err(Refusal::Unreadable),
    }
}

/// The pipe both sides open for `store_dir`: `\\.\pipe\solador-handoff-<hash>`.
/// The name is not a secret (the DACL and the SID checks are the protection),
/// only a function of the directory, as the socket's path is. FNV-1a over the
/// path's text rather than `DefaultHasher`, whose output is not promised
/// across Rust versions: the app and the command are one binary today, but
/// nothing should depend on that.
#[cfg(any(windows, test))]
fn derive_pipe_name(store_dir: &std::path::Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in store_dir.to_string_lossy().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!(r"\\.\pipe\solador-handoff-{hash:016x}")
}

/// The DACL that makes a pipe the current user's alone: protected (inherits
/// nothing) with one ACE granting `GENERIC_ALL` to that SID. The counterpart of
/// the socket directory's `0700`.
#[cfg(any(windows, test))]
fn owner_only_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const ME: &[u8] = &[1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 7, 0, 0, 0];

    #[test]
    fn only_a_peer_with_this_users_sid_is_admitted() {
        assert_eq!(admit_sid(&Ok(ME.to_vec()), ME), Ok(()));
        let mut other = ME.to_vec();
        *other.last_mut().unwrap() = 8;
        assert_eq!(admit_sid(&Ok(other), ME), Err(Refusal::OtherUser));
        // Everyone (S-1-1-0) and a prefix of ours are not us either.
        assert_eq!(
            admit_sid(&Ok(vec![1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]), ME),
            Err(Refusal::OtherUser)
        );
        assert_eq!(
            admit_sid(&Ok(ME[..ME.len() - 1].to_vec()), ME),
            Err(Refusal::OtherUser)
        );
    }

    #[test]
    fn a_sid_that_could_not_be_read_is_never_a_match() {
        let unreadable = Err(std::io::Error::other("access denied"));
        assert_eq!(admit_sid(&unreadable, ME), Err(Refusal::Unreadable));
        // Two empty SIDs must not compare equal to each other.
        assert_eq!(admit_sid(&Ok(Vec::new()), &[]), Err(Refusal::OtherUser));
    }

    #[test]
    fn the_pipe_name_is_a_stable_function_of_the_store_dir() {
        let dir = Path::new(r"C:\Users\me\AppData\Roaming\app.solador.desktop");
        let a = derive_pipe_name(dir);
        assert!(a.starts_with(r"\\.\pipe\solador-handoff-"), "{a}");
        assert_eq!(a, derive_pipe_name(dir));
        // The FNV-1a offset basis: pins the algorithm, since the app and the
        // command compute the name independently.
        assert_eq!(
            derive_pipe_name(Path::new("")),
            r"\\.\pipe\solador-handoff-cbf29ce484222325"
        );
        let b = derive_pipe_name(Path::new(r"C:\scratch\store"));
        assert_ne!(a, b, "two stores must not share a pipe");
        assert!(
            !a.contains("Users"),
            "the name must not spell the path: {a}"
        );
    }

    #[test]
    fn the_dacl_names_exactly_one_principal() {
        assert_eq!(
            owner_only_sddl("S-1-5-21-1-2-3-1001"),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)"
        );
    }
}
