//! `solador-agent update` / `rollback`, end to end (#393).
//!
//! What is real here: the filesystem transaction (a temporary install tree
//! with a live executable, `.new`, `.prev` and the lock), the HTTP consumer
//! (a loopback "release server" serving a feed, its signature and the
//! binaries), the signature and hash verification (the same `minisign-verify`
//! the shipped binary uses, against keys generated *in this test* from fixed
//! seeds — no production private key exists anywhere near this file), the
//! candidate's `--version` execution, and the authenticated `/v1/health`
//! poll (a loopback endpoint that reports whatever the "service" started).
//!
//! What is faked: the service manager. [`FakeService`] does on `restart()`
//! what systemd or launchd would do — executes the live path's `--version`
//! and makes the health endpoint report it — or, per scenario, keeps serving
//! the old version (a stale ExecStart), never comes up, or refuses to
//! restart. Every scenario asserts on *observable* state: the bytes at the
//! live path and at `.prev`, whether `.new` exists, how many restarts
//! happened, which assets the release server was asked for, and the exit
//! code the error maps to.
//!
//! The one place the real service manager is exercised is
//! [`launchd_smoke`], opt-in (`SOLADOR_DEPLOY_TEST_LAUNCHD=1`, macOS): a
//! throwaway LaunchAgent under a temporary HOME, the real built agent, real
//! `launchctl kickstart`, and a failed update that is automatically rolled
//! back on it — the same pattern `agent/deploy/lib_test.sh` uses for the
//! installer. It is never run in CI.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
// Consumed only by the macOS launchd smoke below; gated like it, so the
// Linux clippy leg (`-D unused-imports`) sees no unused import.
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
#[cfg(target_os = "macos")]
use solador_agent::update::Expect;
use solador_agent::update::{
    self, asset_name, sha256_hex, Context, Install, RollbackOutcome, Service, ServiceControl,
    Trust, UpdateError, UpdateOutcome, FEED_ASSET,
};

// ---------------------------------------------------------------------------
// A minisign signer, for tests only
// ---------------------------------------------------------------------------

/// A deterministic minisign keypair. The format is minisign's own, so the
/// shipped verifier (`minisign-verify`, unchanged) is what checks it:
/// Ed25519 over BLAKE2b-512 of the payload (the "ED" prehashed algorithm the
/// pinned `rsign2` and the stock `minisign` both emit), then a global
/// signature over `signature || trusted comment`.
struct TestKey {
    signing: ed25519_dalek::SigningKey,
    key_id: [u8; 8],
    name: &'static str,
}

impl TestKey {
    fn from_seed(name: &'static str, seed: u8) -> Self {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = seed
                .wrapping_mul(31)
                .wrapping_add(i as u8 * 7)
                .wrapping_add(seed);
        }
        let signing = ed25519_dalek::SigningKey::from_bytes(&bytes);
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&<sha2::Sha256 as sha2::Digest>::digest(bytes)[..8]);
        TestKey {
            signing,
            key_id,
            name,
        }
    }

    /// The `.pub` file text: two lines, the second base64 of
    /// `"Ed" || key id || public key`.
    fn pubkey_text(&self) -> String {
        use base64::Engine as _;
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(b"Ed");
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(&self.signing.verifying_key().to_bytes());
        format!(
            "untrusted comment: minisign public key (test key {})\n{}\n",
            self.name,
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }

    /// The id `minisign -V` would print for this key.
    fn id(&self) -> String {
        update::key_id(&self.pubkey_text()).expect("a test key has an id")
    }

    /// A `.minisig` text over `payload`, made for the file named
    /// `trusted_comment` — exactly what `scripts/agent-signing.sh` produces
    /// with `-t <basename>`.
    fn sign(&self, payload: &[u8], trusted_comment: &str) -> String {
        use base64::Engine as _;
        use blake2::Digest as _;
        use ed25519_dalek::Signer as _;
        let digest = blake2::Blake2b512::digest(payload);
        let sig = self.signing.sign(&digest).to_bytes();
        let mut line1 = Vec::with_capacity(74);
        line1.extend_from_slice(b"ED");
        line1.extend_from_slice(&self.key_id);
        line1.extend_from_slice(&sig);
        let mut global_input = Vec::with_capacity(64 + trusted_comment.len());
        global_input.extend_from_slice(&sig);
        global_input.extend_from_slice(trusted_comment.as_bytes());
        let global = self.signing.sign(&global_input).to_bytes();
        let b64 = base64::engine::general_purpose::STANDARD;
        format!(
            "untrusted comment: solador-agent release signature\n{}\ntrusted comment: {}\n{}\n",
            b64.encode(line1),
            trusted_comment,
            b64.encode(global)
        )
    }
}

fn key_a() -> TestKey {
    TestKey::from_seed("A", 1)
}
fn key_b() -> TestKey {
    TestKey::from_seed("B", 2)
}
fn key_c() -> TestKey {
    TestKey::from_seed("C", 3)
}

fn trust(keys: &[&TestKey]) -> Trust {
    let texts: Vec<String> = keys.iter().map(|k| k.pubkey_text()).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    Trust::from_texts(&refs).expect("test keys decode")
}

/// The signer above is not trusted on its own word: a signature it makes
/// must verify under the production verifier, and a moved byte must not.
#[test]
fn the_test_signer_produces_signatures_the_shipped_verifier_accepts() {
    let a = key_a();
    let t = trust(&[&a]);
    let sig = a.sign(b"payload", "thing");
    assert_eq!(t.verify("thing", &sig, b"payload").unwrap(), a.id());
    assert!(t.verify("thing", &sig, b"payloae").is_err());
    assert!(t.verify("other", &sig, b"payload").is_err());
    assert_ne!(key_a().id(), key_b().id());
    assert_ne!(key_b().id(), key_c().id());
    // Deterministic: the same seed is the same key, so a fixture pinned to
    // one of these ids stays pinned.
    assert_eq!(key_a().id(), key_a().id());
}

// ---------------------------------------------------------------------------
// A release, and a loopback server for it
// ---------------------------------------------------------------------------

/// This host's triple — the tests build a feed for the machine they run on.
fn target() -> &'static str {
    update::host_target().expect("tests run on a published target")
}

/// Everything a release carries for the updater: the tag `latest`
/// redirects to, the feed and its signature, and the assets.
#[derive(Clone)]
struct Release {
    tag: String,
    assets: BTreeMap<String, Vec<u8>>,
}

/// Build a feed for `version` over one binary for this host, signed by
/// `signer`, with the feed's entry hash/signature optionally overridden.
struct FeedSpec<'a> {
    version: &'a str,
    binary: &'a [u8],
    signer: &'a TestKey,
    base: &'a str,
}

fn build_feed(spec: &FeedSpec<'_>) -> (String, String) {
    let asset = asset_name(spec.version, target());
    let sig = spec.signer.sign(spec.binary, &asset);
    let doc = serde_json::json!({
        "version": spec.version,
        "targets": {
            target(): {
                "url": format!("{}/releases/download/v{}/{asset}", spec.base, spec.version),
                "signature": sig,
                "sha256": sha256_hex(spec.binary),
            }
        }
    });
    let mut text = serde_json::to_string_pretty(&doc).unwrap();
    text.push('\n');
    let feed_sig = spec.signer.sign(text.as_bytes(), FEED_ASSET);
    (text, feed_sig)
}

#[derive(Clone)]
struct ServerState {
    base: String,
    release: Arc<Mutex<Release>>,
    requests: Arc<Mutex<Vec<String>>>,
    /// Where `/releases/download/…` redirects to: the loopback blob store
    /// (GitHub's shape — every asset download is a 302 to another host), or
    /// a host off loopback that the updater must refuse to follow.
    redirect_base: Arc<Mutex<Option<String>>>,
}

async fn latest(State(s): State<ServerState>) -> Response {
    s.requests.lock().unwrap().push("/releases/latest".into());
    let tag = s.release.lock().unwrap().tag.clone();
    (
        StatusCode::FOUND,
        [(header::LOCATION, format!("{}/releases/tag/{tag}", s.base))],
    )
        .into_response()
}

/// `/releases/download/{tag}/{asset}` answers a 302 to `/blob/{tag}/{asset}`
/// on the redirect base — the shape GitHub serves, so every test exercises
/// the updater's redirect policy rather than only its predicate.
async fn download(
    State(s): State<ServerState>,
    AxumPath((tag, asset)): AxumPath<(String, String)>,
) -> Response {
    s.requests
        .lock()
        .unwrap()
        .push(format!("/releases/download/{tag}/{asset}"));
    let redirect_base = s
        .redirect_base
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| s.base.clone());
    (
        StatusCode::FOUND,
        [(
            header::LOCATION,
            format!("{redirect_base}/blob/{tag}/{asset}"),
        )],
    )
        .into_response()
}

