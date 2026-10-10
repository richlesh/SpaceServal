//! Directory scanning for the per-volume disk-usage explorer window.
//!
//! Given a directory, [`scan_dir`] lists its immediate children and computes
//! each one's *size on disk* (allocated space, not logical length). For a file
//! that's its own allocation; for a subdirectory it's the recursive sum of the
//! allocation of everything beneath it. The totals drive the pie chart.
//!
//! Sizes use the real on-disk allocation where the platform exposes it (Unix
//! `st_blocks` × 512), falling back to the logical byte length elsewhere.

use std::path::{Path, PathBuf};

/// Pie-chart viewbox: a square whose center is (`PIE/2`, `PIE/2`) and whose
/// wedge radius is `PIE/2`. The Slint `Path` scales this to the on-screen size.
pub const PIE: f32 = 200.0;

/// Maximum number of individual wedges drawn. Beyond this, the smallest entries
/// are grouped into a single "Other" slice. Keeps very large directories (e.g.
/// WinSxS, with thousands of entries) from rendering as a mass of sub-pixel,
/// border-only slivers and from emitting thousands of `Path` elements.
pub const MAX_WEDGES: usize = 60;

/// One immediate child of the scanned directory.
#[derive(Clone, Debug)]
pub struct Entry {
    /// File or directory name (the final path component).
    pub name: String,
    /// Absolute path to the entry.
    pub path: PathBuf,
    /// Whether the entry is a directory (drillable).
    pub is_dir: bool,
    /// Size on disk in bytes (recursive for directories).
    pub size_bytes: u64,
}

/// Result of scanning a directory: its children (largest first) and the total
/// size on disk of everything directly under it.
#[derive(Clone, Debug, Default)]
pub struct DirScan {
    pub entries: Vec<Entry>,
    pub total_bytes: u64,
}

/// Size on disk of a single file-system node (not recursing into directories).
/// On Unix this is the allocated block count (`st_blocks` is in 512-byte
/// units); elsewhere it falls back to the logical length.
fn size_on_disk(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Identifies the volume a path lives on, used to detect mount-point crossings
/// so a scan of one volume doesn't descend into another mounted underneath it.
///
/// On macOS the kernel firmlinks the read-only System volume (`/`) and the
/// writable Data volume (`/System/Volumes/Data`) into one namespace, so the
/// Data volume's contents appear under `/` as `/Users`, `/Applications`, etc.
/// `stat`'s `st_dev` reports the *same* device for both halves, so it can't
/// tell them apart — walking `/` would then double-count the whole Data volume
/// on top of the System volume. `statfs` *does* distinguish them (its `f_fsid`
/// / mount-on name differ), so on macOS we identify the volume via `statfs`.
///
/// On other Unix platforms there's no firmlink quirk and `st_dev` is correct
/// (and free — we already stat'd the entry). `None` means the platform doesn't
/// expose an id, which disables the boundary check (a no-op).
type VolumeId = u64;

/// The volume id of a path. See [`VolumeId`] for why macOS needs `statfs`
/// rather than the cheaper `st_dev` from an already-fetched `Metadata`.
///
/// macOS firmlinks the read-only System volume (`/`) and the writable Data
/// volume (`/System/Volumes/Data`) into a single user-visible disk ("Macintosh
/// HD"): the Data volume's contents surface under `/` as `/Users`,
/// `/Applications`, etc. We treat that pair as *one* logical volume so a scan
/// of `/` walks into `/Users` and friends (showing the whole disk, like
/// Finder) — but we still never cross into other volumes (external `/Volumes/*`
/// disks, network shares, other APFS volumes), which keep distinct ids. The
/// volume is identified by its mount-on name (`f_mntonname`), with the Data
/// volume mapped onto `/` so the two halves share an id.
#[cfg(target_os = "macos")]
fn volume_id_of_path(path: &Path, _meta: &std::fs::Metadata) -> Option<VolumeId> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `st` is a plain C struct filled in by statfs on success; `c` is
    // a valid NUL-terminated path.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let on = unsafe { std::ffi::CStr::from_ptr(st.f_mntonname.as_ptr()) };
    let on = on.to_bytes();
    // Fold the firmlinked Data volume onto the root so System + Data read as a
    // single logical volume; every other mount keeps its own mount-on name.
    let canonical: &[u8] = if on == b"/System/Volumes/Data" {
        b"/"
    } else {
        on
    };
    Some(hash_bytes(canonical))
}

