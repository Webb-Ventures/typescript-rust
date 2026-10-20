//! Micro benchmark for the "warm, nothing changed" path (plan target: <100 ms for 10k entries).
//!
//! Run: `cargo run --release -p ts_r_cache --example bench -- [entries] [payload bytes]`
//! Defaults: 10000 entries of 4096 bytes, in a fresh temp dir (removed afterwards).

use std::time::Instant;

use ts_r_cache::{Cache, Key};

// Entry counts and sizes come from the command line.
ts_r_testing::cap_memory!();

fn main() {
    let mut args = std::env::args().skip(1);
    let n: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let size: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(4096);
    let root = std::env::temp_dir().join(format!("ts-r-cache-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let keys: Vec<Key> = (0..n).map(|i| Key::of(&i.to_le_bytes())).collect();
    let payload: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
    println!(
        "ts_r_cache bench: {n} entries x {size} bytes in {}",
        root.display()
    );

    let t = Instant::now();
    {
        let cache = Cache::open_in(&root, "bench", 1).unwrap();
        for k in &keys {
            cache.put(k, &payload).unwrap();
        }
    }
    report("cold: put all + flush", t, n);

    // Each block opens a fresh handle, like a new tool process would.
    let t = Instant::now();
    {
        let cache = Cache::open_in(&root, "bench", 1).unwrap();
        let mut bytes = 0;
        for k in &keys {
            bytes += cache.get(k).unwrap().len();
        }
        assert_eq!(bytes, n as usize * size);
    }
    report("warm: open + get all (1 thread)", t, n);

    let t = Instant::now();
    {
        let cache = Cache::open_in(&root, "bench", 1).unwrap();
        let threads = std::thread::available_parallelism().map_or(4, |p| p.get().min(4));
        let chunk = keys.len().div_ceil(threads);
        std::thread::scope(|s| {
            for part in keys.chunks(chunk) {
                let cache = &cache;
                s.spawn(move || {
                    for k in part {
                        assert!(cache.get(k).is_some());
                    }
                });
            }
        });
    }
    report("warm: open + get all (4 threads)", t, n);

    let t = Instant::now();
    {
        let cache = Cache::open_in(&root, "bench", 1).unwrap();
        assert!(keys.iter().all(|k| cache.contains(k)));
    }
    report("warm: open + contains all (index)", t, n);

    let t = Instant::now();
    let usage = Cache::open_in(&root, "bench", 1).unwrap().usage();
    report("usage() from index", t, 1);
    assert_eq!(usage.entries, n);

    let t = Instant::now();
    let r = Cache::open_in(&root, "bench", 1)
        .unwrap()
        .gc(u64::MAX)
        .unwrap();
    report("gc scan, nothing evicted", t, 1);
    assert_eq!(r.evicted, 0);

    let t = Instant::now();
    let r = Cache::open_in(&root, "bench", 1)
        .unwrap()
        .gc(usage.bytes / 2)
        .unwrap();
    report("gc evicting half", t, 1);
    println!("  evicted {} entries", r.evicted);

    let _ = std::fs::remove_dir_all(&root);
}

fn report(what: &str, t: Instant, n: u64) {
    let el = t.elapsed();
    let per = el.as_secs_f64() * 1e6 / n as f64;
    if n > 1 {
        println!(
            "{what:<36} {:>9.2} ms  ({per:.2} us/entry)",
            el.as_secs_f64() * 1e3
        );
    } else {
        println!("{what:<36} {:>9.2} ms", el.as_secs_f64() * 1e3);
    }
}
