//! Type-aware linting on our own checker (design record 0006, "Type-aware rules").
//!
//! A project is a tsconfig: [`load_project`] loads its program and arranges it on the
//! process-wide shared levels of `ts_r_history` (lib and `node_modules` files are parsed,
//! bound and typed once per run and shared by every project with the same files and
//! options). [`check_and_lint`] checks every file of the project once on up to four threads,
//! each with one checker worker, and lints the requested files right after their check with
//! [`CkTypes`] over the same worker, so rules see exactly the types the check computed. The
//! check's diagnostics are the project's side of the [`parity`] comparison.

pub mod ck_types;
pub mod driver;
pub mod parity;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;
use ts_r_checker::checker::Ck;
use ts_r_checker::{Checker, Layer, LibStore, ProgramFiles, should_check};
use ts_r_config::FileSystem;
use ts_r_diagnostics::{Diagnostic, FileId, Files, format_plain};
use ts_r_span::SourceText;

pub use ck_types::CkTypes;

use crate::engine::{FileConfig, FileResult, lint_parsed};
use crate::types::FileKey;

/// Stack size of checking/linting threads (deep expressions recurse).
const STACK: usize = 256 << 20;

/// A tsconfig project loaded for type-aware linting.
#[derive(Debug)]
pub struct LoadedProject {
    pub config_path: String,
    /// The tsconfig and every config it extends (parity cache inputs).
    pub config_inputs: Vec<String>,
    /// Config diagnostics as tsc prints them (absolute paths).
    pub config_lines: Vec<String>,
    /// `incremental` or `composite`: tsc would write build info.
    pub incremental: bool,
    pub program: ProgramFiles,
    /// The shared level the project's checker builds on.
    pub parent: Option<Arc<Layer>>,
}

impl LoadedProject {
    /// Program file index of an absolute path.
    pub fn file_index(&self, path: &str) -> Option<usize> {
        self.program.file_index(path)
    }
}

/// Loads `config_path`'s program and arranges it on shared levels.
pub fn load_project<F: FileSystem>(
    fs: &F,
    config_path: &str,
    libs: &LibStore,
    threads: usize,
) -> LoadedProject {
    let parsed = ts_r_config::load_tsconfig(fs, config_path);
    let mut config_inputs = vec![parsed.config_path.clone()];
    config_inputs.extend(parsed.extended_config_paths.iter().cloned());
    let config_lines = parsed
        .diagnostics
        .iter()
        .map(|d| d.render(|p| p.to_owned(), |p| fs.read_file(p)))
        .collect();
    let incremental =
        parsed.options.incremental() == Some(true) || parsed.options.composite() == Some(true);
    let mut program =
        ts_r_checker::load_program(fs, &parsed.file_names, &parsed.options, Some(libs), threads);
    let arranged = ts_r_history::arrange(&mut program, &[]);
    LoadedProject {
        config_path: parsed.config_path,
        config_inputs,
        config_lines,
        incremental,
        program,
        parent: arranged.parent,
    }
}

/// A file to lint with types: its program file index and config.
#[derive(Clone, Debug)]
pub struct TypedFile {
    pub file: usize,
    pub cfg: Arc<FileConfig>,
}

/// What [`check_and_lint`] produced.
#[derive(Debug, Default)]
pub struct ProjectRun {
    /// Lint results, in the order of the requested files.
    pub results: Vec<FileResult>,
    /// The project's diagnostics as tsc would report them (parity keys).
    pub keys: BTreeMap<String, usize>,
    pub check_ms: f64,
    /// Program files whose check or lint failed (a checker bug); their results are empty
    /// and the project's keys are incomplete.
    pub failed: Vec<usize>,
}

