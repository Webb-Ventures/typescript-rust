//! Hand-written, error-recovering parser for TS, TSX and JS.
//!
//! Recursive descent for statements, declarations and types; precedence climbing for binary
//! expressions. The only backtracking is bounded speculation (arrow functions, generic call
//! arguments, function types), done by checkpointing the lexer and truncating the AST arrays.
//!
//! Entry point: [`parse`]. The parser never panics on any input; it always returns a tree
//! (using `Missing` nodes where something was expected) plus diagnostics.

mod class;
mod expr;
mod grammar;
mod js_ts;
mod jsdoc;
mod jsx;
mod lists;
mod modifiers;
mod semicolon;
mod stmt;
mod types;

use ts_r_ast::{
    Ast, Checkpoint as AstCheckpoint, Node, NodeId, NodeList, SourceKind, Span, UnaryOp,
};
use ts_r_diagnostics::{Diagnostic, DiagnosticMessage, messages};
use ts_r_lexer::{Lexer, LexerCheckpoint, Token, TokenFlags, TokenKind};

pub use jsdoc::{attach_jsdoc, attach_jsdoc_with_syntax};
pub use ts_r_ast;

/// Result of parsing one file.
#[derive(Debug)]
pub struct ParseResult {
    pub ast: Ast,
    /// Syntax errors in source order, with the grammar errors of [`ParseResult::grammar`]:
    /// what tsc reports for the file before type checking.
    pub diagnostics: Vec<Diagnostic>,
    /// The errors in `diagnostics` that tsc's checker reports (its grammar checks, only in a
    /// file without syntax errors: `class C<> {}`, `set x() {}`), not its parser. Parsers that
    /// stop at syntax errors only, like typescript-eslint's, do not report them. TS1501 and
    /// TS1503 (regex flags and named groups newer than the target,
    /// [`is_target_regex_error`]) are only here, not in `diagnostics`: the checker keeps
    /// them for older targets.
    pub grammar: Vec<Diagnostic>,
    /// Syntax errors to tsc that ESLint's parser and esbuild accept in sloppy code: octal
    /// and `\8` escapes in strings (TS1487, TS1488; `"\033"`). They are also in `grammar`,
    /// so `syntax_errors` and the build tools leave them out; the checker reports them as
    /// syntax errors. (In a template they are an error everywhere: syntax only.) Sorted by
    /// starting position.
    pub lenient: Vec<Diagnostic>,
}

impl ParseResult {
    /// `diagnostics` without the checker's grammar errors.
    pub fn syntax_errors(&self) -> Vec<Diagnostic> {
        without_grammar(&self.diagnostics, &self.grammar)
    }
}

/// `diagnostics` without the ones in `grammar` (see [`ParseResult::grammar`]).
/// Both slices are sorted by starting position. `grammar` may hold entries `diagnostics`
/// doesn't have (TS1501 and TS1503, which the checker filters by target).
pub fn without_grammar(diagnostics: &[Diagnostic], grammar: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut rest = grammar;
    diagnostics
        .iter()
        .filter(|d| !in_sorted(&mut rest, d))
        .cloned()
        .collect()
}

/// The syntax errors tsc reports for a file (what stops checking, and all a `noCheck`
/// build reports): `diagnostics` without `grammar`, but with `lenient` (octal escapes in
/// strings, which are in `grammar` for the build tools). All three are sorted by starting
/// position, and so is the result. Every tsc-facing caller (the checker driver,
/// `ts-r-check --noCheck`, the incremental history) goes through this.
pub fn tsc_syntax_errors(
    diagnostics: &[Diagnostic],
    grammar: &[Diagnostic],
    lenient: &[Diagnostic],
) -> Vec<Diagnostic> {
    let (mut g, mut l) = (grammar, lenient);
    diagnostics
        .iter()
        .filter(|d| {
            let in_grammar = in_sorted(&mut g, d);
            !in_grammar || in_sorted(&mut l, d)
        })
        .cloned()
        .collect()
}

/// Whether `d` is in `rest` (sorted by start), dropping the entries before `d` from `rest`.
/// Calls must come in order of position.
fn in_sorted(rest: &mut &[Diagnostic], d: &Diagnostic) -> bool {
    let skip = rest
        .iter()
        .take_while(|g| g.span.start < d.span.start)
        .count();
    *rest = &rest[skip..];
    rest.iter()
        .take_while(|g| g.span.start == d.span.start)
        .any(|g| g == d)
}

/// TS1501 (a regex flag needs a newer target) and TS1503 (named groups need ES2018): kept in
/// `grammar` only; the checker reports them when the target is older.
pub fn is_target_regex_error(d: &Diagnostic) -> bool {
    d.code == messages::REGEX_FLAG_NEEDS_TARGET.code
        || d.code == messages::REGEX_NAMED_GROUPS_NEED_TARGET.code
}

