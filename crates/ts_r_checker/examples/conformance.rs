//! Compares checker diagnostics with TypeScript 7's conformance baselines.
//!
//! ```text
//! cargo xtask upstream sync --minimal   # once
//! cargo build -j 4 --release -p ts_r_checker --example conformance
//! scripts/limit.sh --mem-mb 4096 --time-s 1800 -- \
//!   target/release/examples/conformance [--verbose] [--threads N] [--filter-file F] [filter...]
//! ```
//!
//! Every test under `tests/cases/{compiler,conformance}` (one run per baseline variant) is
//! split into its files, loaded as a program over an in-memory file system with the test's
//! options, and checked. The reported diagnostics are compared with the `.errors.txt`
//! baseline (no `.errors.txt` but other baselines: no errors expected). A test matches when
//! every diagnostic's file, position, code and full message (chain included) agree. Option
//! errors (`verify_options`) count as global diagnostics, as the directives are no config
//! file, and so are TS5055 for emitted JavaScript inputs and TS6504 for JavaScript roots
//! without `allowJs` (left out of the program; not for a test that is itself a `.js` file).
//! Under `@noImplicitReferences` only
//! the last file is a root. A test with its own `tsconfig.json` uses its options (the
//! directives on top, through `extends`) and its `files`/`include` as the roots; its own
//! diagnostics, and option errors about what it sets, are reported at their place in it.
//! Results per
//! area go to
//! stdout; mismatches to `target/checker-conformance.txt` (or `$TS_R_CONFORMANCE_OUT`).
//! `TS_R_CONFORMANCE_ROOT` replaces `target/upstream/typescript` (ad-hoc tests: a
//! `tests/cases/compiler/x.ts` with a `tsc/testdata/baselines/reference/submodule/compiler/x.*`).
//!
//! Skipped (counted, not compared): tests tsc 7 has no baseline of any kind for (mostly
//! `@module: amd`/`system`, which tsc 7 no longer runs), and `@pretty: true` tests, whose
//! baselines are in the pretty format.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ts_r_checker::{CheckRun, LibStore, check_program_through_syntax_errors, load_program};
use ts_r_config::options::{OptionType, find_option_ignore_case};
use ts_r_config::{MemoryFs, load_tsconfig};

ts_r_testing::cap_memory!();

/// `(file, line, col, code, message)`; message includes chain lines joined with `\n`.
type Diag = (String, u32, u32, u32, String);

struct Test {
    name: String,
    area: String,
    options: Vec<(String, String)>,
    files: Vec<(String, String)>,
    expected: BTreeSet<Diag>,
}

fn upstream() -> PathBuf {
    // `TS_R_CONFORMANCE_ROOT`: another tree of the same layout (ad-hoc tests and baselines).
    if let Some(r) = std::env::var_os("TS_R_CONFORMANCE_ROOT") {
        return PathBuf::from(r);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/upstream/typescript")
}

/// Where the mismatches go: `TS_R_CONFORMANCE_OUT`, else `target/checker-conformance.txt`.
fn mismatch_path() -> PathBuf {
    match std::env::var_os("TS_R_CONFORMANCE_OUT") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/checker-conformance.txt"),
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str())
            && matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs"
            )
        {
            out.push(p);
        }
    }
}

fn parse_directive(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("//")?.trim_start().strip_prefix('@')?;
    let (k, v) = rest.split_once(':')?;
    let k = k.trim_end();
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
}

type Pairs = Vec<(String, String)>;

/// Tests skipped because tsc 7 has no baseline for them.
static SKIPPED_NO_BASELINE: AtomicUsize = AtomicUsize::new(0);
/// Tests skipped because their baselines use the `--pretty` format.
static SKIPPED_PRETTY: AtomicUsize = AtomicUsize::new(0);

