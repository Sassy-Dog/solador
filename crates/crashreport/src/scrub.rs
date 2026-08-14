//! What a crash report is allowed to say.
//!
//! Solador's in-memory state is somebody's infrastructure: tailnet addresses,
//! remote host names, a GitHub organization, repository names, a Sentry org
//! slug, and bearer tokens read out of the OS credential store. A crash payload
//! from this app therefore carries topology a typical app's does not, so the
//! scrubber is not a courtesy pass over the message — it is the thing that
//! makes reporting shippable at all.
//!
//! Two layers, in this order.
//!
//! **Layer 1 — a structural allow-list, and the primary control.**
//! [`scrub_event`] never edits the event it is handed. It builds a *new* one
//! from `Default::default()` and copies across an explicitly enumerated set of
//! fields. Everything else is gone by construction: `server_name` (the
//! machine's hostname), `user`, `request`, `breadcrumbs`, `extra`, `tags`,
//! `contexts`, `modules`, `threads`, `debug_meta`, a frame's `vars` (local
//! variables!) and its `pre_context`/`context_line`/`post_context` (source
//! lines!). A field the SDK grows in a later version is dropped too, because it
//! was never on the list — which is the whole reason for copying rather than
//! deleting. Blocklists are wrong here for the ordinary reason: they can only
//! remove what their author already thought of.
//!
//! **Layer 2 — a word allow-list over the free text that survives.**
//! Two things cannot be allow-listed structurally, because their *content* is
//! the payload: a panic message, and the symbol/file names in a stack frame.
//! [`scrub_text`] runs over those. It redacts whole recognised shapes first
//! (URL, IPv4/IPv6 address, e-mail, absolute path, dotted host name) and then
//! holds every remaining word to a positive rule — see [`word_is_safe`] — so a
//! secret whose shape nobody anticipated is redacted for being long and mixed
//! rather than for being recognised.
//!
//! **Known limits, stated rather than papered over.** A single-label host name
//! in free text (`ubu-3xdv`) is not distinguishable from an ordinary word, and
//! neither is a short secret; both survive Layer 2 if they are short enough.
//! What Layer 1 guarantees is that the app never *volunteers* the hostname —
//! `server_name` is dropped, and the `contexts` feature that would have
//! collected it is not even compiled in (see `Cargo.toml`). Layer 2 is
//! deliberately over-eager in the other direction: `config.toml` reads as a
//! dotted host name and is redacted. Losing a filename from a panic message is
//! a cost; leaking a tailnet FQDN is a breach, and the two are not comparable.

use std::borrow::Cow;

use sentry::protocol::{Event, Exception, Frame, Mechanism, Stacktrace, Values};

/// A word, or a whole chunk, that failed the allow-list.
pub const REDACTED: &str = "[redacted]";
const REDACTED_URL: &str = "[redacted-url]";
const REDACTED_ADDRESS: &str = "[redacted-address]";
const REDACTED_HOST: &str = "[redacted-host]";
const REDACTED_PATH: &str = "[redacted-path]";
const REDACTED_EMAIL: &str = "[redacted-email]";

/// The longest a surviving word may be, whatever else it looks like.
const MAX_WORD: usize = 40;
/// Past this, a word with no `_` to break it up is treated as one opaque run —
/// which is what a token, a hash and a base64 blob all look like.
const MAX_UNBROKEN_WORD: usize = 20;
/// A word that is nothing but digits: line numbers, counts, small quantities.
const MAX_DIGIT_WORD: usize = 6;
/// A word mixing letters and digits: `utf8`, `x86_64`, `sha256`, `i32`. Short
/// on purpose — this is the clause every token would otherwise walk through.
const MAX_MIXED_WORD: usize = 6;

// MARK: the structural allow-list

