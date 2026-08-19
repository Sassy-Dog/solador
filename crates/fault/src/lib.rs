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
//! **Error enums classify; this crate owns the words.** An error type says
//! *what happened* in terms a caller can branch on — the shape
//! `is_auth_failure()` already has — and [`Fault::message`] turns kind + vendor
//! into the one canonical sentence.
//!
//! # Why this is its own crate (#354)
//!
//! #352 put this module in `crates/viewmodel` and stated the rule it was
//! protecting: **data access must never point at presentation**, so no vendor
//! crate may depend on `viewmodel`. That left the mapping with nowhere to live.
//! The shell reaches a failure through `user_message()`, `user_message()` is a
//! method on the error, and the error is in the vendor crate — so Neon and
//! Sentry ended up *retyping* two of these sentences, kept honest only by an
//! integration test in the one crate that could see both sides.
//!
//! Retyping a sentence in seven crates is not a smaller problem than retyping
//! it in two. The rule was never "the words are presentation"; it is "data
//! access must not point at presentation", and a vocabulary of eight sentences
//! is not presentation — it renders no colour, reads no panel, and knows
//! nothing about a layout. So it moved down to a leaf crate with no
//! dependencies at all, which `viewmodel` and every vendor crate can point at
//! without either pointing at the other. `viewmodel::fault` re-exports it, so
//! the name #352 introduced still resolves.
//!
//! # What a stock sentence may say
//!
//! Only the vendor's name is interpolated. A transport error, a request URL, a
//! response body, a service error document and a stack trace are all *detail*,
//! and detail goes to the log — the panel gets the sentence. Sentences are
//! short enough ([`MAX_MESSAGE_CHARS`]) to sit in a Half-width card's header
//! without ellipsising.
//!
//! A vendor name may carry its article — `"the agent"`, `"the status page"`,
//! `"that host"` — so every template has to read correctly with one. That is
//! why [`Fault::Undecodable`] is possessive (`couldn't read {vendor}'s
//! response`) rather than `the {vendor} response`, which said "the the agent
//! response" the first time a non-proper noun reached it.
//!
//! # Two vendor categories, and the one pairing that is not valid
//!
//! The slot is an untyped `&str` filled from two grammatical categories:
//! **proper nouns** — `"Neon"`, `"Sentry"`, `"Vercel"`, `"GitHub"`,
//! `"Azure CLI"`, `"Azure Storage"` — and **names that carry their own
//! determiner** — `"the agent"`, `"the status page"`, `"that host"`. Eight of
//! the nine templates read correctly against both. [`Fault::ToolUnavailable`]
//! does not: it tells the operator to install what it names, and *"that host
//! not found — install it and sign in"* is broken English about a machine
//! nobody can install.
//!
//! Typing the vendor is deliberately **not** the fix (#364). Nine sentences do
//! not justify a vendor-category type, and the slot stays `&str`. What the
//! constraint needed was to be *said*: the crate's own sweeps iterated the
//! whole (state × vendor) cross product, so the one pairing production never
//! produces was implicitly blessed — which is what made the broken sentence
//! look tested. `is_rendered` in the tests below is the table those sweeps now
//! iterate, and the pairing it excludes is asserted excluded, printing the
//! sentence a widening would ship.
//!
//! No test here can see a caller in another crate; only a type could, and a
//! type is not warranted at nine sentences. This states the rule and stops
//! this crate from endorsing its violation.
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
    /// signed in. The `az` case, which is the only one rendered today.
    ///
    /// **The one state whose template needs a proper noun.** Its sentence
    /// tells the operator to go and install what it names, so the vendor has
    /// to be something installable — see the module note on the two vendor
    /// categories. Rendered against `"that host"` it reads "that host not
    /// found — install it and sign in".
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
/// and are clamped by whoever quotes them. A caller appending the one thing
/// only it knows — a reset instant, the Settings tab to open — is likewise
/// past this bound on purpose.
pub const MAX_MESSAGE_CHARS: usize = 64;

