//! GPU sampling.
//!
//! A port of HostMetricsKit's `GPUMonitor`: macOS publishes GPU
//! utilisation and memory occupancy as ordinary properties on the IOKit
//! registry entry of every `IOAccelerator`, so reading them is a registry walk
//! rather than a vendor SDK. Windows has no equivalent cheap read — DXGI or
//! NVML — and is a named non-goal (#205), so it reports [`wire::Gpu::unknown`]
//! and the card renders "—".
//!
//! # What is *not* ported
//!
//! The original monitor ends in a ladder of guesses whenever IOKit declines: an
//! Apple Silicon VRAM capacity of `physicalMemory / 2`, a fallback capacity of
//! a flat `8.0` GB, and a floor that reports "0.5 GB in use" when nothing was
//! measured at all. Those are the fabricated numbers this crate exists to not
//! publish (see the module docs in [`crate`]), so none of them cross. Every
//! figure below is one IOKit actually answered, and anything IOKit declines
//! stays `None`.
//!
//! # Unified memory
//!
//! On Apple Silicon there is no discrete board to measure: the GPU allocates
//! out of system RAM, and `PerformanceStatistics` says so with
//! `In use system memory` rather than any of the `VRAM,*` keys a discrete
//! adapter carries. That figure is real — it is the resident share of the
//! shared pool — but it is only meaningful against the pool it was measured
//! from, so the capacity paired with it is physical memory, the pool itself.
//! A discrete adapter never gets that treatment: its capacity comes from its
//! own `VRAM,*` property or stays unknown.

use std::collections::BTreeMap;

use crate::{BYTES_PER_GIB, BYTES_PER_MIB};

/// One accelerator's numeric properties, lifted out of CoreFoundation into
/// plain Rust.
///
/// This is the seam that keeps the interesting half of this module testable
/// everywhere: [`map_accelerators`] is pure and runs on the Windows CI job,
/// while the IOKit walk that fills these maps is confined to
/// [`macos::accelerators`].
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Accelerator {
    /// The accelerator's own top-level properties. A discrete adapter's VRAM
    /// capacity lives here.
    pub(crate) properties: BTreeMap<String, f64>,
    /// The `PerformanceStatistics` sub-dictionary: utilisation, and how much
    /// memory the GPU currently holds.
    pub(crate) performance_statistics: BTreeMap<String, f64>,
}

/// Utilisation keys, in the original monitor's order — vendors disagree about the
/// spelling and each accelerator publishes exactly one of them. Apple Silicon
/// answers `Device Utilization %`.
const USAGE_KEYS: &[&str] = &[
    "Device Utilization %",
    "GPU Activity(%)",
    "utilization",
    "GPU Core Utilization",
    "Renderer Utilization %",
];

/// Memory-in-use keys of a discrete adapter, in bytes.
const DEDICATED_USED_KEYS: &[&str] = &["vramUsedBytes", "Allocated VRAM", "In use VRAM"];

/// The unified-memory equivalent, in bytes: the resident share of system RAM
/// the GPU is holding. Present instead of the keys above on Apple Silicon.
///
/// Deliberately *not* `Alloc system memory`, which counts everything ever
/// mapped rather than what is resident now — the same distinction as virtual
/// versus resident set size for a process.
const UNIFIED_USED_KEYS: &[&str] = &["In use system memory"];

/// Every key [`read_accelerator`] may consult inside `PerformanceStatistics`.
/// The IOKit walk collects exactly this set; the test below pins it against the
/// ladders so a new key cannot be added to one and forgotten in the other.
///
/// Only the walk reads it, so on a platform that has no walk it does not exist
/// — `cfg`-gated rather than `allow(dead_code)`-ed so the Windows build stays
/// warning-clean without also silencing a *genuinely* dead const later.
#[cfg(any(target_os = "macos", test))]
pub(crate) const PERFORMANCE_STATISTICS_KEYS: &[&str] = &[
    "Device Utilization %",
    "GPU Activity(%)",
    "utilization",
    "GPU Core Utilization",
    "Renderer Utilization %",
    "vramUsedBytes",
    "Allocated VRAM",
    "In use VRAM",
    "In use system memory",
];