/// FNV-1a hash of a byte slice, used to fold a volume's mount-on name into a
/// `VolumeId`. Collisions across the handful of mounted volumes are vanishingly
/// unlikely and would at worst miss one boundary, never corrupt data.
#[cfg(target_os = "macos")]
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Non-macOS Unix: `st_dev` from the metadata we already fetched is correct and
/// free (no extra syscall).
#[cfg(all(unix, not(target_os = "macos")))]
fn volume_id_of_path(_path: &Path, meta: &std::fs::Metadata) -> Option<VolumeId> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.dev())
}

/// Platforms without a volume id: boundary check disabled.
#[cfg(not(unix))]
fn volume_id_of_path(_path: &Path, _meta: &std::fs::Metadata) -> Option<VolumeId> {
    None
}

/// Whether `path`/`meta` live on the same volume as `root_vol`. When `root_vol`
/// is `None` (platform without volume ids) everything is treated as same
/// volume, so the mount-boundary check is disabled and behavior is unchanged.
fn same_volume(root_vol: Option<VolumeId>, path: &Path, meta: &std::fs::Metadata) -> bool {
    match (root_vol, volume_id_of_path(path, meta)) {
        (Some(root), Some(id)) => root == id,
        _ => true,
    }
}

/// macOS: paths that back the firmlinked disk and must not be walked directly,
/// because their browsable contents are already surfaced under `/` via
/// firmlinks (e.g. the Data volume's files appear as `/Users`, `/opt`, …).
/// Walking `/System/Volumes/Data` as well would count the entire Data volume a
/// second time. The sibling `/System/Volumes/{VM,Preboot,Update,…}` mounts are
/// hidden system volumes Finder doesn't show, so skip them too. These are
/// reached by descending into `/System` → `/System/Volumes`.
#[cfg(target_os = "macos")]
fn is_firmlink_backing_path(path: &Path) -> bool {
    path.starts_with("/System/Volumes")
}

#[cfg(not(target_os = "macos"))]
fn is_firmlink_backing_path(_path: &Path) -> bool {
    false
}

/// Whether the scanner should descend into the directory at `child`: it must be
/// on the same (logical) volume as `root_vol` and not a macOS firmlink-backing
/// mount whose contents are already counted via firmlinks.
fn should_descend(root_vol: Option<VolumeId>, child: &Path, meta: &std::fs::Metadata) -> bool {
    same_volume(root_vol, child, meta) && !is_firmlink_backing_path(child)
}

/// Recursively total the size on disk of everything under `path`, following no
/// symlinks (so we never double-count or escape the tree) and never crossing a
/// volume boundary: a child directory on a different volume than `root_vol` is
/// counted as its own inode but not descended into. Unreadable entries are
/// skipped rather than failing the whole scan.
///
/// `root_vol` is the volume id of the directory being scanned (`None` on
/// platforms that don't expose it, which disables the boundary check).
fn dir_size(path: &Path, root_vol: Option<VolumeId>) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            // DirEntry::metadata does not traverse symlinks, so a link is
            // counted as the link itself, never the (possibly huge) target.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                // A directory's own inode occupies space regardless of whether
                // we descend into it.
                total = total.saturating_add(size_on_disk(&meta));
                // Only recurse while staying on the same logical volume and not
                // into a macOS firmlink-backing mount (whose contents are
                // already counted via firmlinks). A directory on another volume
                // is a separate mount point; its contents are that volume's
                // usage, not ours.
                let child = entry.path();
                if should_descend(root_vol, &child, &meta) {
                    stack.push(child);
                }
            } else {
                total = total.saturating_add(size_on_disk(&meta));
            }
        }
    }
    total
}

/// Scan `dir`, returning its immediate children with per-entry size on disk,
/// sorted largest first. Directory sizes are computed recursively. Entries that
/// can't be read are skipped. Returns an empty scan if `dir` isn't readable.
#[cfg_attr(not(test), allow(dead_code))]
pub fn scan_dir(dir: &Path) -> DirScan {
    // Collect the final state of the streaming scan, ignoring progress.
    let mut result = DirScan::default();
    scan_dir_streaming(dir, &mut |snapshot| result = snapshot.clone());
    result
}