async fn blob(
    State(s): State<ServerState>,
    AxumPath((tag, asset)): AxumPath<(String, String)>,
) -> Response {
    s.requests
        .lock()
        .unwrap()
        .push(format!("/blob/{tag}/{asset}"));
    let release = s.release.lock().unwrap();
    if release.tag != tag {
        return StatusCode::NOT_FOUND.into_response();
    }
    match release.assets.get(&asset) {
        Some(bytes) => bytes.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A redirect chain of `n` hops that never lands anywhere: `/hop/{n}` →
/// `/hop/{n-1}` … → `/hop/0` → 200.
async fn hop(State(s): State<ServerState>, AxumPath(n): AxumPath<u32>) -> Response {
    if n == 0 {
        return "landed".into_response();
    }
    (
        StatusCode::FOUND,
        [(header::LOCATION, format!("{}/hop/{}", s.base, n - 1))],
    )
        .into_response()
}

/// Serve `release` on loopback; returns the base URL, a handle to swap the
/// release, and the request log.
type Served2 = (
    String,
    Arc<Mutex<Release>>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Option<String>>>,
);

async fn serve_release(release: Release) -> Served2 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let state = ServerState {
        base: base.clone(),
        release: Arc::new(Mutex::new(release)),
        requests: Arc::new(Mutex::new(Vec::new())),
        redirect_base: Arc::new(Mutex::new(None)),
    };
    let (release, requests, redirect_base) = (
        state.release.clone(),
        state.requests.clone(),
        state.redirect_base.clone(),
    );
    let app = Router::new()
        .route("/releases/latest", get(latest))
        .route("/releases/download/{tag}/{asset}", get(download))
        .route("/blob/{tag}/{asset}", get(blob))
        .route("/hop/{n}", get(hop))
        .with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, release, requests, redirect_base)
}

/// A base URL is only known once the server is up, and the feed's URLs
/// embed it — so the release is built in two steps: bind first, then fill.
async fn release_for(spec_version: &str, binary: &[u8], signer: &TestKey) -> Rig {
    let placeholder = Release {
        tag: format!("v{spec_version}"),
        assets: BTreeMap::new(),
    };
    let (base, release, requests, redirect_base) = serve_release(placeholder).await;
    let (feed, feed_sig) = build_feed(&FeedSpec {
        version: spec_version,
        binary,
        signer,
        base: &base,
    });
    let mut assets = BTreeMap::new();
    assets.insert(FEED_ASSET.to_string(), feed.into_bytes());
    assets.insert(format!("{FEED_ASSET}.minisig"), feed_sig.into_bytes());
    assets.insert(asset_name(spec_version, target()), binary.to_vec());
    release.lock().unwrap().assets = assets;
    Rig {
        base,
        release,
        requests,
        redirect_base,
    }
}

struct Rig {
    base: String,
    release: Arc<Mutex<Release>>,
    requests: Arc<Mutex<Vec<String>>>,
    redirect_base: Arc<Mutex<Option<String>>>,
}

impl Rig {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
    /// Binary bytes actually fetched: the `/blob/` hops (the
    /// `/releases/download/` hits are the 302s in front of them).
    fn binary_requests(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|r| r.starts_with("/blob/") && r.contains("/solador-agent-"))
            .collect()
    }
    fn set_asset(&self, name: &str, bytes: Vec<u8>) {
        self.release
            .lock()
            .unwrap()
            .assets
            .insert(name.to_string(), bytes);
    }
    fn remove_asset(&self, name: &str) {
        self.release.lock().unwrap().assets.remove(name);
    }
    fn set_tag(&self, tag: &str) {
        self.release.lock().unwrap().tag = tag.to_string();
    }
    fn redirect_downloads_to(&self, base: &str) {
        *self.redirect_base.lock().unwrap() = Some(base.to_string());
    }
}

// ---------------------------------------------------------------------------
// A fake agent binary and a fake service manager with a real health endpoint
// ---------------------------------------------------------------------------

/// A stand-in executable: answers `--version` with `version`, or exits 1
/// when `version` is `None` (a build that carries none). Anything else it
/// is asked to do, it does nothing — the fake service never runs it as a
/// server.
fn fake_agent(version: Option<&str>, marker: &str) -> Vec<u8> {
    match version {
        Some(v) => format!(
            "#!/bin/sh\n# {marker}\nif [ \"$1\" = \"--version\" ]; then printf '%s\\n' '{v}'; exit 0; fi\nexit 0\n"
        ),
        None => format!(
            "#!/bin/sh\n# {marker}\nif [ \"$1\" = \"--version\" ]; then exit 1; fi\nexit 0\n"
        ),
    }
    .into_bytes()
}

