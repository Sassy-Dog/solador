//! Where the bearer token may travel over plain HTTP (#449 part 3).
//!
//! An agent that serves TLS is reached by a **paired** host, pinned to its
//! certificate. An **unpaired** host is dialled over `http://`, and that is
//! only safe where the network itself is what protects the token: this
//! machine, or the tailnet. An agent may now listen on any interface (#449),
//! so the cockpit can no longer assume that an address it was given is one of
//! those. This module is the rule, and it is the whole of it:
//!
//! > The bearer token is never sent over plain HTTP to an address that is not
//! > loopback and not Tailscale.
//!
//! * Tailscale is IPv4 `100.64.0.0/10` (CGNAT, the range Tailscale assigns
//!   from) and IPv6 `fd7a:115c:a1e0::/48` **minus** the 4via6 prefix
//!   `fd7a:115c:a1e0:b1a::/64`: an address there is a subnet router's
//!   translation of a *foreign* IPv4 address, and the router's last hop leaves
//!   the tailnet in cleartext.
//! * **This trusts an address range, not tailnet membership.** `100.64.0.0/10`
//!   is RFC 6598 shared (CGNAT) space that some ISPs, hotels and cloud VPCs
//!   also use, so an address in it may not be on a tailnet at all. The guard
//!   cannot tell; pairing (pinned TLS) is the stronger protection and the one
//!   to prefer for any host that is not this machine.
//! * An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is judged as the IPv4
//!   address it stands for: it is the same destination on a dual-stack socket.
//! * A **name** is permitted only if *every* address it resolves to is. One
//!   public address among tailnet ones is a refusal, because the connector is
//!   free to pick it.
//! * The check and the connection use the **same** resolution: [`Vetted`] is
//!   the client's resolver, so the addresses connected to are the addresses
//!   that were checked and there is no second lookup to disagree with the
//!   first (no check-then-connect race).
//! * A name that does not resolve sends nothing.
//!
//! The functions here are pure so the table of addresses can be exhaustive.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Tailscale's IPv4 range, `100.64.0.0/10`.
fn is_tailscale_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// Tailscale's IPv6 range, `fd7a:115c:a1e0::/48`, without its 4via6 prefix
/// `fd7a:115c:a1e0:b1a::/64` (see the module docs).
fn is_tailscale_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    s[..3] == [0xfd7a, 0x115c, 0xa1e0] && s[3] != 0x0b1a
}

/// The IPv4 address an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) stands for.
///
/// Only that form. `::a.b.c.d` (IPv4-compatible, deprecated) is deliberately
/// *not* unwrapped: nothing legitimate is addressed that way, so it falls
/// through to the IPv6 rules and is refused.
fn mapped_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    (s[..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff).then(|| {
        Ipv4Addr::new(
            (s[6] >> 8) as u8,
            (s[6] & 0xff) as u8,
            (s[7] >> 8) as u8,
            (s[7] & 0xff) as u8,
        )
    })
}

/// May the bearer token cross plain HTTP to this one address?
#[must_use]
pub fn plain_http_permitted(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || is_tailscale_v4(v4),
        IpAddr::V6(v6) => match mapped_v4(v6) {
            Some(v4) => plain_http_permitted(IpAddr::V4(v4)),
            None => v6.is_loopback() || is_tailscale_v6(v6),
        },
    }
}

/// May the token cross plain HTTP to a destination that resolves to `addrs`?
///
/// Every address must be permitted, and there must be at least one: an empty
/// answer is a name that resolved to nothing, and nothing is sent to it.
#[must_use]
pub fn all_plain_http_permitted(addrs: &[IpAddr]) -> bool {
    !addrs.is_empty() && addrs.iter().all(|ip| plain_http_permitted(*ip))
}

/// The marker a refused resolution carries through `reqwest`'s error chain, so
/// [`crate::send_failed`] can tell "the guard said no" from "the network did".
#[derive(Debug)]
pub(crate) struct Refused;

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("plain HTTP refused off loopback and the tailnet")
    }
}

impl std::error::Error for Refused {}

type LookupFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;

/// How a host name becomes addresses. The system resolver in production; a
/// fixed table in the tests, which is what lets one map a name to an address
/// whose listener is local.
pub(crate) type Lookup = Arc<dyn Fn(String) -> LookupFuture + Send + Sync>;

/// The system resolver.
pub(crate) fn system_lookup() -> Lookup {
    Arc::new(|name: String| {
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(found.map(|addr| addr.ip()).collect())
        })
    })
}

/// The address policy the resolver applies. A function pointer so the tests can
/// substitute one to make a *local* listener count as off-tailnet, and so the
/// negative control can switch the guard off entirely.
pub(crate) type Policy = fn(&[IpAddr]) -> bool;

/// The unpaired client's resolver: resolves, vets **every** address, and hands
/// the connector only what it vetted — or an error, and no connection.
pub(crate) struct Vetted {
    lookup: Lookup,
    policy: Policy,
}

impl Vetted {
    pub(crate) fn new(lookup: Lookup, policy: Policy) -> Self {
        Self { lookup, policy }
    }
}

