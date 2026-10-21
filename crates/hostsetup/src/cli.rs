//! `solador-app host …`: the app's executable in command mode.
//!
//! Dispatched from the first line of `main()`, before anything else the app
//! does at launch. In command mode nothing else runs — no store open (whose
//! migration would write the file the running app owns), no credential store,
//! no crash reporter, no window. Every change goes through the running app.

#[cfg(any(unix, test))]
use std::io::Read;

#[cfg(unix)]
use crate::protocol::{AddRequest, Envelope, Request, Response, PROTOCOL};
use crate::protocol::{HostSummary, Outcome, Paired};
#[cfg(any(unix, test))]
use crate::Secret;

/// Exit statuses, so a script (or a coding agent) can tell the outcomes apart.
pub mod exit {
    pub const OK: i32 = 0;
    /// Refused, or failed: the message says which, and what changed (nothing,
    /// unless it says otherwise).
    pub const FAILED: i32 = 1;
    pub const USAGE: i32 = 2;
    /// Solador is not running, so nothing could be handed to it.
    pub const NOT_RUNNING: i32 = 4;
}

pub const HELP: &str = "\
solador-app host — add and pair remote hosts through the running Solador

USAGE
  solador-app host list [--json]
  solador-app host pair --address <addr> --fingerprint <sha256> --token-stdin
                        [--port <port>] [--name <name>] [--replace]

  Symlinked as `solador`, the same commands run without the leading
  `solador-app`: ln -s /Applications/Solador.app/Contents/MacOS/solador-app ~/.local/bin/solador

COMMANDS
  list   Every configured host: name, address, and the certificate it is
         pinned to. --json prints the same as JSON. Never prints a token.
  pair   Pair a host whose agent serves TLS. Solador fetches the certificate
         at <addr>:<port> itself and pins it only if its fingerprint is the
         one given (what `solador-agent tls-fingerprint` printed on the host);
         it then checks the token over that pinned connection and saves only
         if the agent accepts it. The token is read from stdin, never from an
         argument:
           ssh host 'grep ^SOLADOR_AGENT_TOKEN= ~/.config/solador-agent.env | cut -d= -f2-' \\
             | solador-app host pair --address 192.168.1.2 --name host \\
                 --fingerprint AB:CD:… --token-stdin
         A host already at that address (unpaired, or pinned to another
         certificate), or this certificate already paired at another
         address, is refused unless --replace is given.

  Solador must be open: it is the only writer of its hosts and the keychain.

EXIT STATUS
  0 done (or already so)   1 refused or failed   2 usage
  4 Solador isn't running
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    List { json: bool },
    Pair(PairArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairArgs {
    pub address: String,
    pub port: u16,
    pub name: String,
    pub fingerprint: String,
    pub replace: bool,
}

/// Whether this invocation is the command mode, and if so its exit status.
///
/// `None` means "launch the app as usual". Command mode is a first argument of
/// `host`, `help`, `--help` or `-h`, or an executable named `solador` (the
/// documented symlink), which never opens a window.
#[must_use]
pub fn dispatch(args: &[String]) -> Option<i32> {
    let invoked_as_solador = args.first().is_some_and(|argv0| {
        std::path::Path::new(argv0)
            .file_stem()
            .is_some_and(|stem| stem == "solador")
    });
    let rest = args.get(1..).unwrap_or_default();
    let first = rest.first().map(String::as_str);
    if !invoked_as_solador && !matches!(first, Some("host" | "help" | "--help" | "-h")) {
        return None;
    }
    Some(match parse(rest) {
        Ok(Command::Help) => {
            print!("{HELP}");
            exit::OK
        }
        Ok(command) => run(command),
        Err(message) => {
            eprintln!("{message}\n\n{HELP}");
            exit::USAGE
        }
    })
}

/// Parses everything after the executable.
///
/// # Errors
/// A one-line usage error.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["help" | "--help" | "-h", ..] | ["host"] | ["host", "help" | "--help" | "-h"] => {
            Ok(Command::Help)
        }
        ["host", "list", flags @ ..] => match flags {
            [] => Ok(Command::List { json: false }),
            ["--json"] => Ok(Command::List { json: true }),
            _ => Err(format!("host list: unexpected {}", flags.join(" "))),
        },
        ["host", "pair", flags @ ..] => parse_pair(flags).map(Command::Pair),
        ["host", other, ..] => Err(format!("unknown host command: {other}")),
        [other, ..] => Err(format!("unknown command: {other}")),
    }
}