/// Scan `dir`, invoking `progress` with a growing snapshot as each child's size
/// is resolved — immediately for files, and after each directory's recursive
/// total is computed. This lets the UI display items as their sizes come in
/// rather than waiting for a large volume's full scan.
///
/// Every snapshot is sorted largest-first and carries the running total, so the
/// caller can render it directly. `progress` runs on the calling thread.
pub fn scan_dir_streaming(dir: &Path, progress: &mut dyn FnMut(&DirScan)) {
    let Ok(read) = std::fs::read_dir(dir) else {
        // Report an empty result so the UI can stop showing "scanning".
        progress(&DirScan::default());
        return;
    };

    // The volume the scanned directory lives on. Child directories on a
    // different volume are mount points (or, on macOS, firmlinks into the Data
    // volume); we list them but don't recurse into them, so a scan of `/`
    // doesn't absorb `/Volumes/*` or the firmlinked `/Users`, `/Applications`,
    // … (macOS), or `/mnt`, `/media`, and home-dir mounts (Linux).
    let root_vol = std::fs::metadata(dir)
        .ok()
        .and_then(|m| volume_id_of_path(dir, &m));

    // First pass: enumerate immediate children and seed each with its own inode
    // allocation. Files are already at their final size; directories will grow.
    let mut entries: Vec<Entry> = Vec::new();
    let mut dir_indices: Vec<usize> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let is_dir = meta.is_dir();
        // Only directories we should descend into get their contents summed: on
        // our own logical volume and not a macOS firmlink-backing mount. A
        // directory we don't descend into (another volume's mount point, or the
        // Data-volume backing path) stays in the list at its own inode size.
        if is_dir && should_descend(root_vol, &path, &meta) {
            dir_indices.push(entries.len());
        }
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            is_dir,
            size_bytes: size_on_disk(&meta),
        });
    }

    // Emit the initial snapshot (files final; directories at inode size).
    progress(&snapshot(&entries));
    if dir_indices.is_empty() {
        return;
    }

    // Second pass: compute each subdirectory's recursive size in parallel. A
    // bounded pool of worker threads pulls (index, path) jobs from a shared
    // queue and sends (index, size) back as each finishes, so the slowest
    // directory no longer gates the others. Results are applied in completion
    // order and a snapshot is emitted after each, keeping the live streaming.
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    let jobs: Vec<(usize, PathBuf)> = dir_indices
        .iter()
        .map(|&i| (i, entries[i].path.clone()))
        .collect();
    let total_jobs = jobs.len();
    let queue = Arc::new(Mutex::new(jobs));
    let (tx, rx) = mpsc::channel::<(usize, u64)>();

    // One thread per subdirectory, capped to the machine's parallelism so a
    // directory with thousands of children doesn't spawn thousands of threads.
    let pool_size = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(total_jobs)
        .max(1);

    let mut handles = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            loop {
                // Pop one job; release the lock before the (slow) walk.
                let job = queue.lock().unwrap().pop();
                let Some((index, path)) = job else {
                    break;
                };
                let size = dir_size(&path, root_vol);
                // The receiver may be gone if the caller stopped early; ignore.
                if tx.send((index, size)).is_err() {
                    break;
                }
            }
        }));
    }
    // Drop our own sender so `rx` ends once every worker finishes.
    drop(tx);

    // Apply results as they arrive, emitting a snapshot after each.
    for (index, sub) in rx.iter() {
        entries[index].size_bytes = entries[index].size_bytes.saturating_add(sub);
        progress(&snapshot(&entries));
    }

    for h in handles {
        let _ = h.join();
    }
}

/// Build a sorted (largest-first) snapshot with the running total from the
/// current per-entry sizes.
fn snapshot(entries: &[Entry]) -> DirScan {
    let mut entries = entries.to_vec();
    entries.sort_by_key(|e| std::cmp::Reverse(e.size_bytes));
    let total_bytes = entries
        .iter()
        .map(|e| e.size_bytes)
        .fold(0u64, u64::saturating_add);
    DirScan {
        entries,
        total_bytes,
    }
}

