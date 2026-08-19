//! The vendor crates' stock sentences are `viewmodel::fault`'s, character for
//! character — asserted here because here is the only place that can see both.
//!
//! #352 folded in a decision: anticipated failures get stock messages and
//! `crates/viewmodel` owns the words, with **no vendor crate taking a
//! `viewmodel` dependency** — data access must not point at presentation. That
//! leaves `crates/usage`'s Neon and Sentry clients rendering those sentences
//! themselves, because the shell reaches them through `user_message()` and the
//! mapping has nowhere else to live until #354 moves it into the panel layer.
//!
//! So the words exist twice for now, and the duplication is *enforced* rather
//! than hoped for: `app/src-tauri` is the one crate that depends on both sides,
//! so this file is where "the sentences live in `viewmodel` and nowhere else"
//! can be a test instead of a comment. When #354 lands, these assertions become
//! trivially true — and until then, changing a sentence in one place and not
//! the other is a red build rather than a drifting vocabulary.
//!
//! `app/src-tauri/src/azure/sas.rs` needs nothing here: it renders straight out
//! of `viewmodel::fault` already, and asserts that in its own tests.

use usage::neon::NeonUsageError;
use usage::sentry::SentryUsageError;
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

/// The bug itself, at the boundary the panel actually reads. `user_message()`
/// is what reaches a footer, and `to_string()` is what a careless caller would
/// reach for instead — both are checked, because the leak arrived through the
/// second one falling through to the first.
#[test]
fn no_payload_carrying_variant_can_put_a_url_in_a_panel() {
    let neon: [NeonUsageError; 2] = [
        NeonUsageError::Unreachable(LEAKY_PAYLOAD.to_owned()),
        NeonUsageError::DecodingFailed(LEAKY_PAYLOAD.to_owned()),
    ];
    let sentry: [SentryUsageError; 2] = [
        SentryUsageError::Unreachable(LEAKY_PAYLOAD.to_owned()),
        SentryUsageError::DecodingFailed(LEAKY_PAYLOAD.to_owned()),
    ];

    let rendered = neon
        .iter()
        .flat_map(|e| [e.user_message(), e.to_string()])
        .chain(
            sentry
                .iter()
                .flat_map(|e| [e.user_message(), e.to_string()]),
        )
        .collect::<Vec<_>>();

    for message in &rendered {
        for forbidden in ["://", "http", "?", "%", "&", "org_id"] {
            assert!(
                !message.contains(forbidden),
                "{message:?} carries {forbidden:?}"
            );
        }
        assert!(
            message.chars().count() <= viewmodel::fault::MAX_MESSAGE_CHARS,
            "{message:?} is {} chars",
            message.chars().count()
        );
    }
}