/// A discrete adapter's VRAM capacity, paired with what takes each key to
/// bytes. `VRAM,totalMB` is mebibytes despite the name — the same reading the
/// original monitor takes of it.
pub(crate) const CAPACITY_KEYS: &[(&str, f64)] = &[
    ("VRAM,totalMB", BYTES_PER_MIB),
    ("VRAM,totalsize", 1.0),
    ("@0,VRAM,memsize", 1.0),
];

/// The first key in `keys` this map answers with a number a measurement could
/// actually have produced.
///
/// IOKit hands back whatever the driver published, so a `NaN` or a negative
/// byte count is possible in principle; either would paint a nonsense card, and
/// unknown is the honest reading of a nonsense answer. Such a value is skipped
/// rather than allowed to end the search — a driver publishing a broken
/// `Device Utilization %` should not also suppress the good
/// `Renderer Utilization %` below it.
fn first_number(map: &BTreeMap<String, f64>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .filter_map(|key| map.get(*key).copied())
        .find(|value| value.is_finite() && *value >= 0.0)
}

/// Maps one accelerator's properties onto the wire contract's GPU.
///
/// `pool_bytes` is physical memory, used *only* as the capacity paired with a
/// unified-memory occupancy figure — see the module docs. Pass `0` for "the
/// pool size is unknown" and the capacity stays unknown with it.
fn read_accelerator(accelerator: &Accelerator, pool_bytes: u64) -> wire::Gpu {
    let usage = first_number(&accelerator.performance_statistics, USAGE_KEYS)
        .map(|usage| usage.clamp(0.0, 100.0));

    let dedicated_capacity = CAPACITY_KEYS.iter().find_map(|(key, to_bytes)| {
        first_number(&accelerator.properties, &[key]).map(|value| value * to_bytes)
    });

    // Which capacity belongs beside the occupancy figure depends on where that
    // figure came from, so the two are chosen together rather than in separate
    // ladders. Pairing a unified-memory occupancy with a discrete capacity (or
    // the reverse) would render a ratio of two different pools.
    let (used_bytes, total_bytes) = if let Some(used) =
        first_number(&accelerator.performance_statistics, DEDICATED_USED_KEYS)
    {
        (Some(used), dedicated_capacity)
    } else if let Some(used) = first_number(&accelerator.performance_statistics, UNIFIED_USED_KEYS)
    {
        let pool = (pool_bytes > 0).then_some(pool_bytes as f64);
        (Some(used), dedicated_capacity.or(pool))
    } else {
        (None, dedicated_capacity)
    };

    wire::Gpu {
        usage,
        vram_used_gb: used_bytes.map(|bytes| bytes / BYTES_PER_GIB),
        vram_total_gb: total_bytes.map(|bytes| bytes / BYTES_PER_GIB),
    }
}

/// Whether this reading says anything at all — the test for "this accelerator
/// answered", as opposed to one that matched but published no statistics.
fn answered(gpu: &wire::Gpu) -> bool {
    gpu.usage.is_some() || gpu.vram_used_gb.is_some() || gpu.vram_total_gb.is_some()
}

/// Picks the reading to publish from every accelerator IOKit matched.
///
/// The first one that answers wins *whole*: a machine with two GPUs would
/// otherwise get a card assembled from both, whose VRAM ratio describes neither.
/// Returns [`wire::Gpu::unknown`] when nothing answered — a VM without an
/// IOAccelerator, or every platform that is not macOS.
pub(crate) fn map_accelerators(accelerators: &[Accelerator], pool_bytes: u64) -> wire::Gpu {
    accelerators
        .iter()
        .map(|accelerator| read_accelerator(accelerator, pool_bytes))
        .find(answered)
        .unwrap_or_else(wire::Gpu::unknown)
}

/// Samples this machine's GPU.
///
/// `pool_bytes` is physical memory, which only matters on unified-memory
/// hardware; see the module docs.
#[cfg(target_os = "macos")]
pub(crate) fn read(pool_bytes: u64) -> wire::Gpu {
    map_accelerators(&macos::accelerators(), pool_bytes)
}

