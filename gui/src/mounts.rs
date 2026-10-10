//! Enumerate mounted drives (Windows) / mount points (macOS, Linux) and their
//! capacity, using [`sysinfo`] so the logic is identical across platforms.
//!
//! sysinfo deliberately omits network mounts (SMB/NFS/AFP/…) on every
//! platform, so those are enumerated separately by the per-OS [`network`]
//! module and merged in when the user enables "Show Network Shares".
//!
//! sysinfo can also miss some *removable* local volumes on macOS (external
//! disks behind certain USB/Thunderbolt bridges, SD readers, read-only
//! mounts). Removable drives are always shown, so on macOS the same
//! `getfsstat` scan recovers any browsable removable volume sysinfo left out
//! (see [`extra_local_removable`]), de-duplicated by mount point.

use std::sync::mpsc;
use std::time::Duration;

use sysinfo::Disks;

/// A single mounted volume as shown in the main window.
pub struct Mount {
    /// Volume label / device name (may be empty on some systems).
    pub name: String,
    /// Mount path: a drive root like `C:\` on Windows, or a mount point such
    /// as `/`, `/System/Volumes/Data`, `/mnt/data` on macOS/Linux.
    pub mount_point: String,
    /// File system type, e.g. `apfs`, `ntfs`, `ext4`.
    pub file_system: String,
    /// Total capacity in bytes.
    pub total_bytes: u64,
    /// Free/available capacity in bytes.
    pub available_bytes: u64,
    /// Whether the device is removable (USB stick, SD card, …).
    pub removable: bool,
    /// Whether this is a network share (SMB, NFS, AFP, WebDAV, …).
    pub network: bool,
}

impl Mount {
    /// Bytes currently in use (saturating, in case `available > total`).
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    /// Fraction of capacity in use in `0.0..=1.0` (0 when total is unknown).
    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.used_bytes() as f64 / self.total_bytes as f64) as f32
        }
    }
}

/// List the current mounts, sorted by mount path for a stable display order.
/// Network shares are included only when `include_network` is true;
/// removable mounts are always included.
pub fn list(include_network: bool) -> Vec<Mount> {
    let disks = Disks::new_with_refreshed_list();
    let mut mounts: Vec<Mount> = disks
        .list()
        .iter()
        .map(|d| {
            let file_system = d.file_system().to_string_lossy().into_owned();
            Mount {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                network: is_network_fs(&file_system),
                file_system,
                total_bytes: d.total_space(),
                available_bytes: d.available_space(),
                removable: d.is_removable(),
            }
        })
        .collect();
    // macOS presents the APFS system (`/`, read-only) and Data
    // (`/System/Volumes/Data`, writable) volumes as one logical disk via
    // firmlinks; Finder shows a single "Macintosh HD". sysinfo lists both with
    // identical capacity, so drop the Data half and keep `/`.
    mounts.retain(|m| !is_hidden_system_volume(&m.mount_point));
    // sysinfo silently omits some removable volumes on macOS (external disks
    // behind certain USB/Thunderbolt bridges, SD readers, drives mounted
    // read-only, …). Removable mounts are always shown, so recover any the OS
    // reports but sysinfo missed, de-duplicating by mount point.
    for extra in extra_local_removable() {
        if !mounts.iter().any(|m| m.mount_point == extra.mount_point) {
            mounts.push(extra);
        }
    }
    if include_network {
        let shares = network::list();
        // The OS-specific listing is authoritative for anything it reports, so
        // drop any sysinfo duplicate of the same mount point.
        mounts.retain(|m| !shares.iter().any(|s| s.mount_point == m.mount_point));
        mounts.extend(shares);
    } else {
        // sysinfo may still report some network file systems (e.g. sshfs on
        // Linux); hide those too, but never a removable mount.
        mounts.retain(keep_without_network);
    }
    mounts.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    mounts
}

/// Whether a mount stays visible when network shares are hidden: removable
/// mounts always do, everything else only if it isn't a network share.
fn keep_without_network(m: &Mount) -> bool {
    m.removable || !m.network
}

