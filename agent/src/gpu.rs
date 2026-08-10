//! GPU sampling by shelling out to a vendor CLI.
//!
//! `sysinfo` reports no GPU on any platform, which is why #183 had the agent
//! omit the `gpu` object outright rather than keep sending the all-zero one
//! that predated it. This module is the first thing here that actually
//! *measures* one: NVIDIA cards, read out of `nvidia-smi` (#217).
//!
//! # Measured, or absent
//!
//! Same rule as the rest of the agent, and the reason every step below hands
//! back an `Option`: a missing binary, a spawn error, a non-zero exit, a run
//! that outlives its timeout, or output this module does not recognise all end
//! at [`Gpu::unknown`] — the `gpu` object serialises as `{}` and a consumer
//! renders an em dash. `Some(0.0)` appears only where `nvidia-smi` printed a
//! zero, which an idle RTX 3060 genuinely does.
//!
//! Failure is re-decided on every probe, so a card that disappears (driver
//! reload, GPU passed through to a VM) goes back to unknown on the next tick
//! rather than freezing at its last reading.
//!
//! # Never on the sample path
//!
//! The metrics sampler writes a snapshot every second ([`SAMPLE_INTERVAL`]),
//! and a subprocess has no business in that path: `nvidia-smi` normally answers
//! in tens of milliseconds, but a wedged driver can leave it in uninterruptible
//! sleep indefinitely, and one such call inside the loop would freeze *every*
//! metric, not just the GPU.
//!
//! So this runs as its own task ([`spawn_probe`]) on its own cadence, writing
//! the last reading into a shared cell. The sampler's contact with it is
//! [`GpuState::latest`] — a mutex lock and a clone, never held across an
//! `await`. The precedent for a slower-than-1s cadence is
//! `PROCESS_SAMPLE_TICKS`, which re-enumerates processes every ~60 ticks; the
//! difference is that this cannot merely be *slow*, it must be *decoupled*, so
//! that a probe that never returns costs a stale GPU reading and nothing else.
//!
//! [`SAMPLE_INTERVAL`]: crate::metrics
//! [`PROCESS_SAMPLE_TICKS`]: crate::metrics

use std::sync::{Arc, Mutex};
use std::time::Duration;

use wire::Gpu;

use crate::metrics::{BYTES_PER_GIB, BYTES_PER_MIB};

/// The vendor CLI this agent knows how to read. NVIDIA ships it with the
/// driver; its absence is the normal case on every other host.
const NVIDIA_SMI: &str = "nvidia-smi";

/// What [`parse_nvidia_csv`] is written against.
///
/// `noheader` drops the column titles and `nounits` drops the ` %` / ` MiB`
/// suffixes, leaving bare numbers; the field order here is the field order the
/// parser reads back. `name` is deliberately not queried — the wire's `gpu`
/// carries no model string, so asking for one would only widen the output this
/// has to recognise.
const NVIDIA_SMI_ARGS: &[&str] = &[
    "--query-gpu=utilization.gpu,memory.used,memory.total",
    "--format=csv,noheader,nounits",
];

/// How often the GPU is probed.
///
/// Deliberately slower than the 1s snapshot but far faster than the ~60s
/// process cadence: utilisation is the volatile half of this reading, and a
/// minute-old figure would be decoration rather than a metric. A dashboard
/// refreshing at 1s therefore repeats a GPU sample a handful of times, which is
/// honest — it is the newest measurement that exists.
const PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Hard cap on a single probe, comfortably above `nvidia-smi`'s tens of
/// milliseconds and well under [`PROBE_INTERVAL`] so a slow probe cannot stack
/// up behind itself.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The most recent GPU reading, shared between the probe task that writes it
/// and the metrics sampler that reads it.
#[derive(Clone)]
pub struct GpuState {
    inner: Arc<Mutex<Gpu>>,
}

impl GpuState {
    /// A state that has measured nothing yet — which is exactly what it should
    /// report until the first probe lands.
    fn unknown() -> Self {
        GpuState {
            inner: Arc::new(Mutex::new(Gpu::unknown())),
        }
    }

    /// The latest reading. A lock and a clone: no I/O, no `await`, nothing the
    /// 1s sampler can block on.
    pub fn latest(&self) -> Gpu {
        self.inner.lock().expect("gpu mutex poisoned").clone()
    }