fn split_test(text: &str, default_name: &str) -> (Pairs, Pairs) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut files: Vec<(String, String)> = Vec::new();
    let mut options = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if let Some((k, v)) = parse_directive(line) {
            if k == "filename" {
                if let Some(n) = cur_name.take() {
                    files.push((n, std::mem::take(&mut cur)));
                } else {
                    cur.clear();
                }
                cur_name = Some(v);
            } else if k == "symlink"
                && let Some(n) = &cur_name
            {
                // `// @symlink: a,b` under `@filename: f` makes `a` and `b` symlinks to `f`.
                for link in v.split(',') {
                    options.push(("link".to_owned(), format!("{n} -> {}", link.trim())));
                }
            } else {
                options.push((k, v));
            }
            continue;
        }
        // tsc's harness drops blank lines at the start of each unit (after the directives
        // or an `@filename` line), so baseline line numbers count from the first non-blank
        // line.
        if cur.is_empty() && line.trim().is_empty() {
            continue;
        }
        cur.push_str(line);
    }
    match cur_name {
        Some(n) => files.push((n, cur)),
        None => files.push((default_name.to_owned(), cur)),
    }
    (files, options)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The diagnostics of a baseline's header (before the first `====`).
fn parse_baseline(text: &str) -> BTreeSet<Diag> {
    let mut out = BTreeSet::new();
    let text = if text.contains('\u{1b}') {
        strip_ansi(text)
    } else {
        text.to_owned()
    };
    let mut cur: Option<Diag> = None;
    for line in text.lines() {
        if line.starts_with("====") {
            break;
        }
        if line.starts_with("  ") && !line.trim().is_empty() {
            if let Some(d) = cur.as_mut() {
                d.4.push('\n');
                d.4.push_str(line);
            }
            continue;
        }
        if let Some(d) = cur.take() {
            out.insert(d);
        }
        if let Some(d) = parse_line(line) {
            cur = Some(d);
        }
    }
    if let Some(d) = cur.take() {
        out.insert(d);
    }
    out
}

fn parse_line(line: &str) -> Option<Diag> {
    // file(line,col): error TS1234: message   |   error TS1234: message
    if let Some(rest) = line.strip_prefix("error TS") {
        let (code, msg) = rest.split_once(": ")?;
        return Some((String::new(), 0, 0, code.parse().ok()?, msg.to_owned()));
    }
    let (loc, rest) = line.split_once("): ")?;
    let (file, lc) = loc.rsplit_once('(')?;
    let (l, c) = lc.split_once(',')?;
    let (l, c) = (l.parse::<u32>().ok()?, c.parse::<u32>().ok()?);
    let (code_part, msg) = rest.split_once(": ")?;
    let code = code_part.rsplit("TS").next()?.parse::<u32>().ok()?;
    if !code_part.starts_with("error") {
        return None;
    }
    Some((
        file.trim_start_matches("./").to_owned(),
        l,
        c,
        code,
        msg.to_owned(),
    ))
}

/// The tsconfig `compilerOptions` members for the test's directives.
fn options_json(opts: &[(String, String)]) -> String {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (k, v) in opts.iter().rev() {
        let Some(decl) = find_option_ignore_case(k) else {
            continue;
        };
        if !seen.insert(decl.name) {
            continue;
        }
        let first = v.split(',').next().unwrap_or("").trim();
        let val = match decl.ty {
            OptionType::Bool => match first.to_ascii_lowercase().as_str() {
                "true" => "true".to_owned(),
                "false" => "false".to_owned(),
                _ => continue,
            },
            OptionType::Number => match first.parse::<f64>() {
                Ok(_) => first.to_owned(),
                Err(_) => continue,
            },
            OptionType::StringList | OptionType::PathList | OptionType::Lib => {
                let items: Vec<String> = v
                    .split(',')
                    .map(|s| format!("{:?}", s.trim()))
                    .filter(|s| s != "\"\"")
                    .collect();
                format!("[{}]", items.join(","))
            }
            OptionType::PathMap | OptionType::Plugins => v.clone(),
            _ => format!("{first:?}"),
        };
        out.push(format!("{:?}: {val}", decl.name));
    }
    // tsc's test harness prints types untruncated (a type met again inside itself is `any`,
    // not `...`).
    if !seen.contains("noErrorTruncation") {
        out.push("\"noErrorTruncation\": true".to_owned());
    }
    out.join(", ")
}

