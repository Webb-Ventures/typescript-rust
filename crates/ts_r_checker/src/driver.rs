//! Checking a whole program: syntax diagnostics first (tsc reports semantic diagnostics
//! only when there are none), then per-file checking on a bounded number of threads, each
//! with its own [`Ck`] over the shared [`Checker`]. Output is sorted and deduplicated like
//! tsc's.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ts_r_diagnostics::{Diagnostic, FileId};

use crate::checker::{Checker, Ck};
use crate::diag::{dg, msg};
use crate::program::ProgramFiles;

/// A file of more than 1/(this × threads) of the program's text is checked before the others.
const BIG_FILE_SHARE: usize = 128;

/// Stack size of checking threads (deep expressions and flow graphs recurse).
const CHECK_STACK: usize = 256 << 20;

/// Options of a program check.
#[derive(Clone, Copy, Debug)]
pub struct CheckRun {
    /// Worker threads (clamped to 1..=4).
    pub threads: usize,
}

impl Default for CheckRun {
    fn default() -> Self {
        CheckRun { threads: 4 }
    }
}

/// The result of checking a program.
#[derive(Debug, Default)]
pub struct CheckResult {
    /// Sorted, deduplicated diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

impl CheckResult {
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }
}

/// Which files get semantic diagnostics: not lib files, not declaration files under
/// `skipLibCheck`, JavaScript files only under `checkJs` or `// @ts-check`, and no file
/// with `// @ts-nocheck`.
pub fn should_check(program: &ProgramFiles, i: usize) -> bool {
    let f = &program.files[i];
    // `noCheck`: no semantic diagnostics at all (syntax errors only).
    if f.is_lib || f.is_json() || program.options.no_check() == Some(true) {
        return false;
    }
    if f.is_declaration() && program.options.skip_lib_check() == Some(true) {
        return false;
    }
    if f.is_js() && program.options.check_js() != Some(true) && !f.has_ts_check() {
        return false;
    }
    !f.has_ts_nocheck()
}

/// Whether program-level (link) diagnostic `d` is reported: its file is checked, and no
/// `// @ts-ignore` or `// @ts-expect-error` directive governs its line (tsc applies the
/// directives to every semantic error of a file, merge errors included: @colors/colors
/// ignores its `bold` member of the global `String`), or it is in a lib file tsc checks
/// ([`lib_link_diagnostics_reported`]).
pub fn link_diagnostic_reported(program: &ProgramFiles, d: &Diagnostic) -> bool {
    let f = d.file.0 as usize;
    if f >= program.files.len() {
        return false;
    }
    if program.files[f].is_lib {
        return lib_link_diagnostics_reported(program, f);
    }
    should_check(program, f) && !crate::check::suppressed_by_directive(&program.files[f], d)
}

/// Whether the link (merge) diagnostics located in file `i` are reported because it is a
/// lib file tsc checks: tsc type-checks the default libraries unless `skipLibCheck` or
/// `skipDefaultLibCheck` is set, so a declaration that collides with a lib one (an
/// augmented `String` with a `bold` property, @colors/colors) is a TS2300 at both
/// declarations, the lib's printed by its path (observed in tsc 7.0.2; the
/// `@ts-ignore` above the package's own declaration does not reach the lib's). The lib
/// files themselves are not walked: only their merge diagnostics come out.
pub fn lib_link_diagnostics_reported(program: &ProgramFiles, i: usize) -> bool {
    let options = &program.options;
    program.files[i].is_lib
        && options.no_check() != Some(true)
        && options.skip_lib_check() != Some(true)
        && options.skip_default_lib_check() != Some(true)
}

/// A "plain" JavaScript file: JavaScript without `checkJs` set either way, `// @ts-check` or
/// `// @ts-nocheck`, outside `node_modules`. tsc 7 checks it but reports only
/// [`PLAIN_JS_CODES`] (observed).
pub fn is_plain_js(program: &ProgramFiles, i: usize) -> bool {
    let f = &program.files[i];
    f.is_js()
        && !f.is_json()
        && !f.is_lib
        && !f.is_external
        && !f.path.contains("/node_modules/")
        && program.options.check_js().is_none()
        && program.options.no_check() != Some(true)
        && !f.has_ts_check()
        && !f.has_ts_nocheck()
}

