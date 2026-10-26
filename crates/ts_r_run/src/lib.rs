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

#[cfg(unix)]
mod unix;

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compile::is_compiled_path;
use crate::graph::Engine;
use crate::project::{Project, absolute};

const HOOKS: &str = include_str!("js/hooks.mjs");
const PRELOAD_ASYNC: &str = include_str!("js/preload-async.mjs");
const HOOKS_ASYNC: &str = include_str!("js/hooks-async.mjs");
const RELAY: &str = include_str!("js/relay.mjs");

const HELP: &str = "ts-r-run: run a TypeScript file with Node

Usage: ts-r-run [options] [node options] <file> [args...]
       ts-r-run [options] [node options] -e|-p <code> [args...]
       <command> | ts-r-run [options] [node options] [-] [args...]
       ts-r-run [options] --test [node options]   (Node's test runner, default files)

Options:
  --watch               Restart when a file of the module graph changes
  --tsconfig <file>     Use this tsconfig (default: nearest tsconfig.json above <file>)
  --no-source-maps      Don't map stack traces back to the TypeScript source
  --no-cache            Don't read or write the transpile cache (~/.cache/ts-r)
  --strategy <s>        prebuild (default): compile the graph, then start Node
                        hook: compile on demand from Node's module.register hooks
  --node <path>         Node binary (default: node on PATH)
  -e, --eval <code>     Run TypeScript code (CommonJS unless --input-type=module
                        or top-level await); with no file, code is read from stdin
  -p, --print <code>    The same, printing the value of the last expression
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
    /// Internal: compile one file to stdout (`esm`/`cjs` line, then the code); `true`
    /// (`--compile-cjs`) when it is loaded with `require()`.
    Compile(String, bool),
    /// Internal: resolve the specifier (the positional argument) imported from this parent
    /// through tsconfig `paths` and `baseUrl`, printing the file or nothing.
    Resolve(String),
    /// Internal: the strategy "hook" compiler process.
    Serve,
}

/// Where the code of an eval run comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalSource {
    /// `-e`/`--eval`/`-p`/`--print` on the command line.
    Arg(String),
    /// Read from stdin: no file was given (or the file is `-`).
    Stdin,
}

/// Code run instead of a file, as `node --eval` runs it (tsx's `--eval`, `--print` and
/// piped input).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eval {
    pub source: EvalSource,
    /// `-p`/`--print`: Node prints the value of the last expression.
    pub print: bool,
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
    /// Code to run instead of `entry` (which is then empty).
    pub eval: Option<Eval>,
}

