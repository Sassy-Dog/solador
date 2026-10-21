//! `host add --ssh` end to end: a stand-in `ssh` runs the embedded script under
//! `/bin/sh` against a temporary HOME, and a real hand-off server (a fake store
//! and a fake agent client behind it) answers on the other side of the socket.
//!
//! Nothing here reaches a real host, service manager or agent. The stand-in
//! starts the script with `env -i` and a PATH whose first directory stubs every
//! command that could (`systemctl`, `launchctl`, `ps`, `tailscale`, `ip`,
//! `curl`, `uname`, `id`, `hostname`), and the script's two absolute system
//! paths, `/Library/LaunchDaemons` and `/Applications/Tailscale.app`, are
//! rewritten into the temporary tree before it runs.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostsetup::channel;
use hostsetup::cli::{self, exit, AddArgs, Command};
use hostsetup::pairing::{Decision, Pairing};
use hostsetup::protocol::ProbeSummary;
use hostsetup::remote::{self, Ssh};
use hostsetup::server::{Agent, HostBook, ServerInfo};
use hostsetup::Secret;
use store::Host;
use uuid::Uuid;

const PIN: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";
const OTHER: &str = "11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11";
/// The agent's token, and the one a fresh install generates. Neither may
/// appear in any argv, log or output; both must reach the store.
const TOKEN: &str = "tok-4f2a-never-shown";
const GENERATED: &str = "gen-7c1e-never-shown";
const TARGET: &str = "svc@agent-1";
const LAN: &str = "192.168.1.2";
const TAILNET: &str = "100.64.0.9";
/// Long enough for every stand-in to answer at once, short enough that an
/// agent that never comes back costs the test a fraction of a second.
const SETTLE: Duration = Duration::from_millis(300);
const BOOTSTRAP_URL: &str =
    "https://raw.githubusercontent.com/Sassy-Dog/solador/main/agent/deploy/bootstrap.sh";

/// The app's hosts and tokens, as the server tests keep them.
#[derive(Default)]
struct Book {
    hosts: Mutex<Vec<Host>>,
    tokens: Mutex<Vec<(Uuid, String)>>,
}

impl HostBook for Book {
    fn hosts(&self) -> Vec<Host> {
        self.hosts.lock().unwrap().clone()
    }
    fn token_matches(&self, id: Uuid, token: &Secret) -> bool {
        self.tokens
            .lock()
            .unwrap()
            .iter()
            .any(|(i, t)| *i == id && t == token.expose())
    }
    fn apply(&self, pairing: &Pairing<'_>, decided: &Decision) -> Result<Host, String> {
        let mut hosts = self.hosts.lock().unwrap();
        pairing.confirm(&hosts, decided)?;
        let mut tokens = self.tokens.lock().unwrap();
        match *decided {
            Decision::Add => {
                let mut host = Host::new(pairing.name, pairing.address);
                host.port = pairing.port;
                host.tls_fingerprint = Some(pairing.pin.into());
                hosts.push(host.clone());
                tokens.push((host.id, pairing.token.expose().into()));
                Ok(host)
            }
            Decision::SamePairing { id } | Decision::Replace { id } => {
                let host = hosts.iter_mut().find(|h| h.id == id).ok_or("gone")?;
                host.address = pairing.address.into();
                host.port = pairing.port;
                host.tls_fingerprint = Some(pairing.pin.into());
                tokens.retain(|(i, _)| *i != id);
                tokens.push((id, pairing.token.expose().into()));
                Ok(host.clone())
            }
            Decision::Refused(_) => unreachable!("never applied"),
        }
    }
}

/// What answers at the address: a certificate (or, with `None`, plain HTTP),
/// and an agent that accepts the token its own env file holds, read when it
/// is asked.
struct FakeAgent {
    presents: Option<String>,
    env_file: PathBuf,
}

impl Agent for FakeAgent {
    async fn probe(&self, _address: &str, _port: u16) -> ProbeSummary {
        match &self.presents {
            Some(fingerprint) => ProbeSummary::Tls {
                fingerprint: fingerprint.clone(),
            },
            None => ProbeSummary::NoTls,
        }
    }
    async fn health(
        &self,
        _address: &str,
        _port: u16,
        pin: &str,
        token: &Secret,
    ) -> Result<String, String> {
        let env = fs::read_to_string(&self.env_file).unwrap_or_default();
        let held = env
            .lines()
            .filter_map(|line| line.strip_prefix("SOLADOR_AGENT_TOKEN="))
            .next_back();
        if Some(pin) == self.presents.as_deref() && held == Some(token.expose()) {
            Ok("✓ agent-1 · agent v2026.10.18".into())
        } else {
            Err("✗ auth failed (401) — check token".into())
        }
    }
}

/// The stand-in ssh. `-G` answers from a fixture; anything else must be the
/// one remote command shape `remote` sends, which it runs with the script
/// from stdin. The two exported settings stand for a login shell's rc file:
/// the script must not let them decide anything. `before-<mode>` and
/// `after-<mode>`, when present, are sourced around that mode: an `exit 255`
/// there is the connection going away, before or after the mode ran on the
/// host, and anything else is the host changing or answering under the
/// command.
const SSH: &str = r#"#!/bin/sh
ROOT='@ROOT@'
printf '%s\n' "$*" >> "$ROOT/ssh.log"
if [ -f "$ROOT/ssh-down" ]; then
    echo "Host key verification failed." >&2
    exit 255