/// macOS: the writable APFS Data volume is firmlinked under `/` and shown by
/// Finder as part of the single "Macintosh HD", so hide it (and anything
/// mounted beneath it, e.g. the `/System/Volumes/Data/home` automount) to avoid
/// a duplicate of `/`. Other platforms hide nothing here.
#[cfg(target_os = "macos")]
fn is_hidden_system_volume(mount_point: &str) -> bool {
    mount_point == "/System/Volumes/Data" || mount_point.starts_with("/System/Volumes/Data/")
}

#[cfg(not(target_os = "macos"))]
fn is_hidden_system_volume(_mount_point: &str) -> bool {
    false
}

/// Removable local volumes that sysinfo may have omitted. Only macOS adds any;
/// every other platform relies on sysinfo's removable detection and returns an
/// empty list here.
#[cfg(target_os = "macos")]
fn extra_local_removable() -> Vec<Mount> {
    network::local_removable()
}

#[cfg(not(target_os = "macos"))]
fn extra_local_removable() -> Vec<Mount> {
    Vec::new()
}

/// Whether a file-system type name denotes a network file system.
pub fn is_network_fs(fs_type: &str) -> bool {
    let fs = fs_type.to_ascii_lowercase();
    matches!(
        fs.as_str(),
        "nfs"
            | "nfs4"
            | "smbfs"
            | "smb"
            | "smb2"
            | "smb3"
            | "cifs"
            | "afpfs"
            | "webdav"
            | "davfs"
            | "ftp"
            | "9p"
            | "afs"
            | "ceph"
            | "glusterfs"
            | "lustre"
    ) || fs.starts_with("fuse.sshfs")
        || fs.starts_with("fuse.rclone")
        || fs.starts_with("fuse.glusterfs")
}

/// Drop the `user@` part of a share source like `//user@host/share`, so the
/// display name reads `//host/share` (macOS uses long Apple ID user names).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn strip_user(source: &str) -> String {
    if let Some(rest) = source.strip_prefix("//") {
        let host_end = rest.find('/').unwrap_or(rest.len());
        if let Some(at) = rest[..host_end].rfind('@') {
            return format!("//{}", &rest[at + 1..]);
        }
    }
    source.to_string()
}

/// Run `f` on a helper thread, giving up after `timeout`. Querying the size
/// of an unreachable (e.g. hard-mounted NFS or disconnected SMB) share can
/// block for minutes, which must not freeze the UI. A thread that times out
/// is left to finish (or stay blocked) on its own.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn with_timeout<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

/// How long to wait for a single network share to report its capacity.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const STAT_TIMEOUT: Duration = Duration::from_millis(1500);

/// macOS: `getfsstat(MNT_NOWAIT)` returns every mount with cached statistics
/// (never blocks on the network). From that single scan we derive two lists:
/// network shares (`list`, the browsable non-`MNT_LOCAL` mounts) and browsable
/// removable local volumes (`local_removable`, which recovers removable disks
/// sysinfo omits). `MNT_DONTBROWSE` mounts are skipped in both to match Finder.
#[cfg(target_os = "macos")]
mod network {
    use super::{strip_user, Mount};
    use std::ffi::CStr;

    /// `MNT_REMOVABLE` from `<sys/mount.h>`: the volume lives on removable
    /// media. `libc` doesn't re-export it, so define it here.
    const MNT_REMOVABLE: u32 = 0x0000_0200;

    /// Snapshot every mount via `getfsstat(MNT_NOWAIT)` (uses cached statistics,
    /// so it never blocks on the network). `MNT_DONTBROWSE` entries are skipped
    /// to match what Finder shows (system volumes, Recovery, mounted app disk
    /// images, simulator runtimes, …).
    fn scan() -> Vec<libc::statfs> {
        // SAFETY: a null buffer asks only for the mount count.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Vec::new();
        }
        // Leave headroom in case mounts appear between the two calls.
        let cap = count as usize + 8;
        let mut buf: Vec<libc::statfs> = Vec::with_capacity(cap);
        let bytes = (cap * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        // SAFETY: `buf` has room for `cap` entries and `bytes` says so.
        let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
        if n <= 0 {
            return Vec::new();
        }
        // SAFETY: getfsstat initialized the first `n` (≤ cap) entries.
        unsafe { buf.set_len(n as usize) };
        buf
    }

