//! Behaviour tests for a single process: round trips, isolation, corruption, index, gc.

mod common;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{TempDir, payload};
use ts_r_cache::{Cache, HEADER_LEN, Key, Options, Usage};

ts_r_testing::cap_memory!();

/// Entries under 16 KiB are packed, loose ones from 4 KiB are mapped.
fn opts() -> Options {
    Options {
        pack_threshold: 16 * 1024,
        mmap_threshold: 4096,
        ..Options::default()
    }
}

/// Everything loose.
fn loose_opts() -> Options {
    Options {
        pack_threshold: 0,
        ..opts()
    }
}

fn open(dir: &TempDir) -> Cache {
    Cache::with_options(dir.path(), "test", 1, opts()).unwrap()
}

fn open_loose(dir: &TempDir) -> Cache {
    Cache::with_options(dir.path(), "test", 1, loose_opts()).unwrap()
}

fn entry_path(cache: &Cache, key: &Key) -> PathBuf {
    let hex = key.to_hex();
    cache.dir().join("objects").join(&hex[..2]).join(&hex[2..])
}

fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir.join("packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        // Segments only, not their writers' `.lock` files.
        .filter(|p| p.extension().is_some_and(|x| x == "open" || x == "pack"))
        .collect();
    v.sort();
    v
}

fn key(i: u64) -> Key {
    Key::of(&i.to_le_bytes())
}

#[test]
fn round_trip_packed_and_loose() {
    let dir = TempDir::new("roundtrip");
    let small = payload(1, 100);
    let large = payload(2, 100_000);
    {
        let cache = open(&dir);
        cache.put(&key(1), &small).unwrap();
        cache.put(&key(2), &large).unwrap();
        cache.put(&key(3), b"").unwrap();
        // Readable by the writer before it flushes.
        assert_eq!(&cache.get(&key(1)).unwrap()[..], &small[..]);
        let s = cache.stats();
        assert_eq!((s.puts, s.bytes_written, s.hits), (3, 100_100, 1));
    }
    let cache = open(&dir);
    let a = cache.get(&key(1)).unwrap();
    assert_eq!(&a[..], &small[..]);
    assert!(
        a.is_mapped(),
        "flushed packed entries are served from the segment map"
    );
    let b = cache.get(&key(2)).unwrap();
    assert_eq!(&b[..], &large[..]);
    assert!(b.is_mapped());
    // Payloads start at a 128-byte boundary so index-based data can be read in place.
    assert_eq!(a.as_ptr() as usize % HEADER_LEN, 0);
    assert_eq!(b.as_ptr() as usize % HEADER_LEN, 0);
    assert_eq!(cache.get(&key(3)).unwrap().len(), 0);
    assert!(cache.get(&key(4)).is_none());
    assert!(cache.meta(&key(1)).unwrap().packed);
    assert!(!cache.meta(&key(2)).unwrap().packed);

    let s = cache.stats();
    assert_eq!((s.hits, s.misses, s.corrupt), (3, 1, 0));
    assert_eq!(s.bytes_read, 100_100);
}

#[test]
fn overwrite_replaces_value() {
    let dir = TempDir::new("overwrite");
    for cache in [open(&dir), open_loose(&dir)] {
        cache.put(&key(1), b"one").unwrap();
        let held = cache.get(&key(1)).unwrap();
        cache.put(&key(1), b"two").unwrap();
        assert_eq!(
            &held[..],
            b"one",
            "an existing Blob is unaffected by a later write"
        );
        assert_eq!(&cache.get(&key(1)).unwrap()[..], b"two");
        cache.flush().unwrap();
        assert_eq!(&cache.get(&key(1)).unwrap()[..], b"two");
    }
    assert_eq!(&open(&dir).get(&key(1)).unwrap()[..], b"two");
}

