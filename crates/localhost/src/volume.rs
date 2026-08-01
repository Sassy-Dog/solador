//! Which mounted filesystems count as a host volume.
//!
//! Three filters, each mirrored from somewhere that already got it right:
//!
//! * **Capacity** — a zero-capacity mount is a pseudo-filesystem, not storage.
//! * **Filesystem type** — the agent's skip list
//!   (`agent/src/metrics.rs::DEFAULT_SKIP_FSTYPES`): transient, remote, or
//!   pseudo filesystems that flap or aren't host storage.
//! * **Mount path** — the Swift collector's `.skipHiddenVolumes` +
//!   `volumeIsBrowsable` pair (`HostMetricsCollector.collectVolumes`). macOS
//!   mounts a dozen synthetic APFS volumes under `/System/Volumes` — `Preboot`,
//!   `VM`, `Update`, `xarts` — each reporting the *container's* capacity on its
//!   own device. Finder hides them; without this filter one Mac renders as ten
//!   identical-looking disks. See [`MountPolicy`].
//!
//! Survivors are then deduped: one filesystem mounted in several places (bind
//! mounts, firmlinks) collapses to the shortest mount path.

use std::collections::HashMap;
use wire::Volume;

const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Filesystem types that are transient, remote, or pseudo. Copied from the
/// agent's list so a local card and a remote card agree on what a "volume" is.
/// The agent's `DEVCANOPY_AGENT_SKIP_FSTYPES` override is deliberately not
/// carried over: that is an operator knob for a headless daemon on someone
/// else's fleet, not something a desktop app should read out of its own env.
const SKIP_FSTYPES: &[&str] = &[
    "autofs",
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smb2",
    "smb3",
    "smbfs",
    "9p",
    "afs",
    "afpfs",
    "ceph",
    "glusterfs",
    "lustre",
    "davfs",
    "davfs2",
    "sshfs",
    "curlftpfs",
    "tmpfs",
    "devtmpfs",
    "ramfs",
    "squashfs",
    "overlay",
    "overlayfs",
    "iso9660",
];

/// A mounted filesystem as plain data.
///
/// sysinfo's `Disk` cannot be constructed in a test, so the platform read is
/// kept to a `Disk` → `MountEntry` map and every filtering decision below is
/// exercised on values. Same trick as the agent's `DiskEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountEntry {
    pub(crate) mount: String,
    /// Backing device (e.g. `/dev/disk3s5`, `C:\`); may be empty.
    pub(crate) device: String,
    pub(crate) total: u64,
    pub(crate) available: u64,
    /// Lowercased filesystem type.
    pub(crate) fstype: String,
}

/// Which mount paths a person would recognise as a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MountPolicy {
    /// macOS: the boot volume and anything under `/Volumes`. Everything else —
    /// `/System/Volumes/*`, `/dev`, `/private/var/vm` — is the synthetic
    /// plumbing Finder hides.
    MacOsBrowsable,
    /// Windows and everything else: whatever survived the fstype filter is a
    /// real volume. Windows enumerates drive letters, all of which are
    /// browsable, so a path rule would only ever be wrong there.
    All,
}

impl MountPolicy {
    /// The policy for the machine this binary is running on.
    ///
    /// A `cfg!` expression rather than `#[cfg]` on purpose: both arms compile
    /// on every platform, so the macOS rule stays type-checked and unit-tested
    /// by CI's windows job too.
    pub(crate) fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOsBrowsable
        } else {
            Self::All
        }
    }

    fn accepts(self, mount: &str) -> bool {
        match self {
            Self::All => true,
            Self::MacOsBrowsable => mount == "/" || mount.starts_with("/Volumes/"),
        }
    }
}

/// Whether a (lowercased) fstype should be dropped.
///
/// `fuse.*` subtypes go too — they're FUSE network/userspace mounts (sshfs,
/// rclone, gvfs). `fuseblk` (NTFS via FUSE, a real local disk) deliberately
/// does not match.
fn should_skip_fstype(fstype: &str) -> bool {
    SKIP_FSTYPES.contains(&fstype) || fstype.starts_with("fuse.")
}

