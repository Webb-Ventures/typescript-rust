//! A project's references as the program sees them: the source-to-output map the resolver
//! redirects through ([`ts_r_resolver::ProjectReferences`]), and tsc's checks of the
//! references themselves (TS6306, TS6310) and of composite programs (TS6307).

use ts_r_checker::ProgramFiles;
use ts_r_config::{ConfigDiagnostic, ConfigLoader, FileSystem, ParsedConfig, path};
use ts_r_diagnostics::{Category, Diagnostic, FileId};
use ts_r_emit::PathRules;
use ts_r_resolver::ProjectReferences;
use ts_r_span::Span;

/// The referenced projects of `config`, transitively, depth first in reference order (each
/// once; `config` itself excluded). `load` loads a config by path; missing configs are
/// skipped (the loader reported TS6053 at the reference).
pub fn referenced_configs(
    config: &ParsedConfig,
    load: &mut dyn FnMut(&str) -> Option<ParsedConfig>,
) -> Vec<ParsedConfig> {
    let cs = config.case_sensitive();
    let mut out: Vec<ParsedConfig> = Vec::new();
    let mut seen = vec![path::key(&config.config_path, cs)];
    let mut stack: Vec<String> = config
        .references
        .iter()
        .rev()
        .map(|r| r.config_path.clone())
        .collect();
    while let Some(p) = stack.pop() {
        let k = path::key(&p, cs);
        if seen.contains(&k) {
            continue;
        }
        seen.push(k);
        let Some(c) = load(&p) else {
            continue;
        };
        for r in c.references.iter().rev() {
            stack.push(r.config_path.clone());
        }
        out.push(c);
    }
    out
}

/// Loads a referenced config from disk.
pub fn load_config(fs: &dyn FileSystem, config_path: &str) -> Option<ParsedConfig> {
    fs.file_exists(config_path)
        .then(|| ConfigLoader::new(fs).load(config_path))
}

/// Output rules of a referenced project (its `rootDir` defaults to its directory).
pub fn path_rules(c: &ParsedConfig) -> PathRules {
    let o = &c.options;
    PathRules {
        root_dir: o
            .root_dir()
            .map_or_else(|| c.config_dir.clone(), str::to_owned),
        out_dir: o.out_dir().map(str::to_owned),
        declaration_dir: o.declaration_dir().map(str::to_owned),
        preserve_jsx: o.jsx() == Some(ts_r_config::JsxEmit::Preserve),
        case_sensitive: c.case_sensitive(),
    }
}

/// The resolver's map for the projects in `refs`: every source root file to its
/// declaration output (and JavaScript output).
pub fn project_references(refs: &[ParsedConfig], case_sensitive: bool) -> ProjectReferences {
    let mut map = ProjectReferences::new(case_sensitive);
    for c in refs {
        // Requests in the project's files resolve with its own options.
        let project = map.add_project(
            &c.config_path,
            ts_r_resolver::ResolverOptions::from_compiler_options(&c.options),
        );
        for f in &c.file_names {
            map.add_member(f, project);
        }
        // A project that does not emit (TS6310) is used from source.
        if c.options.no_emit() == Some(true) {
            continue;
        }
        let rules = path_rules(c);
        use ts_r_config::ModuleKind as M;
        let esm = matches!(
            c.options.effective().module(),
            M::ES2015 | M::ES2020 | M::ES2022 | M::ESNext
        );
        for f in &c.file_names {
            let Some(dts) = rules.dts_output(f) else {
                continue;
            };
            let js = rules.js_output(f);
            map.add(f, &dts, js.as_deref());
            if esm {
                map.mark_esm(f);
            }
        }
    }
    map
}

/// Drops the outputs of program files that belong to a referenced project. Those are only in
/// the program when the reference is not redirected (a `noEmit` project, TS6310); tsc never
/// emits another project's sources (observed: `when-project-references-composite-project-
/// with-noEmit` writes only the referencing project's `index.js`).
pub fn drop_referenced_outputs(
    result: &mut ts_r_emit::EmitResult,
    program: &ProgramFiles,
    refs: &[ParsedConfig],
    case_sensitive: bool,
) {
    if refs.is_empty() {
        return;
    }
    let owned: rustc_hash::FxHashSet<String> = refs
        .iter()
        .flat_map(|c| c.file_names.iter())
        .map(|f| path::key(f, case_sensitive))
        .collect();
    result.outputs.retain(|o| {
        program
            .files
            .get(o.source)
            .is_none_or(|f| !owned.contains(&path::key(&f.path, case_sensitive)))
    });
}