#[test]
fn namespaces_and_versions_are_isolated() {
    let dir = TempDir::new("isolation");
    let root = dir.path();
    let v1 = Cache::with_options(root, "build", 1, loose_opts()).unwrap();
    let v2 = Cache::with_options(root, "build", 2, loose_opts()).unwrap();
    let other = Cache::with_options(root, "lint", 1, loose_opts()).unwrap();
    v1.put(&key(1), b"v1 data").unwrap();
    assert!(v2.get(&key(1)).is_none());
    assert!(other.get(&key(1)).is_none());
    assert_eq!(&v1.get(&key(1)).unwrap()[..], b"v1 data");

    // Even an entry file planted in the wrong version's directory is rejected (and deleted).
    let src = entry_path(&v1, &key(1));
    let dst = entry_path(&v2, &key(1));
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::copy(&src, &dst).unwrap();
    assert!(v2.get(&key(1)).is_none());
    assert_eq!(v2.stats().corrupt, 1);
    assert!(!dst.exists());

    // Packed data is isolated the same way.
    let p1 = Cache::with_options(root, "ast", 1, opts()).unwrap();
    p1.put(&key(9), b"packed").unwrap();
    p1.flush().unwrap();
    assert!(
        Cache::with_options(root, "ast", 2, opts())
            .unwrap()
            .get(&key(9))
            .is_none()
    );
}

#[test]
fn invalid_namespaces_are_rejected() {
    let dir = TempDir::new("badns");
    for ns in ["", "Build", "../x", "a/b", "bin", "tmp", &"x".repeat(65)] {
        assert!(
            Cache::open_in(dir.path(), ns, 1).is_err(),
            "{ns:?} should be rejected"
        );
    }
}

type Damage = fn(&mut Vec<u8>);

/// Every way a loose entry file can be damaged yields a miss, deletes the file, and a later
/// put works again.
#[test]
fn corrupt_loose_entries_are_misses_and_deleted() {
    let cases: &[(&str, usize, Damage)] = &[
        ("flip payload byte (heap)", 100, |f| {
            *f.last_mut().unwrap() ^= 1
        }),
        ("flip payload byte (mapped)", 50_000, |f| {
            f[HEADER_LEN + 7] ^= 0x80
        }),
        ("flip checksum byte", 100, |f| f[60] ^= 1),
        ("bad magic", 100, |f| f[0] = b'Z'),
        ("truncated payload", 100, |f| f.truncate(f.len() - 1)),
        ("truncated header", 100, |f| f.truncate(40)),
        ("empty file", 100, |f| f.clear()),
        ("extra trailing bytes", 100, |f| {
            f.extend_from_slice(b"junk")
        }),
        ("garbage", 100, |f| *f = vec![0xAB; 5000]),
    ];
    let dir = TempDir::new("corrupt-loose");
    let cache = open_loose(&dir);
    for (i, (name, len, damage)) in cases.iter().enumerate() {
        let k = key(i as u64);
        let data = payload(i as u64, *len);
        cache.put(&k, &data).unwrap();
        let path = entry_path(&cache, &k);
        let mut bytes = fs::read(&path).unwrap();
        damage(&mut bytes);
        fs::write(&path, &bytes).unwrap();

        let before = cache.stats().corrupt;
        assert!(cache.get(&k).is_none(), "{name}: should be a miss");
        assert_eq!(
            cache.stats().corrupt,
            before + 1,
            "{name}: should count as corrupt"
        );
        assert!(!path.exists(), "{name}: corrupt file should be deleted");
        cache.put(&k, &data).unwrap();
        assert_eq!(
            &cache.get(&k).unwrap()[..],
            &data[..],
            "{name}: re-put should work"
        );
    }
}

