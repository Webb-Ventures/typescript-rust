//! Measures two editor windows on one repo: through `ts-r-lsp` (one shared backend) versus
//! two plain backends started separately, and versus what VS Code starts per window today
//! (semantic tsserver with automatic type acquisition + a syntax server).
//!
//! ```sh
//! cargo build --release -p ts_r_lsp
//! cargo run --release -p ts_r_lsp --example two_windows -- \
//!     /path/to/repo [--a file1,file2] [--b file3,file4] [--bin target/release/ts-r-lsp]
//! ```
//!
//! Reports total RSS (all processes of each setup, summed with their children) after both
//! windows have diagnostics for every file they opened, and the time from starting a window
//! to its first diagnostics (semantic, for the first file).
//!
//! It starts real tsserver processes on a real repo: run it under `scripts/limit.sh`.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use ts_r_lsp::backend::typescript::{self, TsEntry};
use ts_r_lsp::client::{Dialect, ScriptedClient, did_open_params, initialize_params};
use ts_r_lsp::process::{Snapshot, mib};
use ts_r_lsp::uri::path_to_uri;

ts_r_testing::cap_memory!();

const T: Duration = Duration::from_secs(300);

struct Args {
    root: PathBuf,
    a: Vec<String>,
    b: Vec<String>,
    bin: PathBuf,
    settle: Duration,
}

fn parse() -> Args {
    let mut it = std::env::args().skip(1);
    let mut root = None;
    // Defaults: tRPC's layout (the pinned bench checkout, target/bench-repos/trpc).
    let mut a = vec![
        "packages/server/src/unstable-core-do-not-import/router.ts".to_string(),
        "packages/server/src/index.ts".to_string(),
    ];
    let mut b = vec![
        "packages/client/src/index.ts".to_string(),
        "packages/client/src/links/httpBatchLink.ts".to_string(),
    ];
    let mut bin = std::env::current_exe()
        .ok()
        .and_then(|p| Some(p.parent()?.parent()?.join("ts-r-lsp")))
        .unwrap_or_else(|| PathBuf::from("ts-r-lsp"));
    let mut settle = Duration::from_secs(3);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--a" => {
                a = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(str::to_string)
                    .collect()
            }
            "--b" => {
                b = it
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(str::to_string)
                    .collect()
            }
            "--bin" => bin = PathBuf::from(it.next().unwrap_or_default()),
            "--settle" => {
                settle = Duration::from_secs(it.next().and_then(|s| s.parse().ok()).unwrap_or(3))
            }
            _ => root = Some(PathBuf::from(x)),
        }
    }
    let root = std::fs::canonicalize(
        root.expect("usage: two_windows <repo> [--a f,g] [--b f,g] [--bin path]"),
    )
    .expect("repo exists");
    Args {
        root,
        a,
        b,
        bin,
        settle,
    }
}

/// RSS of each pid's process tree, summed.
fn total_rss(pids: &[u32]) -> (u64, Vec<(u32, u64)>) {
    let snap = Snapshot::take();
    let mut seen = std::collections::BTreeSet::new();
    let mut per = Vec::new();
    let mut total = 0;
    for &pid in pids {
        let mut tree_total = 0;
        for p in snap.tree(pid) {
            if seen.insert(p) {
                tree_total += snap.rss_bytes(p).unwrap_or(0);
            }
        }
        per.push((pid, tree_total));
        total += tree_total;
    }
    (total, per)
}

fn read(root: &Path, f: &str) -> (PathBuf, String) {
    let p = root.join(f);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    (p, text)
}

/// One ts-r-lsp window: initialize, open files, wait for diagnostics of each.
fn lsp_window(
    args: &Args,
    cache: &Path,
    name: &str,
    files: &[String],
) -> (ScriptedClient, Duration, Duration) {
    let mut cmd = Command::new(&args.bin);
    cmd.args(["--name", name])
        .current_dir(&args.root)
        .env("TS_R_CACHE_DIR", cache);
    let mut c = ScriptedClient::spawn(cmd, Dialect::Lsp).expect("spawn ts-r-lsp");
    let t0 = c.spawned_at;
    c.call("initialize", initialize_params(&args.root), T)
        .expect("initialize");
    c.notify("initialized", json!({})).unwrap();
    let mut first = None;
    for f in files {
        let (p, text) = read(&args.root, f);
        c.notify("textDocument/didOpen", did_open_params(&p, &text))
            .unwrap();
    }
    for f in files {
        let uri = path_to_uri(&args.root.join(f).to_string_lossy());
        c.wait_for(T, |m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == json!(uri)
        })
        .unwrap_or_else(|| panic!("no diagnostics for {f}"));
        first.get_or_insert_with(|| t0.elapsed());
    }
    (c, first.unwrap_or_default(), t0.elapsed())
}