impl Resolve for Vetted {
    fn resolve(&self, name: Name) -> Resolving {
        let lookup = Arc::clone(&self.lookup);
        let policy = self.policy;
        Box::pin(async move {
            let ips = lookup(name.as_str().to_owned()).await?;
            if !policy(&ips) {
                return Err(Box::new(Refused) as Box<dyn std::error::Error + Send + Sync>);
            }
            let addrs: Addrs = Box::new(
                ips.into_iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn loopback_is_permitted_in_both_families() {
        for a in ["127.0.0.1", "127.0.0.2", "127.255.255.255", "::1"] {
            assert!(plain_http_permitted(ip(a)), "{a}");
        }
    }

    #[test]
    fn the_tailscale_ipv4_range_is_exactly_100_64_0_0_10() {
        for a in [
            "100.64.0.0",
            "100.64.0.1",
            "100.100.100.100",
            "100.127.255.255",
        ] {
            assert!(plain_http_permitted(ip(a)), "{a}");
        }
        // Both edges, one step outside.
        for a in ["100.63.255.255", "100.128.0.0", "99.64.0.1", "101.64.0.1"] {
            assert!(!plain_http_permitted(ip(a)), "{a}");
        }
    }

    #[test]
    fn the_tailscale_ipv6_range_is_fd7a_115c_a1e0_48_without_the_4via6_prefix() {
        for a in [
            "fd7a:115c:a1e0::",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:ab12:4843:cd96:6265:a1e0",
            "fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff",
            // The edges of the excluded /64, one step outside it.
            "fd7a:115c:a1e0:b19:ffff:ffff:ffff:ffff",
            "fd7a:115c:a1e0:b1b::",
            "fd7a:115c:a1e0:b1a0::1",
            "fd7a:115c:a1e0:1b1a::1",
        ] {
            assert!(plain_http_permitted(ip(a)), "{a}");
        }
        for a in [
            "fd7a:115c:a1df:ffff:ffff:ffff:ffff:ffff",
            "fd7a:115c:a1e1::",
            "fd7a:115d:a1e0::1",
            "fd7b:115c:a1e0::1",
            "fd00::1",
            // 4via6 (`fd7a:115c:a1e0:b1a::/64`): both edges and the middle.
            "fd7a:115c:a1e0:b1a::",
            "fd7a:115c:a1e0:b1a::1",
            "fd7a:115c:a1e0:b1a:0:7:c0a8:101",
            "fd7a:115c:a1e0:b1a:ffff:ffff:ffff:ffff",
        ] {
            assert!(!plain_http_permitted(ip(a)), "{a}");
        }
    }

    #[test]
    fn private_lan_link_local_and_public_addresses_are_refused() {
        for a in [
            // RFC 1918
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "192.168.1.20",
            // Link-local, unspecified, broadcast, multicast
            "169.254.1.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            // Public
            "8.8.8.8",
            "1.1.1.1",
            "203.0.113.5",
            "192.0.2.1",
            // IPv6: unique-local that is not Tailscale's, link-local, global,
            // unspecified
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2606:4700:4700::1111",
            "::",
        ] {
            assert!(!plain_http_permitted(ip(a)), "{a}");
        }
    }

    #[test]
    fn an_ipv4_mapped_ipv6_address_is_judged_as_the_ipv4_it_stands_for() {
        // The same destination on a dual-stack socket, so the same answer.
        for a in [
            "::ffff:127.0.0.1",
            "::ffff:100.64.0.1",
            "::ffff:100.127.1.1",
        ] {
            assert!(plain_http_permitted(ip(a)), "{a}");
        }
        for a in [
            "::ffff:192.168.1.1",
            "::ffff:8.8.8.8",
            "::ffff:10.0.0.1",
            "::ffff:100.128.0.1",
            "::ffff:0.0.0.0",
        ] {
            assert!(!plain_http_permitted(ip(a)), "{a}");
        }
        // The deprecated IPv4-compatible form is not unwrapped, so a
        // loopback-looking one is not loopback.
        assert!(!plain_http_permitted(ip("::127.0.0.1")));
        assert!(!plain_http_permitted(ip("::100.64.0.1")));
    }

    #[test]
    fn a_name_is_permitted_only_if_every_address_it_resolves_to_is() {
        let tail = ip("100.64.0.9");
        let tail6 = ip("fd7a:115c:a1e0::9");
        let lo = ip("127.0.0.1");
        let lan = ip("192.168.1.20");
        let public = ip("8.8.8.8");
        assert!(all_plain_http_permitted(&[tail]));
        assert!(all_plain_http_permitted(&[tail, tail6, lo, ip("::1")]));
        // One bad address among good ones is a refusal, in every position.
        assert!(!all_plain_http_permitted(&[tail, lan]));
        assert!(!all_plain_http_permitted(&[lan, tail]));
        assert!(!all_plain_http_permitted(&[tail, tail6, public]));
        assert!(!all_plain_http_permitted(&[lo, ip("::ffff:10.0.0.1")]));
        // Nothing resolved: nothing is sent.
        assert!(!all_plain_http_permitted(&[]));
    }
}