/// The checker and binder errors tsc 7 reports in a plain JavaScript file ([`is_plain_js`]):
/// grammar errors, redeclarations, strict-mode and private-name misuse, `await` outside
/// `async`, duplicate default exports and labels, and comparisons of object literals
/// (observed in tsc's baselines and probes; other errors, such as TS2300, TS2304 and
/// TS17004, are not reported).
pub const PLAIN_JS_CODES: &[u32] = &[
    1005, 1009, 1013, 1014, 1029, 1030, 1031, 1042, 1044, 1048, 1049, 1053, 1054, 1089, 1090, 1091,
    1097, 1100, 1101, 1102, 1104, 1105, 1106, 1107, 1111, 1113, 1114, 1115, 1116, 1123, 1155, 1156,
    1162, 1171, 1172, 1174, 1182, 1184, 1186, 1188, 1189, 1190, 1191, 1193, 1197, 1200, 1210, 1211,
    1213, 1214, 1215, 1248, 1255, 1258, 1262, 1308, 1312, 1325, 1344, 1358, 1359, 1451, 1473, 1474,
    2451, 2462, 2480, 2492, 2501, 2528, 2566, 2803, 2839, 5076, 17012, 18006, 18012, 18013, 18016,
    18030, 18038, 18041,
];

/// `d` as tsc 7 reports it in a plain JavaScript file, if it does: an unknown `#name`
/// property (TS2339) is TS1111 there (observed).
pub fn plain_js_diagnostic(d: Diagnostic) -> Option<Diagnostic> {
    if PLAIN_JS_CODES.contains(&d.code) {
        return Some(d);
    }
    if d.code == 2339
        && let Some(rest) = d.message.strip_prefix("Property '#")
        && let Some((name, _)) = rest.split_once('\'')
    {
        let name = format!("#{name}");
        let mut n = Diagnostic::new(
            msg(dg::PRIVATE_FIELD_0_MUST_BE_DECLARED_IN_AN_ENCLOSING_CLASS_1111),
            d.span,
            &[&name],
        );
        n.file = d.file;
        return Some(n);
    }
    None
}

/// Grammar errors our parser reports but tsc reports from the checker: they do not suppress
/// semantic diagnostics (observed: `a ?? b || c` gets TS5076 and the file's type errors;
/// TS1164 for `enum E { [e] }` is reported next to syntax errors).
/// The TS8xxx errors for TypeScript syntax in JavaScript files are among them (observed:
/// `import x = require()` in a `.js` file gets TS8002 and TS2307/TS2834). TS8006
/// (`namespace` and other declarations) still stops the check: CommonJS `require()` in
/// JavaScript namespaces is not modelled, so its semantic errors would be wrong.
pub const CHECKER_GRAMMAR_CODES: [u32; 17] = [
    5076, 1106, 1497, 1164, 8002, 8003, 8004, 8005, 8008, 8009, 8010, 8011, 8012, 8013, 8016, 8017,
    8037,
];

/// Checks every file of `program`.
pub fn check_program(program: &ProgramFiles, run: CheckRun) -> CheckResult {
    check_program_with(program, run, Checker::new)
}

/// TS2209/TS2210 for the `exports`/`imports` entries the resolver left unresolved for want
/// of a project root ([`ts_r_resolver::AmbiguousRoot`]): global diagnostics.
pub fn ambiguous_root_diagnostics(program: &ProgramFiles) -> Vec<Diagnostic> {
    program
        .ambiguous_roots
        .iter()
        .map(|a| {
            let info = if a.imports {
                dg::THE_PROJECT_ROOT_IS_AMBIGUOUS_BUT_IS_REQUIRED_TO_RESOLVE_IMPORT_MAP_ENTRY_0_IN_F_2210
            } else {
                dg::THE_PROJECT_ROOT_IS_AMBIGUOUS_BUT_IS_REQUIRED_TO_RESOLVE_EXPORT_MAP_ENTRY_0_IN_F_2209
            };
            let mut d = Diagnostic::new(
                msg(info),
                ts_r_ast::Span::new(0, 0),
                &[&a.entry, &a.package_json],
            );
            d.file = FileId(u32::MAX);
            d
        })
        .collect()
}

/// [`check_program`], but semantic diagnostics are reported even when there are syntax
/// errors, as tsc's test harness does (its baselines list both). `tsc` itself, and
/// [`check_program`], stop at syntax errors.
pub fn check_program_through_syntax_errors(program: &ProgramFiles, run: CheckRun) -> CheckResult {
    check_program_impl(program, run, Checker::new, true)
}

