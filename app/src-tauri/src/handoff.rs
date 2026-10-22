//! The running app's side of `solador-app host …` (`crates/hostsetup`).
//!
//! The crate decides; this module writes, through the same functions the
//! Settings form uses ([`add_host_connection`], [`save_host_connection`]), and
//! makes the change live the same way ([`reload_hosts`]). So a host handed over
//! from the command line is indistinguishable from one added by hand, and this
//! process stays the only writer of `store.json` and the keychain.

use std::sync::{Arc, MutexGuard};

use agentclient::{AgentClient, CertProbe};
use hostsetup::pairing::{Decision, Pairing, MOVED};
use hostsetup::protocol::ProbeSummary;
use hostsetup::server::{Agent, HostBook, ServerInfo};
use hostsetup::Secret;
use store::{CredentialStore, Host, SecretKey, Store};
use uuid::Uuid;

use crate::{
    add_host_connection, reload_hosts, restart_host_task, save_host_connection, settings, App,
};

/// Binds the hand-off channel (a socket on Unix, a named pipe on Windows) and
/// serves it for the life of the process.
///
/// Failing to bind costs the command line, never the cockpit: one line in the
/// log, and the window carries on. A second instance finds the first one's
/// channel alive and leaves it alone.
pub async fn serve(app: Arc<App>) {
    let dir = match hostsetup::channel::store_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("hand-off: {error}; `solador-app host` cannot reach this instance");
            return;
        }
    };
    let listener = match hostsetup::channel::bind(&dir) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("hand-off: {error}; `solador-app host` cannot reach this instance");
            return;
        }
    };
    hostsetup::channel::serve(
        listener,
        Arc::new(AppBook(app)),
        Arc::new(AppAgent),
        ServerInfo {
            app_version: settings::VERSION.map(str::to_owned),
        },
    )
    .await;
}

/// What the hand-off needs from the running app: the two stores it writes
/// through, and the two ways a change is made live. [`App`] in the product; a
/// scratch store and a recorder in the tests below, so the writes are tested
/// without a window.
trait Live: Send + Sync {
    fn store(&self) -> MutexGuard<'_, Store>;
    fn credentials(&self) -> &dyn CredentialStore;
    /// Drops host `id`'s poll task: its client holds the old token.
    fn restart(&self, id: Uuid);
    /// Makes the store's host list the poll set.
    fn reload(&self);
}

impl Live for App {
    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().expect("store poisoned")
    }

    fn credentials(&self) -> &dyn CredentialStore {
        self.credentials.as_ref()
    }

    fn restart(&self, id: Uuid) {
        restart_host_task(self, id);
    }

    fn reload(&self) {
        reload_hosts(self);
    }
}

struct AppBook<L>(Arc<L>);

impl<L: Live> HostBook for AppBook<L> {
    fn hosts(&self) -> Vec<Host> {
        self.0.store().hosts().to_vec()
    }

    fn token_matches(&self, id: Uuid, token: &Secret) -> bool {
        // An unreadable keychain is not a match: the caller then writes the
        // token, which is what the operator asked for anyway.
        matches!(
            self.0.credentials().secret(SecretKey::HostToken(id)),
            Ok(Some(stored)) if stored == token.expose()
        )
    }

    fn apply(&self, pairing: &Pairing<'_>, decided: &Decision) -> Result<Host, String> {
        let live = &*self.0;
        let (result, repointed) = {
            let mut store = live.store();
            pairing.confirm(store.hosts(), decided)?;
            match *decided {
                Decision::Add => (
                    add_host_connection(
                        &mut store,
                        live.credentials(),
                        (pairing.name, pairing.address, pairing.port),
                        pairing.token.expose(),
                        Some(pairing.pin.to_owned()),
                    ),
                    None,
                ),
                Decision::SamePairing { id } | Decision::Replace { id } => (
                    repoint(&mut store, live.credentials(), id, pairing),
                    Some(id),
                ),
                Decision::Refused(_) => return Err("A refused pairing is never applied.".into()),
            }
        };
        // A replaced token needs a new client, and only a restart gives the
        // host one (an unchanged address and pin keep the old task). Whatever
        // else was written goes live either way: a row saved before the
        // credential store refused its token is still a saved row.
        if let (Ok(_), Some(id)) = (&result, repointed) {
            live.restart(id);
        }
        live.reload();
        result
    }
}

