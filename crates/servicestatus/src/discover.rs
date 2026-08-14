//! Turn a hostname into a component choice.
//!
//! [`statuspage`](crate::statuspage) reads a vendor the app already knows: a
//! base URL plus **the id of the one component worth watching**. Those ids are
//! opaque — `k8w3r06qmzrp` — and a Statuspage publishes them nowhere a person
//! would look. Asking an operator to paste one is asking for a list nobody adds
//! to, so discovery is what makes a user-supplied vendor reachable at all.
//!
//! One probe of `{base_url}/api/v2/summary.json` does four jobs: it confirms
//! the host really is an Atlassian Statuspage, it names the ids, it gives an
//! immediate success signal so a vendor is never configured-but-blank, and it
//! makes the status.io trap self-correcting — [`statusio`](crate::statusio)
//! documents that `neonstatus.com` *looks* like a Statuspage and is not, which
//! among the five vendors shipped today is a base rate of surprises of 1 in 5.
//!
//! ## The reasons are the product
//!
//! A failed probe reports what it found and never guesses, so each way of
//! failing is its own [`ProbeError`]. In particular **"not a Statuspage"
//! ([`ProbeError::NoComponents`]) may be concluded only from a well-formed
//! response that lacked the shape**; a body that did not parse is
//! [`ProbeError::NotJson`], a different fact. Collapsing the two is what would
//! send someone hunting for the wrong problem — a captive-portal HTML page and
//! a status.io page are not the same mistake, and only one of them is fixed by
//! looking for a different vendor page.
//!
//! ## This fetches a user-supplied URL
//!
//! Which is why it is the only client in this crate that does not use
//! [`crate::http_client`]:
//!
//! 1. **https only, refused before any request.** [`validate_base_url`] runs
//!    first and is a positive allowlist — the scheme must *be* `https`, rather
//!    than merely not being `http` — so anything unparseable is held too.
//! 2. **No cross-scheme redirect.** [`https_only_redirects`] refuses a hop that
//!    leaves https, and the client is additionally built `https_only(true)` so
//!    the transport refuses one even if this module's policy were ever loosened.
//! 3. **An explicit timeout**, the crate's [`crate::HTTP_TIMEOUT`], because a
//!    person is waiting on this one.
//!
//! What it does *not* do is resolve the host and judge the address: a probe of
//! `https://<something-internal>/` reaches it, exactly as the operator's own
//! browser would. The threat this guards is a URL that quietly downgrades or
//! walks off https, not an operator naming their own network.

/// One entry of a Statuspage's `components[]`, reduced to what a picker needs.
///
/// The `id` is the part that gets stored: display names are the vendor's to
/// re-word, and `components[]` carries rows that are not components at all
/// (GitHub's list includes *"Visit www.githubstatus.com for more
/// information"*). The name exists so a human can choose; the id is what
/// [`crate::StatusPageClient`] is then pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub id: String,
    pub name: String,
}

/// Why a probe did not end in a list of components.
///
/// Every variant is a distinct *finding*, and none of them is a guess. See the
/// module docs on why [`NotJson`](ProbeError::NotJson) and
/// [`NoComponents`](ProbeError::NoComponents) must never be merged.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Not a URL at all. Separate from [`Insecure`](ProbeError::Insecure)
    /// because "add `https://`" and "this cannot be parsed" send a person to
    /// two different corrections.
    #[error("that is not a URL")]
    Malformed,
    /// The URL was not https, or a redirect tried to leave https. Both are the
    /// same refusal — this probe will not read a status page over a scheme that
    /// anyone on the path can rewrite — and neither one reaches a request.
    #[error("refused: the probe only reads https")]
    Insecure,
    /// The host never answered. Carries the sanitized cause; `without_url` is
    /// applied first so the operator's URL never lands in a log line.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// The host answered with a non-2xx.
    #[error("the host returned HTTP {0}")]
    Http(u16),
    /// Something answered, but not with JSON. **Not** a verdict on whether the
    /// host is a Statuspage — nothing here has seen a payload to judge.
    #[error("the host answered with something that is not JSON")]
    NotJson,
    /// Well-formed JSON that carried no components to choose from: the one
    /// finding from which "this is not a Statuspage" may be concluded.
    #[error("the response listed no components")]
    NoComponents,
}

impl ProbeError {
    /// What the operator is shown next to the field they just typed in.
    ///
    /// Six findings, six sentences. The [`NotJson`](ProbeError::NotJson) line
    /// deliberately makes no claim about Statuspage-ness, which
    /// `the_two_shape_failures_do_not_share_a_verdict` holds it to.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            ProbeError::Malformed => {
                "that isn't a URL — try something like https://status.example.com".to_owned()
            }
            ProbeError::Insecure => "only https URLs can be probed".to_owned(),
            ProbeError::Unreachable(_) => "couldn't reach that host".to_owned(),
            ProbeError::Http(code) => format!("that host returned HTTP {code}"),
            ProbeError::NotJson => "that host answered, but not with JSON".to_owned(),
            ProbeError::NoComponents => {
                "that page is JSON but lists no components, so it isn't a Statuspage".to_owned()
            }
        }
    }
}