/// [`check_program`] with the checker made by `make` (for example a layered checker, see
/// [`Checker::layered`]); `make` is not called when syntax errors stop the check.
pub fn check_program_with<'a>(
    program: &'a ProgramFiles,
    run: CheckRun,
    make: impl FnOnce(&'a ProgramFiles) -> Checker<'a>,
) -> CheckResult {
    check_program_impl(program, run, make, false)
}

/// TS1xxx codes that tsc's binder reports whatever the parser found (`eval`/`arguments`,
/// `delete`, `with`, labels, `export as namespace` placement): they survive syntax errors
/// in the file. Reserved words as identifiers (TS1212-TS1214) wait for a clean parse.
const BINDER_CODES: &[u32] = &[1100, 1101, 1102, 1184, 1210, 1215, 1314, 1315, 1316, 1344];

fn check_program_impl<'a>(
    program: &'a ProgramFiles,
    run: CheckRun,
    make: impl FnOnce(&'a ProgramFiles) -> Checker<'a>,
    through_syntax_errors: bool,
) -> CheckResult {
    // The parser's checker-grammar errors (`SourceFile::grammar`) are checker diagnostics:
    // reported only for checked files, and they do not stop semantic checking.
    let mut syntax: Vec<Diagnostic> = Vec::new();
    // Files with syntax errors.
    let mut broken = vec![false; program.files.len()];
    for (i, f) in program.files.iter().enumerate() {
        if f.is_lib {
            continue;
        }
        for d in ts_r_parser::tsc_syntax_errors(&f.syntax, &f.grammar, &f.lenient) {
            if d.is_error() && !CHECKER_GRAMMAR_CODES.contains(&d.code) {
                broken[i] = true;
            }
            syntax.push(d.with_file(FileId(i as u32)));
        }
        // A malformed `/// <reference />` is a parse error in tsc (TS1084).
        let invalid = crate::references::invalid_reference_diagnostics(f);
        if !invalid.is_empty() {
            broken[i] = true;
        }
        syntax.extend(invalid.into_iter().map(|d| d.with_file(FileId(i as u32))));
    }
    // Blocking syntax errors: only they are reported, not the program diagnostics below
    // either (tsc 7.0.2: a missing `/// <reference path|types>` target elsewhere is not
    // reported next to a syntax error).
    if !through_syntax_errors && broken.iter().any(|&b| b) {
        return CheckResult {
            diagnostics: sort_and_dedup(program, syntax),
        };
    }
    // Program diagnostics (`/// <reference path>` targets) come with the syntax ones.
    let paths: rustc_hash::FxHashSet<&str> =
        program.files.iter().map(|f| f.path.as_str()).collect();
    for (i, f) in program.files.iter().enumerate() {
        syntax.extend(
            crate::reference_diagnostics(f, &program.options, &paths)
                .into_iter()
                .map(|d| d.with_file(FileId(i as u32))),
        );
        let missing: Vec<&str> = program
            .missing_type_references
            .iter()
            .filter(|(p, _)| *p == f.path)
            .map(|(_, n)| n.as_str())
            .collect();
        syntax.extend(
            crate::references::type_reference_diagnostics(f, &missing)
                .into_iter()
                .map(|d| d.with_file(FileId(i as u32))),
        );
    }
    // Files with syntax errors get no checker grammar errors (TS1xxx) from the checker
    // either: tsc's checker reports those only in a file without parse errors.
    let n_syntax = syntax.len();
    let mut out = syntax;
    // Global diagnostics.
    for name in &program.missing_types {
        let mut d = Diagnostic::new(
            msg(dg::CANNOT_FIND_TYPE_DEFINITION_FILE_FOR_0_2688),
            ts_r_ast::Span::new(0, 0),
            &[name],
        );
        // tsc says why the missing library is in the program.
        let mut chain = crate::diag::Chain::new();
        chain.push_back(dg::CANNOT_FIND_TYPE_DEFINITION_FILE_FOR_0_2688, &[name]);
        chain.push_back(dg::THE_FILE_IS_IN_THE_PROGRAM_BECAUSE_1430, &[]);
        chain.push_back(
            dg::ENTRY_POINT_OF_TYPE_LIBRARY_0_SPECIFIED_IN_COMPILEROPTIONS_1417,
            &[name],
        );
        d.message = chain.text();
        d.file = FileId(u32::MAX);
        out.push(d);
    }
    out.extend(ambiguous_root_diagnostics(program));
    let mut checker = make(program);
    // tsc's binder reports its strict-mode and `export as namespace` errors whatever the
    // parser found; only the checker's grammar errors wait for a clean parse.
    let bind_kept: rustc_hash::FxHashSet<(u32, u32, u32)> = if through_syntax_errors {
        (0..program.files.len())
            .filter(|&i| broken[i])
            .flat_map(|i| {
                checker
                    .bound(FileId(i as u32))
                    .diagnostics
                    .iter()
                    .filter(|d| BINDER_CODES.contains(&d.code))
                    .map(move |d| (i as u32, d.span.start, d.code))
            })
            .collect()
    } else {
        Default::default()
    };
    out.extend(missing_global_type_diagnostics(&checker));
    for d in &checker.link.diagnostics {
        if link_diagnostic_reported(program, d) {
            out.push(d.clone());
        }
    }
    let targets: Vec<usize> = (0..program.files.len())
        .filter(|&i| should_check(program, i))
        .collect();
    for r in check_files(&checker, &targets, run) {
        out.extend(r);
    }
    let plain: Vec<usize> = (0..program.files.len())
        .filter(|&i| is_plain_js(program, i))
        .collect();
    for (&i, r) in plain.iter().zip(check_files(&checker, &plain, run)) {
        // JSDoc syntax errors are for checked files only.
        let jsdoc = &program.files[i].jsdoc;
        out.extend(
            r.into_iter()
                .filter(|d| !jsdoc.iter().any(|j| j.span == d.span && j.code == d.code))
                .filter_map(plain_js_diagnostic),
        );
    }
    release_in_background(&mut checker);
    if through_syntax_errors {
        let mut i = 0;
        out.retain(|d| {
            i += 1;
            i <= n_syntax
                // (TS1294, `erasableSyntaxOnly`, TS1141 for `import(x)` types, TS1539 for
                // bigint property names, TS1225/TS1228/TS1229/TS1230 for type predicates,
                // TS1156 for a `let`/`const` statement as the body of
                // `if`/`while`/... (the parser keeps the `type`/`interface` one back), and the
                // import checks TS1192, TS1205, TS1340, TS1361, TS1362, TS1484 and TS1485 are
                // semantic errors to tsc 7.)
                || !((1000..2000).contains(&d.code)
                    && !matches!(
                        d.code,
                        1141
                            | 1156
                            | 1192
                            | 1205
                            | 1225
                            | 1228
                            | 1229
                            | 1230
                            | 1294
                            | 1340
                            | 1361
                            | 1362
                            | 1484
                            | 1485
                            | 1539
                    )
                    || matches!(d.code, 8020 | 17019 | 17020))
                || !broken.get(d.file.0 as usize).copied().unwrap_or(false)
                || bind_kept.contains(&(d.file.0, d.span.start, d.code))
                // JSDoc syntax errors stay (tsc 7 parses JSDoc apart from the file).
                || program.files.get(d.file.0 as usize).is_some_and(|f| {
                    f.jsdoc
                        .iter()
                        .any(|j| j.span == d.span && j.code == d.code)
                })
        });
    }
    CheckResult {
        diagnostics: sort_and_dedup(program, out),
    }
}

