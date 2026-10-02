//! `fetch_latest_with` against an in-memory transport: the classification half
//! (`src/lib.rs` tests the verify half against the same signed fixtures). Never
//! the network and never a socket: the transport seam carries no URL scheme, so
//! the production client has no plain-HTTP allowance to test around.

use agentrelease::{fetch_latest_with, Error, Reply, Transport, Trust};
use std::collections::HashMap;
use std::sync::Mutex;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/agent-v");
const FEED_SUFFIX: &str = "/releases/download/agent-latest/agent-latest.json";
const SIG_SUFFIX: &str = "/releases/download/agent-latest/agent-latest.json.minisig";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).expect("fixture")
}

fn trust() -> Trust {
    let key = String::from_utf8(fixture("test-agent-key.pub")).unwrap();
    Trust::from_texts(&[&key]).unwrap()
}

/// Answers by URL suffix; an unknown URL is a 404; `down` makes every request
/// fail to complete. Records every URL asked for.
struct Fake {
    routes: HashMap<&'static str, Reply>,
    down: bool,
    /// Fails only the signature request, with this error.
    sig_fails: Option<Error>,
    asked: Mutex<Vec<String>>,
}

impl Fake {
    fn new(routes: Vec<(&'static str, u16, Vec<u8>)>) -> Self {
        Fake {
            routes: routes
                .into_iter()
                .map(|(suffix, status, body)| (suffix, Reply { status, body }))
                .collect(),
            down: false,
            sig_fails: None,
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl Transport for Fake {
    async fn get(&self, url: &str) -> Result<Reply, Error> {
        self.asked.lock().unwrap().push(url.to_owned());
        if let (true, Some(err)) = (url.ends_with(SIG_SUFFIX), &self.sig_fails) {
            return Err(err.clone());
        }
        if self.down {
            return Err(Error::Unreachable);
        }
        Ok(self
            .routes
            .iter()
            .find(|(suffix, _)| url.ends_with(**suffix))
            .map(|(_, reply)| reply.clone())
            .unwrap_or(Reply {
                status: 404,
                body: b"not found".to_vec(),
            }))
    }
}

fn signed() -> Fake {
    Fake::new(vec![
        (FEED_SUFFIX, 200, fixture("agent-latest.json")),
        (SIG_SUFFIX, 200, fixture("agent-latest.json.minisig")),
    ])
}

#[tokio::test]
async fn a_signed_feed_verifies_and_only_the_two_release_assets_are_asked_for() {
    let fake = signed();
    let latest = fetch_latest_with(&fake, &trust()).await.expect("verifies");
    assert_eq!(latest.version, "2026.9.9");
    let asked = fake.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2);
    for url in &asked {
        assert!(
            url.starts_with("https://github.com/Sassy-Dog/solador/releases/download/agent-latest/"),
            "{url}"
        );
    }
}

#[tokio::test]
async fn a_404_on_the_feed_is_not_published_yet() {
    assert_eq!(
        fetch_latest_with(&Fake::new(vec![]), &trust()).await,
        Err(Error::NotPublished)
    );
}

#[tokio::test]
async fn a_network_failure_is_unreachable_and_not_the_same_state() {
    let mut fake = signed();
    fake.down = true;
    let down = fetch_latest_with(&fake, &trust()).await;
    assert_eq!(down, Err(Error::Unreachable));
    assert_ne!(down, Err(Error::NotPublished));
}

#[tokio::test]
async fn a_server_failure_keeps_its_status() {
    let fake = Fake::new(vec![(FEED_SUFFIX, 503, b"down".to_vec())]);
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::Status(503))
    );
}

#[tokio::test]
async fn a_feed_without_its_signature_is_unsigned_not_unpublished() {
    let fake = Fake::new(vec![(FEED_SUFFIX, 200, fixture("agent-latest.json"))]);
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::Unsigned)
    );
}

#[tokio::test]
async fn an_oversize_feed_is_refused_as_malformed() {
    let fake = Fake::new(vec![(FEED_SUFFIX, 200, vec![b'x'; 256 * 1024 + 1])]);
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::Malformed)
    );
}

#[tokio::test]
async fn a_tampered_feed_is_refused() {
    let mut feed = fixture("agent-latest.json");
    let at = feed.windows(8).position(|w| w == b"2026.9.9").unwrap();
    feed[at + 7] = b'8';
    let fake = Fake::new(vec![
        (FEED_SUFFIX, 200, feed),
        (SIG_SUFFIX, 200, fixture("agent-latest.json.minisig")),
    ]);
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::SignatureRejected)
    );
}

#[tokio::test]
async fn a_foreign_key_is_refused() {
    // The compiled-in set is the real agent key; the fixture is a throwaway's.
    let foreign = Trust::compiled_in().unwrap();
    assert_eq!(
        fetch_latest_with(&signed(), &foreign).await,
        Err(Error::SignatureRejected)
    );
}

#[tokio::test]
async fn a_failure_fetching_the_signature_is_never_reported_as_unsigned() {
    // 503 on the signature keeps its status.
    let mut fake = signed();
    fake.routes.insert(
        SIG_SUFFIX,
        Reply {
            status: 503,
            body: vec![],
        },
    );
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::Status(503))
    );

    // A signature request that cannot complete is unreachable.
    let mut fake = signed();
    fake.sig_fails = Some(Error::Unreachable);
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::Unreachable)
    );

    // An oversize signature is not one.
    let mut fake = signed();
    fake.routes.insert(
        SIG_SUFFIX,
        Reply {
            status: 200,
            body: vec![b'x'; 256 * 1024 + 1],
        },
    );
    assert_eq!(
        fetch_latest_with(&fake, &trust()).await,
        Err(Error::Malformed)
    );
}