#[derive(Clone, Default)]
struct Served {
    up: bool,
    version: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RestartMode {
    /// Execute the live path's `--version` and serve it — what a real
    /// restart does.
    Faithful,
    /// Keep serving whatever was served before: a stale ExecStart.
    Sticky,
    /// Come up serving nothing (503): a crash loop.
    NeverUp,
    /// Come up serving nothing on the first restart, faithfully after: the
    /// new binary crash-loops, the previous one works.
    NeverUpThenFaithful,
    /// Come up answering 200 with **no `version` key**: a build that carries
    /// none, or a process that is not this agent at all.
    UpWithoutVersion,
    /// The same on the first restart only; faithful after.
    UpWithoutVersionThenFaithful,
    /// The manager refuses the first restart, then behaves.
    FailFirst,
    /// The manager refuses every restart.
    FailAlways,
}

struct FakeService {
    live: PathBuf,
    served: Arc<Mutex<Served>>,
    restarts: AtomicUsize,
    mode: Mutex<RestartMode>,
    /// What the manager reports as the service's main pid.
    pid: Mutex<Option<u32>>,
    /// Whether the manager is reachable at all.
    reachable: Mutex<bool>,
}

impl FakeService {
    fn restarts(&self) -> usize {
        self.restarts.load(Ordering::SeqCst)
    }
    fn set_mode(&self, mode: RestartMode) {
        *self.mode.lock().unwrap() = mode;
    }
    fn set_pid(&self, pid: Option<u32>) {
        *self.pid.lock().unwrap() = pid;
    }
    fn set_reachable(&self, reachable: bool) {
        *self.reachable.lock().unwrap() = reachable;
    }
    /// What a faithful restart would serve, applied now (initial state).
    fn start(&self) {
        let version = update::binary_version(&self.live, Duration::from_secs(5)).ok();
        *self.served.lock().unwrap() = Served { up: true, version };
    }
}

impl ServiceControl for FakeService {
    fn describe(&self) -> String {
        "fake service".to_string()
    }
    fn preflight(&self) -> Result<(), String> {
        if *self.reachable.lock().unwrap() {
            Ok(())
        } else {
            Err("cannot reach the fake manager (stubbed)".to_string())
        }
    }
    fn restart(&self) -> Result<(), String> {
        // The mode is read BEFORE the count moves: a test that flips the
        // mode once it sees a restart must not have that flip land on the
        // restart it saw.
        let mode = *self.mode.lock().unwrap();
        let n = self.restarts.fetch_add(1, Ordering::SeqCst) + 1;
        match mode {
            RestartMode::FailAlways => return Err("manager says no".into()),
            RestartMode::FailFirst if n == 1 => return Err("manager says no, once".into()),
            _ => {}
        }
        match mode {
            RestartMode::Sticky => {}
            RestartMode::NeverUp => {
                *self.served.lock().unwrap() = Served {
                    up: false,
                    version: None,
                }
            }
            RestartMode::NeverUpThenFaithful if n == 1 => {
                *self.served.lock().unwrap() = Served {
                    up: false,
                    version: None,
                }
            }
            RestartMode::UpWithoutVersion => {
                *self.served.lock().unwrap() = Served {
                    up: true,
                    version: None,
                }
            }
            RestartMode::UpWithoutVersionThenFaithful if n == 1 => {
                *self.served.lock().unwrap() = Served {
                    up: true,
                    version: None,
                }
            }
            _ => self.start(),
        }
        Ok(())
    }
    fn main_pid(&self) -> Option<u32> {
        *self.pid.lock().unwrap()
    }
    fn inspect_hint(&self, log: Option<&Path>) -> String {
        format!(
            "Inspect:  fake-manager status (log {})",
            log.map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".into())
        )
    }
}

const TOKEN: &str = "tok-MUST-NOT-APPEAR-IN-OUTPUT";

async fn health(State(served): State<Arc<Mutex<Served>>>, headers: HeaderMap) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if auth != format!("Bearer {TOKEN}") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let s = served.lock().unwrap().clone();
    if !s.up {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut body = serde_json::json!({"status": "ok", "hostname": "fake"});
    if let Some(v) = s.version {
        body["version"] = serde_json::json!(v);
    }
    axum::Json(body).into_response()
}

/// Serve `/v1/health` on loopback for `served`; returns the port.
async fn serve_health(served: Arc<Mutex<Served>>, bind: &str) -> Option<u16> {
    let listener = tokio::net::TcpListener::bind(format!("{bind}:0"))
        .await
        .ok()?;
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/health", get(health))
        .with_state(served);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Some(port)
}

/// A temporary install tree: `HOME/.local/bin/solador-agent` (the fake
/// agent), `HOME/.config/solador-agent.env` (token, bind, port), plus the
/// fake service and its health endpoint.
struct Harness {
    _home: tempfile::TempDir,
    install: Install,
    service: Arc<FakeService>,
    served: Arc<Mutex<Served>>,
    lines: Arc<Mutex<Vec<String>>>,
    env_file: PathBuf,
}

impl Harness {
    /// `bind` is what the env file says; the health server listens on the
    /// address `lib.sh`'s rule maps it to.
    async fn new(installed: &[u8], bind: &str) -> Harness {
        let home = tempfile::tempdir().unwrap();
        let bin_dir = home.path().join(".local/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let binary = bin_dir.join("solador-agent");
        fs::write(&binary, installed).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let served = Arc::new(Mutex::new(Served::default()));
        let listen = match bind {
            "" | "0.0.0.0" => "127.0.0.1",
            "::" => "[::1]",
            b => b,
        };
        let port = serve_health(served.clone(), listen)
            .await
            .unwrap_or_else(|| panic!("cannot listen on {listen}"));
        let env_file = home.path().join(".config/solador-agent.env");
        fs::create_dir_all(env_file.parent().unwrap()).unwrap();
        fs::write(
            &env_file,
            format!("SOLADOR_AGENT_TOKEN={TOKEN}\nSOLADOR_AGENT_BIND={bind}\nSOLADOR_AGENT_PORT={port}\n"),
        )
        .unwrap();
        let service = Arc::new(FakeService {
            live: binary.clone(),
            served: served.clone(),
            restarts: AtomicUsize::new(0),
            mode: Mutex::new(RestartMode::Faithful),
            pid: Mutex::new(None),
            reachable: Mutex::new(true),
        });
        service.start();
        Harness {
            install: Install {
                binary,
                env_file: env_file.clone(),
                service: Service::Systemd {
                    unit: "solador-agent".into(),
                },
                log: None,
            },
            _home: home,
            service,
            served,
            lines: Arc::new(Mutex::new(Vec::new())),
            env_file,
        }
    }

    fn live(&self) -> Vec<u8> {
        fs::read(&self.install.binary).unwrap()
    }
    fn prev(&self) -> Option<Vec<u8>> {
        fs::read(self.install.sibling(".prev")).ok()
    }
    fn new_exists(&self) -> bool {
        self.install.sibling(".new").exists()
    }
    fn output(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
    fn served_version(&self) -> Option<String> {
        self.served.lock().unwrap().version.clone()
    }
    /// Make the endpoint report something other than what the live bytes
    /// claim, without a restart: the state an interrupted earlier run leaves.
    fn serve(&self, version: Option<&str>) {
        *self.served.lock().unwrap() = Served {
            up: true,
            version: version.map(str::to_string),
        };
    }

    /// The state every recovered failure must leave: the previous bytes at
    /// the live path, `.prev` still the last-good anchor, no `.new`, the
    /// expected number of restarts, the token nowhere, and the exit code.
    fn assert_recovered(&self, err: &UpdateError, installed: &[u8], restarts: usize, exit: i32) {
        assert_eq!(
            self.live(),
            installed,
            "the previous binary is back at the live path"
        );
        assert_eq!(
            self.prev().unwrap(),
            installed,
            ".prev remains the last-good anchor"
        );
        assert!(!self.new_exists(), ".new was cleaned up");
        assert_eq!(self.service.restarts(), restarts, "restart count");
        assert_eq!(err.exit_code(), exit, "exit code for {err}");
        assert!(err.to_string().contains("Inspect:"), "{err}");
        assert!(!self.output().contains(TOKEN), "{}", self.output());
        assert!(self.output().contains("==> FAILED:"), "{}", self.output());
    }

    async fn update(&self, base: &str, trust: Trust) -> Result<UpdateOutcome, UpdateError> {
        let lines = self.lines.clone();
        let mut report = move |l: &str| lines.lock().unwrap().push(l.to_string());
        let serving = update::read_serving(&self.env_file).unwrap();
        let service: &dyn ServiceControl = &*self.service;
        let mut ctx = Context {
            release_base: base.to_string(),
            trust,
            install: self.install.clone(),
            serving,
            service,
            target: target(),
            running_version: Some("2026.9.1".into()),
            health_attempts: 3,
            health_interval: Duration::from_millis(20),
            report: &mut report,
        };
        update::run_update(&mut ctx).await
    }

    async fn rollback(&self) -> Result<RollbackOutcome, UpdateError> {
        let lines = self.lines.clone();
        let mut report = move |l: &str| lines.lock().unwrap().push(l.to_string());
        let serving = update::read_serving(&self.env_file).unwrap();
        let service: &dyn ServiceControl = &*self.service;
        let mut ctx = Context {
            release_base: "https://unused.invalid".to_string(),
            trust: trust(&[&key_a()]),
            install: self.install.clone(),
            serving,
            service,
            target: target(),
            running_version: None,
            health_attempts: 3,
            health_interval: Duration::from_millis(20),
            report: &mut report,
        };
        update::run_rollback(&mut ctx).await
    }

    /// Nothing changed and nothing restarted: the assertion every refusal
    /// shares.
    fn assert_untouched(&self, installed: &[u8], what: &str) {
        assert_eq!(self.live(), installed, "{what}: the live binary changed");
        assert!(self.prev().is_none(), "{what}: a .prev appeared");
        assert!(!self.new_exists(), "{what}: a .new was left behind");
        assert_eq!(
            self.service.restarts(),
            0,
            "{what}: the service was restarted"
        );
        assert!(
            !self.output().contains(TOKEN),
            "{what}: the token reached the output"
        );
    }
}

const OLD: &str = "2026.9.5";
const NEW: &str = "2026.9.9";

// ---------------------------------------------------------------------------
// Update: the happy path, and either key
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_feed_and_binary_update_stage_swap_restart_and_verify() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;

    let outcome = h
        .update(&rig.base, trust(&[&key_a(), &key_b()]))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        UpdateOutcome::Updated {
            from: OLD.into(),
            to: NEW.into(),
            key: key_a().id(),
        }
    );
    assert_eq!(h.live(), candidate, "the candidate is live");
    assert_eq!(
        h.prev().unwrap(),
        installed,
        ".prev holds the displaced binary"
    );
    assert!(!h.new_exists(), ".new was renamed away");
    assert_eq!(h.service.restarts(), 1);
    assert_eq!(h.served_version().as_deref(), Some(NEW));
    assert!(!h.output().contains(TOKEN), "{}", h.output());
    assert!(h.output().contains("Health OK"), "{}", h.output());
    assert!(
        h.install.sibling(".update.lock").exists(),
        "the lock file is left in place, never unlinked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_trusted_key_verifies_and_a_one_key_set_still_verifies() {
    // Key B signs; the set is {A, B}.
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_b()).await;
    let outcome = h
        .update(&rig.base, trust(&[&key_a(), &key_b()]))
        .await
        .unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { ref key, .. } if *key == key_b().id()));
    assert_eq!(h.live(), candidate);

    // A one-key set — the tree before the standby is committed — verifies
    // its own key's signatures; nothing about verification needs two.
    let h2 = Harness::new(&installed, "127.0.0.1").await;
    let rig2 = release_for(NEW, &candidate, &key_a()).await;
    let outcome = h2.update(&rig2.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { .. }));
    assert_eq!(h2.live(), candidate);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_third_key_is_refused_before_anything_is_downloaded_or_changed() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_c()).await;
    let err = h
        .update(&rig.base, trust(&[&key_a(), &key_b()]))
        .await
        .expect_err("refused");
    assert!(matches!(err, UpdateError::Rejected { .. }), "{err}");
    assert!(err.to_string().contains("does not trust"), "{err}");
    assert_eq!(err.exit_code(), 1);
    h.assert_untouched(&installed, "wrong key");
    assert!(rig.binary_requests().is_empty(), "{:?}", rig.requests());
}

