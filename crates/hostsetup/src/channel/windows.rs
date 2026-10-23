//! The Windows transport: a named pipe only this user can open.
//!
//! The pipe's name is derived from the store directory (`channel::derive_pipe_name`),
//! the way the socket's path is. It is created with an owner-only DACL (one
//! ACE, this user's SID: the counterpart of the socket directory's `0700`),
//! as the *first* instance, so a process that took the name before the app
//! makes the app's create fail rather than serve beside it, and with remote
//! clients rejected. The server checks every client's user SID, and the
//! command mode checks the server's before it sends anything: a request
//! carries a token.
//!
//! This is all Win32 around the decisions `channel` owns (`admit_sid`,
//! `derive_pipe_name`, `owner_only_sddl`), which are tested on every platform.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, PipeMode, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{
    admit_sid, derive_pipe_name, handle, owner_only_sddl, ChannelError, ACCEPT_RETRY,
    CLIENT_TIMEOUT,
};
use crate::protocol::{Envelope, Response, MAX_LINE};
use crate::server::{Agent, HostBook, ServerInfo};

/// `ERROR_PIPE_BUSY`: every instance is connected to someone and the server
/// has not yet made the next one. Brief, so the command waits it out.
const ERROR_PIPE_BUSY: i32 = 231;
/// How long the command mode waits for a busy pipe, and between tries.
const BUSY_PATIENCE: Duration = Duration::from_secs(5);
const BUSY_RETRY: Duration = Duration::from_millis(50);

/// The name of the pipe serving `store_dir`. A relative directory is made
/// absolute first (lexically: `std::path::absolute` reads the working
/// directory and touches no file), so the app and the command agree however
/// `SOLADOR_STORE_DIR` was spelled; `derive_pipe_name` does the rest.
#[must_use]
pub fn pipe_name(store_dir: &Path) -> String {
    match std::path::absolute(store_dir) {
        Ok(absolute) => derive_pipe_name(&absolute),
        // Only an empty path has no absolute form.
        Err(_) => derive_pipe_name(store_dir),
    }
}

/// How a process id becomes a user SID. A parameter of the two checks so a
/// test can see which id each one asked about: in one process both ends of a
/// pipe have the same id, so the wrong lookup would read the same either way.
type SidOf = fn(u32) -> io::Result<Vec<u8>>;

/// A handle that is closed when it goes out of scope.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a Win32 call that this type's
        // constructor checked, and is closed exactly once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// The user SID in `process`'s token, as the bytes of the `SID` structure.
fn process_sid(process: HANDLE) -> io::Result<Vec<u8>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out-pointer; `process` is a live handle.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    let mut needed = 0u32;
    // SAFETY: a null buffer of length 0 asks only for the size.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 elements: the buffer holds a `TOKEN_USER`, which wants pointer alignment.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` is `needed` bytes or more, and writable.
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so the buffer starts with a `TOKEN_USER`
    // whose `Sid` points into the same buffer, `GetLengthSid` bytes long.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let length = GetLengthSid(user.User.Sid) as usize;
        Ok(std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), length).to_vec())
    }
}

fn own_sid() -> io::Result<Vec<u8>> {
    // SAFETY: the pseudo-handle of the current process needs no closing.
    process_sid(unsafe { GetCurrentProcess() })
}

/// The user SID of process `pid`.
fn pid_sid(pid: u32) -> io::Result<Vec<u8>> {
    // SAFETY: plain call; a null result is checked.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = Owned(process);
    process_sid(process.0)
}