impl Cli {
    /// Node's test runner is asked for (`--test` among the Node flags).
    pub fn runs_tests(&self) -> bool {
        self.node_args.iter().any(|a| a == "--test")
    }
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
    "--input-type",
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
        eval: None,
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
            "-e" | "--eval" | "-p" | "--print" | "-pe" | "-ep" => {
                let code = match inline {
                    Some(v) => v,
                    None => value(&mut i, flag)?,
                };
                let print = flag.contains('p') || cli.eval.as_ref().is_some_and(|e| e.print);
                cli.eval = Some(Eval {
                    source: EvalSource::Arg(code),
                    print,
                });
            }
            "--tsconfig" | "--strategy" | "--node" | "--compile" | "--compile-cjs"
            | "--resolve" => {
                let v = match inline {
                    Some(v) => v,
                    None => value(&mut i, flag)?,
                };
                match flag {
                    "--tsconfig" => cli.tsconfig = Some(v),
                    "--node" => cli.node = v,
                    "--compile" => cli.mode = Mode::Compile(v, false),
                    "--compile-cjs" => cli.mode = Mode::Compile(v, true),
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
    if cli.mode == Mode::Run && cli.eval.is_none() && args.get(i).is_none_or(|a| a == "-") {
        if cli.watch {
            return Err(format!("ts-r-run: no file given\n\n{HELP}"));
        }
        // Node's test runner finds the test files itself; otherwise the code comes from
        // stdin (`run_eval` says "no file given" when stdin is a terminal).
        if !cli.runs_tests() || args.get(i).is_some() {
            cli.eval = Some(Eval {
                source: EvalSource::Stdin,
                print: false,
            });
        }
    }
    if cli.eval.is_some() && cli.mode == Mode::Run {
        if cli.watch {
            return Err("ts-r-run: --watch needs a file, not --eval or stdin".to_owned());
        }
        // As under Node: every argument after the code is the program's (`-` included).
        cli.args = args[i.min(args.len())..].to_vec();
    } else if let Some(entry) = args.get(i) {
        cli.entry = entry.clone();
        cli.args = args[i + 1..].to_vec();
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
        Mode::Compile(file, cjs) => compile_to_stdout(&cli, file, *cjs),
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
    let entry = std::env::var("TS_R_RUN_ENTRY")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| absolute(&cli.entry));
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

fn compile_to_stdout(cli: &Cli, file: &str, as_cjs: bool) -> i32 {
    use std::io::Write as _;
    let mut cli = cli.clone();
    if cli.entry.is_empty() {
        cli.entry = file.to_owned();
    }
    let engine = engine_from_env(&cli);
    match engine.compile_file(&absolute(file), as_cjs) {
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
        .str(RELAY)
        .key()
        .to_hex();
    let dir = root.join(format!("hooks-{}", &key[..16]));
    manifest::write_once(&dir, "hooks.mjs", HOOKS.as_bytes())?;
    manifest::write_once(&dir, "hooks-async.mjs", HOOKS_ASYNC.as_bytes())?;
    manifest::write_once(&dir, "preload-async.mjs", PRELOAD_ASYNC.as_bytes())?;
    manifest::write_once(&dir, "relay.mjs", RELAY.as_bytes())?;
    Ok(dir)
}

/// What to start Node with.
#[derive(Debug)]
pub struct Launch<'a> {
    pub cli: &'a Cli,
    pub entry: &'a str,
    pub preload: PathBuf,
    pub manifest: Option<PathBuf>,
    /// Compiled code Node runs in place of the entry; `entry` then only locates the
    /// tsconfig of files the preload compiles.
    pub eval: Option<Code>,
}

/// Compiled code given to Node instead of a file.
#[derive(Debug)]
pub enum Code {
    /// `--eval <code>`.
    Eval(String),
    /// `--print <code>`.
    Print(String),
    /// On Node's stdin (the caller sets it), so the module is `[stdin]` as under tsx.
    Stdin,
}

impl Launch<'_> {
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.cli.node);
        // Node's test runner reports where each test was declared from the stack of the
        // `test()` call; without Node's own source-map support that is a line of the
        // compiled code. Put the flag first so a later `--no-enable-source-maps` still wins.
        if self.cli.source_maps && self.cli.runs_tests() {
            c.arg("--enable-source-maps");
        }
        c.args(&self.cli.node_args);
        c.arg("--import").arg(import_specifier(&self.preload));
        match &self.eval {
            Some(Code::Eval(code)) => {
                c.arg("--eval").arg(code);
            }
            Some(Code::Print(code)) => {
                c.arg("--print").arg(code);
            }
            Some(Code::Stdin) => {}
            None if !self.entry.is_empty() => {
                c.arg(self.entry);
            }
            None => {}
        }
        c.args(&self.cli.args);
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

/// Runs Node and returns the exit code to leave with. On Unix ts-r-run stays the parent and
/// relays signals (`unix.rs`); elsewhere it waits for the child.
fn run_node(cmd: Command) -> i32 {
    #[cfg(unix)]
    {
        unix::run_node(cmd)
    }
    #[cfg(not(unix))]
    {
        let mut cmd = cmd;
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
    if let Some(ev) = &cli.eval {
        return run_eval(cli, ev);
    }
    if cli.entry.is_empty() {
        return run_tests(cli);
    }
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
            eval: None,
        };
        return run_node(launch.command());
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
                eval: None,
            };
            return run_node(launch.command());
        }
    }
    // The engine (and the graph) are dropped before Node starts: this process stays alive
    // as Node's parent for the whole run.
    let manifest = {
        let engine = Engine::new(
            Project::load(&entry, cli.tsconfig.as_deref()),
            cli.cache,
            cli.source_maps,
        );
        match prepare(cli, &engine, &entry, &dir) {
            Ok((m, _)) => m,
            Err(e) => {
                eprint!("{e}");
                return 1;
            }
        }
    };
    let launch = Launch {
        cli,
        entry: &entry,
        preload,
        manifest: Some(manifest),
        eval: None,
    };
    let code = run_node(launch.command());
    if !cli.cache {
        // Nothing may stay behind: delete the temporary directory.
        let _ = std::fs::remove_dir_all(&dir);
    }
    code
}

