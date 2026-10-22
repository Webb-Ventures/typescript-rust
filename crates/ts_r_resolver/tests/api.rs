//! Resolver API behaviour on an in-memory file system.

use std::sync::Arc;

use ts_r_config::{MemoryFs, load_tsconfig};
use ts_r_resolver::{
    Extension, ImportKind, ModuleFormat, ResolutionMode, Resolver, ResolverOptions, Trace,
    mangle_scoped_name, split_package_name,
};

ts_r_testing::cap_memory!();

fn resolver<'a>(fs: &'a MemoryFs, config: &str) -> Resolver<&'a MemoryFs> {
    let cfg = load_tsconfig(fs, config);
    // TS18003 (no inputs) is expected for configs without source files.
    assert!(
        cfg.diagnostics.iter().all(|d| d.code == 18003),
        "{:?}",
        cfg.diagnostics
    );
    Resolver::new(fs, ResolverOptions::from_compiler_options(&cfg.options))
}

fn resolved(
    r: &Resolver<&MemoryFs>,
    spec: &str,
    from: &str,
    mode: ResolutionMode,
) -> Option<String> {
    r.resolve_module(spec, from, mode)
        .map(|m| m.resolved_file_name.clone())
}

#[test]
fn package_name_helpers() {
    assert_eq!(split_package_name("a"), ("a", ""));
    assert_eq!(split_package_name("a/b/c"), ("a", "b/c"));
    assert_eq!(split_package_name("@s/a"), ("@s/a", ""));
    assert_eq!(split_package_name("@s/a/b"), ("@s/a", "b"));
    assert_eq!(mangle_scoped_name("@s/a"), "s__a");
    assert_eq!(mangle_scoped_name("a"), "a");
}

#[test]
fn modes_and_formats_under_nodenext() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext" } }"#,
    )
    .add_file("/p/package.json", r#"{ "type": "module" }"#)
    .add_file("/p/cjs/package.json", r#"{ "type": "commonjs" }"#);
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        r.file_format("/p/a.ts"),
        (ModuleFormat::Esm, Some("/p".into()))
    );
    assert_eq!(r.file_format("/p/a.cts").0, ModuleFormat::CommonJs);
    assert_eq!(
        r.file_format("/p/cjs/a.ts"),
        (ModuleFormat::CommonJs, Some("/p/cjs".into()))
    );
    assert_eq!(r.file_format("/p/cjs/a.mts").0, ModuleFormat::Esm);
    assert_eq!(
        r.mode_for("/p/a.ts", ImportKind::Static),
        ResolutionMode::Import
    );
    assert_eq!(
        r.mode_for("/p/cjs/a.ts", ImportKind::Static),
        ResolutionMode::Require
    );
    assert_eq!(
        r.mode_for("/p/cjs/a.ts", ImportKind::Dynamic),
        ResolutionMode::Import
    );
    assert_eq!(
        r.mode_for("/p/a.ts", ImportKind::ImportEquals),
        ResolutionMode::Require
    );
    assert_eq!(
        r.conditions(ResolutionMode::Import),
        ["import", "types", "node"]
    );
}

#[test]
fn modes_under_bundler() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "esnext", "customConditions": ["dev"] } }"#,
    );
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        r.mode_for("/p/a.ts", ImportKind::Static),
        ResolutionMode::Import
    );
    assert_eq!(
        r.mode_for("/p/a.cts", ImportKind::Static),
        ResolutionMode::Require
    );
    assert_eq!(
        r.mode_for("/p/a.js", ImportKind::RequireCall),
        ResolutionMode::Require
    );
    assert_eq!(
        r.conditions(ResolutionMode::Require),
        ["require", "types", "dev"]
    );

    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "commonjs" } }"#,
    );
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        r.mode_for("/p/a.ts", ImportKind::Dynamic),
        ResolutionMode::Require
    );
}

