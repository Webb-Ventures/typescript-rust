//! Reporting flags (design record 0012, E4): `--listFiles`, `--listFilesOnly`,
//! `--explainFiles`, `--diagnostics` and `--extendedDiagnostics`.
//!
//! The file list is the program's files, lib files first in tsc 7's lib order (the order of
//! the `lib` option's names, files it does not name last), then the rest in program order.
//! `--listFiles` prints absolute paths; `--explainFiles` prints each path relative to the
//! working directory, followed by why the file is in the program, indented by three spaces:
//!
//! - in the order the reasons are found: each root in turn (its include pattern, `files`
//!   entry, or the command line), and after each root, depth first, what it brings in
//!   (`/// <reference path>`, `/// <reference types>`, `/// <reference lib>`, then imports,
//!   each in source order); then `types` entries; then the libs (the `lib` option or the
//!   target's default library, and the libs those reference);
//! - then facts about the file: the referenced project source it is the output of, and,
//!   under `node16`/`nodenext` resolution, why it is an ECMAScript or CommonJS module.
//!
//! All observed in tsc 7's scenario baselines (`tsc/`, `tsbuild/`). Both lists follow the
//! diagnostics and `--listEmittedFiles`, before the summary. `--diagnostics` and
//! `--extendedDiagnostics` print counts and times to stderr (tsc's scenario baselines show
//! nothing for them on stdout).

use std::collections::HashMap;
use std::time::Duration;

use ts_r_checker::ProgramFiles;
use ts_r_config::{
    CompilerOptions, Effective, FileSystem, OptionId, OptionValue, ParsedConfig, cli::CommandLine,
    path,
};

use crate::relative;

/// Which file reports a build prints.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub list: bool,
    pub explain: bool,
    pub stats: bool,
    /// `--traceResolution` (`crate::trace`).
    pub trace: bool,
}

impl Flags {
    /// The flags on the command line (reporting flags are kept out of the merged options).
    pub fn from_command_line(cl: &CommandLine) -> Flags {
        Flags {
            list: cl.flag(OptionId::ListFiles),
            explain: cl.flag(OptionId::ExplainFiles),
            stats: cl.flag(OptionId::Diagnostics) || cl.flag(OptionId::ExtendedDiagnostics),
            trace: cl.flag(OptionId::TraceResolution),
        }
    }

    /// With the ones a config sets.
    pub fn with(self, options: &CompilerOptions) -> Flags {
        let on = |id| matches!(options.get(id), Some(OptionValue::Bool(true)));
        Flags {
            list: self.list || on(OptionId::ListFiles),
            explain: self.explain || on(OptionId::ExplainFiles),
            stats: self.stats || on(OptionId::Diagnostics) || on(OptionId::ExtendedDiagnostics),
            trace: self.trace || on(OptionId::TraceResolution),
        }
    }

    /// A report needs the loaded program.
    pub fn needs_program(self) -> bool {
        self.list || self.explain || self.stats || self.trace
    }
}

/// The `--listFiles` / `--explainFiles` text for `program` (empty without either).
pub fn files(
    program: &ProgramFiles,
    parsed: &ParsedConfig,
    flags: Flags,
    fs: &dyn FileSystem,
    cwd: &str,
) -> String {
    if flags.explain {
        explain(program, parsed, fs, cwd)
    } else if flags.list {
        list(program)
    } else {
        String::new()
    }
}

/// An include edge: the file it reaches and the reason shown for it.
type Edge = (usize, String);

/// Program files in tsc's listing order (indices into `program.files`).
fn ordered(program: &ProgramFiles) -> Vec<usize> {
    let mut libs: Vec<usize> = (0..program.files.len())
        .filter(|&i| program.files[i].is_lib)
        .collect();
    libs.sort_by_key(|&i| lib_rank(&program.files[i].path));
    libs.extend((0..program.files.len()).filter(|&i| !program.files[i].is_lib));
    libs
}