// ---------------------------------------------------------------------------
// Update: every refusal is a refusal, with no mutation and no restart
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_feed_is_refused_before_decoding_and_before_any_binary_request() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let mut feed = rig.release.lock().unwrap().assets[FEED_ASSET].clone();
    // Point the hash at the installed bytes: a tampered feed that would read
    // as "already current" if its bytes were trusted before verification.
    let text = String::from_utf8(feed.clone()).unwrap();
    let text = text.replace(&sha256_hex(&candidate), &sha256_hex(&installed));
    feed = text.into_bytes();
    rig.set_asset(FEED_ASSET, feed);

    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(
        matches!(err, UpdateError::Rejected { ref object, .. } if object == FEED_ASSET),
        "{err}"
    );
    h.assert_untouched(&installed, "tampered feed");
    assert!(rig.binary_requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_binary_is_refused_before_it_touches_disk() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let mut evil = candidate.clone();
    evil.extend_from_slice(b"\n# one more line\n");
    rig.set_asset(&asset_name(NEW, target()), evil);

    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Rejected { .. }), "{err}");
    h.assert_untouched(&installed, "tampered binary");
    assert_eq!(
        rig.binary_requests().len(),
        1,
        "it was downloaded, once, into memory"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_signature_over_the_wrong_binary_is_refused_by_the_hash() {
    // The feed's entry carries a signature that verifies over the served
    // bytes, but a hash of some other binary: the entry and the file
    // disagree, and neither is trusted.
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let other = fake_agent(Some(NEW), "other");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let asset = asset_name(NEW, target());
    let doc = serde_json::json!({
        "version": NEW,
        "targets": { target(): {
            "url": format!("{}/releases/download/v{NEW}/{asset}", rig.base),
            "signature": key_a().sign(&candidate, &asset),
            "sha256": sha256_hex(&other),
        }}
    });
    let mut text = serde_json::to_string_pretty(&doc).unwrap();
    text.push('\n');
    rig.set_asset(
        &format!("{FEED_ASSET}.minisig"),
        key_a().sign(text.as_bytes(), FEED_ASSET).into_bytes(),
    );
    rig.set_asset(FEED_ASSET, text.into_bytes());

    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::HashMismatch { .. }), "{err}");
    h.assert_untouched(&installed, "hash mismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_feed_that_does_not_name_its_release_a_missing_target_and_a_malformed_feed_are_refused() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let t = trust(&[&key_a()]);

    // Signed feed for NEW, replayed onto a release tagged something else.
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    rig.set_tag("v2026.9.10");
    // Move the assets under the new tag too, so only the mismatch remains.
    let err = h.update(&rig.base, t).await.unwrap_err();
    assert!(matches!(err, UpdateError::FeedTag { .. }), "{err}");
    h.assert_untouched(&installed, "feed/tag mismatch");

    // No entry for this host.
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let doc = serde_json::json!({ "version": NEW, "targets": { "riscv64gc-unknown-linux-musl": {
        "url": format!("{}/releases/download/v{NEW}/solador-agent-{NEW}-riscv64gc-unknown-linux-musl", rig.base),
        "signature": key_a().sign(b"x", "y"), "sha256": "0".repeat(64) }}});
    let mut text = serde_json::to_string_pretty(&doc).unwrap();
    text.push('\n');
    rig.set_asset(
        &format!("{FEED_ASSET}.minisig"),
        key_a().sign(text.as_bytes(), FEED_ASSET).into_bytes(),
    );
    rig.set_asset(FEED_ASSET, text.into_bytes());
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::TargetMissing(_)), "{err}");
    h.assert_untouched(&installed, "missing target");

    // Verified bytes that are not a feed.
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let text = "{\"version\": \"2026.9.9\", \"targets\": {}}\n".to_string();
    rig.set_asset(
        &format!("{FEED_ASSET}.minisig"),
        key_a().sign(text.as_bytes(), FEED_ASSET).into_bytes(),
    );
    rig.set_asset(FEED_ASSET, text.into_bytes());
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::FeedMalformed(_)), "{err}");
    h.assert_untouched(&installed, "malformed feed");

    // A release with no feed at all (every release before #391).
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    rig.remove_asset(FEED_ASSET);
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Network { .. }), "{err}");
    assert!(err.to_string().contains("404"), "{err}");
    h.assert_untouched(&installed, "no feed");
}

/// The redirect policy on the wire, not only its predicate: a 302 onto a
/// host off loopback (the only kind a shipped binary could meet over plain
/// http) is refused with nothing changed, and a chain past ten hops is
/// refused too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redirect_off_loopback_or_past_ten_hops_is_refused() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    rig.redirect_downloads_to("http://example.invalid:1");
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Network { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("refusing to follow a redirect to http://"),
        "{err}"
    );
    h.assert_untouched(&installed, "off-loopback redirect");

    let too_many = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().clone();
            match update::redirect_allowed(
                true,
                url.scheme(),
                url.host_str(),
                attempt.previous().len(),
            ) {
                Ok(()) => attempt.follow(),
                Err(why) => attempt.error(why),
            }
        }))
        .build()
        .unwrap();
    let err = update::fetch_asset(&too_many, &format!("{}/hop/11", rig.base), 1024)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("too many redirects"), "{err}");
    let ok = update::fetch_asset(&too_many, &format!("{}/hop/3", rig.base), 1024)
        .await
        .unwrap();
    assert_eq!(ok, b"landed");
}

/// A body past the cap is refused before it is held — a feed the size of a
/// binary is not a feed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_feed_past_the_size_cap_is_refused_before_anything_is_verified() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    rig.set_asset(FEED_ASSET, vec![b'{'; 2 * 1024 * 1024]);
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Network { .. }), "{err}");
    assert!(err.to_string().contains("cap"), "{err}");
    h.assert_untouched(&installed, "oversized feed");
}

/// Discovery refuses a redirect that is not a release tag: a repository
/// with no published release answers with its releases page, and a tag
/// that is not `vCalVer` is not one this updater follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_refuses_a_redirect_that_is_not_a_release_tag() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    rig.set_tag("main");
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Discovery(_)), "{err}");
    assert!(err.to_string().contains("not a release tag"), "{err}");
    h.assert_untouched(&installed, "non-CalVer tag");
    assert_eq!(rig.requests(), vec!["/releases/latest".to_string()]);
}

/// Equal bytes that cannot name their version cannot be verified serving,
/// and "current" is not claimed for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_bytes_that_carry_no_version_are_refused_rather_than_called_current() {
    let installed = fake_agent(None, "versionless");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &installed, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(
        matches!(err, UpdateError::InstalledVersionUnknown { .. }),
        "{err}"
    );
    h.assert_untouched(&installed, "versionless current");
    assert!(rig.binary_requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_bytes_under_a_different_version_exit_zero_with_no_binary_request() {
    // The feed names a newer version, and its hash is the installed bytes'.
    let installed = fake_agent(Some(OLD), "same");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &installed, &key_a()).await;
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert_eq!(
        outcome,
        UpdateOutcome::AlreadyCurrent {
            version: NEW.into(),
            sha256: sha256_hex(&installed),
        }
    );
    h.assert_untouched(&installed, "already current");
    assert!(rig.binary_requests().is_empty(), "{:?}", rig.requests());
    assert_eq!(
        rig.requests(),
        vec![
            "/releases/latest".to_string(),
            format!("/releases/download/v{NEW}/{FEED_ASSET}"),
            format!("/blob/v{NEW}/{FEED_ASSET}"),
            format!("/releases/download/v{NEW}/{FEED_ASSET}.minisig"),
            format!("/blob/v{NEW}/{FEED_ASSET}.minisig"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_feed_that_is_not_newer_than_the_installed_version_is_refused() {
    // Different bytes, older version: a replayed feed, or the operator
    // reaching for a downgrade this command does not do.
    let installed = fake_agent(Some(NEW), "installed-new");
    let older = fake_agent(Some(OLD), "older");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(OLD, &older, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::NotNewer { .. }), "{err}");
    assert_eq!(
        err.exit_code(),
        4,
        "no applicable release is its own exit code"
    );
    h.assert_untouched(&installed, "not newer");
    assert!(
        rig.binary_requests().is_empty(),
        "no download for a refused version"
    );

    // Same version, different bytes: also not newer.
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &fake_agent(Some(NEW), "rebuilt"), &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::NotNewer { .. }), "{err}");
    h.assert_untouched(&installed, "same version");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_installed_binary_that_carries_no_version_is_refused_rather_than_assumed_older() {
    let installed = fake_agent(None, "versionless");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(
        matches!(err, UpdateError::InstalledVersionUnknown { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("install.sh"), "{err}");
    h.assert_untouched(&installed, "versionless install");
    assert!(rig.binary_requests().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_candidate_that_cannot_name_the_expected_version_is_removed_unrun_as_a_service() {
    let installed = fake_agent(Some(OLD), "old");
    // Verifies and hashes fine; answers --version with something else.
    let candidate = fake_agent(Some("2026.9.8"), "mislabelled");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Candidate { .. }), "{err}");
    assert!(err.to_string().contains("2026.9.8"), "{err}");
    h.assert_untouched(&installed, "candidate version");

    // And one that cannot answer at all.
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &fake_agent(None, "mute"), &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Candidate { .. }), "{err}");
    h.assert_untouched(&installed, "mute candidate");
}

// ---------------------------------------------------------------------------
// Update: the live path changed, and what happens after
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_version_after_restart_restores_prev_and_exits_non_zero() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service.set_mode(RestartMode::Sticky);
    let rig = release_for(NEW, &candidate, &key_a()).await;

    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::UpdateFailedRecovered {
            failure, restored, ..
        } => {
            assert!(
                failure.contains(&format!("reports version {OLD}, want {NEW}")),
                "{failure}"
            );
            assert!(failure.contains("main pid before restart"), "{failure}");
            assert_eq!(restored, OLD);
        }
        other => panic!("{other}"),
    }
    h.assert_recovered(&err, &installed, 2, 5);
    assert!(h.output().contains("UPDATE FAILED"), "{}", h.output());
    assert!(
        h.output().contains("Health OK (recovery)"),
        "{}",
        h.output()
    );
}