/// Damage inside a pack segment yields a miss and drops the entry from the index; the other
/// entries in the segment are unaffected and gc reclaims the space.
#[test]
fn corrupt_packed_entries_are_misses_and_dropped() {
    let cases: &[(&str, Damage)] = &[
        ("flip payload byte", |f| f[HEADER_LEN + 3] ^= 1),
        ("flip checksum byte", |f| f[60] ^= 1),
        ("bad magic", |f| f[0] = b'Z'),
        ("wrong length", |f| f[16] ^= 4),
        ("segment truncated", |f| f.truncate(HEADER_LEN + 10)),
        ("segment emptied", |f| f.clear()),
    ];
    for (name, damage) in cases {
        let dir = TempDir::new("corrupt-packed");
        let data = payload(7, 500);
        {
            let c = open(&dir);
            c.put(&key(0), &data).unwrap(); // at offset 0: the one we damage
            c.put(&key(1), &payload(8, 500)).unwrap();
        }
        let ns = dir.path().join("test").join("v1");
        let seg = segments(&ns).pop().unwrap();
        let mut bytes = fs::read(&seg).unwrap();
        damage(&mut bytes);
        fs::write(&seg, &bytes).unwrap();

        let cache = open(&dir);
        assert!(cache.get(&key(0)).is_none(), "{name}: should be a miss");
        assert_eq!(cache.stats().corrupt, 1, "{name}: should count as corrupt");
        assert!(!cache.contains(&key(0)), "{name}: dropped from the index");
        let truncated = bytes.len() < 1000;
        assert_eq!(
            cache.get(&key(1)).is_some(),
            !truncated,
            "{name}: neighbour"
        );
        cache.put(&key(0), &data).unwrap();
        assert_eq!(
            &cache.get(&key(0)).unwrap()[..],
            &data[..],
            "{name}: re-put should work"
        );
        cache.flush().unwrap();
        let report = cache.gc(u64::MAX).unwrap();
        assert_eq!(
            report.after.entries,
            if truncated { 1 } else { 2 },
            "{name}"
        );
        drop(cache);
        assert_eq!(
            &open(&dir).get(&key(0)).unwrap()[..],
            &data[..],
            "{name}: after gc"
        );
    }
}

#[test]
fn entry_under_wrong_key_is_rejected() {
    let dir = TempDir::new("wrongkey");
    let cache = open_loose(&dir);
    cache.put(&key(1), b"one").unwrap();
    let dst = entry_path(&cache, &key(2));
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::copy(entry_path(&cache, &key(1)), &dst).unwrap();
    assert!(cache.get(&key(2)).is_none());
    assert_eq!(cache.stats().corrupt, 1);
}

/// The index is an accelerator: if it is destroyed, loose entries stay readable and gc
/// rebuilds everything, including packed entries, from the self-describing files.
#[test]
fn corrupt_index_and_journal_are_ignored_then_rebuilt() {
    let dir = TempDir::new("badindex");
    {
        let cache = open(&dir);
        for i in 0..20 {
            // Even keys packed, odd keys loose.
            let len = if i % 2 == 0 { 10 } else { 20_000 };
            cache.put(&key(i), &payload(i, len)).unwrap();
        }
        cache.flush().unwrap();
        cache.compact().unwrap();
    }
    let ns = dir.path().join("test").join("v1");
    fs::write(ns.join("index"), b"not an index").unwrap();
    fs::write(ns.join("journal"), vec![7u8; 1000]).unwrap();

    let cache = open(&dir);
    assert_eq!(
        cache.usage().entries,
        0,
        "unreadable index is treated as empty"
    );
    assert_eq!(
        &cache.get(&key(3)).unwrap()[..],
        &payload(3, 20_000)[..],
        "loose still readable"
    );
    assert!(
        cache.get(&key(2)).is_none(),
        "packed entries need the index"
    );
    let report = cache.gc(u64::MAX).unwrap();
    assert_eq!(
        report.before.entries, 20,
        "gc recovers packed entries from the segment"
    );
    assert_eq!(report.evicted, 0);
    assert_eq!(cache.usage().entries, 20, "gc rebuilt the index");
    for i in 0..20 {
        let len = if i % 2 == 0 { 10 } else { 20_000 };
        assert_eq!(
            &cache.get(&key(i)).unwrap()[..],
            &payload(i, len)[..],
            "key {i}"
        );
    }
}

