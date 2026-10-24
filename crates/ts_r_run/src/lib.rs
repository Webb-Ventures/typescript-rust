//! `ts-r-run`: runs a TypeScript file on the Node you already have (replaces tsx, ts-node).
//!
//! The default strategy ("prebuild") compiles the module graph reachable from the entry in
//! parallel (through the `ts_r_cache` transpile cache), writes it as one manifest file, and
//! starts Node with a small preload whose synchronous hooks serve modules from that file and
//! resolve the specifiers ts-r-run resolved (`.js` → `.ts`, extensionless, tsconfig `paths`).
//! A warm run with nothing changed only `stat`s the inputs before starting Node.
//!
//! Strategy "hook" (`--strategy hook`) starts Node right away with off-thread
//! `module.register` hooks that ask a long-lived `ts-r-run --serve` child to resolve and
//! compile each module on demand. It was measured against "prebuild" and is slower (see
//! AGENTS.md); it stays as an option for graphs that are mostly loaded lazily.
//!
//! Design: `docs/design/0002-transform-and-codegen.md`.

pub mod compile;
pub mod graph;
pub mod manifest;
pub mod project;
pub mod serve;
pub mod watch;

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compile::is_compiled_path;
use crate::graph::Engine;
use crate::project::{Project, absolute};

const HOOKS: &str = include_str!("js/hooks.mjs");
const PRELOAD_ASYNC: &str = include_str!("js/preload-async.mjs");
const HOOKS_ASYNC: &str = include_str!("js/hooks-async.mjs");

const HELP: &str = "ts-r-run: run a TypeScript file with Node

Usage: ts-r-run [options] [node options] <file> [args...]

Options:
  --watch               Restart when a file of the module graph changes
  --tsconfig <file>     Use this tsconfig (default: nearest tsconfig.json above <file>)
  --no-source-maps      Don't map stack traces back to the TypeScript source
  --no-cache            Don't read or write the transpile cache (~/.cache/ts-r)
  --strategy <s>        prebuild (default): compile the graph, then start Node
                        hook: compile on demand from Node's module.register hooks
  --node <path>         Node binary (default: node on PATH)
  -h, --help            Show this help
  -V, --version         Show the version

Other options before <file> go to Node (use --flag=value for flags with values,
except -r, --require, --import, --env-file, -C and --conditions).

Environment: TS_R_RUN_THREADS (compile threads), TS_R_CACHE_DIR (cache root).
Needs Node 22.15+ (module.registerHooks).
";

/// How modules reach Node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    Prebuild,
    Hook,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Run,
    Help,
    Version,
    /// Internal: compile one file to stdout (`esm`/`cjs` line, then the code).
    Compile(String),
    /// Internal: resolve the specifier (the positional argument) imported from this parent
    /// through tsconfig `paths` and `baseUrl`, printing the file or nothing.
    Resolve(String),
    /// Internal: the strategy "hook" compiler process.
    Serve,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cli {
    pub mode: Mode,
    pub watch: bool,
    pub strategy: Strategy,
    pub tsconfig: Option<String>,
    pub source_maps: bool,
    pub cache: bool,
    pub node: String,
    pub node_args: Vec<String>,
    pub entry: String,
    pub args: Vec<String>,
}

/// Node flags whose value is the next argument.
const NODE_VALUE_FLAGS: &[&str] = &[
    "-r",
    "--require",
    "--import",
    "--loader",
    "--experimental-loader",
    "--env-file",
    "--env-file-if-exists",
    "-C",
    "--conditions",
    "--inspect-port",
    "--title",
];