/// A plain tsserver: open files, `geterr`, wait for every file's semanticDiag.
fn tsserver_window(
    js: &Path,
    args: &Args,
    files: &[String],
    extra: &[&str],
) -> (ScriptedClient, Duration, Duration) {
    let mut cmd = Command::new(typescript::find_node().expect("node on PATH"));
    cmd.arg(js).args(extra).current_dir(&args.root);
    let mut c = ScriptedClient::spawn(cmd, Dialect::TsServer).expect("spawn tsserver");
    let t0 = c.spawned_at;
    let open: Vec<Value> = files
        .iter()
        .map(|f| {
            let (p, text) = read(&args.root, f);
            json!({"file": p, "fileContent": text, "projectRootPath": args.root})
        })
        .collect();
    c.call("updateOpen", json!({"openFiles": open}), T)
        .expect("updateOpen");
    let paths: Vec<String> = files
        .iter()
        .map(|f| args.root.join(f).to_string_lossy().into_owned())
        .collect();
    c.request("geterr", json!({"files": paths, "delay": 0}))
        .unwrap();
    let mut first = None;
    for p in &paths {
        c.wait_for(T, |m| {
            m["event"] == "semanticDiag" && m["body"]["file"] == json!(p)
        })
        .unwrap_or_else(|| panic!("no semanticDiag for {p}"));
        first.get_or_insert_with(|| t0.elapsed());
    }
    (c, first.unwrap_or_default(), t0.elapsed())
}

