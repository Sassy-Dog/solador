//! The stock vocabulary for anticipated failures — one short sentence per
//! state, naming the vendor that failed.
//!
//! Every vendor crate used to invent its own wording for the same handful of
//! failures, so how useful a warning was depended on which crate happened to
//! produce it: `crates/agentclient` wrote a full sentence, `crates/usage`'s
//! Vercel client wrote four words, and Neon and Sentry interpolated the whole
//! `reqwest` error — request URL, query string and all — into the panel
//! footer. A percent-encoded URL is not a diagnosis.
//!
//! # The split
//!
//! **Error enums classify; this module owns the words.** An error type says
//! *what happened* in terms a caller can branch on — the shape
//! `is_auth_failure()` already has — and [`Fault::message`] turns kind + vendor
//! into the one canonical sentence. That is why no vendor crate depends on
//! `viewmodel`: data access must never point at presentation. The seam is the
//! call site that holds both, and today that is `app/src-tauri` (see
//! `azure/sas.rs`, which renders two of its three states straight out of this
//! vocabulary).
//!
//! `crates/usage`'s Neon and Sentry clients still render their own stock
//! sentences, because the shell calls `user_message()` on them and the mapping
//! has nowhere else to live until #354 moves it. Those sentences are *this*
//! module's, character for character, and `app/src-tauri/tests/
//! fault_vocabulary.rs` — the only crate that can see both sides — asserts it.
//!
//! # What a stock sentence may say
//!
//! Only the vendor's name is interpolated. A transport error, a request URL, a
//! response body, a service error document and a stack trace are all *detail*,
//! and detail goes to the log — the panel gets the sentence. Sentences are
//! short enough ([`MAX_MESSAGE_CHARS`]) to sit in a Half-width card's header
//! without ellipsising.
//!
//! # The fallback is not a category
//!
//! [`Fault::Unexpected`] exists so an unanticipated failure can be *said*
//! rather than sorted into whichever anticipated bucket looks closest. Forcing
//! an unknown failure into a known one is the same class of error as a
//! defaulted number: it reads as a diagnosis and is a guess. Convention #4
//! ("never fabricate a value to fill a gap") binds a state exactly as it binds
//! a figure.

/// A failure state this codebase anticipates — or the admission that this one
/// was not anticipated.
///
/// Deliberately about *kinds*, not codes: a caller that can name something
/// sharper (a status, an org id to fix, a command to run) should still say it.
/// This is the floor a message may not fall below, never a cap on how specific
/// one may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fault {
    /// Nothing saved in Settings yet — the operator has not connected this
    /// vendor.
    NotConfigured,
    /// The request never completed: DNS, TLS, timeout, connection reset, or no
    /// network at all.
    Unreachable,
    /// 401 / 403 — the credential is missing, mistyped, revoked, or too
    /// narrowly scoped.
    CredentialRejected,
    /// 404 — the org, slug or account named in Settings does not exist.
    NotFound,
    /// 429 — the vendor is throttling this read.
    RateLimited,
    /// 5xx — the vendor is failing on its own side and there is nothing here
    /// to fix.
    VendorFailure,
    /// The response arrived and did not decode into the shape this build
    /// expects.
    Undecodable,
    /// A tool this panel shells out to is missing, or is installed and not
    /// signed in. The `az` case.
    ToolUnavailable,
    /// None of the above.
    ///
    /// Its sentence promises nothing except that something failed and the log
    /// knows more. That is the honest claim, and it is the whole point of the
    /// variant: see the module note above.
    Unexpected,
}

/// The longest a stock sentence may be, in characters.
///
/// A panel header is one line beside two clocks, and the narrowest card a
/// panel can be placed in is a Quarter of a four-quarter grid — but the
/// containment budget this is sized against is a **Half**, the width at which
/// the screenshot in #352 wrapped a Neon warning to six lines. #351 makes an
/// over-long message survivable at the rendering layer; this bound is what
/// makes it comfortable rather than merely survivable.
///
/// It is a bound on *this vocabulary*, not on every message the app can show:
/// a vendor's own words (an `az` refusal, say) are quoted, not authored here,
/// and are clamped by whoever quotes them.
pub const MAX_MESSAGE_CHARS: usize = 64;

impl Fault {
    /// Classify an HTTP status.
    ///
    /// A status this vocabulary does not recognise is [`Fault::Unexpected`] —
    /// **never** folded into the nearest named state. A 400 is not a
    /// credential problem and a 418 is not an outage; reporting either as one
    /// would read as a diagnosis and be a guess.
    #[must_use]
    pub const fn from_http_status(status: u16) -> Self {
        match status {
            401 | 403 => Fault::CredentialRejected,
            404 => Fault::NotFound,
            429 => Fault::RateLimited,
            500..=599 => Fault::VendorFailure,
            _ => Fault::Unexpected,
        }
    }

    /// Whether this is one of the states the codebase anticipates.
    ///
    /// False for [`Fault::Unexpected`] and nothing else. Callers that want to
    /// log loudly on the unanticipated path branch on this rather than
    /// re-matching the enum.
    #[must_use]
    pub const fn is_anticipated(self) -> bool {
        !matches!(self, Fault::Unexpected)
    }

