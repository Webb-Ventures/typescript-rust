//! The per-namespace index: where each entry lives, how big it is and when it was last used.
//!
//! The index has two parts:
//!
//! - `index`: an immutable sorted snapshot, replaced atomically (temp file + rename) by whoever
//!   holds `index.lock` exclusively. Readers memory-map it without locking and binary-search it.
//! - `journal`: an append-only log of fixed-size, individually checksummed records (put, touch,
//!   remove). Writers append their batch with one `O_APPEND` write while holding `index.lock`
//!   *shared*, so appends never block each other. Torn or garbage records fail their checksum
//!   and are skipped. A failed append can leave part of a record behind, putting every later
//!   append off the 80-byte grid, so decoding resynchronizes on the next valid record.
//!
//! Compaction (and `gc`) holds `index.lock` exclusively, folds the journal into a new snapshot,
//! renames it into place and then truncates the journal, so no append can be lost. Folding is
//! idempotent, so a crash between the rename and the truncate is harmless. Readers take no lock:
//! a reader that loads the old snapshot and then the truncated journal sees a stale view and
//! refreshes it on a later miss.
//!
//! Packed entries (see `pack.rs`) are only reachable through the index. Loose entries are also
//! found by path, so for them the index only feeds LRU order and `usage()`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use crate::Key;
use crate::entry::{u32_at, u64_at};

const MAGIC: [u8; 4] = *b"TSRI";
const LAYOUT: u32 = 2;
const HEADER: usize = 64;
const RECORD: usize = 64;
pub(crate) const JOURNAL_RECORD: usize = 80;

pub(crate) const KIND_PUT: u8 = 1;
pub(crate) const KIND_TOUCH: u8 = 2;
pub(crate) const KIND_REMOVE: u8 = 3;

/// Segment id meaning "a loose file under `objects/`".
pub(crate) const LOOSE: u64 = 0;

/// What the index knows about one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryMeta {
    /// Size of the entry in bytes (header + payload).
    pub size: u64,
    /// Last time the entry was written or read, in seconds since the Unix epoch. Reads are
    /// recorded at a coarse granularity (see `TOUCH_GRANULARITY_SECS`).
    pub last_used: u64,
    /// Whether the entry lives in a pack segment (small entries) rather than its own file.
    pub packed: bool,
}

/// Index record: metadata plus location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rec {
    pub size: u64,
    pub last_used: u64,
    /// Pack segment id, or [`LOOSE`].
    pub seg: u64,
    /// Byte offset of the entry header inside the segment (0 for loose entries).
    pub offset: u64,
}

impl Rec {
    pub(crate) fn meta(&self) -> EntryMeta {
        EntryMeta {
            size: self.size,
            last_used: self.last_used,
            packed: self.seg != LOOSE,
        }
    }

    pub(crate) fn same_location(&self, other: &Rec) -> bool {
        self.seg == other.seg && self.offset == other.offset
    }
}

fn encode_rec(out: &mut Vec<u8>, r: &Rec) {
    out.extend_from_slice(&r.size.to_le_bytes());
    out.extend_from_slice(&r.last_used.to_le_bytes());
    out.extend_from_slice(&r.seg.to_le_bytes());
    out.extend_from_slice(&r.offset.to_le_bytes());
}

/// Reads the 32 bytes after the key.
fn decode_rec(b: &[u8]) -> Rec {
    Rec {
        size: u64_at(b, 32),
        last_used: u64_at(b, 40),
        seg: u64_at(b, 48),
        offset: u64_at(b, 56),
    }
}

fn key_of(rec: &[u8]) -> Key {
    let mut k = [0u8; 32];
    k.copy_from_slice(&rec[..32]);
    Key::from_bytes(k)
}

/// A memory-mapped index snapshot. Empty if missing or corrupt.
pub(crate) struct Snapshot {
    map: Option<memmap2::Mmap>,
    count: usize,
}

impl Snapshot {
    pub(crate) fn empty() -> Snapshot {
        Snapshot {
            map: None,
            count: 0,
        }
    }

