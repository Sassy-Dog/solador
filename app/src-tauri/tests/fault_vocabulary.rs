//! The vendor crates' stock sentences are `fault`'s — asserted here because
//! here is where every one of them is visible at once.
//!
//! #352 folded in a decision: anticipated failures get stock messages and one
//! module owns the words, with **no vendor crate taking a `viewmodel`
//! dependency** — data access must not point at presentation. The words lived
//! in `crates/viewmodel` and the shell reaches a failure through
//! `user_message()`, which is a method on the error, which is in the vendor
//! crate. So Neon and Sentry *retyped* two sentences each into their
//! `#[error(…)]` attributes, and this file existed to keep the copies equal:
//! `app/src-tauri` was the one crate that could see both sides.
//!
//! **#354 removed the second copy rather than policing it.** `Fault` moved down
//! to `crates/fault`, a leaf crate with no dependencies that every vendor crate
//! and `viewmodel` can point at without either pointing at the other, so these
//! `user_message()` bodies now *call* the vocabulary. The assertions below are
//! trivially true today, exactly as this file predicted they would become, and
//! they are kept because trivially-true is a property of the current
//! implementation, not of the type system: an arm that goes back to composing
//! its own wording for a named state is still a red build here.
//!
//! The URL guard at the bottom is not trivial and never was. It is the #352 bug
//! itself, checked at the boundary a panel actually reads.
//!
//! `app/src-tauri/src/azure/sas.rs` needs nothing here: it renders straight out
//! of the vocabulary already, and asserts that in its own tests.

use agentclient::AgentError;
use github::client::GitHubError;
use servicestatus::{ProbeError, StatusError};
use usage::neon::NeonUsageError;
use usage::sentry::SentryUsageError;
use usage::vercel::VercelUsageError;
use viewmodel::fault::Fault;

/// A payload shaped like the thing that leaked. Whatever these variants carry
/// must not reach the sentence, so the tests below hand them the worst case:
/// the very URL from the screenshot in #352.
const LEAKY_PAYLOAD: &str = "error sending request for url \
    (https://console.neon.tech/api/v2/consumption_history/v2/projects\
?from=2026-08-01T00%3A00%3A00Z&org_id=org-cool-unit-22571507)";