/// The repo's named invariant: an HTTP 200 whose body has no `version`
/// key never satisfies an expected-version check. A service that comes up
/// answering without one is a failed update, recovered like any other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_without_a_version_never_satisfies_the_check_and_is_rolled_back() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service.set_mode(RestartMode::UpWithoutVersion);
    let rig = release_for(NEW, &candidate, &key_a()).await;
    // The recovery's restart also comes up without a version (same mode),
    // so the recovery's own verification — which expects OLD — fails too:
    // exit 3, and the failure names the absent key both times.
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::UpdateFailedRecoveryFailed {
            failure, recovery, ..
        } => {
            assert!(failure.contains("reports no version"), "{failure}");
            assert!(recovery.contains("reports no version"), "{recovery}");
        }
        other => panic!("{other}"),
    }
    h.assert_recovered(&err, &installed, 2, 3);

    // And with a faithful recovery, the update is the only failure.
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service
        .set_mode(RestartMode::UpWithoutVersionThenFaithful);
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::UpdateFailedRecovered { failure, .. } => {
            assert!(failure.contains("reports no version"), "{failure}")
        }
        other => panic!("{other}"),
    }
    h.assert_recovered(&err, &installed, 2, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_that_never_comes_up_is_rolled_back_and_verified_on_the_previous_version() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    // Comes up serving nothing after the update; the recovery's restart is
    // faithful again — the previous binary works, the new one did not.
    h.service.set_mode(RestartMode::NeverUpThenFaithful);
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::UpdateFailedRecovered { failure, .. } => {
            assert!(failure.contains("HTTP 503"), "{failure}")
        }
        other => panic!("{other}"),
    }
    h.assert_recovered(&err, &installed, 2, 5);
    assert_eq!(h.served_version().as_deref(), Some(OLD));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_restart_is_rolled_back() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service.set_mode(RestartMode::FailFirst);
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::UpdateFailedRecovered { failure, .. } => {
            assert!(failure.contains("manager says no"), "{failure}")
        }
        other => panic!("{other}"),
    }
    h.assert_recovered(&err, &installed, 2, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recovery_that_also_fails_is_reported_as_both_failures_and_never_as_a_rollback() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service.set_mode(RestartMode::FailAlways);
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::UpdateFailedRecoveryFailed {
            failure,
            recovery,
            live,
            ..
        } => {
            assert!(failure.contains("manager says no"), "{failure}");
            assert!(recovery.contains("manager says no"), "{recovery}");
            assert!(
                live.starts_with(&format!("the previous binary ({OLD})")),
                "{live}"
            );
        }
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains("RECOVERY ALSO FAILED"));
    assert!(err.to_string().contains("At the live path now:"));
    // The previous bytes were put back even though the restart refused: the
    // known-failed candidate is not left installed.
    h.assert_recovered(&err, &installed, 2, 3);
}

/// The updater refuses to proceed as the metrics service itself — and the
/// check is the manager's word, so a fake that reports our own pid trips it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_updater_refuses_to_run_as_the_service_it_would_restart() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service.set_pid(Some(std::process::id()));
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::IsTheService { .. }), "{err}");
    h.assert_untouched(&installed, "is the service");
    assert!(rig.requests().is_empty());
    let err = h.rollback().await.unwrap_err();
    assert!(matches!(err, UpdateError::IsTheService { .. }), "{err}");
    // Some other pid is fine. Not a literal: inside a container this test
    // process can itself be pid 1.
    h.service
        .set_pid(Some(std::process::id().wrapping_add(7919)));
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { .. }));
}

/// A manager this process cannot reach is refused before any download —
/// not discovered at the restart, after the live path changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_service_manager_is_refused_before_any_request() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    h.service.set_reachable(false);
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Install(_)), "{err}");
    assert!(err.to_string().contains("cannot reach"), "{err}");
    h.assert_untouched(&installed, "unreachable manager");
    assert!(rig.requests().is_empty(), "{:?}", rig.requests());
    fs::write(h.install.sibling(".prev"), &candidate).unwrap();
    let err = h.rollback().await.unwrap_err();
    assert!(matches!(err, UpdateError::Install(_)), "{err}");
    assert_eq!(h.live(), installed);
}

/// "Already current" is a claim about a running service, not bytes on
/// disk: an earlier run interrupted between its swap and its restart leaves
/// the feed's bytes live with the old process still serving. That is
/// reported as a distinct failure — and nothing is restarted or downloaded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_bytes_that_the_service_is_not_serving_are_a_failure_not_a_success() {
    let installed = fake_agent(Some(NEW), "current");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &installed, &key_a()).await;
    // The endpoint still answers with an older version.
    h.serve(Some(OLD));
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    match &err {
        UpdateError::AlreadyCurrentNotServing {
            version, reason, ..
        } => {
            assert_eq!(version, NEW);
            assert!(
                reason.contains(&format!("reports version {OLD}, want {NEW}")),
                "{reason}"
            );
        }
        other => panic!("{other}"),
    }
    assert_eq!(err.exit_code(), 1);
    assert!(err.to_string().contains("Inspect:"), "{err}");
    h.assert_untouched(&installed, "current but not serving");
    assert!(rig.binary_requests().is_empty());

    // And the same bytes with the service down entirely.
    h.serve(None);
    *h.served.lock().unwrap() = Served {
        up: false,
        version: None,
    };
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(
        matches!(err, UpdateError::AlreadyCurrentNotServing { .. }),
        "{err}"
    );
    h.assert_untouched(&installed, "current but down");

    // Serving the bytes' own version: already current, exit 0 — even though
    // the feed's version differs from what the bytes claim, which is the
    // acceptance case for the hash rule.
    let installed = fake_agent(Some(OLD), "same");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &installed, &key_a()).await;
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::AlreadyCurrent { .. }));
    assert!(
        h.output().contains(&format!("reports version {OLD}")),
        "{}",
        h.output()
    );
    h.assert_untouched(&installed, "already current and serving");
}