    pub(crate) fn load(path: &Path) -> Snapshot {
        let Ok(file) = crate::os::retry_busy(|| File::open(path)) else {
            return Snapshot::empty();
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return Snapshot::empty();
        };
        if len < HEADER as u64 {
            return Snapshot::empty();
        }
        // SAFETY: index snapshots are never modified in place: writers create a new file and
        // rename it over the old one, so the inode we map stays immutable while mapped. Only a
        // process outside ts-r truncating the file could invalidate the mapping.
        #[allow(unsafe_code)]
        let map = match unsafe { memmap2::Mmap::map(&file) } {
            Ok(m) => m,
            Err(_) => return Snapshot::empty(),
        };
        match validate(&map) {
            Some(count) => Snapshot {
                map: Some(map),
                count,
            },
            None => Snapshot::empty(),
        }
    }

    fn records(&self) -> &[u8] {
        match &self.map {
            Some(m) => &m[HEADER..HEADER + self.count * RECORD],
            None => &[],
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.count
    }

    pub(crate) fn get(&self, key: &Key) -> Option<Rec> {
        let recs = self.records();
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = &recs[mid * RECORD..(mid + 1) * RECORD];
            match rec[..32].cmp(&key.as_bytes()[..]) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(decode_rec(rec)),
            }
        }
        None
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (Key, Rec)> + '_ {
        self.records()
            .chunks_exact(RECORD)
            .map(|rec| (key_of(rec), decode_rec(rec)))
    }
}

fn validate(map: &[u8]) -> Option<usize> {
    if map.len() < HEADER || map[..4] != MAGIC || u32_at(map, 4) != LAYOUT {
        return None;
    }
    let count = usize::try_from(u64_at(map, 8)).ok()?;
    let end = count.checked_mul(RECORD)?.checked_add(HEADER)?;
    if map.len() != end {
        return None;
    }
    let records = &map[HEADER..end];
    if blake3::hash(records).as_bytes()[..] != map[24..56] {
        return None;
    }
    // Keys must be strictly sorted for binary search to be correct.
    let mut prev: Option<&[u8]> = None;
    for rec in records.chunks_exact(RECORD) {
        if prev.is_some_and(|p| p >= &rec[..32]) {
            return None;
        }
        prev = Some(&rec[..32]);
    }
    Some(count)
}

/// Serialises a snapshot. `entries` must be sorted by key with no duplicates.
pub(crate) fn encode_snapshot(entries: &[(Key, Rec)]) -> Vec<u8> {
    let mut out = vec![0u8; HEADER];
    out.reserve(entries.len() * RECORD);
    let mut total = 0u64;
    for (k, r) in entries {
        out.extend_from_slice(k.as_bytes());
        encode_rec(&mut out, r);
        total += r.size;
    }
    let sum = blake3::hash(&out[HEADER..]);
    out[..4].copy_from_slice(&MAGIC);
    out[4..8].copy_from_slice(&LAYOUT.to_le_bytes());
    out[8..16].copy_from_slice(&(entries.len() as u64).to_le_bytes());
    out[16..24].copy_from_slice(&total.to_le_bytes());
    out[24..56].copy_from_slice(sum.as_bytes());
    out
}

/// One journal record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JournalRecord {
    pub key: Key,
    pub kind: u8,
    pub rec: Rec,
}

impl JournalRecord {
    pub(crate) fn encode(&self) -> [u8; JOURNAL_RECORD] {
        let mut buf = Vec::with_capacity(JOURNAL_RECORD);
        buf.extend_from_slice(self.key.as_bytes());
        encode_rec(&mut buf, &self.rec);
        buf.push(self.kind);
        buf.resize(JOURNAL_RECORD - 4, 0);
        let check = blake3::hash(&buf);
        buf.extend_from_slice(&check.as_bytes()[..4]);
        let mut r = [0u8; JOURNAL_RECORD];
        r.copy_from_slice(&buf);
        r
    }

    fn decode(r: &[u8]) -> Option<JournalRecord> {
        let body = JOURNAL_RECORD - 4;
        if blake3::hash(&r[..body]).as_bytes()[..4] != r[body..] {
            return None;
        }
        let kind = r[64];
        if !(KIND_PUT..=KIND_REMOVE).contains(&kind) {
            return None;
        }
        Some(JournalRecord {
            key: key_of(r),
            kind,
            rec: decode_rec(r),
        })
    }
}