/// A lib file's place in the `lib` option's list of names; `lib.d.ts` and `lib.es6.d.ts`
/// come first and files the list does not name (the targets' `.full` default libraries)
/// last (observed: `tsbuild/sample/when-target-option-changes`).
fn lib_rank(p: &str) -> usize {
    let name = path::file_name(p);
    if name == "lib.d.ts" || name == "lib.es6.d.ts" {
        return 0;
    }
    ts_r_config::enums::LIBS
        .iter()
        .position(|(_, f)| *f == name)
        .map_or(usize::MAX, |i| i + 1)
}

/// `--listFiles`, `--listFilesOnly`: absolute paths.
pub fn list(program: &ProgramFiles) -> String {
    let mut out = String::new();
    for i in ordered(program) {
        out.push_str(&program.files[i].path);
        out.push('\n');
    }
    out
}

/// `--explainFiles`.
pub fn explain(
    program: &ProgramFiles,
    parsed: &ParsedConfig,
    fs: &dyn FileSystem,
    cwd: &str,
) -> String {
    let files = &program.files;
    let cs = fs.case_sensitive();
    let index: HashMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (path::key(&f.path, cs), i))
        .collect();
    let find = |p: &str| index.get(&path::key(p, cs)).copied();
    let show = |p: &str| relative(p, cwd);
    let resolver = ts_r_resolver::Resolver::new(
        &fs,
        ts_r_resolver::ResolverOptions::from_compiler_options(&parsed.options),
    );
    let mut reasons: Vec<Vec<String>> = vec![Vec::new(); files.len()];
    // Each file's references and imports, in the order tsc follows them.
    let edges = |i: usize| -> Vec<Edge> {
        let f = &files[i];
        let from = show(&f.path);
        let mut out = Vec::new();
        for group in ["path", "types", "lib"] {
            for (kind, v) in &f.references {
                let same = if group == "types" {
                    kind.starts_with("types")
                } else {
                    *kind == group
                };
                if !same {
                    continue;
                }
                let target = if group == "lib" {
                    ts_r_config::lib_file_name(v).and_then(|name| {
                        files
                            .iter()
                            .position(|l| l.is_lib && path::file_name(&l.path) == name)
                    })
                } else {
                    ts_r_checker::program::reference_target(&fs, &resolver, &f.path, kind, v)
                        .and_then(|t| find(&t))
                };
                let Some(t) = target else { continue };
                let reason = match group {
                    "path" => format!("Referenced via '{v}' from file '{from}'"),
                    "types" => format!("Type library referenced via '{v}' from file '{from}'"),
                    _ => format!("Library referenced via '{v}' from file '{from}'"),
                };
                out.push((t, reason));
            }
        }
        for req in &f.bound.module_requests {
            let spec = f.ast.atom(req.specifier);
            let by_mode = req.resolution_mode.and_then(|m| {
                program
                    .mode_resolutions
                    .get(&(f.path.clone(), spec.to_owned(), m))
            });
            let res =
                by_mode.or_else(|| program.resolutions.get(&(f.path.clone(), spec.to_owned())));
            let Some(Some(r)) = res else { continue };
            if !r.included {
                continue;
            }
            let Some(t) = find(&r.resolved) else { continue };
            let start = f.ast.span(req.node).start as usize;
            let quote = match f.text.as_bytes().get(start) {
                Some(b'\'') => '\'',
                _ => '"',
            };
            let mut reason = format!("Imported via {quote}{spec}{quote} from file '{from}'");
            // Package identities come from `node_modules` lookups only: resolve bare
            // specifiers again (a symlinked package is not `external`, but has an identity).
            if r.external || !(spec.starts_with('.') || spec.starts_with('/')) {
                let mode = ts_r_checker::program::request_mode(
                    parsed.options.effective().module(),
                    &f.path,
                    f.package_esm,
                    req,
                );
                if let Some(id) = resolver
                    .resolve_module(spec, &f.path, mode)
                    .and_then(|m| m.package_id.clone())
                {
                    reason.push_str(&format!(" with packageId '{id}'"));
                }
            }
            out.push((t, reason));
        }
        out
    };
    // Depth first from each root, in root order.
    let mut visited = vec![false; files.len()];
    let mut walk = |start: usize, reasons: &mut Vec<Vec<String>>| {
        // (file, its outgoing edges, the next edge to follow)
        let mut stack: Vec<(usize, Vec<Edge>, usize)> = Vec::new();
        if !visited[start] {
            visited[start] = true;
            stack.push((start, edges(start), 0));
        }
        while let Some((_, out, next)) = stack.last_mut() {
            let Some((t, reason)) = out.get(*next).cloned() else {
                stack.pop();
                continue;
            };
            *next += 1;
            reasons[t].push(reason);
            if !visited[t] && !files[t].is_lib {
                visited[t] = true;
                stack.push((t, edges(t), 0));
            }
        }
    };
    let config_rel = show(&parsed.config_path);
    for (k, root) in program.roots.iter().enumerate() {
        let Some(i) = find(root) else { continue };
        let original = parsed.file_names.get(k).unwrap_or(root);
        reasons[i].push(root_reason(parsed, original, &config_rel, cs));
        walk(i, &mut reasons);
    }
    // `types` entries.
    let effective = parsed.options.effective();
    let explicit: &[String] = match parsed.options.get(OptionId::Types) {
        Some(_) => effective.types(),
        None => &[],
    };
    for t in ts_r_checker::program::expand_wildcard_types(&fs, &resolver, effective.types()) {
        let Some(r) =
            resolver.resolve_type_reference(&t, None, ts_r_resolver::ResolutionMode::Import)
        else {
            continue;
        };
        let Some(i) = find(&r.resolved_file_name) else {
            continue;
        };
        reasons[i].push(if explicit.contains(&t) {
            format!("Entry point of type library '{t}' specified in compilerOptions")
        } else {
            format!("Entry point for implicit type library '{t}'")
        });
        walk(i, &mut reasons);
    }
    lib_reasons(program, &parsed.options, &effective, &show, &mut reasons);
    // Facts about each file.
    let mut outputs_of: HashMap<usize, String> = HashMap::new();
    for (k, root) in program.roots.iter().enumerate() {
        if let (Some(i), Some(original)) = (find(root), parsed.file_names.get(k))
            && !path::eq(root, original, cs)
        {
            outputs_of.insert(i, original.clone());
        }
    }
    let mut keys: Vec<_> = program.resolutions.values().flatten().collect();
    keys.sort_by(|a, b| a.resolved.cmp(&b.resolved));
    for r in keys {
        if let (Some(src), Some(i)) = (&r.reference_source, find(&r.resolved)) {
            outputs_of.entry(i).or_insert_with(|| src.clone());
        }
    }
    let node_resolution = matches!(
        effective.module_resolution(),
        ts_r_config::ModuleResolution::Node16 | ts_r_config::ModuleResolution::NodeNext
    );
    let mut out = String::new();
    for i in ordered(program) {
        let f = &files[i];
        out.push_str(&show(&f.path));
        out.push('\n');
        let mut lines = std::mem::take(&mut reasons[i]);
        if let Some(src) = outputs_of.get(&i) {
            lines.push(format!(
                "File is output of project reference source '{}'",
                show(src)
            ));
        }
        if node_resolution
            && !f.is_lib
            && let Some(line) = module_format(fs, &f.path, &show)
        {
            lines.push(line);
        }
        for l in lines {
            out.push_str("   ");
            out.push_str(&l);
            out.push('\n');
        }
    }
    out
}

