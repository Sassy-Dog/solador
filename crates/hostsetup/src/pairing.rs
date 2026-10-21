//! Whether a pairing request adds a host, touches an existing one, or is
//! refused — decided before anything is written, from the store as it is.

use store::Host;
use uuid::Uuid;

use crate::Secret;

/// The sentence for a decision the host list moved under before it was
/// carried out ([`Pairing::confirm`]).
pub const MOVED: &str = "The host list changed while this request ran (a Settings edit, say). \
     Nothing was saved; run the command again.";

/// One pairing request, once the app's own probe has matched its fingerprint:
/// what [`decide`] decides about, and what a [`crate::server::HostBook`] is
/// asked to carry out.
#[derive(Debug, Clone, Copy)]
pub struct Pairing<'a> {
    pub name: &'a str,
    pub address: &'a str,
    pub port: u16,
    /// Canonical, and equal to what the app's own probe found there.
    pub pin: &'a str,
    pub token: &'a Secret,
    pub replace: bool,
}

impl Pairing<'_> {
    /// [`decide`] for this request.
    #[must_use]
    pub fn decide(&self, hosts: &[Host]) -> Decision {
        decide(hosts, self.address, self.port, self.pin, self.replace)
    }

    /// Re-asks [`decide`] against the host list as it is *now*, which the
    /// writer reads under the lock its own writes take.
    ///
    /// The server decides against a snapshot and then waits on the network (the
    /// probe, the health check). A host added or edited in Settings in that
    /// window must be neither duplicated nor overwritten, so a decision that no
    /// longer comes out the same is refused rather than carried out.
    ///
    /// # Errors
    /// [`MOVED`], when the answer is not `decided` any more.
    pub fn confirm(&self, hosts: &[Host], decided: &Decision) -> Result<(), String> {
        if self.decide(hosts) == *decided {
            Ok(())
        } else {
            Err(MOVED.to_owned())
        }
    }
}

/// What a pairing request does to the host list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// No host at this address and none holding this certificate.
    Add,
    /// A host already at this `address:port`, pinned to this certificate.
    /// Whether anything is written depends only on the token.
    SamePairing { id: Uuid },
    /// `replace` was given: re-pin the host at this address, or move the host
    /// holding this certificate to this address. Its token is replaced too.
    Replace { id: Uuid },
    /// Nothing is written; the sentence says what is already there and what
    /// would change it.
    Refused(String),
}

/// Decides what pairing `pin` (canonical, already matched against the app's
/// own probe) at `address:port` does, given the hosts already configured.
///
/// One machine must not become two cards, and an existing card must not be
/// repointed by a command that did not ask to. So two hosts conflict with a
/// request:
///
/// * one **at this address and port** — the same row, whatever it is pinned
///   to (or if it is unpaired);
/// * one **holding this certificate** at another address — the same agent,
///   reached another way (a tailnet IP saved earlier, a LAN IP now).
///
/// | Existing host                          | `replace` | Decision          |
/// |----------------------------------------|-----------|-------------------|
/// | this address, this pin                 | either    | `SamePairing`     |
/// | this address, another pin / no pin     | no        | `Refused`         |
/// | this address, another pin / no pin     | yes       | `Replace` (re-pin)|
/// | this pin, another address              | no        | `Refused`         |
/// | this pin, another address              | yes       | `Replace` (move)  |
/// | neither                                | either    | `Add`             |
///
/// The address comparison is the store's own notion of "the same address":
/// trimmed text, case-insensitive (a DNS name is), with no resolution — two
/// names for one machine are two rows until the operator says otherwise.
#[must_use]
pub fn decide(hosts: &[Host], address: &str, port: u16, pin: &str, replace: bool) -> Decision {
    // The row at this address is checked first: it is the one a `replace`
    // touches even when another row holds the pin elsewhere.
    if let Some(existing) = hosts.iter().find(|h| same_endpoint(h, address, port)) {
        return if holds(existing, pin) {
            Decision::SamePairing { id: existing.id }
        } else if replace {
            Decision::Replace { id: existing.id }
        } else {
            let state = match existing.tls_fingerprint {
                Some(_) => "is pinned to a different certificate",
                None => "is not paired (plain HTTP)",
            };
            Decision::Refused(format!(
                "{} at {} {state}; pass --replace to re-pin it to this one.",
                existing.name,
                endpoint(existing),
            ))
        };
    }
    match hosts.iter().find(|h| holds(h, pin)) {
        Some(existing) if replace => Decision::Replace { id: existing.id },
        Some(existing) => Decision::Refused(format!(
            "{} at {} already holds this certificate; pass --replace to move it to {}:{port}.",
            existing.name,
            endpoint(existing),
            address.trim(),
        )),
        None => Decision::Add,
    }
}

/// Whether `host` is pinned to `pin`. Both are canonical colon-hex; the case
/// fold covers a hand-edited `store.json`.
fn holds(host: &Host, pin: &str) -> bool {
    host.tls_fingerprint
        .as_deref()
        .is_some_and(|held| held.eq_ignore_ascii_case(pin))
}

