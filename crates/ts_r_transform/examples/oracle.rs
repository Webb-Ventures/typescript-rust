//! Compares ts-r's transpile output with tsc's on fixture files.
//!
//! ```text
//! scripts/limit.sh --mem-mb 4096 --time-s 600 -- \
//!   cargo run --release -p ts_r_transform --example oracle -- [--bless] [--show N] <file|dir>...
//! ```
//!
//! A fixture is a `.ts`/`.tsx`/`.mts`/`.cts` file whose leading `// @option: value` lines are
//! compiler options (TypeScript test-case style). Fixtures with the same options share one
//! `tsc -p` run (`noCheck`, into a temporary directory). Outputs are compared by AST shape
//! after parsing both as JavaScript; mismatches print the first differing lines.
//!
//! `--bless` writes tsc's output next to each fixture as `<name>.tsc.js` (used by
//! `tests/fixtures.rs`, so `cargo test` needs no tsc). `--stored` compares against those files
//! instead of running tsc.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use ts_r_codegen::CodegenOptions;
use ts_r_transform::{TransformOptions, transpile};

ts_r_testing::cap_memory!();

#[path = "../tests/support/mod.rs"]
mod support;

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        entries.sort();
        for e in entries {
            walk(&e, out);
        }
    } else if support::is_fixture(p) {
        out.push(p.to_path_buf());
    }
}

fn tsc_command() -> Command {
    if let Ok(t) = std::env::var("TSC") {
        return Command::new(t);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for dir in ["1ac1eddd0355a233", "d08dbff2f92d3e31"] {
        let p = format!("{home}/.npm/_npx/{dir}/node_modules/.bin/tsc");
        if Path::new(&p).exists() {
            return Command::new(p);
        }
    }
    let mut c = Command::new("npx");
    c.args(["-y", "-p", "typescript@7.0.2", "tsc"]);
    c
}

/// Runs tsc over a group of fixtures with the same options; returns outputs by fixture.
fn run_tsc(json: &str, files: &[PathBuf]) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    let dir = std::env::temp_dir().join(format!(
        "ts-r-oracle-{}-{}",
        std::process::id(),
        support::hash_str(json)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let src = dir.join("src");
    if std::fs::create_dir_all(&src).is_err() {
        return out;
    }
    let mut names = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let sub = src.join(format!("f{i}"));
        let _ = std::fs::create_dir_all(&sub);
        let name = f.file_name().unwrap_or_default();
        let _ = std::fs::copy(f, sub.join(name));
        names.push(format!("src/f{i}/{}", name.to_string_lossy()));
    }
    let mut opts = json.trim().trim_end_matches('}').trim().to_owned();
    if opts.len() > 1 {
        opts.push(',');
    }
    let config = format!(
        "{opts} \"noCheck\": true, \"outDir\": \"out\", \"rootDir\": \"src\", \"declaration\": false, \"noEmit\": false, \"noEmitOnError\": false, \"skipLibCheck\": true, \"types\": [] }}"
    );
    let files_json: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    let tsconfig = format!(
        "{{ \"compilerOptions\": {config}, \"files\": [{}] }}",
        files_json.join(", ")
    );
    let _ = std::fs::write(dir.join("tsconfig.json"), tsconfig);
    let _ = tsc_command()
        .arg("-p")
        .arg(dir.join("tsconfig.json"))
        .current_dir(&dir)
        .output();
    for (i, f) in files.iter().enumerate() {
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let js = support::js_name(&name);
        let p = dir.join("out").join(format!("f{i}")).join(&js);
        let jsx = p.with_extension("jsx");
        if let Ok(s) = std::fs::read_to_string(&p).or_else(|_| std::fs::read_to_string(&jsx)) {
            out.insert(f.clone(), s);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn main() {
    let mut paths = Vec::new();
    let mut bless = false;
    let mut stored = false;
    let mut show = 20usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bless" => bless = true,
            "--stored" => stored = true,
            "--show" => show = args.next().and_then(|s| s.parse().ok()).unwrap_or(20),
            _ => walk(Path::new(&a), &mut paths),
        }
    }
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for p in &paths {
        let src = std::fs::read_to_string(p).unwrap_or_default();
        groups
            .entry(support::options_json(&src))
            .or_default()
            .push(p.clone());
    }
    let (mut ok, mut bad, mut missing) = (0usize, 0usize, 0usize);
    let mut shown = 0usize;
    for (json, files) in &groups {
        let expected = if stored {
            files
                .iter()
                .filter_map(|f| {
                    std::fs::read_to_string(support::stored_path(f))
                        .ok()
                        .map(|s| (f.clone(), s))
                })
                .collect()
        } else {
            run_tsc(json, files)
        };
        for f in files {
            let Some(theirs) = expected.get(f) else {
                missing += 1;
                eprintln!("no tsc output: {}", f.display());
                continue;
            };
            if bless {
                let _ = std::fs::write(support::stored_path(f), theirs);
            }
            let src = std::fs::read_to_string(f).unwrap_or_default();
            let path = f.to_string_lossy().to_string();
            let opts = TransformOptions::from_json(json, &path);
            let ours = transpile(&path, &src, &opts, &CodegenOptions::default());
            match support::compare(&ours.code, theirs) {
                None => ok += 1,
                Some(diff) => {
                    bad += 1;
                    if shown < show {
                        shown += 1;
                        println!("MISMATCH {} {json}\n{diff}", f.display());
                    }
                }
            }
        }
    }
    println!("oracle: {ok} match, {bad} mismatch, {missing} without tsc output");
    if bad > 0 {
        std::process::exit(1);
    }
}