/// Points row `id` at the pairing's address and pin and replaces its token,
/// through Settings' own save. Its name is kept.
fn repoint(
    store: &mut Store,
    credentials: &dyn CredentialStore,
    id: Uuid,
    pairing: &Pairing<'_>,
) -> Result<Host, String> {
    let name = store.host(id).map(|host| host.name.clone()).ok_or(MOVED)?;
    save_host_connection(
        store,
        credentials,
        id,
        (&name, pairing.address, &pairing.port.to_string()),
        pairing.token.expose(),
        Some(pairing.pin),
    )?;
    store.host(id).cloned().ok_or_else(|| MOVED.to_owned())
}

struct AppAgent;

impl Agent for AppAgent {
    async fn probe(&self, address: &str, port: u16) -> ProbeSummary {
        match agentclient::probe_certificate(address, port).await {
            Ok(CertProbe::Tls { fingerprint }) => ProbeSummary::Tls { fingerprint },
            Ok(CertProbe::NoTls) => ProbeSummary::NoTls,
            Err(error) => {
                // As in Settings: one sentence back, the transport's detail
                // to the log only.
                eprintln!("hand-off: probe of {address}:{port} found nothing: {error}");
                ProbeSummary::Failed {
                    message: error.user_message(),
                }
            }
        }
    }

    async fn health(
        &self,
        address: &str,
        port: u16,
        pin: &str,
        token: &Secret,
    ) -> Result<String, String> {
        let mut host = Host::new("", address);
        host.port = port;
        host.tls_fingerprint = Some(pin.to_owned());
        let result = AgentClient::pinned(host.base_url(), token.expose(), pin)
            .health()
            .await;
        let line = settings::health_result(&result);
        result.map(|_| line.clone()).map_err(|_| line)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use store::{MemoryCredentialStore, SecretError};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::TlsAcceptor;

    use super::*;

    /// The app's side of the hand-off over a scratch store, recording what it
    /// asked the poll set to do.
    struct Recorder {
        store: Mutex<Store>,
        credentials: Box<dyn CredentialStore + Send + Sync>,
        restarts: Mutex<Vec<Uuid>>,
        reloads: Mutex<u32>,
        _dir: tempfile::TempDir,
    }

    impl Live for Recorder {
        fn store(&self) -> MutexGuard<'_, Store> {
            self.store.lock().unwrap()
        }
        fn credentials(&self) -> &dyn CredentialStore {
            self.credentials.as_ref()
        }
        fn restart(&self, id: Uuid) {
            self.restarts.lock().unwrap().push(id);
        }
        fn reload(&self) {
            *self.reloads.lock().unwrap() += 1;
        }
    }

    fn book(credentials: impl CredentialStore + Send + Sync + 'static) -> AppBook<Recorder> {
        let dir = tempfile::TempDir::new().unwrap();
        AppBook(Arc::new(Recorder {
            store: Mutex::new(Store::open_in(dir.path(), false).unwrap()),
            credentials: Box::new(credentials),
            restarts: Mutex::new(Vec::new()),
            reloads: Mutex::new(0),
            _dir: dir,
        }))
    }

    /// A keychain that will not take a write (locked, say).
    struct Refusing;
    impl CredentialStore for Refusing {
        fn secret(&self, _: SecretKey) -> Result<Option<String>, SecretError> {
            Ok(None)
        }
        fn set_secret(&self, key: SecretKey, _: &str) -> Result<(), SecretError> {
            Err(SecretError::CorruptBlob {
                account: key.account(),
            })
        }
        fn delete_secret(&self, _: SecretKey) -> Result<(), SecretError> {
            Ok(())
        }
        fn secret_bytes(&self, _: SecretKey) -> Result<Option<Vec<u8>>, SecretError> {
            Ok(None)
        }
        fn set_secret_bytes(&self, key: SecretKey, _: &[u8]) -> Result<(), SecretError> {
            Err(SecretError::CorruptBlob {
                account: key.account(),
            })
        }
    }