#[test]
fn crashed_writer_segment_is_recovered_by_gc() {
    let dir = TempDir::new("crashed");
    let value = |i: u64| payload(i, 10_000);
    {
        let cache = open(&dir);
        // More than the 256 KiB write buffer, so most of it reaches the file.
        for i in 0..40 {
            cache.put(&key(i), &value(i)).unwrap();
        }
        // Simulate a crash: no flush, so no journal records and the segment stays `.open`.
        std::mem::forget(cache);
    }
    // The forgotten handle still holds its segment lock (a real crash would release it), so
    // gc must leave that file alone. A copy under another id stands in for the crashed file.
    let ns = dir.path().join("test").join("v1");
    let seg = segments(&ns).pop().unwrap();
    assert_eq!(seg.extension().unwrap(), "open");
    fs::copy(&seg, ns.join("packs").join("00000000000000aa.open")).unwrap();

    let cache = open(&dir);
    assert!(cache.get(&key(0)).is_none(), "not in the index");
    let report = cache.gc(u64::MAX).unwrap();
    assert!(
        report.before.entries >= 20,
        "recovered {}",
        report.before.entries
    );
    assert!(seg.exists(), "a locked segment is never touched");
    assert!(
        ns.join("packs").join("00000000000000aa.pack").exists(),
        "recovered and published"
    );
    drop(cache);
    let cache = open(&dir);
    let mut found = 0;
    for i in 0..40 {
        if let Some(b) = cache.get(&key(i)) {
            assert_eq!(&b[..], &value(i)[..]);
            found += 1;
        }
    }
    assert_eq!(found, report.before.entries);
}

/// A crashed writer can leave a key twice in one segment (put, put again), and two crashed
/// segments can share keys. Recovery must index each key once: a snapshot with a duplicate key
/// is invalid, which would lose the whole index.
#[test]
fn recovery_indexes_each_key_once() {
    let dir = TempDir::new("crashed-dup");
    let value = |i: u64| payload(i, 10_000);
    {
        let cache = open(&dir);
        cache.put(&key(0), b"first").unwrap();
        cache.put(&key(0), b"second").unwrap();
        // Past the 256 KiB write buffer, so the duplicate reaches the file.
        for i in 1..40 {
            cache.put(&key(i), &value(i)).unwrap();
        }
        std::mem::forget(cache);
    }
    // As in `crashed_writer_segment_is_recovered_by_gc`, copies stand in for crashed files.
    let ns = dir.path().join("test").join("v1");
    let seg = segments(&ns).pop().unwrap();
    fs::copy(&seg, ns.join("packs").join("00000000000000aa.open")).unwrap();
    fs::copy(&seg, ns.join("packs").join("00000000000000bb.open")).unwrap();

    let cache = open(&dir);
    let report = cache.gc(u64::MAX).unwrap();
    assert!(report.before.entries >= 20, "recovered {:?}", report.before);
    drop(cache);
    let cache = open(&dir);
    assert_eq!(cache.usage().entries, report.after.entries);
    assert_eq!(
        &cache.get(&key(0)).unwrap()[..],
        b"second",
        "the later put wins"
    );
    assert_eq!(&cache.get(&key(1)).unwrap()[..], &value(1)[..]);
}

/// A failed append (disk full) can leave part of a record at the end of the journal. Records
/// appended after it are no longer on an 80-byte boundary and must still be read.
#[test]
fn journal_records_after_a_torn_append_are_kept() {
    let dir = TempDir::new("torn-journal");
    let small = |i: u64| payload(i, 100);
    {
        let cache = open(&dir);
        cache.put(&key(1), &small(1)).unwrap();
    }
    let journal = dir.path().join("test").join("v1").join("journal");
    let mut f = File::options().append(true).open(&journal).unwrap();
    std::io::Write::write_all(&mut f, &[0x42; 40]).unwrap();
    drop(f);
    {
        let cache = open(&dir);
        cache.put(&key(2), &small(2)).unwrap();
    }
    assert_eq!(fs::metadata(&journal).unwrap().len(), 80 + 40 + 80);
    let cache = open(&dir);
    assert_eq!(&cache.get(&key(1)).unwrap()[..], &small(1)[..]);
    assert_eq!(&cache.get(&key(2)).unwrap()[..], &small(2)[..]);
    // Compaction folds the misaligned record into the snapshot too.
    assert!(cache.compact().unwrap());
    drop(cache);
    assert_eq!(&open(&dir).get(&key(2)).unwrap()[..], &small(2)[..]);
}

