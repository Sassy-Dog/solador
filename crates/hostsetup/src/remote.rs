//! `solador-app host add --ssh <target>`: get a host's agent ready for this
//! cockpit over ssh, then pair it through the running app.
//!
//! The remote half is `remote/host.sh`, embedded here and sent on ssh's stdin
//! for every step. What the command knows about the host comes from its
//! `discover` mode, every change it makes from `rebind` or `install`, and the
//! token from `collect`, which is captured and never printed. The pairing is
//! the same `add` request `host pair` sends: the app probes the address
//! itself, compares the certificate with the fingerprint read here over ssh,
//! checks the token over the pinned connection, and only then saves.
//!
//! It never runs sudo (a step only root can take is printed, exit 3), never
//! accepts an ssh host key or prompts (`StrictHostKeyChecking=yes`,
//! `BatchMode=yes`), never installs a package, never removes
//! `SOLADOR_AGENT_REQUIRE_TAILNET`, and never rotates a token.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::channel::{self, ChannelError};
use crate::cli::{exit, render_paired, AddArgs};
use crate::protocol::{AddRequest, Envelope, ProbeSummary, Request, Response};
use crate::Secret;

/// The remote half. Embedded, so the command and the script it runs on the
/// host are always the same build.
pub const SCRIPT: &str = include_str!("../remote/host.sh");

/// How long a just-restarted agent is given to write its certificate, and the
/// app to see it at the address, before the command stops waiting.
const SETTLE: Duration = Duration::from_secs(15);
const SETTLE_EVERY: Duration = Duration::from_millis(500);

/// The system `ssh`, always with the two options that keep this command from
/// deciding anything about trust or answering a prompt on anyone's behalf.
pub struct Ssh {
    program: PathBuf,
}

impl Ssh {
    #[must_use]
    pub fn system() -> Self {
        Ssh::with_program("ssh")
    }

    /// Another executable in ssh's place, taking ssh's arguments: the tests'
    /// stand-in.
    #[must_use]
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Ssh {
            program: program.into(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        // BatchMode: never prompt. Nobody may be there to answer, and a
        // password typed into this pipe would not be ssh's to read.
        // StrictHostKeyChecking=yes: a host key this machine has not already
        // accepted is refused, never learned here.
        command.args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"]);
        // A host that never answers, or stops answering mid-step, is a
        // sentence within a minute rather than a command that hangs.
        command.args([
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]);
        command
    }

    /// `ssh -G`: what ssh would connect to, without connecting.
    fn config(&self, target: &str) -> Result<SshConfig, String> {
        let output = self
            .command()
            .args(["-G", "--", target])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("could not run ssh: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "ssh -G {target} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        parse_config(&String::from_utf8_lossy(&output.stdout))
            .ok_or(format!("ssh -G {target} named no HostName"))
    }

    /// One mode of [`SCRIPT`] on `target`, the script on stdin, so the host's
    /// login shell parses nothing but these fixed words. `Err` only when ssh
    /// could not be started; ssh's own failure is [`Run::dropped`], left to
    /// the caller because after a step that changes the host it means "may
    /// have changed", not "could not reach".
    fn run(&self, target: &str, mode: &[&str]) -> Result<Run, String> {
        self.run_relaying(target, mode, None)
    }

    /// [`Ssh::run`], copying each line of the mode's stdout to `relay`, made
    /// printable, as it arrives: an installer that takes minutes, or never
    /// finishes, is visible while it runs rather than only once ssh exits.
    fn run_relaying(
        &self,
        target: &str,
        mode: &[&str],
        mut relay: Option<&mut dyn Write>,
    ) -> Result<Run, String> {
        let mut child = self
            .command()
            .args(["-T", "--", target, "/bin/sh", "-s", "--"])
            .args(mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not run ssh: {error}"))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let writer = std::thread::spawn(move || stdin.write_all(SCRIPT.as_bytes()));
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let mut stdout = Vec::new();
        let mut lines = BufReader::new(child.stdout.take().expect("stdout is piped"));
        loop {
            let mut line = Vec::new();
            match lines.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(relay) = relay.as_deref_mut() {
                        let _ =
                            relay.write_all(printable(&String::from_utf8_lossy(&line)).as_bytes());
                        let _ = relay.flush();
                    }
                    stdout.extend_from_slice(&line);
                }
            }
        }
        let status = child
            .wait()
            .map_err(|error| format!("ssh {target} failed: {error}"))?;
        // A host that hangs up before reading the script is reported by its
        // exit status; the write's own error adds nothing.
        let _ = writer.join();
        let stderr = errors.join().unwrap_or_default();
        Ok(Run {
            code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: printable(&String::from_utf8_lossy(&stderr)),
        })
    }
}

/// ssh's own failure (exit 255), with the one remedy this command will not
/// apply itself.
fn unreachable(target: &str, stderr: &str) -> String {
    let stderr = stderr.trim();
    let hint = if stderr.contains("Host key verification failed")
        || stderr.contains("No ED25519 host key is known")
        || stderr.contains("host key is known")
    {
        format!(" Connect once by hand (`ssh {target}`) to check and accept its host key; this command never accepts one.")
    } else {
        String::new()
    };
    format!("ssh could not reach {target}: {stderr}{hint}")
}

/// One run of a mode. Deliberately not `Debug`: `collect`'s stdout is the
/// token.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    /// ssh's own failure: the host was never reached, or the connection went
    /// away during the mode. `host.sh` never exits 255 itself.
    fn dropped(&self) -> bool {
        self.code == Some(255)
    }

    /// A `key=value` line the mode printed.
    fn field(&self, key: &str) -> String {
        self.stdout
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(printable)
            .unwrap_or_default()
    }
}

/// Text from the host, made safe to print: every control character other than
/// a newline or a tab becomes `?`, so nothing the host sends can move the
/// cursor or repaint the operator's terminal.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SshConfig {
    hostname: String,
    /// A ProxyJump or ProxyCommand: the HostName is then the far side's name
    /// for the host, not necessarily anything this machine can dial.
    proxied: bool,
}