/// Checks the whole project and lints `lint` with types on up to `threads` threads.
pub fn check_and_lint(project: &LoadedProject, lint: &[TypedFile], threads: usize) -> ProjectRun {
    let t0 = std::time::Instant::now();
    let program = &project.program;
    let checker = Checker::layered(program, project.parent.as_ref())
        .unwrap_or_else(|_| Checker::new(program));
    let mut lint_of: FxHashMap<usize, usize> = FxHashMap::default();
    for (i, t) in lint.iter().enumerate() {
        lint_of.insert(t.file, i);
    }
    let mut targets: Vec<usize> = (0..program.files.len())
        .filter(|&i| should_check(program, i) || lint_of.contains_key(&i))
        .collect();
    // Lint files first so the slowest part starts early.
    targets.sort_by_key(|i| !lint_of.contains_key(i));
    let next = AtomicUsize::new(0);
    let diags: Mutex<Vec<Diagnostic>> = Mutex::new(Vec::new());
    let results: Vec<Mutex<Option<FileResult>>> = lint.iter().map(|_| Mutex::new(None)).collect();
    let failed: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    let work = || {
        let mut ck: Option<Ck<'_, '_>> = None;
        let mut local = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(&fi) = targets.get(i) else { break };
            let file = FileId(fi as u32);
            // A checker bug must not take the run down: the file's worker is dropped, the
            // file is reported as failed and the caller lints it without types.
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let ck = ck.get_or_insert_with(|| Ck::new(&checker, file));
                let d = ck.check_source_file(file);
                let r = lint_of
                    .get(&fi)
                    .map(|&li| (li, lint_program_file(ck, file, &lint[li].cfg)));
                (d, r)
            }));
            match out {
                Ok((d, r)) => {
                    if should_check(program, fi) {
                        local.extend(d);
                    }
                    if let Some((li, r)) = r {
                        *results[li].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    }
                }
                Err(_) => {
                    ck = None;
                    failed.lock().unwrap_or_else(|e| e.into_inner()).push(fi);
                }
            }
        }
        diags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(local);
    };
    let threads = threads.clamp(1, 4).min(targets.len().max(1));
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for _ in 0..threads {
            let work = &work;
            match std::thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(s, work)
            {
                Ok(h) => handles.push(h),
                Err(_) => work(),
            }
        }
        for h in handles {
            let _ = h.join();
        }
    });
    let mut checked = diags.into_inner().unwrap_or_default();
    // As `check_program`: a checked file's, unless `@ts-ignore` or `@ts-expect-error`
    // governs its line.
    for d in &checker.link.diagnostics {
        if ts_r_checker::driver::link_diagnostic_reported(program, d) {
            checked.push(d.clone());
        }
    }
    let ours = tsc_diagnostics(program, checked);
    let keys = keys_of(project, &ours);
    let mut failed = failed.into_inner().unwrap_or_else(|e| e.into_inner());
    failed.sort_unstable();
    ProjectRun {
        failed,
        results: results
            .into_iter()
            .map(|r| {
                r.into_inner()
                    .unwrap_or_else(|e| e.into_inner())
                    .unwrap_or_default()
            })
            .collect(),
        keys,
        check_ms: t0.elapsed().as_secs_f64() * 1000.0,
    }
}

/// Lints one program file with types from `ck`, which has just checked it.
pub fn lint_program_file(ck: &mut Ck<'_, '_>, file: FileId, cfg: &FileConfig) -> FileResult {
    let c = ck.c;
    let sf = c.file(file);
    let types = CkTypes::new(ck, file);
    // A JavaScript file is linted as written, without the types its JSDoc gave the
    // checker's tree (node ids are unchanged).
    let stripped;
    let ast = if sf.ast.jsdoc_start == u32::MAX {
        &sf.ast
    } else {
        let mut a = sf.ast.clone();
        a.strip_jsdoc();
        stripped = a;
        &stripped
    };
    lint_parsed(
        &sf.path,
        &sf.text,
        cfg,
        &types,
        FileKey(file.0),
        ast,
        &ts_r_parser::without_grammar(&sf.syntax, &sf.grammar),
    )
}

/// The diagnostics tsc reports for the program given the checker's per-file diagnostics:
/// syntax errors stop semantic checking (as `ts_r_checker::check_program`).
fn tsc_diagnostics(program: &ProgramFiles, checked: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut syntax: Vec<Diagnostic> = Vec::new();
    for (i, f) in program.files.iter().enumerate() {
        if f.is_lib {
            continue;
        }
        syntax.extend(
            f.syntax
                .iter()
                .cloned()
                .map(|d| d.with_file(FileId(i as u32))),
        );
    }
    if syntax
        .iter()
        .any(|d| d.is_error() && !ts_r_checker::driver::CHECKER_GRAMMAR_CODES.contains(&d.code))
    {
        return ts_r_checker::sort_and_dedup(program, syntax);
    }
    let mut out = syntax;
    for name in &program.missing_types {
        let mut d = Diagnostic::new(
            ts_r_checker::diag::msg(
                ts_r_checker::diag::dg::CANNOT_FIND_TYPE_DEFINITION_FILE_FOR_0_2688,
            ),
            ts_r_span::Span::new(0, 0),
            &[name],
        );
        d.file = FileId(u32::MAX);
        out.push(d);
    }
    out.extend(checked);
    ts_r_checker::sort_and_dedup(program, out)
}

fn keys_of(project: &LoadedProject, diags: &[Diagnostic]) -> BTreeMap<String, usize> {
    struct Shown(FxHashMap<u32, SourceText>);
    impl Files for Shown {
        fn source(&self, file: FileId) -> Option<&SourceText> {
            self.0.get(&file.0)
        }
    }
    let mut shown = Shown(FxHashMap::default());
    for d in diags {
        if let Some(f) = project.program.files.get(d.file.0 as usize) {
            shown
                .0
                .entry(d.file.0)
                .or_insert_with(|| SourceText::new(f.path.clone(), f.text.clone()));
        }
    }
    let mut text = String::new();
    for l in &project.config_lines {
        text.push_str(l);
        text.push('\n');
    }
    for d in diags {
        text.push_str(&format_plain(d, &shown));
        text.push('\n');
    }
    let dir = std::path::Path::new(&project.config_path)
        .parent()
        .unwrap_or(std::path::Path::new("/"));
    parity::keys(&text, dir)
}