/// Why a root file is in the program.
fn root_reason(parsed: &ParsedConfig, file: &str, config_rel: &str, cs: bool) -> String {
    if parsed.config_path.is_empty() {
        return "Root file specified for compilation".to_owned();
    }
    if let Some(files) = &parsed.files
        && files
            .specs
            .iter()
            .any(|s| path::eq(&path::resolve(&files.base_dir, s), file, cs))
    {
        return format!(
            "Part of 'files' list in {}",
            path::file_name(&parsed.config_path)
        );
    }
    match &parsed.include {
        None => "Matched by default include pattern '**/*'".to_owned(),
        Some(inc) => {
            let spec = inc.specs.iter().find(|s| {
                let abs = path::resolve(&inc.base_dir, s);
                ts_r_config::glob::Pattern::new(&abs, true).matches(file, cs)
            });
            match spec {
                Some(s) => format!("Matched by include pattern '{s}' in '{config_rel}'"),
                None => "Matched by default include pattern '**/*'".to_owned(),
            }
        }
    }
}

/// The `lib` option's files (or the target's default library), then the libs they reference.
fn lib_reasons(
    program: &ProgramFiles,
    options: &CompilerOptions,
    effective: &Effective<'_>,
    show: &dyn Fn(&str) -> String,
    reasons: &mut [Vec<String>],
) {
    let files = &program.files;
    let lib_index = |name: &str| {
        files
            .iter()
            .position(|l| l.is_lib && path::file_name(&l.path) == name)
    };
    let explicit = options.get(OptionId::Lib).is_some();
    let mut seen = vec![false; files.len()];
    let mut stack: Vec<usize> = Vec::new();
    for name in effective.lib_files() {
        let Some(i) = lib_index(name) else { continue };
        reasons[i].push(if explicit {
            format!("Library '{name}' specified in compilerOptions")
        } else {
            format!(
                "Default library for target '{}'",
                effective.target().display_name()
            )
        });
        stack.push(i);
        // The libs this one references, depth first (bounded: each lib once).
        while let Some(j) = stack.pop() {
            if std::mem::replace(&mut seen[j], true) {
                continue;
            }
            let mut next = Vec::new();
            for (kind, v) in &files[j].references {
                if *kind != "lib" {
                    continue;
                }
                let Some(t) = ts_r_config::lib_file_name(v).and_then(lib_index) else {
                    continue;
                };
                reasons[t].push(format!(
                    "Library referenced via '{v}' from file '{}'",
                    show(&files[j].path)
                ));
                next.push(t);
            }
            stack.extend(next.into_iter().rev());
        }
    }
}