impl Fault {
    /// Classify an HTTP status **for a read that is scoped to an account**.
    ///
    /// Private, and private is the point (#364). The account reading is true
    /// of the org-scoped consumption read this was written for and false
    /// almost everywhere else in this app: 404 lands in [`Fault::NotFound`] —
    /// "no such account — check Settings" — which is a mistyped URL for a
    /// public status page, version skew for a host agent, a repo the PAT
    /// cannot see for GitHub, and a renamed container for blob storage.
    /// Public, it was the most misusable constructor in the crate, and it had
    /// no caller anywhere but [`http_status_message`] — the safe wrapper
    /// sitting directly beside it.
    ///
    /// It keeps the account-scoped *name* while it is private so that when a
    /// read the account reading actually fits arrives — #283's vendor accounts
    /// — it comes back out already saying what it means, rather than being
    /// rediscovered as the shortest name in the crate.
    ///
    /// A status this vocabulary does not recognise is [`Fault::Unexpected`] —
    /// **never** folded into the nearest named state. A 400 is not a
    /// credential problem and a 418 is not an outage; reporting either as one
    /// would read as a diagnosis and be a guess.
    #[must_use]
    const fn from_account_scoped_status(status: u16) -> Self {
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
    ///
    /// A name may carry its own determiner (`"the agent"`), and every template
    /// but [`Fault::ToolUnavailable`]'s reads correctly with one. That
    /// exception is a constraint on callers, stated on the variant and pinned
    /// by the tests below.
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
            Fault::Undecodable => format!("couldn't read {vendor}'s response"),
            Fault::ToolUnavailable => format!("{vendor} not found — install it and sign in"),
            Fault::Unexpected => format!("{vendor} failed — details in the log"),
        }
    }
}