    fn cstr(a: &[libc::c_char]) -> String {
        // SAFETY: the kernel NUL-terminates these fixed arrays.
        unsafe { CStr::from_ptr(a.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn has_flag(s: &libc::statfs, flag: u32) -> bool {
        s.f_flags & flag != 0
    }

    /// Build a `Mount` from a `statfs` entry, tagging it network/removable per
    /// the caller. Returns `None` for autofs automount placeholders.
    fn to_mount(s: &libc::statfs, network: bool, removable: bool) -> Option<Mount> {
        let file_system = cstr(&s.f_fstypename);
        // autofs entries are automount placeholders, not real volumes.
        if file_system == "autofs" {
            return None;
        }
        let bsize = s.f_bsize as u64;
        Some(Mount {
            name: strip_user(&cstr(&s.f_mntfromname)),
            mount_point: cstr(&s.f_mntonname),
            file_system,
            total_bytes: s.f_blocks.saturating_mul(bsize),
            available_bytes: s.f_bavail.saturating_mul(bsize),
            removable,
            network,
        })
    }

    /// Whether a mount should be listed as a network share, from its
    /// file-system type and two mount flags. Pure so it can be unit-tested
    /// without fabricating `statfs`.
    ///
    /// A mount counts as a share when either its file-system type is a
    /// recognized network protocol (SMB/NFS/AFP/WebDAV/…) — kept even if it's
    /// `nobrowse` — or it's a non-local, browsable mount (covering protocols
    /// not in the name list).
    pub(super) fn is_network_share(fstype: &str, non_local: bool, dontbrowse: bool) -> bool {
        super::is_network_fs(fstype) || (non_local && !dontbrowse)
    }

    /// Network shares (SMB/NFS/AFP/WebDAV/…). Non-network local volumes are
    /// never included here; they come from sysinfo. autofs placeholders are
    /// dropped by `to_mount`.
    pub fn list() -> Vec<Mount> {
        scan()
            .iter()
            .filter(|s| {
                let fs = cstr(&s.f_fstypename);
                is_network_share(
                    &fs,
                    !has_flag(s, libc::MNT_LOCAL as u32),
                    has_flag(s, libc::MNT_DONTBROWSE as u32),
                )
            })
            .filter_map(|s| to_mount(s, true, false))
            .collect()
    }

    /// Browsable removable local volumes (external USB/Thunderbolt disks, SD
    /// cards, …). sysinfo misses some of these, so they are merged in by
    /// [`super::list`]. Non-browsable (`MNT_DONTBROWSE`) removables such as
    /// mounted application disk images are left out, matching Finder.
    pub fn local_removable() -> Vec<Mount> {
        scan()
            .iter()
            .filter(|s| !has_flag(s, libc::MNT_DONTBROWSE as u32))
            .filter(|s| has_flag(s, libc::MNT_LOCAL as u32))
            .filter(|s| has_flag(s, MNT_REMOVABLE))
            .filter_map(|s| to_mount(s, false, true))
            .collect()
    }
}

/// Linux: read network file systems from `/proc/self/mounts`, then
/// `statvfs` each one in parallel under a timeout.
#[cfg(target_os = "linux")]
mod network {
    use super::{is_network_fs, with_timeout, Mount, STAT_TIMEOUT};
    use std::ffi::CString;

    /// Undo the octal escapes `/proc/mounts` uses for whitespace.
    fn unescape(s: &str) -> String {
        s.replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\")
    }

    fn statvfs(path: &str) -> Option<(u64, u64)> {
        let c = CString::new(path).ok()?;
        // SAFETY: `st` is a plain C struct filled in by statvfs on success.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        let frsize = st.f_frsize as u64;
        Some((
            (st.f_blocks as u64).saturating_mul(frsize),
            (st.f_bavail as u64).saturating_mul(frsize),
        ))
    }

    pub fn list() -> Vec<Mount> {
        let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else {
            return Vec::new();
        };
        let entries: Vec<(String, String, String)> = text
            .lines()
            .filter_map(|line| {
                let mut f = line.split_whitespace();
                let spec = unescape(f.next()?);
                let file = unescape(f.next()?);
                let fs = f.next()?.to_string();
                is_network_fs(&fs).then_some((spec, file, fs))
            })
            .collect();

        // Start every stat at once so the total wait is bounded by one timeout.
        let pending: Vec<_> = entries
            .iter()
            .map(|(_, file, _)| {
                let path = file.clone();
                std::thread::spawn(move || with_timeout(STAT_TIMEOUT, move || statvfs(&path)))
            })
            .collect();

        entries
            .into_iter()
            .zip(pending)
            .map(|((spec, file, fs), handle)| {
                let (total, avail) = handle.join().ok().flatten().flatten().unwrap_or((0, 0));
                Mount {
                    name: spec,
                    mount_point: file,
                    file_system: fs,
                    total_bytes: total,
                    available_bytes: avail,
                    removable: false,
                    network: true,
                }
            })
            .collect()
    }
}

/// Windows: mapped drive letters whose type is `DRIVE_REMOTE`; label, file
/// system, and capacity are queried in parallel under a timeout.
#[cfg(target_os = "windows")]
mod network {
    use super::{with_timeout, Mount, STAT_TIMEOUT};
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    use windows::Win32::System::WindowsProgramming::DRIVE_REMOTE;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// (label, file system, total bytes, available bytes) for a drive root.
    fn query(root: String) -> (String, String, u64, u64) {
        let w = wide(&root);
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        // SAFETY: `w` is NUL-terminated and the buffers outlive the calls.
        let (label, fs) = match unsafe {
            GetVolumeInformationW(
                PCWSTR(w.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut fs),
            )
        } {
            Ok(()) => (from_wide(&label), from_wide(&fs)),
            Err(_) => (String::new(), String::new()),
        };
        let (mut avail, mut total) = (0u64, 0u64);
        // SAFETY: as above; the out-pointers reference live locals.
        let _ = unsafe {
            GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut avail), Some(&mut total), None)
        };
        (label, fs, total, avail)
    }

    pub fn list() -> Vec<Mount> {
        // SAFETY: no arguments; returns a bitmask of drive letters A..Z.
        let mask = unsafe { GetLogicalDrives() };
        let roots: Vec<String> = (0..26u8)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| format!("{}:\\", (b'A' + i) as char))
            .filter(|root| {
                let w = wide(root);
                // SAFETY: `w` is a NUL-terminated drive root path.
                unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) == DRIVE_REMOTE }
            })
            .collect();

        // Start every query at once so the total wait is bounded by one timeout.
        let pending: Vec<_> = roots
            .iter()
            .map(|root| {
                let root = root.clone();
                std::thread::spawn(move || with_timeout(STAT_TIMEOUT, move || query(root)))
            })
            .collect();

        roots
            .into_iter()
            .zip(pending)
            .map(|(root, handle)| {
                let (label, fs, total, avail) = handle
                    .join()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| (String::new(), String::new(), 0, 0));
                Mount {
                    name: label,
                    mount_point: root,
                    file_system: fs,
                    total_bytes: total,
                    available_bytes: avail,
                    removable: false,
                    network: true,
                }
            })
            .collect()
    }
}