    /// The one canonical sentence for this state, naming `vendor`.
    ///
    /// `vendor` is the operator-facing name — "Neon", "Sentry", "Azure CLI" —
    /// and it is the only thing interpolated. Nothing a transport, a response
    /// or a subprocess produced belongs in here.
    #[must_use]
    pub fn message(self, vendor: &str) -> String {
        match self {
            Fault::NotConfigured => format!("{vendor} isn't configured — add it in Settings"),
            Fault::Unreachable => format!("couldn't reach {vendor}"),
            Fault::CredentialRejected => {
                format!("{vendor} rejected the credential — update it in Settings")
            }
            Fault::NotFound => format!("{vendor} has no such account — check Settings"),
            Fault::RateLimited => format!("{vendor} is rate-limiting this read"),
            Fault::VendorFailure => format!("{vendor} is failing on its side"),
            Fault::Undecodable => format!("couldn't read the {vendor} response"),
            Fault::ToolUnavailable => format!("{vendor} not found — install it and sign in"),
            Fault::Unexpected => format!("{vendor} failed — details in the log"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every state, so a variant added later cannot slip past the invariants
    /// below by never being enumerated.
    const ALL: &[Fault] = &[
        Fault::NotConfigured,
        Fault::Unreachable,
        Fault::CredentialRejected,
        Fault::NotFound,
        Fault::RateLimited,
        Fault::VendorFailure,
        Fault::Undecodable,
        Fault::ToolUnavailable,
        Fault::Unexpected,
    ];

    /// The vendor names this app actually renders, longest included — the
    /// bound is meaningless against a name nobody uses.
    const VENDORS: &[&str] = &["Neon", "Sentry", "Vercel", "GitHub", "Azure CLI"];

    /// The bug in #352: a request URL, percent-encoded parameters and a query
    /// string reached three panel headers. Asserted per state rather than by
    /// reading the source, because the leak arrived through interpolation that
    /// looked innocent at the construction site.
    #[test]
    fn no_stock_sentence_can_carry_a_url() {
        for fault in ALL {
            for vendor in VENDORS {
                let message = fault.message(vendor);
                for forbidden in ["://", "http", "?", "%", "&", "="] {
                    assert!(
                        !message.contains(forbidden),
                        "{fault:?} for {vendor} carries {forbidden:?}: {message}"
                    );
                }
            }
        }
    }

    /// A warning that does not say *who* failed sends the operator looking
    /// through five panels for the one that is red.
    #[test]
    fn every_stock_sentence_names_the_vendor() {
        for fault in ALL {
            for vendor in VENDORS {
                let message = fault.message(vendor);
                assert!(
                    message.contains(vendor),
                    "{fault:?} does not name {vendor}: {message}"
                );
            }
        }
    }

    /// Six wrapped lines cut off mid-token is what this vocabulary replaces.
    #[test]
    fn every_stock_sentence_fits_a_panel_header() {
        for fault in ALL {
            for vendor in VENDORS {
                let message = fault.message(vendor);
                let width = message.chars().count();
                assert!(
                    width <= MAX_MESSAGE_CHARS,
                    "{fault:?} for {vendor} is {width} chars: {message}"
                );
            }
        }
    }

    /// Two states that read alike are one state with extra steps — and the
    /// operator cannot tell "fix your key" from "Neon is down".
    #[test]
    fn no_two_states_read_alike() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(
                    a.message("Neon"),
                    b.message("Neon"),
                    "{a:?} and {b:?} render the same sentence"
                );
            }
        }
    }

    #[test]
    fn the_recognised_statuses_map_to_the_states_they_mean() {
        assert_eq!(Fault::from_http_status(401), Fault::CredentialRejected);
        assert_eq!(Fault::from_http_status(403), Fault::CredentialRejected);
        assert_eq!(Fault::from_http_status(404), Fault::NotFound);
        assert_eq!(Fault::from_http_status(429), Fault::RateLimited);
        assert_eq!(Fault::from_http_status(500), Fault::VendorFailure);
        assert_eq!(Fault::from_http_status(503), Fault::VendorFailure);
        assert_eq!(Fault::from_http_status(599), Fault::VendorFailure);
    }

    /// The load-bearing one. A status nobody anticipated must land in the
    /// fallback, not in whichever named state is arithmetically nearest — a
    /// 400 reported as "update your credential" sends the operator to rotate a
    /// key that was never the problem.
    #[test]
    fn an_unrecognised_status_lands_in_the_fallback_not_the_nearest_named_state() {
        for status in [400u16, 402, 405, 418, 451, 302, 100] {
            let fault = Fault::from_http_status(status);
            assert_eq!(
                fault,
                Fault::Unexpected,
                "HTTP {status} was classified as {fault:?}"
            );
            assert!(!fault.is_anticipated(), "HTTP {status}");
        }
    }

    /// …and the fallback's sentence must not *read* like an anticipated one
    /// either. Landing in the right variant and then borrowing a neighbour's
    /// words would leak the same wrong diagnosis one layer later.
    #[test]
    fn the_fallback_never_borrows_an_anticipated_sentence() {
        let fallback = Fault::Unexpected.message("Neon");
        for fault in ALL.iter().filter(|f| f.is_anticipated()) {
            assert_ne!(fallback, fault.message("Neon"), "borrowed from {fault:?}");
        }
        assert!(
            !Fault::Unexpected.is_anticipated(),
            "the fallback is the one state that is not anticipated"
        );
        for fault in ALL.iter().filter(|f| **f != Fault::Unexpected) {
            assert!(fault.is_anticipated(), "{fault:?}");
        }
    }
}