#[test]
fn esm_mode_needs_extensions() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext" } }"#,
    )
    .add_file("/p/a.ts", "")
    .add_file("/p/b.ts", "")
    .add_file("/p/dir/index.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(resolved(&r, "./b", "/p/a.ts", ResolutionMode::Import), None);
    assert_eq!(
        resolved(&r, "./b.js", "/p/a.ts", ResolutionMode::Import).as_deref(),
        Some("/p/b.ts")
    );
    assert_eq!(
        resolved(&r, "./b", "/p/a.ts", ResolutionMode::Require).as_deref(),
        Some("/p/b.ts")
    );
    assert_eq!(
        resolved(&r, "./dir", "/p/a.ts", ResolutionMode::Import),
        None
    );
    assert_eq!(
        resolved(&r, "./dir", "/p/a.ts", ResolutionMode::Require).as_deref(),
        Some("/p/dir/index.ts")
    );
    let m = r
        .resolve_module("./b.ts", "/p/a.ts", ResolutionMode::Import)
        .unwrap();
    assert!(m.resolved_using_ts_extension);
    assert_eq!(m.extension, Extension::Ts);
}

#[test]
fn ts_extension_flag_needs_the_written_extension_to_find_the_file() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "commonjs", "moduleResolution": "bundler",
            "paths": { "@a/*": ["./src/*"], "@b/*": ["./src/*.ts"] } } }"#,
    )
    .add_file("/p/a.ts", "")
    .add_file("/p/b.js", "")
    .add_file("/p/dir.ts/index.ts", "")
    .add_file("/p/src/c.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    let flag = |spec: &str| {
        r.resolve_module(spec, "/p/a.ts", ResolutionMode::Require)
            .map(|m| m.resolved_using_ts_extension)
    };
    // A directory `index` behind a `.ts`-looking name was not found through `.ts`.
    assert_eq!(flag("./dir.ts"), Some(false));
    assert_eq!(flag("./b.js"), Some(false));
    // Through `paths`: only when the specifier brought the extension in.
    assert_eq!(flag("@a/c.ts"), Some(true));
    assert_eq!(flag("@b/c"), Some(false));
}

#[test]
fn symlinked_packages_resolve_to_real_paths() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "esnext" } }"#,
    )
    .add_file("/p/src/a.ts", "")
    .add_file(
        "/p/node_modules/.pnpm/x@1.0.0/node_modules/x/package.json",
        r#"{ "name": "x", "version": "1.0.0", "types": "x.d.ts" }"#,
    )
    .add_file("/p/node_modules/.pnpm/x@1.0.0/node_modules/x/x.d.ts", "")
    .add_symlink("/p/node_modules/x", ".pnpm/x@1.0.0/node_modules/x")
    .add_file(
        "/p/packages/w/package.json",
        r#"{ "name": "w", "version": "0.1.0", "exports": { "types": "./src/index.ts" } }"#,
    )
    .add_file("/p/packages/w/src/index.ts", "")
    .add_symlink("/p/node_modules/w", "../packages/w");
    let r = resolver(&fs, "/p/tsconfig.json");
    let x = r
        .resolve_module("x", "/p/src/a.ts", ResolutionMode::Import)
        .unwrap();
    assert_eq!(
        x.resolved_file_name,
        "/p/node_modules/.pnpm/x@1.0.0/node_modules/x/x.d.ts"
    );
    assert_eq!(x.original_path.as_deref(), Some("/p/node_modules/x/x.d.ts"));
    assert_eq!(x.package_id.as_ref().unwrap().to_string(), "x/x.d.ts@1.0.0");
    assert!(x.is_external_library_import);
    let w = r
        .resolve_module("w", "/p/src/a.ts", ResolutionMode::Import)
        .unwrap();
    assert_eq!(w.resolved_file_name, "/p/packages/w/src/index.ts");
    // tsc 7 still checks a linked workspace package's sources, but treats them as a library:
    // not emitted, not under the rootDir / composite file-list rules (observed).
    assert!(w.is_external_library_import);

    let mut fs2 = fs.clone();
    fs2.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "esnext", "preserveSymlinks": true } }"#,
    );
    let r = resolver(&fs2, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "x", "/p/src/a.ts", ResolutionMode::Import).as_deref(),
        Some("/p/node_modules/x/x.d.ts")
    );
}