/// Windows (and anything else) has no `IOAccelerator` registry to walk, and
/// nothing as cheap in its place: DXGI reports adapter memory but not
/// utilisation, and utilisation lives behind PDH counters or a vendor library
/// (NVML, ADL) that differs per GPU. A named non-goal in #205 — unknown until a
/// slice decides that trade is worth making, exactly as `thermal::read` is on
/// the same platform.
///
/// Routed through [`map_accelerators`] with nothing to map rather than
/// returning [`wire::Gpu::unknown`] directly. It is the same value by the same
/// rule — "no accelerator answered" — and taking the same path keeps one
/// definition of that rule instead of two that can drift apart, while leaving
/// the mapping above compiled and lint-checked on the Windows job.
#[cfg(not(target_os = "macos"))]
pub(crate) fn read(pool_bytes: u64) -> wire::Gpu {
    map_accelerators(&[], pool_bytes)
}

#[cfg(target_os = "macos")]
mod macos {
    //! The IOKit registry walk, and the only unsafe code in this crate.
    //!
    //! Every accelerator's properties arrive as one CoreFoundation dictionary
    //! per registry entry; this module lifts the handful of numeric keys
    //! [`super`] knows about out of them and leaves every decision to the pure
    //! mapping there.

    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::ptr::{self, NonNull};

    use objc2_core_foundation::{
        CFDictionary, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType, Type,
    };
    use objc2_io_kit::{
        io_iterator_t, io_object_t, kIOMainPortDefault, IOIteratorNext, IOObjectRelease,
        IORegistryEntryCreateCFProperties, IOServiceGetMatchingServices, IOServiceMatching,
    };

    use super::{Accelerator, CAPACITY_KEYS, PERFORMANCE_STATISTICS_KEYS};

    /// `kern_return_t` for "it worked". IOKit's own `KERN_SUCCESS`, which none
    /// of the bindings in use re-export.
    const KERN_SUCCESS: i32 = 0;

    /// The IOKit class to match.
    ///
    /// One name covers every vendor: `IOServiceMatching` matches on
    /// `IOProviderClass`, which IOKit resolves with a kind-of test rather than
    /// an exact-name one, so `IOAccelerator` also matches the
    /// `AGXAcceleratorG13X` of an Apple Silicon Mac and the `IntelAccelerator`
    /// or `AMDRadeonX…` of an Intel one. The original monitor lists all of those
    /// spellings explicitly and never reaches past the first.
    const ACCELERATOR_CLASS: &str = "IOAccelerator";

    /// The `PerformanceStatistics` sub-dictionary's key on the registry entry.
    const PERFORMANCE_STATISTICS: &str = "PerformanceStatistics";

    /// Every accelerator IOKit has registered, with the properties [`super`]
    /// reads already lifted out.
    ///
    /// An empty vector is the honest answer on a machine with no
    /// `IOAccelerator` — a VM, which is what CI's macOS runners are — and
    /// [`super::map_accelerators`] turns it into an unknown GPU rather than a
    /// zeroed one.
    pub(super) fn accelerators() -> Vec<Accelerator> {
        let Ok(class) = CString::new(ACCELERATOR_CLASS) else {
            return Vec::new();
        };
        // SAFETY: `class` is a valid NUL-terminated C string that outlives the
        // call, which is all `IOServiceMatching` reads.
        let Some(matching) = (unsafe { IOServiceMatching(class.as_ptr()) }) else {
            return Vec::new();
        };
        // `IOServiceGetMatchingServices` consumes one reference of the
        // dictionary it is handed. Retaining through the deref to the immutable
        // superclass hands it a reference of its own, so `matching`'s is still
        // ours to drop.
        // The second deref is the one that matters: it goes through
        // `CFMutableDictionary`'s superclass to `CFDictionary`, which is the
        // type the call below takes.
        let matching: CFRetained<CFDictionary> = (**matching).retain();

        let mut iterator: io_iterator_t = 0;
        // SAFETY: `kIOMainPortDefault` is IOKit's own default port, the
        // dictionary is a valid matching dictionary whose reference this call
        // takes over, and `iterator` is a live out-parameter.
        let result = unsafe {
            IOServiceGetMatchingServices(kIOMainPortDefault, Some(matching), &mut iterator)
        };
        if result != KERN_SUCCESS || iterator == 0 {
            return Vec::new();
        }

        let mut accelerators = Vec::new();
        loop {
            let service = IOIteratorNext(iterator);
            if service == 0 {
                break;
            }
            if let Some(accelerator) = read_service(service) {
                accelerators.push(accelerator);
            }
            // `IOIteratorNext` returns a reference we own; releasing it once,
            // after the read above, balances it.
            IOObjectRelease(service);
        }
        // The iterator is ours too, and is released exactly once, here.
        IOObjectRelease(iterator);

        accelerators
    }