#[test]
fn neons_stock_sentences_are_the_vocabularys() {
    assert_eq!(
        NeonUsageError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Unreachable.message("Neon")
    );
    assert_eq!(
        NeonUsageError::DecodingFailed(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Undecodable.message("Neon")
    );
}

#[test]
fn sentrys_stock_sentences_are_the_vocabularys() {
    assert_eq!(
        SentryUsageError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Unreachable.message("Sentry")
    );
    assert_eq!(
        SentryUsageError::DecodingFailed(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Undecodable.message("Sentry")
    );
}

/// The four types #354 migrated whose transport arms take the stock sentence
/// with nothing appended. Vercel's decode arm is the one that changed wording:
/// it said "couldn't read the Vercel charges", which named the payload rather
/// than the state, and no other panel described its own decode failure that
/// way.
#[test]
fn the_migrated_transport_arms_are_the_vocabularys() {
    assert_eq!(
        VercelUsageError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Unreachable.message("Vercel")
    );
    assert_eq!(
        VercelUsageError::DecodeFailed(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Undecodable.message("Vercel")
    );
    assert_eq!(
        VercelUsageError::MissingToken.user_message(),
        Fault::NotConfigured.message("Vercel")
    );
    assert_eq!(
        StatusError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Unreachable.message("the status page")
    );
    assert_eq!(
        StatusError::DecodeFailed(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Undecodable.message("the status page")
    );
    assert_eq!(
        ProbeError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
        Fault::Unreachable.message("that host")
    );
    assert_eq!(
        AgentError::AuthFailed.user_message(),
        Fault::CredentialRejected.message("the agent")
    );
}

/// The arms that say more than the stock sentence still *start* with it. This
/// is the acceptance bar #354 was held to from the other direction: a stock
/// sentence is the floor a message may not fall below, never a cap on how
/// specific one may be, so each of these keeps the one thing only its own crate
/// knows — the Settings tab holding the PAT, the network to check, the version
/// skew a redeploy causes, the scope a Vercel token needs.
#[test]
fn an_arm_that_knows_more_says_the_stock_sentence_first_and_then_the_extra() {
    for (message, stem, extra) in [
        (
            GitHubError::NotAuthenticated.user_message(),
            Fault::CredentialRejected.message("GitHub"),
            "GitHub Token",
        ),
        (
            GitHubError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
            Fault::Unreachable.message("GitHub"),
            "network connection",
        ),
        (
            GitHubError::DecodeFailed(LEAKY_PAYLOAD.to_owned()).user_message(),
            Fault::Undecodable.message("GitHub"),
            "API contract change",
        ),
        (
            AgentError::Unreachable(LEAKY_PAYLOAD.to_owned()).user_message(),
            Fault::Unreachable.message("the agent"),
            "the agent is running",
        ),
        (
            AgentError::DecodeFailed(LEAKY_PAYLOAD.to_owned()).user_message(),
            Fault::Undecodable.message("the agent"),
            "version skew",
        ),
        (
            VercelUsageError::Unauthorized.user_message(),
            Fault::CredentialRejected.message("Vercel"),
            "scope",
        ),
    ] {
        assert!(
            message.starts_with(&stem),
            "{message:?} does not open {stem:?}"
        );
        assert!(
            message.contains(extra),
            "{message:?} dropped {extra:?} in the migration"
        );
    }
}

/// The bug itself, at the boundary the panel actually reads. `user_message()`
/// is what reaches a footer, and `to_string()` is what a careless caller would
/// reach for instead — both are checked, because the leak arrived through the
/// second one falling through to the first.
///
/// Every payload-carrying variant across the migrated crates is in here now,
/// not just Neon's and Sentry's: each one is handed a `reqwest`-shaped string
/// with a URL in it, which is exactly what its construction site produces when
/// somebody forgets `without_url`.
#[test]
fn no_payload_carrying_variant_can_put_a_url_in_a_panel() {
    let payload = LEAKY_PAYLOAD.to_owned();
    let rendered: Vec<String> = [
        NeonUsageError::Unreachable(payload.clone()),
        NeonUsageError::DecodingFailed(payload.clone()),
    ]
    .iter()
    .flat_map(|e| [e.user_message(), e.to_string()])
    .chain(
        [
            SentryUsageError::Unreachable(payload.clone()),
            SentryUsageError::DecodingFailed(payload.clone()),
        ]
        .iter()
        .flat_map(|e| [e.user_message(), e.to_string()]),
    )
    .chain(
        [
            VercelUsageError::Unreachable(payload.clone()),
            VercelUsageError::DecodeFailed(payload.clone()),
        ]
        .iter()
        .map(VercelUsageError::user_message),
    )
    .chain(
        [
            GitHubError::Unreachable(payload.clone()),
            GitHubError::DecodeFailed(payload.clone()),
        ]
        .iter()
        .map(GitHubError::user_message),
    )
    .chain(
        [
            AgentError::Unreachable(payload.clone()),
            AgentError::DecodeFailed(payload.clone()),
        ]
        .iter()
        .map(AgentError::user_message),
    )
    .chain(
        [
            StatusError::Unreachable(payload.clone()),
            StatusError::DecodeFailed(payload.clone()),
        ]
        .iter()
        .map(StatusError::user_message),
    )
    .chain(std::iter::once(
        ProbeError::Unreachable(payload).user_message(),
    ))
    .collect();

    for message in &rendered {
        for forbidden in ["://", "http", "?", "%", "&", "org_id"] {
            assert!(
                !message.contains(forbidden),
                "{message:?} carries {forbidden:?}"
            );
        }
    }
}

/// The stock half of every panel message fits a Half-width card. Only the stock
/// half: an arm that appends the Settings tab or a reset instant is past the
/// bound on purpose, and #351 is what makes that survivable at the rendering
/// layer.
#[test]
fn every_vendor_this_app_names_fits_the_bound() {
    for vendor in [
        "Neon",
        "Sentry",
        "Vercel",
        "GitHub",
        "Azure CLI",
        "Azure Storage",
        "the agent",
        "the status page",
        "that host",
    ] {
        for fault in [
            Fault::NotConfigured,
            Fault::Unreachable,
            Fault::CredentialRejected,
            Fault::NotFound,
            Fault::RateLimited,
            Fault::VendorFailure,
            Fault::Undecodable,
            Fault::ToolUnavailable,
            Fault::Unexpected,
        ] {
            let message = fault.message(vendor);
            assert!(
                message.chars().count() <= viewmodel::fault::MAX_MESSAGE_CHARS,
                "{message:?} is {} chars",
                message.chars().count()
            );
        }
    }
}
