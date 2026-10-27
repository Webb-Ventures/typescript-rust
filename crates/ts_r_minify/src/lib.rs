//! JavaScript minifier for ts-r.
//!
//! The pieces are usable on their own, because the bundler drives them per module:
//!
//! - [`scope`]: scopes, symbols and resolved references for a JavaScript tree (after
//!   `ts_r_transform`), including the "through" sets the renamers need.
//! - [`purity`]: side-effect analysis of expressions and statements (`/*#__PURE__*/` aware).
//! - [`fold`]: constant folding of tests and dead-branch removal (`if (false)`, `cond ? a : b`,
//!   `true && x`). The bundler always runs it so `process.env.NODE_ENV` branches disappear
//!   before imports are scanned.
//! - [`compress`]: safe size-oriented rewrites (`if` to `&&`/`?:`, statement merging,
//!   `undefined` to `void 0`, `true` to `!0`, property shortening, unreachable code).
//! - [`rename`]: scope-aware renaming, either short names ([`rename::RenameMode::Mangle`])
//!   or the smallest change that removes captures ([`rename::RenameMode::Deconflict`]).
//! - [`build`]: helpers to append synthesized nodes to an [`Ast`].
//!
//! Whitespace removal is `ts_r_codegen`'s compact layout. [`minify`] runs everything on one
//! file. Design: `docs/design/0008-bundler.md`.

pub mod build;
pub mod compress;
pub mod fold;
pub mod keep_names;
pub mod names;
pub mod private;
pub mod props;
pub mod purity;
pub mod rename;
pub mod scope;

use ts_r_ast::{Ast, SourceKind};
use ts_r_codegen::{CodegenOptions, Layout, SourceMapOptions};

pub use scope::{ScopeId, ScopeKind, Scopes, SymbolId, SymbolKind};

/// What [`minify`] does. Mirrors esbuild's `--minify-whitespace`, `--minify-identifiers` and
/// `--minify-syntax`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MinifyOptions {
    pub whitespace: bool,
    pub identifiers: bool,
    pub syntax: bool,
    /// The input is an ES module: top-level bindings are local and may be renamed.
    pub module: bool,
}

impl MinifyOptions {
    /// Everything on.
    pub fn all(module: bool) -> MinifyOptions {
        MinifyOptions {
            whitespace: true,
            identifiers: true,
            syntax: true,
            module,
        }
    }
}

/// Output of [`minify`].
#[derive(Debug, Default)]
pub struct MinifyOutput {
    pub code: String,
    /// Source map JSON when requested.
    pub map: Option<String>,
    /// Parse errors (the output is still produced from the recovered tree).
    pub errors: Vec<String>,
}

/// Minifies one JavaScript file. `path` picks the grammar (`.js`, `.mjs`, `.cjs`, `.jsx`).
/// `source_map` names the source in the map when set.
pub fn minify(
    path: &str,
    src: &str,
    opts: &MinifyOptions,
    source_map: Option<&str>,
) -> MinifyOutput {
    let kind = match SourceKind::from_path(path) {
        SourceKind::Ts | SourceKind::Dts => SourceKind::Js,
        SourceKind::Tsx => SourceKind::Jsx,
        k => k,
    };
    let parsed = ts_r_parser::parse(src, kind);
    let errors = parsed
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.clone())
        .collect();
    let mut ast = parsed.ast;
    let strict = opts.module || path.ends_with(".mjs");
    let deep = ts_r_codegen::max_depth(&ast) >= ts_r_codegen::DEEP_TREE;
    if deep {
        let stack = (ts_r_codegen::max_depth(&ast) * 64 * 1024).clamp(16 << 20, 4 << 30);
        std::thread::scope(|s| {
            let h = std::thread::Builder::new()
                .stack_size(stack)
                .spawn_scoped(s, || minify_ast(&mut ast, src, opts, strict));
            if let Ok(h) = h {
                let _ = h.join();
            }
        });
    } else {
        minify_ast(&mut ast, src, opts, strict);
    }
    let codegen = CodegenOptions {
        layout: if opts.whitespace {
            Layout::Compact
        } else {
            Layout::Readable
        },
        comments: !opts.whitespace,
        source_map: source_map.map(|s| SourceMapOptions {
            source: s.to_owned(),
            include_content: true,
            ..SourceMapOptions::default()
        }),
        ..CodegenOptions::default()
    };
    let out = ts_r_codegen::print(&ast, src, &codegen);
    MinifyOutput {
        code: out.code,
        map: out.map.map(|m| m.to_json()),
        errors,
    }
}

/// Runs folding, compression and mangling on a parsed tree in place. The tree must be
/// shallow enough for the current thread's stack (see `ts_r_codegen::with_stack_for`).
pub fn minify_ast(ast: &mut Ast, src: &str, opts: &MinifyOptions, strict: bool) {
    minify_ast_keep_names(ast, src, opts, strict, None);
}

/// [`minify_ast`] with `--keep-names`: when `keep_names` names the helper, renamed
/// functions and classes get `helper(f, "f")` calls ([`keep_names`]). Returns whether any
/// call was added (the caller defines the helper).
pub fn minify_ast_keep_names(
    ast: &mut Ast,
    src: &str,
    opts: &MinifyOptions,
    strict: bool,
    keep_names: Option<&str>,
) -> bool {
    let mut used = false;
    let mut scopes = Scopes::analyze(ast, strict);
    if fold::fold(ast, src, &scopes) {
        scopes = Scopes::analyze(ast, strict);
    }
    if opts.syntax {
        let names = keep_names.is_none();
        let opts = compress::CompressOptions {
            names,
            ..Default::default()
        };
        compress::compress(ast, src, &scopes, &opts);
    }
    if opts.identifiers {
        let scopes = Scopes::analyze(ast, strict);
        let fixed = rustc_hash::FxHashMap::default();
        let renames = rename::compute(
            ast,
            &scopes,
            &rename::RenameOptions {
                mode: rename::RenameMode::Mangle,
                fixed: &fixed,
                include_module_scope: opts.module,
                reserved: &|n| keep_names == Some(n),
                module_reserved: &|_| false,
            },
        );
        rename::apply(ast, &scopes, &renames);
        if let Some(helper) = keep_names {
            used = keep_names::apply(
                ast,
                &scopes,
                helper,
                keep_names::KeepNamesOptions {
                    program: true,
                    top_classes: false,
                },
            )
            .used;
        }
        private::mangle(ast, scopes.has_direct_eval);
    }
    used
}