fi
for arg in "$@"; do
    if [ "$arg" = -G ]; then cat "$ROOT/ssh-G"; exit 0; fi
done
while [ "$#" -gt 0 ] && [ "$1" != -- ]; do shift; done
shift 2
[ "$1" = /bin/sh ] || { echo "stand-in ssh: unexpected remote command: $*" >&2; exit 98; }
shift
mode=$3
[ -f "$ROOT/before-$mode" ] && . "$ROOT/before-$mode"
cat > "$ROOT/script.sent"
sed -e "s#/Library/LaunchDaemons#$ROOT/sys#" \
    -e "s#/Applications/Tailscale.app#$ROOT/Tailscale.app#" "$ROOT/script.sent" |
    env -i HOME="$ROOT/home" PATH="$ROOT/bin:/usr/bin:/bin" ROOT="$ROOT" \
        SOLADOR_AGENT_REQUIRE_TAILNET=0 SOLADOR_AGENT_BIND=10.9.9.9 \
        /bin/sh "$@"
status=$?
[ -f "$ROOT/after-$mode" ] && . "$ROOT/after-$mode"
exit "$status"
"#;

/// Every host command the script reaches that is not a plain text tool.
const STUBS: &[(&str, &str)] = &[
    ("uname", r#"cat "$ROOT/os""#),
    ("hostname", "echo agent-1.lan"),
    (
        "id",
        r#"case "$1 ${2-}" in
    "-un ") echo svc ;;
    "-un 501") echo svc ;;
    "-un 502") echo sassydog-agents ;;
    "-u ") echo 501 ;;
    *) exit 1 ;;
esac"#,
    ),
    ("ps", r#"cat "$ROOT/ps" 2>/dev/null; true"#),
    (
        "systemctl",
        r#"echo "systemctl $*" >> "$ROOT/calls"
[ ! -f "$ROOT/systemctl-fails" ]"#,
    ),
    (
        "launchctl",
        r#"echo "launchctl $*" >> "$ROOT/calls"
case "$1 $2" in "print gui/"*) [ -f "$ROOT/gui" ] ;; *) true ;; esac"#,
    ),
    (
        "tailscale",
        r#"[ -f "$ROOT/tailnet" ] && cat "$ROOT/tailnet""#,
    ),
    (
        "ip",
        r#"[ "$*" = "-4 -o addr show tailscale0" ] && cat "$ROOT/ip-addr" 2>/dev/null
true"#,
    ),
    (
        "curl",
        r#"echo "curl $*" >> "$ROOT/calls"
out=""
url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) out=$2; shift ;;
        *) url=$1 ;;
    esac
    shift
done
[ -n "$out" ] && [ "$url" = "@URL@" ] || exit 22
cp "$ROOT/bootstrap" "$out""#,
    ),
    ("minisign", "true"),
];

/// The agent binary: its version, and the fingerprint of the certificate it
/// has (none until a TLS agent has started; exit 2 from a pre-TLS release).
const AGENT: &str = r#"#!/bin/sh
case "$1" in
    --version) echo 2026.10.18 ;;
    tls-fingerprint)
        [ -f "$ROOT/pre-tls" ] && exit 2
        [ -f "$ROOT/fingerprint" ] || exit 1
        cat "$ROOT/fingerprint" ;;
    *) exit 2 ;;
esac
"#;

/// Stands in for bootstrap.sh → install.sh: records how it was run, then
/// leaves what a TLS install leaves. Like the real one, it never prints the
/// token. `installer-fails` has it stop with an error, as a failed download
/// or signature check does; `release-pre-tls` has it install a release from
/// before TLS (no TLS line, no certificate); `installer-writes-no-cert`, an
/// agent that never started; `installer-binds`, a bind of its own choosing.
const BOOTSTRAP: &str = r#"#!/bin/bash
{
    echo "bootstrap $*"
    echo "bootstrap bind=${SOLADOR_AGENT_BIND-unset} require=${SOLADOR_AGENT_REQUIRE_TAILNET-unset}"
    if IFS= read -r _; then echo "bootstrap stdin=data"; else echo "bootstrap stdin=closed"; fi
} >> "$ROOT/calls"
if [ -f "$ROOT/installer-fails" ]; then
    echo "error: signature check failed (stand-in installer)" >&2
    exit 1
fi
env_file="$HOME/.config/solador-agent.env"
mkdir -p "$HOME/.config/systemd/user" "$HOME/.local/bin"
token=$(sed -n 's/^SOLADOR_AGENT_TOKEN=//p' "$env_file" 2>/dev/null | tail -n 1)
[ -n "$token" ] || token=@GENERATED@
bind=${SOLADOR_AGENT_BIND:-$(sed -n 's/^SOLADOR_AGENT_BIND=//p' "$env_file" 2>/dev/null | tail -n 1)}
[ -n "$bind" ] || bind=0.0.0.0
[ -f "$ROOT/installer-binds" ] && bind=$(cat "$ROOT/installer-binds")
umask 077
printf 'SOLADOR_AGENT_TOKEN=%s\nSOLADOR_AGENT_BIND=%s\n' "$token" "$bind" > "$env_file"
if [ -f "$ROOT/release-pre-tls" ]; then
    : > "$ROOT/pre-tls"