/// Types at which freeing a checker's store is worth a thread of its own.
const BACKGROUND_FREE_TYPES: usize = 100_000;

/// Hands the type store and symbol levels of a big program's `checker` to a thread of their
/// own to free: that takes tens of milliseconds nothing waits for (a one-shot CLI exits
/// before it is done). The checker must not be used afterwards.
fn release_in_background(checker: &mut Checker<'_>) {
    if checker.store.len() < BACKGROUND_FREE_TYPES {
        return;
    }
    static EMPTY: std::sync::OnceLock<std::sync::Arc<ts_r_types::TypeStore>> =
        std::sync::OnceLock::new();
    let empty = EMPTY.get_or_init(|| std::sync::Arc::new(ts_r_types::TypeStore::new()));
    let store = std::mem::replace(&mut checker.store, empty.clone());
    let layers = std::mem::take(&mut checker.layers);
    // When no thread can be started the closure, and with it the store, is dropped here.
    let _ = std::thread::Builder::new()
        .name("ts-r-free".into())
        .spawn(move || drop((layers, store)));
}

/// Global types every program needs, including programs using a partial library set.
pub const REQUIRED_GLOBAL_TYPES: [&str; 10] = [
    "Array",
    "Boolean",
    "CallableFunction",
    "Function",
    "IArguments",
    "NewableFunction",
    "Number",
    "Object",
    "RegExp",
    "String",
];

