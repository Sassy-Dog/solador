//! The two Blob REST operations a cost read needs, and the SAS-URL transport
//! that performs them. Port of `BlobFetching` / `URLSessionBlobFetcher` /
//! `parseBlobListXML` in
//! `AzureCostService`.
//!
//! There is no Azure SDK and no OAuth here. The container-scoped, read+list
//! user-delegation SAS *is* the credential: the URL splits into a container
//! base and a query string, and the query is appended to plain GETs.
//!
//! # The SAS is a credential in a URL
//!
//! Everything else in this workspace keeps credentials in headers, where they
//! cannot leak through `reqwest`'s error strings (see the invariant on
//! `GitHubClient::with_base_url` in `crates/github/src/client.rs`). This one
//! cannot: the signature travels in the query string. The rules here are
//! therefore stricter, not looser —
//! [`SasBlobFetcher`] has a hand-written [`Debug`] that redacts the query, and
//! every transport error is stripped of its URL with
//! [`reqwest::Error::without_url`] before it becomes a string. Nothing in this
//! crate logs.

use std::future::Future;
use std::time::Duration;

use crate::error::AzureCostError;

/// Matches the original `URLRequest.timeoutInterval`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The blob surface a cost read depends on, abstracted so the
/// list → pick-newest-run → aggregate logic in [`crate::reader`] is testable
/// against an in-memory map with no network at all.
///
/// `prefix` and `path` are container-relative: the SAS already pins the storage
/// account and the container.
pub trait BlobFetcher {
    /// Every blob name under `prefix`, following `<NextMarker>` pagination to
    /// the end. A prefix that matches nothing yields an empty list, not an
    /// error — deciding what "empty" means belongs to the caller.
    fn list_blobs(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<String>, AzureCostError>> + Send;

    /// The UTF-8 text body of a single blob.
    fn get_blob_text(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<String, AzureCostError>> + Send;
}

/// One page of a Blob `comp=list` response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobListPage {
    pub names: Vec<String>,
    /// `None` both when the element is absent and when it is present but empty
    /// — Azure sends `<NextMarker />` on the last page, and reading that as a
    /// marker would loop forever.
    pub next_marker: Option<String>,
}

/// Extract `<Name>` values and the optional `<NextMarker>` from a Blob list
/// response.
///
/// A string scan rather than a full XML parse, and dependency-free for it: the
/// values are blob paths under a prefix this crate chose, so there are no
/// entity-escaped characters to decode.
#[must_use]
pub fn parse_blob_list_xml(xml: &str) -> BlobListPage {
    let names = extract_tag_values(xml, "Name");
    let next_marker = extract_tag_values(xml, "NextMarker")
        .into_iter()
        .next()
        .filter(|marker| !marker.is_empty());
    BlobListPage { names, next_marker }
}

fn extract_tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        values.push(after_open[..end].to_owned());
        rest = &after_open[end + close.len()..];
    }
    values
}

/// [`BlobFetcher`] over a container-scoped SAS URL.
pub struct SasBlobFetcher {
    /// e.g. `https://<account>.blob.core.windows.net/cost-exports`, with no
    /// trailing slash.
    container_base: String,
    /// The SAS query, e.g. `sv=…&sig=…`, with no leading `?`. **A credential**
    /// — see the module docs.
    sas_query: String,
    http: reqwest::Client,
}

/// Hand-written so an accidental `{:?}` cannot print the signature. Deriving
/// `Debug` here would put the whole SAS one interpolation away from a log line.
impl std::fmt::Debug for SasBlobFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SasBlobFetcher")
            .field("container_base", &self.container_base)
            .field("sas_query", &"<redacted>")
            .finish()
    }
}

impl SasBlobFetcher {
    /// Split a full SAS URL into its container base and query.
    ///
    /// A URL with no `?` is accepted as a bare container base with an empty
    /// query: the requests then go out unsigned and Azure rejects them, which
    /// is a clearer failure than refusing to construct.
    #[must_use]
    pub fn new(sas_url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client");
        Self::with_client(sas_url, http)
    }

    /// As [`SasBlobFetcher::new`], with a caller-supplied client. Exists so
    /// tests (and only tests) can point the fetcher at a mock server with a
    /// short timeout.
    #[must_use]
    pub fn with_client(sas_url: &str, http: reqwest::Client) -> Self {
        let (base, query) = match sas_url.split_once('?') {
            Some((base, query)) => (base, query),
            None => (sas_url, ""),
        };
        SasBlobFetcher {
            container_base: base.trim_end_matches('/').to_owned(),
            sas_query: query.to_owned(),
            http,
        }
    }

    /// Whether a SAS query is present — without exposing it.
    #[must_use]
    pub fn has_sas(&self) -> bool {
        !self.sas_query.is_empty()
    }

    fn with_sas(&self, url: &mut String, separator: char) {
        if !self.sas_query.is_empty() {
            url.push(separator);
            url.push_str(&self.sas_query);
        }
    }