else
    rm -f "$ROOT/pre-tls"
    echo SOLADOR_AGENT_TLS=1 >> "$env_file"
    [ -f "$ROOT/installer-writes-no-cert" ] || echo '@PIN@' > "$ROOT/fingerprint"
fi
cp "$ROOT/agent" "$HOME/.local/bin/solador-agent"
chmod 755 "$HOME/.local/bin/solador-agent"
: > "$HOME/.config/systemd/user/solador-agent.service"
echo "==> Installed (stand-in installer)"
"#;

struct World {
    // Dropped first: the server stops before its directory goes.
    rt: Option<tokio::runtime::Runtime>,
    book: Arc<Book>,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

fn write(path: &Path, content: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

impl World {
    /// A Linux host with no agent, reached at the LAN address, and no app.
    fn new() -> World {
        // Short: macOS's temp dir alone is most of `sun_path`.
        let dir = tempfile::Builder::new()
            .prefix("hs")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bake = |text: &str| {
            text.replace("@ROOT@", root.to_str().unwrap())
                .replace("@URL@", BOOTSTRAP_URL)
                .replace("@GENERATED@", GENERATED)
                .replace("@PIN@", PIN)
        };
        write(&root.join("ssh"), &bake(SSH), 0o755);
        for (name, body) in STUBS {
            write(
                &root.join("bin").join(name),
                &bake(&format!("#!/bin/sh\n{body}\n")),
                0o755,
            );
        }
        write(&root.join("agent"), AGENT, 0o755);
        write(&root.join("bootstrap"), &bake(BOOTSTRAP), 0o755);
        write(&root.join("os"), "Linux\n", 0o644);
        write(
            &root.join("ssh-G"),
            &format!("user svc\nhostname {LAN}\nport 22\n"),
            0o644,
        );
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("sys")).unwrap();
        fs::create_dir_all(root.join("s")).unwrap();
        World {
            rt: None,
            book: Arc::new(Book::default()),
            root,
            _dir: dir,
        }
    }

    fn home(&self, rel: &str) -> PathBuf {
        self.root.join("home").join(rel)
    }

    fn env_file(&self) -> PathBuf {
        self.home(".config/solador-agent.env")
    }

    fn store(&self) -> PathBuf {
        self.root.join("s")
    }

    /// Solador, running, with `presents` the certificate at the address.
    fn open_app(self, presents: &str) -> World {
        self.serve(Some(presents.into()))
    }

    /// Solador, running, and only plain HTTP answering at the address: an
    /// agent that never came back over TLS.
    fn open_app_without_tls(self) -> World {
        self.serve(None)
    }

    fn serve(mut self, presents: Option<String>) -> World {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let listener = {
            let _entered = rt.enter();
            channel::bind(&channel::socket_path(&self.store())).unwrap()
        };
        let agent = Arc::new(FakeAgent {
            presents,
            env_file: self.env_file(),
        });
        rt.spawn(channel::serve(
            listener,
            self.book.clone(),
            agent,
            ServerInfo::default(),
        ));
        self.rt = Some(rt);
        self
    }

    /// An installed agent for `svc`: this env file (0600), its binary, the
    /// certificate it serves, and the service that runs it.
    fn agent(self, env: &str, service: Service) -> World {
        write(&self.env_file(), env, 0o600);
        write(&self.home(".local/bin/solador-agent"), AGENT, 0o755);
        write(&self.root.join("fingerprint"), &format!("{PIN}\n"), 0o644);
        match service {
            Service::SystemdUser => {
                write(
                    &self.home(".config/systemd/user/solador-agent.service"),
                    "",
                    0o644,
                );
            }
            Service::LaunchAgent => {
                write(&self.root.join("os"), "Darwin\n", 0o644);
                write(&self.root.join("gui"), "", 0o644);
                write(
                    &self.home("Library/LaunchAgents/app.solador.agent.plist"),
                    "",
                    0o644,
                );
            }
            Service::LaunchDaemon => {
                write(&self.root.join("os"), "Darwin\n", 0o644);
                write(
                    &self.root.join("sys/app.solador.agent.plist"),
                    "<dict>\n\t<key>Label</key>\n\t<string>app.solador.agent</string>\n\
                     \t<key>UserName</key>\n\t<string>svc</string>\n</dict>\n",
                    0o644,
                );
            }
        }
        self
    }

    fn with(self, rel: &str, content: &str) -> World {
        write(&self.root.join(rel), content, 0o644);
        self
    }

    fn without(self, rel: &str) -> World {
        fs::remove_file(self.root.join(rel)).unwrap();
        self
    }

    fn add(&self, flags: &str) -> (i32, String, String) {
        let line = format!("host add --ssh {TARGET} {flags}");
        let argv: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        let Ok(Command::Add(args)) = cli::parse(&argv) else {
            panic!("{line} does not parse as an add");
        };
        self.run(&args)
    }

    fn run(&self, args: &AddArgs) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = remote::add_with_settle(
            args,
            &Ssh::with_program(self.root.join("ssh")),
            &self.store(),
            &mut out,
            &mut err,
            SETTLE,
        );
        let (out, err) = (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        );
        self.assert_no_token_shown(&out, &err);
        (code, out, err)
    }

