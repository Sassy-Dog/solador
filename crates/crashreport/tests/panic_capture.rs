//! The end of the acceptance list, exercised through the real SDK: a real
//! `sentry::init`, the real panic hook it installs, the real `before_send`, and
//! the real scrubber. Only the transport is swapped, for a recorder — **no test
//! in this repo sends an event anywhere**, and this file is the reason that
//! constraint costs nothing.
//!
//! Its own integration-test binary on purpose. The panic hook is process-wide
//! and installed exactly once, so the fewer other tests share the process, the
//! less there is to reason about. Each `#[test]` still runs on its own thread,
//! and `sentry::init` binds its client to the calling thread's hub — which is
//! what keeps the three cases below from seeing each other's clients.

use std::sync::{Arc, Mutex};

use crashreport::{Config, OptIn, Reporting};
use sentry::protocol::{Envelope, EnvelopeItem, Event};
use sentry::Transport;

/// A DSN shape pointing at a reserved-by-RFC name that cannot resolve. Not a
/// destination — the recording transport never opens a socket — but `decide`
/// insists on a *parseable* DSN and this is what one looks like.
const TEST_DSN: &str = "https://0123456789abcdef0123456789abcdef@example.invalid/42";

/// Invented, and shaped like what this app actually holds.
const TAILNET: &str = "100.87.202.125:7878";
const TOKEN: &str = "ghp_1A2b3C4d5E6f7G8h9I0jKlMnOpQrStUvWx";

/// Keeps every envelope it is handed, so a test can ask what would have been
/// sent instead of whether the network happened to be up.
#[derive(Default)]
struct Recorder {
    envelopes: Mutex<Vec<Envelope>>,
}

impl Recorder {
    fn events(&self) -> Vec<Event<'static>> {
        self.envelopes
            .lock()
            .expect("recorder poisoned")
            .iter()
            .flat_map(|envelope| envelope.items())
            .filter_map(|item| match item {
                EnvelopeItem::Event(event) => Some((**event).clone()),
                _ => None,
            })
            .collect()
    }
}

impl Transport for Recorder {
    fn send_envelope(&self, envelope: Envelope) {
        self.envelopes
            .lock()
            .expect("recorder poisoned")
            .push(envelope);
    }
}

fn start_recording(opt_in: OptIn, dsn: Option<&str>) -> (Reporting, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let reporting = crashreport::start_with_transport(
        Config {
            opt_in,
            dsn,
            release: "solador-test",
            environment: "test",
        },
        // `Arc<T: Transport>` is itself a `TransportFactory` that hands back the
        // same instance, so the test keeps the very transport the client uses.
        Arc::new(Arc::clone(&recorder)),
    );
    (reporting, recorder)
}

/// Panics on *this* thread, so the hub the hook reaches is the one
/// `start_with_transport` just bound a client to.
fn force_panic(message: String) {
    let caught = std::panic::catch_unwind(|| panic!("{message}"));
    assert!(caught.is_err(), "the panic under test did not happen");
}

#[test]
fn a_forced_panic_produces_a_scrubbed_event() {
    let (reporting, recorder) = start_recording(OptIn::Granted, Some(TEST_DSN));
    assert!(reporting.is_active());

    force_panic(format!("host {TAILNET} rejected {TOKEN}"));

    let events = recorder.events();
    assert_eq!(events.len(), 1, "a panic must produce exactly one event");
    let json = serde_json::to_string(&events[0]).expect("serialize");

    // It is a panic report and it carries a stack.
    assert!(json.contains("\"type\":\"panic\""), "{json}");
    let frames = &events[0].exception.values[0]
        .stacktrace
        .as_ref()
        .expect("a panic event must carry a stacktrace")
        .frames;
    assert!(
        !frames.is_empty(),
        "a report with no frames is not a report"
    );

    // And nothing that went into the panic message came back out of it. This is
    // the whole point: the scrubber has to hold on the path the SDK actually
    // takes, not only on a hand-built event.
    assert!(
        !json.contains(TAILNET),
        "a tailnet address survived:\n{json}"
    );
    assert!(
        !json.contains("100.87.202.125"),
        "an address survived:\n{json}"
    );
    assert!(!json.contains(TOKEN), "a token survived:\n{json}");
    assert!(json.contains("[redacted-address]"), "{json}");

    // No frame may carry an absolute path, and this run has real ones to drop.
    for frame in frames {
        assert_eq!(frame.abs_path, None, "an absolute path survived");
        assert!(frame.vars.is_empty(), "local variables survived");
        if let Some(filename) = frame.filename.as_deref() {
            assert!(
                !filename.starts_with('/') && !filename.contains(":\\"),
                "an absolute filename survived: {filename}"
            );
        }
    }
    // The event's own hostname field, which the SDK fills in on some builds.
    assert_eq!(events[0].server_name, None);
}

#[test]
fn withdrawing_consent_stops_reports_in_the_same_session() {
    let (reporting, recorder) = start_recording(OptIn::Granted, Some(TEST_DSN));
    reporting.set_consent(false);
    assert!(!reporting.is_active());

    force_panic("this must not be reported".to_owned());

    assert!(
        recorder.events().is_empty(),
        "a withdrawn consent must take effect immediately, not next launch"
    );
}

#[test]
fn a_default_store_reports_nothing_even_with_a_dsn() {
    // The acceptance item, end to end rather than at the gate: default off, a
    // DSN present, a real panic — and nothing recorded, because no client was
    // ever created to record through.
    let (reporting, recorder) = start_recording(OptIn::Declined, Some(TEST_DSN));
    assert!(!reporting.is_active());

    force_panic(format!("host {TAILNET} rejected {TOKEN}"));

    assert!(
        recorder.events().is_empty(),
        "a fresh store must report nothing"
    );
}

#[test]
fn opting_in_with_no_dsn_sends_nothing_and_does_not_error() {
    let (reporting, recorder) = start_recording(OptIn::Granted, None);
    assert!(!reporting.is_active());

    force_panic("this build has nowhere to report to".to_owned());

    assert!(recorder.events().is_empty());
}
