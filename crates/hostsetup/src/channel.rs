//! The local socket between the command mode and the running app (Unix).
//!
//! `<store dir>/run/cli.sock`. The directory is created `0700` and both sides
//! refuse it unless it belongs to the current user and nobody else can enter
//! it; the server also checks every peer's uid. Anyone who can connect could
//! already edit `store.json` as this user, so this adds no new principal — and
//! it never hands a token back, so it adds no new way to read one.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::protocol::{Envelope, Response, MAX_LINE};
use crate::server::{respond, Agent, HostBook, ServerInfo};

pub const RUN_DIR: &str = "run";
pub const SOCKET_FILE: &str = "cli.sock";

/// `sun_path`'s size, terminator included: a longer path cannot be bound or
/// connected to, and the kernel's error for it names neither the path nor the
/// limit.
#[cfg(target_os = "macos")]
const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
const SUN_PATH_MAX: usize = 108;

/// How long the server waits for a request line, and the command mode for a
/// reply. A reply can include a probe and a health check, each with its own
/// timeout, so the client waits longer than the server.
const SERVER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);
/// The pause after a failed `accept` before trying again.
const ACCEPT_RETRY: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum ChannelError {
    /// No socket, or one nobody is listening on: Solador is not running (or
    /// is a build that predates the hand-off).
    NotRunning,
    /// The socket's directory belongs to someone else or is open to others.
    Insecure(String),
    /// A live server already owns the socket (a second Solador instance).
    AlreadyServed,
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
            ChannelError::Insecure(why) => write!(f, "refusing the hand-off socket: {why}"),
            ChannelError::AlreadyServed => {
                f.write_str("another Solador instance already answers the hand-off socket")
            }
            ChannelError::TooLong(path) => write!(
                f,
                "the hand-off socket path is longer than this platform allows ({} bytes): {}",
                SUN_PATH_MAX - 1,
                path.display()
            ),
            ChannelError::Io(error) => write!(f, "hand-off socket: {error}"),
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

#[must_use]
pub fn socket_path(store_dir: &Path) -> PathBuf {
    store_dir.join(RUN_DIR).join(SOCKET_FILE)
}

fn check_len(path: &Path) -> Result<(), ChannelError> {
    if path.as_os_str().len() >= SUN_PATH_MAX {
        return Err(ChannelError::TooLong(path.to_owned()));
    }
    Ok(())
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The socket's directory must be ours, and closed to everyone else.
fn verify_dir(dir: &Path) -> Result<(), ChannelError> {
    let meta = std::fs::symlink_metadata(dir)?;
    check_dir(dir, meta.is_dir(), meta.uid(), meta.mode(), euid())
}

/// [`verify_dir`]'s rule, over what the metadata said and whose process this
/// is, so the refusals can be tested without a second user account.
fn check_dir(dir: &Path, is_dir: bool, owner: u32, mode: u32, me: u32) -> Result<(), ChannelError> {
    if !is_dir {
        return Err(ChannelError::Insecure(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    if owner != me {
        return Err(ChannelError::Insecure(format!(
            "{} belongs to uid {owner}, not this user",
            dir.display()
        )));
    }
    if mode & 0o077 != 0 {
        return Err(ChannelError::Insecure(format!(
            "{} is mode {:o}; it must be 700",
            dir.display(),
            mode & 0o777
        )));
    }
    Ok(())
}

/// Whether a connected peer may be answered: only a process running as this
/// user. `Err` is the reason it is dropped, for the log.
fn admit(peer_uid: std::io::Result<u32>, me: u32) -> Result<(), String> {
    match peer_uid {
        Ok(uid) if uid == me => Ok(()),
        Ok(uid) => Err(format!("a peer running as uid {uid}, not this user")),
        Err(error) => Err(format!("a peer whose uid could not be read ({error})")),
    }
}

/// A connect that failed this way found nothing listening: no socket file, or
/// one an exited instance left behind. Any other failure is not evidence the
/// owner is gone, so neither side treats it as such.
fn nobody_listening(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Binds the socket for the running app.
///
/// A socket file left by an instance that exited is replaced. One a live
/// instance still answers is not: that instance keeps it, and this one runs
/// without a hand-off rather than stealing it.
///
/// # Errors
/// [`ChannelError::AlreadyServed`], an insecure or unusable directory, a path
/// too long to bind, or the bind itself failing.
pub fn bind(path: &Path) -> Result<UnixListener, ChannelError> {
    check_len(path)?;
    let dir = path.parent().expect("socket path has a directory");
    if !dir.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    verify_dir(dir)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => match StdUnixStream::connect(path) {
            Ok(_) => return Err(ChannelError::AlreadyServed),
            Err(error) if nobody_listening(&error) => std::fs::remove_file(path)?,
            Err(error) => return Err(error.into()),
        },
        Ok(_) => {
            return Err(ChannelError::Insecure(format!(
                "{} exists and is not a socket",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accepts connections until the process exits. Each connection is one
/// request and one reply; a peer that is not this user is dropped unanswered.
pub async fn serve<B, A>(listener: UnixListener, book: Arc<B>, agent: Arc<A>, info: ServerInfo)
where
    B: HostBook + 'static,
    A: Agent + 'static,
{
    serve_as(listener, book, agent, info, euid()).await;
}

/// [`serve`], answering only peers running as `me`: a parameter so a test can
/// stand in for another user without a second account.
async fn serve_as<B, A>(
    listener: UnixListener,
    book: Arc<B>,
    agent: Arc<A>,
    info: ServerInfo,
    me: u32,
) where
    B: HostBook + 'static,
    A: Agent + 'static,
{
    let adds = Arc::new(tokio::sync::Mutex::new(()));
    let info = Arc::new(info);
    let mut failing = false;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => {
                failing = false;
                stream
            }
            Err(error) => {
                // A persistent failure (out of descriptors, say) returns at
                // once on every call: say so once, and stop it becoming a
                // busy loop inside the cockpit.
                if !failing {
                    eprintln!("hand-off: accepting a connection failed: {error}; retrying");
                    failing = true;
                }
                tokio::time::sleep(ACCEPT_RETRY).await;
                continue;
            }
        };
        if let Err(why) = admit(stream.peer_cred().map(|cred| cred.uid()), me) {
            eprintln!("hand-off: dropped {why}");
            continue;
        }
        let (book, agent, info, adds) = (
            Arc::clone(&book),
            Arc::clone(&agent),
            Arc::clone(&info),
            Arc::clone(&adds),
        );
        tokio::spawn(async move {
            handle(stream, &*book, &*agent, &info, &adds).await;
        });
    }
}

async fn handle<B: HostBook, A: Agent>(
    stream: UnixStream,
    book: &B,
    agent: &A,
    info: &ServerInfo,
    adds: &tokio::sync::Mutex<()>,
) {
    let (read, mut write) = stream.into_split();
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

/// The command mode's side: one request, one reply, blocking.
///
/// # Errors
/// [`ChannelError::NotRunning`] when nothing is listening, an insecure
/// directory, or an I/O failure mid-exchange.
pub fn request(store_dir: &Path, envelope: &Envelope) -> Result<Response, ChannelError> {
    let path = socket_path(store_dir);
    check_len(&path)?;
    match std::fs::symlink_metadata(path.parent().expect("socket path has a directory")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ChannelError::NotRunning)
        }
        _ => verify_dir(path.parent().expect("socket path has a directory"))?,
    }
    let mut stream = match StdUnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) if nobody_listening(&error) => return Err(ChannelError::NotRunning),
        Err(error) => return Err(error.into()),
    };
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    let mut line = serde_json::to_string(envelope).expect("requests serialise");
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream.take(MAX_LINE as u64 * 64)).read_line(&mut reply)?;
    serde_json::from_str(reply.trim_end()).map_err(|_| {
        ChannelError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the running Solador sent a reply this command cannot read",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::{Decision, Pairing};
    use crate::protocol::{ProbeSummary, Request};
    use crate::Secret;
    use store::Host;
    use uuid::Uuid;

    struct NoHosts;
    impl HostBook for NoHosts {
        fn hosts(&self) -> Vec<Host> {
            vec![Host::new("agent", "192.168.1.2")]
        }
        fn token_matches(&self, _: Uuid, _: &Secret) -> bool {
            false
        }
        fn apply(&self, _: &Pairing<'_>, _: &Decision) -> Result<Host, String> {
            Err("read-only".into())
        }
    }
    struct NoAgent;
    impl Agent for NoAgent {
        async fn probe(&self, _: &str, _: u16) -> ProbeSummary {
            ProbeSummary::NoTls
        }
        async fn health(&self, _: &str, _: u16, _: &str, _: &Secret) -> Result<String, String> {
            Err("no".into())
        }
    }

    /// A short base: macOS's temp dir alone is most of `sun_path`.
    fn short_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("hs")
            .tempdir_in("/tmp")
            .expect("temp dir")
    }

    fn start(dir: &Path) -> tokio::task::JoinHandle<()> {
        start_as(dir, euid())
    }

    /// A server that answers only peers running as `me`.
    fn start_as(dir: &Path, me: u32) -> tokio::task::JoinHandle<()> {
        let listener = bind(&socket_path(dir)).expect("bind");
        tokio::spawn(serve_as(
            listener,
            Arc::new(NoHosts),
            Arc::new(NoAgent),
            ServerInfo {
                app_version: Some("2026.10.20".into()),
            },
            me,
        ))
    }

    async fn ask(dir: &Path, request: Request) -> Result<Response, ChannelError> {
        let dir = dir.to_owned();
        tokio::task::spawn_blocking(move || super::request(&dir, &Envelope::new(request)))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_request_round_trips_and_the_directory_is_private() {
        let dir = short_dir();
        let server = start(dir.path());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().mode() & 0o777;
        assert_eq!(mode(&dir.path().join(RUN_DIR)), 0o700);
        assert_eq!(mode(&socket_path(dir.path())), 0o600);

        let hello = ask(dir.path(), Request::Hello).await.unwrap();
        assert_eq!(
            hello,
            Response::Hello {
                protocol: crate::protocol::PROTOCOL,
                app_version: Some("2026.10.20".into())
            }
        );
        let Response::Hosts { hosts } = ask(dir.path(), Request::List).await.unwrap() else {
            panic!("not a host list");
        };
        assert_eq!(hosts[0].address, "192.168.1.2");
        server.abort();
    }

    #[tokio::test]
    async fn nothing_listening_is_not_running_whether_or_not_a_socket_file_is_left() {
        let dir = short_dir();
        assert!(matches!(
            ask(dir.path(), Request::Hello).await,
            Err(ChannelError::NotRunning)
        ));
        // An instance that exited leaves its socket file behind.
        drop(bind(&socket_path(dir.path())).unwrap());
        assert!(socket_path(dir.path()).exists());
        assert!(matches!(
            ask(dir.path(), Request::Hello).await,
            Err(ChannelError::NotRunning)
        ));
        // ...which the next instance replaces rather than tripping over.
        let server = start(dir.path());
        assert!(ask(dir.path(), Request::Hello).await.is_ok());
        server.abort();
    }

    #[tokio::test]
    async fn a_live_socket_is_not_stolen_by_a_second_instance() {
        let dir = short_dir();
        let server = start(dir.path());
        assert!(matches!(
            bind(&socket_path(dir.path())),
            Err(ChannelError::AlreadyServed)
        ));
        assert!(ask(dir.path(), Request::Hello).await.is_ok());
        server.abort();
    }

    #[tokio::test]
    async fn a_directory_open_to_others_is_refused_by_both_sides() {
        let dir = short_dir();
        let run = dir.path().join(RUN_DIR);
        std::fs::DirBuilder::new().mode(0o755).create(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            bind(&socket_path(dir.path())),
            Err(ChannelError::Insecure(_))
        ));
        assert!(matches!(
            ask(dir.path(), Request::Hello).await,
            Err(ChannelError::Insecure(_))
        ));
    }

    /// Another user's directory, which a test cannot create without a second
    /// account: the rule over its metadata instead.
    #[test]
    fn a_directory_another_user_owns_is_refused_however_closed_it_is() {
        let dir = Path::new("/tmp/x/run");
        assert!(check_dir(dir, true, 501, 0o40700, 501).is_ok());
        let Err(ChannelError::Insecure(why)) = check_dir(dir, true, 0, 0o40700, 501) else {
            panic!("a root-owned 0700 directory was accepted");
        };
        assert!(why.contains("uid 0"), "{why}");
        for mode in [0o40750, 0o40705, 0o40777] {
            assert!(
                check_dir(dir, true, 501, mode, 501).is_err(),
                "mode {mode:o} was accepted"
            );
        }
        assert!(check_dir(dir, false, 501, 0o100600, 501).is_err());
    }

    #[test]
    fn only_a_peer_running_as_this_user_is_answered() {
        assert!(admit(Ok(501), 501).is_ok());
        let refused = admit(Ok(502), 501).unwrap_err();
        assert!(refused.contains("uid 502"), "{refused}");
        assert!(admit(Ok(0), 501).is_err(), "root is another user here too");
        assert!(admit(Err(std::io::Error::other("no creds")), 501).is_err());
    }

    /// The rule above, applied where connections are accepted: a server that
    /// takes this test's own uid for another user's hangs up without a word.
    #[tokio::test]
    async fn a_peer_that_is_not_this_user_is_dropped_unanswered() {
        let exchange = |path: PathBuf| {
            tokio::task::spawn_blocking(move || {
                let mut stream = StdUnixStream::connect(&path).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut line = serde_json::to_string(&Envelope::new(Request::Hello)).unwrap();
                line.push('\n');
                let _ = stream.write_all(line.as_bytes());
                let mut reply = String::new();
                let _ = BufReader::new(stream).read_line(&mut reply);
                reply
            })
        };
        let ours = short_dir();
        let server = start_as(ours.path(), euid());
        let reply = exchange(socket_path(ours.path())).await.unwrap();
        assert!(
            reply.contains("hello"),
            "this user was not answered: {reply:?}"
        );
        server.abort();

        let theirs = short_dir();
        let server = start_as(theirs.path(), euid().wrapping_add(1));
        let reply = exchange(socket_path(theirs.path())).await.unwrap();
        assert_eq!(reply, "", "another user's peer was answered");
        server.abort();
    }

    #[test]
    fn a_path_longer_than_sun_path_is_refused_by_name() {
        let deep = PathBuf::from(format!("/tmp/{}", "d".repeat(SUN_PATH_MAX)));
        let error = bind(&socket_path(&deep)).unwrap_err();
        assert!(matches!(error, ChannelError::TooLong(_)));
        assert!(error.to_string().contains("ddd"), "{error}");
    }

    #[tokio::test]
    async fn an_oversized_request_is_refused() {
        let dir = short_dir();
        let server = start(dir.path());
        let path = socket_path(dir.path());
        let reply = tokio::task::spawn_blocking(move || {
            let mut stream = StdUnixStream::connect(&path).unwrap();
            let mut line = "x".repeat(MAX_LINE + 10);
            line.push('\n');
            let _ = stream.write_all(line.as_bytes());
            let mut reply = String::new();
            BufReader::new(stream).read_line(&mut reply).unwrap();
            reply
        })
        .await
        .unwrap();
        assert!(reply.contains("Request too long."), "{reply}");
        server.abort();
    }
}
