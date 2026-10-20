//! Several processes reading, writing, compacting and collecting one namespace at once.
//!
//! The test re-runs this test binary as child processes (selecting the `worker` test via an
//! environment variable), so no helper binary is needed.

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{TempDir, payload};
use ts_r_cache::{Cache, Key, Options};

ts_r_testing::cap_memory!();

const ROLE: &str = "TS_R_CACHE_TEST_ROLE";
const ROOT: &str = "TS_R_CACHE_TEST_ROOT";
const KEYS: u64 = 300;

fn value_len(i: u64) -> usize {
    // Every 10th value is large enough to be memory-mapped.
    if i.is_multiple_of(10) {
        100_000 + i as usize
    } else {
        200 + i as usize * 7
    }
}

fn opts() -> Options {
    // Every 10th value is loose (above the pack threshold), the rest packed. A tiny compaction
    // threshold makes index compaction race with appends constantly.
    Options {
        pack_threshold: 64 * 1024,
        mmap_threshold: 64 * 1024,
        compact_journal_bytes: 80 * 32,
        ..Options::default()
    }
}

/// Child-process entry point; a no-op when run normally.
#[test]
fn worker() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let root = std::env::var(ROOT).unwrap();
    let cache = Cache::with_options(root.as_ref(), "shared", 1, opts()).unwrap();
    let deadline = Instant::now() + Duration::from_millis(1500);
    if role == "gc" {
        let (mut runs, mut evicted, mut rewritten) = (0, 0, 0);
        while Instant::now() < deadline {
            // A budget of roughly a third of the data forces constant eviction.
            let r = cache.gc(2_000_000).unwrap();
            if !r.skipped_busy {
                runs += 1;
                evicted += r.evicted;
                rewritten += r.segments_rewritten;
            }
        }
        // The interesting paths really ran while the other processes were reading.
        assert!(
            runs > 0 && evicted > 0 && rewritten > 0,
            "{runs} runs, {evicted} evicted, {rewritten} rewritten"
        );
        return;
    }
    let seed: u64 = role.parse().unwrap();
    let mut x = seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    let mut checked = 0u64;
    while Instant::now() < deadline {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let i = x % KEYS;
        let key = Key::of(&i.to_le_bytes());
        let expected = payload(i, value_len(i));
        match cache.get(&key) {
            Some(blob) => {
                assert!(
                    blob[..] == expected[..],
                    "process {seed} read a wrong value for key {i}"
                );
                checked += 1;
            }
            None => cache.put(&key, &expected).unwrap(),
        }
        if x.is_multiple_of(64) {
            cache.flush().unwrap();
        }
    }
    let s = cache.stats();
    assert_eq!(s.corrupt, 0, "no reader may ever see a partial entry");
    assert_eq!(
        s.io_errors,
        0,
        "process {seed}: unexpected I/O errors; the first: {}",
        cache.first_io_error().unwrap_or("?")
    );
    eprintln!("worker {seed}: {checked} verified reads, {} puts", s.puts);
}

#[test]
fn many_processes_share_one_cache() {
    let dir = TempDir::new("multiprocess");
    let exe = std::env::current_exe().unwrap();
    let roles = ["1", "2", "3", "4", "5", "6", "gc"];
    let children: Vec<_> = roles
        .iter()
        .map(|role| {
            Command::new(&exe)
                .args(["--exact", "worker", "--nocapture", "--test-threads=1"])
                .env(ROLE, role)
                .env(ROOT, dir.path())
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for (role, mut child) in roles.iter().zip(children) {
        let status = child.wait().unwrap();
        assert!(status.success(), "worker {role} failed: {status}");
    }

    // Everything left behind is intact, and a final gc reconciles the index with the files.
    let cache = Cache::with_options(dir.path(), "shared", 1, opts()).unwrap();
    let report = cache.gc(u64::MAX).unwrap();
    assert!(report.before.entries > 0);
    for i in 0..KEYS {
        if let Some(blob) = cache.get(&Key::of(&i.to_le_bytes())) {
            assert!(blob[..] == payload(i, value_len(i))[..]);
        }
    }
    assert_eq!(cache.stats().corrupt, 0);
    assert_eq!(cache.usage().entries, report.after.entries);
    let leftover_tmp = std::fs::read_dir(cache.dir().join("tmp")).unwrap().count();
    assert_eq!(leftover_tmp, 0, "every temp file was renamed into place");
}