#[test]
fn paths_fall_through_to_node_modules() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "esnext", "paths": { "*": ["./types/*"], "@app/*": ["./src/*"] } } }"#,
    )
    .add_file("/p/src/a.ts", "")
    .add_file("/p/src/util.ts", "")
    .add_file("/p/types/shim.d.ts", "")
    .add_file("/p/node_modules/lib/index.d.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "@app/util", "/p/src/a.ts", ResolutionMode::Import).as_deref(),
        Some("/p/src/util.ts")
    );
    assert_eq!(
        resolved(&r, "shim", "/p/src/a.ts", ResolutionMode::Import).as_deref(),
        Some("/p/types/shim.d.ts")
    );
    assert_eq!(
        resolved(&r, "lib", "/p/src/a.ts", ResolutionMode::Import).as_deref(),
        Some("/p/node_modules/lib/index.d.ts")
    );
}

#[test]
fn results_are_cached_per_directory_and_mode() {
    let mut fs = MemoryFs::new();
    fs.add_file("/p/tsconfig.json", "{}")
        .add_file("/p/a.ts", "")
        .add_file("/p/b.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    let one = r
        .resolve_module("./b", "/p/a.ts", ResolutionMode::Import)
        .unwrap();
    let two = r
        .resolve_module("./b", "/p/other.ts", ResolutionMode::Import)
        .unwrap();
    assert!(Arc::ptr_eq(&one, &two));
    let three = r
        .resolve_module("./b", "/p/a.ts", ResolutionMode::Require)
        .unwrap();
    assert!(!Arc::ptr_eq(&one, &three));
    r.clear_caches();
    let four = r
        .resolve_module("./b", "/p/a.ts", ResolutionMode::Import)
        .unwrap();
    assert!(!Arc::ptr_eq(&one, &four));
}

/// A traced resolution gives the untraced result and logs each step in tsc's wording; one
/// `Trace` remembers earlier misses.
#[test]
fn traced_resolution_logs_steps() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "esnext" } }"#,
    )
    .add_file("/p/a.ts", "")
    .add_file("/p/b.tsx", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    let mut trace = Trace::new();
    let traced = r
        .resolve_module_traced("./b", "/p/a.ts", ResolutionMode::Import, &mut trace)
        .unwrap();
    let plain = r
        .resolve_module("./b", "/p/a.ts", ResolutionMode::Import)
        .unwrap();
    assert_eq!(traced.resolved_file_name, plain.resolved_file_name);
    assert_eq!(
        trace.take_lines(),
        [
            "======== Resolving module './b' from '/p/a.ts'. ========",
            "Module resolution kind is not specified, using 'Bundler'.",
            // tsc names the mode CJS under bundler, whatever the import kind.
            "Resolving in CJS mode with conditions 'import', 'types'.",
            "Loading module as file / folder, candidate module location '/p/b', target file types: TypeScript, JavaScript, Declaration, JSON.",
            "File '/p/b.ts' does not exist.",
            "File '/p/b.tsx' exists - use it as a name resolution result.",
            "======== Module name './b' was successfully resolved to '/p/b.tsx'. ========",
        ]
    );
    r.resolve_module_traced("./b", "/p/a.ts", ResolutionMode::Import, &mut trace);
    assert!(
        trace
            .lines()
            .iter()
            .any(|l| l == "File '/p/b.ts' does not exist according to earlier cached lookups."),
        "{:?}",
        trace.lines()
    );
}

#[test]
fn resolver_is_shareable_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Resolver<MemoryFs>>();
    assert_send_sync::<Resolver<ts_r_config::RealFs>>();
}

#[test]
fn type_references_use_type_roots_first() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "types": ["node"] } }"#,
    )
    .add_file("/p/src/a.ts", "")
    .add_file("/p/node_modules/@types/node/index.d.ts", "")
    .add_file("/p/node_modules/@types/s__t/index.d.ts", "")
    .add_file(
        "/p/node_modules/plain/package.json",
        r#"{ "name": "plain", "version": "1.0.0", "types": "t.d.ts" }"#,
    )
    .add_file("/p/node_modules/plain/t.d.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    let node = r
        .resolve_type_reference("node", None, ResolutionMode::Require)
        .unwrap();
    assert!(node.primary);
    assert_eq!(
        node.resolved_file_name,
        "/p/node_modules/@types/node/index.d.ts"
    );
    let scoped = r
        .resolve_type_reference("@s/t", Some("/p/src/a.ts"), ResolutionMode::Require)
        .unwrap();
    assert_eq!(
        scoped.resolved_file_name,
        "/p/node_modules/@types/s__t/index.d.ts"
    );
    let plain = r
        .resolve_type_reference("plain", Some("/p/src/a.ts"), ResolutionMode::Require)
        .unwrap();
    assert!(!plain.primary);
    assert_eq!(
        plain.package_id.as_ref().unwrap().to_string(),
        "plain/t.d.ts@1.0.0"
    );
    assert!(
        r.resolve_type_reference("missing", None, ResolutionMode::Require)
            .is_none()
    );
}