#[test]
fn journal_compacts_into_snapshot() {
    let dir = TempDir::new("compact");
    let opts = Options {
        compact_journal_bytes: 80 * 16,
        ..opts()
    };
    {
        let cache = Cache::with_options(dir.path(), "test", 1, opts.clone()).unwrap();
        for i in 0..50 {
            cache.put(&key(i), &payload(i, 10)).unwrap();
        }
        cache.remove(&key(0)).unwrap();
    }
    let ns = dir.path().join("test").join("v1");
    assert!(ns.join("index").is_file());
    assert_eq!(
        fs::metadata(ns.join("journal")).unwrap().len(),
        0,
        "journal folded into snapshot"
    );
    let cache = Cache::with_options(dir.path(), "test", 1, opts).unwrap();
    let usage = cache.usage();
    assert_eq!(usage.entries, 49);
    assert_eq!(usage.bytes, 49 * (HEADER_LEN as u64 + 10));
    assert!(cache.contains(&key(7)));
    assert!(!cache.contains(&key(0)));
    assert_eq!(cache.meta(&key(7)).unwrap().size, HEADER_LEN as u64 + 10);
    assert!(cache.get(&key(0)).is_none());
    assert!(cache.get(&key(49)).is_some());
}

#[test]
fn other_handles_see_writes_after_flush() {
    let dir = TempDir::new("visibility");
    let a = open(&dir);
    let b = open(&dir);
    assert!(b.get(&key(1)).is_none());
    a.put(&key(1), b"x").unwrap();
    assert!(a.contains(&key(1)));
    a.flush().unwrap();
    std::thread::sleep(Duration::from_millis(60)); // past the refresh rate limit
    assert_eq!(
        &b.get(&key(1)).unwrap()[..],
        b"x",
        "b refreshes its index on a miss"
    );
    assert!(a.remove(&key(1)).unwrap());
    assert!(!a.contains(&key(1)));
    assert!(a.get(&key(1)).is_none());
    assert!(!a.remove(&key(1)).unwrap());
}

fn clock_1000() -> u64 {
    1000
}
fn clock_2000() -> u64 {
    2000
}
fn clock_9000() -> u64 {
    9000
}

#[test]
fn gc_evicts_least_recently_used_first() {
    for (label, base) in [("packed", opts()), ("loose", loose_opts())] {
        let dir = TempDir::new("lru");
        let with_clock = |clock| {
            Cache::with_options(
                dir.path(),
                "test",
                1,
                Options {
                    clock,
                    ..base.clone()
                },
            )
            .unwrap()
        };
        let entry_size = (HEADER_LEN + 1000) as u64;
        {
            let old = with_clock(clock_1000);
            for i in 0..5 {
                old.put(&key(i), &payload(i, 1000)).unwrap();
            }
        }
        {
            let newer = with_clock(clock_2000);
            for i in 5..10 {
                newer.put(&key(i), &payload(i, 1000)).unwrap();
            }
        }
        {
            // Reading key 0 much later refreshes it.
            let reader = with_clock(clock_9000);
            assert!(reader.get(&key(0)).is_some());
        }
        let cache = with_clock(clock_9000);
        let report = cache.gc(6 * entry_size).unwrap();
        assert_eq!(report.before.entries, 10, "{label}");
        assert_eq!(report.evicted, 4, "{label}");
        assert_eq!(
            report.after,
            Usage {
                entries: 6,
                bytes: 6 * entry_size
            },
            "{label}"
        );
        drop(cache);
        let cache = with_clock(clock_9000);
        for i in 1..5 {
            assert!(
                cache.get(&key(i)).is_none(),
                "{label}: key {i} was least recently used"
            );
        }
        for i in [0, 5, 6, 7, 8, 9] {
            let got = cache.get(&key(i));
            assert_eq!(
                got.as_deref(),
                Some(&payload(i, 1000)[..]),
                "{label}: key {i} survives"
            );
        }
        assert_eq!(cache.usage().entries, 6, "{label}");
    }
}

