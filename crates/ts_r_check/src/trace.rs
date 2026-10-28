//! `--traceResolution` (design record 0012, E4): every module and type reference
//! resolution of a program, logged in tsc 7's wording by `ts_r_resolver`'s trace.
//!
//! The program is loaded first; the trace replays its resolutions with a fresh resolver
//! in the order tsc runs them (observed in its baselines): depth first from each root in
//! order, each file's `/// <reference types>` directives, then its module requests (imports
//! in source order, then module augmentations, then the JSX runtime import), then its
//! children (`/// <reference path>` targets, type reference targets, imported files); after
//! the roots, the `types` entries. A file is visited once; a request repeated in a file is
//! traced again. Lib files are not traced, except that `libReplacement` looks each one up as
//! a `@typescript/lib-*` package at the end. The trace prints before the diagnostics.

use std::collections::HashMap;
use std::sync::Arc;

use ts_r_binder::RequestKind;
use ts_r_checker::ProgramFiles;
use ts_r_config::{FileSystem, path};
use ts_r_resolver::{
    ImportKind, ProjectReferences, ResolutionMode, Resolver, ResolverOptions, Trace,
};

/// The `--traceResolution` text of `program`: one line per step, each ending in a newline.
pub fn text<F: FileSystem>(
    fs: &F,
    program: &ProgramFiles,
    references: Option<Arc<ProjectReferences>>,
) -> String {
    let mut out = String::new();
    for line in trace(fs, program, references, Trace::new()) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The trace of `program`'s resolutions, one line each (`trace` carries the probe memory
/// and the compiler version the messages name).
pub fn trace<F: FileSystem>(
    fs: &F,
    program: &ProgramFiles,
    references: Option<Arc<ProjectReferences>>,
    mut trace: Trace,
) -> Vec<String> {
    let mut opts = ResolverOptions::from_compiler_options(&program.options);
    opts.project_references = references;
    let resolver = Resolver::new(fs, opts);
    let files = &program.files;
    let cs = fs.case_sensitive();
    let index: HashMap<String, usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.is_lib)
        .map(|(i, f)| (path::key(&f.path, cs), i))
        .collect();
    let find = |p: &str| index.get(&path::key(p, cs)).copied();
    let mut visited = vec![false; files.len()];
    // Resolves (and traces) file `i`'s requests; returns the files they reach, in order.
    let visit = |i: usize, t: &mut Trace| -> Vec<usize> {
        let f = &files[i];
        let mut path_refs = Vec::new();
        let mut type_refs = Vec::new();
        let mut imports = Vec::new();
        for (kind, v) in &f.references {
            match *kind {
                "path" => {
                    if let Some(p) =
                        ts_r_checker::program::reference_target(fs, &resolver, &f.path, kind, v)
                    {
                        path_refs.extend(find(&p));
                    }
                }
                "types" | "types-import" | "types-require" => {
                    let mode = match *kind {
                        "types-import" => ResolutionMode::Import,
                        "types-require" => ResolutionMode::Require,
                        _ if resolver.options().module.is_node() => {
                            resolver.mode_for(&f.path, ImportKind::Static)
                        }
                        _ => ResolutionMode::Import,
                    };
                    if let Some(r) =
                        resolver.resolve_type_reference_traced(v, Some(&f.path), mode, t)
                    {
                        type_refs.extend(find(&r.resolved_file_name));
                    }
                }
                _ => {}
            }
        }
        let requests = f
            .bound
            .module_requests
            .iter()
            .filter(|r| !matches!(r.kind, RequestKind::Augmentation))
            .chain(
                f.bound
                    .module_requests
                    .iter()
                    .filter(|r| matches!(r.kind, RequestKind::Augmentation)),
            );
        for req in requests {
            let spec = f.ast.atom(req.specifier);
            let kind = match req.kind {
                RequestKind::DynamicImport => ImportKind::Dynamic,
                RequestKind::ImportEquals => ImportKind::ImportEquals,
                _ => ImportKind::Static,
            };
            let mode = req
                .resolution_mode
                .unwrap_or_else(|| resolver.mode_for(&f.path, kind));
            if let Some(r) = resolver.resolve_module_traced(spec, &f.path, mode, t) {
                imports.extend(find(&r.resolved_file_name));
            }
        }
        if let Some(spec) = ts_r_checker::jsx_runtime::runtime_specifier(f, &program.options) {
            let mode = resolver.mode_for(&f.path, ImportKind::Static);
            if let Some(r) = resolver.resolve_module_traced(&spec, &f.path, mode, t) {
                imports.extend(find(&r.resolved_file_name));
            }
        }
        path_refs.extend(type_refs);
        path_refs.extend(imports);
        path_refs
    };
    // Depth first, pre-order: (children, next child).
    let walk = |start: usize, t: &mut Trace, visited: &mut Vec<bool>| {
        if visited[start] {
            return;
        }
        visited[start] = true;
        let mut stack: Vec<(Vec<usize>, usize)> = vec![(visit(start, t), 0)];
        while let Some((children, next)) = stack.last_mut() {
            let Some(&c) = children.get(*next) else {
                stack.pop();
                continue;
            };
            *next += 1;
            if !visited[c] {
                visited[c] = true;
                let grand = visit(c, t);
                stack.push((grand, 0));
            }
        }
    };
    for root in &program.roots {
        if let Some(i) = find(root) {
            walk(i, &mut trace, &mut visited);
        }
    }
    let types = ts_r_checker::program::expand_wildcard_types(
        fs,
        &resolver,
        program.options.effective().types(),
    );
    for name in types {
        if let Some(r) =
            resolver.resolve_type_reference_traced(&name, None, ResolutionMode::Import, &mut trace)
            && let Some(i) = find(&r.resolved_file_name)
        {
            walk(i, &mut trace, &mut visited);
        }
    }
    // `libReplacement`: each lib file of the program, by file name, is looked up as a
    // `@typescript/lib-*` package from a pseudo file in the config (or working) directory.
    if program.options.lib_replacement() == Some(true) {
        let mut libs: Vec<&str> = files
            .iter()
            .filter(|f| f.is_lib)
            .map(|f| path::file_name(&f.path))
            .collect();
        libs.sort_unstable();
        libs.dedup();
        let base = resolver
            .options()
            .config_dir
            .clone()
            .unwrap_or_else(|| fs.cwd());
        for lib in libs {
            let Some(spec) = ts_r_checker::program::lib_replacement_specifier(lib) else {
                continue;
            };
            let containing = ts_r_checker::program::lib_replacement_lookup_file(&base, lib);
            // Always in CommonJS mode (observed under nodenext in an ESM package too).
            resolver.resolve_module_traced(&spec, &containing, ResolutionMode::Require, &mut trace);
        }
    }
    trace.take_lines()
}