pub fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        mode: Mode::Run,
        watch: false,
        strategy: Strategy::Prebuild,
        tsconfig: None,
        source_maps: true,
        cache: true,
        node: "node".to_owned(),
        node_args: Vec::new(),
        entry: String::new(),
        args: Vec::new(),
    };
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("ts-r-run: {flag} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (a, None),
        };
        match flag {
            "--watch" => cli.watch = true,
            "--no-source-maps" => cli.source_maps = false,
            "--no-cache" => cli.cache = false,
            "-h" | "--help" => cli.mode = Mode::Help,
            "-V" | "--version" => cli.mode = Mode::Version,
            "--serve" => cli.mode = Mode::Serve,
            "--tsconfig" | "--strategy" | "--node" | "--compile" | "--resolve" => {
                let v = match inline {
                    Some(v) => v,
                    None => value(&mut i, flag)?,
                };
                match flag {
                    "--tsconfig" => cli.tsconfig = Some(v),
                    "--node" => cli.node = v,
                    "--compile" => cli.mode = Mode::Compile(v),
                    "--resolve" => cli.mode = Mode::Resolve(v),
                    _ => {
                        cli.strategy = match v.as_str() {
                            "prebuild" => Strategy::Prebuild,
                            "hook" => Strategy::Hook,
                            _ => return Err(format!("ts-r-run: unknown strategy '{v}'")),
                        }
                    }
                }
            }
            "--" => {
                i += 1;
                break;
            }
            f if f.starts_with('-') && f.len() > 1 => {
                cli.node_args.push(a.to_owned());
                if inline.is_none() && NODE_VALUE_FLAGS.contains(&f) {
                    cli.node_args.push(value(&mut i, f)?);
                }
            }
            _ => break,
        }
        i += 1;
    }
    if let Some(entry) = args.get(i) {
        cli.entry = entry.clone();
        cli.args = args[i + 1..].to_vec();
    } else if cli.mode == Mode::Run {
        return Err(format!("ts-r-run: no file given\n\n{HELP}"));
    }
    Ok(cli)
}

/// Entry point; returns the process exit code.
pub fn main_with(args: &[String]) -> i32 {
    let cli = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    match &cli.mode {
        Mode::Help => {
            print!("{HELP}");
            0
        }
        Mode::Version => {
            println!("ts-r-run {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Mode::Compile(file) => compile_to_stdout(&cli, file),
        Mode::Resolve(parent) => resolve_to_stdout(&cli, parent),
        Mode::Serve => {
            let engine = engine_from_env(&cli);
            match serve::serve(&engine) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        Mode::Run => run(&cli),
    }
}

/// The project for the internal modes: the run's entry and tsconfig come from the parent.
fn engine_from_env(cli: &Cli) -> Engine {
    let entry = std::env::var("TS_R_RUN_ENTRY").unwrap_or_else(|_| absolute(&cli.entry));
    let project = Project::load(&entry, tsconfig_from_env(cli).as_deref());
    Engine::new(project, cli.cache, cli.source_maps)
}

fn tsconfig_from_env(cli: &Cli) -> Option<String> {
    std::env::var("TS_R_RUN_TSCONFIG")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| cli.tsconfig.clone())
}

/// For the preload: a bare specifier Node couldn't resolve, imported from a file the graph
/// walk didn't compile (plain JavaScript), still goes through tsconfig `paths`, as under tsx.
fn resolve_to_stdout(cli: &Cli, parent: &str) -> i32 {
    let parent = absolute(parent);
    let entry = std::env::var("TS_R_RUN_ENTRY")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| parent.clone());
    let project = Project::load(&entry, tsconfig_from_env(cli).as_deref());
    if let Some(file) = project.resolve(&cli.entry, &parent, false) {
        println!("{file}");
    }
    0
}

fn compile_to_stdout(cli: &Cli, file: &str) -> i32 {
    use std::io::Write as _;
    let mut cli = cli.clone();
    if cli.entry.is_empty() {
        cli.entry = file.to_owned();
    }
    let engine = engine_from_env(&cli);
    match engine.compile_file(&absolute(file)) {
        Ok(u) => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", u.compiled.format.as_str());
            let _ = out.write_all(u.compiled.code.as_bytes());
            engine.flush();
            0
        }
        Err(e) => {
            eprint!("{e}");
            1
        }
    }
}

