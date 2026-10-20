//! Pack segments: append-only files holding many small entries back to back.
//!
//! Opening one file per entry costs ~10-20 us on macOS, which alone would blow the "warm,
//! nothing changed: <100 ms for 10k files" budget. Small entries are therefore appended to a
//! per-handle segment and located through the index. A warm reader maps each segment once
//! and serves entries as slices of the map.
//!
//! A segment is `packs/<id>.open` while its single writer appends to it, and is renamed to
//! `packs/<id>.pack` when the writer flushes. The writer holds an exclusive lock on a separate
//! file, `packs/<id>.lock`, from before the segment exists until after its records are in
//! the journal; it deletes the lock file when done. The flush order is: write out the data,
//! rename to `.pack`, append the journal records, release (and delete) the lock. A `.pack`
//! segment is never written again, so readers only ever map immutable files and need no locks.
//!
//! The lock is on its own file, not the segment, because Windows file locks are mandatory: a
//! locked segment could not be read, copied or mapped by anyone else.
//!
//! `gc` uses the two states:
//!
//! - an unlocked `.open` segment was left by a writer that crashed before flushing. Entries are
//!   self-describing (key + checksum in the header), so its intact entries are recovered.
//! - a `.pack` segment no live index entry points into holds only removed or evicted entries
//!   and is deleted. (If the index itself was damaged, `gc` recovers these too.)
//! - a locked segment is never touched. Unlinking leaves existing mappings valid.
//! - an unlocked lock file whose segment is published, or gone (and the lock file old), is
//!   junk and deleted.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Key;

/// Entries start at multiples of this inside a segment (same as the entry header length).
pub(crate) const ALIGN: u64 = crate::entry::HEADER_LEN as u64;
/// A writer starts a new segment once its current one reaches this size.
pub(crate) const ROTATE_BYTES: u64 = 64 * 1024 * 1024;
/// An empty, unlocked segment younger than this may belong to a writer that has created it but
/// not locked it yet, so `gc` leaves it alone.
pub(crate) const ORPHAN_AGE: Duration = Duration::from_secs(10 * 60);

/// Path of a flushed segment.
pub(crate) fn segment_path(packs: &Path, id: u64) -> PathBuf {
    packs.join(format!("{id:016x}.pack"))
}

/// Path of a segment that is being written (or whose writer crashed).
pub(crate) fn open_segment_path(packs: &Path, id: u64) -> PathBuf {
    packs.join(format!("{id:016x}.open"))
}

/// Path of the lock file a segment's writer holds.
pub(crate) fn lock_path(packs: &Path, id: u64) -> PathBuf {
    packs.join(format!("{id:016x}.lock"))
}

/// Parses `<16 hex>.lock`.
pub(crate) fn parse_lock_name(name: &str) -> Option<u64> {
    let hex = name.strip_suffix(".lock").filter(|h| h.len() == 16)?;
    u64::from_str_radix(hex, 16)
        .ok()
        .filter(|&id| id != crate::index::LOOSE)
}

/// Parses `<16 hex>.pack` (→ `(id, false)`) or `<16 hex>.open` (→ `(id, true)`).
pub(crate) fn parse_segment_name(name: &str) -> Option<(u64, bool)> {
    let (hex, open) = match (name.strip_suffix(".pack"), name.strip_suffix(".open")) {
        (Some(h), _) => (h, false),
        (_, Some(h)) => (h, true),
        _ => return None,
    };
    if hex.len() != 16 {
        return None;
    }
    let id = u64::from_str_radix(hex, 16)
        .ok()
        .filter(|&id| id != crate::index::LOOSE)?;
    Some((id, open))
}

fn fresh_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.update(&nanos.to_le_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.finalize().as_bytes()[..8]);
    u64::from_le_bytes(b).max(1)
}

/// The segment a handle is currently appending to.
pub(crate) struct SegmentWriter {
    pub id: u64,
    packs: PathBuf,
    out: BufWriter<File>,
    /// `<id>.lock`, locked exclusively for the writer's whole life.
    _lock: File,
    /// Bytes appended so far (buffered included).
    pub len: u64,
}

impl Drop for SegmentWriter {
    /// Deletes the lock file; the lock itself is released when the `_lock` field closes right
    /// after, so no other process can take the lock and then find the file gone.
    fn drop(&mut self) {
        let _ = fs::remove_file(lock_path(&self.packs, self.id));
    }
}

impl std::fmt::Debug for SegmentWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentWriter")
            .field("id", &self.id)
            .field("len", &self.len)
            .finish()
    }
}