    fn store(&self, gpu: Gpu) {
        *self.inner.lock().expect("gpu mutex poisoned") = gpu;
    }
}

/// Spawn the GPU probe task and return the handle the sampler reads.
///
/// The task loops forever: probe, store, sleep [`PROBE_INTERVAL`]. It has no
/// panic supervisor of its own because there is nothing here to panic on — the
/// probe funnels every failure into [`Gpu::unknown`] rather than unwrapping —
/// and the sampler it feeds is supervised regardless.
pub fn spawn_probe() -> GpuState {
    let state = GpuState::unknown();
    let handle = state.clone();

    tokio::spawn(async move {
        // Log only when the answer *changes*. A host with no NVIDIA card
        // probes and fails every interval forever; a line each time would be
        // noise, while a line at each transition is how an operator tells "no
        // GPU here" from "the GPU stopped answering".
        let mut was_present: Option<bool> = None;

        loop {
            let gpu = probe().await;
            let present = gpu.is_present();
            if was_present != Some(present) {
                if present {
                    tracing::info!("gpu probe measuring via {NVIDIA_SMI}");
                } else {
                    tracing::info!("no gpu measured; the snapshot's gpu stays empty");
                }
                was_present = Some(present);
            }
            handle.store(gpu);
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
    });

    state
}

/// Probe the host's GPU, vendor by vendor.
///
/// One vendor today. Another (AMD's `rocm-smi`, Intel's `xpu-smi`) slots in as
/// a second `Option`-returning arm here plus its own parser — a function
/// boundary, not a trait: nothing yet needs to be generic over vendors, and the
/// contract carries a single GPU regardless.
async fn probe() -> Gpu {
    nvidia_probe().await.unwrap_or_else(Gpu::unknown)
}

/// The NVIDIA arm: run `nvidia-smi`, read the first GPU out of its output.
async fn nvidia_probe() -> Option<Gpu> {
    let stdout = capped_output(NVIDIA_SMI, NVIDIA_SMI_ARGS, PROBE_TIMEOUT).await?;
    parse_nvidia_csv(&stdout)
}

/// Run a vendor CLI and return its stdout, or `None` for every way it can fail
/// to produce one: binary missing, spawn error, non-zero exit, or a run that
/// outlives `timeout`.
///
/// `kill_on_drop` is what makes the timeout mean something. `tokio::time::
/// timeout` only stops *awaiting*; dropping the future drops the `Child` with
/// it, and the flag turns that drop into a kill. Without it a wedged
/// `nvidia-smi` would be abandoned and left running, and the next tick would
/// start another.
///
/// A missing binary is silent — that is every host without an NVIDIA driver,
/// which is most of them, and matches how `containers.rs` treats a runtime that
/// isn't installed. Every other failure is logged with the binary name, status
/// and trimmed stderr; nothing here touches the bearer token or any other
/// credential.
async fn capped_output(bin: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut command = tokio::process::Command::new(bin);
    command.args(args).kill_on_drop(true);

    let output = match tokio::time::timeout(timeout, command.output()).await {
        Err(_elapsed) => {
            tracing::warn!(
                bin,
                timeout_ms = timeout.as_millis() as u64,
                "gpu probe timed out; killed"
            );
            return None;
        }
        Ok(Err(e)) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(bin, error = %e, "failed to run gpu probe");
            }
            return None;
        }
        Ok(Ok(output)) => output,
    };

    if !output.status.success() {
        tracing::warn!(
            bin,
            status = %output.status,
            stderr = String::from_utf8_lossy(&output.stderr).trim(),
            "gpu probe exited non-zero"
        );
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Read `nvidia-smi --query-gpu=… --format=csv,noheader,nounits` output into a
/// wire [`Gpu`].
///
/// One line per GPU, three bare numbers each — utilisation %, used MiB, total
/// MiB:
///
/// ```text
/// 0, 0, 12288
/// 74, 3120, 12288
/// ```
///
/// **The first line wins.** The contract carries one `gpu` object, so a
/// multi-GPU host reports its first card rather than a sum (VRAM totals across
/// distinct cards are not a pool) or an average (a utilisation nothing
/// experienced).
///
/// `None` for anything that is not that shape — the wrong number of fields, a
/// non-number, `nvidia-smi`'s own `[N/A]` / `[Not Supported]` placeholders,
/// units left on by a build that ignored `nounits`. Reading a value out of
/// output this does not understand is how a fabricated number gets onto the
/// wire, and an em dash is the correct answer instead.
fn parse_nvidia_csv(stdout: &str) -> Option<Gpu> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;

    let mut fields = line.split(',').map(str::trim);
    let usage = numeric_field(fields.next()?)?;
    let used_mib = numeric_field(fields.next()?)?;
    let total_mib = numeric_field(fields.next()?)?;
    // A fourth column means this is not the query above, so the three columns
    // just read are not necessarily the three fields expected.
    if fields.next().is_some() {
        return None;
    }

    Some(Gpu {
        usage: Some(usage.clamp(0.0, 100.0)),
        vram_used_gb: Some(mib_to_gb(used_mib)),
        vram_total_gb: Some(mib_to_gb(total_mib)),
    })
}