/// TS6306 (a referenced project must be `composite`, unless it is a container: no files)
/// and TS6310 (it may not set `noEmit`), at the reference in `config`.
pub fn reference_diagnostics(
    config: &ParsedConfig,
    load: &mut dyn FnMut(&str) -> Option<ParsedConfig>,
    build_mode: bool,
) -> Vec<ConfigDiagnostic> {
    let mut out = Vec::new();
    // A solution (no files of its own) has no program to check them against.
    if config.file_names.is_empty() {
        return out;
    }
    let cs = config.case_sensitive();
    // The buildinfo this project writes (`--build` writes one for every project).
    let own_buildinfo = (build_mode || crate::buildinfo::is_incremental(&config.options))
        .then(|| crate::buildinfo::path_for(&config.options, &config.config_path));
    for r in &config.references {
        let Some(c) = load(&r.config_path) else {
            continue;
        };
        let shown = path::resolve(&config.config_dir, &r.path);
        if c.options.composite() != Some(true) && !c.file_names.is_empty() {
            out.push(ConfigDiagnostic::at(
                6306,
                format!("Referenced project '{shown}' must have setting \"composite\": true."),
                &config.config_path,
                r.span,
            ));
        } else if c.options.no_emit() == Some(true) {
            out.push(ConfigDiagnostic::at(
                6310,
                format!("Referenced project '{shown}' may not disable emit."),
                &config.config_path,
                r.span,
            ));
        }
        // Both projects writing one buildinfo file (observed: TS6377 at the reference).
        if let Some(own) = &own_buildinfo
            && (build_mode || crate::buildinfo::is_incremental(&c.options))
            && path::eq(
                own,
                &crate::buildinfo::path_for(&c.options, &c.config_path),
                cs,
            )
        {
            out.push(ConfigDiagnostic::at(
                6377,
                format!(
                    "Cannot write file '{own}' because it will overwrite '.tsbuildinfo' file generated by referenced project '{shown}'"
                ),
                &config.config_path,
                r.span,
            ));
        }
    }
    out
}

/// Whether `diagnostics` hold TS6377: the project's buildinfo would overwrite a referenced
/// project's, so it is not written (its outputs are, in full, and then touched).
pub fn buildinfo_clash(diagnostics: &[ConfigDiagnostic]) -> bool {
    diagnostics.iter().any(|d| d.code == 6377)
}

/// TS6307 for a `composite` program: every source file must be a root, else an error at the
/// import that brought it in. Declaration files, libraries and external files are exempt,
/// and JSON files that are not written (`ts_r_emit::imported_json_is_emitted`).
pub fn composite_diagnostics(program: &ProgramFiles, config: &ParsedConfig) -> Vec<Diagnostic> {
    if config.options.composite() != Some(true) {
        return Vec::new();
    }
    let cs = config.case_sensitive();
    let roots: std::collections::HashSet<String> =
        config.file_names.iter().map(|f| path::key(f, cs)).collect();
    let mut out = Vec::new();
    for f in &program.files {
        if f.is_lib
            || f.is_declaration()
            || f.is_external
            || roots.contains(&path::key(&f.path, cs))
        {
            continue;
        }
        if f.path.to_ascii_lowercase().ends_with(".json")
            && !ts_r_emit::imported_json_is_emitted(
                Some(config.options.root_dir().unwrap_or(&config.config_dir)),
                config.options.out_dir(),
                &f.path,
                cs,
            )
        {
            continue;
        }
        let Some((from, span)) = first_import_of(program, &f.path) else {
            continue;
        };
        out.push(Diagnostic {
            code: 6307,
            category: Category::Error,
            message: format!(
                "File '{}' is not listed within the file list of project '{}'. Projects must list all files or use an 'include' pattern.",
                f.path, config.config_path
            ),
            file: FileId(from as u32),
            span,
            related: Vec::new(),
        });
    }
    out
}

/// The first import (in program order) that resolves to `target`: (file index, specifier
/// span).
fn first_import_of(program: &ProgramFiles, target: &str) -> Option<(usize, Span)> {
    for (i, f) in program.files.iter().enumerate() {
        for req in &f.bound.module_requests {
            let spec = f.ast.atom(req.specifier);
            if let Some(Some(r)) = program.resolutions.get(&(f.path.clone(), spec.to_owned()))
                && r.resolved == target
            {
                let s = f.ast.span(req.node);
                return Some((i, Span::new(s.start, s.end)));
            }
        }
    }
    None
}
