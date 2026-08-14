//! The bytes that leave this process.
//!
//! `sentry`'s own transport is not compiled in. It wants reqwest **0.13**, a
//! second HTTP stack beside the 0.12 the rest of this workspace resolves, and
//! its `rustls` feature drags in the `aws-lc-sys` provider — a cmake and C
//! build — for a feature that sends a few hundred bytes on the rare day
//! something panics. So this module supplies a transport over the reqwest the
//! workspace already has.
//!
//! Owning it buys a second thing, which matters more than the dependency count:
//! this is the last gate before the network, and it is code in this repo. It
//! forwards **event items only**. Sessions, attachments, transactions, client
//! reports and profiles are dropped — not because any of them is produced here
//! (none is; the features that produce them are off), but because "we don't
//! produce those" is a statement about today and dropping them is a property of
//! the code.
//!
//! The queue is bounded and never blocks its caller. A panic hook runs on a
//! thread that is already unwinding, and the one thing it must not do is wait.

use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sentry::protocol::EnvelopeItem;
use sentry::{Envelope, Transport, TransportFactory, TransportOptions};

/// How many envelopes may be waiting. A crash reporter that has fallen this far
/// behind has something worse wrong with it than a dropped report.
const QUEUE_DEPTH: usize = 8;
/// Ceiling on one request. The panic hook flushes synchronously, so this is
/// also the longest a crashing process can be held up by the reporter.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds [`HttpTransport`] when the client asks for one.
pub(crate) struct HttpTransportFactory;

impl TransportFactory for HttpTransportFactory {
    fn create_transport_with_options(&self, options: TransportOptions) -> Arc<dyn Transport> {
        Arc::new(HttpTransport::start(&options))
    }
}

/// A worker thread, a bounded queue, and a counter the flush waits on.
pub(crate) struct HttpTransport {
    sender: SyncSender<Vec<u8>>,
    queued: Arc<Queued>,
}

/// The in-flight count, plus the condvar `flush` sleeps on.
#[derive(Default)]
struct Queued {
    depth: Mutex<usize>,
    drained: Condvar,
}

impl HttpTransport {
    fn start(options: &TransportOptions) -> Self {
        let (sender, receiver) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);
        let queued = Arc::new(Queued::default());

        let url = options.dsn.envelope_api_url().to_string();
        let auth = options.dsn.to_auth(Some(&options.user_agent)).to_string();
        let user_agent = options.user_agent.to_string();
        let http_proxy = options.http_proxy.as_deref().map(str::to_owned);
        let https_proxy = options.https_proxy.as_deref().map(str::to_owned);

        let worker_queued = Arc::clone(&queued);
        // A plain OS thread, not a tokio task. The panic hook can fire at any
        // point in the process's life — including while the runtime is shutting
        // down — and a reporter that needs an executor to still be alive is a
        // reporter that goes quiet exactly when it is wanted.
        let spawned = std::thread::Builder::new()
            .name("solador-crashreport".to_owned())
            .spawn(move || {
                let client = build_client(http_proxy.as_deref(), https_proxy.as_deref());
                for body in receiver {
                    if let Some(client) = client.as_ref() {
                        // Failure is silent by design: there is no one to tell.
                        // A crash reporter that reports its own network trouble
                        // to the operator is noise on the worst possible day.
                        let _ = client
                            .post(&url)
                            .header("X-Sentry-Auth", &auth)
                            .header("User-Agent", &user_agent)
                            .header("Content-Type", "application/x-sentry-envelope")
                            .body(body)
                            .send();
                    }
                    worker_queued.finished();
                }
            });
        if spawned.is_err() {
            // No worker means nothing drains the queue. Say so once; the
            // bounded channel then simply fills and every send is dropped,
            // which is the failure mode we want rather than an unbounded leak.
            eprintln!("crash reporting: could not start the reporter thread; nothing will be sent");
        }

        HttpTransport { sender, queued }
    }
}

fn build_client(
    http_proxy: Option<&str>,
    https_proxy: Option<&str>,
) -> Option<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder().timeout(REQUEST_TIMEOUT);
    for proxy in [https_proxy, http_proxy].into_iter().flatten() {
        match reqwest::Proxy::all(proxy) {
            Ok(proxy) => builder = builder.proxy(proxy),
            // Fail closed: a proxy the operator configured and we could not
            // parse means we do not know where the request would go.
            Err(_) => return None,
        }
    }
    builder.build().ok()
}

impl Queued {
    fn started(&self) {
        *self.depth.lock().expect("crash-report queue poisoned") += 1;
    }

    fn finished(&self) {
        let mut depth = self.depth.lock().expect("crash-report queue poisoned");
        *depth = depth.saturating_sub(1);
        if *depth == 0 {
            self.drained.notify_all();
        }
    }
}

impl Transport for HttpTransport {
    fn send_envelope(&self, envelope: Envelope) {
        // The last filter before the wire. Anything that is not an event item
        // is dropped here, whether or not this build can produce one.
        let Some(envelope) =
            envelope.filter(|item: &EnvelopeItem| matches!(item, EnvelopeItem::Event(_)))
        else {
            return;
        };
        let mut body = Vec::new();
        if envelope.to_writer(&mut body).is_err() {
            // An envelope we could not serialise is an envelope we cannot
            // inspect, so it is not one we may send.
            return;
        }
        self.queued.started();
        match self.sender.try_send(body) {
            Ok(()) => {}
            // Full or disconnected: drop it, and undo the count so a later
            // flush is not left waiting on an envelope nobody holds.
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => self.queued.finished(),
        }
    }

    fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut depth = self.depth_lock();
        while *depth > 0 {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (guard, result) = self
                .queued
                .drained
                .wait_timeout(depth, remaining)
                .expect("crash-report queue poisoned");
            depth = guard;
            if result.timed_out() && *depth > 0 {
                return false;
            }
        }
        true
    }
}

impl HttpTransport {
    fn depth_lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.queued
            .depth
            .lock()
            .expect("crash-report queue poisoned")
    }
}
