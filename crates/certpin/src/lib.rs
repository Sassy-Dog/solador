//! The certificate fingerprint format, in one place (#448).
//!
//! An agent serving its own self-signed certificate (#447) and a cockpit that
//! pins it (#448) have to agree, byte for byte, on what a fingerprint looks
//! like: it is the string an operator reads off `solador-agent
//! tls-fingerprint` and compares with what Settings shows, and it is the string
//! `store.json` keeps. Two implementations of that would drift silently — the
//! agent uppercases, the cockpit lowercases, and a pin that is right stops
//! matching.
//!
//! **The format:** SHA-256 over the *whole DER certificate*, written as
//! uppercase hex pairs separated by colons — `45:39:AF:…:4F`, 95 characters.
//!
//! **What this crate does not do:** hash. It has no dependencies, so each
//! consumer computes the digest with the SHA-256 it already resolves and hands
//! over 32 bytes. That is also why the two consumers' tests share a fixture
//! (`tests/fixtures/tls/`): the digest step is theirs, the string is ours, and
//! only a shared certificate and its expected string prove the two halves meet.

/// The length of a SHA-256 digest, in bytes.
pub const DIGEST_LEN: usize = 32;

/// A fingerprint's canonical text length: 32 hex pairs and 31 colons.
pub const TEXT_LEN: usize = DIGEST_LEN * 3 - 1;

/// A SHA-256 digest as the canonical fingerprint: uppercase hex, colon
/// separated. The form `solador-agent tls-fingerprint` prints.
#[must_use]
pub fn format(digest: &[u8; DIGEST_LEN]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(TEXT_LEN);
    for (i, byte) in digest.iter().enumerate() {
        if i > 0 {
            out.push(':');
        }
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0F)]));
    }
    out
}

/// Reads a fingerprint back into its digest, or `None` when `text` is not one.
///
/// Lenient about what an operator's clipboard does — case, surrounding
/// whitespace, and colons, spaces or dashes between the pairs (or none at all,
/// which is how `sha256sum` writes it) — and strict about everything that
/// matters: exactly 32 bytes of hex, nothing else. A prefix of a fingerprint is
/// not a fingerprint; a pin that matches on 31 bytes is a pin that does not
/// mean what it says.
#[must_use]
pub fn parse(text: &str) -> Option<[u8; DIGEST_LEN]> {
    let mut nibbles = text
        .trim()
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()));
    let mut digest = [0u8; DIGEST_LEN];
    for byte in &mut digest {
        let high = nibbles.next()??;
        let low = nibbles.next()??;
        *byte = (high << 4) | low;
    }
    // Anything left over means more than 32 bytes.
    nibbles.next().is_none().then_some(digest)
}

/// The canonical spelling of a fingerprint the operator or the store handed
/// over, or `None` when it is not one. `normalise(&format(d)) == Some(format(d))`
/// for every digest, which is what lets two fingerprints be compared as strings.
#[must_use]
pub fn normalise(text: &str) -> Option<String> {
    parse(text).map(|digest| format(&digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared vector: a certificate and the string it must fingerprint to.
    /// `agent/src/tls.rs` and `crates/agentclient` assert the same pair with
    /// their own SHA-256, which is the point of it being a file and not a
    /// constant in each.
    const EXPECTED: &str = include_str!("../../../tests/fixtures/tls/pinned-cert.sha256");

    fn sample() -> [u8; DIGEST_LEN] {
        let mut d = [0u8; DIGEST_LEN];
        for (i, byte) in d.iter_mut().enumerate() {
            *byte = u8::try_from(i * 8 + 5).expect("fits");
        }
        d
    }

    #[test]
    fn formats_uppercase_colon_hex() {
        let text = format(&sample());
        assert_eq!(text.len(), TEXT_LEN);
        assert!(text.starts_with("05:0D:15:1D:"), "{text}");
        assert_eq!(text, text.to_uppercase());
        assert_eq!(text.matches(':').count(), DIGEST_LEN - 1);
    }

    #[test]
    fn the_fixture_fingerprint_is_in_the_canonical_form() {
        assert_eq!(EXPECTED.len(), TEXT_LEN);
        assert_eq!(normalise(EXPECTED).as_deref(), Some(EXPECTED));
    }

    #[test]
    fn parse_inverts_format() {
        assert_eq!(parse(&format(&sample())), Some(sample()));
    }

    #[test]
    fn a_pasted_fingerprint_is_read_whatever_its_case_or_separators() {
        let canonical = format(&sample());
        let lower = canonical.to_lowercase();
        let bare: String = lower.chars().filter(|c| *c != ':').collect();
        let spaced = canonical.replace(':', " ");
        let dashed = canonical.replace(':', "-");
        for variant in [
            canonical.clone(),
            lower,
            bare,
            spaced,
            dashed,
            format!("  {canonical}\n"),
        ] {
            assert_eq!(normalise(&variant).as_deref(), Some(canonical.as_str()));
        }
    }

    #[test]
    fn anything_that_is_not_exactly_32_bytes_of_hex_is_refused() {
        let canonical = format(&sample());
        let short = &canonical[..canonical.len() - 3];
        let long = format!("{canonical}:00");
        let odd = &canonical[..canonical.len() - 1];
        let not_hex = canonical.replacen("05", "0G", 1);
        for bad in ["", "  ", "sha256:abc", short, odd, &long, &not_hex] {
            assert_eq!(parse(bad), None, "{bad:?} must not parse");
        }
    }
}