/// The module format `--input-type` asks for, and the value Node is given in its place (the
/// code is JavaScript once compiled, so `*-typescript` becomes the plain type).
fn input_type(value: &str) -> Option<compile::Format> {
    match value {
        "module" | "module-typescript" => Some(compile::Format::Esm),
        "commonjs" | "commonjs-typescript" => Some(compile::Format::Cjs),
        _ => None,
    }
}

/// Splits `--input-type` out of the Node flags: the format it names (when it is one Node
/// knows) and the other flags. An unknown value stays for Node to reject.
fn split_input_type(node_args: &[String]) -> (Option<compile::Format>, Vec<String>) {
    let mut format = None;
    let mut rest = Vec::with_capacity(node_args.len());
    let mut i = 0;
    while i < node_args.len() {
        let a = &node_args[i];
        let (value, width) = match a.strip_prefix("--input-type=") {
            Some(v) => (Some(v), 1),
            None if a == "--input-type" => (node_args.get(i + 1).map(String::as_str), 2),
            None => (None, 1),
        };
        match value.and_then(input_type) {
            Some(f) => format = Some(f),
            None => rest.extend(
                node_args[i..(i + width).min(node_args.len())]
                    .iter()
                    .cloned(),
            ),
        }
        i += width;
    }
    (format, rest)
}

