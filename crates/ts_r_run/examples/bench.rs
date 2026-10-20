//! Startup and module-graph benchmark: ts-r-run against `node --experimental-strip-types` and
//! tsx, with timing loops (no hyperfine needed).
//!
//! ```text
//! cargo build --release -p ts_r_run --bin ts-r-run --example bench
//! TS_R_RUN_THREADS=4 scripts/limit.sh --mem-mb 4096 --time-s 600 -- \
//!   target/release/examples/bench [--modules 1000] [--runs 15] [--tsx /path/to/tsx]
//! ```
//!
//! Generates a project of N modules (interfaces, classes, typed functions, `.ts` imports so
//! Node's type stripping can run it too) plus `hello.ts`, and prints the median and minimum
//! wall time of each command. tsx is taken from `--tsx`, `$TSX`, or skipped.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

ts_r_testing::cap_memory!();

fn generate(root: &Path, n: usize) -> std::io::Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join("package.json"),
        "{\"name\":\"bench\",\"type\":\"module\"}\n",
    )?;
    std::fs::write(
        root.join("tsconfig.json"),
        "{\"compilerOptions\":{\"target\":\"esnext\",\"module\":\"nodenext\",\"allowImportingTsExtensions\":true,\"noEmit\":true,\"strict\":true}}\n",
    )?;
    for i in 0..n {
        let mut deps: Vec<usize> = [i.wrapping_sub(1), i / 2, i / 3]
            .into_iter()
            .filter(|&j| j < i)
            .collect();
        deps.sort_unstable();
        deps.dedup();
        let mut s = String::new();
        for j in &deps {
            let _ = writeln!(s, "import {{ f{j} }} from \"./m{j}.ts\";");
        }
        if let Some(j) = deps.first() {
            let _ = writeln!(s, "import type {{ Shape{j} as S }} from \"./m{j}.ts\";");
        }
        let _ = writeln!(
            s,
            "export interface Shape{i} {{ id: number; name: string; tags: readonly string[] }}"
        );
        let _ = writeln!(s, "type Key{i} = keyof Shape{i};");
        let _ = writeln!(
            s,
            "export class C{i} {{ private items: Map<string, number> = new Map(); add(k: string, v: number): this {{ this.items.set(k, v); return this; }} total(): number {{ let t = 0; for (const v of this.items.values()) t += v; return t; }} }}"
        );
        let body = if deps.is_empty() {
            "0".to_owned()
        } else {
            deps.iter()
                .map(|j| format!("f{j}(x - 1)"))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        let _ = writeln!(s, "const memo{i} = new Map<number, number>();");
        let _ = writeln!(
            s,
            "export function f{i}(x: number): number {{ if (x <= 0) return {i}; const m = memo{i}.get(x); if (m !== undefined) return m; const r = (x + {i} + ({body})) % 1000003; memo{i}.set(x, r); return r; }}"
        );
        let _ = writeln!(
            s,
            "export const k{i}: Key{i} = \"name\" as const satisfies Key{i};"
        );
        let _ = writeln!(
            s,
            "export function describe{i}(s: Shape{i}): string {{ return `${{s.id}}:${{s.name}}:${{s.tags.join(',')}}`; }}"
        );
        std::fs::write(root.join(format!("src/m{i}.ts")), s)?;
    }
    let mut main = String::new();
    for i in 0..n {
        let _ = writeln!(main, "import {{ f{i} }} from \"./src/m{i}.ts\";");
    }
    main.push_str("let total: number = 0;\n");
    for i in 0..n {
        let _ = writeln!(main, "total += f{i}(3);");
    }
    main.push_str("console.log(total);\n");
    std::fs::write(root.join("main.ts"), main)?;
    std::fs::write(
        root.join("hello.ts"),
        "const greeting: string = 'hello';\ninterface X { a: number }\nconsole.log(greeting);\n",
    )
}