#[test]
fn root_dirs_with_unicode_paths_outside_them() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "rootDirs": ["x"] }, "files": ["main.ts"] }"#,
    )
    .add_file("/p/main.ts", "")
    .add_file("/p/é.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "./é.ts", "/p/main.ts", ResolutionMode::Import).as_deref(),
        Some("/p/é.ts")
    );
}

#[test]
fn imports_cycles_fail_and_chains_resolve() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext" }, "files": ["main.ts"] }"#,
    )
    .add_file(
        "/p/package.json",
        r##"{ "type": "module", "imports": { "#a": "#b", "#b": "#a", "#c": "#d", "#d": "./d.js" } }"##,
    )
    .add_file("/p/main.ts", "")
    .add_file("/p/d.ts", "");
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "#a", "/p/main.ts", ResolutionMode::Import),
        None
    );
    assert_eq!(
        resolved(&r, "#c", "/p/main.ts", ResolutionMode::Import).as_deref(),
        Some("/p/d.ts")
    );
}

#[test]
fn imports_depth_limit_is_independent_of_resolution_order() {
    let mut imports = String::new();
    for i in 0..33 {
        imports.push_str(&format!("\"#{i}\":\"#{}\",", i + 1));
    }
    imports.push_str("\"#33\":\"./ok.ts\"");
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext" } }"#,
    )
    .add_file(
        "/p/package.json",
        &format!(r#"{{ "type": "module", "imports": {{ {imports} }} }}"#),
    )
    .add_file("/p/ok.ts", "");
    for mode in [ResolutionMode::Import, ResolutionMode::Require] {
        for order in [["#0", "#32"], ["#32", "#0"]] {
            let r = resolver(&fs, "/p/tsconfig.json");
            for spec in order.into_iter().cycle().take(4) {
                let expected = if spec == "#0" { None } else { Some("/p/ok.ts") };
                assert_eq!(
                    resolved(&r, spec, "/p/main.ts", mode).as_deref(),
                    expected,
                    "{mode:?} {order:?}: {spec}"
                );
            }
        }
    }
}

#[test]
fn bare_names_fall_back_to_custom_type_roots() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "commonjs", "typeRoots": ["./typings"] } }"#,
    )
    .add_file(
        "/p/typings/phaser/package.json",
        r#"{ "name": "phaser", "version": "1.2.3", "types": "types/phaser.d.ts" }"#,
    )
    .add_file("/p/typings/phaser/types/phaser.d.ts", "export {};")
    .add_file("/p/typings/@sc/pkg/index.d.ts", "export {};")
    .add_file("/p/typings/other.d.ts", "export {};");
    let r = resolver(&fs, "/p/tsconfig.json");
    let m = r
        .resolve_module("phaser", "/p/a.ts", ResolutionMode::Require)
        .unwrap();
    assert_eq!(m.resolved_file_name, "/p/typings/phaser/types/phaser.d.ts");
    assert!(m.package_id.is_none() && !m.is_external_library_import);
    assert_eq!(
        resolved(&r, "@sc/pkg", "/p/a.ts", ResolutionMode::Require).as_deref(),
        Some("/p/typings/@sc/pkg/index.d.ts")
    );
    assert_eq!(
        resolved(&r, "other", "/p/a.ts", ResolutionMode::Require).as_deref(),
        Some("/p/typings/other.d.ts")
    );
    assert_eq!(
        resolved(&r, "./other", "/p/a.ts", ResolutionMode::Require),
        None
    );
}