    /// One mode of the script, through the stand-in ssh exactly as `remote`
    /// sends it, with nothing of `remote`'s own checks in front: the script's
    /// guards, alone.
    fn script(&self, mode: &[&str]) -> (Option<i32>, String, String) {
        use std::io::Write as _;
        let mut child = std::process::Command::new(self.root.join("ssh"))
            .args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"])
            .args(["-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15"])
            .args(["-o", "ServerAliveCountMax=3"])
            .args(["-T", "--", TARGET, "/bin/sh", "-s", "--"])
            .args(mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(remote::SCRIPT.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    /// Every ssh invocation, as argv.
    fn ssh(&self) -> Vec<String> {
        self.read("ssh.log").lines().map(str::to_owned).collect()
    }

    /// The script mode of every remote run, in order.
    fn modes(&self) -> Vec<String> {
        self.ssh()
            .iter()
            .filter_map(|line| {
                line.split_once(" /bin/sh -s -- ")
                    .map(|(_, mode)| mode.to_owned())
            })
            .collect()
    }

    /// What the stubbed service managers, curl and the installer were asked.
    fn calls(&self) -> String {
        self.read("calls")
    }

    fn stored(&self) -> Vec<(Host, String)> {
        let hosts = self.book.hosts();
        let tokens = self.book.tokens.lock().unwrap();
        hosts
            .into_iter()
            .map(|host| {
                let token = tokens
                    .iter()
                    .find(|(id, _)| *id == host.id)
                    .unwrap()
                    .1
                    .clone();
                (host, token)
            })
            .collect()
    }

    fn assert_no_token_shown(&self, out: &str, err: &str) {
        for (what, text) in [
            ("stdout", out.to_owned()),
            ("stderr", err.to_owned()),
            ("ssh argv", self.read("ssh.log")),
            ("host commands", self.calls()),
        ] {
            for token in [TOKEN, GENERATED] {
                assert!(!text.contains(token), "the token reached {what}:\n{text}");
            }
        }
        // Every ssh call refuses to prompt or learn a host key, and every
        // remote run is the one shape `remote` sends.
        for call in self.ssh() {
            assert!(
                call.starts_with("-o BatchMode=yes -o StrictHostKeyChecking=yes "),
                "{call}"
            );
            assert!(call.contains(" -o ConnectTimeout=15 "), "{call}");
            assert!(
                call.ends_with(&format!("-G -- {TARGET}"))
                    || call.contains(&format!("-T -- {TARGET} /bin/sh -s -- ")),
                "{call}"
            );
        }
    }
}

enum Service {
    SystemdUser,
    LaunchAgent,
    LaunchDaemon,
}

fn env(lines: &[&str]) -> String {
    let mut text = format!("# written by install.sh\nSOLADOR_AGENT_TOKEN={TOKEN}\n");
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

#[test]
fn a_ready_agent_is_paired_with_nothing_changed_on_the_host() {
    let world = World::new()
        .agent(
            &env(&["SOLADOR_AGENT_TLS=1", "SOLADOR_AGENT_BIND=0.0.0.0"]),
            Service::SystemdUser,
        )
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();

    let (code, out, err) = world.add("");
    assert_eq!(code, exit::OK, "{err}");
    assert!(
        out.contains(&format!("Added agent-1 ({LAN}:7878), pinned to {PIN}")),
        "{out}"
    );
    assert_eq!(world.modes(), ["discover", "collect"]);
    assert_eq!(world.calls(), "", "no restart, no install");
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
    let stored = world.stored();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].0.tls_fingerprint.as_deref(), Some(PIN));
    assert_eq!(stored[0].1, TOKEN, "the agent's own token, unchanged");
    // The script ran as sent: embedded, whole, on stdin.
    assert_eq!(world.read("script.sent"), remote::SCRIPT);

    // Again: already so, and still nothing changed on the host.
    let (code, out, _) = world.add("");
    assert_eq!(code, exit::OK);
    assert!(out.starts_with("Already paired, nothing changed:"), "{out}");
    assert_eq!(world.stored().len(), 1);
}

#[test]
fn a_certificate_other_than_the_hosts_own_is_never_pinned() {
    // The comparison that replaces the operator's eyes: the host says PIN,
    // what answers at the address presents OTHER.
    let world = World::new()
        .agent(
            &env(&["SOLADOR_AGENT_TLS=1", "SOLADOR_AGENT_BIND=0.0.0.0"]),
            Service::SystemdUser,
        )
        .open_app(OTHER);
    let (code, _, err) = world.add("");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("Fingerprint mismatch"), "{err}");
    assert!(world.stored().is_empty());
}

#[test]
fn a_tailnet_bound_agent_is_refused_without_bind_all_and_left_as_it_was() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();
    let (code, _, err) = world.add("");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("--bind-all") && err.contains(TAILNET), "{err}");
    assert!(err.contains("Nothing was changed"), "{err}");
    assert_eq!(world.modes(), ["discover"], "no token read for a refusal");
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
    assert!(world.stored().is_empty());
}

