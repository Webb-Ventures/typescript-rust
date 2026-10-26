//! `cargo xtask npm-pack`: builds the npm packages for this machine and checks them.
//!
//! 1. Builds the seven binaries (`--profile`, default release, `-j 4`) unless `--bin-dir` names
//!    a finished build (for example `target/pgo/optimized/<target>/release`).
//! 2. Runs `npm/scripts/build-packages.mjs` into `target/npm/dist` (platform packages for the
//!    host target, every front package, `ts-r` and `@ts-r.dev/typescript`).
//! 3. `npm pack --dry-run --json` on each: required files present, packed size within budget,
//!    and a size table (packed, unpacked, and what installing each front costs on this platform).
//! 4. `--smoke`: packs real tarballs, installs each front package from them into its own temp
//!    project (`npm install --offline --ignore-scripts`: nothing comes from the registry except
//!    TypeScript itself for the drop-in test), and runs its bin: `--version` plus a smoke
//!    command. The `@ts-r.dev/typescript` test compares `npx tsc --noEmit` through the drop-in with
//!    the real tsc on the fixtures in `npm/tests/fixtures`.
//!
//! Everything that runs npm, node or our binaries goes through `scripts/limit.sh`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::pgo::{BINARIES, LIB_DIR};
use crate::util::{Result, npm_tool, read_to_string, root, run_capture, run_status};

/// Packed-size budgets in bytes, per package. Measured on macOS arm64 (M7): ts-r 0.27 MB,
/// run 1.2, dev 1.8, build 1.5, check 2.4 (0.7 of it TypeScript's lib files), lint 1.2,
/// lsp 3.8; PGO builds are about 25% larger. Linux and Windows binaries run a little larger.
const BUDGETS: &[(&str, u64)] = &[
    ("@ts-r.dev/cli-", 1 << 20),
    ("@ts-r.dev/run-", 3 << 20),
    ("@ts-r.dev/dev-", 4 << 20),
    ("@ts-r.dev/build-", 3 << 20),
    ("@ts-r.dev/check-", 4 << 20),
    ("@ts-r.dev/lint-", 3 << 20),
    ("@ts-r.dev/lsp-", 6 << 20),
    ("@ts-r.dev/typescript", 48 << 10),
];
/// Front packages (a shim and a package.json).
const FRONT_BUDGET: u64 = 8 << 10;

const HELP: &str = "\
cargo xtask npm-pack [--profile release|dist] [--bin-dir <dir>] [--out <dir>] [--smoke] [--keep]

Builds the npm packages for this machine into target/npm (or --out), checks their contents and
sizes with npm pack --dry-run, and with --smoke installs each front package from tarballs in a
temp project and runs its bin (--version plus a smoke command), including the @ts-r.dev/typescript
drop-in test against the real tsc. --keep leaves the temp projects in place.
";

struct Pack {
    name: String,
    version: String,
    kind: String,
    dir: PathBuf,
    size: u64,
    unpacked: u64,
    files: Vec<String>,
}

