//! The Unix transport: a socket at `<store dir>/run/cli.sock`.
//!
//! The directory is created `0700` and both sides refuse it unless it belongs
//! to the current user and nobody else can enter it; the server also checks
//! every peer's uid.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::UnixListener;

use super::{handle, ChannelError, ACCEPT_RETRY, CLIENT_TIMEOUT};
use crate::protocol::{Envelope, Response, MAX_LINE};
use crate::server::{Agent, HostBook, ServerInfo};

/// What [`bind`] returns and [`serve`] accepts.
pub type Listener = UnixListener;

pub const RUN_DIR: &str = "run";
pub const SOCKET_FILE: &str = "cli.sock";

/// `sun_path`'s size, terminator included: a longer path cannot be bound or
/// connected to, and the kernel's error for it names neither the path nor the
/// limit.
#[cfg(target_os = "macos")]
pub(super) const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
pub(super) const SUN_PATH_MAX: usize = 108;

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
/// user. `Err` is the reason it is dropped, for the log, and it is a constant:
/// nothing read from the peer's credentials reaches a log line.
fn admit(peer_uid: std::io::Result<u32>, me: u32) -> Result<(), &'static str> {
    match peer_uid {
        Ok(uid) if uid == me => Ok(()),
        Ok(_) => Err("a peer running as another user"),
        Err(_) => Err("a peer whose user could not be read"),
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
pub fn bind(store_dir: &Path) -> Result<Listener, ChannelError> {
    let path = &socket_path(store_dir);
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
pub async fn serve<B, A>(listener: Listener, book: Arc<B>, agent: Arc<A>, info: ServerInfo)
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
    use std::time::Duration;
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
        let listener = bind(dir).expect("bind");
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
        drop(bind(dir.path()).unwrap());
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
        assert!(matches!(bind(dir.path()), Err(ChannelError::AlreadyServed)));
        assert!(ask(dir.path(), Request::Hello).await.is_ok());
        server.abort();
    }

    #[tokio::test]
    async fn a_directory_open_to_others_is_refused_by_both_sides() {
        let dir = short_dir();
        let run = dir.path().join(RUN_DIR);
        std::fs::DirBuilder::new().mode(0o755).create(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(bind(dir.path()), Err(ChannelError::Insecure(_))));
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
        assert_eq!(admit(Ok(502), 501), Err("a peer running as another user"));
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
        let error = bind(&deep).unwrap_err();
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
