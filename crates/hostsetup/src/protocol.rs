//! What crosses the hand-off channel (a Unix socket or a Windows named pipe): one JSON request line, one JSON reply line.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Secret;

/// Bumped whenever a request or reply changes shape. The command mode is the
/// executable on disk and the server is whatever copy is *running*, so after an
/// in-place update the two can differ; `hello` is how the command mode finds
/// out before it sends anything that matters.
pub const PROTOCOL: u32 = 1;

/// The longest request line the server reads. An `add` is a few hundred bytes;
/// anything near this is not a request.
pub const MAX_LINE: usize = 16 * 1024;

/// One request: the protocol version beside the operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    #[serde(flatten)]
    pub request: Request,
}

impl Envelope {
    #[must_use]
    pub fn new(request: Request) -> Self {
        Envelope {
            v: PROTOCOL,
            request,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// Answered whatever `v` says, so a mismatch can be reported rather than
    /// guessed at.
    Hello,
    /// Every configured host, without a token.
    List,
    /// Fetch the certificate at an address without trusting it, exactly as
    /// Settings' **Check certificate** does.
    Probe { address: String, port: u16 },
    /// Pair a host: pinned only if what the app itself finds at
    /// `address:port` is `expected_fingerprint`, and saved only if the agent
    /// then accepts `token` over that pinned connection.
    Add(AddRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddRequest {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub token: Secret,
    /// What the host itself printed for `solador-agent tls-fingerprint`, read
    /// over a channel the operator already trusts (an ssh session, or their
    /// own eyes). The app compares its own probe against it; it is never
    /// stored unless the two match.
    pub expected_fingerprint: String,
    /// Allows re-pinning a host already at this address, or moving a host
    /// already paired with this certificate to the new address. Without it
    /// both are refused.
    #[serde(default)]
    pub replace: bool,
}

/// One reply. **No variant carries a token**, in either direction of any
/// operation: the socket is a way in for a credential, never a way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Response {
    Hello {
        protocol: u32,
        /// The running app's CalVer, when it can name itself.
        #[serde(skip_serializing_if = "Option::is_none")]
        app_version: Option<String>,
    },
    Hosts {
        hosts: Vec<HostSummary>,
    },
    Probe {
        probe: ProbeSummary,
    },
    Paired(Paired),
    /// The request was not carried out, and this sentence says why — and
    /// what, if anything, was written regardless (a row saved whose token
    /// the credential store then refused says so). Every other refusal
    /// changed nothing.
    Refused {
        message: String,
    },
}

impl Response {
    #[must_use]
    pub fn refused(message: impl Into<String>) -> Self {
        Response::Refused {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSummary {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

impl From<&store::Host> for HostSummary {
    fn from(host: &store::Host) -> Self {
        HostSummary {
            id: host.id,
            name: host.name.clone(),
            address: host.address.clone(),
            port: host.port,
            enabled: host.enabled,
            fingerprint: host.tls_fingerprint.clone(),
        }
    }
}

/// What a probe found, in the three findings the Settings form paints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ProbeSummary {
    Tls { fingerprint: String },
    NoTls,
    Failed { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// A new host row, pinned, with its token stored.
    Added,
    /// Already present with this pin and this token: nothing was written.
    Unchanged,
    /// Already present with this pin; the stored token was replaced.
    TokenReplaced,
    /// `replace` re-pinned a host at this address, or moved the host that
    /// holds this certificate here. Its token was replaced too.
    Updated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paired {
    pub outcome: Outcome,
    pub host: HostSummary,
    /// The Settings **Test** line for the pinned health check that gated the
    /// save (`✓ <hostname> · agent v…`).
    pub health: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_with_the_version_beside_the_op() {
        let add = Envelope::new(Request::Add(AddRequest {
            name: "agent".into(),
            address: "192.168.1.2".into(),
            port: 7878,
            token: Secret::new("t0ken"),
            expected_fingerprint: "AA:BB".into(),
            replace: false,
        }));
        let line = serde_json::to_string(&add).unwrap();
        assert!(line.starts_with(r#"{"v":1,"op":"add","#), "{line}");
        assert!(line.contains(r#""expectedFingerprint":"AA:BB""#), "{line}");
        assert_eq!(serde_json::from_str::<Envelope>(&line).unwrap(), add);

        let hello: Envelope = serde_json::from_str(r#"{"v":7,"op":"hello"}"#).unwrap();
        assert_eq!(hello.request, Request::Hello);
        assert_eq!(hello.v, 7);
    }

    #[test]
    fn an_add_without_replace_defaults_to_refusing_replacement() {
        let line = r#"{"v":1,"op":"add","name":"n","address":"a","port":1,"token":"t","expectedFingerprint":"f"}"#;
        let Request::Add(add) = serde_json::from_str::<Envelope>(line).unwrap().request else {
            panic!("not an add");
        };
        assert!(!add.replace);
    }

    #[test]
    fn replies_are_tagged_and_carry_no_token_field() {
        let paired = Response::Paired(Paired {
            outcome: Outcome::TokenReplaced,
            host: HostSummary {
                id: Uuid::nil(),
                name: "agent".into(),
                address: "192.168.1.2".into(),
                port: 7878,
                enabled: true,
                fingerprint: Some("AA".into()),
            },
            health: "✓ agent · agent v1".into(),
        });
        let line = serde_json::to_string(&paired).unwrap();
        assert!(line.contains(r#""result":"paired""#), "{line}");
        assert!(line.contains(r#""outcome":"token-replaced""#), "{line}");
        assert!(!line.to_lowercase().contains("token\":"), "{line}");
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), paired);
    }
}