#[test]
fn hash_slash_imports_are_invalid_under_node16_only() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/package.json",
        r##"{ "type": "module", "imports": { "#/*": "./src/*" } }"##,
    )
    .add_file("/p/src/foo.ts", "export {};")
    .add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "node16" } }"#,
    )
    .add_file(
        "/p/next/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext" } }"#,
    );
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "#/foo.js", "/p/a.ts", ResolutionMode::Import),
        None
    );
    let r = resolver(&fs, "/p/next/tsconfig.json");
    assert_eq!(
        resolved(&r, "#/foo.js", "/p/a.ts", ResolutionMode::Import).as_deref(),
        Some("/p/src/foo.ts")
    );
}

#[test]
fn main_directories_ignore_their_own_package_json() {
    let mut fs = MemoryFs::new();
    fs.add_file("/p/tsconfig.json", r#"{ "compilerOptions": {} }"#)
        .add_file("/p/node_modules/foo/package.json", r#"{ "main": "oof" }"#)
        .add_file(
            "/p/node_modules/foo/oof/package.json",
            r#"{ "main": "ofo", "types": "ofo.d.ts" }"#,
        )
        .add_file("/p/node_modules/foo/oof/ofo.d.ts", "export {};");
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "foo", "/p/a.ts", ResolutionMode::Require),
        None
    );
}

#[test]
fn at_types_fields_load_typescript_files() {
    let mut fs = MemoryFs::new();
    fs.add_file("/p/tsconfig.json", r#"{ "compilerOptions": {} }"#)
        .add_file(
            "/p/node_modules/@types/lquery/package.json",
            r#"{ "typings": "lquery" }"#,
        )
        .add_file("/p/node_modules/@types/lquery/lquery.ts", "export {};")
        .add_file("/p/node_modules/@types/zed/index.ts", "export {};");
    let r = resolver(&fs, "/p/tsconfig.json");
    assert_eq!(
        resolved(&r, "lquery", "/p/a.ts", ResolutionMode::Require).as_deref(),
        Some("/p/node_modules/@types/lquery/lquery.ts")
    );
    // The package's own `index` stays declaration-only.
    assert_eq!(
        resolved(&r, "zed", "/p/a.ts", ResolutionMode::Require),
        None
    );
}

#[test]
fn type_ref_misses_share_the_module_miss_cache() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "typeRoots": ["./typings"] } }"#,
    )
    .add_file("/p/typings/phaser/index.d.ts", "export {};");
    let r = resolver(&fs, "/p/tsconfig.json");
    let mut t = Trace::new();
    r.resolve_module_traced("phaser", "/p/a.ts", ResolutionMode::Require, &mut t);
    r.resolve_type_reference_traced("phaser", None, ResolutionMode::Import, &mut t);
    let lines = t.take_lines();
    let cached =
        "File '/p/typings/phaser.d.ts' does not exist according to earlier cached lookups.";
    assert!(lines.iter().any(|l| l == cached), "{lines:#?}");
}

/// TS2209/TS2210: with `outDir` set and neither `rootDir` nor a tsconfig, the program's own
/// package cannot resolve its `exports` self-name or `imports` to relative targets; the
/// entries are recorded and the modules stay unresolved. A tsconfig fixes the root.
#[test]
fn ambiguous_project_root_blocks_own_exports_and_imports() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext", "outDir": "out" }, "files": ["main.ts"] }"#,
    )
    .add_file(
        "/p/package.json",
        r##"{ "name": "self", "type": "module", "exports": { ".": "./main.js", "./sub/*": "./sub/*.js" }, "imports": { "#x": "./x.js" } }"##,
    )
    .add_file("/p/main.ts", "")
    .add_file("/p/x.ts", "")
    .add_file("/p/sub/y.ts", "");
    let mut cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    let rooted = Resolver::new(&fs, ResolverOptions::from_compiler_options(&cfg.options));
    assert_eq!(
        resolved(&rooted, "self", "/p/main.ts", ResolutionMode::Import).as_deref(),
        Some("/p/main.ts")
    );
    assert!(rooted.ambiguous_roots().is_empty());

    cfg.options.config_file_path = None;
    let r = Resolver::new(&fs, ResolverOptions::from_compiler_options(&cfg.options));
    for mode in [ResolutionMode::Import, ResolutionMode::Require] {
        for spec in ["self", "self/sub/y", "#x"] {
            assert_eq!(resolved(&r, spec, "/p/main.ts", mode), None);
        }
    }
    let roots: Vec<_> = r
        .ambiguous_roots()
        .into_iter()
        .map(|a| (a.imports, a.entry, a.package_json))
        .collect();
    assert_eq!(
        roots,
        vec![
            (false, ".".to_owned(), "/p/package.json".to_owned()),
            (false, "y".to_owned(), "/p/package.json".to_owned()),
            (true, ".".to_owned(), "/p/package.json".to_owned()),
        ]
    );
}