/// TS2318 for each global type every program needs that no file declares (observed with
/// `noLib`). Global diagnostics: reported with option errors too. `CallableFunction` and
/// `NewableFunction` are only looked up under `strictBindCallApply` (without it `Function`
/// stands in; `tsc/internal/checker/checker.go`, the checker's global type setup).
pub fn missing_global_type_diagnostics(checker: &Checker<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for name in REQUIRED_GLOBAL_TYPES {
        if !checker.opts.strict_bind_call_apply
            && matches!(name, "CallableFunction" | "NewableFunction")
        {
            continue;
        }
        if checker
            .global_symbol(name, ts_r_binder::meaning::TYPE)
            .is_none()
        {
            let mut d = Diagnostic::new(
                msg(dg::CANNOT_FIND_GLOBAL_TYPE_0_2318),
                ts_r_ast::Span::new(0, 0),
                &[name],
            );
            d.file = FileId(u32::MAX);
            out.push(d);
        }
    }
    out
}

/// The checker's diagnostics of each of `targets` (file indices), in order, checked on up to
/// `run.threads` threads. Link and global diagnostics are not included.
pub fn check_files(
    checker: &Checker<'_>,
    targets: &[usize],
    run: CheckRun,
) -> Vec<Vec<Diagnostic>> {
    let threads = run.threads.clamp(1, 4).min(targets.len().max(1));
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Vec<Diagnostic>>> =
        targets.iter().map(|_| Mutex::new(Vec::new())).collect();
    // Files big enough to keep a thread busy after the others are done go first (biggest
    // first); the rest follow in program order, which finds more types already made. The
    // results don't depend on the order (shared caches hold order-independent answers).
    let mut order: Vec<usize> = (0..targets.len()).collect();
    if threads > 1 {
        let size = |i: usize| checker.files.get(targets[i]).map_or(0, |f| f.text.len());
        let total: usize = order.iter().map(|&i| size(i)).sum();
        let big = total / (BIG_FILE_SHARE * threads);
        order.sort_by_key(|&i| std::cmp::Reverse(size(i).max(big)));
    }
    let work = || {
        let mut ck: Option<Ck<'_, '_>> = None;
        loop {
            let Some(&i) = order.get(next.fetch_add(1, Ordering::Relaxed)) else {
                break;
            };
            let fi = targets[i];
            let file = FileId(fi as u32);
            let ck = ck.get_or_insert_with(|| Ck::new(checker, file));
            // (JSDoc syntax errors of JavaScript files come with the checker errors.)
            let diags = ck.check_source_file(file);
            *results[i].lock().unwrap_or_else(|e| e.into_inner()) = diags;
        }
    };
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for _ in 0..threads {
            let work = &work;
            let h = std::thread::Builder::new()
                .stack_size(CHECK_STACK)
                .spawn_scoped(s, work);
            match h {
                Ok(h) => handles.push(h),
                Err(_) => work(),
            }
        }
        for h in handles {
            let _ = h.join();
        }
    });
    results
        .into_iter()
        .map(|r| r.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect()
}

/// Sorts by file path, position, length, code and message; removes duplicates.
pub fn sort_and_dedup(program: &ProgramFiles, mut diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let path_of = |d: &Diagnostic| -> &str {
        program
            .files
            .get(d.file.0 as usize)
            .map_or("", |f| f.path.as_str())
    };
    diags.sort_by(|a, b| {
        path_of(a)
            .cmp(path_of(b))
            .then(a.span.start.cmp(&b.span.start))
            .then(a.span.len().cmp(&b.span.len()))
            .then(a.code.cmp(&b.code))
            .then(a.message.cmp(&b.message))
    });
    diags.dedup_by(|a, b| {
        a.file == b.file && a.span == b.span && a.code == b.code && a.message == b.message
    });
    diags
}