fn parse_pair(flags: &[&str]) -> Result<PairArgs, String> {
    let (mut address, mut port, mut name, mut fingerprint) = (None, None, None, None);
    let (mut token_stdin, mut replace) = (false, false);
    let mut iter = flags.iter();
    while let Some(&flag) = iter.next() {
        let mut value = || {
            iter.next()
                .map(|v| (*v).to_owned())
                .ok_or(format!("host pair: {flag} needs a value"))
        };
        match flag {
            "--address" => address = Some(value()?),
            "--port" => {
                let raw = value()?;
                port = Some(
                    raw.parse::<u16>()
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or(format!("host pair: --port {raw} is not a port"))?,
                );
            }
            "--name" => name = Some(value()?),
            "--fingerprint" => fingerprint = Some(value()?),
            "--token-stdin" => token_stdin = true,
            "--replace" => replace = true,
            // The one argument shape this command must never accept: a token
            // in argv is in `ps`, in shell history and in a coding agent's
            // transcript.
            "--token" => {
                return Err(
                    "host pair: pass the token on stdin with --token-stdin, never as an argument"
                        .into(),
                )
            }
            other => return Err(format!("host pair: unexpected {other}")),
        }
    }
    let address = address.ok_or("host pair: --address is required")?;
    let fingerprint = fingerprint.ok_or("host pair: --fingerprint is required")?;
    if !token_stdin {
        return Err("host pair: --token-stdin is required (the token is read from stdin)".into());
    }
    Ok(PairArgs {
        name: name.unwrap_or_else(|| address.clone()),
        address,
        port: port.unwrap_or(store::DEFAULT_AGENT_PORT),
        fingerprint,
        replace,
    })
}

#[cfg(unix)]
fn run(command: Command) -> i32 {
    match crate::channel::store_dir() {
        Ok(dir) => run_in(&dir, command, read_token),
        Err(message) => {
            eprintln!("{message}");
            exit::FAILED
        }
    }
}

/// [`run`] against the running app whose store is `store_dir`, with the token
/// read by `token` (stdin, in the product).
#[cfg(unix)]
fn run_in(
    store_dir: &std::path::Path,
    command: Command,
    token: impl FnOnce() -> Result<Secret, String>,
) -> i32 {
    use crate::channel::{self, ChannelError};

    let ask = |request: Request| channel::request(store_dir, &Envelope::new(request));
    let failed = |error: ChannelError| {
        eprintln!("{error}");
        match error {
            ChannelError::NotRunning => exit::NOT_RUNNING,
            _ => exit::FAILED,
        }
    };

    // First, before anything else is read or sent: is the app there, and does
    // it speak this command's protocol?
    match ask(Request::Hello) {
        Ok(Response::Hello { protocol, .. }) if protocol == PROTOCOL => {}
        Ok(Response::Hello { protocol, .. }) => {
            eprintln!("{}", crate::server::version_skew(protocol));
            return exit::FAILED;
        }
        Ok(other) => {
            eprintln!("unexpected reply to hello: {}", describe(&other));
            return exit::FAILED;
        }
        Err(error) => return failed(error),
    }

    match command {
        Command::Help => unreachable!("handled by dispatch"),
        Command::List { json } => match ask(Request::List) {
            Ok(Response::Hosts { hosts }) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&hosts).expect("serialises")
                    );
                } else {
                    print!("{}", render_hosts(&hosts));
                }
                exit::OK
            }
            Ok(other) => refused(&other),
            Err(error) => failed(error),
        },
        Command::Pair(args) => {
            let token = match token() {
                Ok(token) => token,
                Err(message) => {
                    eprintln!("{message}");
                    return exit::USAGE;
                }
            };
            let request = Request::Add(AddRequest {
                name: args.name,
                address: args.address,
                port: args.port,
                token,
                expected_fingerprint: args.fingerprint,
                replace: args.replace,
            });
            match ask(request) {
                Ok(Response::Paired(paired)) => {
                    println!("{}", render_paired(&paired));
                    exit::OK
                }
                Ok(other) => refused(&other),
                Err(error) => failed(error),
            }
        }
    }
}

