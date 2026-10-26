//! TypeScript/TSX → JavaScript transforms for ts-r.
//!
//! [`transform`] rewrites a parsed [`Ast`] in place so that `ts_r_codegen` prints JavaScript
//! matching tsc 7's emit (compared by AST shape, not text): type erasure, parameter
//! properties, class fields (`useDefineForClassFields`), `accessor` fields, enums and const
//! enums, namespaces, legacy and TC39 decorators, JSX (classic and automatic runtimes),
//! import elision (`verbatimModuleSyntax` aware), `rewriteRelativeImportExtensions`, and the
//! module format (ESM, CommonJS, `preserve`).
//!
//! [`transpile`] is the one-call API (parse, transform, print) used by `ts-r-run` and the dev
//! server. Design: `docs/design/0002-transform-and-codegen.md`.
//!
//! [`downlevel`] lowers syntax newer than the target (ES2021 down to ES2015, record 0012 E5).

mod build_errors;
mod builder;
mod cjs;
mod class;
mod decorators;
pub mod downlevel;
mod entities;
mod enums;
mod es_decorators;
mod helpers;
mod jsx;
mod namespace;
mod options;
pub mod resolver;
mod scope;
mod static_privates;
mod transform;

#[cfg(feature = "hmr")]
pub mod hmr;

use ts_r_ast::{Ast, NodeId, NodeKind, SourceKind, Span};
use ts_r_codegen::{CodegenOptions, PrintHints, SourceMap, SpecifierSpan};
use ts_r_diagnostics::Diagnostic;

pub use build_errors::{
    SloppyOnly, build_errors, has_module_syntax, parse_build_errors, sloppy_only_syntax,
};
pub use options::{JsxMode, ModuleFormat, TransformOptions};
pub use resolver::{Answer, ConstValue, EmitResolver, MetadataType, SyntacticResolver};