/// Applies the module's filters and dedupe, returning volumes sorted by mount.
///
/// Dedupe keys on the backing device when known — **not** on `(total,
/// available)`: a bind mount reads its capacity at a different instant than its
/// source, so on a busy host the two momentarily disagree and a size-based key
/// flaps. Size is only the fallback when the device name is empty.
pub(crate) fn build_volumes(entries: Vec<MountEntry>, policy: MountPolicy) -> Vec<Volume> {
    let mut by_filesystem: HashMap<String, Volume> = HashMap::new();
    for entry in entries {
        if entry.total == 0 || should_skip_fstype(&entry.fstype) || !policy.accepts(&entry.mount) {
            continue;
        }
        let key = if entry.device.is_empty() {
            format!("size:{}:{}", entry.total, entry.available)
        } else {
            format!("dev:{}", entry.device)
        };
        let volume = Volume {
            mount: entry.mount.clone(),
            used_gb: entry.total.saturating_sub(entry.available) as f64 / BYTES_PER_GIB,
            total_gb: entry.total as f64 / BYTES_PER_GIB,
            fstype: Some(entry.fstype),
        };
        by_filesystem
            .entry(key)
            .and_modify(|existing| {
                if entry.mount.len() < existing.mount.len() {
                    *existing = volume.clone();
                }
            })
            .or_insert(volume);
    }
    let mut volumes: Vec<Volume> = by_filesystem.into_values().collect();
    volumes.sort_by(|a, b| a.mount.cmp(&b.mount));
    volumes
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn entry(mount: &str, device: &str, total: u64, available: u64, fstype: &str) -> MountEntry {
        MountEntry {
            mount: mount.to_string(),
            device: device.to_string(),
            total,
            available,
            fstype: fstype.to_string(),
        }
    }

    fn mounts(volumes: &[Volume]) -> Vec<&str> {
        volumes.iter().map(|v| v.mount.as_str()).collect()
    }

    #[test]
    fn transient_and_pseudo_filesystems_are_skipped() {
        let volumes = build_volumes(
            vec![
                entry("/", "/dev/disk3s5", 500 * GIB, 200 * GIB, "apfs"),
                entry("/net", "map", 100 * GIB, 50 * GIB, "autofs"),
                entry("/mnt/nas", "nas:/vol", 8000 * GIB, 100 * GIB, "nfs"),
                entry("/run", "tmpfs", 4 * GIB, 4 * GIB, "tmpfs"),
                entry("/mnt/rclone", "rclone", 10 * GIB, 5 * GIB, "fuse.rclone"),
            ],
            MountPolicy::All,
        );

        assert_eq!(mounts(&volumes), vec!["/"]);
    }

    /// `fuseblk` is NTFS-via-FUSE — a real local disk. The `fuse.` prefix rule
    /// must not swallow it.
    #[test]
    fn fuseblk_is_a_real_disk_and_survives() {
        let volumes = build_volumes(
            vec![entry(
                "/mnt/win",
                "/dev/sda2",
                900 * GIB,
                400 * GIB,
                "fuseblk",
            )],
            MountPolicy::All,
        );

        assert_eq!(mounts(&volumes), vec!["/mnt/win"]);
    }

    #[test]
    fn zero_capacity_mounts_are_skipped() {
        let volumes = build_volumes(
            vec![entry("/dev", "devfs", 0, 0, "devfs")],
            MountPolicy::All,
        );

        assert!(volumes.is_empty());
    }

    /// The macOS parity case: one Mac must not render as ten disks.
    #[test]
    fn the_macos_policy_hides_the_synthetic_system_volumes() {
        let volumes = build_volumes(
            vec![
                entry("/", "/dev/disk3s5", 500 * GIB, 200 * GIB, "apfs"),
                entry(
                    "/System/Volumes/Preboot",
                    "/dev/disk3s2",
                    500 * GIB,
                    200 * GIB,
                    "apfs",
                ),
                entry(
                    "/System/Volumes/VM",
                    "/dev/disk3s6",
                    500 * GIB,
                    200 * GIB,
                    "apfs",
                ),
                entry(
                    "/System/Volumes/Data",
                    "/dev/disk3s1",
                    500 * GIB,
                    200 * GIB,
                    "apfs",
                ),
                entry(
                    "/Volumes/Backup",
                    "/dev/disk5s1",
                    2000 * GIB,
                    900 * GIB,
                    "apfs",
                ),
            ],
            MountPolicy::MacOsBrowsable,
        );

        assert_eq!(mounts(&volumes), vec!["/", "/Volumes/Backup"]);
    }

    /// Windows drive letters carry no leading slash, so the macOS path rule
    /// would drop every one of them. `All` is why that job still sees disks.
    #[test]
    fn the_all_policy_keeps_windows_drive_letters() {
        let volumes = build_volumes(
            vec![
                entry("C:\\", "C:\\", 900 * GIB, 300 * GIB, "ntfs"),
                entry("D:\\", "D:\\", 4000 * GIB, 100 * GIB, "ntfs"),
            ],
            MountPolicy::All,
        );

        assert_eq!(mounts(&volumes), vec!["C:\\", "D:\\"]);
    }

    #[test]
    fn bind_mounts_of_one_device_collapse_to_the_shortest_path() {
        let volumes = build_volumes(
            vec![
                entry(
                    "/mnt/data/deep/path",
                    "/dev/sdb1",
                    100 * GIB,
                    40 * GIB,
                    "ext4",
                ),
                entry("/data", "/dev/sdb1", 100 * GIB, 40 * GIB, "ext4"),
            ],
            MountPolicy::All,
        );

        assert_eq!(mounts(&volumes), vec!["/data"]);
    }

    /// The flap the device key exists to stop: the same device read twice at
    /// slightly different instants must still be one volume.
    #[test]
    fn one_device_with_disagreeing_sizes_is_still_one_volume() {
        let volumes = build_volumes(
            vec![
                entry("/data", "/dev/sdb1", 100 * GIB, 40 * GIB, "ext4"),
                entry("/mnt/data", "/dev/sdb1", 100 * GIB, 39 * GIB, "ext4"),
            ],
            MountPolicy::All,
        );

        assert_eq!(volumes.len(), 1);
    }

    #[test]
    fn distinct_devices_with_identical_sizes_stay_distinct() {
        let volumes = build_volumes(
            vec![
                entry("/a", "/dev/sdb1", 100 * GIB, 40 * GIB, "ext4"),
                entry("/b", "/dev/sdc1", 100 * GIB, 40 * GIB, "ext4"),
            ],
            MountPolicy::All,
        );

        assert_eq!(mounts(&volumes), vec!["/a", "/b"]);
    }

    #[test]
    fn capacity_is_reported_in_gibibytes_alongside_the_fstype() {
        let volumes = build_volumes(
            vec![entry("/", "/dev/disk3s5", 100 * GIB, 25 * GIB, "apfs")],
            MountPolicy::All,
        );

        assert_eq!(volumes[0].total_gb, 100.0);
        assert_eq!(volumes[0].used_gb, 75.0);
        assert_eq!(volumes[0].fstype.as_deref(), Some("apfs"));
    }
}