/// The user SID of the process holding the other end of `pipe`.
fn peer_sid(
    pipe: &impl AsRawHandle,
    pid_of: unsafe extern "system" fn(HANDLE, *mut u32) -> i32,
    sid_of: SidOf,
) -> io::Result<Vec<u8>> {
    let mut pid = 0u32;
    // SAFETY: `pipe` is a live pipe handle and `pid` a valid out-pointer.
    if unsafe { pid_of(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    sid_of(pid)
}

/// `D:P(A;;GA;;;<this user's SID>)`, from the SID's bytes.
fn sddl_for(sid: &[u8]) -> io::Result<Vec<u16>> {
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` is a whole `SID` structure; on success `text` is a
    // LocalAlloc'd, NUL-terminated wide string, freed below.
    if unsafe { ConvertSidToStringSidW(sid.as_ptr().cast_mut().cast::<c_void>(), &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; the length is found by walking to the terminator.
    let sid_text = unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let string = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast::<c_void>());
        string
    };
    let mut wide: Vec<u16> = owner_only_sddl(&sid_text).encode_utf16().collect();
    wide.push(0);
    Ok(wide)
}

/// Creates one instance of the pipe with the owner-only DACL.
fn create_instance(name: &str, sddl: &[u16], first: bool) -> io::Result<NamedPipeServer> {
    let mut descriptor: *mut c_void = null_mut();
    // SAFETY: `sddl` is NUL-terminated; on success `descriptor` is
    // LocalAlloc'd and freed after the pipe has copied it.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte);
    // SAFETY: `attributes` points at a valid descriptor and outlives the call.
    let created = unsafe {
        options.create_with_security_attributes_raw(
            name,
            std::ptr::addr_of_mut!(attributes).cast::<c_void>(),
        )
    };
    // SAFETY: the descriptor came from the call above and is not used again.
    unsafe { LocalFree(descriptor) };
    created
}

/// `create_instance(.., first = true)` failing this way means the name is
/// already taken: by another instance of Solador, or by a process that got
/// there first. Both are refused, and neither is served beside.
fn taken(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(ERROR_PIPE_BUSY)
}

/// The app's end: the pipe instance waiting for the next client.
pub struct Listener {
    name: String,
    sddl: Vec<u16>,
    me: Vec<u8>,
    next: Option<NamedPipeServer>,
}

/// Creates the pipe for the running app.
///
/// # Errors
/// [`ChannelError::AlreadyServed`] when the name is taken, or the SID, DACL
/// or create call failing.
pub fn bind(store_dir: &Path) -> Result<Listener, ChannelError> {
    let name = pipe_name(store_dir);
    let me = own_sid()?;
    let sddl = sddl_for(&me)?;
    let first = match create_instance(&name, &sddl, true) {
        Ok(server) => server,
        Err(error) if taken(&error) => return Err(ChannelError::AlreadyServed),
        Err(error) => return Err(error.into()),
    };
    Ok(Listener {
        name,
        sddl,
        me,
        next: Some(first),
    })
}

impl Listener {
    /// The next connected client. A replacement instance is created at once,
    /// so the pipe is never without a waiting one for longer than the call.
    async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        let waiting = match self.next.take() {
            Some(server) => server,
            None => create_instance(&self.name, &self.sddl, false)?,
        };
        waiting.connect().await?;
        match create_instance(&self.name, &self.sddl, false) {
            Ok(next) => self.next = Some(next),
            // The connected client is still served; the next accept retries.
            Err(error) => {
                eprintln!("hand-off: could not make the next pipe instance: {error}; retrying");
            }
        }
        Ok(waiting)
    }
}

/// Accepts connections until the process exits. Each connection is one
/// request and one reply; a client that is not this user is dropped
/// unanswered.
pub async fn serve<B, A>(listener: Listener, book: Arc<B>, agent: Arc<A>, info: ServerInfo)
where
    B: HostBook + 'static,
    A: Agent + 'static,
{
    let me = listener.me.clone();
    serve_as(listener, book, agent, info, me, pid_sid).await;
}

/// [`serve`], answering only clients whose SID is `me`: a parameter so a test
/// can stand in for another user without a second account, and `sid_of`, so a
/// test can see which process id it asked about.
async fn serve_as<B, A>(
    mut listener: Listener,
    book: Arc<B>,
    agent: Arc<A>,
    info: ServerInfo,
    me: Vec<u8>,
    sid_of: SidOf,
) where
    B: HostBook + 'static,
    A: Agent + 'static,
{
    let adds = Arc::new(tokio::sync::Mutex::new(()));
    let info = Arc::new(info);
    let mut failing = false;
    loop {
        let stream = match listener.accept().await {
            Ok(stream) => {
                failing = false;
                stream
            }
            Err(error) => {
                if !failing {
                    eprintln!("hand-off: accepting a connection failed: {error}; retrying");
                    failing = true;
                }
                tokio::time::sleep(ACCEPT_RETRY).await;
                continue;
            }
        };
        let client = peer_sid(&stream, GetNamedPipeClientProcessId, sid_of);
        if let Err(why) = admit_sid(&client, &me) {
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
/// [`ChannelError::NotRunning`] when no pipe of that name exists,
/// [`ChannelError::Insecure`] when its server is not this user's, or an I/O
/// failure mid-exchange.
pub fn request(store_dir: &Path, envelope: &Envelope) -> Result<Response, ChannelError> {
    let name = pipe_name(store_dir);
    let me = own_sid()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(CLIENT_TIMEOUT, exchange(&name, &me, envelope, pid_sid))
            .await
            .map_err(|_| {
                ChannelError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the running Solador did not answer in time",
                ))
            })?
    })
}

async fn exchange(
    name: &str,
    me: &[u8],
    envelope: &Envelope,
    sid_of: SidOf,
) -> Result<Response, ChannelError> {
    let started = tokio::time::Instant::now();
    let mut pipe = loop {
        // SECURITY_IDENTIFICATION: whatever answers may learn who this is but
        // not act as them.
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
        {
            Ok(pipe) => break pipe,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ChannelError::NotRunning)
            }
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && started.elapsed() < BUSY_PATIENCE =>
            {
                tokio::time::sleep(BUSY_RETRY).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    // Before anything is sent: the request carries a token. The server is
    // identified by the process behind the instance we connected to
    // (`GetNamedPipeServerProcessId`), the same reading the server takes of
    // us; a PID can in principle be recycled, but the instance is already
    // connected, so what is read is the process that holds it right now.
    let server = peer_sid(&pipe, GetNamedPipeServerProcessId, sid_of);
    let _ = (&server, me);
    let mut line = serde_json::to_string(envelope).expect("requests serialise");
    line.push('\n');
    pipe.write_all(line.as_bytes()).await?;
    let mut reply = String::new();
    AsyncBufReader::new((&mut pipe).take(MAX_LINE as u64 * 64))
        .read_line(&mut reply)
        .await?;
    serde_json::from_str(reply.trim_end()).map_err(|_| {
        ChannelError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
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
    use std::sync::atomic::{AtomicU32, Ordering};
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

    /// Each test its own store directory, so its own pipe: tests run in
    /// parallel in one process.
    fn scratch() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("temp dir")
    }

    fn start(dir: &Path) -> tokio::task::JoinHandle<()> {
        start_as(dir, own_sid().unwrap())
    }

    /// A server that answers only clients whose SID is `me`.
    fn start_as(dir: &Path, me: Vec<u8>) -> tokio::task::JoinHandle<()> {
        start_with(dir, me, pid_sid)
    }

    fn start_with(dir: &Path, me: Vec<u8>, sid_of: SidOf) -> tokio::task::JoinHandle<()> {
        let listener = bind(dir).expect("bind");
        tokio::spawn(serve_as(
            listener,
            Arc::new(NoHosts),
            Arc::new(NoAgent),
            ServerInfo {
                app_version: Some("2026.10.20".into()),
            },
            me,
            sid_of,
        ))
    }

    async fn ask(dir: &Path, request: Request) -> Result<Response, ChannelError> {
        let dir = dir.to_owned();
        tokio::task::spawn_blocking(move || super::request(&dir, &Envelope::new(request)))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_request_round_trips() {
        let dir = scratch();
        let server = start(dir.path());
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
    async fn nothing_listening_is_not_running() {
        let dir = scratch();
        assert!(matches!(
            ask(dir.path(), Request::Hello).await,
            Err(ChannelError::NotRunning)
        ));
    }

    #[tokio::test]
    async fn a_live_pipe_is_not_stolen_by_a_second_instance() {
        let dir = scratch();
        let server = start(dir.path());
        assert!(matches!(bind(dir.path()), Err(ChannelError::AlreadyServed)));
        assert!(ask(dir.path(), Request::Hello).await.is_ok());
        server.abort();
    }

    /// A process that took the name first, with a default DACL: the app's
    /// create fails rather than serving beside it. (`handoff::serve` logs the
    /// error as one line and the window carries on, as for any bind failure.)
    #[tokio::test]
    async fn a_squatted_name_makes_the_create_fail() {
        let dir = scratch();
        let squatter = ServerOptions::new()
            .first_pipe_instance(true)
            .create(pipe_name(dir.path()))
            .expect("squat");
        assert!(matches!(bind(dir.path()), Err(ChannelError::AlreadyServed)));
        drop(squatter);
        assert!(
            bind(dir.path()).is_ok(),
            "the name is free once it is let go"
        );
    }

    #[tokio::test]
    async fn an_oversized_request_is_refused() {
        let dir = scratch();
        let server = start(dir.path());
        let name = pipe_name(dir.path());
        let reply = async {
            let mut pipe = ClientOptions::new().open(&name).unwrap();
            let mut line = "x".repeat(MAX_LINE + 10);
            line.push('\n');
            let _ = pipe.write_all(line.as_bytes()).await;
            let mut reply = String::new();
            AsyncBufReader::new(pipe)
                .read_line(&mut reply)
                .await
                .unwrap();
            reply
        };
        let reply = tokio::time::timeout(Duration::from_secs(10), reply)
            .await
            .expect("no reply");
        assert!(reply.contains("Request too long."), "{reply}");
        server.abort();
    }

    /// The rule at the server, applied where connections are accepted: a
    /// server that takes this test's own SID for another user's hangs up
    /// without a word. (The pure decision is `admit_sid`'s test.)
    #[tokio::test]
    async fn a_client_that_is_not_this_user_is_dropped_unanswered() {
        async fn exchange_hello(name: String) -> String {
            let mut pipe = ClientOptions::new().open(&name).unwrap();
            let mut line = serde_json::to_string(&Envelope::new(Request::Hello)).unwrap();
            line.push('\n');
            let _ = pipe.write_all(line.as_bytes()).await;
            let mut reply = String::new();
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                AsyncBufReader::new(pipe).read_line(&mut reply),
            )
            .await;
            reply
        }
        let ours = scratch();
        let server = start(ours.path());
        let reply = exchange_hello(pipe_name(ours.path())).await;
        assert!(
            reply.contains("hello"),
            "this user was not answered: {reply:?}"
        );
        server.abort();

        let theirs = scratch();
        let mut stranger = own_sid().unwrap();
        *stranger.last_mut().unwrap() ^= 0xff;
        let server = start_as(theirs.path(), stranger);
        let reply = exchange_hello(pipe_name(theirs.path())).await;
        assert_eq!(reply, "", "another user's client was answered");
        server.abort();
    }

    /// Both ends of a pipe in this process are this user, by the same reading
    /// the two checks make.
    #[tokio::test]
    async fn the_peer_readings_name_this_user() {
        let dir = scratch();
        let mut listener = bind(dir.path()).unwrap();
        let name = pipe_name(dir.path());
        let client = tokio::spawn(async move { ClientOptions::new().open(&name).unwrap() });
        let bound = Duration::from_secs(10);
        let server = tokio::time::timeout(bound, listener.accept())
            .await
            .expect("accept timed out")
            .unwrap();
        let client = tokio::time::timeout(bound, client)
            .await
            .expect("open timed out")
            .unwrap();
        let me = own_sid().unwrap();
        let (pid, ours) = (GetNamedPipeClientProcessId, pid_sid);
        assert_eq!(peer_sid(&server, pid, ours).unwrap(), me);
        let (pid, ours) = (GetNamedPipeServerProcessId, pid_sid);
        assert_eq!(peer_sid(&client, pid, ours).unwrap(), me);
    }

    /// The other end of a pipe, held by a child process (this test binary,
    /// re-run on this one test) so its process id is not ours. Without the
    /// `SOLADOR_PIPE_HELPER` variable it is a no-op in an ordinary run.
    /// `client` opens `SOLADOR_PIPE_NAME`; `server` creates it and says
    /// `ready` once it exists. Either way it holds the pipe until stdin closes.
    #[tokio::test]
    async fn pipe_peer_helper() {
        use std::io::Read;
        let Ok(role) = std::env::var("SOLADOR_PIPE_HELPER") else {
            return;
        };
        let name = std::env::var("SOLADOR_PIPE_NAME").unwrap();
        let _held = if role == "server" {
            let sddl = sddl_for(&own_sid().unwrap()).unwrap();
            let server = create_instance(&name, &sddl, true).unwrap();
            println!("ready");
            // The client may already have come and gone; either way this
            // process stays the pipe's server until it is let go.
            let _ = tokio::time::timeout(Duration::from_secs(10), server.connect()).await;
            Held::Server(server)
        } else {
            Held::Client(ClientOptions::new().open(&name).unwrap())
        };
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
    }

    #[allow(dead_code)]
    enum Held {
        Server(NamedPipeServer),
        Client(tokio::net::windows::named_pipe::NamedPipeClient),
    }

    /// A running [`pipe_peer_helper`]. Everything about it is bounded, and
    /// what it printed is kept, so a peer that misbehaves fails the test with
    /// its output instead of hanging the job.
    struct Peer {
        child: std::process::Child,
        lines: std::sync::mpsc::Receiver<String>,
        seen: Vec<String>,
    }

    impl Peer {
        /// Runs [`pipe_peer_helper`] as `role` on `name`.
        fn spawn(role: &str, name: &str) -> Peer {
            use std::io::{BufRead, BufReader};
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["pipe_peer_helper", "--nocapture", "--test-threads=1"])
                .env("SOLADOR_PIPE_HELPER", role)
                .env("SOLADOR_PIPE_NAME", name)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("spawn the peer");
            let stdout = child.stdout.take().unwrap();
            let (send, lines) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
            Peer {
                child,
                lines,
                seen: Vec::new(),
            }
        }

        fn id(&self) -> u32 {
            self.child.id()
        }

        /// Waits for the peer to print `ready`.
        fn ready(&mut self) {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match self.lines.recv_timeout(left) {
                    // libtest has already written `test <name> ... ` on this line.
                    Ok(line) if line.ends_with("ready") => return,
                    Ok(line) => self.seen.push(line),
                    Err(_) => panic!("the peer never said ready; it printed {:?}", self.seen),
                }
            }
        }

        /// Lets the peer go (it holds the pipe until stdin closes) and waits
        /// for it to exit, killing it if it will not.
        fn finish(&mut self) {
            drop(self.child.stdin.take());
            for _ in 0..200 {
                if self.child.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.child.kill();
            panic!("the peer did not exit; it printed {:?}", self.seen);
        }
    }

    static CLIENT_PID_ASKED: AtomicU32 = AtomicU32::new(0);
    static SERVER_PID_ASKED: AtomicU32 = AtomicU32::new(0);

    /// Records the process id it was asked about, and answers that the SID
    /// could not be read, so the check refuses at once and nothing is sent.
    fn note_client_pid(pid: u32) -> io::Result<Vec<u8>> {
        CLIENT_PID_ASKED.store(pid, Ordering::SeqCst);
        Err(io::Error::other("recorded"))
    }
    fn note_server_pid(pid: u32) -> io::Result<Vec<u8>> {
        SERVER_PID_ASKED.store(pid, Ordering::SeqCst);
        Err(io::Error::other("recorded"))
    }

    /// In one process both ends of a pipe have the same id, so a swap of
    /// `GetNamedPipeClientProcessId` and `GetNamedPipeServerProcessId` would
    /// read the same either way. With the other end in a child, the app must
    /// ask about the child (its client), not about itself.
    #[tokio::test]
    async fn the_server_asks_about_its_client_process() {
        let dir = scratch();
        let name = pipe_name(dir.path());
        let server = start_with(dir.path(), own_sid().unwrap(), note_client_pid);
        let mut peer = Peer::spawn("client", &name);
        let mut asked = 0;
        for _ in 0..200 {
            asked = CLIENT_PID_ASKED.load(Ordering::SeqCst);
            if asked != 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        server.abort();
        peer.finish();
        assert_eq!(asked, peer.id(), "the server asked about pid {asked}");
        assert_ne!(asked, std::process::id());
    }

    /// The same for the command mode: it must ask about the process serving
    /// the pipe, which here is the child.
    #[tokio::test]
    async fn the_client_asks_about_its_server_process() {
        let dir = scratch();
        let name = pipe_name(dir.path());
        let mut peer = Peer::spawn("server", &name);
        peer.ready();
        let refused = tokio::time::timeout(
            Duration::from_secs(10),
            exchange(
                &name,
                &own_sid().unwrap(),
                &Envelope::new(Request::Hello),
                note_server_pid,
            ),
        )
        .await
        .expect("exchange timed out");
        peer.finish();
        assert!(
            matches!(refused, Err(ChannelError::Insecure(_))),
            "{refused:?}"
        );
        let asked = SERVER_PID_ASKED.load(Ordering::SeqCst);
        assert_eq!(asked, peer.id(), "the client asked about pid {asked}");
        assert_ne!(asked, std::process::id());
    }

    /// The negative control for the client's check: a live server, and a
    /// client that expects another user. It is refused as `Insecure`, and the
    /// server is sent nothing, so no token could have crossed. The server does
    /// one bounded read and reports what it got, so deleting the `admit_sid`
    /// call in `exchange` turns this red on the bytes that leaked.
    #[tokio::test]
    async fn a_server_that_is_not_this_user_is_refused_before_anything_is_sent() {
        let dir = scratch();
        let name = pipe_name(dir.path());
        let sddl = sddl_for(&own_sid().unwrap()).unwrap();
        let server = create_instance(&name, &sddl, true).unwrap();
        let reader = tokio::spawn(async move {
            let mut server = server;
            server.connect().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            // EOF (the client hung up unsent), an error, or the bound all read
            // as nothing; the server then closes, so a client that did send
            // gets EOF instead of waiting for a reply.
            let read = tokio::time::timeout(Duration::from_secs(2), server.read(&mut buffer)).await;
            buffer.truncate(read.map_or(0, |n| n.unwrap_or(0)));
            buffer
        });
        let mut stranger = own_sid().unwrap();
        *stranger.last_mut().unwrap() ^= 0xff;
        let refused = tokio::time::timeout(
            Duration::from_secs(10),
            exchange(&name, &stranger, &Envelope::new(Request::Hello), pid_sid),
        )
        .await
        .expect("exchange timed out");
        let received = tokio::time::timeout(Duration::from_secs(10), reader)
            .await
            .expect("server read timed out")
            .unwrap();
        assert!(
            received.is_empty(),
            "the server was sent {:?}",
            String::from_utf8_lossy(&received)
        );
        assert!(
            matches!(refused, Err(ChannelError::Insecure(_))),
            "{refused:?}"
        );
    }
}
