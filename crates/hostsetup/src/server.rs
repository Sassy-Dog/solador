//! The running app's side of the hand-off: one request in, one reply out.
//!
//! Generic over [`HostBook`] (the app's store and credential store) and
//! [`Agent`] (the app's agent client), so every rule here — the fingerprint
//! comparison above all — is tested without a network, a keychain or a window.

use std::future::Future;

use store::Host;
use uuid::Uuid;

use crate::pairing::{Decision, Pairing};
use crate::protocol::{
    AddRequest, Envelope, HostSummary, Outcome, Paired, ProbeSummary, Request, Response, PROTOCOL,
};
use crate::Secret;

/// The app's hosts, as the server may read and change them.
///
/// The app is the only writer of `store.json` and of the credential store, and
/// this trait is how it stays that way: the server decides, the app writes, and
/// the app's own reload makes the change live. One `apply` runs at a time (the
/// server serialises them).
pub trait HostBook: Send + Sync {
    fn hosts(&self) -> Vec<Host>;
    /// Whether the token stored for `id` is `token`. Compared inside the app;
    /// no token is ever returned.
    fn token_matches(&self, id: Uuid, token: &Secret) -> bool;
    /// Carries out `decided` for `pairing`, after [`Pairing::confirm`] against
    /// the host list read under the lock these writes take:
    ///
    /// * [`Decision::Add`]: a new row, pinned to `pairing.pin`, its token
    ///   stored;
    /// * [`Decision::SamePairing`] / [`Decision::Replace`]: row `id` pointed at
    ///   `address:port`, pinned to the pin, its token replaced. Its name,
    ///   enabled state and hidden volumes are kept.
    ///
    /// `Err` says what was and was not written: a row saved whose token the
    /// credential store then refused is not "nothing changed".
    fn apply(&self, pairing: &Pairing<'_>, decided: &Decision) -> Result<Host, String>;
}

/// The two things the server asks an agent, through the app's own client.
pub trait Agent: Send + Sync {
    /// What `address:port` presents, trusted for nothing.
    fn probe(&self, address: &str, port: u16) -> impl Future<Output = ProbeSummary> + Send;
    /// One authenticated `/v1/health` over a connection pinned to `pin`.
    /// `Ok` and `Err` both carry the Settings **Test** line; only `Ok` means
    /// the agent accepted `token` behind that certificate.
    fn health(
        &self,
        address: &str,
        port: u16,
        pin: &str,
        token: &Secret,
    ) -> impl Future<Output = Result<String, String>> + Send;
}

/// What the server says about itself in `hello`.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    pub app_version: Option<String>,
}

/// Answers one request line. Never logs the line: an `add` carries a token.
pub async fn respond<B: HostBook, A: Agent>(
    line: &str,
    book: &B,
    agent: &A,
    info: &ServerInfo,
    adds: &tokio::sync::Mutex<()>,
) -> Response {
    // serde's error names a position, not the input — but none of it is the
    // operator's business, and the reply must never echo request bytes.
    let Ok(envelope) = serde_json::from_str::<Envelope>(line) else {
        return Response::refused("Malformed request.");
    };
    if envelope.request == Request::Hello {
        return Response::Hello {
            protocol: PROTOCOL,
            app_version: info.app_version.clone(),
        };
    }
    if envelope.v != PROTOCOL {
        return Response::refused(version_skew(envelope.v));
    }
    match envelope.request {
        Request::Hello => unreachable!("answered above"),
        Request::List => Response::Hosts {
            hosts: book.hosts().iter().map(HostSummary::from).collect(),
        },
        Request::Probe { address, port } => Response::Probe {
            probe: agent.probe(address.trim(), port).await,
        },
        Request::Add(add) => {
            // One at a time: two adds racing would both decide against the
            // same snapshot of the host list.
            let _turn = adds.lock().await;
            // Named before `pair` takes the request; never the token, never
            // the line.
            let target = format!("{} at {}:{}", add.name.trim(), add.address.trim(), add.port);
            match pair(add, book, agent).await {
                Ok(paired) => {
                    eprintln!("hand-off: {target}: {:?}", paired.outcome);
                    Response::Paired(paired)
                }
                Err(message) => {
                    eprintln!("hand-off: {target}: refused: {message}");
                    Response::refused(message)
                }
            }
        }
    }
}

/// The sentence for a request in a protocol this server does not speak.
#[must_use]
pub fn version_skew(theirs: u32) -> String {
    format!(
        "The running Solador speaks hand-off protocol v{PROTOCOL}, this command v{theirs}. \
         Quit and reopen Solador so both are the same version."
    )
}