#[test]
fn ambiguous_project_root_keeps_distinct_imports_in_order() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/p/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext", "outDir": "out" } }"#,
    )
    .add_file(
        "/p/package.json",
        r##"{ "imports": { "#*": "./src/*.ts" } }"##,
    );
    let mut cfg = load_tsconfig(&fs, "/p/tsconfig.json");
    cfg.options.config_file_path = None;
    let r = Resolver::new(&fs, ResolverOptions::from_compiler_options(&cfg.options));
    for i in 0..4096 {
        assert_eq!(
            resolved(
                &r,
                &format!("#entry{i}"),
                "/p/main.ts",
                ResolutionMode::Import
            ),
            None
        );
    }
    for i in (0..4096).rev() {
        assert_eq!(
            resolved(
                &r,
                &format!("#entry{i}"),
                "/p/main.ts",
                ResolutionMode::Require
            ),
            None
        );
    }
    let roots = r.ambiguous_roots();
    assert_eq!(roots.len(), 4096);
    for (i, root) in roots.iter().enumerate() {
        assert!(root.imports);
        assert_eq!(root.entry, format!("entry{i}"));
        assert_eq!(root.package_json, "/p/package.json");
    }
}

/// A type reference to a package whose `exports` has no types for `.` is not resolved
/// through its `types` field.
#[test]
fn type_reference_honours_exports_without_types() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/tsconfig.json",
        r#"{ "compilerOptions": { "module": "commonjs" } }"#,
    )
    .add_file("/usage.ts", "")
    .add_file("/node_modules/pkg/index.d.ts", "interface G {}")
    .add_file(
        "/node_modules/pkg/package.json",
        r#"{ "name": "pkg", "types": "index.d.ts", "exports": "some-other-thing.js" }"#,
    );
    let r = resolver(&fs, "/tsconfig.json");
    let mut t = Trace::new();
    let res =
        r.resolve_type_reference_traced("pkg", Some("/usage.ts"), ResolutionMode::Require, &mut t);
    let lines = t.take_lines();
    assert!(res.is_none(), "{lines:#?}");
}

/// An `import` whose `exports` lead to JavaScript only records the declarations a lookup
/// ignoring `exports` finds, as a hint; the resolution stays the JavaScript.
#[test]
fn alternate_result_names_declarations_hidden_by_exports() {
    let mut fs = MemoryFs::new();
    fs.add_file(
        "/tsconfig.json",
        r#"{ "compilerOptions": { "module": "nodenext" } }"#,
    )
    .add_file("/index.mts", "")
    .add_file("/node_modules/foo/index.mjs", "")
    .add_file("/node_modules/foo/index.d.ts", "")
    .add_file(
        "/node_modules/foo/package.json",
        r#"{ "name": "foo", "version": "1.0.0", "exports": { ".": { "import": "./index.mjs" } } }"#,
    );
    let r = resolver(&fs, "/tsconfig.json");
    let res = r
        .resolve_module("foo", "/index.mts", ResolutionMode::Import)
        .unwrap();
    assert_eq!(res.resolved_file_name, "/node_modules/foo/index.mjs");
    assert_eq!(
        res.alternate_result.as_deref(),
        Some("/node_modules/foo/index.d.ts")
    );
    let cjs = r.resolve_module("foo", "/index.mts", ResolutionMode::Require);
    assert!(cjs.is_none_or(|m| m.alternate_result.is_none()));
}