/// A single pie wedge, ready to render and list in the legend.
#[derive(Clone, Debug)]
pub struct Wedge {
    /// Entry name shown in the legend.
    pub name: String,
    /// Absolute path (used to drill in when the entry is a directory).
    pub path: PathBuf,
    /// Whether this entry is a directory (double-click drills in).
    pub is_dir: bool,
    /// Human-readable size on disk, e.g. "1.20 GB".
    pub size_text: String,
    /// Percent of the parent total, e.g. "42%".
    pub percent_text: String,
    /// SVG path `commands` for the wedge, in the `PIE`×`PIE` viewbox.
    pub commands: String,
    /// Fill color as `#rrggbb`.
    pub color: String,
    /// Wedge start angle in degrees, clockwise from 12 o'clock. Together with
    /// `sweep_deg` this lets the UI map a click position back to a wedge.
    pub start_deg: f32,
    /// Wedge angular span in degrees.
    pub sweep_deg: f32,
}

/// Point on the pie circle at `deg` degrees clockwise from 12 o'clock, at the
/// given `radius` from the center. Returned in the `PIE`×`PIE` viewbox.
fn point_on_circle(deg: f32, radius: f32) -> (f32, f32) {
    let c = PIE / 2.0;
    // 12 o'clock is -90° in standard math orientation; clockwise is +.
    let rad = (deg - 90.0).to_radians();
    (c + radius * rad.cos(), c + radius * rad.sin())
}

/// Color for a wedge whose fill hue is its position on the pie: the hue (in
/// degrees) equals the wedge's start angle clockwise from 12 o'clock, so a
/// slice starting at 12 o'clock is red (hue 0), at 3 o'clock green-ish
/// (hue 90), at 6 o'clock cyan (hue 180), at 9 o'clock magenta (hue 270), and
/// so on around the wheel. Returned as `#rrggbb`.
fn wedge_color(hue_deg: f32) -> String {
    let (r, g, b) = hsv_to_rgb(hue_deg, 0.55, 0.85);
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

/// HSV (`h` in degrees, `s`/`v` in 0..=1) to 8-bit RGB.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to8 = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to8(r1), to8(g1), to8(b1))
}

/// SVG `commands` for a filled wedge spanning `[start_deg, start_deg+sweep_deg)`
/// (clockwise from 12 o'clock). A full-circle sweep is drawn as two arcs since
/// a single 360° arc is degenerate.
fn wedge_commands(start_deg: f32, sweep_deg: f32) -> String {
    let c = PIE / 2.0;
    let r = PIE / 2.0;
    if sweep_deg >= 359.999 {
        // Full circle: two half-arcs.
        let (sx, sy) = point_on_circle(0.0, r);
        let (mx, my) = point_on_circle(180.0, r);
        return format!("M {sx} {sy} A {r} {r} 0 1 1 {mx} {my} A {r} {r} 0 1 1 {sx} {sy} Z");
    }
    let (sx, sy) = point_on_circle(start_deg, r);
    let (ex, ey) = point_on_circle(start_deg + sweep_deg, r);
    let large_arc = if sweep_deg > 180.0 { 1 } else { 0 };
    // Center → arc start → arc (clockwise, sweep=1) → back to center.
    format!("M {c} {c} L {sx} {sy} A {r} {r} 0 {large_arc} 1 {ex} {ey} Z")
}