/// Rebuild `event` from the fields a crash report is allowed to carry.
///
/// Returns a fresh event; the argument is consumed so no caller can
/// accidentally keep sending the unscrubbed one.
///
/// This and every helper below it write an **exhaustive struct literal** — no
/// `..event`, no `..Default::default()`. That is deliberate and it is the
/// allow-list's enforcement mechanism: when a later SDK version adds a field to
/// one of these protocol types, this file stops compiling and a human has to
/// decide whether the new field may leave the machine. A struct-update tail
/// would carry it silently, and a `Default::default()` base would hide it — in
/// both cases the omission is invisible until it is in someone's crash report.
#[must_use]
pub fn scrub_event(event: Event<'static>) -> Event<'static> {
    Event {
        // Identity and shape. None of this is operator data: `event_id` is a
        // uuid the client just minted, `platform` is the constant "native", and
        // `release`/`environment` are build strings this app chose itself.
        event_id: event.event_id,
        timestamp: event.timestamp,
        level: event.level,
        platform: event.platform,
        release: event.release,
        environment: event.environment,
        // Kept because ingest attributes the event to an SDK with it: a name, a
        // version, and a list of integration names.
        sdk: event.sdk,
        // Grouping. Nothing in this app sets a fingerprint, so in practice this
        // is the SDK's `{{ default }}` — but "in practice" is not a guarantee,
        // and it is free text, so it goes through the word allow-list too.
        fingerprint: event
            .fingerprint
            .iter()
            .map(|part| scrub_text(part).into())
            .collect::<Vec<_>>()
            .into(),

        // The two free-text carriers, each through `scrub_text`.
        message: event.message.as_deref().map(scrub_text),
        exception: Values::from(
            event
                .exception
                .values
                .into_iter()
                .map(scrub_exception)
                .collect::<Vec<_>>(),
        ),

        // Dropped, each for its own reason.
        //
        // The machine's hostname. The `contexts` SDK feature that populates it
        // is not compiled in either (see Cargo.toml), so this is belt and
        // braces — but it is the single field that would name a host outright.
        server_name: None,
        // PII by definition. `send_default_pii` is false as well.
        user: None,
        request: None,
        // An event log of whatever ran before the crash. `max_breadcrumbs` is 0
        // and `before_breadcrumb` drops them, so this is the third gate.
        breadcrumbs: Values::new(),
        // Free-form maps. Nothing writes them here — and "nothing writes them"
        // is a claim about today, whereas not copying them is a property.
        tags: Default::default(),
        extra: Default::default(),
        // Device model, OS build, and — with the `contexts` feature — hostname.
        contexts: Default::default(),
        // The loaded crate and dylib list.
        modules: Default::default(),
        // Other threads' stacks. Nothing has scrubbed those frames.
        threads: Values::new(),
        // On-disk image paths.
        debug_meta: Default::default(),
        // The top-level stacktrace: a second, unscrubbed route to the same
        // frames the exception already carries in scrubbed form.
        stacktrace: None,
        // Free text with no crash-diagnostic value that the exception does not
        // already carry.
        culprit: None,
        transaction: None,
        logentry: None,
        logger: None,
        template: None,
        dist: None,
    }
}

fn scrub_exception(exception: Exception) -> Exception {
    Exception {
        // "panic", from the SDK. Scrubbed anyway — nothing here trusts a
        // producer it did not write.
        ty: scrub_text(&exception.ty),
        // The panic message. The reason Layer 2 exists.
        value: exception.value.as_deref().map(scrub_text),
        module: exception.module.as_deref().map(scrub_text),
        stacktrace: exception.stacktrace.map(scrub_stacktrace),
        // The unprocessed twin of `stacktrace` — the same frames, none of them
        // scrubbed. Dropped rather than scrubbed: one copy is enough.
        raw_stacktrace: None,
        thread_id: None,
        mechanism: exception.mechanism.map(scrub_mechanism),
    }
}

fn scrub_mechanism(mechanism: Mechanism) -> Mechanism {
    Mechanism {
        ty: scrub_text(&mechanism.ty),
        handled: mechanism.handled,
        synthetic: mechanism.synthetic,
        description: None,
        help_link: None,
        data: Default::default(),
        // `MechanismMeta`'s three platform error blocks — `errno`, `signal`,
        // `mach_exception` — go with it. They carry addresses.
        meta: Default::default(),
    }
}

fn scrub_stacktrace(stacktrace: Stacktrace) -> Stacktrace {
    Stacktrace {
        frames: stacktrace.frames.into_iter().map(scrub_frame).collect(),
        frames_omitted: stacktrace.frames_omitted,
        // Register values are raw memory contents.
        registers: Default::default(),
    }
}

fn scrub_frame(frame: Frame) -> Frame {
    Frame {
        function: frame.function.as_deref().map(scrub_text),
        symbol: frame.symbol.as_deref().map(scrub_text),
        module: frame.module.as_deref().map(scrub_text),
        filename: frame.filename.as_deref().and_then(scrub_source_path),
        lineno: frame.lineno,
        colno: frame.colno,
        in_app: frame.in_app,
        // An absolute path: the operator's home directory, and therefore their
        // username. `filename` above carries the repo-relative tail instead.
        abs_path: None,
        // The on-disk binary.
        package: None,
        // Local variable values — the single richest leak in the whole payload.
        vars: Default::default(),
        // Source lines, verbatim, from the machine that crashed.
        pre_context: Vec::new(),
        context_line: None,
        post_context: Vec::new(),
        // This process's memory layout.
        image_addr: None,
        instruction_addr: None,
        symbol_addr: None,
        addr_mode: None,
    }
}

/// A frame's source path, reduced to the part that is about code rather than
/// about this machine.
///
/// Keeps the tail beginning at the **last** `src/` segment — `src/main.rs` out
/// of `/Users/somebody/Repos/solador/app/src-tauri/src/main.rs`, and
/// `src/lib.rs` out of a path through the cargo registry. A path with no `src/`
/// segment is dropped rather than guessed at: the crate and function are
/// already on the frame, so a missing filename costs a little context, and a
/// wrong rule here costs a home directory.
#[must_use]
pub fn scrub_source_path(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let tail = normalized
        .rmatch_indices("src/")
        .next()
        .map(|(i, _)| &normalized[i..])?;
    // The tail is still text, so it still goes through the word allow-list.
    // `src/main.rs` survives it; anything stranger does not.
    Some(scrub_text(tail))
}

// MARK: the word allow-list

/// Redact everything in `input` that is not positively recognised as safe.
///
/// See the module docs for the two-layer argument. This is Layer 2, and it is
/// the only defence for text whose *content* is the point.
#[must_use]
pub fn scrub_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        let chunk_len = rest.find(|c| !is_chunk_char(c)).unwrap_or(rest.len());
        if chunk_len == 0 {
            // A separator run: whitespace, quotes, brackets, commas. Kept
            // verbatim — separators carry no value, and preserving them is what
            // keeps a redacted message readable as a sentence.
            let sep_len = rest.find(is_chunk_char).unwrap_or(rest.len());
            out.push_str(&rest[..sep_len]);
            rest = &rest[sep_len..];
            continue;
        }
        out.push_str(&scrub_chunk(&rest[..chunk_len]));
        rest = &rest[chunk_len..];
    }
    out
}