fn syntax_server(js: &Path, args: &Args, files: &[String]) -> ScriptedClient {
    let mut cmd = Command::new(typescript::find_node().expect("node"));
    cmd.arg(js)
        .args([
            "--serverMode",
            "partialSemantic",
            "--useInferredProjectPerProjectRoot",
            "--disableAutomaticTypingAcquisition",
        ])
        .current_dir(&args.root);
    let mut c = ScriptedClient::spawn(cmd, Dialect::TsServer).expect("spawn syntax server");
    let open: Vec<Value> = files
        .iter()
        .map(|f| {
            let (p, text) = read(&args.root, f);
            json!({"file": p, "fileContent": text, "projectRootPath": args.root})
        })
        .collect();
    c.call("updateOpen", json!({"openFiles": open}), T)
        .expect("updateOpen");
    let p = args.root.join(&files[0]).to_string_lossy().into_owned();
    let _ = c.call("navtree", json!({"file": p}), T);
    c
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn main() {
    let args = parse();
    let ts =
        typescript::find_typescript(&args.root).expect("no node_modules/typescript in the repo");
    println!("repo: {}", args.root.display());
    println!("typescript: {} ({:?})", ts.version, ts.entry);
    println!("window A opens {:?}\nwindow B opens {:?}\n", args.a, args.b);

    // 1. ts-r-lsp: two windows, one daemon, one backend.
    let cache = std::env::temp_dir().join(format!("tsr-two-windows-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let (a, a_first, a_all) = lsp_window(&args, &cache, "window A", &args.a);
    let (b, b_first, b_all) = lsp_window(&args, &cache, "window B", &args.b);
    std::thread::sleep(args.settle);
    let status = Command::new(&args.bin)
        .args(["status", "--root", &args.root.to_string_lossy()])
        .env("TS_R_CACHE_DIR", &cache)
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .unwrap_or(Value::Null);
    let daemon = status["daemonPid"].as_u64().unwrap_or(0) as u32;
    // The daemon first: it was spawned by window A's front, so it would count in A's tree.
    let (tsr_total, tsr_per) = total_rss(&[daemon, a.pid(), b.pid()]);
    println!(
        "== ts-r-lsp (2 fronts + 1 daemon + {} backend(s))",
        status["slots"].as_array().map_or(0, Vec::len)
    );
    for (pid, rss) in &tsr_per {
        let what = if *pid == daemon {
            "daemon + backend tree"
        } else {
            "front"
        };
        println!("   pid {pid:>6} {what:<22} {:>6} MB", mib(*rss));
    }
    println!("   total {:>6} MB", mib(tsr_total));
    println!(
        "   window A: first diagnostics {} (all files {})",
        ms(a_first),
        ms(a_all)
    );
    println!(
        "   window B: first diagnostics {} (all files {})",
        ms(b_first),
        ms(b_all)
    );
    println!(
        "   mode {}, slots {}",
        status["mode"],
        status["slots"].as_array().map_or(0, Vec::len)
    );
    a.shutdown();
    b.shutdown();
    let _ = Command::new(&args.bin)
        .args(["stop", "--root", &args.root.to_string_lossy()])
        .env("TS_R_CACHE_DIR", &cache)
        .output();
    std::thread::sleep(Duration::from_secs(1));
    println!("   daemon log: {}", cache.join("lsp").display());
    if std::env::var_os("TWO_WINDOWS_ONLY_TSR").is_some() {
        return;
    }

    let TsEntry::TsServer(js) = &ts.entry else {
        let _ = std::fs::remove_dir_all(&cache);
        println!("\n(TS 7 project: plain-backend comparison runs tsserver only; skipping)");
        return;
    };

    // 2. Two plain backends, one per window, started the way ts-r-lsp starts its one.
    let flags = [
        "--useInferredProjectPerProjectRoot",
        "--disableAutomaticTypingAcquisition",
        "--noGetErrOnBackgroundUpdate",
    ];
    let (pa, pa_first, _) = tsserver_window(js, &args, &args.a, &flags);
    let (pb, pb_first, _) = tsserver_window(js, &args, &args.b, &flags);
    std::thread::sleep(args.settle);
    let (plain_total, plain_per) = total_rss(&[pa.pid(), pb.pid()]);
    println!("\n== two plain tsservers (one per window)");
    for (pid, rss) in &plain_per {
        println!("   pid {pid:>6} tsserver tree          {:>6} MB", mib(*rss));
    }
    println!("   total {:>6} MB", mib(plain_total));
    println!("   window A: first diagnostics {}", ms(pa_first));
    println!("   window B: first diagnostics {}", ms(pb_first));
    pa.shutdown();
    pb.shutdown();
    std::thread::sleep(Duration::from_secs(1));

    // 3. What VS Code starts per window today: semantic tsserver (typings installer on) +
    //    a partialSemantic syntax server.
    let vs_flags = [
        "--useInferredProjectPerProjectRoot",
        "--noGetErrOnBackgroundUpdate",
    ];
    let (va, va_first, _) = tsserver_window(js, &args, &args.a, &vs_flags);
    let sa = syntax_server(js, &args, &args.a);
    let (vb, vb_first, _) = tsserver_window(js, &args, &args.b, &vs_flags);
    let sb = syntax_server(js, &args, &args.b);
    std::thread::sleep(args.settle);
    let (vs_total, vs_per) = total_rss(&[va.pid(), sa.pid(), vb.pid(), sb.pid()]);
    println!(
        "\n== VS Code today: per window semantic tsserver (+ typings installer) + syntax server"
    );
    for (pid, rss) in &vs_per {
        println!("   pid {pid:>6} tree                   {:>6} MB", mib(*rss));
    }
    println!("   total {:>6} MB", mib(vs_total));
    println!("   window A: first diagnostics {}", ms(va_first));
    println!("   window B: first diagnostics {}", ms(vb_first));
    for c in [va, sa, vb, sb] {
        c.shutdown();
    }
    if std::env::var_os("TWO_WINDOWS_KEEP_CACHE").is_none() {
        let _ = std::fs::remove_dir_all(&cache);
    }

    println!(
        "\nsummary: ts-r-lsp {} MB vs two plain backends {} MB ({:.0}%) vs VS Code today {} MB ({:.0}%)",
        mib(tsr_total),
        mib(plain_total),
        100.0 * tsr_total as f64 / plain_total.max(1) as f64,
        mib(vs_total),
        100.0 * tsr_total as f64 / vs_total.max(1) as f64,
    );
}