/// What to do with TypeScript-only syntax (types, `interface`, `as`, ...) in a JavaScript
/// file (`SourceKind::Js` / `Jsx`). Ignored for TypeScript kinds.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum TsInJs {
    /// JavaScript grammar only: TypeScript syntax is a syntax error. What [`parse`] does.
    #[default]
    Reject,
    /// tsc's handling of `.js` files: TypeScript syntax parses as in a `.tsx` file (so
    /// `<T>(x) => x` is JSX, `<T,>(x) => x` an arrow) except that expressions take no type
    /// arguments (`f<T>(x)` and `new A<T>(x)` are comparisons), and each TypeScript construct
    /// gets tsc's TS8xxx grammar error ("Type annotations can only be used in TypeScript
    /// files.").
    Report,
    /// The same grammar as `Report` without the TS8xxx errors: typescript-eslint's parser on
    /// `.js` files, which accepts the syntax silently.
    Allow,
}

/// Parser settings beyond the [`SourceKind`].
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct ParseOptions {
    pub ts_in_js: TsInJs,
}

/// Parses `src` with the grammar for `kind`.
pub fn parse(src: &str, kind: SourceKind) -> ParseResult {
    parse_with(src, kind, ParseOptions::default())
}

/// Parses `src` with the grammar for `kind`, adjusted by `opts`.
pub fn parse_with(src: &str, kind: SourceKind, opts: ParseOptions) -> ParseResult {
    if src.len() >= u32::MAX as usize {
        let mut ast = Ast::new(kind, 0);
        ast.root = ast.add(
            Node::Program {
                stmts: NodeList::EMPTY,
            },
            Span::EMPTY,
        );
        return ParseResult {
            ast,
            diagnostics: vec![Diagnostic::new(messages::FILE_TOO_LARGE, Span::EMPTY, &[])],
            grammar: Vec::new(),
            lenient: Vec::new(),
        };
    }
    let js_ts = !kind.is_typescript() && opts.ts_in_js != TsInJs::Reject;
    let start = |module_await: bool| {
        let mut p = Parser::new(src, kind);
        if js_ts {
            p.ts = true;
            p.jsx = true;
            p.js_ts = true;
        }
        p.module_await = module_await;
        p.parse_program();
        p
    };
    let mut p = start(false);
    // In a module, `await` at the top level is an operator even before a line break: the
    // file is parsed again when that changes anything (rare: `await` then a newline).
    if p.await_newline_seen && p.program_is_module() {
        p = start(true);
    }
    if js_ts && opts.ts_in_js == TsInJs::Report {
        p.report_ts_in_js();
    }
    p.finish()
}

impl Parser<'_> {
    /// The parsed program has top-level `import` or `export` syntax.
    fn program_is_module(&self) -> bool {
        let Node::Program { stmts } = *self.ast.node(self.ast.root) else {
            return false;
        };
        self.ast
            .list(stmts)
            .iter()
            .any(|&s| match *self.ast.node(s) {
                Node::ImportDecl { .. } => !self.ast.is_jsdoc(s),
                Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. } => true,
                ref n => n.modifiers().contains(ts_r_ast::Modifiers::EXPORT),
            })
    }
}

/// Parses a JSON file as tsc does for a `.json` source file (`resolveJsonModule`): a
/// program holding one expression statement with the JSON value (none for an empty file).
/// Anything after the value is parsed as statements, so it gets ordinary syntax errors.
pub fn parse_json(src: &str) -> ParseResult {
    if src.len() >= u32::MAX as usize {
        return parse(src, SourceKind::Ts);
    }
    let mut p = Parser::new(src, SourceKind::Ts);
    let mark = p.scratch.len();
    if !p.at(TokenKind::Eof) {
        let start = p.start();
        let expr = p.parse_expression();
        p.check_json_value(expr);
        let stmt = p.finish_node(start, Node::ExprStmt { expr });
        p.scratch.push(stmt);
        let rest = p.parse_statements(true, false);
        let rest: Vec<NodeId> = p.ast.list(rest).to_vec();
        p.scratch.extend(rest);
    }
    let stmts = p.finish_list(mark);
    p.ast.root = p
        .ast
        .add(Node::Program { stmts }, Span::new(0, src.len() as u32));
    p.finish()
}

/// Parses a file, picking the grammar from its path (`.ts`, `.tsx`, `.d.ts`, `.js`, ...).
pub fn parse_path(path: &str, src: &str) -> ParseResult {
    parse(src, SourceKind::from_path(path))
}

/// Context flags that change how constructs parse.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
struct Ctx(u32);

impl Ctx {
    /// Inside an async function: `await` is an operator.
    const AWAIT: u32 = 1 << 0;
    /// Inside a generator: `yield` is an operator.
    const YIELD: u32 = 1 << 1;
    /// `in` is not a binary operator (the init of a `for` statement).
    const DISALLOW_IN: u32 = 1 << 2;
    /// Inside a function body (any kind).
    const IN_FUNCTION: u32 = 1 << 3;
    /// Ambient context (`declare`, `.d.ts`).
    const AMBIENT: u32 = 1 << 4;
    /// Conditional types are not allowed (the `extends` clause of a conditional type).
    const DISALLOW_CONDITIONAL_TYPES: u32 = 1 << 5;
    /// Parsing the true branch of `a ? b : c`; arrow functions with return types must be
    /// followed by `:`.
    const IN_CONDITIONAL_TRUE: u32 = 1 << 6;
    /// Inside a class body.
    const IN_CLASS: u32 = 1 << 7;
    /// Parsing a decorator expression: no `[` element access.
    const DECORATOR: u32 = 1 << 8;
    /// Parsing a type (affects `<` handling and keywords).
    const TYPE: u32 = 1 << 9;
    /// Directly in a class static block, where `await` is an operator but every await
    /// expression is TS18037. Functions and field initializers clear it.
    const STATIC_BLOCK: u32 = 1 << 10;