    /// Lifts one registry entry's interesting properties into plain Rust.
    ///
    /// `None` when the entry publishes no properties at all; an entry that
    /// publishes some but none this crate reads yields empty maps, which
    /// [`super::map_accelerators`] reads as "did not answer".
    fn read_service(service: io_object_t) -> Option<Accelerator> {
        let mut raw: *mut CFMutableDictionary = ptr::null_mut();
        // SAFETY: `service` is a live registry entry, `raw` is a valid
        // out-pointer, and a null allocator asks for the default one.
        let result = unsafe { IORegistryEntryCreateCFProperties(service, &mut raw, None, 0) };
        // Ordered so that a dictionary is only ever taken ownership of after
        // the call is known to have succeeded: bailing out on the pointer first
        // would drop a populated one on the floor unreleased.
        if result != KERN_SUCCESS {
            return None;
        }
        let properties = NonNull::new(raw)?;
        // SAFETY: the `Create` in the name is CoreFoundation's create rule —
        // the dictionary comes back with a reference we now own, and
        // `CFRetained` releases it on drop.
        let properties: CFRetained<CFMutableDictionary> =
            unsafe { CFRetained::from_raw(properties) };
        // SAFETY: an IOKit property table is keyed by `CFString` and holds
        // arbitrary CF values; naming those types is what lets `get` below take
        // a `CFString` key.
        let properties: &CFDictionary<CFString, CFType> =
            unsafe { properties.cast_unchecked::<CFString, CFType>() };

        let performance_statistics = properties
            .get(&CFString::from_str(PERFORMANCE_STATISTICS))
            .and_then(|value| {
                // SAFETY (of the cast): same property-table reasoning as above,
                // applied to the sub-dictionary this key holds.
                let nested = value.downcast_ref::<CFDictionary>()?;
                let nested = unsafe { nested.cast_unchecked::<CFString, CFType>() };
                Some(numbers(nested, PERFORMANCE_STATISTICS_KEYS))
            })
            .unwrap_or_default();

        let capacity_keys: Vec<&str> = CAPACITY_KEYS.iter().map(|(key, _)| *key).collect();

        Some(Accelerator {
            properties: numbers(properties, &capacity_keys),
            performance_statistics,
        })
    }