    fn pin(byte: u8) -> String {
        certpin::format(&[byte; certpin::DIGEST_LEN])
    }

    fn pairing<'a>(address: &'a str, pin: &'a str, token: &'a Secret) -> Pairing<'a> {
        Pairing {
            name: "agent",
            address,
            port: 7878,
            pin,
            token,
            replace: true,
        }
    }

    fn token_of(book: &AppBook<Recorder>, id: Uuid) -> Option<String> {
        book.0.credentials.secret(SecretKey::HostToken(id)).unwrap()
    }

    #[test]
    fn an_added_host_is_saved_pinned_with_its_token_and_made_live() {
        let book = book(MemoryCredentialStore::new());
        let (pin, token) = (pin(0x33), Secret::new("new-token"));
        let host = book
            .apply(&pairing("192.168.1.2", &pin, &token), &Decision::Add)
            .unwrap();
        assert_eq!(book.hosts(), vec![host.clone()]);
        assert_eq!(host.tls_fingerprint.as_deref(), Some(pin.as_str()));
        assert_eq!(token_of(&book, host.id).as_deref(), Some("new-token"));
        assert_eq!(*book.0.reloads.lock().unwrap(), 1);
        assert!(book.0.restarts.lock().unwrap().is_empty());
    }

    /// The agent moved (a tailnet address saved earlier, the LAN one now): the
    /// row keeps its name and its place, takes the pairing's address and
    /// token — and its poll task is dropped, or the card would go on polling
    /// with the client that holds the old token.
    #[test]
    fn a_moved_host_keeps_its_name_takes_the_new_token_and_restarts() {
        let book = book(MemoryCredentialStore::new());
        let held = pin(0x22);
        let id = {
            let live = &book.0;
            let mut store = live.store();
            add_host_connection(
                &mut store,
                live.credentials(),
                ("by hand", "100.64.0.9", 7878),
                "old-token",
                Some(held.clone()),
            )
            .unwrap()
            .id
        };
        let token = Secret::new("new-token");
        let host = book
            .apply(
                &pairing("192.168.1.2", &held, &token),
                &Decision::Replace { id },
            )
            .unwrap();
        assert_eq!(host.id, id);
        assert_eq!(host.name, "by hand");
        assert_eq!(host.address, "192.168.1.2");
        assert_eq!(host.tls_fingerprint.as_deref(), Some(held.as_str()));
        assert_eq!(token_of(&book, id).as_deref(), Some("new-token"));
        assert_eq!(*book.0.restarts.lock().unwrap(), vec![id]);
        assert_eq!(*book.0.reloads.lock().unwrap(), 1);

        // Same address and pin, new token: nothing in the row moved, so only
        // the restart rebuilds the client.
        let newer = Secret::new("newer-token");
        book.apply(
            &pairing("192.168.1.2", &held, &newer),
            &Decision::SamePairing { id },
        )
        .unwrap();
        assert_eq!(token_of(&book, id).as_deref(), Some("newer-token"));
        assert_eq!(*book.0.restarts.lock().unwrap(), vec![id, id]);
    }

    #[test]
    fn token_matches_compares_the_stored_token_and_nothing_else() {
        let book = book(MemoryCredentialStore::new());
        let (pin, token) = (pin(0x33), Secret::new("right"));
        let id = book
            .apply(&pairing("192.168.1.2", &pin, &token), &Decision::Add)
            .unwrap()
            .id;
        assert!(book.token_matches(id, &Secret::new("right")));
        assert!(!book.token_matches(id, &Secret::new("wrong")));
        assert!(!book.token_matches(Uuid::new_v4(), &Secret::new("right")));
    }

    /// The keychain refused the token after the row was saved: the row is
    /// real, so it is made live, and the sentence says it was added.
    #[test]
    fn a_token_the_keychain_refuses_still_makes_the_saved_row_live() {
        let book = book(Refusing);
        let (pin, token) = (pin(0x33), Secret::new("new-token"));
        let refused = book
            .apply(&pairing("192.168.1.2", &pin, &token), &Decision::Add)
            .unwrap_err();
        assert!(refused.contains("token could not be stored"), "{refused}");
        assert_eq!(book.hosts().len(), 1);
        assert_eq!(*book.0.reloads.lock().unwrap(), 1);
    }

    /// Settings added a host at that address while the probe and the health
    /// check ran: the hand-off writes nothing rather than a second card.
    #[test]
    fn a_decision_the_store_moved_under_writes_nothing() {
        let book = book(MemoryCredentialStore::new());
        book.0
            .store()
            .upsert_host(Host::new("by hand", "192.168.1.2"));
        let (pin, token) = (pin(0x33), Secret::new("new-token"));
        let refused = book
            .apply(&pairing("192.168.1.2", &pin, &token), &Decision::Add)
            .unwrap_err();
        assert_eq!(refused, MOVED);
        assert_eq!(book.hosts().len(), 1);
        assert!(book.0.restarts.lock().unwrap().is_empty());
    }

    // MARK: AppAgent, against a real TLS agent on a loopback port

    const TOKEN: &str = "s3cret-token";
    const HEALTH: &str = include_str!("../../../crates/wire/tests/fixtures/health.json");

    /// An agent serving `/v1/health` behind a throwaway certificate: 200 for
    /// [`TOKEN`], 401 otherwise. Returns its port, its certificate's
    /// fingerprint, and the head of every request it read.
    async fn tls_agent() -> (u16, String, Arc<Mutex<Vec<String>>>) {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert = generated.cert.der().to_vec();
        let fingerprint = agentclient::fingerprint(&cert);
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                generated.signing_key.serialize_der(),
            )),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&heads);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, seen) = (acceptor.clone(), Arc::clone(&seen));
                tokio::spawn(async move {
                    // A refused handshake ends here, with nothing read.
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => head.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).to_lowercase();
                    let reply = if head.contains(&format!("authorization: bearer {TOKEN}")) {
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n{HEALTH}",
                            HEALTH.len()
                        )
                    } else {
                        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\
                         connection: close\r\n\r\n"
                            .to_owned()
                    };
                    seen.lock().unwrap().push(head);
                    let _ = tls.write_all(reply.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        (port, fingerprint, heads)
    }

    #[tokio::test]
    async fn the_probe_reports_the_certificate_that_answers_there() {
        let (port, fingerprint, _) = tls_agent().await;
        assert_eq!(
            AppAgent.probe("127.0.0.1", port).await,
            ProbeSummary::Tls { fingerprint }
        );
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(matches!(
            AppAgent.probe("127.0.0.1", closed).await,
            ProbeSummary::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn health_is_asked_over_a_connection_pinned_to_the_given_certificate() {
        let (port, fingerprint, heads) = tls_agent().await;
        let line = AppAgent
            .health("127.0.0.1", port, &fingerprint, &Secret::new(TOKEN))
            .await
            .unwrap();
        assert!(line.starts_with('✓'), "{line}");

        // Pinned to another certificate, the handshake fails before the token
        // could be sent: the agent reads no second request.
        let before = heads.lock().unwrap().len();
        let refused = AppAgent
            .health("127.0.0.1", port, &pin(0x44), &Secret::new(TOKEN))
            .await;
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(heads.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn a_token_the_agent_refuses_is_an_err_carrying_the_settings_line() {
        let (port, fingerprint, _) = tls_agent().await;
        let refused = AppAgent
            .health("127.0.0.1", port, &fingerprint, &Secret::new("wrong-token"))
            .await
            .unwrap_err();
        assert!(refused.contains("auth failed (401)"), "{refused}");
    }
}