fn parse_config(text: &str) -> Option<SshConfig> {
    let mut hostname = None;
    let mut proxied = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "hostname" => hostname = Some(value.to_owned()),
            "proxyjump" | "proxycommand" if !value.eq_ignore_ascii_case("none") => proxied = true,
            _ => {}
        }
    }
    hostname
        .filter(|name| !name.is_empty())
        .map(|hostname| SshConfig { hostname, proxied })
}

/// How the agent on the host is run, which decides how it can be restarted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Service {
    /// A systemd user unit: `systemctl --user restart`.
    SystemdUser,
    /// A per-user LaunchAgent: `launchctl kickstart -k gui/<uid>/…`.
    LaunchAgent,
    /// A LaunchDaemon run as this user: only root can restart it.
    LaunchDaemon,
    None,
}

/// What `discover` reported. Never the token: only whether there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Discovery {
    pub os: String,
    pub hostname: String,
    pub user: String,
    pub env: bool,
    pub tls: bool,
    pub bind: String,
    pub bind_auto: bool,
    pub require_tailnet: bool,
    pub port: Option<u16>,
    pub token: bool,
    pub bin: bool,
    pub version: String,
    pub fingerprint: Option<String>,
    /// Why there is no fingerprint: `unsupported` (a release from before
    /// TLS) or `missing` (no certificate yet).
    pub fingerprint_status: String,
    pub service: Service,
    pub daemon_user: String,
    pub gui: bool,
    pub running_as: String,
    pub tailnet_ip: String,
    pub have_minisign: bool,
    pub have_curl: bool,
    pub have_bash: bool,
}

/// Parses `discover`'s `key=value` lines.
///
/// # Errors
/// Output that is not `discover`'s (a login shell that prints a banner, a
/// host without `/bin/sh`), or a port that is not one.
pub(crate) fn parse_discovery(text: &str) -> Result<Discovery, String> {
    // Every value is the host's to choose, and several reach the terminal
    // (the hostname becomes the saved name): made printable once, here.
    let fields: BTreeMap<&str, String> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), printable(value.trim())))
        .collect();
    let get = |key: &str| fields.get(key).map_or("", String::as_str);
    let is = |key: &str, word: &str| get(key) == word;
    if get("os").is_empty() || get("user").is_empty() {
        return Err(
            "the host answered the discovery step with output this command does not recognise"
                .into(),
        );
    }
    let port = match get("port") {
        "" => None,
        raw => Some(
            raw.parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or(format!(
                    "the agent's SOLADOR_AGENT_PORT ({raw}) is not a port"
                ))?,
        ),
    };
    let service = match get("service") {
        "systemd-user" => Service::SystemdUser,
        "launch-agent" => Service::LaunchAgent,
        "launch-daemon" => Service::LaunchDaemon,
        _ => Service::None,
    };
    Ok(Discovery {
        os: get("os").into(),
        hostname: get("hostname").into(),
        user: get("user").into(),
        env: is("env", "present"),
        tls: is("tls", "1"),
        bind: get("bind").into(),
        bind_auto: is("bind_auto", "1"),
        require_tailnet: is("require_tailnet", "1"),
        port,
        token: is("token", "present"),
        bin: is("bin", "present"),
        version: get("version").into(),
        fingerprint: Some(get("fingerprint"))
            .filter(|fingerprint| !fingerprint.is_empty())
            .map(Into::into),
        fingerprint_status: get("fingerprint_status").into(),
        service,
        daemon_user: get("daemon_user").into(),
        gui: is("gui", "yes"),
        running_as: get("running_as").into(),
        tailnet_ip: get("tailnet_ip").into(),
        have_minisign: is("have_minisign", "yes"),
        have_curl: is("have_curl", "yes"),
        have_bash: is("have_bash", "yes"),
    })
}

/// One change the command makes on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Run the installer: a fresh install, or a re-run that turns TLS on
    /// (which is also the update path). `bind_all` puts
    /// `SOLADOR_AGENT_BIND=0.0.0.0` in its environment.
    Install { enable_tls: bool, bind_all: bool },
    /// Rewrite the bind to all interfaces and restart: agent/README.md's
    /// "Reaching a Tailscale host from the LAN too".
    Rebind,
}

impl Step {
    fn describe(self) -> &'static str {
        match self {
            Step::Install {
                enable_tls: false,
                bind_all: false,
            } => "install the agent (a signed release, verified before it runs)",
            Step::Install {
                enable_tls: false,
                bind_all: true,
            } => "install the agent (a signed release, verified before it runs), listening on all interfaces",
            Step::Install {
                enable_tls: true,
                bind_all: false,
            } => "re-run the installer to turn TLS on (it also updates the agent to the newest release)",
            Step::Install {
                enable_tls: true,
                bind_all: true,
            } => "re-run the installer to turn TLS on and listen on all interfaces (it also updates the agent to the newest release)",
            Step::Rebind => "set the agent to listen on all interfaces and restart it (its tailnet address keeps working)",
        }
    }
}

/// What the command will do: the steps on the host, then the pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub steps: Vec<Step>,
    /// Things worth knowing that change nothing.
    pub notes: Vec<String>,
}