/// What [`transform`] produced besides the rewritten tree.
#[derive(Debug, Default)]
pub struct TransformResult {
    /// Source ranges of erased code, sorted and disjoint (for comment dropping in codegen).
    pub erased: Vec<Span>,
    /// Runtime helpers to print after the directive prologue (empty if none).
    pub prelude: String,
    /// Problems with the input that tsc would report (e.g. `import x = require()` in an ES
    /// module). The output is still produced.
    pub diagnostics: Vec<Diagnostic>,
    /// Constructs the [`EmitResolver`] answered [`Answer::Unknown`] for: where, and what was
    /// asked. A program emitter must not write the file (TSR2002).
    pub unknown: Vec<(Span, &'static str)>,
    /// Syntax newer than the target that ts-r cannot lower yet (record 0012, E5), with what
    /// it is. A program emitter must not write the file (TSR2003).
    pub unlowered: Vec<(Span, &'static str)>,
    /// Synthesized nodes tsc prints on one line ([`PrintHints::single_line`]). Sorted.
    pub single_line: Vec<NodeId>,
    /// Synthesized array literals tsc prints one element per line, and blocks it prints
    /// over several lines ([`PrintHints::multi_line`]). Sorted.
    pub multi_line: Vec<NodeId>,
    /// Synthesized strings tsc prints with `'` ([`PrintHints::single_quoted`]). Sorted.
    pub single_quoted: Vec<NodeId>,
    /// Source-map ranges of synthesized nodes ([`PrintHints::map_ranges`]). Sorted by node.
    pub map_ranges: Vec<(NodeId, Span)>,
    /// Statements standing for moved source nodes ([`PrintHints::moved_comments`]). Sorted
    /// by node.
    pub moved_comments: Vec<(NodeId, Span)>,
}

/// Rewrites `ast` (parsed from `src`) into a JavaScript tree.
pub fn transform(ast: &mut Ast, src: &str, opts: &TransformOptions) -> TransformResult {
    transform_with(
        ast,
        src,
        opts,
        &SyntacticResolver,
        ts_r_diagnostics::FileId(0),
    )
}

/// [`transform`] with the cross-file facts of `resolver` for `file` (a program emitter's
/// checker; design record 0012).
pub fn transform_with(
    ast: &mut Ast,
    src: &str,
    opts: &TransformOptions,
    resolver: &dyn EmitResolver,
    file: ts_r_diagnostics::FileId,
) -> TransformResult {
    // The visitors recurse once per tree level; deep trees get a big stack.
    let shallow = ast.node_count() < ts_r_codegen::DEEP_TREE
        || ts_r_codegen::max_depth(ast) < ts_r_codegen::DEEP_TREE;
    if shallow {
        transform::run(ast, src, opts, resolver, file)
    } else {
        let depth = ts_r_codegen::max_depth(ast);
        let stack = (depth * 64 * 1024).clamp(16 << 20, 4 << 30);
        std::thread::scope(|s| {
            let h = std::thread::Builder::new()
                .stack_size(stack)
                .spawn_scoped(s, || transform::run(ast, src, opts, resolver, file));
            match h {
                Ok(h) => match h.join() {
                    Ok(r) => r,
                    Err(e) => std::panic::resume_unwind(e),
                },
                Err(_) => panic!("ts_r_transform: cannot spawn a thread for a deep tree"),
            }
        })
    }
}

/// Output of [`transpile`].
#[derive(Debug, Default)]
pub struct TranspileOutput {
    pub code: String,
    pub map: Option<SourceMap>,
    /// Syntax errors (from the parser) followed by transform diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// The errors in `diagnostics` a build tool fails on (the dev server, as esbuild): the
    /// parser's per [`build_errors`], then the transform's.
    pub build_errors: Vec<Diagnostic>,
    /// Module specifiers in `code` (when `CodegenOptions::collect_specifiers` is set).
    pub specifiers: Vec<SpecifierSpan>,
}

impl TranspileOutput {
    /// True if the parser reported syntax errors (the output may be wrong).
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// Parses `src` (grammar from `path`), transforms it and prints it.
pub fn transpile(
    path: &str,
    src: &str,
    opts: &TransformOptions,
    codegen: &CodegenOptions,
) -> TranspileOutput {
    let kind = SourceKind::from_path(path);
    let mut parsed = ts_r_parser::parse(src, kind);
    let mut build_errors: Vec<Diagnostic> = build_errors(&parsed).into_iter().cloned().collect();
    let mut diagnostics = std::mem::take(&mut parsed.diagnostics);
    let mut ast = parsed.ast;
    if kind == SourceKind::Dts {
        // Declaration files have no JavaScript.
        return TranspileOutput {
            diagnostics,
            build_errors,
            ..TranspileOutput::default()
        };
    }
    let result = transform(&mut ast, src, opts);
    build_errors.extend(result.diagnostics.iter().filter(|d| d.is_error()).cloned());
    diagnostics.extend(result.diagnostics);
    let hints = PrintHints {
        erased: &result.erased,
        prelude: &result.prelude,
        single_line: &result.single_line,
        multi_line: &result.multi_line,
        single_quoted: &result.single_quoted,
        map_ranges: &result.map_ranges,
        moved_comments: &result.moved_comments,
        strip_numeric_separators: opts.strip_numeric_separators,
    };
    let out = ts_r_codegen::print_with(&ast, src, codegen, &hints);
    TranspileOutput {
        code: out.code,
        map: out.map,
        diagnostics,
        build_errors,
        specifiers: out.specifiers,
    }
}

/// Type-only node kinds (never walked by value transforms).
pub fn is_type_kind(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        KeywordType
            | TypeRef
            | QualifiedName
            | ThisType
            | TemplateLitType
            | ArrayType
            | TupleType
            | NamedTupleMember
            | OptionalType
            | RestType
            | UnionType
            | IntersectionType
            | FunctionType
            | ConstructorType
            | TypeLiteral
            | PropertySignature
            | MethodSignature
            | CallSignature
            | ConstructSignature
            | TypeQuery
            | TypeOperator
            | IndexedAccessType
            | ConditionalType
            | InferType
            | MappedType
            | ParenType
            | TypePredicate
            | ImportType
            | TypeParams
            | TypeParam
            | TypeArgs
            | IndexSignature
    )
}