#[cfg(not(unix))]
fn run(_command: Command) -> i32 {
    eprintln!("solador-app host is not available on this platform yet (macOS only for now).");
    exit::FAILED
}

#[cfg(unix)]
fn refused(response: &Response) -> i32 {
    eprintln!("{}", describe(response));
    exit::FAILED
}

#[cfg(unix)]
fn describe(response: &Response) -> String {
    match response {
        Response::Refused { message } => message.clone(),
        other => format!("unexpected reply: {other:?}"),
    }
}

/// The token, from stdin and nowhere else. A terminal on stdin is refused
/// rather than prompted: typing a token there echoes it into the scrollback
/// (and into a coding agent's transcript, when one is driving).
#[cfg(unix)]
fn read_token() -> Result<Secret, String> {
    use std::io::IsTerminal;

    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    token_from(stdin, terminal)
}

/// [`read_token`]'s rules over any reader, so they are testable without a
/// terminal or a pipe.
#[cfg(any(unix, test))]
fn token_from(mut input: impl Read, terminal: bool) -> Result<Secret, String> {
    if terminal {
        return Err(
            "--token-stdin reads the token from a pipe, not a terminal: \
             pipe it in (see `solador-app host --help`)."
                .into(),
        );
    }
    let mut raw = String::new();
    input
        .read_to_string(&mut raw)
        .map_err(|error| format!("could not read the token from stdin: {error}"))?;
    let token = raw.trim();
    if token.is_empty() {
        return Err("no token on stdin".into());
    }
    if token.contains(char::is_whitespace) {
        return Err("stdin held more than one token-shaped word; pipe in exactly the token".into());
    }
    Ok(Secret::new(token))
}