/// A crashed earlier run can leave a stale `.new` and an older `.prev`; the
/// next update owns both (it holds the lock) and must not trip over either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_new_and_an_older_prev_do_not_stop_the_next_update() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    fs::write(h.install.sibling(".new"), b"garbage from a crashed run").unwrap();
    fs::write(
        h.install.sibling(".prev"),
        fake_agent(Some("2026.9.1"), "ancient"),
    )
    .unwrap();
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { .. }));
    assert_eq!(h.live(), candidate);
    assert_eq!(
        h.prev().unwrap(),
        installed,
        ".prev is the binary just displaced, not the ancient one"
    );
    assert!(!h.new_exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_competing_transaction_reports_busy_without_a_request_or_a_change() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    // The competing transaction is ANOTHER process's: the lock is held
    // through a plain handle whose note names a foreign pid, which is what
    // a second `update`/`rollback` on the host writes. (A note naming our
    // own pid is the stale-inherited-reference case `acquire` waits out.)
    let foreign_pid = std::process::id().wrapping_add(7919);
    let mut held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(h.install.sibling(".update.lock"))
        .unwrap();
    held.try_lock().unwrap();
    {
        use std::io::Write as _;
        writeln!(held, "pid={foreign_pid} since=1").unwrap();
        held.sync_all().unwrap();
    }

    let started = std::time::Instant::now();
    let err = h.update(&rig.base, trust(&[&key_a()])).await.unwrap_err();
    assert!(matches!(err, UpdateError::Busy { .. }), "{err}");
    assert_eq!(err.exit_code(), 75);
    assert!(
        err.to_string().contains(&format!("pid={foreign_pid}")),
        "the busy line names the holder: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "another process's lock is busy at once, never waited on"
    );
    h.assert_untouched(&installed, "busy");
    assert!(
        rig.requests().is_empty(),
        "busy is decided before any request"
    );

    let err = h.rollback().await.unwrap_err();
    assert!(matches!(err, UpdateError::Busy { .. }), "{err}");
    h.assert_untouched(&installed, "busy rollback");

    drop(held);
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_health_probe_dials_loopback_for_a_wildcard_bind() {
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "0.0.0.0").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { .. }));
    assert!(h.output().contains("http://127.0.0.1:"), "{}", h.output());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_health_probe_brackets_an_ipv6_bind() {
    // Best effort: a host with no ::1 cannot run this and says so.
    if tokio::net::TcpListener::bind("[::1]:0").await.is_err() {
        eprintln!("SKIP: no IPv6 loopback on this host");
        return;
    }
    let installed = fake_agent(Some(OLD), "old");
    let candidate = fake_agent(Some(NEW), "new");
    let h = Harness::new(&installed, "::").await;
    let rig = release_for(NEW, &candidate, &key_a()).await;
    let outcome = h.update(&rig.base, trust(&[&key_a()])).await.unwrap();
    assert!(matches!(outcome, UpdateOutcome::Updated { .. }));
    assert!(h.output().contains("http://[::1]:"), "{}", h.output());
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_restores_prev_keeps_the_displaced_binary_and_is_reversible() {
    let old = fake_agent(Some(OLD), "old");
    let new = fake_agent(Some(NEW), "new");
    let h = Harness::new(&new, "127.0.0.1").await;
    fs::write(h.install.sibling(".prev"), &old).unwrap();

    let out = h.rollback().await.unwrap();
    assert_eq!(out.restored_version.as_deref(), Some(OLD));
    assert_eq!(out.served_version.as_deref(), Some(OLD));
    assert_eq!(h.live(), old);
    assert_eq!(
        h.prev().unwrap(),
        new,
        "the displaced binary is the new .prev"
    );
    assert!(!h.new_exists());
    assert!(!h.install.sibling(".rollback-displaced").exists());
    assert_eq!(h.service.restarts(), 1);
    assert!(!h.output().contains(TOKEN));

    // Reversible: rolling back again rolls forward.
    let out = h.rollback().await.unwrap();
    assert_eq!(out.restored_version.as_deref(), Some(NEW));
    assert_eq!(h.live(), new);
    assert_eq!(h.prev().unwrap(), old);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_with_no_prev_refuses_without_touching_the_live_binary() {
    let installed = fake_agent(Some(NEW), "only");
    let h = Harness::new(&installed, "127.0.0.1").await;
    let err = h.rollback().await.unwrap_err();
    assert!(matches!(err, UpdateError::NoPrevious(_)), "{err}");
    h.assert_untouched(&installed, "no .prev");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_to_a_versionless_previous_binary_verifies_liveness_and_says_so() {
    let versionless = fake_agent(None, "source-built");
    let new = fake_agent(Some(NEW), "new");
    let h = Harness::new(&new, "127.0.0.1").await;
    fs::write(h.install.sibling(".prev"), &versionless).unwrap();
    let out = h.rollback().await.unwrap();
    assert_eq!(out.restored_version, None);
    assert_eq!(
        out.served_version, None,
        "the endpoint reports no version and that is fine"
    );
    assert_eq!(h.live(), versionless);
    assert!(h.output().contains("carries no version"), "{}", h.output());
    assert!(h.output().contains("back online"), "{}", h.output());
}

/// The rollback twin of the absent-version invariant: a `.prev` that names
/// its version is held to it, and a 200 without the key is not "back".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_to_a_versioned_prev_is_not_satisfied_by_an_answer_without_a_version() {
    let old = fake_agent(Some(OLD), "old");
    let new = fake_agent(Some(NEW), "new");
    let h = Harness::new(&new, "127.0.0.1").await;
    fs::write(h.install.sibling(".prev"), &old).unwrap();
    h.service.set_mode(RestartMode::UpWithoutVersion);
    let err = h.rollback().await.unwrap_err();
    match &err {
        UpdateError::RollbackUnhealthy { reason, inspect } => {
            assert!(reason.contains("reports no version"), "{reason}");
            assert!(inspect.contains("Inspect:"), "{inspect}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(h.live(), old, "the swap happened");
    assert_eq!(h.prev().unwrap(), new, "and is reversible");

    // And the mirror image: a versionless .prev answered by a process that
    // DOES report a version is the displaced process still holding the
    // socket, not the restored binary.
    let versionless = fake_agent(None, "source-built");
    let h = Harness::new(&new, "127.0.0.1").await;
    fs::write(h.install.sibling(".prev"), &versionless).unwrap();
    h.service.set_mode(RestartMode::Sticky);
    let err = h.rollback().await.unwrap_err();
    match &err {
        UpdateError::RollbackUnhealthy { reason, .. } => {
            assert!(
                reason.contains("displaced process still answering"),
                "{reason}"
            )
        }
        other => panic!("{other}"),
    }
}

/// A `.rollback-displaced` left by a half-done rollback is the displaced
/// binary's only copy; the next rollback must refuse rather than overwrite
/// it and report a clean swap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_refuses_to_overwrite_a_leftover_displaced_binary() {
    let old = fake_agent(Some(OLD), "old");
    let new = fake_agent(Some(NEW), "new");
    let orphan = fake_agent(Some("2026.9.1"), "orphan");
    let h = Harness::new(&new, "127.0.0.1").await;
    fs::write(h.install.sibling(".prev"), &old).unwrap();
    fs::write(h.install.sibling(".rollback-displaced"), &orphan).unwrap();
    let err = h.rollback().await.unwrap_err();
    assert!(matches!(err, UpdateError::RollbackHalfDone { .. }), "{err}");
    assert!(err.to_string().contains("left this file behind"), "{err}");
    assert_eq!(h.live(), new, "the live binary was not touched");
    assert_eq!(h.prev().unwrap(), old);
    assert_eq!(
        fs::read(h.install.sibling(".rollback-displaced")).unwrap(),
        orphan
    );
    assert!(!h.new_exists());
    assert_eq!(h.service.restarts(), 0);
    // Once the operator has moved it, rollback proceeds.
    fs::remove_file(h.install.sibling(".rollback-displaced")).unwrap();
    h.rollback().await.unwrap();
    assert_eq!(h.live(), old);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_that_does_not_come_back_is_reported_and_left_reversible() {
    let old = fake_agent(Some(OLD), "old");
    let new = fake_agent(Some(NEW), "new");
    let h = Harness::new(&new, "127.0.0.1").await;
    fs::write(h.install.sibling(".prev"), &old).unwrap();
    h.service.set_mode(RestartMode::NeverUp);
    let err = h.rollback().await.unwrap_err();
    assert!(
        matches!(err, UpdateError::RollbackUnhealthy { .. }),
        "{err}"
    );
    assert_eq!(h.live(), old, "the swap happened");
    assert_eq!(h.prev().unwrap(), new, "and is reversible");
}

// ---------------------------------------------------------------------------
// The real binary's dispatch, on every platform the suite runs on
// ---------------------------------------------------------------------------

/// `main.rs`'s wiring is exercised by executing the built binary: a HOME with
/// nothing installed is refused (exit 1, naming the installer) before any
/// request is made, and an argument the command does not take is refused as
/// usage (exit 2). Neither needs a service, a network, or a key.
#[test]
fn the_cli_refuses_an_empty_home_and_an_unknown_argument_before_doing_anything() {
    let real = PathBuf::from(env!("CARGO_BIN_EXE_solador-agent"));
    let home = tempfile::tempdir().unwrap();
    for cmd in ["update", "rollback"] {
        let out = Command::new(&real)
            .arg(cmd)
            .env("HOME", home.path())
            .env_remove("SOLADOR_AGENT_LAUNCHD_LABEL")
            .env_remove("SOLADOR_AGENT_TOKEN")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{cmd}: {stderr}");
        assert!(stderr.starts_with("ERROR: "), "{cmd}: {stderr}");
        // Root is refused before the install is even looked for; a CI
        // runner is not root, a container often is, and both are honest.
        if unsafe { libc::geteuid() } == 0 {
            assert!(stderr.contains("running as root"), "{cmd}: {stderr}");
        } else {
            assert!(stderr.contains("install.sh"), "{cmd}: {stderr}");
        }
        assert!(
            fs::read_dir(home.path()).unwrap().next().is_none(),
            "{cmd} wrote into an empty HOME"
        );
    }
    for args in [
        vec!["update", "--force"],
        vec!["rollback", "now"],
        vec!["update", "rollback"],
    ] {
        let out = Command::new(&real)
            .args(&args)
            .env("HOME", home.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("unrecognized argument"),
            "{args:?}: {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// The install contract on the real service files of this OS
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn resolve_install_reads_the_systemd_unit_install_sh_renders() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join(".local/bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let binary = bin_dir.join("solador-agent");
    fs::write(&binary, fake_agent(Some(OLD), "x")).unwrap();
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).unwrap();
    let template = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/solador-agent.service"),
    )
    .unwrap();
    fs::write(
        unit_dir.join("solador-agent.service"),
        template.replace("@SOLADOR_AGENT_BIN@", &binary.to_string_lossy()),
    )
    .unwrap();
    let install = update::resolve_install(home.path(), update::LAUNCHD_LABEL).unwrap();
    assert_eq!(install.binary, binary);
    assert_eq!(
        install.env_file,
        home.path().join(".config/solador-agent.env")
    );
    assert_eq!(
        install.service,
        Service::Systemd {
            unit: "solador-agent".into()
        }
    );

    // The pre-#392 layout: ExecStart somewhere this user cannot write.
    fs::write(
        unit_dir.join("solador-agent.service"),
        template.replace("@SOLADOR_AGENT_BIN@", "/opt/solador-agent/solador-agent"),
    )
    .unwrap();
    let err = update::resolve_install(home.path(), update::LAUNCHD_LABEL).unwrap_err();
    assert!(matches!(err, UpdateError::Install(_)), "{err}");
    assert!(err.to_string().contains("install.sh"), "{err}");
}

#[cfg(target_os = "macos")]
#[test]
fn resolve_install_reads_the_launchagent_plist_install_sh_renders() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join(".local/bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let binary = bin_dir.join("solador-agent");
    fs::write(&binary, fake_agent(Some(OLD), "x")).unwrap();
    let plist_dir = home.path().join("Library/LaunchAgents");
    fs::create_dir_all(&plist_dir).unwrap();
    let label = "app.solador.agent.resolvetest";
    let env_file = home.path().join(".config/solador-agent.env");
    fs::write(
        plist_dir.join(format!("{label}.plist")),
        render_plist(
            label,
            &bin_dir.join("solador-agent-launchd"),
            &binary,
            &env_file,
            &home.path().join("log"),
        ),
    )
    .unwrap();
    let install = update::resolve_install(home.path(), label).unwrap();
    assert_eq!(install.binary, binary);
    assert_eq!(install.env_file, env_file);
    assert!(matches!(install.service, Service::Launchd { label: ref l, .. } if l == label));
    assert!(update::resolve_install(home.path(), "bad/label").is_err());
}

#[test]
fn an_unwritable_install_directory_is_refused_with_the_migration_step() {
    // Root can write anywhere; the refusal is meaningless there.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: running as root");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("opt");
    fs::create_dir_all(&bin_dir).unwrap();
    let binary = bin_dir.join("solador-agent");
    fs::write(&binary, fake_agent(Some(OLD), "x")).unwrap();
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let install = Install {
        binary: binary.clone(),
        env_file: home.path().join("env"),
        service: Service::Systemd {
            unit: "solador-agent".into(),
        },
        log: None,
    };
    let err = update::check_install_replaceable(&install).unwrap_err();
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(err.to_string().contains("--migrate-from-opt"), "{err}");
    assert!(err.to_string().contains("never takes"), "{err}");
}

#[cfg(target_os = "macos")]
fn render_plist(
    label: &str,
    launcher: &Path,
    binary: &Path,
    env_file: &Path,
    log: &Path,
) -> String {
    let template = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/app.solador.agent.plist"),
    )
    .unwrap();
    let esc = |p: &Path| {
        p.to_string_lossy()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    template
        .replace("@LABEL@", label)
        .replace("@LAUNCHER@", &esc(launcher))
        .replace("@BINARY@", &esc(binary))
        .replace("@ENV_FILE@", &esc(env_file))
        .replace("@LOG_FILE@", &esc(log))
}

// ---------------------------------------------------------------------------
// The real thing: a throwaway LaunchAgent, the real built agent, real
// launchctl, a failed update rolled back automatically. Opt-in, macOS only.
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launchd_smoke() {
    if std::env::var("SOLADOR_DEPLOY_TEST_LAUNCHD").as_deref() != Ok("1") {
        eprintln!("SKIP launchd_smoke: opt-in; set SOLADOR_DEPLOY_TEST_LAUNCHD=1 on macOS");
        return;
    }
    let uid = unsafe { libc::getuid() };
    if !Command::new("launchctl")
        .args(["print", &format!("gui/{uid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        eprintln!("SKIP launchd_smoke: no gui launchd domain for uid {uid}");
        return;
    }
    let real = PathBuf::from(env!("CARGO_BIN_EXE_solador-agent"));
    let Ok(real_version) = update::binary_version(&real, Duration::from_secs(10)) else {
        eprintln!(
            "SKIP launchd_smoke: {} carries no version (shallow checkout)",
            real.display()
        );
        return;
    };

    // --- a disposable install under a temporary HOME ------------------------
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join(".local/bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let binary = bin_dir.join("solador-agent");
    fs::copy(&real, &binary).unwrap();
    let launcher = bin_dir.join("solador-agent-launchd");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("deploy/run-agent.sh"),
        &launcher,
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let env_file = home.path().join(".config/solador-agent.env");
    fs::create_dir_all(env_file.parent().unwrap()).unwrap();
    fs::write(
        &env_file,
        format!("SOLADOR_AGENT_TOKEN={TOKEN}\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT={port}\n"),
    )
    .unwrap();
    fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600)).unwrap();
    let log = home.path().join("Library/Logs/solador-agent.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    let label = format!("app.solador.agent.updatetest.{}", std::process::id());
    let plist = home
        .path()
        .join(format!("Library/LaunchAgents/{label}.plist"));
    fs::create_dir_all(plist.parent().unwrap()).unwrap();
    fs::write(
        &plist,
        render_plist(&label, &launcher, &binary, &env_file, &log),
    )
    .unwrap();
    let service_id = format!("gui/{uid}/{label}");
    let bootout = || {
        let _ = Command::new("launchctl")
            .args(["bootout", &service_id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    let booted = Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}")])
        .arg(&plist)
        .status()
        .unwrap()
        .success();
    assert!(booted, "launchctl bootstrap {service_id} failed");

    let install = update::resolve_install(home.path(), &label).unwrap();
    assert_eq!(install.binary, binary);
    let serving = update::read_serving(&install.env_file).unwrap();
    let health = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let up = update::wait_for_health(
        &health,
        &serving,
        &Expect::Version(real_version.clone()),
        30,
        Duration::from_secs(1),
        &mut |l: &str| println!("{l}"),
    )
    .await;
    if up.is_err() {
        bootout();
        panic!("the real agent did not come up under launchd: {up:?}");
    }

    let lines = Arc::new(Mutex::new(Vec::<String>::new()));

    // --- 1. A failed update, rolled back automatically on the real service ---
    // The candidate names a newer version to `--version`, verifies and
    // hashes, and once launchd starts it, it execs the real agent — which
    // serves the REAL version. Health never reports the expected one, so the
    // updater must restore .prev and bring the real agent back.
    let candidate = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '2099.1.1\\n'; exit 0; fi\nexec '{}' \"$@\"\n",
        real.display()
    )
    .into_bytes();
    let rig = release_for("2099.1.1", &candidate, &key_a()).await;
    let service = Service::Launchd {
        label: label.clone(),
        uid,
    };
    let result = {
        let lines = lines.clone();
        let mut report = move |l: &str| {
            println!("{l}");
            lines.lock().unwrap().push(l.to_string());
        };
        let mut ctx = Context {
            release_base: rig.base.clone(),
            trust: trust(&[&key_a(), &key_b()]),
            install: install.clone(),
            serving: update::read_serving(&env_file).unwrap(),
            service: &service,
            target: target(),
            running_version: Some(real_version.clone()),
            health_attempts: 20,
            health_interval: Duration::from_secs(1),
            report: &mut report,
        };
        update::run_update(&mut ctx).await
    };
    let real_bytes = fs::read(&real).unwrap();
    let live_after = fs::read(&binary).unwrap();
    let served_after = update::wait_for_health(
        &health,
        &serving,
        &Expect::Version(real_version.clone()),
        20,
        Duration::from_secs(1),
        &mut |l: &str| println!("{l}"),
    )
    .await;

    // --- 2. A successful update on the real service ---------------------------
    // Needs a second real agent binary with a newer CalVer — built with
    // `MARKETING_VERSION=<newer> cargo build -p solador-agent --target-dir …`
    // and named by SOLADOR_AGENT_SMOKE_NEWER_BINARY. Optional, because a
    // second build inside a test is not something CI or a casual run should
    // pay for; without it this section is skipped and says so.
    let newer = std::env::var_os("SOLADOR_AGENT_SMOKE_NEWER_BINARY").map(PathBuf::from);
    let newer_version = newer
        .as_deref()
        .and_then(|p| update::binary_version(p, Duration::from_secs(10)).ok());
    let success = match (&newer, &newer_version) {
        (Some(path), Some(version)) => {
            let bytes = fs::read(path).unwrap();
            let rig = release_for(version, &bytes, &key_b()).await;
            let lines = lines.clone();
            let mut report = move |l: &str| {
                println!("{l}");
                lines.lock().unwrap().push(l.to_string());
            };
            let mut ctx = Context {
                release_base: rig.base.clone(),
                trust: trust(&[&key_a(), &key_b()]),
                install: install.clone(),
                serving: update::read_serving(&env_file).unwrap(),
                service: &service,
                target: target(),
                running_version: Some(real_version.clone()),
                health_attempts: 20,
                health_interval: Duration::from_secs(1),
                report: &mut report,
            };
            let result = update::run_update(&mut ctx).await;
            let live = fs::read(&binary).unwrap();
            Some((result, live == bytes, version.clone()))
        }
        _ => {
            println!("SKIP launchd_smoke success path: set SOLADOR_AGENT_SMOKE_NEWER_BINARY to a versioned agent binary newer than {real_version}");
            None
        }
    };

    // --- 3. Explicit rollback through the REAL CLI, offline, twice ----------
    // The CLI resolves the plist with plutil, reads the env file, swaps,
    // kickstarts and verifies — main.rs's wiring, on a real LaunchAgent.
    // With the success path above, the first rollback returns to the real
    // build and the second rolls forward to the newer one; without it both
    // swap identical bytes (the recovery left .prev equal to the live path).
    let run_cli = |cmd: &str| {
        let out = Command::new(&real)
            .arg(cmd)
            .env("HOME", home.path())
            .env("SOLADOR_AGENT_LAUNCHD_LABEL", &label)
            .env_remove("SOLADOR_AGENT_TOKEN")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        println!(
            "--- solador-agent {cmd} (exit {:?}) ---\n{stdout}{stderr}",
            out.status.code()
        );
        (out.status, stdout, stderr)
    };
    let cli = run_cli("rollback");
    let served_after_cli = update::wait_for_health(
        &health,
        &serving,
        &Expect::Version(real_version.clone()),
        20,
        Duration::from_secs(1),
        &mut |l: &str| println!("{l}"),
    )
    .await;
    let cli_forward = run_cli("rollback");
    let served_after_forward = update::wait_for_health(
        &health,
        &serving,
        &Expect::Version(
            newer_version
                .clone()
                .unwrap_or_else(|| real_version.clone()),
        ),
        20,
        Duration::from_secs(1),
        &mut |l: &str| println!("{l}"),
    )
    .await;

    // --- 3. The REAL feed, read-only, through the REAL CLI -------------------
    // Opt-in on top of opt-in: it reaches github.com. The installed agent is
    // this build, whose bytes are not a published release's, so the honest
    // outcomes are "not newer" (this checkout is at or past the published
    // CalVer) or a real update — and a real update would put a published
    // binary on a throwaway service, which is fine, but is not what this
    // asserts. It asserts the read-only path: discovery, feed verification
    // under the COMPILED-IN production key, target selection, hash, version
    // rule, and no mutation.
    let real_feed = if std::env::var("SOLADOR_AGENT_SMOKE_REAL_FEED").as_deref() == Ok("1") {
        let before = fs::read(&binary).unwrap();
        let out = Command::new(&real)
            .arg("update")
            .env("HOME", home.path())
            .env("SOLADOR_AGENT_LAUNCHD_LABEL", &label)
            .env_remove("SOLADOR_AGENT_TOKEN")
            .output()
            .unwrap();
        let after = fs::read(&binary).unwrap();
        Some((out, before == after))
    } else {
        None
    };

    // Whatever happened, take the throwaway service down. `bootout` returns
    // before the service is fully torn down on some releases (install.sh
    // records the same race), so the "unloaded" check polls rather than
    // reading once.
    let is_loaded = || {
        Command::new("launchctl")
            .args(["print", &service_id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let loaded = is_loaded();
    bootout();
    let mut still_loaded = is_loaded();
    for _ in 0..20 {
        if !still_loaded {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
        still_loaded = is_loaded();
    }

    // --- assertions, after cleanup ------------------------------------------
    let output = lines.lock().unwrap().join("\n");
    assert!(
        loaded,
        "the throwaway LaunchAgent was loaded during the smoke"
    );
    assert!(!still_loaded, "the throwaway service was unloaded again");
    match &result {
        Err(UpdateError::UpdateFailedRecovered {
            failure, restored, ..
        }) => {
            assert!(failure.contains("2099.1.1"), "{failure}");
            assert_eq!(restored, &real_version);
        }
        other => {
            panic!("expected a recovered failure on the real service, got {other:?}\n{output}")
        }
    }
    assert_eq!(
        live_after, real_bytes,
        "the real agent is back at the live path"
    );
    assert!(
        served_after.is_ok(),
        "the real agent serves its version again: {served_after:?}"
    );
    assert!(!output.contains(TOKEN), "{output}");
    assert!(output.contains("UPDATE FAILED"), "{output}");
    assert!(output.contains("Restarting the LaunchAgent"), "{output}");

    if let Some((result, live_is_newer, version)) = success {
        match &result {
            Ok(UpdateOutcome::Updated { from, to, key }) => {
                assert_eq!(from, &real_version);
                assert_eq!(to, &version);
                assert_eq!(key, &key_b().id());
            }
            other => {
                panic!("expected a successful update on the real service, got {other:?}\n{output}")
            }
        }
        assert!(live_is_newer, "the newer real binary is at the live path");
        assert!(
            output.contains(&format!(
                "Health OK: http://127.0.0.1:{port}/v1/health reports version {version}"
            )),
            "{output}"
        );
    }

    let (status, cli_stdout, cli_stderr) = &cli;
    assert!(
        status.success(),
        "solador-agent rollback exited {:?}\n{cli_stdout}{cli_stderr}",
        status.code()
    );
    assert!(cli_stdout.contains("Done: rolled back"), "{cli_stdout}");
    assert!(!cli_stdout.contains(TOKEN) && !cli_stderr.contains(TOKEN));
    assert!(served_after_cli.is_ok(), "{served_after_cli:?}");
    let (status, fwd_stdout, fwd_stderr) = &cli_forward;
    assert!(
        status.success(),
        "second rollback exited {:?}\n{fwd_stdout}{fwd_stderr}",
        status.code()
    );
    assert!(fwd_stdout.contains("Done: rolled back"), "{fwd_stdout}");
    assert!(!fwd_stdout.contains(TOKEN) && !fwd_stderr.contains(TOKEN));
    assert!(served_after_forward.is_ok(), "{served_after_forward:?}");

    if let Some((out, unchanged)) = real_feed {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        println!("--- real feed ---\n{stdout}{stderr}");
        assert!(
            unchanged,
            "the read-only real-feed smoke must not change the live binary"
        );
        assert!(
            stdout.contains("agent-latest.json verified under key B2E5C62B763FD2C4"),
            "the real feed verified under the compiled-in production key:\n{stdout}{stderr}"
        );
        assert!(
            stderr.contains("not newer") || stdout.contains("Already current"),
            "{stdout}{stderr}"
        );
        assert!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
    }
}