    #[inline]
    fn has(self, f: u32) -> bool {
        self.0 & f != 0
    }
}

#[derive(Clone, Copy)]
struct ParserCheckpoint {
    lx: LexerCheckpoint,
    ast: AstCheckpoint,
    diags: u32,
    prev_end: u32,
    scratch: u32,
    ctx: Ctx,
    spec_failed: bool,
    halted: bool,
    grammar_diags: u32,
    soft_diags: u32,
    lex_diags_seen: u32,
    lists: u32,
    deferred: u32,
    checker_errs: u32,
    misplaced_module_names: u32,
    shorthand_inits: u32,
    pattern_literals: u32,
    regex_errs: u32,
    kept_grammar: u32,
}

/// Maximum nesting depth of recursive constructs before giving up on the file.
const MAX_DEPTH: u32 = 5000;

/// Native stack the parser may use (measured from where parsing started). Keeps deep inputs
/// from overflowing a 2 MiB thread stack in debug builds.
const STACK_BUDGET: usize = 1 << 20;

/// `enter` calls allowed at one token without the parser advancing. Real code needs a few
/// dozen; more means a loop that does not consume input.
const MAX_STALL: u32 = 10_000;

/// Node budget per source byte (plus a constant). Real code uses about 0.1; recovery on
/// garbage stays well under 2. Passing it means a runaway loop.
const NODES_PER_BYTE: usize = 4;

/// Approximate current stack address (no `unsafe`: only the address is taken).
#[inline(always)]
fn stack_addr() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