/// Other platforms: no network-share enumeration.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod network {
    pub fn list() -> Vec<super::Mount> {
        Vec::new()
    }
}

/// Format a byte count as a human-readable string with binary units (KiB, MiB,
/// …) shown as KB/MB/GB for brevity, matching what users expect from disk
/// tools.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_scales_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(10 * 1024), "10.0 KB");
        assert_eq!(format_bytes(100 * 1024), "100 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.00 GB");
    }

    #[test]
    fn used_fraction_is_bounded_and_sane() {
        let m = Mount {
            name: "x".into(),
            mount_point: "/".into(),
            file_system: "apfs".into(),
            total_bytes: 100,
            available_bytes: 25,
            removable: false,
            network: false,
        };
        assert_eq!(m.used_bytes(), 75);
        assert!((m.used_fraction() - 0.75).abs() < 1e-6);

        // Degenerate: unknown total → zero fraction, no panic.
        let z = Mount {
            total_bytes: 0,
            available_bytes: 0,
            ..Mount {
                name: String::new(),
                mount_point: String::new(),
                file_system: String::new(),
                total_bytes: 0,
                available_bytes: 0,
                removable: false,
                network: false,
            }
        };
        assert_eq!(z.used_fraction(), 0.0);
    }

    #[test]
    fn network_fs_detection() {
        for fs in [
            "nfs",
            "NFS4",
            "smbfs",
            "cifs",
            "afpfs",
            "webdav",
            "fuse.sshfs",
        ] {
            assert!(is_network_fs(fs), "{fs} should be network");
        }
        for fs in ["apfs", "ntfs", "ext4", "btrfs", "fuse.portal", ""] {
            assert!(!is_network_fs(fs), "{fs} should be local");
        }
    }

    #[test]
    fn strip_user_removes_userinfo_only() {
        assert_eq!(strip_user("//alice@nas.local/Share"), "//nas.local/Share");
        assert_eq!(strip_user("//nas.local/Share"), "//nas.local/Share");
        assert_eq!(strip_user("//nas/a@b"), "//nas/a@b");
        assert_eq!(strip_user("server:/export"), "server:/export");
        assert_eq!(strip_user("/"), "/");
    }

    #[test]
    fn local_listing_excludes_network_shares() {
        assert!(list(false).iter().all(|m| m.removable || !m.network));
    }

    #[test]
    fn removable_mounts_survive_network_filter() {
        let mount = |removable, network| Mount {
            name: String::new(),
            mount_point: String::new(),
            file_system: String::new(),
            total_bytes: 0,
            available_bytes: 0,
            removable,
            network,
        };
        assert!(keep_without_network(&mount(false, false)));
        assert!(keep_without_network(&mount(true, false)));
        assert!(keep_without_network(&mount(true, true)));
        assert!(!keep_without_network(&mount(false, true)));
    }

    #[test]
    fn listing_has_no_duplicate_mount_points() {
        // Merging OS-reported removable volumes with sysinfo's list must not
        // produce duplicate rows for the same mount point.
        for include_network in [false, true] {
            let mut points: Vec<String> = list(include_network)
                .into_iter()
                .map(|m| m.mount_point)
                .collect();
            points.sort();
            let total = points.len();
            points.dedup();
            assert_eq!(
                total,
                points.len(),
                "duplicate mount points with include_network={include_network}"
            );
        }
    }

    /// On macOS, every entry returned by the removable-volume recovery path is
    /// a browsable (`!MNT_DONTBROWSE`) local removable disk: tagged removable,
    /// never network. This is the path that recovers drives sysinfo omits.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_local_removable_entries_are_removable_and_local() {
        for m in super::network::local_removable() {
            assert!(m.removable, "{} should be removable", m.mount_point);
            assert!(!m.network, "{} should not be network", m.mount_point);
            // Browsable removables live under /Volumes, like Finder shows.
            assert!(
                m.mount_point.starts_with("/Volumes/"),
                "unexpected removable mount point: {}",
                m.mount_point
            );
        }
    }

    /// The macOS share predicate: recognized network filesystems are always
    /// treated as shares (even when flagged `nobrowse`); unknown types count
    /// only when non-local and browsable; local volumes are never shares.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_network_share_predicate() {
        use super::network::is_network_share;
        // SMB/NFS/AFP: shares regardless of the browse flag.
        assert!(is_network_share("smbfs", true, false));
        assert!(
            is_network_share("smbfs", true, true),
            "nobrowse SMB still a share"
        );
        assert!(
            is_network_share("nfs", false, false),
            "network fs even if flagged local"
        );
        assert!(is_network_share("afpfs", true, false));
        assert!(is_network_share("webdav", true, false));
        // Unknown fstype: a share only when non-local and browsable.
        assert!(is_network_share("myproto", true, false));
        assert!(
            !is_network_share("myproto", true, true),
            "nobrowse unknown is not shown"
        );
        assert!(
            !is_network_share("myproto", false, false),
            "local unknown is not a share"
        );
        // Local volumes are never network shares.
        assert!(!is_network_share("apfs", false, false));
        assert!(!is_network_share("apfs", false, true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_hides_data_volume_but_keeps_root() {
        assert!(is_hidden_system_volume("/System/Volumes/Data"));
        assert!(is_hidden_system_volume("/System/Volumes/Data/home"));
        assert!(!is_hidden_system_volume("/"));
        assert!(!is_hidden_system_volume("/Volumes/External"));

        for include_network in [false, true] {
            let listed = list(include_network);
            assert!(
                listed.iter().any(|m| m.mount_point == "/"),
                "root volume should always be listed"
            );
            assert!(
                !listed
                    .iter()
                    .any(|m| m.mount_point == "/System/Volumes/Data"),
                "the firmlinked Data volume should be hidden"
            );
        }
    }
}