    async fn get_text(&self, url: &str) -> Result<String, AzureCostError> {
        // Parse first: `url::ParseError` carries no URL, so a malformed SAS
        // cannot reach a string here.
        let parsed = reqwest::Url::parse(url).map_err(|_| AzureCostError::InvalidUrl)?;
        let response = self.http.get(parsed).send().await.map_err(unreachable)?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // The body is read only so `is_auth_failure` can see Azure's
            // `AuthenticationFailed`; it never reaches the displayed message.
            let body = response.text().await.ok();
            return Err(AzureCostError::Http { status, body });
        }
        response.text().await.map_err(unreachable)
    }
}

/// Strip the request URL — and with it the SAS — before a transport error
/// becomes a string.
fn unreachable(error: reqwest::Error) -> AzureCostError {
    AzureCostError::Unreachable(error.without_url().to_string())
}

impl BlobFetcher for SasBlobFetcher {
    async fn list_blobs(&self, prefix: &str) -> Result<Vec<String>, AzureCostError> {
        let mut names = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut url = format!(
                "{}?restype=container&comp=list&prefix={}",
                self.container_base,
                encode(prefix)
            );
            if let Some(marker) = &marker {
                url.push_str(&format!("&marker={}", encode(marker)));
            }
            self.with_sas(&mut url, '&');

            let page = parse_blob_list_xml(&self.get_text(&url).await?);
            names.extend(page.names);
            // Azure caps a page at 5000 blobs; a busy month's export exceeds
            // that, and stopping at page one would silently pick a stale run as
            // "newest".
            match page.next_marker {
                // A marker that does not advance would spin this loop forever
                // against a misbehaving endpoint. Stop instead: a short list is
                // recoverable, a hung poll task is not.
                Some(next) if Some(&next) == marker.as_ref() => break,
                Some(next) => marker = Some(next),
                None => break,
            }
        }
        Ok(names)
    }

    async fn get_blob_text(&self, path: &str) -> Result<String, AzureCostError> {
        let encoded: Vec<String> = path.split('/').map(encode).collect();
        let mut url = format!("{}/{}", self.container_base, encoded.join("/"));
        self.with_sas(&mut url, '?');
        self.get_text(&url).await
    }
}