pub fn run(mut args: Args) -> Result<ExitCode> {
    if args.flag("help") {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    let profile = args.opt("profile")?.unwrap_or_else(|| "release".into());
    let bin_dir = args.opt("bin-dir")?.map(PathBuf::from);
    let out = args
        .opt("out")?
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("target/npm"));
    let smoke = args.flag("smoke");
    let keep = args.flag("keep");
    args.finish()?;
    let root = root();
    if !root.join(LIB_DIR).join("lib.es5.d.ts").is_file() {
        return Err(
            "no TypeScript lib files: run `cargo xtask upstream sync --minimal` first".into(),
        );
    }

    let bin_dir = match bin_dir {
        Some(d) => d,
        None => {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let mut cmd = Command::new(cargo);
            cmd.current_dir(&root)
                .args(["build", "--profile", &profile, "-j", "4"]);
            for (pkg, _) in BINARIES {
                cmd.args(["-p", pkg]);
            }
            run_status(cmd.arg("--bins"))?;
            crate::util::profile_dir(&profile)
        }
    };

    let dist = out.join("dist");
    run_status(
        Command::new("node")
            .arg(root.join("npm/scripts/build-packages.mjs"))
            .arg("--bin-dir")
            .arg(&bin_dir)
            .arg("--out")
            .arg(&dist)
            .current_dir(&root),
    )?;
    let manifest: serde_json::Value =
        serde_json::from_str(&read_to_string(&dist.join("packages.json"))?)?;

    let mut packs = Vec::new();
    for p in manifest["packages"].as_array().into_iter().flatten() {
        let dir = dist.join(p["dir"].as_str().unwrap_or_default());
        let json = limited_capture(
            Command::new(npm_tool("npm"))
                .args(["pack", "--dry-run", "--json", "--ignore-scripts"])
                .current_dir(&dir),
            1024,
            120,
        )?;
        let v: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| format!("npm pack --json in {}: {e}", dir.display()))?;
        let info = &v[0];
        packs.push(Pack {
            name: p["name"].as_str().unwrap_or_default().to_string(),
            version: p["version"].as_str().unwrap_or_default().to_string(),
            kind: p["kind"].as_str().unwrap_or_default().to_string(),
            dir,
            size: info["size"].as_u64().unwrap_or(0),
            unpacked: info["unpackedSize"].as_u64().unwrap_or(0),
            files: info["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["path"].as_str().map(String::from))
                .collect(),
        });
    }

    let problems = check_packs(&packs);
    print_table(&packs);
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("npm-pack: {p}");
        }
        return Ok(ExitCode::from(1));
    }

    if smoke {
        let tarballs = out.join("tarballs");
        let _ = std::fs::remove_dir_all(&tarballs);
        std::fs::create_dir_all(&tarballs)?;
        let mut tgz = BTreeMap::new();
        for p in &packs {
            let name = limited_capture(
                Command::new(npm_tool("npm"))
                    .args(["pack", "--ignore-scripts", "--pack-destination"])
                    .arg(&tarballs)
                    .current_dir(&p.dir),
                1024,
                120,
            )?;
            let file = name.lines().last().unwrap_or_default().trim().to_string();
            tgz.insert(p.name.clone(), tarballs.join(file));
        }
        let temp = std::env::temp_dir().join(format!("ts-r-npm-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp)?;
        let failures = smoke_all(&packs, &tgz, &temp)?;
        if keep {
            eprintln!("npm-pack: temp projects kept in {}", temp.display());
        } else {
            std::fs::remove_dir_all(&temp)?;
        }
        if failures > 0 {
            eprintln!("npm-pack: {failures} smoke check(s) failed");
            return Ok(ExitCode::from(1));
        }
        eprintln!("npm-pack: every smoke check passed");
    }
    Ok(ExitCode::SUCCESS)
}

fn check_packs(packs: &[Pack]) -> Vec<String> {
    let mut problems = Vec::new();
    for p in packs {
        let budget = BUDGETS
            .iter()
            .find(|(prefix, _)| p.name.starts_with(prefix))
            .map(|(_, b)| *b)
            .unwrap_or(FRONT_BUDGET);
        if p.size > budget {
            problems.push(format!(
                "{} is {} packed, over its {} budget",
                p.name,
                human(p.size),
                human(budget)
            ));
        }
        let has = |f: &str| p.files.iter().any(|x| x == f);
        if !has("package.json") || !has("README.md") || !has("LICENSE") {
            problems.push(format!(
                "{} lacks package.json, README.md or LICENSE",
                p.name
            ));
        }
        if p.kind == "platform" {
            let tool = p
                .name
                .trim_start_matches("@ts-r.dev/")
                .split('-')
                .next()
                .unwrap_or_default();
            let binary = if tool == "cli" {
                "ts-r".to_string()
            } else {
                format!("ts-r-{tool}")
            };
            let exe = if p.name.contains("-win32-") {
                format!("bin/{binary}.exe")
            } else {
                format!("bin/{binary}")
            };
            if !has(&exe) {
                problems.push(format!("{} lacks {exe}", p.name));
            }
            if (tool == "check" || tool == "lsp")
                && !(has("lib/lib.es5.d.ts")
                    && has("lib/lib.dom.d.ts")
                    && has("lib/TypeScript-LICENSE.txt"))
            {
                problems.push(format!(
                    "{} lacks TypeScript's lib files or their license",
                    p.name
                ));
            }
        } else if !p.files.iter().any(|f| f.starts_with("bin/")) {
            problems.push(format!("{} has no bin", p.name));
        }
        if p.files
            .iter()
            .any(|f| f.contains("node_modules") || f.ends_with(".tgz"))
        {
            problems.push(format!(
                "{} would publish node_modules or a tarball",
                p.name
            ));
        }
    }
    problems
}

fn human(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.2} MB", bytes as f64 / (1 << 20) as f64)
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

fn print_table(packs: &[Pack]) {
    println!("| package | files | packed | unpacked |");
    println!("|---|---:|---:|---:|");
    for p in packs {
        println!(
            "| {}@{} | {} | {} | {} |",
            p.name,
            p.version,
            p.files.len(),
            human(p.size),
            human(p.unpacked)
        );
    }
    // What installing each front package downloads on this platform: itself plus its binary.
    println!("\n| install | downloads (packed) |");
    println!("|---|---:|");
    let platform = |prefix: &str| {
        packs
            .iter()
            .filter(|p| p.kind == "platform" && p.name.starts_with(prefix))
            .map(|p| p.size)
            .sum::<u64>()
    };
    let size_of = |name: &str| {
        packs
            .iter()
            .filter(|p| p.name == name)
            .map(|p| p.size)
            .sum::<u64>()
    };
    let mut all = 0;
    for (front, id) in [
        ("@ts-r.dev/run", "run"),
        ("@ts-r.dev/dev", "dev"),
        ("@ts-r.dev/build", "build"),
        ("@ts-r.dev/check", "check"),
        ("@ts-r.dev/lint", "lint"),
        ("@ts-r.dev/lsp", "lsp"),
    ] {
        let s = size_of(front) + platform(&format!("@ts-r.dev/{id}-"));
        all += s;
        println!("| `{front}` | {} |", human(s));
    }
    let launcher = size_of("@ts-r.dev/cli") + platform("@ts-r.dev/cli-");
    println!(
        "| `@ts-r.dev/cli` (the ts-r launcher + every tool) | {} |",
        human(all + launcher)
    );
    let check = size_of("@ts-r.dev/check") + platform("@ts-r.dev/check-");
    println!(
        "| `@ts-r.dev/typescript` (+ `@ts-r.dev/check`) | {} |",
        human(size_of("@ts-r.dev/typescript") + check)
    );
}

/// Runs a command under scripts/limit.sh and returns stdout; non-zero exit is an error.
fn limited_capture(cmd: &mut Command, mem_mb: u32, time_s: u32) -> Result<String> {
    run_capture(&mut limited(cmd, mem_mb, time_s))
}

fn limited(cmd: &Command, mem_mb: u32, time_s: u32) -> Command {
    let mut c = Command::new("bash");
    c.arg(root().join("scripts/limit.sh"))
        .args([
            "--mem-mb",
            &mem_mb.to_string(),
            "--time-s",
            &time_s.to_string(),
            "--",
        ])
        .arg(cmd.get_program())
        .args(cmd.get_args());
    if let Some(dir) = cmd.get_current_dir() {
        c.current_dir(dir);
    }
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    c
}

struct Smoke<'a> {
    temp: &'a Path,
    failures: usize,
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Smoke<'_> {
    /// Runs `bin args` in `dir` (node_modules/.bin first on PATH), capped at 2048 MB / 120 s.
    fn exec(&self, dir: &Path, program: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(dir);
        smoke_env(&mut cmd, dir, self.temp);
        match limited(&cmd, 2048, 120).output() {
            Ok(o) => Output {
                code: o.status.code(),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(e) => Output {
                code: None,
                stdout: String::new(),
                stderr: e.to_string(),
            },
        }
    }

    fn check(&mut self, what: &str, ok: bool, detail: &str) {
        if ok {
            println!("  ok    {what}");
        } else {
            self.failures += 1;
            println!("  FAIL  {what}\n{}", indent(detail));
        }
    }

    /// `<bin> --version` prints `version`; then a smoke command.
    fn version(&mut self, dir: &Path, bin: &str, version: &str) {
        let o = self.exec(dir, &bin_path(dir, bin), &["--version"]);
        self.check(
            &format!("{bin} --version"),
            o.code == Some(0) && o.stdout.contains(version),
            &format!("{o:?}"),
        );
    }
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {:?}\nstdout:\n{}\nstderr:\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("        {l}\n")).collect()
}