#[test]
fn bind_all_rebinds_as_the_readme_does_and_then_pairs() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
                "SOLADOR_AGENT_PORT=7878",
            ]),
            Service::SystemdUser,
        )
        .open_app(PIN);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(world.modes(), ["discover", "rebind", "discover", "collect"]);
    assert_eq!(
        world.calls(),
        "systemctl --user restart solador-agent.service\n"
    );

    let rewritten = fs::read_to_string(world.env_file()).unwrap();
    assert_eq!(
        rewritten,
        format!(
            "# written by install.sh\nSOLADOR_AGENT_TOKEN={TOKEN}\nSOLADOR_AGENT_TLS=1\n\
             SOLADOR_AGENT_PORT=7878\nSOLADOR_AGENT_BIND=0.0.0.0\n"
        )
    );
    let mode = fs::metadata(world.env_file()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // The way back is named.
    assert!(
        err.contains(&format!("SOLADOR_AGENT_BIND was {TAILNET}")),
        "{err}"
    );
    assert_eq!(world.stored()[0].1, TOKEN);
}

#[test]
fn a_restart_that_fails_after_the_rewrite_says_what_changed() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .with("systemctl-fails", "")
        .open_app(PIN);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    assert!(
        err.contains(&format!(
            "On {TARGET}: its env file now sets SOLADOR_AGENT_BIND=0.0.0.0 (SOLADOR_AGENT_BIND \
             was {TAILNET}), but the restart did not complete, so the agent may be stopped or \
             still on its old bind (put SOLADOR_AGENT_BIND={TAILNET} back and restart to undo)."
        )),
        "{err}"
    );
    assert!(
        err.contains("systemctl --user status solador-agent.service"),
        "{err}"
    );
    assert!(err.contains("Nothing was added to Solador."), "{err}");
    assert_eq!(world.modes(), ["discover", "rebind"], "no token read");
    assert!(world.stored().is_empty());
}

#[test]
fn a_launch_agent_is_restarted_in_its_own_gui_domain() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::LaunchAgent,
        )
        .open_app(PIN);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::OK, "{err}");
    assert!(
        world
            .calls()
            .contains("launchctl kickstart -k gui/501/app.solador.agent\n"),
        "{}",
        world.calls()
    );
}

#[test]
fn require_tailnet_is_refused_whatever_the_flags_and_nothing_changes() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
                "SOLADOR_AGENT_REQUIRE_TAILNET=1",
            ]),
            Service::SystemdUser,
        )
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();
    let (code, _, err) = world.add("--bind-all --enable-tls --install");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("SOLADOR_AGENT_REQUIRE_TAILNET=1"), "{err}");
    assert_eq!(world.modes(), ["discover"]);
    assert_eq!(world.calls(), "");
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
}

#[test]
fn a_system_daemon_owes_root_its_restart_and_nothing_is_taken() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::LaunchDaemon,
        )
        .open_app(PIN);
    // The dry run says so before anything is written.
    let (code, _, err) = world.add("--bind-all --dry-run");
    assert_eq!(code, exit::OK, "{err}");
    assert!(err.contains("only root can restart it"), "{err}");

    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::ROOT_STEP, "{err}");
    assert!(
        err.contains(
            "on agent-1 as an administrator, then run this command again:\n  \
             sudo launchctl kickstart -k system/app.solador.agent\n"
        ),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "(SOLADOR_AGENT_BIND was {TAILNET}; put SOLADOR_AGENT_BIND={TAILNET} back and \
             restart to undo)"
        )),
        "{err}"
    );
    assert!(!world.calls().contains("kickstart"), "{}", world.calls());
    assert_eq!(
        world.modes(),
        ["discover", "discover", "rebind"],
        "no token read"
    );
    assert!(world.stored().is_empty());
    // What it did write is what it said.
    assert!(fs::read_to_string(world.env_file())
        .unwrap()
        .ends_with("SOLADOR_AGENT_BIND=0.0.0.0\n"));
}

#[test]
fn another_users_agent_is_never_shadowed_by_an_install() {
    let world = World::new()
        // As the script asks for it: a uid, never procps's user column, which
        // cuts this name to "sassydo+".
        .with(
            "ps",
            "0 launchd\n502 /home/sassydog-agents/.local/bin/solador-agent\n",
        )
        .open_app(PIN);
    let (code, _, err) = world.add("--install");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("--ssh sassydog-agents@agent-1"), "{err}");
    assert_eq!(world.calls(), "", "nothing downloaded, nothing installed");
}