/// (label, command line, extra environment, per-run setup)
type Row<'a> = (&'a str, Vec<String>, Vec<(&'a str, String)>, &'a dyn Fn());

fn time(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, String)],
    setup: &dyn Fn(),
    runs: usize,
) -> Option<(f64, f64)> {
    let run = || {
        setup();
        let start = Instant::now();
        let status = Command::new(&cmd[0])
            .args(&cmd[1..])
            .current_dir(cwd)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        status
            .success()
            .then(|| start.elapsed().as_secs_f64() * 1000.0)
    };
    for _ in 0..2 {
        run()?;
    }
    let mut times: Vec<f64> = (0..runs).map(|_| run()).collect::<Option<_>>()?;
    times.sort_by(f64::total_cmp);
    Some((times[times.len() / 2], times[0]))
}

fn main() {
    let mut modules = 1000usize;
    let mut runs = 15usize;
    let mut tsx = std::env::var("TSX").ok();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--modules" => modules = args.next().and_then(|v| v.parse().ok()).unwrap_or(modules),
            "--runs" => runs = args.next().and_then(|v| v.parse().ok()).unwrap_or(runs),
            "--tsx" => tsx = args.next(),
            _ => {}
        }
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let run_bin: PathBuf = std::env::var_os("TS_R_RUN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            exe.parent()
                .and_then(Path::parent)
                .map(|d| d.join("ts-r-run"))
                .unwrap_or_default()
        });
    if !run_bin.exists() {
        eprintln!("build ts-r-run first: cargo build --release -p ts_r_run --bin ts-r-run");
        std::process::exit(2);
    }
    let root = std::env::temp_dir().join(format!("ts-r-run-bench-{}", std::process::id()));
    let cold_cache = root.join("cold-cache");
    let warm_cache = root.join("warm-cache");
    let project = root.join("project");
    if let Err(e) = generate(&project, modules) {
        eprintln!("cannot generate the project: {e}");
        std::process::exit(1);
    }
    let r = run_bin.to_string_lossy().to_string();
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    let none = || {};
    let wipe_cold = || {
        let _ = std::fs::remove_dir_all(&cold_cache);
    };
    for script in ["hello.ts", "main.ts"] {
        let entry = project.join(script);
        let touch = || {
            let _ = std::fs::File::options()
                .append(true)
                .open(&entry)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        };
        let label = if script == "main.ts" {
            format!("{modules} modules")
        } else {
            "hello.ts".to_owned()
        };
        println!("== {label}");
        let warm = vec![("TS_R_CACHE_DIR", warm_cache.to_string_lossy().to_string())];
        let cold = vec![("TS_R_CACHE_DIR", cold_cache.to_string_lossy().to_string())];
        let mut rows: Vec<Row> = vec![
            ("node -e 0 (floor)", s(&["node", "-e", "0"]), vec![], &none),
            (
                "node --experimental-strip-types",
                s(&["node", "--experimental-strip-types", script]),
                vec![],
                &none,
            ),
            ("ts-r-run (warm)", s(&[&r, script]), warm.clone(), &none),
            (
                "ts-r-run (cache warm, entry touched)",
                s(&[&r, script]),
                warm.clone(),
                &touch,
            ),
            ("ts-r-run (empty cache)", s(&[&r, script]), cold, &wipe_cold),
            (
                "ts-r-run --no-source-maps (warm)",
                s(&[&r, "--no-source-maps", script]),
                warm.clone(),
                &none,
            ),
            (
                "ts-r-run --strategy hook (warm)",
                s(&[&r, "--strategy", "hook", script]),
                warm,
                &none,
            ),
        ];
        if let Some(t) = &tsx {
            rows.insert(2, ("tsx", s(&[t, script]), vec![], &none));
        }
        for (name, cmd, env, setup) in rows {
            match time(&cmd, &project, &env, setup, runs) {
                Some((med, min)) => println!("  {name:40} median {med:7.1} ms   min {min:7.1} ms"),
                None => println!("  {name:40} failed"),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