fn bin_path(dir: &Path, bin: &str) -> String {
    let p = dir.join("node_modules/.bin").join(bin);
    p.to_string_lossy().into_owned()
}

/// A clean environment for installed packages: no developer overrides, an isolated cache.
fn smoke_env(cmd: &mut Command, dir: &Path, temp: &Path) {
    for k in [
        "TS_R_LIB_DIR",
        "TS_R_BIN_DIR",
        "TS_R_BINARY",
        "TS_R_TYPESCRIPT",
        "TS_R_TSC",
        "TS_R_VERSION",
    ] {
        cmd.env_remove(k);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.join("node_modules/.bin")];
    paths.extend(std::env::split_paths(&path));
    cmd.env("PATH", std::env::join_paths(paths).unwrap_or(path))
        .env("TS_R_CACHE_DIR", temp.join("cache"));
}

/// Writes package.json with `deps` (name -> spec) and installs it.
fn install(dir: &Path, deps: &BTreeMap<String, String>, offline: bool) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let pkg = serde_json::json!({ "name": "ts-r-smoke", "private": true, "devDependencies": deps });
    std::fs::write(
        dir.join("package.json"),
        serde_json::to_string_pretty(&pkg)? + "\n",
    )?;
    let mut cmd = Command::new(npm_tool("npm"));
    cmd.args([
        "install",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--loglevel=error",
    ])
    .arg(if offline {
        "--offline"
    } else {
        "--prefer-offline"
    })
    .current_dir(dir)
    .stdin(Stdio::null());
    limited_capture(&mut cmd, 2048, 300)
        .map(|_| ())
        .map_err(|e| format!("npm install in {}: {e}", dir.display()).into())
}