/// One test per baseline variant (`stem(opt=v,...).errors.txt`) or the plain configuration.
fn load(path: &Path, root: &Path, baselines: &Path, names: &BTreeSet<String>) -> Vec<Test> {
    let Ok(raw) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = ts_r_config::fs::decode_text(raw);
    let (Some(stem), Some(file_name)) = (
        path.file_stem().and_then(|s| s.to_str()),
        path.file_name().and_then(|s| s.to_str()),
    ) else {
        return Vec::new();
    };
    let rel = path
        .strip_prefix(root.join("tests/cases"))
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let comps: Vec<String> = rel
        .iter()
        .filter_map(|c| c.to_str().map(str::to_owned))
        .collect();
    let area = match comps.first().map(String::as_str) {
        Some("conformance") if comps.len() > 3 => format!("{}/{}", comps[1], comps[2]),
        Some("conformance") if comps.len() > 2 => comps[1].clone(),
        Some(s) => s.to_owned(),
        None => String::new(),
    };
    let (files, options) = split_test(&text, file_name);
    let mut variants: BTreeSet<Option<String>> = BTreeSet::new();
    for n in names.range(stem.to_owned()..) {
        if !n.starts_with(stem) {
            break;
        }
        let rest = &n[stem.len()..];
        if let Some(ext) = rest.strip_prefix('.') {
            // `x.errors.txt`, `x.js`, ...; not another test's `x.es6.js` (`x.es6.ts`).
            let first = ext.split('.').next().unwrap_or("");
            if matches!(
                first,
                "errors" | "js" | "types" | "symbols" | "trace" | "sourcemap"
            ) {
                variants.insert(None);
            }
        } else if let Some(v) = rest.strip_prefix('(')
            && let Some((v, _)) = v.split_once(')')
        {
            variants.insert(Some(v.to_owned()));
        }
    }
    if variants.is_empty() {
        SKIPPED_NO_BASELINE.fetch_add(1, Ordering::Relaxed);
        return Vec::new();
    }
    if options
        .iter()
        .any(|(k, v)| k == "pretty" && v.eq_ignore_ascii_case("true"))
    {
        SKIPPED_PRETTY.fetch_add(1, Ordering::Relaxed);
        return Vec::new();
    }
    let mut out = Vec::new();
    for v in variants {
        let mut options = options.clone();
        let base = match &v {
            Some(v) => {
                for kv in v.split(',') {
                    if let Some((k, val)) = kv.split_once('=') {
                        options.push((k.trim().to_ascii_lowercase(), val.trim().to_owned()));
                    }
                }
                format!("{stem}({v}).errors.txt")
            }
            None => format!("{stem}.errors.txt"),
        };
        let expected = if names.contains(&base) {
            match std::fs::read_to_string(baselines.join(&base)) {
                Ok(t) => parse_baseline(&t),
                Err(_) => continue,
            }
        } else {
            BTreeSet::new()
        };
        let name = match &v {
            Some(v) => format!("{}({v})", rel.display()),
            None => rel.display().to_string(),
        };
        out.push(Test {
            name,
            area: area.clone(),
            options,
            files: files.clone(),
            expected,
        });
    }
    out
}

/// The harness's shared test libraries (`tests/lib`), visible to tests as `/.lib/...`.
fn test_libs() -> &'static [(String, String)] {
    static LIBS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    LIBS.get_or_init(|| {
        let dir = upstream().join("tests/lib");
        let mut paths = Vec::new();
        collect(&dir, &mut paths);
        paths
            .iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(&dir).ok()?.to_str()?.replace('\\', "/");
                Some((format!("/.lib/{rel}"), std::fs::read_to_string(p).ok()?))
            })
            .collect()
    })
}

