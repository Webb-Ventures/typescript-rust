//! `--watch`: run, and restart Node whenever a file of the module graph (or a tsconfig /
//! `package.json` that shaped it) changes. Unchanged files come from the transpile cache, so a
//! restart recompiles only what changed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::graph::{self, Engine};
use crate::project::Project;
use crate::{Cli, Launch, manifest};

/// Asks `child` to stop (SIGTERM on Unix), then kills it after `grace`.
pub fn stop(child: &mut Child, grace: Duration) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let until = Instant::now() + grace;
        while Instant::now() < until {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = grace;
    let _ = child.kill();
    let _ = child.wait();
}

/// Every spelling of `inputs` an event may carry (as given, and with symlinks resolved).
fn input_set(inputs: &BTreeSet<String>) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for i in inputs {
        let p = PathBuf::from(i);
        if let Ok(c) = std::fs::canonicalize(&p) {
            out.insert(c);
        }
        out.insert(p);
    }
    out
}

fn is_input(set: &BTreeSet<PathBuf>, p: &Path) -> bool {
    set.contains(p) || std::fs::canonicalize(p).is_ok_and(|c| set.contains(&c))
}

pub fn run(cli: &Cli, entry: &str, dir: &Path, hooks: &Path) -> i32 {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                let _ = tx.send(p);
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            eprintln!("ts-r-run: cannot watch files: {e}");
            return 1;
        }
    };
    let mut watched: BTreeSet<PathBuf> = BTreeSet::new();
    let mut child: Option<Child> = None;
    loop {
        let engine = Engine::new(
            Project::load(entry, cli.tsconfig.as_deref()),
            cli.cache,
            cli.source_maps,
        );
        let g = if crate::compile::is_compiled_path(entry) {
            graph::build(&engine, entry, graph::default_threads())
        } else {
            graph::Graph::default()
        };
        let mut inputs = g.inputs.clone();
        inputs.insert(entry.to_owned());
        if g.errors.is_empty() {
            match manifest::write(dir, &g) {
                Ok(m) => {
                    let launch = Launch {
                        cli,
                        entry,
                        preload: hooks.join("hooks.mjs"),
                        manifest: Some(m),
                    };
                    let mut cmd = launch.command();
                    cmd.env("TS_R_RUN_WATCH_PARENT", std::process::id().to_string());
                    match cmd.spawn() {
                        Ok(c) => child = Some(c),
                        Err(e) => eprintln!("ts-r-run: cannot start node: {e}"),
                    }
                }
                Err(e) => eprintln!("ts-r-run: cannot write {}: {e}", dir.display()),
            }
        } else {
            eprint!("{}", g.errors.concat());
            eprintln!("[ts-r-run] waiting for changes");
        }
        // Watch the directory of every input (a save often replaces the file).
        let dirs: BTreeSet<PathBuf> = inputs
            .iter()
            .filter_map(|i| Path::new(i).parent().map(Path::to_path_buf))
            .collect();
        for d in dirs.difference(&watched.clone()) {
            if watcher.watch(d, RecursiveMode::NonRecursive).is_ok() {
                watched.insert(d.clone());
            }
        }
        let set = input_set(&inputs);
        let changed = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(p) => {
                    if is_input(&set, &p) {
                        break p;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return 1,
            }
            if let Some(c) = &mut child
                && let Ok(Some(status)) = c.try_wait()
            {
                let code = status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |c| format!("code {c}"));
                eprintln!("[ts-r-run] process exited with {code}; waiting for changes");
                child = None;
            }
        };
        // Let a burst of writes (formatters, git checkouts) settle.
        let settle = Instant::now() + Duration::from_millis(30);
        while let Some(left) = settle.checked_duration_since(Instant::now()) {
            if rx.recv_timeout(left).is_err() {
                break;
            }
        }
        if let Some(mut c) = child.take() {
            stop(&mut c, Duration::from_secs(3));
        }
        let shown = changed
            .strip_prefix(std::env::current_dir().unwrap_or_default())
            .unwrap_or(&changed)
            .display()
            .to_string();
        eprintln!("[ts-r-run] restarting: {shown} changed");
    }
}