fn file_spec(tgz: &BTreeMap<String, PathBuf>, name: &str) -> Result<String> {
    tgz.get(name)
        .map(|p| format!("file:{}", p.display()))
        .ok_or_else(|| format!("no tarball for {name}").into())
}

/// Front package -> its host platform package and binary.
fn tool_deps(
    packs: &[Pack],
    tgz: &BTreeMap<String, PathBuf>,
    id: &str,
    front: &str,
) -> Result<BTreeMap<String, String>> {
    let mut deps = BTreeMap::new();
    deps.insert(front.to_string(), file_spec(tgz, front)?);
    let platform = packs
        .iter()
        .find(|p| p.kind == "platform" && p.name.starts_with(&format!("@ts-r.dev/{id}-")))
        .ok_or_else(|| format!("no platform package for {id}"))?;
    deps.insert(platform.name.clone(), file_spec(tgz, &platform.name)?);
    Ok(deps)
}

fn smoke_all(packs: &[Pack], tgz: &BTreeMap<String, PathBuf>, temp: &Path) -> Result<usize> {
    let version = packs
        .iter()
        .find(|p| p.name == "@ts-r.dev/cli")
        .map(|p| p.version.clone())
        .unwrap_or_default();
    let mut s = Smoke { temp, failures: 0 };
    let fixtures = root().join("npm/tests/fixtures");

    for (id, front, bin) in [
        ("run", "@ts-r.dev/run", "ts-r-run"),
        ("build", "@ts-r.dev/build", "ts-r-build"),
        ("check", "@ts-r.dev/check", "ts-r-check"),
        ("lint", "@ts-r.dev/lint", "ts-r-lint"),
        ("dev", "@ts-r.dev/dev", "ts-r-dev"),
        ("lsp", "@ts-r.dev/lsp", "ts-r-lsp"),
    ] {
        println!("{front}");
        let dir = temp.join(id);
        install(&dir, &tool_deps(packs, tgz, id, front)?, true)?;
        let installed: Vec<String> = std::fs::read_dir(dir.join("node_modules/@ts-r.dev"))?
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        s.check(
            &format!(
                "installs only {id} and its binary ({})",
                installed.join(", ")
            ),
            installed.len() == 2,
            "other @ts-r.dev packages were installed",
        );
        s.version(&dir, bin, &version);
        std::fs::write(
            dir.join("hello.ts"),
            "const msg: string = 'smoke-ok';\nconsole.log(msg);\n",
        )?;
        match id {
            "run" => {
                let o = s.exec(&dir, &bin_path(&dir, bin), &["hello.ts"]);
                s.check(
                    "ts-r-run hello.ts",
                    o.code == Some(0) && o.stdout.contains("smoke-ok"),
                    &format!("{o:?}"),
                );
            }
            "build" => {
                let o = s.exec(
                    &dir,
                    &bin_path(&dir, bin),
                    &["hello.ts", "--bundle", "--minify"],
                );
                s.check(
                    "ts-r-build hello.ts --bundle --minify",
                    o.code == Some(0) && o.stdout.contains("smoke-ok"),
                    &format!("{o:?}"),
                );
            }
            "check" => {
                for (fx, code, needle) in [("pass", 0, ""), ("errors", 1, "error TS2322")] {
                    copy_dir(&fixtures.join(fx), &dir.join(fx))?;
                    let o = s.exec(
                        &dir.join(fx),
                        &bin_path(&dir, bin),
                        &["-p", "tsconfig.json", "--pretty", "false"],
                    );
                    s.check(
                        &format!("ts-r-check -p {fx}/tsconfig.json (lib files from the package) exits {code}"),
                        o.code == Some(code) && o.stdout.contains(needle),
                        &format!("{o:?}"),
                    );
                }
            }
            "lint" => {
                std::fs::write(
                    dir.join("eslint.config.mjs"),
                    "export default [{ rules: { 'no-debugger': 'error' } }];\n",
                )?;
                std::fs::write(dir.join("a.js"), "debugger;\n")?;
                let o = s.exec(&dir, &bin_path(&dir, bin), &["a.js", "-f", "json"]);
                s.check(
                    "ts-r-lint a.js reports no-debugger",
                    o.code == Some(1) && o.stdout.contains("no-debugger"),
                    &format!("{o:?}"),
                );
            }
            "dev" => {
                let (ok, detail) = dev_smoke(&dir, &bin_path(&dir, bin), temp);
                s.check("ts-r-dev serves index.html", ok, &detail);
            }
            "lsp" => {
                let o = s.exec(&dir, &bin_path(&dir, bin), &["--help"]);
                s.check(
                    "ts-r-lsp --help",
                    o.code == Some(0) && o.stdout.contains("USAGE"),
                    &format!("{o:?}"),
                );
            }
            _ => {}
        }
    }

    // @ts-r.dev/cli: the launcher package with every tool.
    println!("@ts-r.dev/cli");
    let dir = temp.join("ts-r");
    let mut deps = BTreeMap::new();
    for p in packs.iter().filter(|p| p.name != "@ts-r.dev/typescript") {
        deps.insert(p.name.clone(), file_spec(tgz, &p.name)?);
    }
    install(&dir, &deps, true)?;
    s.version(&dir, "ts-r", &version);
    std::fs::write(dir.join("hello.ts"), "console.log('launcher-' + 'ok');\n")?;
    let o = s.exec(&dir, &bin_path(&dir, "ts-r"), &["run", "hello.ts"]);
    s.check(
        "ts-r run hello.ts finds ts-r-run",
        o.code == Some(0) && o.stdout.contains("launcher-ok"),
        &format!("{o:?}"),
    );
    let o = s.exec(&dir, &bin_path(&dir, "ts-r"), &["doctor"]);
    s.check("ts-r doctor", o.code == Some(0), &format!("{o:?}"));

    dropin_smoke(&mut s, packs, tgz, temp, &fixtures)?;
    Ok(s.failures)
}