pub(crate) struct Parser<'a> {
    lx: Lexer<'a>,
    src: &'a str,
    ast: Ast,
    diags: Vec<Diagnostic>,
    /// Stack used to build lists without allocating per list.
    scratch: Vec<NodeId>,
    ctx: Ctx,
    /// TypeScript syntax enabled.
    ts: bool,
    /// JSX enabled.
    jsx: bool,
    /// TypeScript syntax in a JavaScript file (`TsInJs::Report`/`Allow`): no type arguments
    /// on calls, `new`, tagged templates, instantiation expressions or JSX tags, as tsc
    /// parses `.js`.
    js_ts: bool,
    /// Parsing a JSDoc type expression (`jsdoc.rs`): `*`, `?T`, `T?`, `!T`, `T!` and
    /// `Array.<T>` are types.
    jsdoc: bool,
    /// End of the previous token: the end of the node being finished.
    prev_end: u32,
    depth: u32,
    /// Speculation nesting; while > 0, the first error marks the attempt as failed.
    speculating: u32,
    spec_failed: bool,
    /// Start positions (with context bits) where an arrow function parse already failed, so
    /// nested parentheses are not re-speculated exponentially.
    failed_arrows: std::collections::HashSet<u64>,
    /// Positions of `<` whose type-argument list failed to parse during speculation. Lets
    /// `a < b < c < ...` stay linear: each `<` is tried as type arguments at most once.
    failed_type_args: std::collections::HashSet<u32>,
    /// Nesting of `is_contextual_declaration_after_modifier` lookaheads.
    modifier_lookahead: u32,
    /// The tag name of the JSX element whose child element is about to be parsed (set by
    /// `parse_jsx_children`, taken at the child's start).
    jsx_parent_name: Option<NodeId>,
    /// End of the `}` of the last block (statement block or function body) read, for
    /// TS2809 (`{ a: 0 } = value;`).
    last_block_end: u32,
    /// `{ a = 1 }` shorthand initializers: (`=` offset, start of the object literal). One
    /// whose literal never becomes a destructuring target is the checker's TS1312.
    shorthand_inits: Vec<(u32, u32)>,
    /// Starts of the object literals used as destructuring targets.
    pattern_literals: Vec<u32>,
    /// Regular expression errors (flags, escapes, class ranges): the checker's in tsc 7, so
    /// kept only in a file without syntax errors, and never merged with another error at
    /// the same position.
    regex_errs: Vec<Diagnostic>,
    /// Checker grammar errors tsc reports even in a file with syntax errors, and which do
    /// not stop the check: in both `diagnostics` and `grammar`, never deduplicated.
    kept_grammar: Vec<Diagnostic>,
    /// Start of the object literal being parsed (`u32::MAX` outside one).
    object_literal_start: u32,
    /// Start of the operand of the innermost prefix operator (`~<a/>`): a JSX element there
    /// takes no adjacent sibling elements (no TS2657).
    unary_operand_start: u32,
    /// Parsing a JSDoc `@import` tag (an `import type` without `import x =` forms).
    jsdoc_import: bool,
    /// A top-level `await` had its operand on the next line, so it was read as a name.
    await_newline_seen: bool,
    /// Reparsing a module: such an `await` is an operator (as in tsc, topLevelAwait.1).
    module_await: bool,
    /// Stack address when parsing started.
    stack_base: usize,
    /// The nesting limit was hit; the rest of the file was skipped.
    halted: bool,
    /// How many entries of `diags` are grammar errors (see `grammar_error_at`).
    grammar_diags: u32,
    /// Start of the last token read as a name (`intern_token`): an escaped keyword there is
    /// no TS1260.
    name_tok: u32,
    /// How many entries of `diags` are syntax errors that do not fail an enclosing
    /// speculative parse (see `soft_error`).
    soft_diags: u32,
    /// Length of the lexer's diagnostics when the parser last reported one. Scanner and
    /// parser errors form one stream in tsc; this tells which of the two came last.
    lex_diags_seen: u32,
    /// The list contexts being parsed (`lists::List` bits), for tsc's list recovery.
    lists: u32,
    /// Grammar errors tsc's checker reports only in a file without syntax errors (TS1123,
    /// TS1009, TS1248, ...): kept only if the file has no other parse error.
    deferred: Vec<Diagnostic>,
    /// Grammar errors tsc's checker reports even in a file with syntax errors
    /// (`checker_error_at`). Kept apart so they never merge with a syntax error.
    checker_errs: Vec<Diagnostic>,
    /// Names of misplaced `module` declarations, whose TS1540 errors are suppressed.
    misplaced_module_names: Vec<Span>,
    /// Enclosing loops, switches, labels and function bodies, for `break`/`continue`.
    jumps: Vec<grammar::Jump>,
    /// Nesting of `with` statement bodies, where tsc checks nothing.
    with_depth: u32,
    /// Token start where `enter` was last called, and how many times in a row.
    stall_pos: u32,
    stall_count: u32,
    /// Halt once the arena holds more nodes than this.
    node_budget: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, kind: SourceKind) -> Parser<'a> {
        Parser::with_ast(src, Ast::new(kind, src.len()))
    }

    /// A parser over `src` that adds to `ast` (whose kind picks the grammar).
    fn with_ast(src: &'a str, ast: Ast) -> Parser<'a> {
        let kind = ast.source_kind;
        let mut lx = Lexer::new(src);
        lx.next_token();
        let mut ctx = Ctx::default();
        if kind.is_declaration() {
            ctx.0 |= Ctx::AMBIENT;
        }
        Parser {
            lx,
            src,
            ast,
            diags: Vec::new(),
            scratch: Vec::with_capacity(256),
            ctx,
            ts: kind.is_typescript(),
            jsx: kind.is_jsx(),
            js_ts: false,
            jsdoc: false,
            prev_end: 0,
            depth: 0,
            speculating: 0,
            spec_failed: false,
            failed_arrows: std::collections::HashSet::new(),
            failed_type_args: std::collections::HashSet::new(),
            modifier_lookahead: 0,
            jsx_parent_name: None,
            last_block_end: u32::MAX,
            shorthand_inits: Vec::new(),
            pattern_literals: Vec::new(),
            regex_errs: Vec::new(),
            kept_grammar: Vec::new(),
            object_literal_start: u32::MAX,
            unary_operand_start: u32::MAX,
            jsdoc_import: false,
            await_newline_seen: false,
            module_await: false,
            stack_base: stack_addr(),
            halted: false,
            grammar_diags: 0,
            name_tok: u32::MAX,
            soft_diags: 0,
            lex_diags_seen: 0,
            lists: 0,
            deferred: Vec::new(),
            checker_errs: Vec::new(),
            misplaced_module_names: Vec::new(),
            jumps: Vec::new(),
            with_depth: 0,
            stall_pos: u32::MAX,
            stall_count: 0,
            node_budget: src
                .len()
                .saturating_mul(NODES_PER_BYTE)
                .saturating_add(1024),
        }
    }

    fn finish(mut self) -> ParseResult {
        // `push_diag` already dropped a parser error at the position of the error just
        // before it (scanner or parser); scanner errors are always kept, as in tsc.
        let clean = self.diags.len() as u32 == self.grammar_diags && self.lx.diagnostics.is_empty();
        // `{ s = 5 }` outside a destructuring target: TS1312 at the `=`
        // (shorthandPropertyAssignmentsInDestructuring).
        let patterns: std::collections::HashSet<u32> =
            self.pattern_literals.iter().copied().collect();
        let mut seen = std::collections::HashSet::new();
        for (eq, lit) in std::mem::take(&mut self.shorthand_inits) {
            if !patterns.contains(&lit) && seen.insert(eq) {
                self.deferred.push(Diagnostic::new(
                    messages::SHORTHAND_INIT_OUTSIDE_PATTERN,
                    Span::new(eq, eq + 1),
                    &[],
                ));
            }
        }
        let mut diags = std::mem::take(&mut self.diags);
        diags.append(&mut self.checker_errs);
        let mut grammar = Vec::new();
        if clean {
            if !self.misplaced_module_names.is_empty() {
                let misplaced: std::collections::HashSet<_> =
                    self.misplaced_module_names.into_iter().collect();
                self.deferred
                    .retain(|d| d.code != 1540 || !misplaced.contains(&d.span));
            }
            grammar = self.deferred.clone();
            grammar.sort_by_key(|d| (d.span.start, d.span.end));
            diags.append(&mut self.deferred);
        }
        diags.sort_by_key(|d| (d.span.start, d.span.end));
        // A deferred or TS8xxx error at the position of another error is dropped. Syntax
        // errors were deduplicated in the order they were reported (`push_diag`): two at
        // one position stay when another came between them (`<` at the end of the file is
        // `Type expected`, `'>' expected` at the end, then `Expression expected`).
        diags.dedup_by(|a, b| {
            a.span.start == b.span.start && (clean || a.code >= 2000 || b.code >= 2000)
        });
        let mut kept = diags.iter().peekable();
        grammar.retain(|d| {
            while kept.peek().is_some_and(|k| k.span.start < d.span.start) {
                kept.next();
            }
            kept.peek().is_some_and(|k| *k == d)
        });
        grammar.dedup_by(|a, b| a.span.start == b.span.start);
        if clean && !self.regex_errs.is_empty() {
            grammar.extend(self.regex_errs.iter().cloned());
            grammar.sort_by_key(|d| d.span.start);
            // A regex flag or named group newer than the target (TS1501, TS1503) is only
            // known to the checker, which has the target: grammar only.
            self.regex_errs.retain(|d| !is_target_regex_error(d));
            diags.append(&mut self.regex_errs);
        }
        let mut lenient: Vec<Diagnostic> = self
            .lx
            .diagnostics
            .iter()
            .filter(|d| {
                d.code == messages::OCTAL_ESCAPE_NOT_ALLOWED.code
                    || d.code == messages::INVALID_ESCAPE_SEQUENCE.code
            })
            .cloned()
            .collect();
        lenient.sort_by_key(|d| d.span.start);
        if !lenient.is_empty() {
            grammar.extend(lenient.iter().cloned());
            grammar.sort_by_key(|d| d.span.start);
        }
        if !self.kept_grammar.is_empty() {
            grammar.extend(self.kept_grammar.iter().cloned());
            grammar.sort_by_key(|d| d.span.start);
            diags.append(&mut self.kept_grammar);
        }
        diags.append(&mut self.lx.diagnostics);
        diags.sort_by_key(|d| d.span.start);
        self.ast.comments = std::mem::take(&mut self.lx.comments);
        ParseResult {
            ast: self.ast,
            diagnostics: diags,
            grammar,
            lenient,
        }
    }

    // ---- Tokens ----

    #[inline]
    fn tok(&self) -> Token {
        self.lx.token()
    }

    #[inline]
    fn kind(&self) -> TokenKind {
        self.lx.kind()
    }

    #[inline]
    fn at(&self, k: TokenKind) -> bool {
        self.lx.kind() == k
    }

    #[inline]
    fn start(&self) -> u32 {
        self.lx.token().start
    }

    #[inline]
    fn newline_before(&self) -> bool {
        self.lx.token().newline_before()
    }

    /// Consumes the current token.
    #[inline]
    fn bump(&mut self) {
        let tok = self.lx.token();
        if tok.flags.has(TokenFlags::ESCAPED_KEYWORD) && tok.start != self.name_tok {
            self.escaped_keyword_error(tok);
        }
        self.prev_end = tok.end;
        self.lx.next_token();
    }

    /// An escaped keyword consumed as a keyword, not read as a name (`intern_token`):
    /// TS1260, as in tsc (scannerUnicodeEscapeInKeyword1: `\u{0076}ar x`).
    #[cold]
    fn escaped_keyword_error(&mut self, tok: Token) {
        self.error_at(messages::KEYWORD_WITH_ESCAPES, tok.span(), &[]);
    }

    #[inline]
    fn eat(&mut self, k: TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consumes `k` or reports "'k' expected." without consuming.
    fn expect(&mut self, k: TokenKind) -> bool {
        if self.eat(k) {
            return true;
        }
        self.error_expected(k.as_str());
        false
    }

    /// Like [`Parser::expect`] for a closing token, adding "to match" related info.
    fn expect_closing(&mut self, k: TokenKind, open: TokenKind, open_pos: u32) -> bool {
        if self.eat(k) {
            return true;
        }
        let span = self.error_span();
        let d = Diagnostic::new(messages::EXPECTED, span, &[k.as_str()]).with_related(
            messages::THE_PARSER_EXPECTED_TO_FIND_A_1_TO_MATCH_THE_0_TOKEN_HERE,
            ts_r_diagnostics::FileId(0),
            Span::new(open_pos, open_pos + open.as_str().len() as u32),
            &[open.as_str(), k.as_str()],
        );
        self.push_diag(d);
        false
    }

    /// Text of the current token.
    #[inline]
    fn text(&self) -> &'a str {
        self.lx.token_text()
    }

    /// True if the current token is the identifier-like keyword or identifier with this text.
    fn at_contextual(&self, k: TokenKind) -> bool {
        self.at(k)
    }

    /// Identifier or a keyword usable as one in this context.
    fn at_identifier(&self) -> bool {
        let k = self.kind();
        if k == TokenKind::Ident {
            return true;
        }
        if !k.is_identifier_like() {
            return false;
        }
        match k {
            TokenKind::Yield => !self.ctx.has(Ctx::YIELD),
            TokenKind::Await => !self.ctx.has(Ctx::AWAIT),
            _ => true,
        }
    }

    // ---- Lookahead and speculation ----

    fn checkpoint(&self) -> ParserCheckpoint {
        ParserCheckpoint {
            lx: self.lx.checkpoint(),
            ast: self.ast.checkpoint(),
            diags: self.diags.len() as u32,
            prev_end: self.prev_end,
            scratch: self.scratch.len() as u32,
            ctx: self.ctx,
            spec_failed: self.spec_failed,
            halted: self.halted,
            grammar_diags: self.grammar_diags,
            soft_diags: self.soft_diags,
            lex_diags_seen: self.lex_diags_seen,
            lists: self.lists,
            deferred: self.deferred.len() as u32,
            checker_errs: self.checker_errs.len() as u32,
            misplaced_module_names: self.misplaced_module_names.len() as u32,
            shorthand_inits: self.shorthand_inits.len() as u32,
            pattern_literals: self.pattern_literals.len() as u32,
            regex_errs: self.regex_errs.len() as u32,
            kept_grammar: self.kept_grammar.len() as u32,
        }
    }

    fn rewind(&mut self, cp: ParserCheckpoint) {
        self.lx.rewind(cp.lx);
        self.ast.rollback(cp.ast);
        self.diags.truncate(cp.diags as usize);
        self.prev_end = cp.prev_end;
        self.scratch.truncate(cp.scratch as usize);
        self.ctx = cp.ctx;
        self.spec_failed = cp.spec_failed;
        self.halted = cp.halted;
        self.grammar_diags = cp.grammar_diags;
        self.soft_diags = cp.soft_diags;
        self.lex_diags_seen = cp.lex_diags_seen;
        self.lists = cp.lists;
        self.deferred.truncate(cp.deferred as usize);
        self.checker_errs.truncate(cp.checker_errs as usize);
        self.misplaced_module_names
            .truncate(cp.misplaced_module_names as usize);
        self.shorthand_inits.truncate(cp.shorthand_inits as usize);
        self.pattern_literals.truncate(cp.pattern_literals as usize);
        self.regex_errs.truncate(cp.regex_errs as usize);
        self.kept_grammar.truncate(cp.kept_grammar as usize);
    }

    /// Runs `f` and always restores the parser state afterwards.
    fn lookahead<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let cp = self.checkpoint();
        self.speculating += 1;
        let r = f(self);
        self.speculating -= 1;
        self.rewind(cp);
        r
    }

    /// The token after the current one.
    fn peek(&mut self) -> Token {
        let cp = self.lx.checkpoint();
        self.lx.next_token();
        let t = self.lx.token();
        self.lx.rewind(cp);
        t
    }

    /// Kind of the token after the current one.
    fn peek_kind(&mut self) -> TokenKind {
        self.peek().kind
    }

    /// Runs `f` speculatively. Keeps its result if it returns `Some` without reporting any
    /// error; otherwise restores the state and returns `None`.
    fn try_parse<R>(&mut self, f: impl FnOnce(&mut Self) -> Option<R>) -> Option<R> {
        let cp = self.checkpoint();
        let lex_errs = self.lx.diagnostics.len();
        self.speculating += 1;
        self.spec_failed = false;
        let r = f(self);
        self.speculating -= 1;
        let new_diags = self.diags.len() as u32 - cp.diags;
        let new_grammar = self.grammar_diags - cp.grammar_diags + self.soft_diags - cp.soft_diags;
        let failed =
            self.spec_failed || new_diags > new_grammar || self.lx.diagnostics.len() > lex_errs;
        match r {
            Some(v) if !failed => {
                self.spec_failed = cp.spec_failed;
                Some(v)
            }
            _ => {
                self.rewind(cp);
                None
            }
        }
    }

    /// True while a speculative parse has already failed; loops use it to bail out early.
    #[inline]
    fn aborting(&self) -> bool {
        self.halted || (self.speculating > 0 && self.spec_failed)
    }

    // ---- Errors ----

    fn error_span(&self) -> Span {
        let t = self.tok();
        if t.kind == TokenKind::Eof {
            Span::new(t.start, t.start)
        } else {
            t.span()
        }
    }

    fn push_diag(&mut self, d: Diagnostic) {
        if self.speculating > 0 {
            self.spec_failed = true;
        }
        // tsc drops a parse error at the position of the previous error, which may be the
        // scanner's: a stray backslash is TS1127 only, not also TS1128.
        let lex = &self.lx.diagnostics;
        let last = if lex.len() as u32 > self.lex_diags_seen {
            lex.last()
        } else {
            self.diags.last()
        };
        if last.is_some_and(|l| l.span.start == d.span.start) {
            return;
        }
        self.lex_diags_seen = lex.len() as u32;
        self.diags.push(d);
    }

    /// Reports an error that is about grammar rather than token structure: it does not
    /// make an enclosing speculative parse fail (its diagnostics are still rolled back).
    fn grammar_error_at(&mut self, msg: DiagnosticMessage, span: Span, args: &[&str]) {
        let failed = self.spec_failed;
        let before = self.diags.len();
        self.push_diag(Diagnostic::new(msg, span, args));
        self.spec_failed = failed;
        if self.diags.len() > before {
            self.grammar_diags += 1;
        }
    }

    /// A syntax error at the current token that does not make an enclosing speculative
    /// parse fail (it is still rolled back with it).
    fn soft_error(&mut self, msg: DiagnosticMessage) {
        let failed = self.spec_failed;
        let before = self.diags.len();
        let span = self.error_span();
        self.push_diag(Diagnostic::new(msg, span, &[]));
        self.spec_failed = failed;
        if self.diags.len() > before {
            self.soft_diags += 1;
        }
    }

    /// A grammar error tsc's checker reports even in a file with syntax errors (TS1164 is in
    /// the checker's `CHECKER_GRAMMAR_CODES`, so it does not stop the check; TS1141 for a
    /// module specifier is not). Never merged with a parse error at the same position, not
    /// part of the order `push_diag` deduplicates in, and does not fail speculation.
    fn checker_error_at(&mut self, msg: DiagnosticMessage, span: Span) {
        self.checker_errs.push(Diagnostic::new(msg, span, &[]));
    }

    /// A checker grammar error: deferred in TypeScript, and kept next to syntax errors in
    /// JavaScript, as tsc 7 does (plainJSGrammarErrors).
    fn checker_grammar_error(&mut self, msg: DiagnosticMessage, span: Span) {
        if !self.ts || self.js_ts {
            self.grammar_error_at(msg, span, &[]);
        } else {
            self.deferred_grammar_error_at(msg, span, &[]);
        }
    }

    /// A checker grammar error tsc reports next to syntax errors without stopping the
    /// check (TS18037): see `kept_grammar`. Does not fail speculation.
    fn kept_grammar_error_at(&mut self, msg: DiagnosticMessage, span: Span) {
        self.kept_grammar.push(Diagnostic::new(msg, span, &[]));
    }

    /// A grammar error tsc's checker reports only when the file has no syntax errors: kept
    /// by `finish` only if no parse error was reported. Does not fail speculation.
    pub(crate) fn deferred_grammar_error_at(
        &mut self,
        msg: DiagnosticMessage,
        span: Span,
        args: &[&str],
    ) {
        self.deferred.push(Diagnostic::new(msg, span, args));
    }

    /// After a rest element: a `,` directly followed by `close` is TS1013, except in an
    /// ambient context (`declare function f(...args,): void;`, tsc 7).
    fn check_rest_trailing_comma(&mut self, close: TokenKind) {
        if self.at(TokenKind::Comma) && self.peek_kind() == close && !self.ctx.has(Ctx::AMBIENT) {
            // The checker's error in tsc 7: only in a TypeScript file without syntax errors
            // (kept next to them in JavaScript, plainJSGrammarErrors).
            let span = self.tok().span();
            self.rest_grammar_error(messages::REST_TRAILING_COMMA, span);
        }
    }

    /// A rest element in a binding pattern that more elements follow: TS2462 at its target
    /// (a checker grammar error in tsc 7, on the binding element's name).
    fn check_rest_is_last(&mut self, close: TokenKind, target: Span) {
        let last = self.at(close)
            || self.at(TokenKind::Eof)
            || (self.at(TokenKind::Comma) && self.peek_kind() == close);
        if !last {
            self.rest_grammar_error(messages::REST_ELEMENT_NOT_LAST, target);
        }
    }

    /// A rest element's grammar error: kept next to syntax errors in JavaScript, only
    /// without them in TypeScript (plainJSGrammarErrors).
    fn rest_grammar_error(&mut self, msg: DiagnosticMessage, span: Span) {
        if !self.ts || self.js_ts {
            self.grammar_error_at(msg, span, &[]);
        } else {
            self.deferred_grammar_error_at(msg, span, &[]);
        }
    }

    fn error_at(&mut self, msg: DiagnosticMessage, span: Span, args: &[&str]) {
        self.push_diag(Diagnostic::new(msg, span, args));
    }

    /// tsc's checks on a parsed JSON value (`convertToJson`): only JSON literals, objects of
    /// `"key": value` properties and arrays, with double-quoted strings.
    fn check_json_value(&mut self, e: NodeId) {
        let span = self.ast.span(e);
        match *self.ast.node(e) {
            Node::BoolLit { .. } | Node::NullLit {} | Node::NumberLit { .. } => {}
            // Already a syntax error.
            Node::Missing {} => {}
            Node::StringLit { .. } => self.check_json_double_quoted(e),
            Node::Unary {
                op: UnaryOp::Minus,
                arg,
            } if matches!(self.ast.node(arg), Node::NumberLit { .. }) => {}
            Node::ObjectLit { props } => {
                for prop in self.ast.list(props).to_vec() {
                    match *self.ast.node(prop) {
                        Node::Property { key, value, .. } => {
                            self.check_json_double_quoted(key);
                            self.check_json_value(value);
                        }
                        _ => {
                            let span = self.ast.span(prop);
                            self.error_at(messages::PROPERTY_ASSIGNMENT_EXPECTED, span, &[]);
                        }
                    }
                }
            }
            Node::ArrayLit { elements } => {
                for el in self.ast.list(elements).to_vec() {
                    self.check_json_value(el);
                }
            }
            _ => self.error_at(messages::JSON_PROPERTY_VALUE, span, &[]),
        }
    }

    fn check_json_double_quoted(&mut self, e: NodeId) {
        let span = self.ast.span(e);
        let quoted = matches!(self.ast.node(e), Node::StringLit { .. })
            && self.src.as_bytes().get(span.start as usize) == Some(&b'"');
        if !quoted {
            self.error_at(
                messages::STRING_LITERAL_WITH_DOUBLE_QUOTES_EXPECTED,
                span,
                &[],
            );
        }
    }

    /// Reports `msg` at the current token.
    fn error(&mut self, msg: DiagnosticMessage) {
        let mut span = self.error_span();
        // As in tsc 7, a missing expression or type at the end of the file is reported
        // just after the last token, before any trailing trivia (`'}' expected` is not).
        if self.at(TokenKind::Eof) && matches!(msg.code, 1003 | 1109 | 1110) {
            span = Span::at(self.prev_end.min(span.start));
        }
        self.error_at(msg, span, &[]);
    }

    fn error_expected(&mut self, what: &str) {
        // `() => {} = value;`: TS2809 at the `=`, which is skipped, then the `;` (or `,`)
        // is expected at the next token (tsc 7).
        if matches!(what, ";" | ",") && self.at_equals_after_block() {
            let span = self.tok().span();
            self.error_at(messages::EQUALS_AFTER_BLOCK, span, &[]);
            self.bump();
        }
        let span = self.error_span();
        self.error_at(messages::EXPECTED, span, &[what]);
    }

    /// At an `=` right after the `}` of a block (`{ a: 0 } = value;`, `() => {} = x`), not
    /// a `}` read some other way (`var {a, %} = x;` is TS1128 at the `=`).
    fn at_equals_after_block(&self) -> bool {
        self.at(TokenKind::Eq) && self.prev_end == self.last_block_end
    }

    // ---- Node building ----

    #[inline]
    fn finish_node(&mut self, start: u32, node: Node) -> NodeId {
        // A node that consumed no tokens (error recovery) is zero-width at the end of the
        // previous token, like `Missing`.
        let start = start.min(self.prev_end);
        self.check_node_budget();
        self.ast.add(node, Span::new(start, self.prev_end))
    }

    #[inline]
    fn add_node(&mut self, span: Span, node: Node) -> NodeId {
        self.check_node_budget();
        self.ast.add(node, span)
    }

    /// Safety net against runaway loops: past the node budget, halt.
    #[inline]
    fn check_node_budget(&mut self) {
        if self.ast.node_count() > self.node_budget {
            self.halt(messages::PARSER_NO_PROGRESS);
        }
    }

    /// A zero-width `Missing` node at the end of the previous token.
    fn missing(&mut self) -> NodeId {
        self.check_node_budget();
        self.ast.add(Node::Missing {}, Span::at(self.prev_end))
    }

    /// Converts `self.scratch[mark..]` into a list and pops it.
    fn finish_list(&mut self, mark: usize) -> NodeList {
        let list = self.ast.add_list(&self.scratch[mark..]);
        self.scratch.truncate(mark);
        list
    }

    fn intern_token(&mut self) -> ts_r_ast::Atom {
        self.name_tok = self.tok().start;
        let v = self.lx.ident_value(self.tok());
        self.ast.intern(&v)
    }

    /// Enters a nested construct. Returns false if nesting is too deep (by depth or by native
    /// stack use). The first time that happens, the error is reported and the rest of the file
    /// is skipped, so every loop terminates at end of file.
    ///
    /// It also catches loops that stop consuming input: `MAX_STALL` calls at one token halt
    /// the parse with TS99003.
    fn enter(&mut self) -> bool {
        self.depth += 1;
        if self.halted {
            return false;
        }
        if self.depth > MAX_DEPTH || self.stack_base.abs_diff(stack_addr()) > STACK_BUDGET {
            self.halt(messages::MAXIMUM_NESTING_DEPTH);
            return false;
        }
        let pos = self.tok().start;
        if pos == self.stall_pos {
            self.stall_count += 1;
            if self.stall_count > MAX_STALL {
                self.halt(messages::PARSER_NO_PROGRESS);
                return false;
            }
        } else {
            self.stall_pos = pos;
            self.stall_count = 0;
        }
        true
    }

    /// Stops parsing: reports `msg` (once) and skips to end of file. `aborting()` is then
    /// true, so every list loop exits. Speculation that rewinds past this point undoes it.
    fn halt(&mut self, msg: DiagnosticMessage) {
        if !self.halted {
            self.halted = true;
            let span = self.error_span();
            self.error_at(msg, span, &[]);
        }
        while !self.at(TokenKind::Eof) {
            self.lx.next_token();
        }
        self.prev_end = self
            .prev_end
            .max(self.lx.token().start.min(self.src.len() as u32));
    }

    #[inline]
    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Runs `f` with context flags added and removed, restoring them afterwards.
    fn with_ctx<R>(&mut self, add: u32, remove: u32, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.ctx;
        self.ctx.0 = (self.ctx.0 | add) & !remove;
        let r = f(self);
        self.ctx = saved;
        r
    }

    // ---- Semicolons ----

    /// True if a statement can end here (`;`, `}`, end of file or a preceding line break).
    fn can_insert_semicolon(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Semi | TokenKind::RBrace | TokenKind::Eof
        ) || self.newline_before()
    }

    /// Consumes a `;` or applies automatic semicolon insertion.
    fn semicolon(&mut self) {
        if self.eat(TokenKind::Semi) || self.can_insert_semicolon() {
            return;
        }
        self.error_expected(";");
    }
}

/// Convenience for tests and tools: parse and dump the tree.
pub fn dump(src: &str, kind: SourceKind) -> String {
    parse(src, kind).ast.dump()
}

#[cfg(test)]
mod tests;