/// Lints `code` as `/p/file.ts` of a strict tsconfig project with only `rule` on, for rule
/// unit tests: the messages and the source after one pass of fixes. `None` when the
/// TypeScript lib files are missing (`cargo xtask upstream sync --minimal`).
#[cfg(test)]
pub(crate) fn lint_for_test(rule: &str, code: &str) -> Option<(Vec<String>, String)> {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    let libs = LibStore::locate()?;
    let mut fs = ts_r_config::MemoryFs::new();
    fs.add_file("/p/tsconfig.json", r#"{"compilerOptions":{"strict":true}}"#);
    fs.add_file("/p/file.ts", code);
    let project = load_project(&fs, "/p/tsconfig.json", &libs, 1);
    let config = serde_json::json!([{
        "files": ["**/*.ts"],
        "languageOptions": { "parser": "typescript-eslint/parser" },
        "rules": { rule: "error" },
    }]);
    let objs = objects_from_eslint_json(&config, crate::Registry::global()).expect("config");
    let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
    let mut opts = ResolveOptions::new();
    opts.types_available = true;
    let cfg = cfg.file_config("file.ts", &opts).expect("file config");
    let file = project.file_index("/p/file.ts").expect("file in project");
    let run = check_and_lint(&project, &[TypedFile { file, cfg }], 1);
    assert!(run.failed.is_empty(), "the check or lint panicked");
    let r = run.results.into_iter().next().expect("lint result");
    let fixes: Vec<&crate::Fix> = r
        .diagnostics
        .iter()
        .filter_map(|d| d.fix.as_ref())
        .collect();
    let (fixed, _) = crate::fix::apply_fixes(code, &fixes);
    Some((
        r.diagnostics.into_iter().map(|d| d.message).collect(),
        fixed,
    ))
}

/// The config of `/p/a.ts` with the ESLint `rules`, type-aware rules included (tests).
#[cfg(test)]
pub(crate) fn file_config_for_test(rules: &str) -> Option<Arc<FileConfig>> {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    let reg = crate::Registry::global();
    let json: serde_json::Value =
        serde_json::from_str(&format!(r#"[{{"files": ["**/*.ts"], "rules": {rules}}}]"#)).ok()?;
    let objs = objects_from_eslint_json(&json, reg).ok()?;
    let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
    let mut opts = ResolveOptions::new();
    opts.types_available = true;
    file.file_config("a.ts", &opts).ok()
}

/// Lints `/p/a.ts` (`code`, strict) with the ESLint `rules` through the checker, for rule
/// tests; `None` when the TypeScript lib files are missing.
#[cfg(test)]
pub(crate) fn lint_rules_for_test(code: &str, rules: &str) -> Option<FileResult> {
    static LIBS: std::sync::OnceLock<Option<LibStore>> = std::sync::OnceLock::new();
    let Some(libs) = LIBS.get_or_init(LibStore::locate).as_ref() else {
        eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
        return None;
    };
    let mut fs = ts_r_config::MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{"compilerOptions": {"strict": true, "target": "es2022", "noEmit": true}}"#,
    );
    fs.add_file("/p/a.ts", code);
    let project = load_project(&fs, "/p/tsconfig.json", libs, 1);
    let fc = file_config_for_test(rules)?;
    let run = check_and_lint(
        &project,
        &[TypedFile {
            file: project.file_index("/p/a.ts")?,
            cfg: fc,
        }],
        1,
    );
    run.results.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link diagnostic's half under `@ts-ignore` is not in the parity keys, while its lib
    /// file's half is, as with tsc 7.0.2 (@colors/colors: the ignored `bold` member merged
    /// into the global `String`).
    #[test]
    fn ts_ignore_hides_link_diagnostics() {
        let Some(libs) = LibStore::locate() else {
            eprintln!("skipped: lib files missing (cargo xtask upstream sync --minimal)");
            return;
        };
        let keys = |ignore: &str| {
            let mut fs = ts_r_config::MemoryFs::new();
            fs.set_cwd("/p");
            fs.add_file(
                "/p/tsconfig.json",
                r#"{"compilerOptions": {"strict": true, "noEmit": true, "types": []}, "files": ["a.ts"]}"#,
            );
            fs.add_file(
                "/p/a.ts",
                &format!(
                    "export {{}};\ndeclare global {{\n  interface String {{\n{ignore}    bold: string;\n  }}\n}}\nconst n: number = \"s\";\n"
                ),
            );
            let project = load_project(&fs, "/p/tsconfig.json", &libs, 1);
            check_and_lint(&project, &[], 1).keys
        };
        let ignored = keys("    // @ts-ignore\n");
        assert!(ignored.keys().any(|k| k.contains("TS2322")), "{ignored:?}");
        // tsc 7.0.2 drops the ignored half in a.ts but still reports the lib file's half.
        assert!(
            !ignored
                .keys()
                .any(|k| k.contains("a.ts") && k.contains("TS2300")),
            "{ignored:?}"
        );
        assert!(
            ignored
                .keys()
                .any(|k| k.contains("lib.es2015.core.d.ts") && k.contains("TS2300")),
            "{ignored:?}"
        );
        let reported = keys("");
        assert!(
            reported.keys().any(|k| k.contains("TS2300")),
            "{reported:?}"
        );
    }
}