/// One host per line: name, address, and the pin (or `unpaired`).
#[must_use]
pub fn render_hosts(hosts: &[HostSummary]) -> String {
    if hosts.is_empty() {
        return "No hosts configured.\n".into();
    }
    let name_w = hosts
        .iter()
        .map(|h| h.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let addr = |h: &HostSummary| format!("{}:{}", h.address, h.port);
    let addr_w = hosts
        .iter()
        .map(|h| addr(h).chars().count())
        .max()
        .unwrap_or(0)
        .max(7);
    let mut out = format!("{:name_w$}  {:addr_w$}  PINNED TO\n", "NAME", "ADDRESS");
    for host in hosts {
        let pin = host
            .fingerprint
            .as_deref()
            .unwrap_or("unpaired (plain HTTP)");
        let off = if host.enabled { "" } else { "  (disabled)" };
        out.push_str(&format!(
            "{:name_w$}  {:addr_w$}  {pin}{off}\n",
            host.name,
            addr(host)
        ));
    }
    out
}

#[must_use]
pub fn render_paired(paired: &Paired) -> String {
    let host = &paired.host;
    let what = match paired.outcome {
        Outcome::Added => "Added",
        Outcome::Unchanged => "Already paired, nothing changed:",
        Outcome::TokenReplaced => "Already paired; replaced the stored token for",
        Outcome::Updated => "Re-paired",
    };
    format!(
        "{what} {} ({}:{}), pinned to {}\n{}",
        host.name,
        host.address,
        host.port,
        host.fingerprint.as_deref().unwrap_or("—"),
        paired.health
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_token_comes_from_a_pipe_as_exactly_one_word() {
        let read = |input: &str, terminal| token_from(input.as_bytes(), terminal);
        assert_eq!(read("abc123\n", false).unwrap().expose(), "abc123");
        // A terminal would echo what is typed: refused before anything is read.
        let refused = read("abc123\n", true).unwrap_err();
        assert!(refused.contains("not a terminal"), "{refused}");
        assert!(read(" \n", false).is_err());
        let two = read("SOLADOR_AGENT_TOKEN= abc123\n", false).unwrap_err();
        assert!(two.contains("exactly the token"), "{two}");
        assert!(
            !two.contains("abc123"),
            "a refusal must not echo stdin: {two}"
        );
    }

    /// Exit 4 is what a script (or a coding agent) branches on to say "open
    /// Solador", and `pair` reaches it before reading the token at all.
    #[cfg(unix)]
    #[test]
    fn with_no_app_running_every_command_exits_4_and_reads_no_token() {
        let dir = tempfile::TempDir::new().unwrap();
        let never =
            || -> Result<Secret, String> { panic!("read the token with no app to give it to") };
        assert_eq!(
            run_in(dir.path(), Command::List { json: false }, never),
            exit::NOT_RUNNING
        );
        let pair = Command::Pair(PairArgs {
            address: "192.168.1.2".into(),
            port: 7878,
            name: "agent".into(),
            fingerprint: "AB:CD".into(),
            replace: false,
        });
        assert_eq!(run_in(dir.path(), pair, never), exit::NOT_RUNNING);
    }

    #[test]
    fn only_host_help_or_the_solador_name_enter_command_mode() {
        // The GUI's own launches, and the existing dump flags, are untouched.
        assert_eq!(
            dispatch(&args(
                "/Applications/Solador.app/Contents/MacOS/solador-app"
            )),
            None
        );
        assert_eq!(dispatch(&args("solador-app --dump")), None);
        assert_eq!(dispatch(&args("solador-app --help")), Some(exit::OK));
        assert_eq!(
            dispatch(&args("/Users/me/.local/bin/solador")),
            Some(exit::OK)
        );
        assert_eq!(dispatch(&args("solador host nonsense")), Some(exit::USAGE));
    }

    #[test]
    fn pair_requires_an_address_a_fingerprint_and_the_token_on_stdin() {
        let full = parse(&args(
            "host pair --address 192.168.1.2 --fingerprint AA:BB --token-stdin --replace --port 7879",
        ))
        .unwrap();
        assert_eq!(
            full,
            Command::Pair(PairArgs {
                address: "192.168.1.2".into(),
                port: 7879,
                name: "192.168.1.2".into(),
                fingerprint: "AA:BB".into(),
                replace: true,
            })
        );
        assert!(parse(&args("host pair --fingerprint AA --token-stdin")).is_err());
        assert!(parse(&args("host pair --address a --token-stdin")).is_err());
        assert!(parse(&args("host pair --address a --fingerprint AA")).is_err());
        assert!(parse(&args(
            "host pair --address a --fingerprint AA --token-stdin --port 0"
        ))
        .is_err());
        assert!(parse(&args(
            "host pair --address a --fingerprint AA --token-stdin --port"
        ))
        .is_err());
    }

    #[test]
    fn a_token_argument_is_refused_without_echoing_it() {
        let error = parse(&args(
            "host pair --address a --fingerprint AA --token s3cret-value --token-stdin",
        ))
        .unwrap_err();
        assert!(error.contains("--token-stdin"), "{error}");
        assert!(!error.contains("s3cret"), "{error}");
    }

    #[test]
    fn list_and_help_parse() {
        assert_eq!(parse(&args("host list")), Ok(Command::List { json: false }));
        assert_eq!(
            parse(&args("host list --json")),
            Ok(Command::List { json: true })
        );
        assert_eq!(parse(&args("host")), Ok(Command::Help));
        assert_eq!(parse(&[]), Ok(Command::Help));
        assert!(parse(&args("host list --verbose")).is_err());
    }

    #[test]
    fn the_host_table_names_the_pin_or_says_unpaired() {
        let hosts = vec![
            HostSummary {
                id: uuid::Uuid::nil(),
                name: "agent-1".into(),
                address: "192.168.1.2".into(),
                port: 7878,
                enabled: true,
                fingerprint: Some("AB:CD".into()),
            },
            HostSummary {
                id: uuid::Uuid::nil(),
                name: "old".into(),
                address: "100.64.0.9".into(),
                port: 7878,
                enabled: false,
                fingerprint: None,
            },
        ];
        let table = render_hosts(&hosts);
        assert!(
            table.contains("agent-1  192.168.1.2:7878  AB:CD\n"),
            "{table}"
        );
        assert!(
            table.contains("unpaired (plain HTTP)  (disabled)"),
            "{table}"
        );
        assert_eq!(render_hosts(&[]), "No hosts configured.\n");
    }
}