/// How far a redirect chain may run before the 3xx itself is reported.
///
/// Statuspage vendors do redirect — `status.anthropic.com` 302s to
/// `status.claude.com` — so refusing every hop would fail a probe of a URL that
/// works. Four is generous for a hostname change and short of a loop.
const MAX_REDIRECTS: usize = 4;

/// Follow a redirect only while it stays on https.
///
/// A hop off https is [`ProbeError::Insecure`], reported through
/// `reqwest`'s redirect-error kind. A chain past [`MAX_REDIRECTS`] *stops*
/// instead, so what surfaces is `Http(30x)` — the literal thing the host sent,
/// which also happens to be the hint that fixes it: point at the destination.
fn https_only_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            attempt.error(ProbeError::Insecure)
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

/// Everything the probe's client is, minus the one guard tests must drop.
fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(crate::HTTP_TIMEOUT)
        .user_agent(concat!("Solador/", env!("CARGO_PKG_VERSION")))
        .redirect(https_only_redirects())
}

/// The client [`probe`] uses: the builder above plus `https_only`.
fn probe_client() -> reqwest::Client {
    client_builder()
        .https_only(true)
        .build()
        .expect("reqwest client")
}

/// Refuse anything that is not an https URL, before a request exists.
///
/// Returns the base to build the summary URL from, with any trailing slash
/// removed so `{base}/api/v2/summary.json` never doubles one. The check is a
/// positive allowlist: a scheme that is not `https`, and a URL that does not
/// parse, are both held.
///
/// # Errors
/// [`ProbeError::Malformed`] if it is not a URL or names no host;
/// [`ProbeError::Insecure`] if its scheme is not https.
pub fn validate_base_url(input: &str) -> Result<String, ProbeError> {
    let url = reqwest::Url::parse(input.trim()).map_err(|_| ProbeError::Malformed)?;
    if url.scheme() != "https" {
        return Err(ProbeError::Insecure);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(ProbeError::Malformed);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Decode `summary.json` down to the components a person can choose between.
///
/// A free function over the body, like [`crate::statuspage::parse_summary`], so
/// the classification that matters most here is tested one `include_str!` away
/// from a fixture and with no mock server involved.
///
/// Entries missing a string `id` or `name` are skipped rather than filled in —
/// a component that cannot be identified cannot be chosen, and inventing either
/// field would store a handle that resolves to nothing.
///
/// # Errors
/// [`ProbeError::NotJson`] if the body did not parse;
/// [`ProbeError::NoComponents`] if it parsed and offered nothing to choose.
pub fn parse_components(body: &str) -> Result<Vec<Component>, ProbeError> {
    let root: serde_json::Value = serde_json::from_str(body).map_err(|_| ProbeError::NotJson)?;

    let components: Vec<Component> = root
        .get("components")
        .and_then(serde_json::Value::as_array)
        .ok_or(ProbeError::NoComponents)?
        .iter()
        .filter_map(|c| {
            Some(Component {
                id: c.get("id")?.as_str()?.to_owned(),
                name: c.get("name")?.as_str()?.to_owned(),
            })
        })
        .collect();

    if components.is_empty() {
        // Present-but-empty is the same finding as absent: JSON that named no
        // component this page could be watched by.
        return Err(ProbeError::NoComponents);
    }
    Ok(components)
}

/// One GET of `{base_url}/api/v2/summary.json`.
///
/// `base_url` must already have been through [`validate_base_url`]. Split out
/// from [`probe`] so the transport's own failures — a non-2xx, a dead host, a
/// redirect off https — are testable against a mock server, which necessarily
/// speaks http.
async fn fetch_summary(http: &reqwest::Client, base_url: &str) -> Result<String, ProbeError> {
    let url = format!("{base_url}/api/v2/summary.json");
    let response = http.get(&url).send().await.map_err(classify)?;
    if !response.status().is_success() {
        return Err(ProbeError::Http(response.status().as_u16()));
    }
    response.text().await.map_err(classify)
}

/// Sort a transport failure into the finding it actually is.
///
/// `reqwest` reports both scheme refusals this client can raise as its own
/// kinds: the redirect policy's error arrives as `is_redirect`, and
/// `https_only` rejecting a request URL arrives as `is_builder`. Everything
/// else is the host not answering.
fn classify(e: reqwest::Error) -> ProbeError {
    if e.is_redirect() || e.is_builder() {
        return ProbeError::Insecure;
    }
    // `without_url` so an operator's URL never reaches a string this crate
    // hands out — the same discipline `crate::get_text` follows.
    ProbeError::Unreachable(e.without_url().to_string())
}

/// Ask a host for the components it publishes.
///
/// Validate, GET `{base_url}/api/v2/summary.json`, decode. The validation is
/// first and is not optional: a non-https URL is refused here, not by the
/// transport, so no request is ever made for one.
///
/// # Errors
/// [`ProbeError`] — and the variant is the finding, so pass it to the operator
/// through [`ProbeError::user_message`] rather than flattening it to "failed".
pub async fn probe(base_url: &str) -> Result<Vec<Component>, ProbeError> {
    let base = validate_base_url(base_url)?;
    let body = fetch_summary(&probe_client(), &base).await?;
    parse_components(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Synthetic**, unlike this crate's other Statuspage fixtures, which were
    /// captured live from a named vendor. Discovery is for a host this build
    /// has never heard of, so naming a real one would be the shipped-example
    /// problem again; the shape is `statuspage_healthy.json`'s, down to the
    /// pseudo-component every page carries, and the ids are Statuspage's
    /// twelve-character form.
    const STATUSPAGE: &str = include_str!("../tests/fixtures/discover_statuspage.json");
    /// Also synthetic: a page that answers JSON and publishes no components.
    const NO_COMPONENTS: &str = include_str!("../tests/fixtures/discover_no_components.json");
    /// The trap this probe exists to make self-correcting, as captured live
    /// from `api.status.io` for [`crate::statusio`]'s own tests.
    const STATUS_IO: &str = include_str!("../tests/fixtures/statusio_healthy.json");

    /// The probe's client minus `https_only`, which no mock server can satisfy.
    ///
    /// It is the *same* [`client_builder`] otherwise — in particular the same
    /// [`https_only_redirects`] policy, so the redirect tests below exercise
    /// the code that ships. The one guard dropped here is the transport's
    /// second opinion on the request URL's scheme; the first, and the one that
    /// runs before any request exists, is covered by
    /// `a_non_https_url_is_refused_without_a_request`.
    fn client_without_transport_https_only() -> reqwest::Client {
        client_builder().build().expect("reqwest client")
    }

    #[test]
    fn a_statuspage_summary_yields_its_components() {
        let components = parse_components(STATUSPAGE).expect("a Statuspage summary");
        assert_eq!(
            components.first(),
            Some(&Component {
                id: "k8w3r06qmzrp".to_owned(),
                name: "API".to_owned(),
            })
        );
        assert_eq!(
            components
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            [
                "API",
                "Dashboard",
                "Webhooks",
                "Visit status.example.com for more information",
            ],
            "every row is offered in page order, including the ones that are \
             not components — filtering them needs a heuristic, and a heuristic \
             that is wrong hides the component someone came for"
        );
    }

    #[test]
    fn well_formed_json_without_components_is_not_a_statuspage() {
        assert!(matches!(
            parse_components(NO_COMPONENTS),
            Err(ProbeError::NoComponents)
        ));
    }

    #[test]
    fn html_is_reported_as_not_json_rather_than_as_not_a_statuspage() {
        assert!(matches!(
            parse_components("<!doctype html>"),
            Err(ProbeError::NotJson)
        ));
    }

    #[test]
    fn a_non_https_url_is_refused_without_a_request() {
        assert!(matches!(
            validate_base_url("http://x.example.com"),
            Err(ProbeError::Insecure)
        ));
        assert!(validate_base_url("https://x.example.com").is_ok());
    }

    /// `neonstatus.com` looks like a Statuspage and is not. A probe of it must
    /// say so, which is the whole reason the probe is worth making.
    #[test]
    fn a_status_io_payload_is_reported_as_not_a_statuspage() {
        assert!(matches!(
            parse_components(STATUS_IO),
            Err(ProbeError::NoComponents)
        ));
    }

    /// Present-but-empty is not a choice, and returning `Ok(vec![])` would end
    /// the probe in the configured-but-blank state it exists to prevent.
    #[test]
    fn a_components_key_with_nothing_in_it_is_not_a_choice() {
        assert!(matches!(
            parse_components(r#"{"page": {}, "components": []}"#),
            Err(ProbeError::NoComponents)
        ));
    }

    /// An entry with no usable id cannot be stored, and one with no name cannot
    /// be chosen. Skipping beats inventing either.
    #[test]
    fn an_entry_missing_an_id_or_a_name_is_skipped_not_invented() {
        let components = parse_components(
            r#"{"components": [
                 {"name": "No id here"},
                 {"id": "abc123def456"},
                 {"id": 7, "name": "Numeric id"},
                 {"id": "k8w3r06qmzrp", "name": "API"}
               ]}"#,
        )
        .expect("one usable entry");
        assert_eq!(
            components,
            vec![Component {
                id: "k8w3r06qmzrp".to_owned(),
                name: "API".to_owned(),
            }]
        );
    }

    #[test]
    fn a_url_that_does_not_parse_is_held_rather_than_called_insecure() {
        for input in ["", "status.example.com", "not a url", "https://"] {
            assert!(
                matches!(validate_base_url(input), Err(ProbeError::Malformed)),
                "{input:?}"
            );
        }
        assert!(
            matches!(
                validate_base_url("ftp://x.example.com"),
                Err(ProbeError::Insecure)
            ),
            "the allowlist is positive: a scheme that is not https is refused \
             whatever it is, not just http"
        );
    }

    #[test]
    fn a_validated_base_carries_no_trailing_slash_to_double_the_path() {
        assert_eq!(
            validate_base_url("  https://status.example.com/  ").expect("https"),
            "https://status.example.com"
        );
    }

    /// The distinction the panel is built on: a parse failure and a wrong shape
    /// are two findings, and only one of them is entitled to the verdict.
    #[test]
    fn the_two_shape_failures_do_not_share_a_verdict() {
        let not_json = ProbeError::NotJson.user_message();
        let no_components = ProbeError::NoComponents.user_message();
        assert_ne!(not_json, no_components);
        assert!(
            !not_json.contains("Statuspage"),
            "a body that did not parse says nothing about what the host is: {not_json}"
        );
        assert!(no_components.contains("Statuspage"), "{no_components}");
    }

    #[test]
    fn every_finding_has_its_own_sentence() {
        let messages = [
            ProbeError::Malformed,
            ProbeError::Insecure,
            ProbeError::Unreachable("dns".to_owned()),
            ProbeError::Http(503),
            ProbeError::NotJson,
            ProbeError::NoComponents,
        ]
        .map(|e| e.user_message());
        for (i, m) in messages.iter().enumerate() {
            assert!(!m.is_empty());
            assert!(
                !messages[..i].contains(m),
                "two findings would read identically: {m}"
            );
        }
    }

    #[tokio::test]
    async fn a_probe_of_a_non_https_url_never_reaches_the_host() {
        let server = wiremock::MockServer::start().await;
        // The mock is deliberately unmounted: any request at all is a failure,
        // and `received_requests` is what proves none was made.
        let err = probe(&server.uri()).await.expect_err("http:// is refused");
        assert!(matches!(err, ProbeError::Insecure));
        assert!(
            server
                .received_requests()
                .await
                .expect("recording is on")
                .is_empty(),
            "the refusal must happen before a request exists"
        );
    }

    #[tokio::test]
    async fn a_summary_read_lists_the_pages_components() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v2/summary.json"))
            .and(wiremock::matchers::header_exists("user-agent"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_raw(STATUSPAGE, "application/json"),
            )
            .mount(&server)
            .await;

        let body = fetch_summary(&client_without_transport_https_only(), &server.uri())
            .await
            .expect("summary");
        assert_eq!(parse_components(&body).expect("components").len(), 4);
    }

    #[tokio::test]
    async fn a_non_2xx_is_reported_with_its_code() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let err = fetch_summary(&client_without_transport_https_only(), &server.uri())
            .await
            .expect_err("404");
        assert!(matches!(err, ProbeError::Http(404)));
    }

    #[tokio::test]
    async fn an_unroutable_host_is_unreachable_without_leaking_the_url() {
        let err = fetch_summary(&client_without_transport_https_only(), "http://127.0.0.1:1")
            .await
            .expect_err("connection refused");
        assert!(matches!(err, ProbeError::Unreachable(_)));
        assert!(!format!("{err}").contains("127.0.0.1:1"), "{err}");
    }

    #[tokio::test]
    async fn a_redirect_that_leaves_https_is_refused() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("location", "http://127.0.0.1:1/api/v2/summary.json"),
            )
            .mount(&server)
            .await;
        let err = fetch_summary(&client_without_transport_https_only(), &server.uri())
            .await
            .expect_err("a hop off https");
        assert!(matches!(err, ProbeError::Insecure), "{err}");
    }

    /// The control for the test above: the policy refuses the *scheme*, not
    /// redirection itself. This one is allowed through and then fails to
    /// connect, which is only reachable by having followed it.
    #[tokio::test]
    async fn a_redirect_that_stays_on_https_is_followed() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("location", "https://127.0.0.1:1/api/v2/summary.json"),
            )
            .mount(&server)
            .await;
        let err = fetch_summary(&client_without_transport_https_only(), &server.uri())
            .await
            .expect_err("the destination is dead");
        assert!(matches!(err, ProbeError::Unreachable(_)), "{err}");
    }
}