/// Reads every valid record of a journal file, in order, plus the number of bytes read.
/// Missing file = no records.
pub(crate) fn read_journal(path: &Path) -> (Vec<JournalRecord>, u64) {
    let Ok(bytes) = fs::read(path) else {
        return (Vec::new(), 0);
    };
    (decode_journal(&bytes), bytes.len() as u64)
}

pub(crate) fn decode_journal(bytes: &[u8]) -> Vec<JournalRecord> {
    let mut out = Vec::with_capacity(bytes.len() / JOURNAL_RECORD);
    let mut at = 0;
    while let Some(r) = bytes.get(at..at + JOURNAL_RECORD) {
        match JournalRecord::decode(r) {
            Some(rec) => {
                out.push(rec);
                at += JOURNAL_RECORD;
            }
            // Garbage or a partial record: the next valid one may start at any byte.
            None => at += 1,
        }
    }
    out
}

/// Appends records with a single write. Returns the journal size afterwards.
pub(crate) fn append_journal(path: &Path, records: &[JournalRecord]) -> io::Result<u64> {
    let mut buf = Vec::with_capacity(records.len() * JOURNAL_RECORD);
    for r in records {
        buf.extend_from_slice(&r.encode());
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(&buf)?;
    Ok(f.metadata().map(|m| m.len()).unwrap_or(0))
}

/// Journal changes keyed by entry: `None` = removed.
pub(crate) type Overlay = HashMap<Key, Option<Rec>>;

/// Applies `records` in order to `overlay`, which sits on top of `under`.
pub(crate) fn fold_journal(
    records: &[JournalRecord],
    overlay: &mut Overlay,
    under: impl Fn(&Key) -> Option<Rec>,
) {
    for r in records {
        let current = match overlay.get(&r.key) {
            Some(v) => *v,
            None => under(&r.key),
        };
        let next = match r.kind {
            KIND_PUT => Some(r.rec),
            // A touch only refreshes the time of the entry it was read from.
            KIND_TOUCH => match current {
                Some(c) if c.same_location(&r.rec) => Some(Rec {
                    last_used: c.last_used.max(r.rec.last_used),
                    ..c
                }),
                Some(c) => Some(c),
                // A touch proves a loose file exists (its writer may have crashed before
                // flushing). It proves nothing for a packed location, which gc may since have
                // deleted, so it must not resurrect one.
                None => (r.rec.seg == LOOSE).then_some(r.rec),
            },
            // A remove only applies to the location it names, so a remove of a corrupt entry
            // never hides a newer, valid write of the same key.
            _ => match current {
                Some(c) if !c.same_location(&r.rec) => Some(c),
                _ => None,
            },
        };
        overlay.insert(r.key, next);
    }
}

/// Snapshot + overlay merged into one list sorted by key.
pub(crate) fn merged(snapshot: &Snapshot, overlay: &Overlay) -> Vec<(Key, Rec)> {
    let mut out: Vec<(Key, Rec)> = snapshot
        .iter()
        .filter_map(|(k, r)| match overlay.get(&k) {
            Some(Some(o)) => Some((k, *o)),
            Some(None) => None,
            None => Some((k, r)),
        })
        .collect();
    out.extend(overlay.iter().filter_map(|(k, v)| match v {
        Some(r) if snapshot.get(k).is_none() => Some((*k, *r)),
        _ => None,
    }));
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `base` (sorted by key, no duplicates) with `overlay` applied, sorted by key.
pub(crate) fn merged_with(base: &[(Key, Rec)], overlay: &Overlay) -> Vec<(Key, Rec)> {
    let mut out: Vec<(Key, Rec)> = base
        .iter()
        .filter_map(|(k, r)| match overlay.get(k) {
            Some(Some(o)) => Some((*k, *o)),
            Some(None) => None,
            None => Some((*k, *r)),
        })
        .collect();
    let in_base = |k: &Key| base.binary_search_by(|(b, _)| b.cmp(k)).is_ok();
    out.extend(overlay.iter().filter_map(|(k, v)| match v {
        Some(r) if !in_base(k) => Some((*k, *r)),
        _ => None,
    }));
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(size: u64, t: u64, seg: u64) -> Rec {
        Rec {
            size,
            last_used: t,
            seg,
            offset: 128 * size,
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ts-r-index-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn snapshot_round_trip() {
        let dir = tmp("snap");
        let path = dir.join("index");
        let mut entries: Vec<_> = (0..100u64)
            .map(|i| (Key::of(&i.to_le_bytes()), rec(i, i * 2, i % 3)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        fs::write(&path, encode_snapshot(&entries)).unwrap();
        let snap = Snapshot::load(&path);
        assert_eq!(snap.len(), 100);
        for (k, r) in &entries {
            assert_eq!(snap.get(k), Some(*r));
        }
        assert_eq!(snap.get(&Key::of(b"absent")), None);
        // The cache never rewrites a snapshot in place (it renames a new file over it), but
        // this test does: unmap first, Windows refuses to truncate a mapped file.
        drop(snap);

        // A flipped byte makes the whole snapshot invalid (empty), never a panic.
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Snapshot::load(&path).len(), 0);
        fs::write(&path, b"garbage").unwrap();
        assert_eq!(Snapshot::load(&path).len(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn journal_skips_torn_records() {
        let r = JournalRecord {
            key: Key::of(b"a"),
            kind: KIND_PUT,
            rec: rec(10, 5, 7),
        };
        let mut bytes = r.encode().to_vec();
        let mut bad = r.encode();
        bad[33] ^= 1;
        bytes.extend_from_slice(&bad);
        bytes.extend_from_slice(&r.encode()[..20]); // torn tail
        let dir = tmp("journal");
        let path = dir.join("journal");
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read_journal(&path).0, vec![r]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn journal_resyncs_after_a_partial_record() {
        let r = |i: u64| JournalRecord {
            key: Key::of(&i.to_le_bytes()),
            kind: KIND_PUT,
            rec: rec(10, 5, i),
        };
        let mut bytes = r(1).encode().to_vec();
        bytes.extend_from_slice(&r(2).encode()[..40]); // a failed append
        bytes.extend_from_slice(&r(3).encode());
        bytes.extend_from_slice(&r(4).encode());
        assert_eq!(decode_journal(&bytes), vec![r(1), r(3), r(4)]);
    }

    #[test]
    fn fold_applies_in_order() {
        let snap = Snapshot::empty();
        let (a, b) = (Key::of(b"a"), Key::of(b"b"));
        let recs = [
            JournalRecord {
                key: a,
                kind: KIND_PUT,
                rec: rec(10, 1, 5),
            },
            JournalRecord {
                key: a,
                kind: KIND_TOUCH,
                rec: rec(10, 9, 5),
            },
            JournalRecord {
                key: b,
                kind: KIND_PUT,
                rec: rec(20, 2, 5),
            },
            JournalRecord {
                key: b,
                kind: KIND_REMOVE,
                rec: rec(20, 3, 5),
            },
        ];
        let mut overlay = Overlay::new();
        fold_journal(&recs, &mut overlay, |k| snap.get(k));
        assert_eq!(merged(&snap, &overlay), vec![(a, rec(10, 9, 5))]);
    }

    #[test]
    fn remove_of_old_location_keeps_newer_write() {
        let snap = Snapshot::empty();
        let a = Key::of(b"a");
        let recs = [
            JournalRecord {
                key: a,
                kind: KIND_PUT,
                rec: rec(10, 1, 5),
            },
            JournalRecord {
                key: a,
                kind: KIND_PUT,
                rec: rec(10, 2, 6),
            },
            JournalRecord {
                key: a,
                kind: KIND_REMOVE,
                rec: rec(10, 3, 5),
            },
            JournalRecord {
                key: a,
                kind: KIND_TOUCH,
                rec: rec(10, 9, 5),
            },
        ];
        let mut overlay = Overlay::new();
        fold_journal(&recs, &mut overlay, |k| snap.get(k));
        assert_eq!(merged(&snap, &overlay), vec![(a, rec(10, 2, 6))]);
    }
}