/// Decides what has to change on the host for this cockpit to pair it at
/// `address`, which resolves (here) to `dial`. Every refusal is a sentence
/// naming the flag or the step that would get past it, and none of them has
/// changed anything.
///
/// # Errors
/// The refusal.
pub(crate) fn plan(
    found: &Discovery,
    args: &AddArgs,
    address: &str,
    dial: &[IpAddr],
) -> Result<Plan, String> {
    let target = args.target.as_str();
    if dial.is_empty() {
        return Err(format!(
            "{address} does not resolve on this machine, so Solador could not reach it either. \
             Pass --address with an address this machine can dial."
        ));
    }
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| default_name(found, address));
    let port = args
        .port
        .or(found.port)
        .unwrap_or(store::DEFAULT_AGENT_PORT);
    let mut steps = Vec::new();
    let mut notes = Vec::new();
    let user = &found.user;

    if !(found.env && found.bin) {
        if let Some(owner) = foreign_owner(found) {
            return Err(format!(
                "The agent on {target} runs as {owner}, not {user}. Re-run as that user \
                 (--ssh {owner}@{host}), which needs ssh access as {owner}; or pair it with \
                 `solador-app host pair`, reading its fingerprint and token as {owner}.",
                host = host_part(target)
            ));
        }
        if !args.install {
            return Err(format!(
                "No agent is installed for {user} on {target}. Pass --install to install one: \
                 it downloads a signed release and verifies it before running anything."
            ));
        }
        if found.os == "Darwin" && !found.gui {
            return Err(format!(
                "{user} has no login session on {target}, which a per-user LaunchAgent needs. \
                 Install the agent as a system daemon by hand instead (agent/README.md, \
                 \"Running as a system daemon under a service user\"), then re-run this command \
                 as the service user."
            ));
        }
        for (have, tool) in [
            (found.have_curl, "curl"),
            (found.have_bash, "bash"),
            (found.have_minisign, "minisign"),
        ] {
            if !have {
                return Err(format!(
                    "{tool} isn't installed on {target}, and the installer needs it. \
                     Install it there, then re-run."
                ));
            }
        }
        let bind = if args.bind_all || found.tailnet_ip.is_empty() {
            "0.0.0.0"
        } else {
            found.tailnet_ip.as_str()
        };
        if !covers(bind, address, dial) {
            return Err(format!(
                "A fresh install on {target} would listen on its tailnet address {bind} only, \
                 which {address} is not. Pass --bind-all to have it listen on all interfaces, \
                 or --address {bind} to dial that address."
            ));
        }
        if !args.bind_all && found.tailnet_ip.is_empty() {
            notes.push(format!(
                "{target} has no tailnet address, so its installer will have the agent listen \
                 on all interfaces, provisionally (SOLADOR_AGENT_BIND_AUTO=1: a later installer \
                 run binds the tailnet address instead if Tailscale is up by then)."
            ));
        }
        steps.push(Step::Install {
            enable_tls: false,
            bind_all: args.bind_all,
        });
        return Ok(Plan {
            name,
            address: address.into(),
            port,
            steps,
            notes,
        });
    }

    if !found.token {
        return Err(format!(
            "The agent's env file on {target} has no SOLADOR_AGENT_TOKEN, so it cannot be \
             serving. Re-run its installer there."
        ));
    }
    if found.service == Service::None {
        return Err(format!(
            "{target} has an agent for {user}, but no service this command knows how to \
             restart: no systemd user unit or LaunchAgent, and no system daemon run as {user}. \
             Pair it with `solador-app host pair` instead."
        ));
    }

    let mut installer = None;
    // The binary decides whether TLS is served, not the env file: a rollback
    // or a hand edit can leave SOLADOR_AGENT_TLS=1 beside a release that
    // predates it, and rebinding that one would put a plain-HTTP agent on
    // every interface.
    let binary_predates_tls = found.fingerprint_status == "unsupported";
    if !found.tls || binary_predates_tls {
        if !args.enable_tls {
            return Err(if found.tls {
                format!(
                    "The agent installed on {target} predates TLS, although its env file says \
                     SOLADOR_AGENT_TLS=1. Pass --enable-tls: it re-runs the installer there, \
                     which updates the agent to the newest release."
                )
            } else {
                format!(
                    "TLS is off on {target}'s agent, and a cockpit off the tailnet pairs only a \
                     TLS agent. Pass --enable-tls: it re-runs the installer there, which also \
                     updates the agent to the newest release."
                )
            });
        }
        if found.service == Service::LaunchDaemon {
            return Err(format!(
                "{target}'s agent is a system daemon, and turning TLS on there is \
                 `install.sh --system-daemon --enable-tls` plus a step only root can take \
                 (agent/README.md, \"Running as a system daemon under a service user\"). \
                 Do that, then re-run this command."
            ));
        }
        installer = Some(Step::Install {
            enable_tls: true,
            bind_all: false,
        });
    }

    let bind = effective_bind(found);
    let mut listen_everywhere = false;
    if !covers(&bind, address, dial) {
        if found.require_tailnet {
            return Err(format!(
                "{target}'s agent is held to its tailnet (SOLADOR_AGENT_REQUIRE_TAILNET=1) and \
                 listens on {bind}, which {address} is not. Someone decided that, so this \
                 command never undoes it: dial its tailnet address instead."
            ));
        }
        if !args.bind_all {
            return Err(format!(
                "{target}'s agent listens on {bind} only, which {address} is not. Pass \
                 --bind-all to have it listen on all interfaces (agent/README.md, \"Reaching a \
                 Tailscale host from the LAN too\"); its tailnet address keeps working."
            ));
        }
        listen_everywhere = true;
    } else if found.bind_auto && is_wildcard(&bind) {
        if args.bind_all {
            listen_everywhere = true;
        } else {
            notes.push(format!(
                "{target}'s agent listens on all interfaces because its installer chose to \
                 (SOLADOR_AGENT_BIND_AUTO=1); a later installer run binds its tailnet address \
                 instead if Tailscale is up then. Pass --bind-all to make all interfaces explicit."
            ));
        }
    }
    match (installer, listen_everywhere) {
        (Some(Step::Install { enable_tls, .. }), true) => steps.push(Step::Install {
            enable_tls,
            bind_all: true,
        }),
        (Some(step), false) => steps.push(step),
        (None, true) => steps.push(Step::Rebind),
        _ => {}
    }
    if !steps.is_empty() && found.service == Service::LaunchAgent && !found.gui {
        return Err(format!(
            "{user} has no login session on {target}, so its LaunchAgent can be neither \
             restarted nor reinstalled from here. Log in as {user} on {target}, then re-run."
        ));
    }
    if steps == [Step::Rebind] && found.service == Service::LaunchDaemon {
        notes.push(format!(
            "{target}'s agent is a system daemon: after the rewrite only root can restart it, so \
             this command will stop there, print that step and exit 3."
        ));
    }

    if steps.is_empty() && found.fingerprint.is_none() {
        return Err(format!(
            "The agent on {target} has TLS on but no certificate yet, which it writes when it \
             starts. Is it running?"
        ));
    }
    Ok(Plan {
        name,
        address: address.into(),
        port,
        steps,
        notes,
    })
}

