# HostMetricsKit

Local Swift package for **local-machine** metric collection on macOS — CPU, memory,
GPU, battery, and disk/network sampling via IOKit/Mach — consumed by the DevCanopy
app. (Remote hosts are sampled by the Rust agent in `agent/`, not this package.)

## Ownership & sync policy

**This copy of HostMetricsKit is the sole canonical home of the metrics engine.**

The engine originated as a verbatim copy of Lupita's `PerformanceMonitor.swift`
(see the header notes in `Sources/HostMetricsKit/LupitaMetricsTypes.swift` and
`SystemMonitorV2.swift`). That lineage is **historical only**:

- **Lupita is dead / reference-only.** The Lupita copy is a frozen historical
  reference — it is **not** a sync target and **not** an upstream.
- **No upstreaming.** Do not push changes here back to Lupita.
- **No two-way sync.** There is no fork relationship to maintain, no manual
  sync step, and no "patch upstream first" rule. Edit this engine freely as the
  canonical source.

In short: a correctness fix (e.g. per-core accounting on a new macOS) lands here
and here only. The `Lupita`-prefixed names that remain (e.g. `LupitaMetricsTypes`)
reflect that provenance, not a live relationship.

### Decision

Decided **2026-06-15** (GitHub issue
[#41](https://github.com/Sassy-Dog/devcanopy/issues/41)): neither
extract-a-shared-package nor Lupita-as-upstream. DevCanopy's HostMetricsKit is
canonical; Lupita is dead and reference-only.

## Layout

```
Sources/HostMetricsKit/
├── HostMetricsCollector.swift   # Public entry point
├── HostSnapshot.swift           # Public snapshot type
├── SystemMonitorV2.swift        # IOKit/Mach collection engine (canonical)
├── LupitaMetricsTypes.swift     # Internal data structs (provenance: Lupita)
├── BatteryMonitor.swift         # Battery via IOKit.ps
├── GPUMonitor.swift             # GPU via Metal/IOKit
├── IOKitUtilities.swift         # IOKit helpers
├── CircularBuffer.swift         # History buffers
├── Logger.swift                 # os.Logger wrapper
└── SystemMonitorError.swift     # Error surface
Tests/                           # Package tests (`./dev test`)
```