#[test]
fn gc_rewrites_dead_and_small_segments() {
    let dir = TempDir::new("rewrite");
    // Eight flushes = eight small segments.
    for round in 0..8u64 {
        let c = open(&dir);
        for i in 0..10 {
            c.put(&key(round * 10 + i), &payload(round * 10 + i, 300))
                .unwrap();
        }
    }
    let ns = dir.path().join("test").join("v1");
    assert_eq!(segments(&ns).len(), 8);
    let cache = open(&dir);
    for i in 0..30 {
        cache.remove(&key(i)).unwrap();
    }
    let report = cache.gc(u64::MAX).unwrap();
    assert_eq!(report.after.entries, 50);
    assert_eq!(report.segments_rewritten, 5);
    assert_eq!(
        segments(&ns).len(),
        1,
        "three dead segments deleted, five small ones merged"
    );
    drop(cache);
    let cache = open(&dir);
    for i in 0..80 {
        let got = cache.get(&key(i));
        if i < 30 {
            assert!(got.is_none());
        } else {
            assert_eq!(got.as_deref(), Some(&payload(i, 300)[..]), "key {i}");
        }
    }
}

#[test]
fn readers_survive_a_concurrent_rewrite() {
    let dir = TempDir::new("stale-view");
    for round in 0..4u64 {
        let c = open(&dir);
        c.put(&key(round), &payload(round, 300)).unwrap();
    }
    let reader = open(&dir);
    assert!(reader.get(&key(0)).is_some()); // loads the index and maps segment 0
    let held = reader.get(&key(1)).unwrap();
    open(&dir).gc(u64::MAX).unwrap(); // merges all four segments into a new one
    assert_eq!(
        &held[..],
        &payload(1, 300)[..],
        "mapped data outlives the deleted segment"
    );
    for i in 0..4 {
        assert_eq!(
            reader.get(&key(i)).as_deref(),
            Some(&payload(i, 300)[..]),
            "key {i}"
        );
    }
}

#[test]
fn warm_reads_do_not_write_the_journal() {
    let dir = TempDir::new("touch");
    let with_clock =
        |clock| Cache::with_options(dir.path(), "test", 1, Options { clock, ..opts() }).unwrap();
    {
        let c = with_clock(clock_1000);
        c.put(&key(1), b"x").unwrap();
    }
    let journal = dir.path().join("test").join("v1").join("journal");
    let len = fs::metadata(&journal).unwrap().len();
    {
        let c = with_clock(clock_2000); // within TOUCH_GRANULARITY_SECS of the write
        for _ in 0..10 {
            assert!(c.get(&key(1)).is_some());
        }
    }
    assert_eq!(
        fs::metadata(&journal).unwrap().len(),
        len,
        "fresh entries are not touched"
    );
    {
        let c = with_clock(clock_9000);
        for _ in 0..10 {
            assert!(c.get(&key(1)).is_some());
        }
    }
    assert_eq!(
        fs::metadata(&journal).unwrap().len(),
        len + 80,
        "one touch for ten stale reads"
    );
}

#[test]
fn gc_is_skipped_while_another_gc_runs() {
    let dir = TempDir::new("gcbusy");
    let cache = open(&dir);
    let held = File::create(cache.dir().join("gc.lock")).unwrap();
    held.lock().unwrap();
    assert!(cache.gc(0).unwrap().skipped_busy);
    assert!(
        !cache.compact().unwrap(),
        "compaction also yields to a running gc"
    );
    held.unlock().unwrap();
    assert!(!cache.gc(0).unwrap().skipped_busy);
}

#[test]
fn gc_removes_junk_and_abandoned_files() {
    let dir = TempDir::new("junk");
    let cache = open(&dir);
    cache.put(&key(1), b"keep").unwrap();
    let shard = cache.dir().join("objects").join("zz");
    fs::create_dir_all(&shard).unwrap();
    fs::write(shard.join("not-a-key"), b"?").unwrap();
    let old_tmp = cache.dir().join("tmp").join("123-0-0.tmp");
    let new_tmp = cache.dir().join("tmp").join("123-1-0.tmp");
    let garbage_seg = cache.dir().join("packs").join("00000000000000ff.pack");
    fs::write(&old_tmp, b"half written").unwrap();
    fs::write(&new_tmp, b"in progress").unwrap();
    fs::write(&garbage_seg, vec![0x55; 1000]).unwrap();
    // Segment lock files nobody holds: an old one without a segment (a writer that crashed
    // before creating it) is junk; a new one may belong to a writer about to create it.
    let old_lock = cache.dir().join("packs").join("00000000000000ee.lock");
    let new_lock = cache.dir().join("packs").join("00000000000000dd.lock");
    fs::write(&old_lock, b"").unwrap();
    fs::write(&new_lock, b"").unwrap();
    let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
    for old in [&old_tmp, &old_lock] {
        File::options()
            .write(true)
            .open(old)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();
    }

    let report = cache.gc(u64::MAX).unwrap();
    assert_eq!(report.files_removed, 4);
    assert!(!old_lock.exists());
    assert!(
        new_lock.exists(),
        "a recent lock file may belong to a new writer"
    );
    assert!(!old_tmp.exists());
    assert!(
        !garbage_seg.exists(),
        "an unlocked segment with nothing recoverable is removed"
    );
    assert!(
        new_tmp.exists(),
        "a recent temp file may belong to a live writer"
    );
    assert!(cache.get(&key(1)).is_some());
}

