//! Save → browser-updated latency on generated apps of 1k and 10k modules (the plan's
//! "HMR latency vs size" benchmark). Ignored by default; run with
//!
//! ```sh
//! scripts/limit.sh -- cargo test --release -p ts_r_dev --test latency -- --ignored --nocapture
//! TS_R_LATENCY_SIZES=1000,10000,50000 TS_R_LATENCY_EDITS=60 scripts/limit.sh -- cargo test --release ...
//! ```
//!
//! `scripts/limit.sh` bounds the whole tree (the server and Chrome too); `cap_memory!` below
//! only covers this binary.
//!
//! Each edit rewrites a self-accepting module deep in the graph whose new body records
//! `performance.timeOrigin + performance.now()` when it executes; latency = that time minus
//! the wall-clock time just before the write. Same machine, same clock.

mod support;

ts_r_testing::cap_memory!();

use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use support::{Chrome, DevProcess, find_chrome};
use ts_r_hmr::testutil::TempDir;

const GROUP: usize = 100;

/// Writes an app with `n` leaf modules in groups of 100, plus `src/g0/hot.js`.
fn generate(dir: &TempDir, n: usize) {
    let groups = n.div_ceil(GROUP);
    dir.write(
        "index.html",
        "<!doctype html><html><head><title>bench</title></head><body><div id=\"out\"></div><div id=\"hot\"></div><script type=\"module\" src=\"/src/main.js\"></script></body></html>\n",
    );
    let mut main = String::new();
    for g in 0..groups {
        main.push_str(&format!(
            "import {{ total as t{g} }} from './g{g}/index.js';\n"
        ));
    }
    main.push_str("let sum = 0;\n");
    for g in 0..groups {
        main.push_str(&format!("sum += t{g};\n"));
    }
    main.push_str(
        "document.getElementById('out').textContent = 'loaded ' + sum;\nwindow.__loaded = true;\n",
    );
    dir.write("src/main.js", &main);
    for g in 0..groups {
        let mut index = String::new();
        let count = GROUP.min(n - g * GROUP);
        for i in 0..count {
            index.push_str(&format!("import {{ v{i} }} from './m{i}.js';\n"));
        }
        if g == 0 {
            index.push_str("import { hot } from './hot.js';\nexport const hotValue = hot;\n");
        }
        index.push_str("export const total = 0");
        for i in 0..count {
            index.push_str(&format!(" + v{i}"));
        }
        index.push_str(";\n");
        dir.write(&format!("src/g{g}/index.js"), &index);
        for i in 0..count {
            dir.write(
                &format!("src/g{g}/m{i}.js"),
                &format!(
                    "// Generated module {g}/{i}.\nexport const v{i} = {i};\nexport function f{i}(x) {{\n  return x * {i} + {g};\n}}\nexport const label{i} = 'module-{g}-{i}';\n"
                ),
            );
        }
    }
    dir.write("src/g0/hot.js", &hot_module(0));
}

fn hot_module(seq: u64) -> String {
    format!(
        "export const hot = {seq};\n\
         (window.__hmrTimes ||= []).push([{seq}, performance.timeOrigin + performance.now()]);\n\
         document.getElementById('hot').textContent = 'hot ' + {seq};\n\
         if (import.meta.hot) {{ import.meta.hot.accept(); }}\n"
    )
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[test]
#[ignore = "benchmark: needs Chrome; run with --ignored --nocapture (ideally --release)"]
fn hmr_latency_by_project_size() {
    let Some(chrome_bin) = find_chrome() else {
        eprintln!("skipping: Chrome not found (set TS_R_CHROME)");
        return;
    };
    let sizes: Vec<usize> = std::env::var("TS_R_LATENCY_SIZES")
        .unwrap_or_else(|_| "1000,10000".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let edits: u64 = std::env::var("TS_R_LATENCY_EDITS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let mut report = Vec::new();
    for &n in &sizes {
        let app = TempDir::new(&format!("lat-{n}"));
        let cache = TempDir::new(&format!("lat-cache-{n}"));
        let t = Instant::now();
        generate(&app, n);
        let gen_time = t.elapsed();
        let dev = DevProcess::start(app.path(), cache.path(), &[]);
        let mut chrome = Chrome::launch(&chrome_bin);
        let t = Instant::now();
        chrome.open(&dev.url);
        chrome.cdp.wait_for(
            "window.__loaded === true && window.__hmrTimes?.length === 1",
            Duration::from_secs(300),
        );
        let load_time = t.elapsed();
        std::thread::sleep(Duration::from_millis(500));

        let hot = app.path().join("src/g0/hot.js");
        let mut samples = Vec::new();
        let mut first_save = f64::NAN;
        for seq in 1..=edits {
            let written = now_ms();
            fs::write(&hot, hot_module(seq)).unwrap();
            let applied = chrome.cdp.wait_for(
                &format!("window.__hmrTimes.find(e => e[0] === {seq})?.[1]"),
                Duration::from_secs(10),
            );
            let latency = applied.as_f64().unwrap() - written;
            if seq == 1 {
                // First save of this file: macOS has no kqueue fast-path watch on it yet.
                first_save = latency;
            }
            if seq > 3 {
                samples.push(latency); // first edits warm up the JIT and caches
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        let reloads = chrome
            .cdp
            .eval("performance.getEntriesByType('navigation')[0].type")
            .unwrap();
        assert_eq!(reloads, "navigate", "no full reloads during the run");
        samples.sort_by(f64::total_cmp);
        // Give the last acknowledgements a moment, then read the server's breakdown.
        std::thread::sleep(Duration::from_millis(200));
        let stats = dev.stats();
        let step = |k: &str| {
            format!(
                "{k} {:.2}/{:.2}",
                stats[k]["p50"].as_f64().unwrap_or(f64::NAN),
                stats[k]["p95"].as_f64().unwrap_or(f64::NAN)
            )
        };
        let line = format!(
            "{n:>6} modules: save→update p50 {:.1} ms, p95 {:.1} ms, min {:.1}, max {:.1} ({} edits), first save {:.1} ms | \
             initial page load {:.2} s, generate {:.2} s | server p50/p95 ms: {}, {}, {}, {}, {}, {}, {}",
            pct(&samples, 50.0),
            pct(&samples, 95.0),
            samples[0],
            samples[samples.len() - 1],
            samples.len(),
            first_save,
            load_time.as_secs_f64(),
            gen_time.as_secs_f64(),
            step("watch"),
            step("debounce"),
            step("transform"),
            step("graph"),
            step("push"),
            step("browser"),
            step("total"),
        );
        eprintln!("{line}");
        report.push(line);
    }
    eprintln!("\nHMR latency report\n{}", report.join("\n"));
}