impl SegmentWriter {
    pub(crate) fn create(packs: &Path) -> io::Result<SegmentWriter> {
        let mut attempts = 0;
        loop {
            let id = fresh_id();
            // The lock first, so gc never sees this segment unlocked (an empty young `.open`
            // file without a lock is left alone too, see `cache.rs`).
            let lock = match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(lock_path(packs, id))
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 8 => {
                    attempts += 1;
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound && attempts < 8 => {
                    attempts += 1;
                    fs::create_dir_all(packs)?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if let Err(e) = lock.lock() {
                drop(lock);
                let _ = fs::remove_file(lock_path(packs, id));
                return Err(e);
            }
            let file = match OpenOptions::new()
                .read(true)
                .append(true)
                .create_new(true)
                .open(open_segment_path(packs, id))
            {
                Ok(f) => f,
                Err(e) => {
                    let _ = fs::remove_file(lock_path(packs, id));
                    drop(lock);
                    if e.kind() == io::ErrorKind::AlreadyExists && attempts < 8 {
                        attempts += 1;
                        continue;
                    }
                    return Err(e);
                }
            };
            let out = BufWriter::with_capacity(256 * 1024, file);
            return Ok(SegmentWriter {
                id,
                packs: packs.to_path_buf(),
                out,
                _lock: lock,
                len: 0,
            });
        }
    }

    /// Appends one entry (header + payload + padding) and returns its offset.
    pub(crate) fn append(&mut self, header: &[u8], payload: &[u8]) -> io::Result<u64> {
        let offset = self.len;
        let len = (header.len() + payload.len()) as u64;
        let pad = (ALIGN - len % ALIGN) % ALIGN;
        self.out.write_all(header)?;
        self.out.write_all(payload)?;
        self.out.write_all(&[0u8; ALIGN as usize][..pad as usize])?;
        self.len += len + pad;
        Ok(offset)
    }

    /// Writes out buffered data and renames the segment to `.pack`. The lock is held until
    /// the writer is dropped, which must happen only after the journal records are appended.
    /// Nothing may be appended afterwards.
    pub(crate) fn finish(&mut self) -> io::Result<()> {
        self.out.flush()?;
        let (from, to) = (
            open_segment_path(&self.packs, self.id),
            segment_path(&self.packs, self.id),
        );
        crate::os::retry_busy(|| fs::rename(&from, &to))
    }

    /// Reads back an entry from this (still open) segment.
    pub(crate) fn read_at(&mut self, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        self.out.flush()?;
        let len = usize::try_from(len).map_err(|_| io::Error::other("entry too large"))?;
        let mut buf = vec![0u8; len];
        let f = self.out.get_mut();
        // The file is in append mode, so moving the cursor does not affect where writes go.
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Maps a flushed (immutable) segment. Opening one gc deleted (on Windows: or is still
/// deleting) fails with an error `os::is_gone` recognises.
pub(crate) fn map_segment(path: &Path) -> io::Result<Arc<memmap2::Mmap>> {
    let file = crate::os::retry_busy(|| File::open(path))?;
    // SAFETY: only flushed segments are mapped, and a flushed segment is never written again
    // (its writer rotated to a new file before releasing it). gc only unlinks segments, which
    // leaves existing mappings valid. Truncation by a process outside ts-r could raise SIGBUS;
    // AGENTS.md documents that as outside the cache's contract.
    #[allow(unsafe_code)]
    let map = unsafe { memmap2::Mmap::map(&file)? };
    Ok(Arc::new(map))
}

/// Whether some process holds segment `id`'s writer lock (`<id>.lock`). Writers from before
/// lock files locked the `.open` segment itself, so without a lock file that is checked.
pub(crate) fn is_locked(packs: &Path, id: u64) -> bool {
    match lock_state(&lock_path(packs, id)) {
        LockState::Locked => true,
        LockState::Free => false,
        LockState::Missing => lock_state(&open_segment_path(packs, id)) == LockState::Locked,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockState {
    Missing,
    Free,
    Locked,
}

/// Tries to lock `path` (released at once). A file that exists but can't be opened (on
/// Windows: one being deleted) counts as locked.
pub(crate) fn lock_state(path: &Path) -> LockState {
    let f = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LockState::Missing,
        Err(_) => return LockState::Locked,
    };
    match f.try_lock() {
        Ok(()) => LockState::Free,
        Err(_) => LockState::Locked,
    }
}

/// Walks a segment and returns `(key, offset, size)` for every intact entry. Damaged regions
/// are skipped by stepping to the next [`ALIGN`] boundary.
pub(crate) fn scan_entries(seg: &[u8], format_version: u32) -> Vec<(Key, u64, u64)> {
    let mut out = Vec::new();
    let header = crate::entry::HEADER_LEN;
    let mut at = 0usize;
    while at + header <= seg.len() {
        let h = &seg[at..at + header];
        let len = usize::try_from(crate::entry::u64_at(h, 16)).unwrap_or(usize::MAX);
        let end = at.saturating_add(header).saturating_add(len);
        if h[..4] == crate::entry::MAGIC && end <= seg.len() {
            let mut k = [0u8; 32];
            k.copy_from_slice(&h[24..56]);
            let key = Key::from_bytes(k);
            if crate::entry::verify(&seg[at..end], &key, format_version).is_ok() {
                out.push((key, at as u64, (end - at) as u64));
                at = end.next_multiple_of(ALIGN as usize);
                continue;
            }
        }
        at += ALIGN as usize;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_finds_intact_entries_and_skips_damage() {
        let mut seg = Vec::new();
        let mut expect = Vec::new();
        for i in 0..5u64 {
            let key = Key::of(&i.to_le_bytes());
            let payload = vec![i as u8; 100 + i as usize * 70];
            let at = seg.len() as u64;
            seg.extend_from_slice(&crate::entry::encode_header(&key, 3, &payload));
            seg.extend_from_slice(&payload);
            let size = seg.len() as u64 - at;
            seg.resize(seg.len().next_multiple_of(ALIGN as usize), 0);
            expect.push((key, at, size));
        }
        assert_eq!(scan_entries(&seg, 3), expect);
        assert!(scan_entries(&seg, 4).is_empty(), "other format version");
        let second = expect[1].1 as usize;
        seg[second + 130] ^= 1;
        expect.remove(1);
        assert_eq!(scan_entries(&seg, 3), expect);
        seg.truncate(seg.len() - 200);
        expect.pop();
        assert_eq!(scan_entries(&seg, 3), expect);
    }

    #[test]
    fn names_round_trip() {
        let p = segment_path(Path::new("/p"), 0xabc);
        assert_eq!(
            parse_segment_name(p.file_name().unwrap().to_str().unwrap()),
            Some((0xabc, false))
        );
        let p = open_segment_path(Path::new("/p"), 0xabc);
        assert_eq!(
            parse_segment_name(p.file_name().unwrap().to_str().unwrap()),
            Some((0xabc, true))
        );
        assert_eq!(parse_segment_name("0000000000000000.pack"), None);
        assert_eq!(parse_segment_name("xyz.pack"), None);
        assert_eq!(parse_segment_name("0000000000000abc.tmp"), None);
        assert_eq!(parse_segment_name("0000000000000abc.lock"), None);
        let p = lock_path(Path::new("/p"), 0xabc);
        assert_eq!(
            parse_lock_name(p.file_name().unwrap().to_str().unwrap()),
            Some(0xabc)
        );
        assert_eq!(parse_lock_name("0000000000000000.lock"), None);
    }

    #[test]
    fn append_aligns_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("ts-r-pack-test-{}", std::process::id()));
        let mut w = SegmentWriter::create(&dir).unwrap();
        assert!(is_locked(&dir, w.id));
        // The segment itself is not locked: on Windows that would block every other reader.
        assert_eq!(lock_state(&open_segment_path(&dir, w.id)), LockState::Free);
        let a = w.append(&[1u8; 128], b"hello").unwrap();
        let b = w.append(&[2u8; 128], &[7u8; 300]).unwrap();
        assert_eq!((a, b), (0, 256));
        assert_eq!(w.len, 256 + 512);
        assert_eq!(&w.read_at(128, 5).unwrap(), b"hello");
        let c = w.append(&[3u8; 128], b"").unwrap();
        assert_eq!(c, 768);
        w.finish().unwrap();
        let id = w.id;
        let path = segment_path(&dir, id);
        assert!(is_locked(&dir, id), "still locked after the rename");
        drop(w);
        assert!(!is_locked(&dir, id));
        assert!(
            !lock_path(&dir, id).exists(),
            "the writer deletes its lock file"
        );
        assert!(!open_segment_path(&dir, id).exists());
        let map = map_segment(&path).unwrap();
        assert_eq!(map.len(), 896);
        assert_eq!(&map[384..684], &[7u8; 300][..]);
        // Fake headers, so nothing verifies.
        assert!(scan_entries(&map, 1).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