#[test]
fn no_agent_needs_install_which_runs_the_installer_with_stdin_closed() {
    let world = World::new().open_app(PIN);
    let (code, _, err) = world.add("");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("--install"), "{err}");
    assert_eq!(world.calls(), "");

    let sha = "0123456789abcdef0123456789abcdef01234567";
    let (code, _, err) = world.add(&format!("--install --ref {sha}"));
    assert_eq!(code, exit::OK, "{err}");
    // No tailnet here, so the installer's own wildcard: said, not hidden.
    assert!(
        err.contains("on all interfaces, provisionally (SOLADOR_AGENT_BIND_AUTO=1"),
        "{err}"
    );
    let calls = world.calls();
    assert!(
        calls.contains(&format!(
            "curl -fsSL --connect-timeout 15 --max-time 120 -o {} {BOOTSTRAP_URL}\n",
            world.home(".cache/solador-cli/bootstrap.sh").display()
        )),
        "{calls}"
    );
    assert!(
        calls.contains(&format!("bootstrap --ref {sha}\n")),
        "{calls}"
    );
    // The installer generates the token, and neither the bind nor the
    // tailnet rule is decided by whatever the login shell exported.
    assert!(calls.contains("bootstrap stdin=closed\n"), "{calls}");
    assert!(
        calls.contains("bootstrap bind=unset require=unset\n"),
        "{calls}"
    );
    assert!(
        err.contains("Installed (stand-in installer)"),
        "the installer's output is relayed: {err}"
    );
    let stored = world.stored();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].1, GENERATED);
    let mode = fs::metadata(world.home(".cache/solador-cli"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn bind_all_on_a_fresh_install_is_the_installers_explicit_bind() {
    let world = World::new()
        .with("tailnet", &format!("{TAILNET}\n"))
        .open_app(PIN);
    let (code, _, err) = world.add("--install");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("--bind-all"), "{err}");

    let (code, _, err) = world.add("--install --bind-all");
    assert_eq!(code, exit::OK, "{err}");
    assert!(
        world
            .calls()
            .contains("bootstrap bind=0.0.0.0 require=unset\n"),
        "{}",
        world.calls()
    );
}

#[test]
fn tls_off_needs_enable_tls_which_re_runs_the_installer() {
    let world = World::new()
        .agent(
            &env(&[&format!("SOLADOR_AGENT_BIND={LAN}")]),
            Service::SystemdUser,
        )
        .open_app(PIN);
    fs::remove_file(world.root.join("fingerprint")).unwrap();
    let (code, _, err) = world.add("");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("--enable-tls"), "{err}");
    assert_eq!(world.calls(), "");

    let (code, _, err) = world.add("--enable-tls");
    assert_eq!(code, exit::OK, "{err}");
    assert!(
        world.calls().contains("bootstrap --enable-tls\n"),
        "{}",
        world.calls()
    );
    assert_eq!(world.stored()[0].1, TOKEN, "an existing token is kept");
}

#[test]
fn dry_run_says_what_would_change_and_reads_no_token() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();
    let (code, out, err) = world.add("--bind-all --dry-run");
    assert_eq!(code, exit::OK, "{err}");
    assert!(
        out.contains(&format!("Would pair agent-1 at {LAN}:7878")),
        "{out}"
    );
    assert!(out.contains("listen on all interfaces"), "{out}");
    assert_eq!(world.modes(), ["discover"]);
    assert_eq!(world.calls(), "");
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
    assert!(world.stored().is_empty());
}

#[test]
fn with_no_app_running_nothing_is_sent_over_ssh() {
    let world = World::new().agent(&env(&["SOLADOR_AGENT_TLS=1"]), Service::SystemdUser);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::NOT_RUNNING, "{err}");
    assert!(world.ssh().is_empty(), "{:?}", world.ssh());
}

#[test]
fn a_proxied_host_needs_an_address_and_an_unknown_host_key_is_never_accepted() {
    let world = World::new()
        .agent(
            &env(&["SOLADOR_AGENT_TLS=1", "SOLADOR_AGENT_BIND=0.0.0.0"]),
            Service::SystemdUser,
        )
        .with("ssh-G", "hostname agent-1.internal\nproxyjump bastion\n")
        .open_app(PIN);
    let (code, _, err) = world.add("");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("Pass --address"), "{err}");
    assert!(world.modes().is_empty());

    let world = world.with("ssh-down", "");
    let (code, _, err) = world.add(&format!("--address {LAN}"));
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("Connect once by hand"), "{err}");
    assert!(world.stored().is_empty());
}

#[test]
fn an_agent_that_never_comes_back_over_tls_is_not_added_and_the_change_is_said() {
    // The rebind happens and the restart returns, but nothing answers over
    // TLS at the address afterwards: its own failure, never a fall-through
    // to the add.
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .open_app_without_tls();
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(
        err.contains(&format!(
            "the agent on {TARGET} did not answer over TLS at {LAN}:7878"
        )),
        "{err}"
    );
    assert!(err.contains("it answers plain HTTP there"), "{err}");
    assert!(
        err.contains("systemctl --user status solador-agent.service"),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "On {TARGET}: its agent now listens on all interfaces; SOLADOR_AGENT_BIND was \
             {TAILNET} (put SOLADOR_AGENT_BIND={TAILNET} back and restart to undo)."
        )),
        "{err}"
    );
    assert_eq!(
        world.modes(),
        ["discover", "rebind", "discover"],
        "no token read"
    );
    assert!(world.stored().is_empty());
}

#[test]
fn a_mismatch_after_a_rebind_still_says_what_changed() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .open_app(OTHER);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("Fingerprint mismatch"), "{err}");
    assert!(
        err.contains(&format!(
            "On {TARGET}: its agent now listens on all interfaces"
        )),
        "{err}"
    );
    assert!(world.stored().is_empty());
}

#[test]
fn an_installer_that_fails_is_relayed_and_may_have_changed_the_host() {
    let world = World::new().with("installer-fails", "").open_app(PIN);
    let (code, _, err) = world.add("--install");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("signature check failed"), "relayed: {err}");
    assert!(
        err.contains(&format!(
            "The installer on {TARGET} did not finish (exit 1); its output is above, and it \
             may have changed {TARGET} partway: the agent's env file there is \
             ~/.config/solador-agent.env"
        )),
        "{err}"
    );
    assert!(err.contains("--dry-run"), "{err}");
    // No service was found before the install, so the hint is the one the
    // installer would have made on this platform.
    assert!(
        err.contains("look at its service with `systemctl --user status solador-agent.service`"),
        "{err}"
    );
    assert_eq!(world.modes(), ["discover", "install"], "no token read");
    assert!(world.stored().is_empty());
}

