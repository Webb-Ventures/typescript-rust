//! `--watch`: build, then rebuild whenever an input file (or a `package.json` /
//! `tsconfig.json` next to one) changes. The [`BuildContext`] keeps modules and printed
//! fragments in memory, so a rebuild re-parses and re-prints only what changed.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher as _};
use ts_r_bundler::BuildContext;

use crate::args::{Cli, LogLevel};

pub fn run(cli: &Cli) -> i32 {
    let guard = crate::guard_fs(cli);
    let ctx = match BuildContext::new(cli.options.clone(), guard.clone()) {
        Ok(c) => c,
        Err(m) => {
            for m in m {
                eprintln!("{m}");
            }
            return 1;
        }
    };
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !matches!(ev.kind, EventKind::Access(_))
        {
            for p in ev.paths {
                let _ = tx.send(p);
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            eprintln!("ts-r-build: cannot watch files: {e}");
            return 1;
        }
    };
    let outdir = cli
        .options
        .outdir
        .as_ref()
        .map(|d| ts_r_config::path::resolve(&cli.options.working_dir, d));
    let mut watched: BTreeSet<String> = BTreeSet::new();
    loop {
        let start = Instant::now();
        let mut r = ctx.build();
        crate::add_guard_errors(&mut r, &guard, &cli.options.working_dir);
        let _ = crate::finish(cli, &r, start);
        if cli.log_level >= LogLevel::Info {
            eprintln!("[watch] build finished, watching for changes...");
        }
        let dirs = watch_dirs(&r.inputs, &guard.touched(), &cli.options.working_dir);
        for d in &dirs {
            if !watched.contains(d) {
                let _ = watcher.watch(std::path::Path::new(d), RecursiveMode::NonRecursive);
                watched.insert(d.clone());
            }
        }
        let inputs: BTreeSet<&str> = r.inputs.iter().map(String::as_str).collect();
        // Wait for a relevant change, then debounce.
        let mut changed: BTreeSet<String> = BTreeSet::new();
        while changed.is_empty() {
            let Ok(p) = rx.recv() else {
                return 0;
            };
            let s = ts_r_config::path::normalize_slashes(&p.to_string_lossy());
            if relevant(&s, &inputs, outdir.as_deref()) {
                changed.insert(s);
            }
        }
        let until = Instant::now() + Duration::from_millis(20);
        while let Ok(p) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            let s = ts_r_config::path::normalize_slashes(&p.to_string_lossy());
            if relevant(&s, &inputs, outdir.as_deref()) {
                changed.insert(s);
            }
        }
        // Validators depend on declarations no runtime import reaches, and on tsconfig
        // options (`paths`): any source or config change re-reads the files with `parse<T>`
        // calls and the validator modules.
        let config = changed.iter().any(|p| p.ends_with(".json"));
        if config {
            guard.forget_configs();
        } else {
            // Parsed copies of changed (or deleted) files are stale; the rest stay cached.
            guard.forget_files(&changed.iter().collect::<Vec<_>>());
        }
        if config
            || changed
                .iter()
                .any(|p| ts_r_guard::expand::is_expandable_path(p) || p.ends_with(".d.ts"))
        {
            changed.extend(guard.touched());
            changed.extend(
                r.inputs
                    .iter()
                    .filter(|p| p.starts_with(guard.vroot()))
                    .cloned(),
            );
        }
        let changed: Vec<String> = changed.into_iter().collect();
        if cli.log_level >= LogLevel::Info {
            eprintln!("[watch] build started (change: {:?})", changed[0]);
        }
        ctx.invalidate(&changed);
    }
}

/// The directories to watch (non-recursive): those of every input and of the declarations
/// validators were generated from (no runtime import reaches a type-only file), plus the
/// working directory so new files and config changes are seen.
fn watch_dirs(inputs: &[String], guard_files: &[String], wd: &str) -> BTreeSet<String> {
    let mut dirs: BTreeSet<String> = inputs
        .iter()
        .chain(guard_files)
        .map(|p| ts_r_config::path::directory_of(p).to_owned())
        .collect();
    dirs.insert(wd.to_owned());
    dirs
}

fn relevant(p: &str, inputs: &BTreeSet<&str>, outdir: Option<&str>) -> bool {
    if outdir.is_some_and(|d| p.starts_with(d)) {
        return false;
    }
    inputs.contains(p)
        || p.ends_with("/package.json")
        || p.ends_with("/tsconfig.json")
        || [
            ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".json", ".css",
        ]
        .iter()
        .any(|e| p.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watches_declarations_behind_validators() {
        // `main.ts` validates `User` from `types/user.ts`: the graph has only the virtual
        // validator module, so the declaration's directory comes from the guard's files.
        let inputs = [
            "/p/main.ts".to_owned(),
            "/p/.ts-r-guard/types/user.ts/User.js".to_owned(),
        ];
        let guard = ["/p/main.ts".to_owned(), "/p/types/user.ts".to_owned()];
        let dirs: Vec<String> = watch_dirs(&inputs, &guard, "/p").into_iter().collect();
        assert!(dirs.contains(&"/p/types".to_owned()), "{dirs:?}");
        assert!(dirs.contains(&"/p".to_owned()));
    }
}