/// Percent-encode one path or query segment the way `encodeURIComponent` does
/// — notably encoding `/`, so a multi-segment prefix survives as a single query
/// value instead of splitting into path segments.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn xml(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.to_owned(), "application/xml")
    }

    fn fetcher(server: &MockServer) -> SasBlobFetcher {
        SasBlobFetcher::new(&format!(
            "{}/cost-exports?sv=2024-11-04&sig=s3cret",
            server.uri()
        ))
    }

    // MARK: parse_blob_list_xml

    #[test]
    fn parses_names_and_treats_an_empty_marker_as_no_marker() {
        let page = parse_blob_list_xml(
            "<EnumerationResults><Blobs>\
             <Blob><Name>daily/x/000001.csv</Name></Blob>\
             <Blob><Name>daily/x/000002.csv</Name></Blob>\
             </Blobs><NextMarker></NextMarker></EnumerationResults>",
        );
        assert_eq!(page.names, ["daily/x/000001.csv", "daily/x/000002.csv"]);
        assert_eq!(page.next_marker, None);
    }

    #[test]
    fn returns_a_non_empty_marker() {
        assert_eq!(
            parse_blob_list_xml("<NextMarker>page2</NextMarker>").next_marker,
            Some("page2".to_owned())
        );
    }

    #[test]
    fn a_response_with_no_blobs_parses_to_an_empty_page() {
        let page = parse_blob_list_xml("<EnumerationResults><Blobs/></EnumerationResults>");
        assert_eq!(page, BlobListPage::default());
    }

    // MARK: SAS URL handling

    #[test]
    fn splits_the_sas_url_into_a_container_base_and_a_query() {
        let fetcher = SasBlobFetcher::new("https://acct.blob.core.windows.net/exports/?sv=1&sig=s");
        assert_eq!(
            fetcher.container_base,
            "https://acct.blob.core.windows.net/exports"
        );
        assert_eq!(fetcher.sas_query, "sv=1&sig=s");
        assert!(fetcher.has_sas());
    }

    #[test]
    fn a_url_with_no_query_has_no_sas() {
        let fetcher = SasBlobFetcher::new("https://acct.blob.core.windows.net/exports");
        assert!(!fetcher.has_sas());
    }

    /// The signature must not be one `{:?}` away from a log line.
    #[test]
    fn debug_redacts_the_sas_query() {
        let rendered = format!(
            "{:?}",
            SasBlobFetcher::new("https://acct.blob.core.windows.net/exports?sv=1&sig=s3cret")
        );
        assert!(!rendered.contains("s3cret"), "got {rendered}");
        assert!(rendered.contains("<redacted>"), "got {rendered}");
    }

    #[test]
    fn encode_escapes_slashes_and_spaces_like_encode_uri_component() {
        assert_eq!(encode("daily/mc-platform"), "daily%2Fmc-platform");
        assert_eq!(encode("a b~c._d"), "a%20b~c._d");
    }

    // MARK: list_blobs over the wire

    #[tokio::test]
    async fn lists_blobs_with_the_container_list_query_and_the_sas() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cost-exports"))
            .and(query_param("restype", "container"))
            .and(query_param("comp", "list"))
            .and(query_param("prefix", "daily/run/"))
            .and(query_param("sig", "s3cret"))
            .respond_with(xml(
                "<EnumerationResults><Blobs><Blob><Name>daily/run/000001.csv</Name></Blob>\
                 </Blobs><NextMarker/></EnumerationResults>",
            ))
            .mount(&server)
            .await;

        let names = fetcher(&server)
            .list_blobs("daily/run/")
            .await
            .expect("should list");
        assert_eq!(names, ["daily/run/000001.csv"]);
    }

    /// The pagination loop is the reason a big month lists correctly: page one
    /// hands back a marker, and the follow-up request must carry it.
    #[tokio::test]
    async fn follows_next_marker_pagination_to_the_last_page() {
        let server = MockServer::start().await;
        // Disjoint on `marker`, so neither mock can answer the other's request
        // — a page-one response served twice would loop this test forever.
        Mock::given(method("GET"))
            .and(path("/cost-exports"))
            .and(query_param_is_missing("marker"))
            .respond_with(xml(
                "<EnumerationResults><Blobs><Blob><Name>b/1.csv</Name></Blob></Blobs>\
                 <NextMarker>page2</NextMarker></EnumerationResults>",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cost-exports"))
            .and(query_param("marker", "page2"))
            .respond_with(xml(
                "<EnumerationResults><Blobs><Blob><Name>b/2.csv</Name></Blob></Blobs>\
                 <NextMarker></NextMarker></EnumerationResults>",
            ))
            .mount(&server)
            .await;

        let names = fetcher(&server)
            .list_blobs("b/")
            .await
            .expect("should list");
        assert_eq!(names, ["b/1.csv", "b/2.csv"], "both pages, in page order");
    }

    /// An endpoint that keeps handing back the same marker must not hang the
    /// poll task.
    #[tokio::test]
    async fn a_repeating_marker_stops_instead_of_looping_forever() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cost-exports"))
            .respond_with(xml(
                "<EnumerationResults><Blobs><Blob><Name>b/1.csv</Name></Blob></Blobs>\
                 <NextMarker>stuck</NextMarker></EnumerationResults>",
            ))
            .mount(&server)
            .await;

        let names = fetcher(&server)
            .list_blobs("b/")
            .await
            .expect("should list");
        assert_eq!(names, ["b/1.csv", "b/1.csv"], "one retry, then it stops");
    }

    #[tokio::test]
    async fn gets_a_blob_body_by_container_relative_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cost-exports/daily/run/000001.csv"))
            .and(query_param("sig", "s3cret"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("cost\n1", "text/csv"))
            .mount(&server)
            .await;

        let body = fetcher(&server)
            .get_blob_text("daily/run/000001.csv")
            .await
            .expect("should download");
        assert_eq!(body, "cost\n1");
    }

    /// A revoked SAS: the status alone settles it, and the operator gets the
    /// one message that leads to a fix.
    #[tokio::test]
    async fn a_403_becomes_an_auth_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cost-exports"))
            .respond_with(ResponseTemplate::new(403).set_body_raw(
                "<Error><Code>AuthenticationFailed</Code></Error>",
                "application/xml",
            ))
            .mount(&server)
            .await;

        let err = fetcher(&server).list_blobs("daily/").await.unwrap_err();
        assert!(err.is_auth_failure(), "got {err:?}");
        assert_eq!(
            err.user_message(),
            "SAS expired or invalid — paste a new one in Settings"
        );
    }

    #[tokio::test]
    async fn a_server_error_keeps_its_status_and_stays_a_plain_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cost-exports"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = fetcher(&server).list_blobs("daily/").await.unwrap_err();
        assert!(matches!(err, AzureCostError::Http { status: 503, .. }));
        assert!(!err.is_auth_failure());
    }

    /// The one that matters most: a dead host produces a transport error, and
    /// `reqwest` would have attached the full SAS URL to it.
    #[tokio::test]
    async fn a_transport_failure_never_carries_the_sas() {
        // Port 1 on loopback refuses immediately and needs no mock server.
        let fetcher = SasBlobFetcher::with_client(
            "http://127.0.0.1:1/cost-exports?sv=2024-11-04&sig=s3cret",
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
        );
        let err = fetcher.list_blobs("daily/").await.unwrap_err();
        let AzureCostError::Unreachable(message) = &err else {
            panic!("expected Unreachable, got {err:?}");
        };
        assert!(!message.contains("s3cret"), "leaked the SAS: {message}");
        assert!(!message.contains("sig="), "leaked the SAS: {message}");
        assert!(!format!("{err:?}").contains("s3cret"), "leaked via Debug");
    }
}