#[test]
fn a_connection_that_drops_during_the_restart_says_the_host_changed() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .with("after-rebind", "exit 255\n")
        .open_app(PIN);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    assert!(
        err.contains("The ssh connection dropped during the restart."),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "On {TARGET}: its env file now sets SOLADOR_AGENT_BIND=0.0.0.0"
        )),
        "{err}"
    );
    assert_eq!(world.modes(), ["discover", "rebind"], "no token read");
    assert!(world.stored().is_empty());
}

#[test]
fn the_scripts_own_rebind_guards_leave_the_env_file_as_it_was() {
    // `plan` refuses both of these first; the script refuses them again, on
    // its own, for any caller that reaches it some other way.
    for (lines, pre_tls, why) in [
        (
            vec![format!("SOLADOR_AGENT_BIND={TAILNET}")],
            false,
            "TLS is off",
        ),
        (
            vec![
                "SOLADOR_AGENT_TLS=1".to_owned(),
                format!("SOLADOR_AGENT_BIND={TAILNET}"),
                "SOLADOR_AGENT_REQUIRE_TAILNET=1".to_owned(),
            ],
            false,
            "SOLADOR_AGENT_REQUIRE_TAILNET=1 holds",
        ),
        (
            vec![
                "SOLADOR_AGENT_TLS=1".to_owned(),
                format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ],
            true,
            "predates TLS",
        ),
    ] {
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut world = World::new().agent(&env(&lines), Service::SystemdUser);
        if pre_tls {
            world = world.with("pre-tls", "");
        }
        let before = fs::read(world.env_file()).unwrap();
        let (code, out, err) = world.script(&["rebind"]);
        assert_eq!(code, Some(1), "{out}{err}");
        assert!(err.contains(why), "{err}");
        assert!(!out.contains("rewritten="), "{out}");
        assert_eq!(fs::read(world.env_file()).unwrap(), before);
        assert_eq!(world.calls(), "", "no restart");
    }
}

#[test]
fn the_scripts_rebind_drops_only_the_bind_lines() {
    let world = World::new().agent(
        &env(&[
            "SOLADOR_AGENT_TLS=1",
            "SOLADOR_AGENT_BIND=0.0.0.0",
            "SOLADOR_AGENT_BIND_AUTO=1",
            "SOLADOR_AGENT_REQUIRE_TAILNET=0",
        ]),
        Service::SystemdUser,
    );
    let (code, out, err) = world.script(&["rebind"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(
        out,
        "previous_bind=0.0.0.0\nprevious_bind_auto=1\nrewritten=yes\n"
    );
    assert_eq!(
        fs::read_to_string(world.env_file()).unwrap(),
        format!(
            "# written by install.sh\nSOLADOR_AGENT_TOKEN={TOKEN}\nSOLADOR_AGENT_TLS=1\n\
             SOLADOR_AGENT_REQUIRE_TAILNET=0\nSOLADOR_AGENT_BIND=0.0.0.0\n"
        )
    );
}

#[test]
fn the_scripts_install_refuses_a_bad_ref_before_any_download() {
    let world = World::new();
    for (mode, want) in [
        (&["install", "--ref", "main"][..], Some(1)),
        (&["install", "--ref", &"A".repeat(40)][..], Some(1)),
        (&["install", "--ref", &"a".repeat(39)][..], Some(1)),
        (&["install", "--ref"][..], Some(1)),
        (&["install", "--system-daemon"][..], Some(2)),
    ] {
        let (code, out, err) = world.script(mode);
        assert_eq!(code, want, "{mode:?}: {out}{err}");
    }
    assert_eq!(world.calls(), "", "nothing downloaded, nothing run");
}

#[test]
fn a_binary_that_predates_tls_is_never_rebound_whatever_its_env_file_says() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .with("pre-tls", "")
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    assert!(
        err.contains("predates TLS") && err.contains("--enable-tls"),
        "{err}"
    );
    assert_eq!(world.modes(), ["discover"], "refused before any change");
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
    assert_eq!(world.calls(), "");

    // The flag it names gets past it: one installer run updates the agent
    // and binds all interfaces, and the token is kept.
    let (code, _, err) = world.add("--bind-all --enable-tls");
    assert_eq!(code, exit::OK, "{err}");
    let calls = world.calls();
    assert!(calls.contains("bootstrap --enable-tls\n"), "{calls}");
    assert!(
        calls.contains("bootstrap bind=0.0.0.0 require=unset\n"),
        "{calls}"
    );
    assert_eq!(world.stored()[0].1, TOKEN);
}

#[test]
fn a_launch_agent_with_no_login_session_is_left_alone() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::LaunchAgent,
        )
        .without("gui")
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("no login session"), "{err}");
    assert_eq!(world.modes(), ["discover"]);
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
    assert!(!world.calls().contains("kickstart"), "{}", world.calls());
}