/// Writes the preload scripts (once per content) and returns the directory holding them.
pub fn hooks_dir(root: &Path) -> std::io::Result<PathBuf> {
    let key = ts_r_cache::KeyBuilder::new("ts-r-run/hooks")
        .str(HOOKS)
        .str(PRELOAD_ASYNC)
        .str(HOOKS_ASYNC)
        .key()
        .to_hex();
    let dir = root.join(format!("hooks-{}", &key[..16]));
    manifest::write_once(&dir, "hooks.mjs", HOOKS.as_bytes())?;
    manifest::write_once(&dir, "hooks-async.mjs", HOOKS_ASYNC.as_bytes())?;
    manifest::write_once(&dir, "preload-async.mjs", PRELOAD_ASYNC.as_bytes())?;
    Ok(dir)
}

/// What to start Node with.
#[derive(Debug)]
pub struct Launch<'a> {
    pub cli: &'a Cli,
    pub entry: &'a str,
    pub preload: PathBuf,
    pub manifest: Option<PathBuf>,
}

impl Launch<'_> {
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.cli.node);
        c.args(&self.cli.node_args);
        c.arg("--import").arg(import_specifier(&self.preload));
        c.arg(self.entry).args(&self.cli.args);
        if let Some(m) = &self.manifest {
            c.env("TS_R_RUN_MANIFEST", m);
        }
        if let Ok(exe) = std::env::current_exe() {
            c.env("TS_R_RUN_BIN", exe);
        }
        c.env("TS_R_RUN_ENTRY", self.entry);
        c.env(
            "TS_R_RUN_TSCONFIG",
            self.cli
                .tsconfig
                .as_deref()
                .map(absolute)
                .unwrap_or_default(),
        );
        c.env(
            "TS_R_RUN_SOURCE_MAPS",
            if self.cli.source_maps { "1" } else { "0" },
        );
        c
    }
}

/// The preload as an `--import` specifier. Node reads it as a URL, so on Windows `C:\x`
/// would be a URL with scheme `c:`: there it becomes `file:///C:/x`. Unix paths are given
/// as they are (`/x` is a valid specifier). Made absolute first: a relative cache root
/// (`TS_R_CACHE_DIR=cache`) would be a package name, or a URL without its drive.
fn import_specifier(path: &Path) -> std::ffi::OsString {
    let path = &std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    if cfg!(windows) {
        file_url(&path.to_string_lossy()).into()
    } else {
        path.as_os_str().to_owned()
    }
}

/// `C:\a b\x.mjs` → `file:///C:/a%20b/x.mjs`; `\\srv\share\x` → `file://srv/share/x`.
fn file_url(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let p = path.replace('\\', "/");
    let (prefix, rest) = match p.strip_prefix("//") {
        Some(unc) => ("file://", unc),
        None => ("file:///", p.trim_start_matches('/')),
    };
    let mut out = String::from(prefix);
    for b in rest.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Replaces this process with `cmd` (Unix) or runs it and returns its exit code.
fn exec(mut cmd: Command) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let err = cmd.exec();
        eprintln!("ts-r-run: cannot start node: {err}");
        127
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("ts-r-run: cannot start node: {e}");
                127
            }
        }
    }
}

fn state_dir(cli: &Cli) -> PathBuf {
    if cli.cache {
        manifest::default_dir()
    } else {
        std::env::temp_dir().join(format!("ts-r-run-{}", std::process::id()))
    }
}

/// Compiles the graph of `entry` and writes its manifest. `Err` holds rendered errors.
pub fn prepare(
    cli: &Cli,
    engine: &Engine,
    entry: &str,
    dir: &Path,
) -> Result<(PathBuf, graph::Graph), String> {
    let g = if is_compiled_path(entry) {
        graph::build(engine, entry, graph::default_threads())
    } else {
        graph::Graph::default()
    };
    if !g.errors.is_empty() {
        return Err(g.errors.concat());
    }
    let m = manifest::write(dir, &g)
        .map_err(|e| format!("ts-r-run: cannot write {}: {e}\n", dir.display()))?;
    if cli.cache {
        let fp = graph::build_fingerprint();
        let key = manifest::stamp_key(&fp, entry, cli.tsconfig.as_deref(), cli.source_maps);
        let inputs: Vec<&str> = g.inputs.iter().map(String::as_str).collect();
        let _ = manifest::write_stamp(dir, &key, &m, &inputs);
    }
    Ok((m, g))
}