fn same_endpoint(host: &Host, address: &str, port: u16) -> bool {
    host.port == port && host.address.trim().eq_ignore_ascii_case(address.trim())
}

fn endpoint(host: &Host) -> String {
    format!("{}:{}", host.address, host.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "AA";
    const OTHER: &str = "BB";

    fn host(name: &str, address: &str, port: u16, pin: Option<&str>) -> Host {
        let mut host = Host::new(name, address);
        host.port = port;
        host.tls_fingerprint = pin.map(str::to_owned);
        host
    }

    #[test]
    fn nothing_there_is_an_add() {
        let hosts = [host("other", "10.0.0.9", 7878, Some(OTHER))];
        assert_eq!(
            decide(&hosts, "192.168.1.2", 7878, PIN, false),
            Decision::Add
        );
        assert_eq!(decide(&[], "192.168.1.2", 7878, PIN, true), Decision::Add);
    }

    #[test]
    fn the_same_address_and_pin_is_the_same_pairing_whatever_replace_says() {
        let existing = host("agent", "192.168.1.2", 7878, Some(PIN));
        let id = existing.id;
        for replace in [false, true] {
            assert_eq!(
                decide(
                    std::slice::from_ref(&existing),
                    "192.168.1.2",
                    7878,
                    PIN,
                    replace
                ),
                Decision::SamePairing { id }
            );
        }
        // Text-equal is the rule: case and surrounding space do not make a
        // second row.
        assert_eq!(
            decide(
                std::slice::from_ref(&existing),
                " 192.168.1.2 ",
                7878,
                PIN,
                false
            ),
            Decision::SamePairing { id }
        );
    }

    #[test]
    fn another_port_at_the_same_address_is_not_the_same_endpoint() {
        let hosts = [host("agent", "192.168.1.2", 7879, Some(OTHER))];
        assert_eq!(
            decide(&hosts, "192.168.1.2", 7878, PIN, false),
            Decision::Add
        );
    }

    #[test]
    fn a_differently_pinned_or_unpaired_host_at_this_address_needs_replace() {
        for pin in [Some(OTHER), None] {
            let existing = host("agent", "AGENT.lan", 7878, pin);
            let id = existing.id;
            let refused = decide(
                std::slice::from_ref(&existing),
                "agent.lan",
                7878,
                PIN,
                false,
            );
            let Decision::Refused(message) = refused else {
                panic!("expected a refusal for {pin:?}, got {refused:?}");
            };
            assert!(message.contains("agent"), "{message}");
            assert!(message.contains("--replace"), "{message}");
            assert_eq!(
                decide(
                    std::slice::from_ref(&existing),
                    "agent.lan",
                    7878,
                    PIN,
                    true
                ),
                Decision::Replace { id }
            );
        }
    }

    #[test]
    fn this_certificate_at_another_address_needs_replace_to_move_it() {
        let existing = host("agent-1", "100.64.0.9", 7878, Some(PIN));
        let id = existing.id;
        let refused = decide(
            std::slice::from_ref(&existing),
            "192.168.1.2",
            7878,
            PIN,
            false,
        );
        let Decision::Refused(message) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert!(message.contains("agent-1"), "{message}");
        assert!(message.contains("100.64.0.9:7878"), "{message}");
        assert!(message.contains("--replace"), "{message}");
        assert_eq!(
            decide(
                std::slice::from_ref(&existing),
                "192.168.1.2",
                7878,
                PIN,
                true
            ),
            Decision::Replace { id }
        );
    }

    #[test]
    fn the_host_at_this_address_wins_over_one_holding_the_pin_elsewhere() {
        // Both conflicts at once: the row at the address is the one a
        // `replace` touches, never the one elsewhere.
        let here = host("here", "192.168.1.2", 7878, Some(OTHER));
        let there = host("there", "100.64.0.9", 7878, Some(PIN));
        let hosts = [there, here.clone()];
        assert_eq!(
            decide(&hosts, "192.168.1.2", 7878, PIN, true),
            Decision::Replace { id: here.id }
        );
    }

    #[test]
    fn a_decision_the_host_list_moved_under_is_not_carried_out() {
        let token = Secret::new("t");
        let pairing = Pairing {
            name: "agent",
            address: "192.168.1.2",
            port: 7878,
            pin: PIN,
            token: &token,
            replace: false,
        };
        assert_eq!(pairing.confirm(&[], &Decision::Add), Ok(()));
        // Settings added a host at the same address while the probe and the
        // health check ran: adding another would be a second card.
        let added_meanwhile = [host("by hand", "192.168.1.2", 7878, None)];
        assert_eq!(
            pairing.confirm(&added_meanwhile, &Decision::Add),
            Err(MOVED.to_owned())
        );
        // The row decided on was deleted: nothing is left to re-point.
        let row = host("agent", "192.168.1.2", 7878, Some(PIN));
        let same = Decision::SamePairing { id: row.id };
        assert_eq!(pairing.confirm(std::slice::from_ref(&row), &same), Ok(()));
        assert_eq!(pairing.confirm(&[], &same), Err(MOVED.to_owned()));
    }
}