/// `-e`/`--eval`, `-p`/`--print` or code piped to stdin, as tsx runs them: the code is
/// compiled as a TypeScript file named `[eval]` (`[stdin]`) in the working directory, which
/// is where its imports and its tsconfig are looked up from, and Node gets the JavaScript
/// with `--eval`/`--print`. It is CommonJS (an `import` becomes `require`) unless
/// `--input-type=module` (or `module-typescript`) is given or it has top-level `await`;
/// Node is told which with `--input-type`. Files it loads are compiled by the preload when
/// they load (there is no graph to build first, as with `--test`).
fn run_eval(cli: &Cli, ev: &Eval) -> i32 {
    use std::io::{IsTerminal as _, Read as _};
    let (name, code) = match &ev.source {
        EvalSource::Arg(code) => ("[eval]", code.clone()),
        EvalSource::Stdin => {
            let mut stdin = std::io::stdin();
            if stdin.is_terminal() {
                eprintln!("ts-r-run: no file given\n\n{HELP}");
                return 2;
            }
            let mut code = String::new();
            if let Err(e) = stdin.read_to_string(&mut code) {
                eprintln!("ts-r-run: cannot read stdin: {e}");
                return 1;
            }
            ("[stdin]", code)
        }
    };
    let (asked, node_args) = split_input_type(&cli.node_args);
    let ext = match asked {
        Some(compile::Format::Esm) => "mts",
        Some(compile::Format::Cjs) => "cts",
        None => "ts",
    };
    // The working directory as every other path here is written (`/` separators).
    let path = absolute(&format!("{name}.{ext}"));
    let compiled = {
        let project = Project::load(&path, cli.tsconfig.as_deref());
        if !project.config_errors.is_empty() {
            eprint!("{}", project.config_errors);
            return 1;
        }
        // No source map: there is no file for Node to map the code back to.
        let engine = Engine::new(project, cli.cache, false);
        // `parse<T>` call sites are expanded as in a file (`Engine::compile_file`).
        let code = match engine.guard.expand(&path, &code) {
            Some(x) if !x.errors.is_empty() => {
                x.errors
                    .iter()
                    .for_each(|e| eprintln!("{}", e.render(&path)));
                return 1;
            }
            Some(x) => x.code,
            None => code,
        };
        let compiled = engine.compile_source(
            &path,
            &code,
            compile::PackageType::Unspecified,
            asked.is_none(),
        );
        engine.flush();
        match compiled {
            Ok(c) => c,
            Err(e) => {
                eprint!("{e}");
                return 1;
            }
        }
    };
    let dir = state_dir(cli);
    let hooks = match hooks_dir(&dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("ts-r-run: cannot write to {}: {e}", dir.display());
            return 1;
        }
    };
    let preload = match cli.strategy {
        Strategy::Prebuild => hooks.join("hooks.mjs"),
        Strategy::Hook => hooks.join("preload-async.mjs"),
    };
    let mut cli = cli.clone();
    cli.node_args = node_args;
    cli.node_args.push(format!(
        "--input-type={}",
        match compiled.format {
            compile::Format::Esm => "module",
            compile::Format::Cjs => "commonjs",
        }
    ));
    // Piped code goes back to Node on its stdin (from a file, which is deleted after the
    // run), so `__filename` and stack traces say `[stdin]`, as under Node and tsx.
    let stdin_file = dir.join(format!("stdin-{}.js", std::process::id()));
    let eval = match (&ev.source, ev.print) {
        (EvalSource::Stdin, _) => {
            if let Err(e) = std::fs::write(&stdin_file, &compiled.code) {
                eprintln!("ts-r-run: cannot write {}: {e}", stdin_file.display());
                return 1;
            }
            Code::Stdin
        }
        (EvalSource::Arg(_), true) => Code::Print(compiled.code),
        (EvalSource::Arg(_), false) => Code::Eval(compiled.code),
    };
    let piped = matches!(eval, Code::Stdin);
    let launch = Launch {
        cli: &cli,
        entry: &path,
        preload,
        manifest: None,
        eval: Some(eval),
    };
    let mut cmd = launch.command();
    if piped {
        match std::fs::File::open(&stdin_file) {
            Ok(f) => {
                cmd.stdin(f);
            }
            Err(e) => {
                eprintln!("ts-r-run: cannot read {}: {e}", stdin_file.display());
                return 1;
            }
        }
    }
    let code = run_node(cmd);
    if piped {
        let _ = std::fs::remove_file(&stdin_file);
    }
    if !cli.cache {
        let _ = std::fs::remove_dir_all(&dir);
    }
    code
}