fn run(cli: &Cli) -> i32 {
    let entry = absolute(&cli.entry);
    if !Path::new(&entry).is_file() {
        eprintln!("ts-r-run: cannot find {}", cli.entry);
        return 1;
    }
    let dir = state_dir(cli);
    let hooks = match hooks_dir(&dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("ts-r-run: cannot write to {}: {e}", dir.display());
            return 1;
        }
    };
    if cli.watch {
        return watch::run(cli, &entry, &dir, &hooks);
    }
    if cli.strategy == Strategy::Hook {
        let launch = Launch {
            cli,
            entry: &entry,
            preload: hooks.join("preload-async.mjs"),
            manifest: None,
        };
        return exec(launch.command());
    }
    let preload = hooks.join("hooks.mjs");
    if cli.cache {
        let fp = graph::build_fingerprint();
        let key = manifest::stamp_key(&fp, &entry, cli.tsconfig.as_deref(), cli.source_maps);
        if let Some(m) = manifest::check_stamp(&dir, &key) {
            let launch = Launch {
                cli,
                entry: &entry,
                preload,
                manifest: Some(m),
            };
            return exec(launch.command());
        }
    }
    let engine = Engine::new(
        Project::load(&entry, cli.tsconfig.as_deref()),
        cli.cache,
        cli.source_maps,
    );
    let manifest = match prepare(cli, &engine, &entry, &dir) {
        Ok((m, _)) => m,
        Err(e) => {
            eprint!("{e}");
            return 1;
        }
    };
    let launch = Launch {
        cli,
        entry: &entry,
        preload,
        manifest: Some(manifest),
    };
    if cli.cache {
        exec(launch.command())
    } else {
        // Nothing may stay behind: run, then delete the temporary directory.
        let code = match launch.command().status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("ts-r-run: cannot start node: {e}");
                127
            }
        };
        let _ = std::fs::remove_dir_all(&dir);
        code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn preload_file_urls() {
        assert_eq!(
            file_url(r"C:\Users\a b\hooks.mjs"),
            "file:///C:/Users/a%20b/hooks.mjs"
        );
        assert_eq!(file_url(r"\\srv\share\h.mjs"), "file://srv/share/h.mjs");
        assert_eq!(file_url("/tmp/h#1.mjs"), "file:///tmp/h%231.mjs");
        let rel = Path::new("cache").join("hooks.mjs");
        let spec = PathBuf::from(import_specifier(&rel));
        let want = std::env::current_dir().unwrap().join(&rel);
        if cfg!(windows) {
            assert!(spec.to_string_lossy().starts_with("file:///"), "{spec:?}");
        } else {
            assert_eq!(spec, want);
        }
    }

    #[test]
    fn parses_options_node_flags_and_script_args() {
        let c = parse_args(&args(
            "--watch --inspect -r dotenv/config --tsconfig t.json app.ts --port 3 -x",
        ))
        .unwrap();
        assert!(c.watch);
        assert_eq!(c.tsconfig.as_deref(), Some("t.json"));
        assert_eq!(c.node_args, args("--inspect -r dotenv/config"));
        assert_eq!(c.entry, "app.ts");
        assert_eq!(c.args, args("--port 3 -x"));
        let c = parse_args(&args("--strategy=hook --no-cache -- --weird.ts a")).unwrap();
        assert_eq!(c.strategy, Strategy::Hook);
        assert!(!c.cache);
        assert_eq!(c.entry, "--weird.ts");
        assert!(parse_args(&args("--strategy nope a.ts")).is_err());
        assert!(parse_args(&args("--watch")).is_err());
        assert_eq!(parse_args(&args("--help")).unwrap().mode, Mode::Help);
    }
}