async fn pair<B: HostBook, A: Agent>(
    add: AddRequest,
    book: &B,
    agent: &A,
) -> Result<Paired, String> {
    let name = add.name.trim();
    let address = add.address.trim();
    if name.is_empty() || address.is_empty() {
        return Err("A name and an address are required.".into());
    }
    if add.port == 0 {
        return Err("Port 0 is not a port.".into());
    }
    if add.token.is_empty() {
        return Err("A token is required.".into());
    }
    let Some(expected) = certpin::normalise(&add.expected_fingerprint) else {
        return Err("The expected fingerprint is not a certificate fingerprint.".into());
    };

    // The comparison that replaces the operator's eyes on the Settings form:
    // what the app itself finds at the address, against what the host printed.
    let found = match agent.probe(address, add.port).await {
        ProbeSummary::Tls { fingerprint } => certpin::normalise(&fingerprint),
        ProbeSummary::NoTls => {
            return Err(format!(
                "{address}:{} answers plain HTTP, so there is no certificate to pair. \
                 Turn TLS on for that agent (SOLADOR_AGENT_TLS=1) first. Nothing was saved.",
                add.port
            ));
        }
        ProbeSummary::Failed { message } => return Err(format!("{message}. Nothing was saved.")),
    };
    let Some(found) = found else {
        return Err(format!(
            "{address}:{} presented a certificate whose fingerprint could not be read. \
             Nothing was saved.",
            add.port
        ));
    };
    if found != expected {
        return Err(format!(
            "Fingerprint mismatch at {address}:{}: expected {expected} (--fingerprint), but \
             what answers there presented {found}. Either something else is answering at \
             that address or the fingerprint is another host's. Nothing was saved.",
            add.port
        ));
    }

    let pairing = Pairing {
        name,
        address,
        port: add.port,
        pin: &found,
        token: &add.token,
        replace: add.replace,
    };
    let decision = pairing.decide(&book.hosts());
    if let Decision::Refused(message) = decision {
        return Err(format!("{message} Nothing was saved."));
    }

    // The token is proven before it is stored: an agent that refuses it would
    // leave a card that can only ever say "auth failed".
    let health = agent
        .health(address, add.port, &found, &add.token)
        .await
        .map_err(|line| format!("{line}. Nothing was saved."))?;

    let (outcome, host) = match decision {
        Decision::SamePairing { id } if book.token_matches(id, &add.token) => {
            let host = book
                .hosts()
                .into_iter()
                .find(|host| host.id == id)
                .ok_or("That host was removed while this request ran. Nothing was saved.")?;
            (Outcome::Unchanged, host)
        }
        Decision::Add => (Outcome::Added, book.apply(&pairing, &decision)?),
        Decision::SamePairing { .. } => (Outcome::TokenReplaced, book.apply(&pairing, &decision)?),
        Decision::Replace { .. } => (Outcome::Updated, book.apply(&pairing, &decision)?),
        Decision::Refused(_) => unreachable!("returned above"),
    };
    Ok(Paired {
        outcome,
        host: HostSummary::from(&host),
        health,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const PIN: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";
    const OTHER: &str = "11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11:11";

    #[derive(Default)]
    struct Book {
        hosts: Mutex<Vec<Host>>,
        tokens: Mutex<Vec<(Uuid, String)>>,
        writes: Mutex<u32>,
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
            *self.writes.lock().unwrap() += 1;
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

    struct FakeAgent {
        probe: ProbeSummary,
        accepts: &'static str,
        health_calls: Mutex<u32>,
    }

    impl FakeAgent {
        fn presenting(fingerprint: &str) -> Self {
            FakeAgent {
                probe: ProbeSummary::Tls {
                    fingerprint: fingerprint.into(),
                },
                accepts: "good-token",
                health_calls: Mutex::new(0),
            }
        }
    }

    impl Agent for FakeAgent {
        async fn probe(&self, _address: &str, _port: u16) -> ProbeSummary {
            self.probe.clone()
        }
        async fn health(
            &self,
            _address: &str,
            _port: u16,
            _pin: &str,
            token: &Secret,
        ) -> Result<String, String> {
            *self.health_calls.lock().unwrap() += 1;
            if token.expose() == self.accepts {
                Ok("✓ agent-1 · agent v2026.10.18".into())
            } else {
                Err("✗ auth failed (401) — check token".into())
            }
        }
    }

    fn add_line(address: &str, token: &str, expected: &str, replace: bool) -> String {
        serde_json::to_string(&Envelope::new(Request::Add(AddRequest {
            name: "agent-1".into(),
            address: address.into(),
            port: 7878,
            token: Secret::new(token),
            expected_fingerprint: expected.into(),
            replace,
        })))
        .unwrap()
    }

    async fn ask(line: &str, book: &Book, agent: &FakeAgent) -> Response {
        respond(
            line,
            book,
            agent,
            &ServerInfo::default(),
            &tokio::sync::Mutex::new(()),
        )
        .await
    }

    fn refusal(response: Response) -> String {
        match response {
            Response::Refused { message } => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn paired(response: Response) -> Paired {
        match response {
            Response::Paired(paired) => paired,
            other => panic!("expected a pairing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_matching_probe_and_an_accepted_token_add_a_pinned_host() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        // Any notation the host printed is the same fingerprint.
        let line = add_line("192.168.1.2", "good-token", &PIN.to_lowercase(), false);
        let result = paired(ask(&line, &book, &agent).await);
        assert_eq!(result.outcome, Outcome::Added);
        assert_eq!(result.host.fingerprint.as_deref(), Some(PIN));
        assert_eq!(result.health, "✓ agent-1 · agent v2026.10.18");
        assert_eq!(book.hosts().len(), 1);
    }

    /// The rule the whole command rests on. Break the equality check and this
    /// goes red: a certificate the host did not report is never pinned, and
    /// the token is never sent to it.
    #[tokio::test]
    async fn a_fingerprint_mismatch_saves_nothing_and_sends_no_token() {
        let book = Book::default();
        let agent = FakeAgent::presenting(OTHER);
        let message = refusal(
            ask(
                &add_line("192.168.1.2", "good-token", PIN, false),
                &book,
                &agent,
            )
            .await,
        );
        assert!(message.contains("Fingerprint mismatch"), "{message}");
        assert!(
            message.contains(PIN) && message.contains(OTHER),
            "{message}"
        );
        assert!(book.hosts().is_empty());
        assert_eq!(*agent.health_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn plain_http_and_unreachable_are_their_own_refusals() {
        let book = Book::default();
        let mut agent = FakeAgent::presenting(PIN);
        agent.probe = ProbeSummary::NoTls;
        let message = refusal(
            ask(
                &add_line("192.168.1.2", "good-token", PIN, false),
                &book,
                &agent,
            )
            .await,
        );
        assert!(message.contains("plain HTTP"), "{message}");

        agent.probe = ProbeSummary::Failed {
            message: "that host isn't answering".into(),
        };
        let message = refusal(
            ask(
                &add_line("192.168.1.2", "good-token", PIN, false),
                &book,
                &agent,
            )
            .await,
        );
        assert!(
            message.starts_with("that host isn't answering"),
            "{message}"
        );
        assert!(book.hosts().is_empty());
    }

    #[tokio::test]
    async fn a_token_the_agent_refuses_is_never_stored() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        let message =
            refusal(ask(&add_line("192.168.1.2", "wrong", PIN, false), &book, &agent).await);
        assert!(message.contains("auth failed"), "{message}");
        assert!(book.hosts().is_empty());
        assert_eq!(*book.writes.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn the_same_pairing_again_writes_nothing_and_a_new_token_replaces_the_old() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        let line = add_line("192.168.1.2", "good-token", PIN, false);
        paired(ask(&line, &book, &agent).await);
        let again = paired(ask(&line, &book, &agent).await);
        assert_eq!(again.outcome, Outcome::Unchanged);
        assert_eq!(*book.writes.lock().unwrap(), 1);

        let id = book.hosts()[0].id;
        book.tokens.lock().unwrap()[0].1 = "stale".into();
        let replaced = paired(ask(&line, &book, &agent).await);
        assert_eq!(replaced.outcome, Outcome::TokenReplaced);
        assert!(book.token_matches(id, &Secret::new("good-token")));
        assert_eq!(book.hosts().len(), 1);
    }

    #[tokio::test]
    async fn moving_a_paired_host_needs_replace_and_keeps_one_row() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        paired(
            ask(
                &add_line("100.64.0.9", "good-token", PIN, false),
                &book,
                &agent,
            )
            .await,
        );
        let message = refusal(
            ask(
                &add_line("192.168.1.2", "good-token", PIN, false),
                &book,
                &agent,
            )
            .await,
        );
        assert!(message.contains("--replace"), "{message}");
        // `decide` says what is there; this layer says nothing was saved — once.
        assert_eq!(message.matches("Nothing was saved").count(), 1, "{message}");
        assert_eq!(book.hosts()[0].address, "100.64.0.9");

        let moved = paired(
            ask(
                &add_line("192.168.1.2", "good-token", PIN, true),
                &book,
                &agent,
            )
            .await,
        );
        assert_eq!(moved.outcome, Outcome::Updated);
        assert_eq!(book.hosts().len(), 1);
        assert_eq!(book.hosts()[0].address, "192.168.1.2");
    }

    #[tokio::test]
    async fn hello_is_answered_in_any_version_and_everything_else_is_not() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        let hello = ask(r#"{"v":99,"op":"hello"}"#, &book, &agent).await;
        assert_eq!(
            hello,
            Response::Hello {
                protocol: PROTOCOL,
                app_version: None
            }
        );
        let message = refusal(ask(r#"{"v":99,"op":"list"}"#, &book, &agent).await);
        assert!(message.contains("Quit and reopen Solador"), "{message}");
    }

    #[tokio::test]
    async fn a_malformed_line_is_refused_without_echoing_it() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        let message = refusal(ask(r#"{"v":1,"op":"add","token":"leak-me""#, &book, &agent).await);
        assert_eq!(message, "Malformed request.");
    }

    #[tokio::test]
    async fn list_carries_no_token() {
        let book = Book::default();
        let agent = FakeAgent::presenting(PIN);
        paired(
            ask(
                &add_line("192.168.1.2", "good-token", PIN, false),
                &book,
                &agent,
            )
            .await,
        );
        let list = ask(r#"{"v":1,"op":"list"}"#, &book, &agent).await;
        let line = serde_json::to_string(&list).unwrap();
        assert!(line.contains("192.168.1.2"), "{line}");
        assert!(!line.contains("good-token"), "{line}");
    }
}