/// Turn a scan into renderable wedges. Entries are already sorted largest
/// first. When the total is zero (empty directory) the result is empty.
pub fn wedges(scan: &DirScan) -> Vec<Wedge> {
    let total = scan.total_bytes;
    if total == 0 || scan.entries.is_empty() {
        return Vec::new();
    }
    // Entries are sorted largest-first. Render at most `MAX_WEDGES` individual
    // slices and roll the remaining long tail into a single "Other" slice, so a
    // directory with thousands of tiny files (e.g. WinSxS) doesn't become a
    // mass of sub-pixel, border-only slivers (and we don't emit thousands of
    // Path elements). Each slice's color is its hue by angular position, so the
    // wheel reads continuously (red at 12 o'clock, around through the hues).
    let shown = scan.entries.len().min(MAX_WEDGES);
    let tail = &scan.entries[shown..];
    let tail_bytes: u64 = tail
        .iter()
        .map(|e| e.size_bytes)
        .fold(0, u64::saturating_add);
    let has_other = !tail.is_empty() && tail_bytes > 0;
    let slice_count = shown + usize::from(has_other);

    let mut out = Vec::with_capacity(slice_count);
    let mut cursor = 0.0f32;
    let pct = |bytes: u64| (bytes as f64 / total as f64 * 100.0).round() as i64;
    let sweep_of = |bytes: u64| (bytes as f64 / total as f64 * 360.0) as f32;

    for e in scan.entries.iter().take(shown) {
        let sweep = sweep_of(e.size_bytes);
        out.push(Wedge {
            name: e.name.clone(),
            path: e.path.clone(),
            is_dir: e.is_dir,
            size_text: crate::mounts::format_bytes(e.size_bytes),
            percent_text: format!("{}%", pct(e.size_bytes)),
            commands: wedge_commands(cursor, sweep),
            color: wedge_color(cursor),
            start_deg: cursor,
            sweep_deg: sweep,
        });
        cursor += sweep;
    }

    if has_other {
        // Fill the remaining angle exactly, avoiding rounding gaps.
        let sweep = (360.0 - cursor).max(0.0);
        out.push(Wedge {
            name: format!("Other ({} items)", tail.len()),
            path: std::path::PathBuf::new(),
            is_dir: false,
            size_text: crate::mounts::format_bytes(tail_bytes),
            percent_text: format!("{}%", pct(tail_bytes)),
            commands: wedge_commands(cursor, sweep),
            color: wedge_color(cursor),
            start_deg: cursor,
            sweep_deg: sweep,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scans_children_with_sizes_sorted_desc() {
        let tmp = std::env::temp_dir().join(format!("ss_scan_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        // A small file and a bigger file inside a subdir.
        fs::write(tmp.join("small.txt"), b"hi").unwrap();
        fs::write(tmp.join("sub").join("big.bin"), vec![0u8; 200_000]).unwrap();

        let scan = scan_dir(&tmp);
        let names: Vec<&str> = scan.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"sub"));
        assert!(names.contains(&"small.txt"));

        // The subdir (containing the 200 KB file) must sort before the tiny file.
        let sub = scan.entries.iter().find(|e| e.name == "sub").unwrap();
        let small = scan.entries.iter().find(|e| e.name == "small.txt").unwrap();
        assert!(sub.is_dir);
        assert!(!small.is_dir);
        assert!(sub.size_bytes > small.size_bytes);
        assert_eq!(scan.entries[0].name, "sub", "largest entry sorts first");
        assert!(scan.total_bytes >= sub.size_bytes);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unreadable_dir_yields_empty_scan() {
        let scan = scan_dir(Path::new("/this/path/does/not/exist/ss"));
        assert!(scan.entries.is_empty());
        assert_eq!(scan.total_bytes, 0);
    }

    #[test]
    fn streaming_emits_progress_and_grows_total() {
        let tmp = std::env::temp_dir().join(format!("ss_stream_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("d1")).unwrap();
        fs::create_dir_all(tmp.join("d2")).unwrap();
        fs::write(tmp.join("f.bin"), vec![0u8; 50_000]).unwrap();
        fs::write(tmp.join("d1").join("a.bin"), vec![0u8; 150_000]).unwrap();
        fs::write(tmp.join("d2").join("b.bin"), vec![0u8; 10_000]).unwrap();

        let mut totals: Vec<u64> = Vec::new();
        let mut last_final = DirScan::default();
        scan_dir_streaming(&tmp, &mut |snap| {
            totals.push(snap.total_bytes);
            last_final = snap.clone();
        });

        // At least an initial snapshot plus one per directory (2).
        assert!(
            totals.len() >= 3,
            "expected multiple progress updates, got {totals:?}"
        );
        // Totals never shrink as directory sizes are added in.
        for w in totals.windows(2) {
            assert!(w[1] >= w[0], "total must not decrease: {totals:?}");
        }
        // The final snapshot's d1 (150 KB file) outweighs d2 (10 KB) and sorts
        // ahead of it; every entry present.
        let names: Vec<&str> = last_final.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"d1") && names.contains(&"d2") && names.contains(&"f.bin"));
        let d1 = last_final.entries.iter().find(|e| e.name == "d1").unwrap();
        let d2 = last_final.entries.iter().find(|e| e.name == "d2").unwrap();
        assert!(d1.size_bytes > d2.size_bytes);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn wedges_cover_full_circle_and_carry_labels() {
        let scan = DirScan {
            entries: vec![
                Entry {
                    name: "a".into(),
                    path: "/a".into(),
                    is_dir: true,
                    size_bytes: 75,
                },
                Entry {
                    name: "b".into(),
                    path: "/b".into(),
                    is_dir: false,
                    size_bytes: 25,
                },
            ],
            total_bytes: 100,
        };
        let ws = wedges(&scan);
        assert_eq!(ws.len(), 2);
        // Sweeps are proportional and together span the full circle.
        let sum: f32 = ws.iter().map(|w| w.sweep_deg).sum();
        assert!(
            (sum - 360.0).abs() < 0.5,
            "sweeps should total ~360°, got {sum}"
        );
        assert!((ws[0].sweep_deg - 270.0).abs() < 0.5);
        assert_eq!(ws[0].start_deg, 0.0);
        assert!((ws[1].start_deg - 270.0).abs() < 0.5);
        // Labels present.
        assert_eq!(ws[0].percent_text, "75%");
        assert!(ws[0].commands.starts_with("M "));
        assert!(ws[0].color.starts_with('#') && ws[0].color.len() == 7);
        assert!(ws[0].is_dir && !ws[1].is_dir);
    }

    #[test]
    fn single_entry_draws_full_circle() {
        let scan = DirScan {
            entries: vec![Entry {
                name: "only".into(),
                path: "/only".into(),
                is_dir: true,
                size_bytes: 10,
            }],
            total_bytes: 10,
        };
        let ws = wedges(&scan);
        assert_eq!(ws.len(), 1);
        assert!((ws[0].sweep_deg - 360.0).abs() < 0.5);
        // Full-circle path uses two arcs.
        assert_eq!(ws[0].commands.matches('A').count(), 2);
    }

    #[test]
    fn empty_scan_has_no_wedges() {
        assert!(wedges(&DirScan::default()).is_empty());
    }

    #[test]
    fn wedge_color_follows_start_angle_hue() {
        // The fill color is HSV with hue = the wedge's start angle. Spot-check
        // the cardinal positions: 0° (12 o'clock) is red (R dominant), 120° is
        // green-dominant, 240° is blue-dominant. Each wedge's `color` must
        // match `wedge_color(start_deg)` exactly, so the pie and legend agree.
        let hues = [
            (0.0f32, (true, false, false)),   // red
            (120.0, (false, true, false)),    // green
            (240.0, (false, false, true)),    // blue
        ];
        for (hue, (r_max, g_max, b_max)) in hues {
            let c = wedge_color(hue);
            assert!(c.starts_with('#') && c.len() == 7, "bad color: {c}");
            let r = u8::from_str_radix(&c[1..3], 16).unwrap();
            let g = u8::from_str_radix(&c[3..5], 16).unwrap();
            let b = u8::from_str_radix(&c[5..7], 16).unwrap();
            if r_max {
                assert!(r >= g && r >= b, "hue {hue} should be red-dominant: {c}");
            }
            if g_max {
                assert!(g >= r && g >= b, "hue {hue} should be green-dominant: {c}");
            }
            if b_max {
                assert!(b >= r && b >= g, "hue {hue} should be blue-dominant: {c}");
            }
        }

        // In a real scan, the first wedge starts at 0° so it is colored by
        // hue 0 (red), and its color equals wedge_color(start_deg).
        let scan = DirScan {
            entries: vec![
                Entry { name: "a".into(), path: "/a".into(), is_dir: true, size_bytes: 50 },
                Entry { name: "b".into(), path: "/b".into(), is_dir: false, size_bytes: 50 },
            ],
            total_bytes: 100,
        };
        let ws = wedges(&scan);
        for w in &ws {
            assert_eq!(
                w.color,
                wedge_color(w.start_deg),
                "wedge color must be its start-angle hue"
            );
        }
        // First wedge begins at 12 o'clock → red-dominant.
        let first = &ws[0];
        let r = u8::from_str_radix(&first.color[1..3], 16).unwrap();
        let g = u8::from_str_radix(&first.color[3..5], 16).unwrap();
        let b = u8::from_str_radix(&first.color[5..7], 16).unwrap();
        assert!(r >= g && r >= b, "first wedge should be red: {}", first.color);
    }

    #[test]
    fn many_entries_group_into_other() {
        // More entries than MAX_WEDGES: expect MAX_WEDGES slices plus one
        // "Other" slice, together spanning the full circle with no gap.
        let count = MAX_WEDGES + 500;
        let entries: Vec<Entry> = (0..count)
            .map(|k| Entry {
                name: format!("e{k}"),
                path: format!("/e{k}").into(),
                is_dir: false,
                // Strictly decreasing so sort order is stable and the tail is
                // the smallest items.
                size_bytes: (count - k) as u64,
            })
            .collect();
        let total = entries.iter().map(|e| e.size_bytes).sum();
        let scan = DirScan {
            entries,
            total_bytes: total,
        };

        let ws = wedges(&scan);
        assert_eq!(ws.len(), MAX_WEDGES + 1, "capped slices + one Other");
        let other = ws.last().unwrap();
        assert!(other.name.starts_with("Other ("), "last slice is Other");
        assert!(!other.is_dir, "Other is not drillable");
        assert_eq!(other.path.as_os_str().len(), 0, "Other has no path");
        // Slices tile the full circle without a visible gap.
        let sum: f32 = ws.iter().map(|w| w.sweep_deg).sum();
        assert!(
            (sum - 360.0).abs() < 0.5,
            "slices should tile 360°, got {sum}"
        );
        // "Other" accounts for the 500 smallest items.
        assert!(other.name.contains("500"));
    }

    #[test]
    fn few_entries_are_not_grouped() {
        // At or below the cap, every entry is its own wedge (no "Other").
        let entries: Vec<Entry> = (0..5)
            .map(|k| Entry {
                name: format!("e{k}"),
                path: format!("/e{k}").into(),
                is_dir: true,
                size_bytes: 10 + k as u64,
            })
            .collect();
        let total = entries.iter().map(|e| e.size_bytes).sum();
        let ws = wedges(&DirScan {
            entries,
            total_bytes: total,
        });
        assert_eq!(ws.len(), 5);
        assert!(ws.iter().all(|w| !w.name.starts_with("Other (")));
    }

    #[test]
    fn parallel_scan_totals_are_correct_and_sorted() {
        // Many subdirectories, each with a distinct, increasing payload, exercise
        // the parallel worker pool. The final snapshot must be independent of
        // completion order: correctly summed and sorted largest-first.
        let tmp = std::env::temp_dir().join(format!("ss_par_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        const N: usize = 24;
        for k in 0..N {
            let d = tmp.join(format!("dir{k:02}"));
            fs::create_dir_all(&d).unwrap();
            // dir{k} holds (k+1) * 20 KB so sizes are strictly increasing.
            fs::write(d.join("payload.bin"), vec![0u8; (k + 1) * 20_000]).unwrap();
        }

        let final_scan = scan_dir(&tmp);
        assert_eq!(final_scan.entries.len(), N);
        // Sorted largest-first: dir23 (biggest) leads, dir00 (smallest) trails.
        assert_eq!(final_scan.entries.first().unwrap().name, "dir23");
        assert_eq!(final_scan.entries.last().unwrap().name, "dir00");
        // Monotonically non-increasing sizes.
        for w in final_scan.entries.windows(2) {
            assert!(w[0].size_bytes >= w[1].size_bytes);
        }
        // Total equals the sum of the parts.
        let sum: u64 = final_scan.entries.iter().map(|e| e.size_bytes).sum();
        assert_eq!(final_scan.total_bytes, sum);
        // Each directory's size includes at least its payload file.
        for k in 0..N {
            let e = final_scan
                .entries
                .iter()
                .find(|e| e.name == format!("dir{k:02}"))
                .unwrap();
            assert!(e.size_bytes >= ((k + 1) * 20_000) as u64);
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn same_volume_disabled_when_no_root_vol() {
        // With no known root volume (e.g. non-Unix), the boundary check is a
        // no-op: everything counts as same-volume.
        let tmp = std::env::temp_dir().join(format!("ss_vol_none_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let meta = fs::metadata(&tmp).unwrap();
        assert!(same_volume(None, &tmp, &meta));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_respects_mount_boundary() {
        // dir_size descends into same-volume subdirectories but not into ones
        // on a different volume. We can't create a real mount point in a unit
        // test, so we drive the boundary directly:
        //
        //   root/
        //     a.bin        (file)
        //     sub/
        //       b.bin      (file, only counted if we descend into `sub`)
        //
        // With root_vol = the tree's real volume, `sub` is descended into and
        // b.bin is counted. With a bogus root_vol that matches nothing, `sub`
        // is treated as a foreign mount: its inode counts, but b.bin does not.
        let tmp = std::env::temp_dir().join(format!("ss_mount_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::write(tmp.join("a.bin"), vec![0u8; 40_000]).unwrap();
        fs::write(tmp.join("sub").join("b.bin"), vec![0u8; 500_000]).unwrap();

        let real_vol = volume_id_of_path(&tmp, &fs::metadata(&tmp).unwrap());
        assert!(real_vol.is_some(), "unix exposes a volume id");
        // A volume id that won't match the tree's real one.
        let bogus_vol = real_vol.map(|d| d ^ 0xFFFF_FFFF);

        let with_descend = dir_size(&tmp, real_vol);
        let without_descend = dir_size(&tmp, bogus_vol);

        // Descending includes the big nested file; not descending excludes it.
        assert!(
            with_descend >= 540_000,
            "same-volume scan should include nested file: {with_descend}"
        );
        assert!(
            without_descend < with_descend,
            "cross-volume scan must not descend into the foreign subdir \
             (with={with_descend}, without={without_descend})"
        );
        // The foreign-mount case still counts the top-level file and the
        // `sub` inode itself (just not its contents).
        assert!(without_descend >= 40_000);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// macOS: the firmlinked System (`/`) and Data (`/System/Volumes/Data`)
    /// halves are one *logical* volume, so a scan of `/` walks into `/Users`,
    /// `/Applications`, … (which live on Data). Those must share `/`'s volume
    /// id. The Data backing mount itself is skipped by `is_firmlink_backing_path`
    /// so its contents aren't counted a second time.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_firmlinked_data_is_same_logical_volume() {
        let root_meta = match fs::metadata("/") {
            Ok(m) => m,
            Err(_) => return,
        };
        let root_vol = volume_id_of_path(Path::new("/"), &root_meta);
        assert!(root_vol.is_some(), "macOS exposes a volume id for /");

        // Firmlinked Data entries read as the same logical volume as `/`, so the
        // scanner descends into them and reports their real sizes.
        for p in ["/System/Volumes/Data", "/Users", "/Applications", "/private"] {
            let path = Path::new(p);
            if let Ok(m) = fs::metadata(path) {
                assert_eq!(
                    volume_id_of_path(path, &m),
                    root_vol,
                    "{p} should fold onto the root logical volume"
                );
            }
        }

        // System-volume paths also match `/` (same mount-on name).
        if let Ok(m) = fs::metadata("/System/Library") {
            assert_eq!(
                volume_id_of_path(Path::new("/System/Library"), &m),
                root_vol,
                "/System/Library is on the System volume, same as /"
            );
        }

        // But the Data *backing* mount is not descended into (its contents are
        // already counted via the firmlinks), so we don't double-count it.
        assert!(is_firmlink_backing_path(Path::new("/System/Volumes/Data")));
        assert!(is_firmlink_backing_path(Path::new("/System/Volumes/Data/Users")));
        assert!(is_firmlink_backing_path(Path::new("/System/Volumes/VM")));
        assert!(!is_firmlink_backing_path(Path::new("/Users")));
        assert!(!is_firmlink_backing_path(Path::new("/System/Library")));

        // A separate real volume (e.g. an external disk) keeps a distinct id and
        // is still never crossed. We can't guarantee one is mounted, so only
        // check when `/Volumes` has a sibling that isn't the root alias.
        // (Covered functionally by `dir_size_respects_mount_boundary`.)
    }
}