/// The sentence for an HTTP status the caller has no sharper reading of.
///
/// Only two of the classes `Fault::from_account_scoped_status` recognises mean
/// the same thing wherever they arrive from, and those render through the
/// vocabulary: a 429 is throttling and a 5xx is the far side failing, for a
/// host agent and a blob container and a public status page alike. The 5xx
/// sentence keeps the code beside it, because "is failing on its side" is a
/// kind and a 502 and a 500 are still two different things to go and read
/// about.
///
/// **Everything else names the status**, deliberately. That classifier reads
/// 401/403 as "update the credential" and 404 as "no such account", which is
/// true of the org-scoped consumption read it was written for and false almost
/// everywhere else in this app: a 404 from a host agent is a missing endpoint
/// (version skew), from GitHub a repo the PAT cannot see, from a status page a
/// mistyped URL, and from blob storage a container that was renamed. Reporting
/// any of those as "no such account — check Settings" sends the operator to
/// edit a field that was never wrong. Which is why the classifier is **private
/// to this module** and this wrapper is the crate's only public way in from a
/// bare status (#364).
///
/// A caller for which the account reading *is* right says so in its own arm
/// before it gets here — `crates/usage`'s Neon 404 ("check the org ID") and
/// every crate's auth variant already do, and each of those is sharper than
/// the stock sentence anyway.
#[must_use]
pub fn http_status_message(status: u16, vendor: &str) -> String {
    match Fault::from_account_scoped_status(status) {
        Fault::RateLimited => Fault::RateLimited.message(vendor),
        Fault::VendorFailure => {
            format!("{} (HTTP {status})", Fault::VendorFailure.message(vendor))
        }
        _ => format!("{vendor} returned HTTP {status}"),
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

    /// The vendor names this app renders that are **proper nouns** — a name
    /// the operator could type at a shell or read off a Settings field.
    /// Longest included: the bound is meaningless against a name nobody uses.
    const PROPER_NOUN_VENDORS: &[&str] = &[
        "Neon",
        "Sentry",
        "Vercel",
        "GitHub",
        "Azure CLI",
        "Azure Storage",
    ];

    /// …and the ones that **carry their own determiner**. These three arrived
    /// with #354 and are the reason `Undecodable` is possessive.
    const ARTICLE_CARRYING_VENDORS: &[&str] = &["the agent", "the status page", "that host"];

    /// Every vendor name this app renders, both categories.
    fn all_vendors() -> impl Iterator<Item = &'static str> {
        PROPER_NOUN_VENDORS
            .iter()
            .chain(ARTICLE_CARRYING_VENDORS.iter())
            .copied()
    }

    /// Whether production can put this (state, vendor) pairing on a panel.
    ///
    /// The sweeps below iterate this rather than the whole cross product. A
    /// sweep of every pairing is what made `ToolUnavailable`'s incompatibility
    /// with an article-carrying name look tested: it asserted the three
    /// properties that hold perfectly well for "that host not found — install
    /// it and sign in" — it names the vendor, it fits the header, it carries no
    /// URL — and none that would notice the sentence is nonsense.
    fn is_rendered(fault: Fault, vendor: &str) -> bool {
        match fault {
            // The one template that needs a proper noun: it tells the operator
            // to go and install what it names, and a host is not installable.
            Fault::ToolUnavailable => PROPER_NOUN_VENDORS.contains(&vendor),
            // Everything else is a thing that can happen to anything this app
            // reads, through a template valid against both categories.
            _ => true,
        }
    }

    /// Every pairing `is_rendered` admits.
    fn rendered_pairings() -> impl Iterator<Item = (Fault, &'static str)> {
        ALL.iter()
            .copied()
            .flat_map(|fault| all_vendors().map(move |vendor| (fault, vendor)))
            .filter(|(fault, vendor)| is_rendered(*fault, vendor))
    }

    /// The bug in #352: a request URL, percent-encoded parameters and a query
    /// string reached three panel headers. Asserted per state rather than by
    /// reading the source, because the leak arrived through interpolation that
    /// looked innocent at the construction site.
    #[test]
    fn no_stock_sentence_can_carry_a_url() {
        for (fault, vendor) in rendered_pairings() {
            let message = fault.message(vendor);
            for forbidden in ["://", "http", "?", "%", "&", "="] {
                assert!(
                    !message.contains(forbidden),
                    "{fault:?} for {vendor} carries {forbidden:?}: {message}"
                );
            }
        }
    }

    /// A warning that does not say *who* failed sends the operator looking
    /// through five panels for the one that is red.
    #[test]
    fn every_stock_sentence_names_the_vendor() {
        for (fault, vendor) in rendered_pairings() {
            let message = fault.message(vendor);
            assert!(
                message.contains(vendor),
                "{fault:?} does not name {vendor}: {message}"
            );
        }
    }

    /// Six wrapped lines cut off mid-token is what this vocabulary replaces.
    #[test]
    fn every_stock_sentence_fits_a_panel_header() {
        for (fault, vendor) in rendered_pairings() {
            let message = fault.message(vendor);
            let width = message.chars().count();
            assert!(
                width <= MAX_MESSAGE_CHARS,
                "{fault:?} for {vendor} is {width} chars: {message}"
            );
        }
    }

    /// A vendor name may carry its article, so no template may supply a second
    /// one. "couldn't read the the agent response" is what this caught.
    #[test]
    fn no_template_doubles_an_article_on_a_vendor_that_carries_one() {
        for (fault, vendor) in rendered_pairings().filter(|(_, v)| v.starts_with("the ")) {
            let message = fault.message(vendor);
            assert!(
                !message.contains("the the"),
                "{fault:?} for {vendor}: {message}"
            );
        }
    }

    /// The one pairing the sweeps above exclude, asserted directly so the
    /// exclusion is a stated rule rather than a gap somebody has to notice.
    ///
    /// The failure prints the sentence a widening would ship, because that
    /// sentence *is* the argument: `ToolUnavailable`'s template supplies the
    /// grammar of a thing you can go and install, and none of the three
    /// determiner-carrying names is one. The pairing production does render —
    /// `AZURE_CLI`, from `app/src-tauri/src/azure/sas.rs` — is pinned here
    /// byte-for-byte alongside it.
    #[test]
    fn tool_unavailable_is_rendered_only_against_a_tool_that_can_be_named() {
        for vendor in ARTICLE_CARRYING_VENDORS {
            assert!(
                !is_rendered(Fault::ToolUnavailable, vendor),
                "would ship {:?}",
                Fault::ToolUnavailable.message(vendor)
            );
        }
        for vendor in PROPER_NOUN_VENDORS {
            assert!(
                is_rendered(Fault::ToolUnavailable, vendor),
                "{vendor} is installable and nameable"
            );
        }
        assert_eq!(
            Fault::ToolUnavailable.message("Azure CLI"),
            "Azure CLI not found — install it and sign in"
        );
    }

    /// …and it is the *only* exclusion. Narrowing any other state here would
    /// quietly stop sweeping a pairing that does reach a panel, which is the
    /// same defect as blessing one that cannot — in the other direction, and
    /// harder to see because the sweeps would still be green.
    #[test]
    fn every_other_state_is_swept_against_both_vendor_categories() {
        for fault in ALL.iter().filter(|f| **f != Fault::ToolUnavailable) {
            for vendor in all_vendors() {
                assert!(is_rendered(*fault, vendor), "{fault:?} for {vendor}");
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
        assert_eq!(
            Fault::from_account_scoped_status(401),
            Fault::CredentialRejected
        );
        assert_eq!(
            Fault::from_account_scoped_status(403),
            Fault::CredentialRejected
        );
        assert_eq!(Fault::from_account_scoped_status(404), Fault::NotFound);
        assert_eq!(Fault::from_account_scoped_status(429), Fault::RateLimited);
        assert_eq!(Fault::from_account_scoped_status(500), Fault::VendorFailure);
        assert_eq!(Fault::from_account_scoped_status(503), Fault::VendorFailure);
        assert_eq!(Fault::from_account_scoped_status(599), Fault::VendorFailure);
    }

    /// The load-bearing one. A status nobody anticipated must land in the
    /// fallback, not in whichever named state is arithmetically nearest — a
    /// 400 reported as "update your credential" sends the operator to rotate a
    /// key that was never the problem.
    #[test]
    fn an_unrecognised_status_lands_in_the_fallback_not_the_nearest_named_state() {
        for status in [400u16, 402, 405, 418, 451, 302, 100] {
            let fault = Fault::from_account_scoped_status(status);
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

    /// The two status classes that mean the same thing wherever they come
    /// from, and the code that stays beside the 5xx one.
    #[test]
    fn throttling_and_a_far_side_failure_read_as_the_states_they_are() {
        assert_eq!(
            http_status_message(429, "GitHub"),
            Fault::RateLimited.message("GitHub")
        );
        assert_eq!(
            http_status_message(503, "the agent"),
            "the agent is failing on its side (HTTP 503)"
        );
        assert_ne!(
            http_status_message(500, "the agent"),
            http_status_message(503, "the agent"),
            "two server failures the operator would read about separately"
        );
    }

    /// The trap this helper exists for: a 404 is only "no such account" when
    /// the read was account-scoped, and for five of the six callers migrated in
    /// #354 it is a path, a repo or a mistyped URL instead. Same for a 403,
    /// which reaches here only from callers that already ruled out their own
    /// auth variant.
    #[test]
    fn an_account_shaped_reading_is_never_guessed_from_a_bare_status() {
        for (status, vendor) in [(404u16, "the agent"), (404, "GitHub"), (403, "that host")] {
            let message = http_status_message(status, vendor);
            assert_eq!(message, format!("{vendor} returned HTTP {status}"));
            assert!(!message.contains("account"), "{message}");
            assert!(!message.contains("credential"), "{message}");
        }
    }

    /// An unclassified status keeps its number: `Fault::Unexpected`'s "details
    /// in the log" is true but vaguer than the code the vendor actually sent,
    /// and a message may not get vaguer on its way through this vocabulary.
    #[test]
    fn an_unclassified_status_keeps_the_code_rather_than_shrugging() {
        assert_eq!(
            http_status_message(418, "Vercel"),
            "Vercel returned HTTP 418"
        );
        assert_ne!(
            http_status_message(418, "Vercel"),
            Fault::Unexpected.message("Vercel")
        );
    }
}