/// Characters that hold a chunk together. Deliberately generous: the more of a
/// URL, path or token lands in one chunk, the more of it a single verdict
/// covers.
fn is_chunk_char(c: char) -> bool {
    is_word_char(c) || matches!(c, '.' | ':' | '/' | '\\' | '@' | '~')
}

/// Characters inside one word.
///
/// `-`, `%`, `+` and `=` are word characters rather than separators on
/// purpose. Treating them as separators is what would split
/// `sig=abc%2Fdef%2F...` or a base64 blob into a crowd of short, individually
/// innocent-looking fragments — every one of which passes the length rules
/// while the concatenation is a credential.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '%' | '+' | '=')
}

/// One chunk's verdict: the chunk itself, or the marker that replaces it.
///
/// Whole recognised shapes are decided first and as a unit. Deciding
/// word-by-word is what would let `100.87.202.125` through: every one of its
/// four pieces is a two-or-three digit number, and no per-word rule can call
/// that a tailnet address.
/// The chunk's **core** — what is left after stripping the separator
/// characters clinging to either end — is what gets classified. A sentence's
/// punctuation is not part of the thing it follows, and this is not cosmetic:
/// `could not reach 100.87.202.125:7878: bad token` puts a trailing `:` inside
/// the chunk, and with it there `is_ipv4` sees a fifth, non-numeric segment,
/// `is_dotted_host` sees a label with a colon in it, and the tailnet address
/// walks out through the per-word rule with every one of its pieces looking
/// like a small number. It did exactly that until this test was written.
fn scrub_chunk(chunk: &str) -> Cow<'_, str> {
    // A path is judged on the whole chunk, because the leading `/` or `~` is
    // most of what makes it one.
    if chunk.contains('\\') || chunk.starts_with('/') || chunk.starts_with('~') {
        return Cow::Borrowed(REDACTED_PATH);
    }
    let core = chunk.trim_matches(|c: char| !is_word_char(c));
    if core.is_empty() {
        // Nothing but separators — `::`, `...`. Carries nothing.
        return Cow::Borrowed(chunk);
    }
    let leading =
        &chunk[..chunk.len() - chunk.trim_start_matches(|c: char| !is_word_char(c)).len()];
    let trailing = &chunk[leading.len() + core.len()..];

    let marker = if core.contains("://") {
        REDACTED_URL
    } else if core.contains('@') {
        REDACTED_EMAIL
    } else if is_ipv4(core) || is_ipv6(core) {
        REDACTED_ADDRESS
    } else if is_dotted_host(core) {
        REDACTED_HOST
    } else if core.split(|c: char| !is_word_char(c)).all(word_is_safe) {
        // A chunk survives only if *every* word in it does. Redacting the whole
        // chunk rather than the offending word keeps `key=<secret>` from
        // shipping as `key=[redacted]`, which names the thing that was there.
        return Cow::Borrowed(chunk);
    } else {
        REDACTED
    };
    if leading.is_empty() && trailing.is_empty() {
        Cow::Borrowed(marker)
    } else {
        Cow::Owned(format!("{leading}{marker}{trailing}"))
    }
}