/// One bare non-negative number, or `None` for anything else.
fn numeric_field(raw: &str) -> Option<f64> {
    let value = raw.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

/// MiB (what the `nounits` memory fields are) → the contract's `…GB`.
///
/// Routed through the module's existing `BYTES_PER_*` pair, so VRAM is the same
/// 1024-base "GB" that `memory.usedGB` and every volume already report — the
/// wire's `GB` has meant GiB since the first snapshot, and one field quietly
/// using 1000-base would make a 12 GiB card read 12.9.
fn mib_to_gb(mib: f64) -> f64 {
    mib * BYTES_PER_MIB / BYTES_PER_GIB
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The verified `nvidia-smi` line from ubu-01's RTX 3060 12GB, in the
    /// `nounits` form this agent queries.
    const RTX_3060_IDLE: &str = "0, 0, 12288\n";

    /// An idle card reads zeros — and a zero that was measured is a NUMBER.
    /// This is the exact case #183's doctrine turns on: the same `0` the agent
    /// used to fabricate is now legitimate, because something read it.
    #[test]
    fn the_rtx_3060_line_parses_into_measured_values() {
        let gpu = parse_nvidia_csv(RTX_3060_IDLE).expect("the verified line parses");

        assert_eq!(gpu.usage, Some(0.0));
        assert_eq!(gpu.vram_used_gb, Some(0.0));
        // 12288 MiB is 12 GiB exactly — the conversion, not a passthrough.
        assert_eq!(gpu.vram_total_gb, Some(12.0));
        assert!(gpu.is_present(), "12 GiB of VRAM is a present GPU");
    }

    /// A busy card: utilisation and used VRAM are read straight through, in
    /// GiB.
    #[test]
    fn a_busy_card_reports_its_utilisation_and_used_vram() {
        let gpu = parse_nvidia_csv("74, 3072, 12288\n").expect("parses");
        assert_eq!(gpu.usage, Some(74.0));
        assert_eq!(gpu.vram_used_gb, Some(3.0));
        assert_eq!(gpu.vram_total_gb, Some(12.0));
    }

    /// `nvidia-smi` prints one line per GPU. The contract carries one `gpu`, so
    /// the first card wins — never a sum of VRAM across cards that share no
    /// pool, never an averaged utilisation nothing experienced.
    #[test]
    fn a_multi_gpu_host_reports_the_first_card_only() {
        let two_cards = "5, 512, 12288\n97, 23000, 24576\n";
        let gpu = parse_nvidia_csv(two_cards).expect("parses");

        assert_eq!(gpu.usage, Some(5.0), "the first line's utilisation");
        assert_eq!(gpu.vram_total_gb, Some(12.0), "the first card's VRAM");
        // Not the sum (36 GiB), not the mean (18 GiB), not the second card.
        assert_ne!(gpu.vram_total_gb, Some(36.0));
    }

    /// Output this parser does not recognise yields NOTHING. Every case here
    /// has a tempting partial reading in it (a leading number, two of three
    /// fields, a value with its unit still attached) and taking any of them
    /// would put a figure on the wire that `nvidia-smi` did not report.
    #[test]
    fn unrecognised_output_is_unknown_rather_than_a_guess() {
        for body in [
            "",
            "\n  \n",
            "garbage",
            "0, 0",                                           // a field short
            "0, 0, 12288, 42",                                // a field long
            "0 %, 0 MiB, 12288 MiB",                          // `nounits` ignored
            "[N/A], 0, 12288",                                // utilisation unsupported
            "0, [Not Supported], 12288",                      // memory unsupported
            "-1, 0, 12288",                                   // a negative reading
            "NaN, 0, 12288",                                  // a non-finite reading
            "NVIDIA GeForce RTX 3060, 0 %, 0 MiB, 12288 MiB", // the with-name query
        ] {
            assert_eq!(
                parse_nvidia_csv(body),
                None,
                "unrecognised nvidia-smi output must read unknown: {body:?}"
            );
        }
    }

    /// Utilisation is a percentage on the wire, so an out-of-range figure is
    /// clamped rather than passed through — same rule the PSI pressure read
    /// applies.
    #[test]
    fn utilisation_is_clamped_to_the_contract_range() {
        let gpu = parse_nvidia_csv("142, 0, 12288").expect("parses");
        assert_eq!(gpu.usage, Some(100.0));
    }

    /// THE DECISION PATH, not the parser: whatever [`capped_output`] failed to
    /// produce, the probe reports a GPU it never measured as absent — which
    /// serialises to the `{}` this agent has emitted since #183.
    #[tokio::test]
    async fn a_host_with_no_nvidia_smi_measures_no_gpu() {
        // The full probe, on this machine. CI and every developer Mac run it
        // without an NVIDIA driver, so this exercises the real
        // spawn → NotFound → unknown path end to end.
        let gpu = probe().await;

        assert!(!gpu.is_present(), "no nvidia-smi here, so no GPU: {gpu:?}");
        assert_eq!(
            serde_json::to_value(&gpu).unwrap(),
            json!({}),
            "an unmeasured GPU must omit every key, never send zeros"
        );
    }

    /// A binary that is not on `PATH` is the ordinary case (every host without
    /// an NVIDIA driver) and produces no output to read.
    #[tokio::test]
    async fn a_missing_binary_produces_no_output() {
        let got = capped_output(
            "solador-no-such-binary-217",
            &["--version"],
            PROBE_TIMEOUT,
        )
        .await;
        assert_eq!(got, None);
    }

    /// A non-zero exit is a failed reading even though the process ran and may
    /// well have printed something to stdout first.
    #[tokio::test]
    async fn a_non_zero_exit_discards_the_output() {
        let got = capped_output("sh", &["-c", "echo '0, 0, 12288'; exit 1"], PROBE_TIMEOUT).await;
        assert_eq!(
            got, None,
            "a command that failed has not measured anything, whatever it printed"
        );
    }

    /// The cap is real: a probe that hangs is abandoned rather than awaited
    /// forever. `timeout` is a parameter precisely so this runs in
    /// milliseconds instead of the production seconds.
    #[tokio::test]
    async fn a_hung_probe_is_capped_rather_than_awaited() {
        let started = std::time::Instant::now();
        let got = capped_output(
            "sh",
            &["-c", "sleep 30; echo '0, 0, 12288'"],
            Duration::from_millis(150),
        )
        .await;

        assert_eq!(got, None, "a probe that never answered measured nothing");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the cap must end the wait, took {:?}",
            started.elapsed()
        );
    }

    /// The success half of the same path: stdout from a command that exited
    /// cleanly reaches the parser intact.
    #[tokio::test]
    async fn a_clean_exit_hands_its_stdout_back() {
        let got = capped_output("sh", &["-c", "echo '0, 0, 12288'"], PROBE_TIMEOUT).await;
        let gpu = parse_nvidia_csv(&got.expect("stdout captured")).expect("parses");
        assert_eq!(gpu.vram_total_gb, Some(12.0));
    }

    /// The handle the sampler holds starts out measuring nothing, and reports
    /// whatever the probe task last stored.
    #[test]
    fn the_shared_state_starts_unknown_and_carries_what_the_probe_stored() {
        let state = GpuState::unknown();
        assert!(
            !state.latest().is_present(),
            "before the first probe there is nothing to report"
        );

        state.store(parse_nvidia_csv(RTX_3060_IDLE).unwrap());
        assert_eq!(state.latest().vram_total_gb, Some(12.0));

        // …and a card that stops answering goes back to unknown rather than
        // freezing at its last reading.
        state.store(Gpu::unknown());
        assert!(!state.latest().is_present());
    }
}