fn run(test: &Test, libs: &LibStore) -> BTreeSet<Diag> {
    let abs = |n: &str| {
        let n = n.trim_start_matches("./");
        if ts_r_config::path::is_rooted(n) {
            n.to_owned()
        } else {
            format!("/.src/{n}")
        }
    };
    let shown = |p: &str| p.strip_prefix("/.src/").unwrap_or(p).to_owned();
    // The harness's file system is case-sensitive unless `@useCaseSensitiveFileNames: false`
    // (`tsc: tsc/internal/testutil/harnessutil/harnessutil.go`).
    let insensitive = test.options.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("usecasesensitivefilenames") && v.eq_ignore_ascii_case("false")
    });
    let mut fs = if insensitive {
        MemoryFs::case_insensitive()
    } else {
        MemoryFs::new()
    };
    fs.set_cwd("/.src");
    let mut roots = Vec::new();
    // tsc's harness compiles only the last unit (the others are just on disk) under
    // `@noImplicitReferences`, or when the last unit has a `require(` or a
    // `reference path` directive. (A test with a `tsconfig.json` unit takes its roots from
    // that config there; not modelled, so every unit stays a root.)
    let has_config = test
        .files
        .iter()
        .any(|(n, _)| n.to_ascii_lowercase().ends_with("tsconfig.json"));
    let no_implicit = test
        .options
        .iter()
        .any(|(k, v)| k == "noimplicitreferences" && v.eq_ignore_ascii_case("true"));
    let last_refers = test.files.last().is_some_and(|(_, src)| {
        src.contains("require(")
            || src
                .split("reference")
                .skip(1)
                .any(|r| r.starts_with(char::is_whitespace) && r.trim_start().starts_with("path"))
    });
    let only_last = !has_config && (no_implicit || last_refers);
    let last = test.files.len().saturating_sub(1);
    for (i, (name, src)) in test.files.iter().enumerate() {
        let a = abs(name);
        fs.add_file(&a, src);
        let lower = a.to_ascii_lowercase();
        if lower.ends_with(".json") || (only_last && i != last) {
            continue;
        }
        roots.push(a);
    }
    // `@noImplicitReferences`: only the last file is a root; the others load when referenced
    // (the TypeScript test harness). Tests with their own `tsconfig.json` take their roots
    // from it (below).
    if test
        .options
        .iter()
        .any(|(k, v)| k == "noimplicitreferences" && v.eq_ignore_ascii_case("true"))
        && !test
            .files
            .iter()
            .any(|(n, _)| n == "tsconfig.json" || n.ends_with("/tsconfig.json"))
        && let Some((last, _)) = test.files.last()
    {
        let last = abs(last);
        roots.retain(|r| *r == last);
    }
    if test.files.iter().any(|(_, src)| src.contains("/.lib/")) {
        for (path, src) in test_libs() {
            fs.add_file(path, src);
        }
    }
    // `// @link: target -> link` makes `link` a symlink to `target`.
    for (k, v) in &test.options {
        if k == "link"
            && let Some((target, link)) = v.split_once("->")
        {
            fs.add_symlink(&abs(link.trim()), &abs(target.trim()));
        }
    }
    let co = options_json(&test.options);
    // A test's own `tsconfig.json` supplies options (the directives on top) and, through its
    // `files`/`include`, the root files.
    let own_config = test
        .files
        .iter()
        .map(|(n, _)| abs(n))
        .find(|n| n == "/.src/tsconfig.json" || n.ends_with("/tsconfig.json"));
    // The wrapper sits next to the test's own config, so paths that default from the config's
    // directory (type roots: every `node_modules/@types` above it) match tsc's.
    let wrapper = match &own_config {
        Some(own) => {
            let dir = own.rsplit_once('/').map_or("/.src", |(d, _)| d);
            let wrapper = format!("{dir}/__tsconfig.json");
            fs.add_file(
                &wrapper,
                &format!("{{\"extends\": {own:?}, \"compilerOptions\": {{{co}}}}}"),
            );
            wrapper
        }
        None => {
            fs.add_file(
                "/.src/__tsconfig.json",
                &format!("{{\"compilerOptions\": {{{co}}}, \"files\": []}}"),
            );
            "/.src/__tsconfig.json".to_owned()
        }
    };
    let mut cfg = load_tsconfig(&fs, &wrapper);
    // Errors in the test's own `tsconfig.json` (TS5024 and the like) are reported at their
    // place in it, as tsc reports a config file's diagnostics.
    let mut own_diagnostics = Vec::new();
    if let Some(own) = &own_config {
        let parsed = load_tsconfig(&fs, own);
        roots = parsed.file_names;
        own_diagnostics = parsed.diagnostics;
    } else {
        // tsc's harness passes the directives as options, not as a config file: there is no
        // tsconfig directory to stand in for `rootDir` (TS2209/TS2210).
        cfg.options.config_file_path = None;
    }
    // TS6504: without `allowJs`, a JavaScript root is not compiled (the directives are no
    // config file, so tsc names the root and why it is in the program).
    let mut js_roots = Vec::new();
    if own_config.is_none()
        && !cfg.options.effective().allow_js()
        && !test
            .name
            .split('(')
            .next()
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(".js"))
    {
        roots.retain(|r| {
            let lower = r.to_ascii_lowercase();
            let js = [".js", ".jsx", ".mjs", ".cjs"]
                .iter()
                .any(|e| lower.ends_with(e));
            if js {
                js_roots.push(r.clone());
            }
            !js
        });
    }
    // tsc's root files (`tsc/internal/compiler/fileloader.go`): a root with an extension that
    // is not a supported one (JSON only with `resolveJsonModule`) is left out, TS6054; one
    // without an extension is the first file of it plus `.ts`, `.tsx`, `.d.ts` (and `.js`,
    // `.jsx` with `allowJs`) that exists.
    let mut unsupported_roots = Vec::new();
    if own_config.is_none() {
        let e = cfg.options.effective();
        let mut groups: Vec<&[&str]> = if e.allow_js() {
            vec![
                &[".ts", ".tsx", ".d.ts", ".js", ".jsx"],
                &[".cts", ".d.cts", ".cjs"],
                &[".mts", ".d.mts", ".mjs"],
            ]
        } else {
            vec![
                &[".ts", ".tsx", ".d.ts"],
                &[".cts", ".d.cts"],
                &[".mts", ".d.mts"],
            ]
        };
        let listed = groups
            .iter()
            .flat_map(|g| g.iter())
            .map(|x| format!("'{x}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let first = groups[0];
        if e.resolve_json_module() {
            groups.push(&[".json"]);
        }
        let mut kept = Vec::with_capacity(roots.len());
        for r in std::mem::take(&mut roots) {
            let base = r.rsplit('/').next().unwrap_or(&r);
            let lower = base.to_ascii_lowercase();
            if !base.contains('.') {
                let found = first
                    .iter()
                    .map(|x| format!("{r}{x}"))
                    .find(|c| ts_r_config::FileSystem::file_exists(&fs, c));
                kept.push(found.unwrap_or(r));
            } else if groups
                .iter()
                .flat_map(|g| g.iter())
                .any(|x| lower.ends_with(x))
                || [".js", ".jsx", ".mjs", ".cjs"]
                    .iter()
                    .any(|x| lower.ends_with(x))
            {
                kept.push(r);
            } else {
                unsupported_roots.push(format!(
                    "File '{}' has an unsupported extension. The only supported extensions are {listed}.\n  The file is in the program because:\n    Root file specified for compilation",
                    shown(&r)
                ));
            }
        }
        roots = kept;
    }
    let program = load_program(&fs, &roots, &cfg.options, Some(libs), 1);
    let result = check_program_through_syntax_errors(&program, CheckRun { threads: 1 });
    let mut out = BTreeSet::new();
    // Option checks: the directives are not a config file, so tsc reports them without a
    // location.
    // Options from the test's own config are reported where it sets them.
    let located = |d: &ts_r_config::ConfigDiagnostic| -> Option<(String, u32, u32)> {
        let (file, span) = (d.file.as_deref()?, d.span?);
        if file == wrapper || own_config.is_none() {
            return None;
        }
        let text = ts_r_config::FileSystem::read_file(&fs, file)?;
        let lc = ts_r_span::LineIndex::new(&text).line_col_utf16(&text, span.0);
        Some((shown(file), lc.line + 1, lc.col + 1))
    };
    let own_errors = own_diagnostics
        .iter()
        .filter(|d| d.file.as_deref().is_some_and(|f| f != wrapper));
    // The test's own config is the leaf for the option checks: what it sets is located
    // there, the directives (the wrapper's options) stay global.
    let verified = match &own_config {
        Some(own) => {
            let mut o = cfg.options.clone();
            o.config_file_path = Some(own.clone());
            ts_r_config::verify::verify_options(&o, None)
        }
        None => ts_r_config::verify::verify_options(&cfg.options, None),
    };
    for d in verified.iter().chain(own_errors) {
        if d.severity == ts_r_config::Severity::Error {
            let mut message = d.message.clone();
            for r in &d.related {
                message.push_str("\n  ");
                message.push_str(r);
            }
            let (file, line, col) = located(d).unwrap_or_default();
            out.insert((file, line, col, d.code, message));
        }
    }
    for m in unsupported_roots {
        out.insert((String::new(), 0, 0, 6054, m));
    }
    for r in &js_roots {
        out.insert((
            String::new(),
            0,
            0,
            6504,
            format!(
                "File '{}' is a JavaScript file. Did you mean to enable the 'allowJs' option?\n  The file is in the program because:\n    Root file specified for compilation",
                shown(r)
            ),
        ));
    }
    // TS5055/TS5056: tsc's output-path check (a program diagnostic): an output that is a
    // program file or that an earlier source writes too. Without a config file TS5055 has
    // the tsconfig hint. Not with the harness's `@suppressOutputPathCheck`.
    let opt_true = |name: &str| {
        test.options
            .iter()
            .any(|(k, v)| k == name && v.eq_ignore_ascii_case("true"))
    };
    let emit_cfg = ts_r_emit::EmitConfig {
        options: &cfg.options,
        config: own_config.as_ref().map(|_| &cfg),
        threads: 1,
        case_sensitive: ts_r_config::FileSystem::case_sensitive(&fs),
        specifiers: None,
    };
    if !opt_true("suppressoutputpathcheck") {
        for (code, path) in ts_r_emit::output_path_conflicts(&program, &emit_cfg) {
            let message = if code == 5055 {
                let hint = if own_config.is_none() {
                    "\n  Adding a tsconfig.json file will help organize projects that contain both TypeScript and JavaScript files. Learn more at https://aka.ms/tsconfig."
                } else {
                    ""
                };
                format!(
                    "Cannot write file '{}' because it would overwrite input file.{hint}",
                    shown(&path)
                )
            } else {
                format!(
                    "Cannot write file '{}' because it would be overwritten by multiple input files.",
                    shown(&path)
                )
            };
            out.insert((String::new(), 0, 0, code, message));
        }
    }
    // The program's output-layout checks (TS5009, TS5011, TS6059), as option errors.
    // They name the test's own config, not the wrapper that extends it.
    let named = own_config.as_ref().map(|own| {
        let mut c = cfg.clone();
        c.config_path = own.clone();
        c
    });
    let layout_cfg = ts_r_emit::EmitConfig {
        config: named.as_ref(),
        ..emit_cfg
    };
    let (_, layout) = ts_r_emit::root_dir_diagnostics(&program, &layout_cfg);
    for d in &layout {
        let mut message = d.message.clone();
        for r in &d.related {
            message.push_str("\n  ");
            message.push_str(r);
        }
        let (file, line, col) = located(d).unwrap_or_default();
        out.insert((file, line, col, d.code, message));
    }
    let declaration_diagnostics = declaration_diagnostics(&fs, &program, &cfg.options);
    for d in result.diagnostics.iter().chain(&declaration_diagnostics) {
        if !d.is_error() {
            continue;
        }
        match program.files.get(d.file.0 as usize) {
            // tsc's CLI reports a lib file's half of a clash with a project declaration
            // (TS2300 'Promise' in four lib files), but the baselines keep only some of them
            // and without a position (`lib.es5.d.ts(--,--)`, which `parse_line` skips): they
            // are not compared.
            Some(f) if f.is_lib => {}
            Some(f) => {
                let lc = f.lines().line_col_utf16(&f.text, d.span.start);
                // tsc's harness keeps test file names relative, and a source file's module
                // symbol is named by its file name (TS1192 `Module '"server"'`,
                // `typeof import("server")`).
                // TS6263, TS2306 and TS7016 name the resolved file as the harness gave it too,
                // and so do the declaration errors naming a module (TS4023 `external module`).
                let message = if d.message.contains("\"/.src/") || d.message.contains("'/.src/") {
                    d.message
                        .replace("'\"/.src/", "'\"")
                        .replace("import(\"/.src/", "import(\"")
                        .replace("resolved to '/.src/", "resolved to '")
                        .replace("File '/.src/", "File '")
                        .replace(". '/.src/", ". '")
                        .replace("from \"/.src/", "from \"")
                        .replace("external module \"/.src/", "external module \"")
                } else {
                    d.message.clone()
                };
                out.insert((shown(&f.path), lc.line + 1, lc.col + 1, d.code, message));
            }
            None => {
                // TS2209/TS2210 name the `package.json` as the harness gave it (relative).
                let message = d.message.replace("in file '/.src/", "in file '");
                out.insert((String::new(), 0, 0, d.code, message));
            }
        }
    }
    out
}

/// tsc's harness adds the program's declaration diagnostics when declarations are emitted
/// (`declaration` or `composite`), whether or not the program is emitted: TS2883, TS4094,
/// TS7056, the TS9xxx `isolatedDeclarations` errors, ... They come from declaration emit
/// (ts_r_emit), run here for declarations only. ts-r's own codes (TSR: a file it does not
/// write) and the output-path errors (program diagnostics to tsc) are left out.
fn declaration_diagnostics(
    fs: &MemoryFs,
    program: &ts_r_checker::ProgramFiles,
    options: &ts_r_config::CompilerOptions,
) -> Vec<ts_r_diagnostics::Diagnostic> {
    use ts_r_config::options::{OptionId, OptionValue};
    if !options.effective().declaration() {
        return Vec::new();
    }
    let mut options = options.clone();
    options.set(OptionId::NoEmit, Some(OptionValue::Bool(false)), None);
    options.set(
        OptionId::EmitDeclarationOnly,
        Some(OptionValue::Bool(true)),
        None,
    );
    let checker = ts_r_checker::Checker::new(program);
    let resolver = ts_r_emit::CheckerResolver::new(&checker, &options);
    let host = ts_r_checker::typenode::specifier_host(fs, &options);
    let cfg = ts_r_emit::EmitConfig {
        options: &options,
        config: None,
        threads: 1,
        case_sensitive: ts_r_config::FileSystem::case_sensitive(fs),
        specifiers: Some(&host),
    };
    let mut out = ts_r_emit::emit(program, &resolver, &cfg).diagnostics;
    out.retain(|d| d.code < ts_r_diagnostics::SOUND_CODE_BASE && !matches!(d.code, 5055 | 5056));
    out
}

fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or(s)
}