/// A dotted quad, with an optional `:port`.
fn is_ipv4(chunk: &str) -> bool {
    let host = chunk.split_once(':').map_or(chunk, |(host, port)| {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            host
        } else {
            chunk
        }
    });
    let mut parts = 0;
    for part in host.split('.') {
        parts += 1;
        if part.is_empty()
            || part.len() > 3
            || !part.bytes().all(|b| b.is_ascii_digit())
            || part.parse::<u16>().is_ok_and(|n| n > 255)
        {
            return false;
        }
    }
    parts == 4
}

/// A colon-separated run of hex groups.
///
/// Rust's `::` path separator is what this has to avoid firing on, and the hex
/// test is what keeps them apart: `core::option::Option` has segments full of
/// letters that are not hex digits. `dead::beef` does read as an address here
/// — a false positive in the direction that costs a word, not a subnet.
fn is_ipv6(chunk: &str) -> bool {
    if chunk.matches(':').count() < 2 {
        return false;
    }
    chunk.split(':').all(|group| {
        group.is_empty() || (group.len() <= 4 && group.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

/// A multi-label name whose last label is alphabetic: `nas.local`,
/// `ubu-3xdv.tail9c4f.ts.net`, `acme.blob.core.windows.net`.
///
/// Over-eager by design — `config.toml` and `self.field` match too. See the
/// module docs for why that trade is the right way round.
fn is_dotted_host(chunk: &str) -> bool {
    let labels: Vec<&str> = chunk.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    if !labels.iter().all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) {
        return false;
    }
    let last = labels[labels.len() - 1];
    (2..=24).contains(&last.len())
        && last.bytes().all(|b| b.is_ascii_alphabetic())
        && labels
            .iter()
            .any(|l| l.bytes().any(|b| b.is_ascii_alphabetic()))
}

/// The positive rule every surviving word must pass.
///
/// Stated as "what may leave", not "what may not": a word is kept because it
/// looks like a word, never because it failed to look like a secret. The three
/// clauses, in order — long-and-unbroken, all-digits, mixed — are what a
/// credential trips on. `ghp_…`, `github_pat_…`, `sntryu_…`, a JWT segment, a
/// hex digest and a base64 blob are all long runs mixing letters and digits,
/// and none of them fits inside six characters.
#[must_use]
pub fn word_is_safe(word: &str) -> bool {
    let len = word.chars().count();
    if len == 0 {
        return true;
    }
    if len > MAX_WORD {
        return false;
    }
    if len > MAX_UNBROKEN_WORD && !word.contains('_') {
        return false;
    }
    let digits = word.chars().filter(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return true;
    }
    if digits == len {
        return len <= MAX_DIGIT_WORD;
    }
    len <= MAX_MIXED_WORD
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::{Context, User};

    /// Invented, but shaped like the real things this app holds: a tailnet
    /// address, a tailnet FQDN, a fine-grained GitHub PAT, a Sentry user token,
    /// a Neon key, a bearer token and a home-directory path. Nothing here is a
    /// live credential and nothing here names a real machine.
    const TAILNET_ADDRESS: &str = "100.87.202.125";
    const TAILNET_PORT: &str = "100.87.202.125:7878";
    const TAILNET_FQDN: &str = "edge-7fkq.tail0d1e.ts.net";
    const GITHUB_PAT: &str = "github_pat_11ABCDEFG0aBcDeFgHiJkLmNoPqRsTuVwXyZ";
    const GITHUB_CLASSIC: &str = "ghp_1A2b3C4d5E6f7G8h9I0jKlMnOpQrStUvWx";
    const SENTRY_TOKEN: &str = "sntryu_0123456789abcdef0123456789abcdef01234567";
    const NEON_KEY: &str = "napi_9z8y7x6w5v4u3t2s1r0qponmlkjihgfedcba";
    const BEARER: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5";
    const HOME_PATH: &str = "/Users/somebody/Repos/solador/store.json";

    /// Everything that must never appear in a payload, in one list, so no
    /// assertion below can quietly stop checking one of them.
    const SECRETS: &[&str] = &[
        TAILNET_ADDRESS,
        TAILNET_FQDN,
        GITHUB_PAT,
        GITHUB_CLASSIC,
        SENTRY_TOKEN,
        NEON_KEY,
        BEARER,
        HOME_PATH,
    ];

    #[test]
    fn addresses_and_host_names_do_not_survive() {
        for raw in [TAILNET_ADDRESS, TAILNET_PORT] {
            assert_eq!(scrub_text(raw), REDACTED_ADDRESS, "{raw}");
        }
        assert_eq!(scrub_text(TAILNET_FQDN), REDACTED_HOST);
        assert_eq!(scrub_text("fe80::1"), REDACTED_ADDRESS);
        assert_eq!(scrub_text("nas.local"), REDACTED_HOST);
        assert_eq!(
            scrub_text("https://console.neon.tech/api/v2/consumption"),
            REDACTED_URL
        );
        assert_eq!(scrub_text(HOME_PATH), REDACTED_PATH);
        assert_eq!(scrub_text("~/.claude/projects"), REDACTED_PATH);
        assert_eq!(scrub_text(r"C:\Users\somebody\AppData"), REDACTED_PATH);
        assert_eq!(scrub_text("someone@example.invalid"), REDACTED_EMAIL);
    }

    #[test]
    fn token_shaped_words_do_not_survive() {
        for token in [GITHUB_PAT, GITHUB_CLASSIC, SENTRY_TOKEN, NEON_KEY] {
            assert_eq!(scrub_text(token), REDACTED, "{token}");
        }
        // A JWT is three dotted labels, so it trips the word rule rather than
        // the host rule — either way none of it may survive.
        assert!(!scrub_text(BEARER).contains("eyJhbGciOiJIUzI1NiJ9"));
    }

    #[test]
    fn a_token_takes_its_chunks_verdict_not_its_fragments() {
        // The failure this guards: splitting a chunk on `=` or `%` scatters a
        // credential into short, individually innocent-looking fragments, every
        // one of which passes the length rules while the concatenation is a key.
        let scrubbed = scrub_text(&format!("Authorization: Bearer {SENTRY_TOKEN}"));
        assert!(!scrubbed.contains("0123456789abcdef"), "{scrubbed}");
        let sas = scrub_text("sig=abc%2Fdef%2Fghi%2F0123456789abcdef0123456789");
        assert!(!sas.contains("0123456789abcdef"), "{sas}");
    }

    #[test]
    fn an_ordinary_panic_message_still_reads_as_one() {
        // Scrubbing that destroys every message is scrubbing nobody ships. The
        // panic texts this workspace actually produces have to come through
        // untouched, or the feature reports nothing worth reading.
        for message in [
            "called `Option::unwrap()` on a `None` value",
            "index out of bounds: the len is 3 but the index is 5",
            "store poisoned",
            "attempt to subtract with overflow",
        ] {
            assert_eq!(scrub_text(message), message, "{message}");
        }
    }

    #[test]
    fn rust_symbols_come_through() {
        for symbol in [
            "solador_app::main::{{closure}}",
            "<store::Store as core::fmt::Debug>::fmt",
            "std::panicking::begin_panic_handler",
            "viewmodel::cockpit::reflow",
        ] {
            assert_eq!(scrub_text(symbol), symbol, "{symbol}");
        }
    }

    #[test]
    fn a_source_path_keeps_the_code_and_drops_the_machine() {
        assert_eq!(
            scrub_source_path("/Users/somebody/Repos/solador/app/src-tauri/src/main.rs"),
            Some("src/main.rs".to_owned())
        );
        assert_eq!(
            scrub_source_path(
                "/Users/somebody/.cargo/registry/src/index.crates.io-1949cf/tokio-1.47.1/src/lib.rs"
            ),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(
            scrub_source_path(r"C:\Users\somebody\solador\crates\store\src\lib.rs"),
            Some("src/lib.rs".to_owned())
        );
        // No `src/` segment: dropped rather than guessed at.
        assert_eq!(scrub_source_path("/opt/homebrew/lib/thing.rs"), None);
    }

    #[test]
    fn the_word_rule_is_stated_as_what_may_leave() {
        for safe in [
            "unwrap", "Option", "x86_64", "utf8", "sha256", "i32", "42", "1024",
        ] {
            assert!(word_is_safe(safe), "{safe} should survive");
        }
        for unsafe_word in [
            "abcdefghijklmnopqrstuvwxyz",       // long and unbroken
            "0123456789abcdef0123456789abcdef", // a digest
            "1234567",                          // more digits than a line number
            "a1b2c3d4e5f6",                     // mixed and long
        ] {
            assert!(
                !word_is_safe(unsafe_word),
                "{unsafe_word} should not survive"
            );
        }
    }

    /// The synthetic payload the acceptance item asks for: an event carrying a
    /// tailnet address, a host name and token-shaped strings in every field a
    /// producer could put them in.
    fn synthetic_event() -> Event<'static> {
        let frame = Frame {
            function: Some("agentclient::poll".to_owned()),
            filename: Some(
                "/Users/somebody/Repos/solador/crates/agentclient/src/lib.rs".to_owned(),
            ),
            abs_path: Some(HOME_PATH.to_owned()),
            lineno: Some(88),
            context_line: Some(format!("let token = \"{BEARER}\";")),
            vars: [(
                "token".to_owned(),
                serde_json::Value::String(BEARER.to_owned()),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let exception = Exception {
            ty: "panic".to_owned(),
            value: Some(format!(
                "host {TAILNET_FQDN} at {TAILNET_ADDRESS} rejected {NEON_KEY}"
            )),
            stacktrace: Some(Stacktrace {
                frames: vec![frame],
                ..Default::default()
            }),
            ..Default::default()
        };
        Event {
            server_name: Some(TAILNET_FQDN.into()),
            message: Some(format!(
                "could not reach {TAILNET_PORT}: bad token {GITHUB_PAT}"
            )),
            user: Some(User {
                id: Some(SENTRY_TOKEN.to_owned()),
                email: Some("someone@example.invalid".to_owned()),
                username: Some("somebody".to_owned()),
                ..Default::default()
            }),
            tags: [("host".to_owned(), TAILNET_FQDN.to_owned())]
                .into_iter()
                .collect(),
            extra: [(
                "token".to_owned(),
                serde_json::Value::String(GITHUB_CLASSIC.to_owned()),
            )]
            .into_iter()
            .collect(),
            contexts: [("os".to_owned(), Context::Other(Default::default()))]
                .into_iter()
                .collect(),
            modules: [("solador".to_owned(), NEON_KEY.to_owned())]
                .into_iter()
                .collect(),
            exception: Values::from(vec![exception]),
            ..Default::default()
        }
    }

    #[test]
    fn nothing_sensitive_survives_a_synthetic_payload() {
        let scrubbed = scrub_event(synthetic_event());
        // Asserted against the **serialized** event, because what leaves this
        // process is JSON: a field the struct assertions forgot to look at is
        // still in the bytes.
        let json = serde_json::to_string(&scrubbed).expect("serialize");
        for secret in SECRETS {
            assert!(
                !json.contains(secret),
                "{secret} survived scrubbing:\n{json}"
            );
        }
        // Not only the literals: the username inside the home path is the thing
        // an operator would least expect to ship, and it appears under several
        // different keys.
        assert!(!json.contains("somebody"), "a username survived:\n{json}");
    }

    #[test]
    fn the_structural_allow_list_drops_whole_fields() {
        let scrubbed = scrub_event(synthetic_event());
        assert_eq!(scrubbed.server_name, None, "the hostname field");
        assert_eq!(scrubbed.user, None, "user PII");
        assert!(scrubbed.tags.is_empty(), "tags");
        assert!(scrubbed.extra.is_empty(), "extra");
        assert!(scrubbed.contexts.is_empty(), "contexts");
        assert!(scrubbed.modules.is_empty(), "the module list");
        assert!(scrubbed.breadcrumbs.is_empty(), "breadcrumbs");
        assert_eq!(scrubbed.stacktrace, None, "the unscrubbed top-level stack");

        let frame = &scrubbed.exception.values[0]
            .stacktrace
            .as_ref()
            .expect("stacktrace")
            .frames[0];
        assert_eq!(frame.abs_path, None, "the absolute path");
        assert_eq!(frame.context_line, None, "the source line");
        assert!(frame.vars.is_empty(), "local variables");
        // And what a frame is allowed to keep — so a change that guts the frame
        // entirely is caught too. A report with no frames is not a report.
        assert_eq!(frame.function.as_deref(), Some("agentclient::poll"));
        assert_eq!(frame.filename.as_deref(), Some("src/lib.rs"));
        assert_eq!(frame.lineno, Some(88));
    }

    #[test]
    fn scrubbing_is_stable_under_a_second_pass() {
        // The markers left behind must not themselves be redacted, or an event
        // that passed twice would decay into `[redacted]` soup.
        for raw in [TAILNET_ADDRESS, TAILNET_FQDN, HOME_PATH, GITHUB_PAT] {
            let once = scrub_text(raw);
            assert_eq!(scrub_text(&once), once, "{raw}");
        }
    }
}
