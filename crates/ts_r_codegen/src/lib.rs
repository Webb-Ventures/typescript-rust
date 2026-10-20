//! AST → JavaScript (or TypeScript) printer with V3 source maps.
//!
//! The printer prints whatever is in the [`Ast`]: run `ts_r_transform` first to get
//! JavaScript out of TypeScript. It never does text surgery on the source; source text is used
//! only for the raw form of literals (numbers, strings, templates, regexes, JSX text) of nodes
//! that came from the parser, so they round-trip byte for byte.
//!
//! Synthesized nodes (made by a transform) are recognised by an **empty span** (`start ==
//! end`): their literals are printed from their cooked values, and their span start is used
//! only as the source-map position (`0..0` means "unmapped").
//!
//! Output is ASI-safe: every statement ends with `;` (or `}`), and parentheses are added from
//! operator precedence and statement-start ambiguities (`{`, `function`, `class`, `let [`), so
//! a transformed tree always prints as the same tree.
//!
//! See `docs/design/0002-transform-and-codegen.md`.

mod expr;
pub mod literals;
mod printer;
pub mod shape;
pub mod sourcemap;
mod stmt;
mod types;

use ts_r_ast::{Ast, NodeId, Span};

pub use printer::Printer;
pub use sourcemap::{LineLookup, Mapping, SourceMap, SourceMapBuilder};

/// Output layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layout {
    /// One statement per line, 4-space indentation, comments kept (tsc-like).
    #[default]
    Readable,
    /// No optional whitespace or newlines; only legal/`#__PURE__` comments are kept.
    Compact,
    /// tsc 7's emit layout (design record 0012): like `Readable`, but line breaks inside
    /// blocks and lists follow the source (`{ return x; }` stays on one line), blank lines are
    /// dropped and comments attach as tsc attaches them. The target is byte equality with
    /// tsc's `.js` output.
    Tsc,
}

/// Source-map settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMapOptions {
    /// `file`: the generated file's name.
    pub file: Option<String>,
    /// The single entry in `sources` (usually the source path relative to the map).
    pub source: String,
    pub source_root: Option<String>,
    /// Embed the source text as `sourcesContent`.
    pub include_content: bool,
}

/// Printer options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodegenOptions {
    pub layout: Layout,
    /// Keep comments (statement-level comments in readable mode; legal comments and
    /// `#__PURE__`-style annotations in both layouts). `false` drops all but legal comments.
    pub comments: bool,
    /// Produce a source map.
    pub source_map: Option<SourceMapOptions>,
    /// Spaces per indentation level in readable layout.
    pub indent_width: u8,
    /// Line terminator (`"\n"` or `"\r\n"`).
    pub newline: &'static str,
    /// Record the output spans of module specifiers (`import`/`export ... from`, `import()`).
    pub collect_specifiers: bool,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        CodegenOptions {
            layout: Layout::Readable,
            comments: true,
            source_map: None,
            indent_width: 4,
            newline: "\n",
            collect_specifiers: false,
        }
    }
}

/// Extra input from a transform.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrintHints<'a> {
    /// Source ranges whose code was removed (types, elided imports, ...). Sorted by start,
    /// non-overlapping. Comments inside them, and comments attached in front of them, are
    /// dropped.
    pub erased: &'a [Span],
    /// Raw JS printed right after the directive prologue (runtime helpers). Must end with a
    /// newline if non-empty.
    pub prelude: &'a str,
    /// [`Layout::Tsc`]: synthesized blocks (and embedded statements) a transform wants on one
    /// line, as tsc prints its single-line factory nodes (`get x() { return this.#x; }`,
    /// `if (_metadata) f();`). Sorted.
    pub single_line: &'a [NodeId],
    /// [`Layout::Tsc`]: synthesized array and object literals printed one element per line
    /// (`__decorate([\n    dec\n], C)`), comma sequences broken after every comma
    /// (`(_a = class {\n    },\n    _a.x = 1,\n    _a)`), and blocks printed over several
    /// lines though the source has them on one. Sorted.
    pub multi_line: &'a [NodeId],
    /// [`Layout::Tsc`] source maps: the source range tsc maps a synthesized node to (its
    /// start and end segments, as for an original node), sorted by node id. A synthesized
    /// block's range is the original block whose `}` it maps. A start or end of
    /// [`NO_MAP`] maps nothing on that side; a statement whose range is `NO_MAP` on both
    /// sides stands for no original statement (the comments in front of the code at 0 are
    /// not its own).
    pub map_ranges: &'a [(NodeId, Span)],
    /// [`Layout::Tsc`]: synthesized statements that stand for a source node moved elsewhere
    /// (a field initializer moved into the constructor, `this.x = v;`), with that node's
    /// range, sorted by node id. The statement prints the node's comments: those leading it
    /// (after the previous token's line), those inside it and those trailing it on its last
    /// line. They are not printed where the node was.
    pub moved_comments: &'a [(NodeId, Span)],
    /// Print a numeric literal written with `_` separators by its value (`10_000` →
    /// `10000`), as tsc does without a `target` or below ES2021.
    pub strip_numeric_separators: bool,
}