/// `ts-r-run --test` with no file: Node's test runner finds the test files with its default
/// patterns (`.ts` included, as Node strips types by default) and runs each in a child that
/// inherits the preload; with no manifest the preload compiles every file when it is loaded.
fn run_tests(cli: &Cli) -> i32 {
    let dir = state_dir(cli);
    let hooks = match hooks_dir(&dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("ts-r-run: cannot write to {}: {e}", dir.display());
            return 1;
        }
    };
    let preload = match cli.strategy {
        Strategy::Prebuild => hooks.join("hooks.mjs"),
        Strategy::Hook => hooks.join("preload-async.mjs"),
    };
    let launch = Launch {
        cli,
        entry: "",
        preload,
        manifest: None,
        eval: None,
    };
    let code = run_node(launch.command());
    if !cli.cache {
        let _ = std::fs::remove_dir_all(&dir);
    }
    code
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

    #[test]
    fn test_runner_gets_node_source_maps_first() {
        let argv = |cli: &Cli| -> Vec<String> {
            let launch = Launch {
                cli,
                entry: "/p/t.ts",
                preload: PathBuf::from("/h/hooks.mjs"),
                manifest: None,
                eval: None,
            };
            let c = launch.command();
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let c = parse_args(&args("--test --no-enable-source-maps t.ts")).unwrap();
        let a = argv(&c);
        assert_eq!(
            a[..3],
            args("--enable-source-maps --test --no-enable-source-maps")
        );
        let c = parse_args(&args("--no-source-maps --test t.ts")).unwrap();
        assert_eq!(argv(&c)[0], "--test");
        let c = parse_args(&args("--inspect t.ts")).unwrap();
        assert_eq!(argv(&c)[0], "--inspect");
    }

    #[test]
    fn test_runner_needs_no_file() {
        let c = parse_args(&args("--test --test-reporter=dot")).unwrap();
        assert_eq!(c.entry, "");
        let launch = Launch {
            cli: &c,
            entry: "",
            preload: PathBuf::from("/h/hooks.mjs"),
            manifest: None,
            eval: None,
        };
        let a: Vec<String> = launch
            .command()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The preload is the last argument: no entry follows it.
        assert!(a.last().is_some_and(|l| l.ends_with("hooks.mjs")), "{a:?}");
        assert!(parse_args(&args("--watch --test")).is_err());
        assert_eq!(c.eval, None);
    }

    #[test]
    fn eval_print_and_stdin() {
        let c = parse_args(&args("--inspect -e 1+1 a -x")).unwrap();
        let arg = |code: &str, print| {
            Some(Eval {
                source: EvalSource::Arg(code.to_owned()),
                print,
            })
        };
        assert_eq!(c.eval, arg("1+1", false));
        assert_eq!((c.entry.as_str(), c.args.clone()), ("", args("a -x")));
        assert_eq!(c.node_args, args("--inspect"));
        assert_eq!(parse_args(&args("--print=x")).unwrap().eval, arg("x", true));
        assert_eq!(parse_args(&args("-pe x")).unwrap().eval, arg("x", true));
        let stdin = Some(Eval {
            source: EvalSource::Stdin,
            print: false,
        });
        // No file: the code comes from stdin, and `-` is an argument of the program.
        assert_eq!(parse_args(&args("--inspect")).unwrap().eval, stdin);
        let c = parse_args(&args("- q")).unwrap();
        assert_eq!((c.eval, c.args), (stdin, args("- q")));
        assert!(parse_args(&args("--watch -e 1")).is_err());
        assert!(parse_args(&args("-e")).is_err());
        let c = parse_args(&args("--input-type module -e 1")).unwrap();
        assert_eq!(c.node_args, args("--input-type module"));
        assert_eq!(c.entry, "");
    }

    #[test]
    fn input_type_is_taken_from_node_flags() {
        let (f, rest) = split_input_type(&args("--a --input-type=module-typescript --b"));
        assert_eq!((f, rest), (Some(compile::Format::Esm), args("--a --b")));
        let (f, rest) = split_input_type(&args("--input-type commonjs"));
        assert_eq!((f, rest), (Some(compile::Format::Cjs), vec![]));
        // Unknown values (and a missing one) are left for Node to report.
        let (f, rest) = split_input_type(&args("--input-type=wasm --input-type"));
        assert_eq!((f, rest), (None, args("--input-type=wasm --input-type")));
    }

    #[test]
    fn eval_code_takes_the_place_of_the_entry() {
        let c = parse_args(&args("-p 1 a")).unwrap();
        let launch = Launch {
            cli: &c,
            entry: "/p/[eval].ts",
            preload: PathBuf::from("/h/hooks.mjs"),
            manifest: None,
            eval: Some(Code::Print("1;".to_owned())),
        };
        let a: Vec<String> = launch
            .command()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The preload as the code passes it (a `file:///D:/...` URL on Windows).
        let preload = import_specifier(Path::new("/h/hooks.mjs"))
            .to_string_lossy()
            .into_owned();
        let want = ["--import", &preload, "--print", "1;", "a"];
        assert_eq!(a, want);
    }
}