#[test]
fn a_mac_whose_tailscale_is_the_app_is_seen_on_its_tailnet() {
    // No CLI on PATH, only the one inside Tailscale.app: the installer finds
    // the tailnet address there, so the plan must too, and refuse a fresh
    // install the address would miss before anything is downloaded.
    let world = World::new()
        .with("os", "Darwin\n")
        .with("gui", "")
        .without("bin/tailscale");
    write(
        &world.root.join("Tailscale.app/Contents/MacOS/Tailscale"),
        &format!("#!/bin/sh\n[ \"$*\" = \"ip -4\" ] && echo {TAILNET}\n"),
        0o755,
    );
    let (code, out, err) = world.script(&["discover"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains(&format!("tailnet_ip={TAILNET}\n")), "{out}");

    let world = world.open_app(PIN);
    let (code, _, err) = world.add("--install");
    assert_eq!(code, exit::FAILED);
    assert!(
        err.contains(&format!("its tailnet address {TAILNET} only")),
        "{err}"
    );
    assert!(!world.calls().contains("curl"), "{}", world.calls());
}

#[test]
fn a_linux_host_with_no_tailscale_cli_is_read_from_its_interface() {
    let world = World::new().without("bin/tailscale").with(
        "ip-addr",
        &format!("4: tailscale0    inet {TAILNET}/32 scope global tailscale0\\       valid_lft forever\n"),
    );
    let (code, out, err) = world.script(&["discover"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains(&format!("tailnet_ip={TAILNET}\n")), "{out}");
}

#[test]
fn what_the_installer_left_is_checked_before_the_token_is_read() {
    for (toggle, problem) in [
        (
            ("release-pre-tls", String::new()),
            "TLS is still off there: the release it installed predates TLS.".to_owned(),
        ),
        (
            ("installer-writes-no-cert", String::new()),
            "its agent has not written a certificate, so it may not be running. Look at it \
             with `systemctl --user status solador-agent.service`"
                .to_owned(),
        ),
        (
            ("installer-binds", TAILNET.to_owned()),
            format!("its agent still listens on {TAILNET}, which {LAN} is not."),
        ),
    ] {
        let world = World::new().with(toggle.0, &toggle.1).open_app(PIN);
        let (code, _, err) = world.add("--install");
        assert_eq!(code, exit::FAILED, "{}: {err}", toggle.0);
        assert!(
            err.contains(&format!("After that, on {TARGET}, {problem}")),
            "{}: {err}",
            toggle.0
        );
        assert!(
            err.contains(&format!("On {TARGET}: the agent was installed.")),
            "{}: {err}",
            toggle.0
        );
        assert!(
            !world.modes().iter().any(|mode| mode == "collect"),
            "{}: no token read: {:?}",
            toggle.0,
            world.modes()
        );
        assert!(world.stored().is_empty());
    }
}

#[test]
fn a_token_read_that_fails_stores_nothing_and_shows_nothing() {
    // The script printed the token each time; what comes back around it is
    // wrong, and the refusal must not repeat any of it.
    for (after, says) in [
        (
            "exit 1\n",
            format!("could not read the agent's token on {TARGET}"),
        ),
        ("exit 255\n", format!("ssh could not reach {TARGET}")),
        (
            "echo token=a-second-line\n",
            "not shown: it may hold the token".to_owned(),
        ),
    ] {
        let world = World::new()
            .agent(
                &env(&["SOLADOR_AGENT_TLS=1", "SOLADOR_AGENT_BIND=0.0.0.0"]),
                Service::SystemdUser,
            )
            .with("after-collect", after)
            .open_app(PIN);
        let (code, out, err) = world.add("");
        assert_eq!(code, exit::FAILED, "{after}: {err}");
        assert!(err.contains(&says), "{after}: {err}");
        assert_eq!(out, "", "{after}");
        assert!(world.stored().is_empty(), "{after}");
    }
}

#[test]
fn a_connection_lost_before_the_rebind_ran_says_the_host_may_have_changed() {
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .with("before-rebind", "exit 255\n")
        .open_app(PIN);
    let before = fs::read(world.env_file()).unwrap();
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    // It cannot know the mode never ran, so it does not claim so.
    assert!(
        err.contains(&format!(
            "The ssh connection to {TARGET} dropped during the rebind, and {TARGET} may have \
             changed."
        )),
        "{err}"
    );
    assert_eq!(fs::read(world.env_file()).unwrap(), before);
    assert!(world.stored().is_empty());
}

#[test]
fn the_scripts_own_refusal_reaches_the_operator_as_nothing_changed() {
    // TLS turned off on the host between the discovery and the rebind: the
    // plan allowed the rebind, the script refuses it, and says so.
    let world = World::new()
        .agent(
            &env(&[
                "SOLADOR_AGENT_TLS=1",
                &format!("SOLADOR_AGENT_BIND={TAILNET}"),
            ]),
            Service::SystemdUser,
        )
        .with(
            "before-rebind",
            "env=\"$ROOT/home/.config/solador-agent.env\"\n\
             grep -v '^SOLADOR_AGENT_TLS=' \"$env\" > \"$ROOT/env.edit\"\n\
             cat \"$ROOT/env.edit\" > \"$env\"\n",
        )
        .open_app(PIN);
    let (code, _, err) = world.add("--bind-all");
    assert_eq!(code, exit::FAILED);
    assert!(
        err.contains("TLS is off on this agent")
            && err.contains(&format!("Nothing was changed on {TARGET}.")),
        "{err}"
    );
    assert_eq!(
        fs::read_to_string(world.env_file()).unwrap(),
        env(&[&format!("SOLADOR_AGENT_BIND={TAILNET}")]),
        "only the edit made under the command"
    );
    assert_eq!(world.calls(), "", "no restart");
    assert!(world.stored().is_empty());
}