/// How a module specifier is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecifierKind {
    /// `import ... from "x"` / `import "x"`.
    Import,
    /// `export ... from "x"`.
    Export,
    /// `import("x")`.
    Dynamic,
}

/// A module specifier in the output: `start..end` is the text between the quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecifierSpan {
    pub kind: SpecifierKind,
    pub start: u32,
    pub end: u32,
}

/// Printer output.
#[derive(Clone, Debug, Default)]
pub struct CodegenOutput {
    pub code: String,
    pub map: Option<SourceMap>,
    /// Filled when [`CodegenOptions::collect_specifiers`] is set.
    pub specifiers: Vec<SpecifierSpan>,
}

/// What the printed text is meant to be. It only decides which modifiers are printed: the
/// tree is printed as it is otherwise (run `ts_r_transform` to remove types).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Syntax {
    /// Every modifier in the tree, misplaced ones included (`{ public a: 1 }`,
    /// `async class C {}`), so `parse(print(parse(x)))` keeps them.
    #[default]
    TypeScript,
    /// tsc 7's JavaScript emit: TypeScript-only modifiers (`public`, `private`, `protected`,
    /// `readonly`, `abstract`, `declare`, `override`, `in`, `out`, `const` on a member) are
    /// never printed, object literal properties print no modifiers and parameters no `export`.
    /// Misplaced JavaScript modifiers stay (`static const x`, `async class C`), as in tsc.
    JavaScript,
}

/// Prints the whole program as TypeScript (every modifier in the tree; see [`Syntax`]).
pub fn print(ast: &Ast, src: &str, opts: &CodegenOptions) -> CodegenOutput {
    print_as(ast, src, opts, &PrintHints::default(), Syntax::TypeScript)
}

/// Prints the whole program with transform hints, as JavaScript ([`Syntax::JavaScript`]):
/// the entry point after `ts_r_transform`.
pub fn print_with(
    ast: &Ast,
    src: &str,
    opts: &CodegenOptions,
    hints: &PrintHints<'_>,
) -> CodegenOutput {
    print_as(ast, src, opts, hints, Syntax::JavaScript)
}

/// Prints the whole program with transform hints as `syntax`.
pub fn print_as(
    ast: &Ast,
    src: &str,
    opts: &CodegenOptions,
    hints: &PrintHints<'_>,
    syntax: Syntax,
) -> CodegenOutput {
    with_stack_for(ast, || {
        let mut p = Printer::new(ast, src, opts, hints).with_syntax(syntax);
        p.program(ast.root);
        p.finish()
    })
}

/// Depth of the deepest node under the root (explicit stack; never overflows).
pub fn max_depth(ast: &Ast) -> usize {
    let mut max = 0;
    let mut stack = vec![(ast.root, 1usize)];
    while let Some((id, d)) = stack.pop() {
        max = max.max(d);
        ast.for_each_child(id, |c| stack.push((c, d + 1)));
    }
    max
}

/// Trees deeper than this are printed (and transformed) on a thread with a large stack.
pub const DEEP_TREE: usize = 150;

/// In a [`PrintHints::map_ranges`] range: no segment on that side (tsc maps only the end of
/// some nodes it rebuilds, only the start of others).
pub const NO_MAP: u32 = u32::MAX;

/// Runs `f` on the current thread for shallow trees, or on a scoped thread whose stack is
/// sized for the tree's depth. The printer and transforms recurse once per tree level.
pub fn with_stack_for<R: Send, F: FnOnce() -> R + Send>(ast: &Ast, f: F) -> R {
    if ast.node_count() < DEEP_TREE {
        return f();
    }
    let depth = max_depth(ast);
    if depth < DEEP_TREE {
        return f();
    }
    // Generous per-level budget (debug builds have large frames).
    let stack = (depth * 64 * 1024).clamp(16 << 20, 4 << 30);
    std::thread::scope(|s| {
        match std::thread::Builder::new()
            .stack_size(stack)
            .spawn_scoped(s, f)
        {
            Ok(h) => match h.join() {
                Ok(r) => r,
                Err(e) => std::panic::resume_unwind(e),
            },
            Err(_) => panic!("ts_r_codegen: cannot spawn a thread for a deep tree"),
        }
    })
}

/// Prints one expression, statement or type subtree (for diagnostics and tests). No comments,
/// no source map.
pub fn print_node(ast: &Ast, src: &str, id: ts_r_ast::NodeId) -> String {
    let opts = CodegenOptions {
        comments: false,
        ..CodegenOptions::default()
    };
    let hints = PrintHints::default();
    let mut p = Printer::new(ast, src, &opts, &hints);
    p.any(id);
    p.finish().code
}
