//! Host memory-pressure level, shared by the local cockpit and remote agent.
//!
//! macOS keeps RAM full on purpose (compressed memory, file cache), so
//! `used ÷ total` reads high all day on a healthy Mac. The kernel's own
//! verdict, the one Activity Monitor colours its Memory Pressure graph by, is
//! `kern.memorystatus_vm_pressure_level`. That sysctl predates the agent's
//! macOS 11.0 floor by many releases.

/// How hard the kernel says memory is under pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warning,
    Critical,
}

impl Level {
    /// Maps `kern.memorystatus_vm_pressure_level`: `1` normal, `2` warn,
    /// `4` critical. Anything else is `None`: unknown beats guessing.
    #[must_use]
    pub fn from_kernel(raw: i32) -> Option<Self> {
        match raw {
            1 => Some(Level::Normal),
            2 => Some(Level::Warning),
            4 => Some(Level::Critical),
            _ => None,
        }
    }

    /// This level as `wire::Memory::pressure_level`, in `crates/thermal`'s
    /// style of integer encoding: `0` normal, `1` warning, `2` critical.
    #[must_use]
    pub fn to_wire(self) -> i64 {
        match self {
            Level::Normal => 0,
            Level::Warning => 1,
            Level::Critical => 2,
        }
    }
}

/// Reads the kernel's memory-pressure level, or `None` where the platform
/// exposes none or the read failed.
#[cfg(target_os = "macos")]
#[must_use]
pub fn read() -> Option<Level> {
    use std::ffi::CStr;
    use std::os::raw::c_void;

    const NAME: &CStr = c"kern.memorystatus_vm_pressure_level";
    let mut value: i32 = 0;
    let mut size = std::mem::size_of::<i32>();
    // SAFETY: `NAME` is NUL-terminated, `value` and `size` outlive the call and
    // `size` matches the buffer. No new value is written (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr(),
            (&mut value as *mut i32).cast::<c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size != std::mem::size_of::<i32>() {
        return None;
    }
    Level::from_kernel(value)
}

/// Linux PSI and Windows have no equivalent ladder; they stay unknown and keep
/// `used ÷ total` against the operator's thresholds.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn read() -> Option<Level> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_values_map_and_everything_else_is_unknown() {
        assert_eq!(Level::from_kernel(1), Some(Level::Normal));
        assert_eq!(Level::from_kernel(2), Some(Level::Warning));
        assert_eq!(Level::from_kernel(4), Some(Level::Critical));
        for raw in [0, 3, 5, -1, i32::MAX] {
            assert_eq!(Level::from_kernel(raw), None, "{raw}");
        }
    }

    /// Pins this side of the encoding; `viewmodel::color::MemoryPressure::from_wire`
    /// is the decoder and carries its own test of the same numbers.
    #[test]
    fn the_wire_encoding_is_zero_one_two() {
        assert_eq!(Level::Normal.to_wire(), 0);
        assert_eq!(Level::Warning.to_wire(), 1);
        assert_eq!(Level::Critical.to_wire(), 2);
    }

    #[test]
    fn a_platform_without_the_sysctl_reports_unknown() {
        let level = read();
        if cfg!(target_os = "macos") {
            assert!(level.is_some(), "macOS answers the sysctl");
        } else {
            assert_eq!(level, None);
        }
    }
}