    /// The numeric values of `keys`, skipping every key the dictionary does not
    /// hold or holds as something other than a number.
    fn numbers(
        dictionary: &CFDictionary<CFString, CFType>,
        keys: &[&str],
    ) -> BTreeMap<String, f64> {
        keys.iter()
            .filter_map(|key| {
                let value = dictionary.get(&CFString::from_str(key))?;
                let number = value.downcast_ref::<CFNumber>()?.as_f64()?;
                Some(((*key).to_string(), number))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accelerator(performance_statistics: &[(&str, f64)]) -> Accelerator {
        Accelerator {
            properties: BTreeMap::new(),
            performance_statistics: performance_statistics
                .iter()
                .map(|(key, value)| ((*key).to_string(), *value))
                .collect(),
        }
    }

    /// 32 GiB, the pool of the Apple Silicon Mac the fixtures below were taken
    /// from.
    const POOL: u64 = 34_359_738_368;

    /// The exact `PerformanceStatistics` of an `AGXAcceleratorG13X` (M1 Max),
    /// so the mapping is pinned against a real payload rather than an invented
    /// one.
    fn apple_silicon() -> Accelerator {
        accelerator(&[
            ("Device Utilization %", 69.0),
            ("Renderer Utilization %", 68.0),
            ("Tiler Utilization %", 69.0),
            ("In use system memory", 2_026_127_360.0),
            ("In use system memory (driver)", 0.0),
            ("Alloc system memory", 11_595_104_256.0),
        ])
    }

    #[test]
    fn an_apple_silicon_accelerator_reports_utilisation_and_its_share_of_the_pool() {
        let gpu = map_accelerators(&[apple_silicon()], POOL);

        assert_eq!(gpu.usage, Some(69.0));
        // Resident GPU memory measured against the pool it was taken from, not
        // a guess at half of RAM.
        assert_eq!(gpu.vram_used_gb, Some(2_026_127_360.0 / BYTES_PER_GIB));
        assert_eq!(gpu.vram_total_gb, Some(32.0));
        assert!(
            gpu.is_present(),
            "the card must render, not show an em dash"
        );
    }

    /// The occupancy figure is the resident one. `Alloc system memory` counts
    /// everything ever mapped — 10.8 GiB against 1.9 GiB resident on the same
    /// sample — and reporting it would overstate the GPU's footprint fivefold.
    #[test]
    fn unified_occupancy_is_the_resident_figure_not_the_allocated_one() {
        let gpu = map_accelerators(&[apple_silicon()], POOL);

        let used_gb = gpu.vram_used_gb.expect("a measured occupancy");
        assert!(used_gb < 2.0, "got {used_gb}");
    }

    /// The acceptance criterion for the CI runners: a macOS VM matches no
    /// `IOAccelerator` at all, and must read as unknown rather than as a GPU
    /// sitting at 0%.
    #[test]
    fn a_machine_with_no_accelerator_reports_unknown_never_zeros() {
        let gpu = map_accelerators(&[], POOL);

        assert_eq!(gpu, wire::Gpu::unknown());
        assert!(!gpu.is_present());
        assert_ne!(gpu, wire::Gpu::zeros());
    }

    /// An accelerator that matched but published no statistics is the same
    /// answer as no accelerator — it did not decline to have a GPU, it declined
    /// to measure one.
    #[test]
    fn an_accelerator_that_publishes_nothing_readable_reports_unknown() {
        let silent = accelerator(&[("SplitSceneCount", 0.0), ("recoveryCount", 0.0)]);

        assert_eq!(map_accelerators(&[silent], POOL), wire::Gpu::unknown());
    }

    /// A real GPU sitting idle reports `0`, and that is a measurement — the
    /// distinction `wire::Gpu` exists to carry.
    #[test]
    fn an_idle_gpu_reports_a_measured_zero_not_an_unknown() {
        let idle = accelerator(&[("Device Utilization %", 0.0), ("In use system memory", 0.0)]);

        let gpu = map_accelerators(&[idle], POOL);

        assert_eq!(gpu.usage, Some(0.0));
        assert_eq!(gpu.vram_used_gb, Some(0.0));
        assert!(gpu.is_present(), "capacity is what makes a GPU present");
    }

    /// Without a pool size there is nothing to measure the occupancy against,
    /// so the capacity stays unknown — and `is_present` then blanks the card
    /// rather than showing a ratio with no denominator.
    #[test]
    fn unified_memory_without_a_known_pool_leaves_the_capacity_unknown() {
        let gpu = map_accelerators(&[apple_silicon()], 0);

        assert_eq!(gpu.usage, Some(69.0));
        assert_eq!(gpu.vram_total_gb, None);
        assert!(!gpu.is_present());
    }

    /// A discrete adapter carries its own capacity, and must never be handed
    /// system RAM as one: its VRAM is a separate pool, and pairing the two
    /// would render a ratio of a board against a motherboard.
    #[test]
    fn a_discrete_adapter_uses_its_own_capacity_never_the_system_pool() {
        let discrete = Accelerator {
            properties: [("VRAM,totalMB".to_string(), 24_576.0)]
                .into_iter()
                .collect(),
            performance_statistics: [("vramUsedBytes".to_string(), 3_221_225_472.0)]
                .into_iter()
                .collect(),
        };

        let gpu = map_accelerators(&[discrete], POOL);

        assert_eq!(gpu.vram_total_gb, Some(24.0));
        assert_eq!(gpu.vram_used_gb, Some(3.0));
    }

    /// A discrete adapter that reports occupancy but no capacity gets no
    /// capacity — the original monitor's flat `8.0` GB fallback is exactly the
    /// invented number that does not cross.
    #[test]
    fn a_discrete_adapter_without_a_capacity_key_gets_no_invented_capacity() {
        let discrete = Accelerator {
            properties: BTreeMap::new(),
            performance_statistics: [("Allocated VRAM".to_string(), 1_073_741_824.0)]
                .into_iter()
                .collect(),
        };

        let gpu = map_accelerators(&[discrete], POOL);

        assert_eq!(gpu.vram_used_gb, Some(1.0));
        assert_eq!(gpu.vram_total_gb, None, "no capacity was measured");
    }

    /// Vendors disagree about the spelling, so the ladder is the contract.
    #[test]
    fn every_utilisation_spelling_is_read() {
        for key in USAGE_KEYS {
            let gpu = map_accelerators(&[accelerator(&[(key, 42.0)])], POOL);
            assert_eq!(gpu.usage, Some(42.0), "{key}");
        }
    }

    /// Earlier keys win, so an accelerator publishing both a device and a
    /// renderer figure reports the device one — the original monitor's order, kept
    /// so the two apps agree on which number they are showing.
    #[test]
    fn the_first_spelling_present_wins() {
        let gpu = map_accelerators(&[apple_silicon()], POOL);

        assert_eq!(gpu.usage, Some(69.0), "device, not the renderer's 68");
    }

    /// A driver can publish anything; a percentage outside 0–100 is a driver
    /// bug, and a bar drawn past its own end is a rendering one.
    #[test]
    fn utilisation_is_clamped_to_a_percentage() {
        let over = map_accelerators(&[accelerator(&[("utilization", 140.0)])], POOL);

        assert_eq!(over.usage, Some(100.0));
    }

    /// Nonsense is unknown, never a number. A negative byte count or a `NaN`
    /// would otherwise reach the card as a measurement.
    #[test]
    fn values_no_measurement_could_produce_read_as_unknown() {
        let nonsense = accelerator(&[
            ("Device Utilization %", f64::NAN),
            ("In use system memory", -1.0),
        ]);

        assert_eq!(map_accelerators(&[nonsense], POOL), wire::Gpu::unknown());
    }

    /// …and nonsense in one spelling must not suppress a good reading in the
    /// next one down. Ending the search on the broken key would blank a card
    /// the machine could have filled.
    #[test]
    fn a_broken_key_is_skipped_rather_than_ending_the_ladder() {
        let partly_broken = accelerator(&[
            ("Device Utilization %", f64::NAN),
            ("Renderer Utilization %", 68.0),
        ]);

        assert_eq!(map_accelerators(&[partly_broken], POOL).usage, Some(68.0));
    }

    /// One accelerator answers for the whole card. A second GPU's capacity
    /// beside the first's occupancy is a ratio of two different pools.
    #[test]
    fn the_first_accelerator_that_answers_is_used_whole() {
        let silent = accelerator(&[("recoveryCount", 0.0)]);
        let answering = accelerator(&[("Device Utilization %", 12.0)]);

        let gpu = map_accelerators(&[silent, answering], POOL);

        assert_eq!(gpu.usage, Some(12.0), "the silent one must not shadow it");
    }

    /// The IOKit walk collects [`PERFORMANCE_STATISTICS_KEYS`] and nothing
    /// else, so a key added to a ladder but not to that list would be read on
    /// no machine at all — and every test above would still pass, because they
    /// build their maps directly.
    #[test]
    fn every_ladder_key_is_one_the_iokit_walk_collects() {
        for key in USAGE_KEYS
            .iter()
            .chain(DEDICATED_USED_KEYS)
            .chain(UNIFIED_USED_KEYS)
        {
            assert!(
                PERFORMANCE_STATISTICS_KEYS.contains(key),
                "{key} is read but never collected"
            );
        }
    }

    /// macOS reads a real accelerator or admits it found none; every other
    /// platform admits it before looking. Both arms are the same rule, so both
    /// are checked wherever the tests run — the shape `thermal::read`'s test
    /// already uses.
    #[test]
    fn a_platform_without_an_accelerator_registry_reports_unknown() {
        let gpu = read(POOL);

        if cfg!(target_os = "macos") {
            // A macOS VM (CI's runners) legitimately matches no accelerator, so
            // the assertion is coherence, not presence: whatever was read must
            // be a real reading or no reading.
            assert!(
                gpu == wire::Gpu::unknown() || gpu.usage.is_some() || gpu.vram_used_gb.is_some(),
                "got {gpu:?}"
            );
            assert_ne!(gpu, wire::Gpu::zeros(), "never the pre-#183 fabrication");
        } else {
            assert_eq!(gpu, wire::Gpu::unknown());
        }
    }
}