/// Starts ts-r-dev on a free port under limit.sh (2048 MB / 60 s), fetches `/`, then stops
/// the whole process group.
fn dev_smoke(dir: &Path, bin: &str, temp: &Path) -> (bool, String) {
    let _ = std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>smoke-ok</title>\n",
    );
    let mut server = Command::new(bin);
    server
        .args([".", "--port", "0", "--no-watch", "--no-cache"])
        .current_dir(dir);
    smoke_env(&mut server, dir, temp);
    let mut cmd = limited(&server, 2048, 60);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        }
    });
    let start = Instant::now();
    let mut url = None;
    let mut seen = String::new();
    while start.elapsed() < Duration::from_secs(20) && url.is_none() {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(u) = line.trim().strip_prefix("Local:") {
                    url = Some(u.trim().to_string());
                }
                seen.push_str(&line);
                seen.push('\n');
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    let result = match &url {
        Some(u) => http_get(u).map_err(|e| e.to_string()),
        None => Err(format!("no `Local:` line within 20 s; output:\n{seen}")),
    };
    // Stop limit.sh, which takes the shim and the native server it started down with it. On
    // unix it gets SIGTERM and time to do that (it exits by its own deadline regardless); a
    // SIGKILL would leave the server running.
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", child.id())])
            .status();
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
    match result {
        Ok(body) if body.contains("200") && body.contains("smoke-ok") => (true, String::new()),
        Ok(body) => (false, format!("unexpected response:\n{body}")),
        Err(e) => (false, e),
    }
}