/// Why a file is an ECMAScript or CommonJS module under `node16`/`nodenext` resolution: its
/// nearest `package.json` and its `type` field. Files whose extension decides
/// (`.mts`, `.cts`, ...) and JSON files get none.
fn module_format(fs: &dyn FileSystem, file: &str, show: &dyn Fn(&str) -> String) -> Option<String> {
    let lower = file.to_ascii_lowercase();
    let decided = [".mts", ".cts", ".mjs", ".cjs", ".json"];
    if decided.iter().any(|e| lower.ends_with(e)) {
        return None;
    }
    let mut dir = path::directory_of(file).to_owned();
    for _ in 0..256 {
        let pkg = path::combine(&dir, "package.json");
        if fs.file_exists(&pkg) {
            let text = fs.read_file(&pkg).unwrap_or_default();
            let parsed = ts_r_config::jsonc::parse(&text, &pkg);
            let ty = parsed
                .root
                .as_ref()
                .and_then(|v| v.get("type"))
                .map(|v| v.as_str().map(str::to_owned));
            let shown = show(&pkg);
            return Some(match ty {
                Some(Some(t)) if t == "module" => format!(
                    "File is ECMAScript module because '{shown}' has field \"type\" with value \"module\""
                ),
                Some(_) => format!(
                    "File is CommonJS module because '{shown}' has field \"type\" whose value is not \"module\""
                ),
                None => format!(
                    "File is CommonJS module because '{shown}' does not have field \"type\""
                ),
            });
        }
        let parent = path::directory_of(&dir).to_owned();
        if parent == dir || parent.is_empty() {
            break;
        }
        dir = parent;
    }
    Some("File is CommonJS module because 'package.json' was not found".to_owned())
}

/// `--diagnostics`, `--extendedDiagnostics`: counts and the time taken, on stderr.
pub fn stats(program: &ProgramFiles, elapsed: Duration) {
    let lines: usize = program.files.iter().map(|f| f.text.lines().count()).sum();
    let libs = program.files.iter().filter(|f| f.is_lib).count();
    eprintln!("Files:              {}", program.files.len());
    eprintln!("Lines of Library:   {}", {
        program
            .files
            .iter()
            .filter(|f| f.is_lib)
            .map(|f| f.text.lines().count())
            .sum::<usize>()
    });
    eprintln!("Lines:              {lines}");
    eprintln!("Library files:      {libs}");
    eprintln!("Total time:         {:.2}s", elapsed.as_secs_f64());
}