/// The address the agent listens on, as the agent itself resolves an unset
/// bind with TLS on: the tailnet address when there is one, otherwise all
/// interfaces.
fn effective_bind(found: &Discovery) -> String {
    let bind = found.bind.trim();
    let bind = bind
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(bind);
    if !bind.is_empty() {
        bind.into()
    } else if !found.tailnet_ip.is_empty() {
        found.tailnet_ip.clone()
    } else {
        "0.0.0.0".into()
    }
}

fn is_wildcard(bind: &str) -> bool {
    matches!(bind, "0.0.0.0" | "::")
}

/// Whether an agent listening on `bind` answers at `address`.
fn covers(bind: &str, address: &str, dial: &[IpAddr]) -> bool {
    if is_wildcard(bind) {
        return true;
    }
    match bind.parse::<IpAddr>() {
        Ok(ip) => dial.contains(&ip),
        Err(_) => bind.eq_ignore_ascii_case(address),
    }
}

/// Another user whose agent is on this host: running, or named by a system
/// daemon's plist.
fn foreign_owner(found: &Discovery) -> Option<&str> {
    [found.running_as.as_str(), found.daemon_user.as_str()]
        .into_iter()
        .find(|owner| !owner.is_empty() && *owner != found.user)
}

fn host_part(target: &str) -> &str {
    target.rsplit_once('@').map_or(target, |(_, host)| host)
}

/// The host's own short name, else the address.
fn default_name(found: &Discovery, address: &str) -> String {
    let short = found.hostname.split('.').next().unwrap_or_default().trim();
    if short.is_empty() {
        address.into()
    } else {
        short.into()
    }
}

/// Every address `address` names, from here.
fn resolve(address: &str) -> Vec<IpAddr> {
    let bare = address
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(address);
    if let Ok(ip) = bare.parse() {
        return vec![ip];
    }
    (bare, 0)
        .to_socket_addrs()
        .map(|found| found.map(|socket| socket.ip()).collect())
        .unwrap_or_default()
}

fn discover(ssh: &Ssh, target: &str) -> Result<Discovery, String> {
    let run = ssh.run(target, &["discover"])?;
    if run.dropped() {
        return Err(unreachable(target, &run.stderr));
    }
    if run.code != Some(0) {
        return Err(format!(
            "the discovery step on {target} failed: {}",
            run.stderr.trim()
        ));
    }
    parse_discovery(&run.stdout).map_err(|message| format!("{target}: {message}"))
}

/// The agent's token, from the one mode that prints it. Whatever else comes
/// back is reported without being shown: it may hold the token.
fn collect(ssh: &Ssh, target: &str) -> Result<Secret, String> {
    let run = ssh.run(target, &["collect"])?;
    if run.dropped() {
        return Err(unreachable(target, &run.stderr));
    }
    if run.code != Some(0) {
        return Err(format!(
            "could not read the agent's token on {target}: {}",
            run.stderr.trim()
        ));
    }
    let mut lines = run.stdout.lines().filter(|line| !line.is_empty());
    match (
        lines.next().and_then(|line| line.strip_prefix("token=")),
        lines.next(),
    ) {
        (Some(token), None) if !token.is_empty() && !token.contains(char::is_whitespace) => {
            Ok(Secret::new(token))
        }
        _ => Err(format!(
            "{target} answered the token read with output this command does not recognise \
             (not shown: it may hold the token)."
        )),
    }
}

/// Restarting a system daemon: root's, so printed and never run.
const DAEMON_RESTART: &str = "sudo launchctl kickstart -k system/app.solador.agent";

/// How to look at an agent that did not come back, as `update.rs`'s
/// `inspect_hint` says it.
fn inspect(service: Service) -> &'static str {
    match service {
        Service::SystemdUser => {
            "`systemctl --user status solador-agent.service` and \
             `journalctl --user -u solador-agent.service`"
        }
        Service::LaunchAgent => {
            "`launchctl print gui/$(id -u)/app.solador.agent` (its log path is in \
             ~/Library/LaunchAgents/app.solador.agent.plist)"
        }
        Service::LaunchDaemon => "`launchctl print system/app.solador.agent`",
        Service::None => "its service manager",
    }
}

macro_rules! say {
    ($err:expr, $($arg:tt)*) => {
        let _ = writeln!($err, $($arg)*);
    };
}

/// A refusal or a failure: the sentence on `err`, exit 1.
fn refuse(err: &mut dyn Write, message: &str) -> i32 {
    say!(err, "{message}");
    exit::FAILED
}

/// What changed on the host, said after a refusal too: a host the command
/// changed and then could not pair is exactly when the operator needs it.
fn report_changes(err: &mut dyn Write, target: &str, changed: &[String]) {
    for change in changed {
        say!(err, "On {target}: {change}.");
    }
}

/// Runs `host add --ssh`. The result goes to `out`; progress, notes and every
/// refusal to `err`. Never writes the token to either.
pub fn add(
    args: &AddArgs,
    ssh: &Ssh,
    store_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    add_with_settle(args, ssh, store_dir, out, err, SETTLE)
}