#[test]
fn maybe_gc_only_runs_over_budget() {
    let dir = TempDir::new("maybegc");
    let cache = open(&dir);
    for i in 0..10 {
        cache.put(&key(i), &payload(i, 872)).unwrap(); // 1000-byte entries
    }
    assert!(cache.maybe_gc(20_000).unwrap().is_none());
    let report = cache.maybe_gc(5_000).unwrap().unwrap();
    assert_eq!(
        report.after.bytes, 4_000,
        "collects down to 80% of the budget"
    );
}

#[test]
fn survives_cache_dir_deleted_mid_run() {
    let dir = TempDir::new("deleted");
    for cache in [open(&dir), open_loose(&dir)] {
        cache.put(&key(1), b"a").unwrap();
        cache.flush().unwrap();
        fs::remove_dir_all(dir.path().join("test")).unwrap();
        assert!(cache.get(&key(1)).is_none());
        cache.put(&key(2), b"b").unwrap();
        cache.flush().unwrap();
        cache.put(&key(3), b"c").unwrap();
        cache.flush().unwrap();
        assert_eq!(&cache.get(&key(3)).unwrap()[..], b"c");
        cache.gc(u64::MAX).unwrap();
        assert_eq!(&open(&dir).get(&key(3)).unwrap()[..], b"c");
    }
}

#[test]
fn threads_share_one_handle() {
    let dir = TempDir::new("threads");
    let cache = open(&dir);
    std::thread::scope(|s| {
        for t in 0..8u64 {
            let cache = &cache;
            s.spawn(move || {
                for i in 0..400u64 {
                    let k = key(i % 50);
                    // Sizes straddle the pack threshold.
                    let expected = payload(i % 50, 100 + (i % 50) as usize * 500);
                    match cache.get(&k) {
                        Some(b) => assert_eq!(&b[..], &expected[..], "thread {t}"),
                        None => cache.put(&k, &expected).unwrap(),
                    }
                    if i % 97 == 0 {
                        cache.flush().unwrap();
                    }
                }
            });
        }
    });
    assert_eq!(cache.stats().corrupt, 0);
    assert_eq!(cache.usage().entries, 50);
}

/// gc on another thread publishes the handle's segment while puts are landing in it; every
/// put that returned must still be readable afterwards.
#[test]
fn puts_racing_gc_on_one_handle_stay_readable() {
    use std::sync::atomic::{AtomicBool, Ordering};
    // The window is short: each round races a fresh handle (whose first put opens a segment)
    // against back-to-back gcs.
    let (threads, n) = (4u64, 300u64);
    for round in 0..100 {
        let dir = TempDir::new("put-gc-race");
        let cache = open(&dir);
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    cache.gc(u64::MAX).unwrap();
                }
            });
            let putters: Vec<_> = (0..threads)
                .map(|t| {
                    let cache = &cache;
                    s.spawn(move || {
                        for i in 0..n {
                            let k = t * n + i;
                            cache.put(&key(k), &payload(k, 100)).unwrap();
                        }
                    })
                })
                .collect();
            for p in putters {
                p.join().unwrap();
            }
            done.store(true, Ordering::Relaxed);
        });
        cache.flush().unwrap();
        let missing: Vec<u64> = (0..threads * n)
            .filter(|&i| cache.get(&key(i)).is_none())
            .collect();
        assert!(missing.is_empty(), "round {round}: lost {missing:?}");
    }
}