fn main() {
    let mut verbose = false;
    let mut filters: Vec<String> = Vec::new();
    let mut threads = 4usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--verbose" {
            verbose = true;
        } else if a == "--threads" {
            threads = args
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(4)
                .clamp(1, 4);
        } else if a == "--filter-file" {
            // Whitespace-separated filters from a file.
            if let Some(text) = args.next().and_then(|p| std::fs::read_to_string(p).ok()) {
                filters.extend(text.split_whitespace().map(str::to_owned));
            }
        } else {
            filters.push(a);
        }
    }
    let root = upstream();
    let Some(libs) = LibStore::locate() else {
        eprintln!("lib files not found; run `cargo xtask upstream sync --minimal`");
        std::process::exit(2);
    };
    let mut paths = Vec::new();
    let mut suites = Vec::new();
    for suite in ["compiler", "conformance"] {
        let mut v = Vec::new();
        collect(&root.join("tests/cases").join(suite), &mut v);
        let base = root
            .join("tsc/testdata/baselines/reference/submodule")
            .join(suite);
        let names: BTreeSet<String> = std::fs::read_dir(&base)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        suites.push((base, names));
        for p in v {
            let s = p.display().to_string();
            if filters.is_empty() || filters.iter().any(|f| s.contains(f.as_str())) {
                paths.push((suites.len() - 1, p));
            }
        }
    }
    let progress_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/checker-conformance-progress.txt");
    let progress = Mutex::new(std::fs::File::create(&progress_path).ok());
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let start = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            let b = std::thread::Builder::new().stack_size(512 << 20);
            let _ = b.spawn_scoped(s, || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((suite, p)) = paths.get(i) else {
                        break;
                    };
                    let (base, names) = &suites[*suite];
                    for test in load(p, &root, base, names) {
                        if let Ok(mut g) = progress.lock()
                            && let Some(f) = g.as_mut()
                        {
                            let _ = writeln!(f, "start {}", test.name);
                        }
                        let t0 = std::time::Instant::now();
                        let actual = run(&test, &libs);
                        let ms = t0.elapsed().as_millis();
                        if let Ok(mut g) = progress.lock()
                            && let Some(f) = g.as_mut()
                        {
                            let _ = writeln!(f, "done {} {ms}ms", test.name);
                        }
                        if let Ok(mut r) = results.lock() {
                            r.push((
                                test.name.clone(),
                                test.area.clone(),
                                test.expected.clone(),
                                actual,
                                ms,
                            ));
                        }
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_default();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    // area → (tests, exact, exact ignoring chain lines, tests with no expected errors,
    // false-positive tests, tests with any diagnostic tsc does not report)
    let mut areas: BTreeMap<String, [u32; 6]> = BTreeMap::new();
    let mut per_code: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new();
    let mut report = String::new();
    let mut slow = Vec::new();
    for (name, area, expected, actual, ms) in &results {
        if *ms > 2000 {
            slow.push((ms, name.clone()));
        }
        let top = area.split('/').next().unwrap_or("").to_owned();
        let key4 = |d: &Diag| (d.0.clone(), d.1, d.2, d.3, first_line(&d.4).to_owned());
        let exact = expected == actual;
        let ek: BTreeSet<_> = expected.iter().map(key4).collect();
        let ak: BTreeSet<_> = actual.iter().map(key4).collect();
        let loose = ek == ak;
        let missing: Vec<&Diag> = expected.iter().filter(|d| !actual.contains(d)).collect();
        let extra: Vec<&Diag> = actual.iter().filter(|d| !expected.contains(d)).collect();
        for a in [area.clone(), format!("{top}/*"), "ALL".to_owned()] {
            let e = areas.entry(a).or_default();
            e[0] += 1;
            e[1] += exact as u32;
            e[2] += loose as u32;
            if expected.is_empty() {
                e[3] += 1;
                e[4] += (!actual.is_empty()) as u32;
            }
            e[5] += (!extra.is_empty()) as u32;
        }
        for d in expected {
            per_code.entry(d.3).or_default().0 += 1;
        }
        for d in &missing {
            per_code.entry(d.3).or_default().1 += 1;
        }
        for d in &extra {
            per_code.entry(d.3).or_default().2 += 1;
        }
        if !exact {
            // `(tsc: none)` marks a false-positive test: tsc reports nothing.
            let none = if expected.is_empty() {
                " (tsc: none)"
            } else {
                ""
            };
            report.push_str(&format!("== {name}{none}\n"));
            for d in missing {
                report.push_str(&format!(
                    "  - {}({},{}) TS{}: {}\n",
                    d.0,
                    d.1,
                    d.2,
                    d.3,
                    d.4.replace('\n', "\n      ")
                ));
            }
            for d in extra {
                report.push_str(&format!(
                    "  + {}({},{}) TS{}: {}\n",
                    d.0,
                    d.1,
                    d.2,
                    d.3,
                    d.4.replace('\n', "\n      ")
                ));
            }
        }
    }
    let out = mismatch_path();
    let _ = std::fs::write(&out, &report);
    println!(
        "{} tests in {:.1}s (skipped: {} without a tsc 7 baseline, {} with --pretty baselines)",
        results.len(),
        start.elapsed().as_secs_f64(),
        SKIPPED_NO_BASELINE.load(Ordering::Relaxed),
        SKIPPED_PRETTY.load(Ordering::Relaxed),
    );
    println!(
        "area: tests / exact / exact-first-line / no-error tests / with false positives / with extra"
    );
    for (a, v) in &areas {
        if a.contains('/') && !a.ends_with("/*") && v[0] < 20 && filters.is_empty() {
            continue;
        }
        let pct = |n: u32| {
            if v[0] == 0 {
                0.0
            } else {
                100.0 * n as f64 / v[0] as f64
            }
        };
        println!(
            "  {a}: {} / {} ({:.1}%) / {} ({:.1}%) / {} / {} / {}",
            v[0],
            v[1],
            pct(v[1]),
            v[2],
            pct(v[2]),
            v[3],
            v[4],
            v[5]
        );
    }
    if verbose {
        println!("per code (expected / missing / extra):");
        for (c, (e, m, x)) in &per_code {
            println!("  TS{c}: {e} / {m} / {x}");
        }
    }
    slow.sort();
    for (ms, n) in slow.iter().rev().take(10) {
        println!("slow: {n} {ms}ms");
    }
    println!("mismatches: {}", out.display());
}