/// [`add`], giving a changed agent `settle` to come back: the tests' seam, so
/// an agent that never does is a test that takes milliseconds.
pub fn add_with_settle(
    args: &AddArgs,
    ssh: &Ssh,
    store_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
    settle: Duration,
) -> i32 {
    let target = args.target.as_str();
    let ask = |request: Request| channel::request(store_dir, &Envelope::new(request));
    let failed = |err: &mut dyn Write, error: ChannelError| {
        say!(err, "{error}");
        crate::cli::not_running_or_failed(&error)
    };

    // The app first, before any ssh: without it nothing could be saved, and
    // the host would be changed for nothing.
    if let Err(code) = crate::cli::hello(store_dir, err) {
        return code;
    }

    let address = match &args.address {
        Some(address) => address.trim().to_owned(),
        None => match ssh.config(target) {
            Ok(config) if config.proxied => {
                return refuse(
                    err,
                    &format!(
                        "{target} is reached through a ProxyJump or ProxyCommand, so its ssh \
                         HostName ({}) need not be an address this machine can dial. Pass --address.",
                        config.hostname
                    ),
                )
            }
            Ok(config) => config.hostname,
            Err(message) => return refuse(err, &message),
        },
    };
    let dial = resolve(&address);

    say!(err, "==> Looking at the agent on {target}");
    let mut found = match discover(ssh, target) {
        Ok(found) => found,
        Err(message) => return refuse(err, &message),
    };
    let plan = match plan(&found, args, &address, &dial) {
        Ok(plan) => plan,
        Err(message) => return refuse(err, &format!("{message} Nothing was changed on {target}.")),
    };
    for note in &plan.notes {
        say!(err, "note: {note}");
    }

    if args.dry_run {
        let now = if found.version.is_empty() {
            String::new()
        } else {
            format!(" (agent {} there now)", found.version)
        };
        let _ = writeln!(
            out,
            "Would pair {} at {}:{} with Solador{now}.",
            plan.name, plan.address, plan.port
        );
        if plan.steps.is_empty() {
            let _ = writeln!(out, "Nothing would change on {target}.");
        }
        for step in &plan.steps {
            let _ = writeln!(out, "On {target}, first: {}.", step.describe());
        }
        if let Ok(Response::Hosts { hosts }) = ask(Request::List) {
            if let Some(host) = hosts.iter().find(|host| {
                host.address.eq_ignore_ascii_case(&plan.address) && host.port == plan.port
            }) {
                let pinned = host.fingerprint.as_deref().map_or_else(
                    || "unpaired".to_owned(),
                    |fingerprint| format!("pinned to {fingerprint}"),
                );
                let _ = writeln!(
                    out,
                    "Solador already has {} at {}:{} ({pinned}): pairing it again needs --replace \
                     unless it already holds this certificate.",
                    host.name, host.address, host.port
                );
            }
        }
        return exit::OK;
    }

    let mut changed = Vec::new();
    for step in &plan.steps {
        say!(err, "==> On {target}: {}", step.describe());
        let mode = match *step {
            Step::Install {
                enable_tls,
                bind_all,
            } => {
                let mut mode = vec!["install"];
                if let Some(git_ref) = &args.git_ref {
                    mode.extend(["--ref", git_ref.as_str()]);
                }
                if enable_tls {
                    mode.push("--enable-tls");
                }
                if bind_all {
                    mode.push("--bind-all");
                }
                mode
            }
            Step::Rebind => vec!["rebind"],
        };
        let run = match *step {
            // The installer's own output, relayed as it runs: it reads the
            // token silently and never prints it.
            Step::Install { .. } => ssh.run_relaying(target, &mode, Some(&mut *err)),
            Step::Rebind => ssh.run(target, &mode),
        };
        let run = match run {
            Ok(run) => run,
            // ssh never started: nothing ran on the host.
            Err(message) => return refuse(err, &message),
        };
        match *step {
            Step::Install { enable_tls, .. } => {
                let _ = err.write_all(run.stderr.as_bytes());
                if run.code != Some(0) {
                    let how = if run.dropped() {
                        "the ssh connection dropped".to_owned()
                    } else {
                        run.code
                            .map_or_else(|| "killed".to_owned(), |code| format!("exit {code}"))
                    };
                    let service = match found.service {
                        Service::None if found.os == "Darwin" => Service::LaunchAgent,
                        Service::None => Service::SystemdUser,
                        service => service,
                    };
                    return refuse(
                        err,
                        &format!(
                            "The installer on {target} did not finish ({how}); its output is \
                             above, and it may have changed {target} partway: the agent's env \
                             file there is ~/.config/solador-agent.env, its binary \
                             ~/.local/bin/solador-agent (the one it replaced, if any, beside it \
                             as .prev); look at its service with {}. Run this command with \
                             --dry-run to see where it stands. Nothing was added to Solador.",
                            inspect(service)
                        ),
                    );
                }
                changed.push(
                    if enable_tls {
                        "the installer re-ran and turned TLS on"
                    } else {
                        "the agent was installed"
                    }
                    .to_owned(),
                );
            }
            Step::Rebind => {
                if let Err(code) = rebound(&run, &found, target, &mut changed, err) {
                    return code;
                }
            }
        }
    }

    if !plan.steps.is_empty() {
        // Read the host again: what the steps left, and the certificate a
        // just-restarted agent writes as it starts.
        let deadline = Instant::now() + settle;
        loop {
            found = match discover(ssh, target) {
                Ok(found) => found,
                Err(message) => {
                    report_changes(err, target, &changed);
                    return refuse(err, &message);
                }
            };
            if (found.tls && found.fingerprint.is_some()) || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(SETTLE_EVERY.min(settle));
        }
        let problem = if !found.tls {
            Some("TLS is still off there: the release it installed predates TLS.".to_owned())
        } else if found.fingerprint.is_none() {
            Some(format!(
                "its agent has not written a certificate, so it may not be running. Look at it \
                 with {}.",
                inspect(found.service)
            ))
        } else if !covers(&effective_bind(&found), &address, &dial) {
            Some(format!(
                "its agent still listens on {}, which {address} is not.",
                effective_bind(&found)
            ))
        } else {
            None
        };
        if let Some(problem) = problem {
            report_changes(err, target, &changed);
            return refuse(
                err,
                &format!("After that, on {target}, {problem} Nothing was added to Solador."),
            );
        }

        // A restarted agent takes a moment to listen again. Only an answer
        // over TLS ends the wait; what answers is judged by the add.
        let deadline = Instant::now() + settle;
        loop {
            let last = match ask(Request::Probe {
                address: plan.address.clone(),
                port: plan.port,
            }) {
                Ok(Response::Probe {
                    probe: ProbeSummary::Tls { .. },
                }) => break,
                Ok(Response::Probe {
                    probe: ProbeSummary::NoTls,
                }) => "it answers plain HTTP there".to_owned(),
                Ok(Response::Probe {
                    probe: ProbeSummary::Failed { message },
                }) => message,
                Ok(other) => format!("unexpected reply: {other:?}"),
                Err(error) => {
                    report_changes(err, target, &changed);
                    return failed(err, error);
                }
            };
            if Instant::now() >= deadline {
                report_changes(err, target, &changed);
                return refuse(
                    err,
                    &format!(
                        "After that, the agent on {target} did not answer over TLS at {}:{} \
                         within {settle:?} ({last}). Look at it with {}. Nothing was added to \
                         Solador.",
                        plan.address,
                        plan.port,
                        inspect(found.service)
                    ),
                );
            }
            std::thread::sleep(SETTLE_EVERY.min(settle));
        }
    }
    let fingerprint = found.fingerprint.clone().expect("checked above");

    let token = match collect(ssh, target) {
        Ok(token) => token,
        Err(message) => {
            report_changes(err, target, &changed);
            return refuse(err, &message);
        }
    };

    say!(
        err,
        "==> Pairing {} at {}:{} through Solador",
        plan.name,
        plan.address,
        plan.port
    );
    let request = Request::Add(AddRequest {
        name: plan.name.clone(),
        address: plan.address.clone(),
        port: plan.port,
        token,
        expected_fingerprint: fingerprint,
        replace: args.replace,
    });
    match ask(request) {
        Ok(Response::Paired(paired)) => {
            let _ = writeln!(out, "{}", render_paired(&paired));
            report_changes(err, target, &changed);
            exit::OK
        }
        Ok(Response::Refused { message }) => {
            say!(err, "{message}");
            report_changes(err, target, &changed);
            exit::FAILED
        }
        Ok(other) => {
            say!(err, "unexpected reply: {other:?}");
            report_changes(err, target, &changed);
            exit::FAILED
        }
        Err(error) => {
            report_changes(err, target, &changed);
            failed(err, error)
        }
    }
}