/// Minimal HTTP/1.0 GET of `http://host:port/` (status line and body).
fn http_get(url: &str) -> std::io::Result<String> {
    let host = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let mut stream = std::net::TcpStream::connect(&host)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(stream, "GET / HTTP/1.0\r\nHost: {host}\r\n\r\n")?;
    let mut body = String::new();
    stream.read_to_string(&mut body)?;
    Ok(body)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

/// `@ts-r.dev/typescript` installed in place of `typescript` (with the real one as
/// `typescript-real`): `npx tsc --noEmit` gives tsc's exit code and output on every fixture,
/// using ts-r-check where the parity check passes and tsc where it doesn't.
fn dropin_smoke(
    s: &mut Smoke<'_>,
    packs: &[Pack],
    tgz: &BTreeMap<String, PathBuf>,
    temp: &Path,
    fixtures: &Path,
) -> Result<()> {
    println!("@ts-r.dev/typescript");
    let upstream: serde_json::Value =
        serde_json::from_str(&read_to_string(&root().join("upstream/UPSTREAM.json"))?)?;
    let ts_version = upstream["typescript"]["version"]
        .as_str()
        .ok_or("UPSTREAM.json has no typescript.version")?
        .to_string();
    let dir = temp.join("typescript");
    let mut deps = tool_deps(packs, tgz, "check", "@ts-r.dev/check")?;
    deps.insert("typescript".into(), file_spec(tgz, "@ts-r.dev/typescript")?);
    deps.insert(
        "typescript-real".into(),
        format!("npm:typescript@{ts_version}"),
    );
    install(&dir, &deps, false)?;
    let real_tsc = dir.join("node_modules/typescript-real/bin/tsc");
    let real_tsc = real_tsc.to_string_lossy().into_owned();
    let npx = npm_tool("npx");

    for (fx, expect) in [
        ("pass", "match"),
        ("errors", "match"),
        ("mismatch", "mismatch"),
    ] {
        let fdir = dir.join(fx);
        copy_dir(&fixtures.join(fx), &fdir)?;
        let real = s.exec(&fdir, "node", &[&real_tsc, "--noEmit"]);
        let _ = std::fs::remove_dir_all(temp.join("cache/typescript/parity"));
        for run in ["first run (parity check)", "second run (cached)"] {
            let ours = s.exec(&fdir, &npx, &["--no-install", "tsc", "--noEmit"]);
            s.check(
                &format!(
                    "{fx}: npx tsc --noEmit, {run}: exit {:?} and stdout equal real tsc's",
                    real.code
                ),
                ours.code == real.code && ours.stdout == real.stdout,
                &format!("drop-in: {ours:?}\nreal tsc: {real:?}"),
            );
        }
        let parity = parity_results(temp);
        s.check(
            &format!(
                "{fx}: parity {expect} (ts-r-check {})",
                if expect == "match" {
                    "used"
                } else {
                    "falls back to tsc"
                }
            ),
            parity == [expect],
            &format!("parity results recorded: {parity:?}"),
        );
    }

    let fdir = dir.join("pass");
    let ours = s.exec(
        &fdir,
        &npx,
        &["--no-install", "tsc", "--noEmit", "--listFiles"],
    );
    let real = s.exec(&fdir, "node", &[&real_tsc, "--noEmit", "--listFiles"]);
    s.check(
        "unsupported flag (--listFiles) goes to the real tsc with a note",
        ours.code == real.code
            && ours.stdout == real.stdout
            && ours.stderr.contains("doesn't support --listFiles"),
        &format!("drop-in: {ours:?}\nreal tsc: {real:?}"),
    );
    let ours = s.exec(&fdir, &npx, &["--no-install", "tsc", "--version"]);
    s.check(
        "tsc --version is the real TypeScript's",
        ours.code == Some(0) && ours.stdout.contains(&ts_version),
        &format!("{ours:?}"),
    );
    let o = s.exec(
        &dir,
        "node",
        &[
            "--input-type=module",
            "-e",
            "import ts from 'typescript'; const m = await import('typescript/unstable/sync'); console.log(ts.version, typeof m.API);",
        ],
    );
    s.check(
        "typescript module API delegates to the real TypeScript",
        o.code == Some(0) && o.stdout.trim() == format!("{ts_version} function"),
        &format!("{o:?}"),
    );
    Ok(())
}

/// The parity results recorded in the smoke cache (one per project checked since it was cleared).
fn parity_results(temp: &Path) -> Vec<String> {
    let dir = temp.join("cache/typescript/parity");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .filter_map(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .map(|v| v["result"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}