/// What a `rebind` run did, said whatever it was: `Ok` when the agent now
/// listens everywhere and was restarted, else the exit status, its reason
/// already on `err`. The way back is named every time the file changed.
fn rebound(
    run: &Run,
    found: &Discovery,
    target: &str,
    changed: &mut Vec<String>,
    err: &mut dyn Write,
) -> Result<(), i32> {
    let previous = run.field("previous_bind");
    let was = if previous.is_empty() {
        "SOLADOR_AGENT_BIND was unset".to_owned()
    } else {
        format!("SOLADOR_AGENT_BIND was {previous}")
    };
    let auto = if run.field("previous_bind_auto") == "1" {
        ", and SOLADOR_AGENT_BIND_AUTO=1 was removed with it"
    } else {
        ""
    };
    let undo = if previous.is_empty() {
        "remove the SOLADOR_AGENT_BIND line and restart to undo".to_owned()
    } else {
        format!("put SOLADOR_AGENT_BIND={previous} back and restart to undo")
    };
    let rewritten = run.field("rewritten") == "yes";
    let host = host_part(target);
    let said = run.stderr.trim();
    match run.code {
        Some(0) if rewritten => {
            changed.push(format!(
                "its agent now listens on all interfaces; {was}{auto} ({undo})"
            ));
            Ok(())
        }
        // Exit 3 is believed only from the one install it can be true of,
        // and the step is this command's own words, never the host's.
        Some(3) if rewritten && found.service == Service::LaunchDaemon => {
            say!(
                err,
                "{target}'s env file now sets SOLADOR_AGENT_BIND=0.0.0.0 ({was}{auto}; {undo}), \
                 but the agent there is a system daemon, which only root can restart. Run this \
                 on {host} as an administrator, then run this command again:\n  {DAEMON_RESTART}"
            );
            Err(exit::ROOT_STEP)
        }
        _ if rewritten => {
            changed.push(format!(
                "its env file now sets SOLADOR_AGENT_BIND=0.0.0.0 ({was}{auto}), but the restart \
                 did not complete, so the agent may be stopped or still on its old bind ({undo})"
            ));
            report_changes(err, target, changed);
            let why = if run.dropped() {
                "The ssh connection dropped during the restart."
            } else {
                said
            };
            Err(refuse(
                err,
                &format!(
                    "{why} Look at it with {}. Nothing was added to Solador.",
                    inspect(found.service)
                ),
            ))
        }
        _ if run.dropped() => {
            let detail = if said.is_empty() {
                String::new()
            } else {
                format!(" ({said})")
            };
            report_changes(err, target, changed);
            Err(refuse(
                err,
                &format!(
                    "The ssh connection to {target} dropped during the rebind{detail}, and \
                     {target} may have changed. Run this command with --dry-run to see where it \
                     stands. Nothing was added to Solador."
                ),
            ))
        }
        // The script's own refusals all come before the rewrite.
        _ => {
            let said = if said.is_empty() {
                "The rebind was refused there."
            } else {
                said
            };
            Err(refuse(
                err,
                &format!("{said} Nothing was changed on {target}."),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";
    const LAN: &str = "192.168.1.2";
    const TAILNET: &str = "100.64.0.9";

    /// A healthy per-user Linux agent listening on all interfaces, as
    /// `discover` prints it, with `overrides` replacing or adding lines.
    fn found(overrides: &[&str]) -> Discovery {
        let mut lines: BTreeMap<String, String> = [
            "os=Linux",
            "hostname=agent-1.lan",
            "user=svc",
            "env=present",
            "tls=1",
            "bind=0.0.0.0",
            "bind_auto=",
            "require_tailnet=",
            "port=",
            "token=present",
            "bin=present",
            "version=2026.10.18",
            &format!("fingerprint={PIN}"),
            "service=systemd-user",
            "running_as=svc",
            "tailnet_ip=",
            "have_minisign=yes",
            "have_curl=yes",
            "have_bash=yes",
        ]
        .iter()
        .chain(overrides)
        .map(|line| {
            let (key, value) = line.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect();
        if overrides.contains(&"fingerprint=") {
            lines.remove("fingerprint");
        }
        let text: String = lines.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        parse_discovery(&text).unwrap()
    }

    fn args(flags: &[&str]) -> AddArgs {
        AddArgs {
            target: "svc@agent-1".into(),
            address: None,
            port: None,
            name: None,
            install: flags.contains(&"--install"),
            bind_all: flags.contains(&"--bind-all"),
            enable_tls: flags.contains(&"--enable-tls"),
            replace: false,
            git_ref: None,
            dry_run: false,
        }
    }

    fn lan() -> Vec<IpAddr> {
        vec![LAN.parse().unwrap()]
    }

    fn steps(found: &Discovery, flags: &[&str]) -> Result<Vec<Step>, String> {
        plan(found, &args(flags), LAN, &lan()).map(|plan| plan.steps)
    }

    #[test]
    fn ssh_config_names_the_host_and_whether_it_is_proxied() {
        let direct = "user svc\nhostname 192.168.1.2\nport 22\nproxyusefdpass no\n";
        assert_eq!(
            parse_config(direct),
            Some(SshConfig {
                hostname: LAN.into(),
                proxied: false
            })
        );
        let jumped = "hostname agent-1.internal\nproxyjump bastion\n";
        assert!(parse_config(jumped).unwrap().proxied);
        assert!(
            !parse_config("hostname h\nproxycommand none\n")
                .unwrap()
                .proxied
        );
        assert_eq!(parse_config("user svc\n"), None);
    }

    #[test]
    fn discovery_reads_every_field_and_refuses_output_that_is_not_its_own() {
        let found = found(&[
            "port=7979",
            "service=launch-daemon",
            "daemon_user=svc",
            "gui=yes",
        ]);
        assert_eq!(found.port, Some(7979));
        assert_eq!(found.service, Service::LaunchDaemon);
        assert_eq!(found.fingerprint.as_deref(), Some(PIN));
        assert!(found.gui && found.tls && found.token);
        assert!(parse_discovery("Welcome to the host!\n").is_err());
        assert!(parse_discovery("os=Linux\nuser=svc\nport=99999\n").is_err());
    }

    #[test]
    fn a_ready_agent_needs_nothing_and_takes_the_hosts_short_name() {
        let plan = plan(&found(&[]), &args(&[]), LAN, &lan()).unwrap();
        assert_eq!(plan.steps, vec![]);
        assert_eq!((plan.name.as_str(), plan.port), ("agent-1", 7878));
        let custom = found(&["port=7979"]);
        assert_eq!(plan_port(&custom), 7979, "the agent's own port");
    }

    fn plan_port(found: &Discovery) -> u16 {
        plan(found, &args(&[]), LAN, &lan()).unwrap().port
    }

    #[test]
    fn a_bind_that_misses_the_address_needs_bind_all_and_then_rebinds() {
        let tailnet_only = found(&[&format!("bind={TAILNET}")]);
        let refused = steps(&tailnet_only, &[]).unwrap_err();
        assert!(
            refused.contains("--bind-all") && refused.contains(TAILNET),
            "{refused}"
        );
        assert_eq!(
            steps(&tailnet_only, &["--bind-all"]),
            Ok(vec![Step::Rebind])
        );
        // Dialled at the address it already listens on, it needs nothing.
        let at_tailnet = plan(
            &tailnet_only,
            &args(&[]),
            TAILNET,
            &[TAILNET.parse().unwrap()],
        );
        assert_eq!(at_tailnet.unwrap().steps, vec![]);
    }

    #[test]
    fn an_unset_bind_is_the_tailnet_address_when_there_is_one() {
        let auto = found(&["bind=", &format!("tailnet_ip={TAILNET}")]);
        assert!(steps(&auto, &[]).unwrap_err().contains(TAILNET));
        let no_tailnet = found(&["bind="]);
        assert_eq!(steps(&no_tailnet, &[]), Ok(vec![]));
    }

    #[test]
    fn require_tailnet_is_never_undone_whatever_the_flags() {
        let held = found(&[&format!("bind={TAILNET}"), "require_tailnet=1"]);
        let refused = steps(&held, &["--bind-all", "--enable-tls", "--install"]).unwrap_err();
        assert!(
            refused.contains("SOLADOR_AGENT_REQUIRE_TAILNET=1"),
            "{refused}"
        );
        // Pairing it where it already listens is still fine.
        let at_tailnet = plan(&held, &args(&[]), TAILNET, &[TAILNET.parse().unwrap()]);
        assert_eq!(at_tailnet.unwrap().steps, vec![]);
    }

    #[test]
    fn tls_off_needs_enable_tls_which_re_runs_the_installer() {
        let plain = found(&["tls=", "fingerprint=", "fingerprint_status=missing"]);
        let refused = steps(&plain, &[]).unwrap_err();
        assert!(refused.contains("--enable-tls"), "{refused}");
        assert_eq!(
            steps(&plain, &["--enable-tls"]),
            Ok(vec![Step::Install {
                enable_tls: true,
                bind_all: false
            }])
        );
        // Off TLS and off the address: one installer run does both.
        let plain_tailnet = found(&["tls=", "fingerprint=", &format!("bind={TAILNET}")]);
        assert_eq!(
            steps(&plain_tailnet, &["--enable-tls", "--bind-all"]),
            Ok(vec![Step::Install {
                enable_tls: true,
                bind_all: true
            }])
        );
        // A system daemon's installer re-run owes a root step this command
        // does not take.
        let daemon = found(&["tls=", "service=launch-daemon", "os=Darwin"]);
        assert!(steps(&daemon, &["--enable-tls"])
            .unwrap_err()
            .contains("--system-daemon"));
    }

    #[test]
    fn a_binary_that_predates_tls_is_tls_off_whatever_the_env_file_says() {
        // SOLADOR_AGENT_TLS=1 beside a release that does not know the
        // subcommand: rebinding it would put plain HTTP on every interface.
        let stale = found(&[
            "tls=1",
            "fingerprint=",
            "fingerprint_status=unsupported",
            &format!("bind={TAILNET}"),
        ]);
        let refused = steps(&stale, &["--bind-all"]).unwrap_err();
        assert!(refused.contains("predates TLS"), "{refused}");
        assert!(refused.contains("--enable-tls"), "{refused}");
        // And the flag it names is the one that gets past it.
        assert_eq!(
            steps(&stale, &["--bind-all", "--enable-tls"]),
            Ok(vec![Step::Install {
                enable_tls: true,
                bind_all: true
            }])
        );
    }

    #[test]
    fn a_launch_agent_with_no_login_session_is_refused_before_anything_changes() {
        let away = found(&[
            "os=Darwin",
            "service=launch-agent",
            "gui=no",
            &format!("bind={TAILNET}"),
        ]);
        let refused = steps(&away, &["--bind-all"]).unwrap_err();
        assert!(refused.contains("no login session"), "{refused}");
        // Nothing to restart, nothing refused: a session is needed only to
        // change something.
        let ready = found(&["os=Darwin", "service=launch-agent", "gui=no"]);
        assert_eq!(steps(&ready, &[]), Ok(vec![]));
        let present = found(&[
            "os=Darwin",
            "service=launch-agent",
            "gui=yes",
            &format!("bind={TAILNET}"),
        ]);
        assert_eq!(steps(&present, &["--bind-all"]), Ok(vec![Step::Rebind]));
    }

    #[test]
    fn no_agent_needs_install_and_another_users_agent_is_never_shadowed() {
        let none = found(&["env=absent", "bin=absent", "running_as=", "fingerprint="]);
        assert!(steps(&none, &[]).unwrap_err().contains("--install"));
        assert_eq!(
            steps(&none, &["--install"]),
            Ok(vec![Step::Install {
                enable_tls: false,
                bind_all: false
            }])
        );
        let theirs = found(&["env=absent", "bin=absent", "running_as=agents"]);
        let refused = steps(&theirs, &["--install"]).unwrap_err();
        assert!(refused.contains("--ssh agents@agent-1"), "{refused}");
        let daemon = found(&[
            "env=absent",
            "bin=absent",
            "running_as=",
            "daemon_user=agents",
        ]);
        assert!(steps(&daemon, &["--install"])
            .unwrap_err()
            .contains("agents@"));
    }

    #[test]
    fn a_fresh_install_is_refused_where_it_could_not_work() {
        let none = |extra: &str| {
            found(&[
                "env=absent",
                "bin=absent",
                "running_as=",
                "fingerprint=",
                extra,
            ])
        };
        let tailnet = none(&format!("tailnet_ip={TAILNET}"));
        assert!(steps(&tailnet, &["--install"])
            .unwrap_err()
            .contains("--bind-all"));
        assert_eq!(
            steps(&tailnet, &["--install", "--bind-all"]),
            Ok(vec![Step::Install {
                enable_tls: false,
                bind_all: true
            }])
        );
        assert!(steps(&none("have_minisign=no"), &["--install"])
            .unwrap_err()
            .starts_with("minisign isn't installed"));
        let headless_mac = found(&[
            "env=absent",
            "bin=absent",
            "running_as=",
            "os=Darwin",
            "gui=no",
        ]);
        assert!(steps(&headless_mac, &["--install"])
            .unwrap_err()
            .contains("system daemon"));
    }

    #[test]
    fn a_provisional_wildcard_is_noted_and_bind_all_makes_it_explicit() {
        let provisional = found(&["bind_auto=1"]);
        let plan = plan(&provisional, &args(&[]), LAN, &lan()).unwrap();
        assert_eq!(plan.steps, vec![]);
        assert!(
            plan.notes[0].contains("SOLADOR_AGENT_BIND_AUTO=1"),
            "{:?}",
            plan.notes
        );
        assert_eq!(steps(&provisional, &["--bind-all"]), Ok(vec![Step::Rebind]));
    }

    #[test]
    fn an_address_that_does_not_resolve_is_refused_before_anything() {
        let refused = plan(&found(&[]), &args(&[]), "nowhere.invalid", &[]).unwrap_err();
        assert!(refused.contains("--address"), "{refused}");
    }

    #[test]
    fn a_running_tls_agent_without_a_certificate_is_not_paired_blind() {
        let starting = found(&["fingerprint=", "fingerprint_status=missing"]);
        assert!(steps(&starting, &[])
            .unwrap_err()
            .contains("Is it running?"));
        let old = found(&["fingerprint=", "fingerprint_status=unsupported"]);
        assert!(steps(&old, &[]).unwrap_err().contains("--enable-tls"));
    }

    /// The service names this file prints (the root step, the inspect hints)
    /// are the script's own. Where the script looks is held to where the
    /// installer installs by agent/deploy/lib_test.sh, which runs when either
    /// file changes.
    #[test]
    fn the_commands_own_words_name_the_scripts_label_and_unit() {
        assert!(SCRIPT.contains("LABEL=app.solador.agent\n"));
        assert!(SCRIPT.contains("UNIT=solador-agent.service\n"));
        assert!(DAEMON_RESTART.ends_with(" system/app.solador.agent"));
        for service in [Service::LaunchAgent, Service::LaunchDaemon] {
            assert!(inspect(service).contains("/app.solador.agent"));
        }
        assert!(inspect(Service::SystemdUser).contains("solador-agent.service"));
    }

    #[test]
    fn host_text_cannot_repaint_the_terminal() {
        assert_eq!(printable("ok\n\tline\x1b[2Jx\r"), "ok\n\tline?[2Jx?");
        let found = parse_discovery("os=Linux\nuser=svc\nhostname=evil\x1b]0;x\x07\n").unwrap();
        assert_eq!(found.hostname, "evil?]0;x?");
    }

    #[test]
    fn the_script_is_the_remote_half_and_is_small_enough_for_one_pipe_write() {
        assert!(SCRIPT.starts_with("#!/bin/sh"));
        for mode in ["discover)", "rebind)", "install)", "collect)"] {
            assert!(SCRIPT.contains(mode), "{mode}");
        }
        assert!(SCRIPT.len() < 16 * 1024);
    }
}
