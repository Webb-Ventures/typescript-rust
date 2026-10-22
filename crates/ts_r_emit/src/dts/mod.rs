//! Declaration emit (design record 0012, E2): the `.d.ts` text of one source file.
//!
//! The emitter prints the declaration view of the file's (untransformed) AST: explicit
//! surfaces are copied (signatures, annotations, heritage, modifiers), bodies and
//! initializers are dropped, and every missing annotation is asked of the checker
//! ([`Ck::type_to_type_node`]), whose nodes are appended to the emitter's copy of the AST
//! (the checker's tree is never mutated). Layout is tsc 7.0.2's declaration layout
//! (observed): 4-space indentation, one member per line, type literals always multi-line,
//! JSDoc comments only.
//!
//! Which statements appear: in a script every declaration; in a module (and in a namespace
//! body) the exported ones plus the local declarations and imports that printed names
//! refer to. The emitter prints the file, and while names refer to declarations it did not
//! print yet, prints it again (a fixpoint over the "needed" set, usually two passes).
//!
//! A type the checker cannot write ([`NotNameable`]) is never replaced by `any`: the file's
//! `.d.ts` is refused with TSR2001 (record 0012, invariant 2).

mod expando;
mod isolated;
mod reuse;
mod types;

use rustc_hash::FxHashSet;
use ts_r_ast::{
    Ast, Checkpoint, CommentKind, MethodKind, Modifiers, ModuleKind, Node, NodeId, NodeList,
    ParentMap, VarKind,
};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_checker::typenode::{ModuleSpecifierHost, NotNameable, TypeNodeCx};
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_diagnostics::{Category, Diagnostic, FileId, SOUND_CODE_BASE};
use ts_r_span::Span;
use ts_r_types::{SymId, TypeFlags, TypeId, TypeKind};

/// TSR2001: an inferred type in a declaration is unresolved; the `.d.ts` is skipped.
pub const TSR_UNRESOLVED_TYPE: u32 = SOUND_CODE_BASE + 2001;
/// TSR2006: `.d.ts` output for a JavaScript input is not supported.
pub const TSR_JS_DECLARATIONS: u32 = SOUND_CODE_BASE + 2006;

/// Options of one declaration file.
#[derive(Clone, Copy, Debug)]
pub struct DtsOptions {
    pub newline: &'static str,
    /// `removeComments`.
    pub remove_comments: bool,
    /// `stripInternal`.
    pub strip_internal: bool,
    /// `declarationMap`: build [`DtsFile::map`].
    pub declaration_map: bool,
    /// `isolatedDeclarations`: types must follow from syntax (TS9xxx refuses the file).
    pub isolated_declarations: bool,
}

/// The declaration text of one file, or why there is none.
#[derive(Debug, Default)]
pub struct DtsFile {
    /// `None` when the file's declarations cannot be written (see `diagnostics`).
    pub text: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// With `declarationMap`: each written declaration mapped to where it starts in the
    /// source, and each name copied from the source to its start and end (`file`,
    /// `sources` and the URL are the caller's).
    pub map: Option<ts_r_codegen::SourceMap>,
}

/// Emits the declaration file of `file`. `host` finds package names for `import()` types
/// (without one they are relative paths or `node_modules` paths).
pub fn declaration_file(
    checker: &Checker,
    file: FileId,
    opts: &DtsOptions,
    host: Option<&dyn ModuleSpecifierHost>,
) -> DtsFile {
    let f = checker.file(file);
    let mut e = Emitter::new(checker, file, opts, host);
    let root = f.ast.root;
    let stmts: Vec<NodeId> = match f.ast.node(root) {
        Node::Program { stmts } => f.ast.list(*stmts).to_vec(),
        _ => Vec::new(),
    };
    let cp = e.ast.checkpoint();
    // Each growing pass adds source nodes to the finite needed/used sets.
    loop {
        e.reset(cp);
        e.file_header(&stmts);
        let scope = if e.is_module {
            Scope::ModuleTop
        } else {
            Scope::ScriptTop
        };
        e.statements(&stmts, scope, None);
        e.eof_comments(&stmts);
        if !e.grew {
            break;
        }
    }
    let mut out = DtsFile::default();
    if !e.failed.is_empty() {
        // With isolatedDeclarations errors, the TS9xxx explain the refusal.
        let isolated = e.failed.iter().any(|f| (9000..10000).contains(&f.0));
        for (code, span, message, related) in std::mem::take(&mut e.failed) {
            if isolated && code == TSR_UNRESOLVED_TYPE {
                continue;
            }
            let related = related
                .map(|(rs, rm)| ts_r_diagnostics::RelatedInfo {
                    code: 9027,
                    category: Category::Message,
                    message: rm,
                    file,
                    span: rs,
                })
                .into_iter()
                .collect();
            out.diagnostics.push(Diagnostic {
                code,
                category: Category::Error,
                message,
                file,
                span,
                related,
            });
        }
        return out;
    }
    let text = std::mem::take(&mut e.out);
    if opts.declaration_map {
        out.map = Some(declaration_map(&text, e.src, &e.marks));
    }
    out.text = Some(text);
    out
}

/// The map of a declaration file: `marks` are `(output offset, source offset)` pairs in
/// output order.
fn declaration_map(out: &str, src: &str, marks: &[(u32, u32)]) -> ts_r_codegen::SourceMap {
    let mut b = ts_r_codegen::SourceMapBuilder::new();
    let mut lines = ts_r_codegen::LineLookup::new(src);
    let mut out_lines = ts_r_codegen::LineLookup::new(out);
    for &(o, s) in marks {
        let (gl, gc) = out_lines.lookup(o);
        let (sl, sc) = lines.lookup(s);
        // tsc merges marks at one output position: a later one replaces an earlier one
        // unless it maps further back in the source.
        b.add_tsc(gl, gc, sl, sc);
    }
    b.finish(None, String::new(), None, None)
}

/// Where a statement list is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// A module's top level: exported declarations and what they need.
    ModuleTop,
    /// A script's top level: every declaration.
    ScriptTop,
    /// A (non-ambient) namespace body: exported declarations and what they need.
    Namespace,
    /// An ambient body (`declare namespace`, `declare module "x"`, `declare global`):
    /// everything, as written.
    Ambient,
}

impl Scope {
    fn top(self) -> bool {
        matches!(self, Scope::ModuleTop | Scope::ScriptTop)
    }
}

pub(crate) struct Emitter<'c, 'a> {
    ck: Ck<'c, 'a>,
    c: &'c Checker<'a>,
    file: FileId,
    src: &'a str,
    /// The emitter's copy of the file's AST, with synthesized type nodes appended.
    ast: Ast,
    /// Nodes below this index are the source's.
    orig_len: usize,
    parents: &'a ParentMap,
    opts: DtsOptions,
    host: Option<&'c dyn ModuleSpecifierHost>,
    is_module: bool,
    out: String,
    indent: usize,
    type_depth: u32,
    line_start: bool,
    /// Local declaration statements printed although not exported.
    needed: FxHashSet<NodeId>,
    /// Alias declarations (import specifiers, `import x = ...`) printed names use.
    used_aliases: FxHashSet<NodeId>,
    /// The needed or used sets grew during this pass (print again).
    grew: bool,
    /// Types that could not be written: `(code, span, message)`, in output order.
    failed: Vec<Failure>,
    /// Names generated in this pass (`_default`, `X_base`), kept unique.
    generated: FxHashSet<String>,
    /// `declarationMap`: `(output offset, source offset)` pairs in output order. As tsc
    /// does, every node written from the source maps its start (its first token) and its
    /// end; synthesized nodes map nothing (see [`Emitter::mark`]).
    marks: Vec<(u32, u32)>,
    /// While non-zero, [`Emitter::mark`] records nothing (nodes tsc rebuilds, such as the
    /// parameters of a function type written from a function expression).
    quiet: u32,
    /// The next parameter list printed is one tsc rebuilds (a function type written from a
    /// function expression): its parameters map only their annotations.
    pub(super) rebuilt_params: bool,
    /// The end of the last source declaration written (its comments were placed).
    last_written_end: Option<u32>,
    /// The next generated local name of expando namespaces (`_a`, `_b`, ...).
    temp_index: u32,
    /// The end of the file's detached header comments (the first statement's comments
    /// start after it).
    header_end: u32,
    /// Synthesized members of object literal types and the object literal members of
    /// this file that declare them (their JSDoc is written on the member).
    member_docs: rustc_hash::FxHashMap<NodeId, NodeId>,
    /// Synthesized nodes that stand for source text (object literal keys tsc writes from
    /// the literal's syntax) and that text's span: they map like source nodes.
    origins: rustc_hash::FxHashMap<NodeId, Span>,
}

/// A declaration that could not be written: `(code, span, message, related)`, where
/// `related` is where to add an annotation (TS9027).
type Failure = (u32, Span, String, Option<(Span, String)>);

impl<'c, 'a> Emitter<'c, 'a> {
    fn new(
        c: &'c Checker<'a>,
        file: FileId,
        opts: &DtsOptions,
        host: Option<&'c dyn ModuleSpecifierHost>,
    ) -> Emitter<'c, 'a> {
        let f = c.file(file);
        let mut ck = Ck::new(c, file);
        ck.no_report += 1;
        Emitter {
            ck,
            c,
            file,
            src: &f.text,
            ast: f.ast.clone(),
            orig_len: f.ast.nodes.len(),
            parents: f.parents(),
            opts: *opts,
            host,
            is_module: f.bound.is_module,
            out: String::new(),
            indent: 0,
            type_depth: 0,
            line_start: true,
            needed: FxHashSet::default(),
            used_aliases: FxHashSet::default(),
            grew: false,
            failed: Vec::new(),
            generated: FxHashSet::default(),
            marks: Vec::new(),
            quiet: 0,
            rebuilt_params: false,
            last_written_end: None,
            temp_index: 0,
            header_end: 0,
            member_docs: rustc_hash::FxHashMap::default(),
            origins: rustc_hash::FxHashMap::default(),
        }
    }

    fn reset(&mut self, cp: Checkpoint) {
        self.ast.rollback(cp);
        self.out.clear();
        self.indent = 0;
        self.line_start = true;
        self.grew = false;
        self.failed.clear();
        self.generated.clear();
        self.marks.clear();
        self.quiet = 0;
        self.rebuilt_params = false;
        self.last_written_end = None;
        self.temp_index = 0;
        self.member_docs.clear();
        self.origins.clear();
    }

    /// `base`, or `base_1`, `base_2`, ... when the file already uses the name.
    fn unique_name(&mut self, base: &str) -> String {
        let atoms = &self.c.file(self.file).ast.atoms;
        let mut name = base.to_owned();
        let mut i = 0;
        while atoms.find(&name).is_some() || self.generated.contains(&name) {
            i += 1;
            name = format!("{base}_{i}");
        }
        self.generated.insert(name.clone());
        name
    }

    // ---- writing ----------------------------------------------------------------------

    fn w(&mut self, s: &str) {
        if self.line_start {
            for _ in 0..self.indent {
                self.out.push_str("    ");
            }
            self.line_start = false;
        }
        self.out.push_str(s);
    }

    // ---- declaration map --------------------------------------------------------------
    //
    // Observed in tsc 7.0.2's `.d.ts.map`: each node the declaration keeps from the source
    // maps the output position where it starts to its first token, and the position after
    // it to its end (so a statement's end mark follows its `;` and maps past a function
    // body). Tokens such as `?`, modifiers and `readonly` / `+` / `-` in mapped types map
    // their own start and end; keywords and punctuation the node writes do not. Nodes tsc
    // creates map nothing: inferred types, `declare` modifier lists (a list is the
    // source's only when the declaration keeps exactly the source's modifiers), the
    // properties of parameter properties, `export default` / `export =` of an identifier.
    // Comments map their start and end.

    /// The output offset of the next written text (after the indentation a line start
    /// still owes).
    fn cur(&self) -> u32 {
        let indent = if self.line_start { self.indent * 4 } else { 0 };
        (self.out.len() + indent) as u32
    }

    /// Maps the current output position to source offset `pos`.
    pub(super) fn mark(&mut self, pos: u32) {
        if self.quiet == 0 {
            let at = self.cur();
            self.marks.push((at, pos));
        }
    }

    /// The start mark of source node `n`.
    pub(super) fn start(&mut self, n: NodeId) {
        if !self.is_synth(n) {
            self.mark(self.ast.span(n).start);
        } else if let Some(sp) = self.origins.get(&n) {
            self.mark(sp.start);
        }
    }

    /// The end mark of source node `n`.
    pub(super) fn end(&mut self, n: NodeId) {
        if !self.is_synth(n) {
            self.mark(self.ast.span(n).end);
        } else if let Some(sp) = self.origins.get(&n) {
            self.mark(sp.end);
        }
    }

    /// Writes `;`, the end mark of statement or member `n` and a newline.
    fn semi_end(&mut self, n: NodeId) {
        self.w(";");
        self.end(n);
        self.nl();
    }

    /// Writes `text` for the source token at `pos` with marks at its start and end.
    pub(super) fn token(&mut self, text: &str, pos: Option<u32>) {
        match pos {
            Some(p) => {
                self.mark(p);
                self.w(text);
                self.mark(p + text.len() as u32);
            }
            None => self.w(text),
        }
    }

    /// The offset of the first token at or after `pos` (whitespace and comments skipped).
    pub(super) fn next_token(&self, pos: u32) -> u32 {
        let b = self.src.as_bytes();
        let mut i = pos as usize;
        while i < b.len() {
            match b[i] {
                b' ' | b'\t' | b'\n' | b'\r' => i += 1,
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    i = self.src[i..].find('\n').map_or(b.len(), |n| i + n);
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i = self.src[i + 2..]
                        .find("*/")
                        .map_or(b.len(), |n| i + 2 + n + 2);
                }
                _ => break,
            }
        }
        i as u32
    }

    /// The offset of `ch` when it is the next token at or after `pos`.
    pub(super) fn char_token(&self, pos: u32, ch: u8) -> Option<u32> {
        let at = self.next_token(pos);
        (self.src.as_bytes().get(at as usize) == Some(&ch)).then_some(at)
    }

    /// The modifier keywords of declaration `n` with their flags and source offsets, and
    /// the offset of the token after them.
    pub(super) fn modifier_tokens(&self, n: NodeId) -> (Vec<(Modifiers, u32)>, u32) {
        let toks = self.ast.modifier_tokens_of(n);
        let after = match toks.last() {
            Some(t) => t.span.end,
            None => self.ast.span(n).start,
        };
        let out = toks.iter().map(|t| (t.flag, t.span.start)).collect();
        (out, self.next_token(after))
    }

    /// Writes the modifiers `words` (in tsc's order, each with its flag) of declaration
    /// `n`, each followed by a space. When they are exactly the source's modifier tokens,
    /// tsc keeps the source's list: they are written in source order with their marks.
    pub(super) fn modifiers(&mut self, n: NodeId, words: &[(Modifiers, &str)]) {
        if !self.is_synth(n) && !words.is_empty() {
            let (tokens, _) = self.modifier_tokens(n);
            let same = tokens.len() == words.len()
                && tokens
                    .iter()
                    .all(|(f, _)| words.iter().any(|(g, _)| g == f));
            if same {
                for (f, at) in tokens {
                    if let Some((_, w)) = words.iter().find(|(g, _)| *g == f) {
                        self.token(w, Some(at));
                        self.w(" ");
                    }
                }
                return;
            }
        }
        for (_, w) in words {
            self.w(w);
            self.w(" ");
        }
    }

    fn mark_opt(&mut self, pos: Option<u32>) {
        if let Some(p) = pos {
            self.mark(p);
        }
    }

    /// The source offsets of the `{` and after the `}` around the specifier list `specs`
    /// of import or export declaration `s` (named imports and exports are nodes of their
    /// own in tsc's tree).
    fn braces(&self, s: NodeId, specs: &[NodeId]) -> (Option<u32>, Option<u32>) {
        if self.is_synth(s) {
            return (None, None);
        }
        let span = self.ast.span(s);
        let first = specs.first().map_or(span.end, |&f| self.ast.span(f).start);
        let open = self.src[span.start as usize..first as usize]
            .rfind('{')
            .map(|i| span.start + i as u32);
        let from = specs
            .last()
            .map_or(open.map_or(span.start, |o| o + 1), |&l| {
                self.ast.span(l).end
            });
        let close = self.src[from as usize..span.end as usize]
            .find('}')
            .map(|i| from + i as u32 + 1);
        (open, close)
    }

    /// Whether a `,` follows the last of the original specifiers `specs` before the `}`
    /// ending at `close` (comments skipped).
    fn trailing_comma(&self, specs: &[NodeId], close: Option<u32>) -> bool {
        let (Some(&last), Some(close)) = (specs.last(), close) else {
            return false;
        };
        if self.is_synth(last) {
            return false;
        }
        let from = self.ast.span(last).end;
        let to = close.saturating_sub(1);
        if from >= to {
            return false;
        }
        let mut pos = from;
        for c in self.ast.comments_between(from, to) {
            if self.src[pos as usize..c.span.start as usize].contains(',') {
                return true;
            }
            pos = c.span.end;
        }
        self.src[pos as usize..to as usize].contains(',')
    }

    /// The comments before an original import or export specifier, as tsc keeps them in a
    /// declaration file (observed): a comment on the line of the `{` or `,` before it of
    /// any kind (`a, // c` then a line break; `/* c */ b`); a comment on its own line only
    /// when it is `/** */` or `/*! */`, on a line of its own (`a, \n/** c */\nb`).
    fn specifier_comments(&mut self, sp: NodeId) {
        if self.opts.remove_comments || self.is_synth(sp) {
            return;
        }
        let start = self.ast.span(sp).start;
        let from = self.list_token_end(start, &['{', ',']);
        let comments: Vec<_> = self.ast.comments_between(from, start).to_vec();
        let mut prev = from;
        for c in comments {
            let own_line = self
                .src
                .get(prev as usize..c.span.start as usize)
                .is_some_and(|t| t.contains('\n'));
            prev = c.span.end;
            let text = self.src[c.span.start as usize..c.span.end as usize].to_owned();
            let doc = text.starts_with("/**") || text.starts_with("/*!");
            if own_line && !doc {
                continue;
            }
            if own_line && !self.line_start {
                self.nl();
            }
            match c.kind {
                ts_r_ast::CommentKind::Line => {
                    self.mark(c.span.start);
                    self.w(text.trim_end());
                    self.mark(c.span.end);
                    self.nl();
                }
                _ => {
                    self.write_comment(&text, c.span.start);
                    let item_on_next_line = self
                        .src
                        .get(prev as usize..start as usize)
                        .is_some_and(|t| t.contains('\n'));
                    if own_line && item_on_next_line {
                        self.nl();
                    } else {
                        self.w(" ");
                    }
                }
            }
        }
    }

    /// Writes a name from the source with declaration-map marks at its start and end, so
    /// go-to-definition through the map lands on the name.
    fn name_text(&mut self, n: NodeId) {
        if self.is_synth(n) {
            let raw = self.raw(n).to_owned();
            self.w(&raw);
            return;
        }
        let span = self.ast.span(n);
        self.mark(span.start);
        let raw = self.raw(n).to_owned();
        self.w(&raw);
        self.mark(span.end);
    }

    fn nl(&mut self) {
        self.out.push_str(self.opts.newline);
        self.line_start = true;
    }

    fn loc(&self, n: NodeId) -> Loc {
        Loc::new(self.file, n)
    }

    fn is_synth(&self, n: NodeId) -> bool {
        n.index() >= self.orig_len
    }

    fn raw(&self, n: NodeId) -> &'a str {
        let sp = self.ast.span(n);
        self.src
            .get(sp.start as usize..sp.end as usize)
            .unwrap_or("")
    }

    /// Records that the declaration `at` cannot be written (the file's `.d.ts` is refused).
    fn fail(&mut self, at: NodeId, why: NotNameable) {
        // The declaration's name, or the name of the declaration an unnamed function
        // initializes (`const f = () => ...`).
        let mut name = None;
        let mut variable = false;
        let mut cur = Some(at).filter(|&n| !self.is_synth(n));
        for _ in 0..3 {
            let Some(n) = cur else { break };
            name = match *self.ast.node(n) {
                Node::VarDeclarator { name, .. } => {
                    variable = matches!(self.ast.node(name), Node::Ident { .. });
                    Some(name)
                }
                _ => ts_r_binder::decl_name_node(&self.ast, n),
            };
            if name.is_some()
                || !matches!(
                    self.ast.node(n),
                    Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::Paren { .. }
                )
            {
                break;
            }
            cur = self.parents.parent(n);
        }
        let span = match name {
            Some(n) => self.ast.span(n),
            None if self.is_synth(at) => Span::EMPTY,
            None => self.ast.span(at),
        };
        let decl = name.map_or("default", |n| self.raw(n));
        if let NotNameable::PrivateMember(names) = &why {
            // One TS4094 per member; on a variable, with where to add an annotation
            // (observed: related TS9027).
            let related = variable.then(|| {
                (
                    span,
                    format!("Add a type annotation to the variable {decl}."),
                )
            });
            for n in names {
                let message = format!(
                    "Property '{n}' of exported anonymous class type may not be private or protected."
                );
                if !self
                    .failed
                    .iter()
                    .any(|f| f.0 == 4094 && f.1 == span && f.2 == message)
                {
                    self.failed.push((4094, span, message, related.clone()));
                }
            }
            return;
        }
        let (code, message) = match why {
            NotNameable::CannotBeNamed { name, module } => (
                4023,
                format!(
                    "Exported variable '{decl}' has or is using name '{name}' from external module {module} but cannot be named."
                ),
            ),
            NotNameable::NonSerializableProperty(name) => (
                4118,
                format!(
                    "The type of this node cannot be serialized because its property '{name}' cannot be serialized."
                ),
            ),
            NotNameable::TooLong => (
                7056,
                "The inferred type of this node exceeds the maximum length the compiler will serialize. An explicit type annotation is needed.".to_owned(),
            ),
            NotNameable::NotPortable { name, module } => (
                2883,
                format!(
                    "The inferred type of '{decl}' cannot be named without a reference to '{name}' from '{module}'. This is likely not portable. A type annotation is necessary."
                ),
            ),
            why => {
                let text = match why {
                    NotNameable::TooDeep => "the type nests too deeply".to_owned(),
                    NotNameable::Inaccessible(n) => format!("'{n}' cannot be named here"),
                    _ => "the type is unresolved".to_owned(),
                };
                (
                    TSR_UNRESOLVED_TYPE,
                    format!(
                        "The declaration emitter cannot write this type ({text}); the file's '.d.ts' was not written."
                    ),
                )
            }
        };
        if !self.failed.iter().any(|f| f.0 == code && f.1 == span) {
            self.failed.push((code, span, message, None));
        }
    }

    // ---- references -------------------------------------------------------------------

    /// Records that printed output refers to `sym`: an alias keeps its import, a local
    /// declaration of this file is printed.
    fn reference_symbol(&mut self, sym: SymId) {
        for d in self.c.decls(sym) {
            if d.file != self.file {
                continue;
            }
            let f = self.c.sym_flags(sym);
            if f.intersects(SymbolFlags::ALIAS) {
                if self.used_aliases.insert(d.node) {
                    self.grew = true;
                }
                continue;
            }
            if let Some(stmt) = self.statement_of(d.node) {
                // The declarator itself: a local variable statement prints only the
                // declarators that are referred to.
                if d.node != stmt && self.needed.insert(d.node) {
                    self.grew = true;
                }
                self.need(stmt);
            }
        }
    }

    /// Marks a declaration statement (and the namespaces around it) as printed.
    fn need(&mut self, stmt: NodeId) {
        let mut cur = Some(stmt);
        while let Some(s) = cur {
            if matches!(self.ast.node(s), Node::Program { .. }) {
                break;
            }
            if matches!(
                self.ast.node(s),
                Node::Module { .. }
                    | Node::VarDecl { .. }
                    | Node::FunctionDecl { .. }
                    | Node::ClassDecl { .. }
                    | Node::Interface { .. }
                    | Node::TypeAlias { .. }
                    | Node::Enum { .. }
                    | Node::ImportEquals { .. }
            ) && self.needed.insert(s)
            {
                self.grew = true;
            }
            cur = self.parents.parent(s);
        }
    }

    /// The statement (in a program or module block) a declaration node belongs to.
    fn statement_of(&self, n: NodeId) -> Option<NodeId> {
        let mut cur = n;
        for _ in 0..256 {
            let p = self.parents.parent(cur)?;
            if matches!(
                self.ast.node(p),
                Node::Program { .. } | Node::ModuleBlock { .. }
            ) {
                return Some(cur);
            }
            // A dotted namespace's inner part is its own statement.
            if let Node::Module { .. } = self.ast.node(cur)
                && matches!(self.ast.node(p), Node::Module { .. })
            {
                return Some(cur);
            }
            cur = p;
        }
        None
    }

    /// Records the references of an identifier written in the source at `n` with `m`.
    fn reference_ident(&mut self, n: NodeId, m: u8) {
        if self.is_synth(n) {
            return;
        }
        if let Some(sym) = self.c.resolve_ident(self.loc(n), m) {
            // `typeof $` where the import `$` is only a type names another `$` (a global):
            // the import is not referenced (observed: typeReferenceDirectives5).
            if m == meaning::VALUE
                && self.c.sym_flags(sym).intersects(SymbolFlags::ALIAS)
                && self.c.resolve_alias(sym).is_some_and(|t| {
                    !self
                        .c
                        .sym_flags(t)
                        .intersects(SymbolFlags::VALUE | SymbolFlags::ALIAS)
                })
            {
                return;
            }
            if self.declared_in_block(sym) && self.private_name(n) {
                return;
            }
            self.reference_symbol(sym);
        }
    }

    /// Whether `sym` has a declaration in this file whose statement sits in a block, a
    /// loop head or any other place that is not a file or namespace body (`{ var a; }`,
    /// exported or not): a declaration file cannot print it, so names written from the
    /// source cannot refer to it (`tsc: tsc/internal/checker/emitresolver.go`, where the
    /// container of such a statement is never visible).
    fn declared_in_block(&self, sym: SymId) -> bool {
        if self
            .c
            .sym_flags(sym)
            .intersects(SymbolFlags::ALIAS | SymbolFlags::TYPE_PARAMETER)
        {
            return false;
        }
        self.c.decls(sym).iter().any(|d| {
            if d.file != self.file || d.node.index() >= self.orig_len {
                return false;
            }
            // From the declared name (through binding patterns) to its statement.
            let mut cur = d.node;
            let mut stmt = None;
            for _ in 0..64 {
                match self.ast.node(cur) {
                    Node::VarDeclarator { .. } => {
                        stmt = self
                            .parents
                            .parent(cur)
                            .filter(|&p| matches!(self.ast.node(p), Node::VarDecl { .. }));
                        break;
                    }
                    Node::FunctionDecl { .. }
                    | Node::ClassDecl { .. }
                    | Node::Enum { .. }
                    | Node::Interface { .. }
                    | Node::TypeAlias { .. } => {
                        stmt = Some(cur);
                        break;
                    }
                    Node::Ident { .. }
                    | Node::ObjectPattern { .. }
                    | Node::ArrayPattern { .. }
                    | Node::BindingProperty { .. }
                    | Node::AssignPattern { .. }
                    | Node::Rest { .. } => match self.parents.parent(cur) {
                        Some(p) => cur = p,
                        None => break,
                    },
                    _ => break,
                }
            }
            let Some(stmt) = stmt else {
                return false;
            };
            self.parents.parent(stmt).is_some_and(|p| {
                !matches!(
                    self.ast.node(p),
                    Node::Program { .. } | Node::ModuleBlock { .. } | Node::Module { .. }
                )
            })
        })
    }

    /// Reports the source name `n` as a private name of the declaration it is written in
    /// (TS4025 and its kin, at `n`). False when the declaration is of a kind not covered,
    /// so the caller goes on as before.
    fn private_name(&mut self, n: NodeId) -> bool {
        let name = self.raw(n);
        let mut cur = n;
        let mut from_param = None;
        let (code, message) = 'found: {
            for _ in 0..256 {
                let Some(p) = self.parents.parent(cur) else {
                    return false;
                };
                match *self.ast.node(p) {
                    Node::VarDeclarator { name: d, .. } => {
                        if !matches!(self.ast.node(d), Node::Ident { .. }) {
                            return false;
                        }
                        break 'found (
                            4025,
                            format!(
                                "Exported variable '{}' has or is using private name '{name}'.",
                                self.raw(d)
                            ),
                        );
                    }
                    Node::TypeAlias { name: d, ty, .. } if ty == cur => {
                        break 'found (
                            4081,
                            format!(
                                "Exported type alias '{}' has or is using private name '{name}'.",
                                self.raw(d)
                            ),
                        );
                    }
                    Node::Param {
                        pattern, type_ann, ..
                    } if type_ann == Some(cur) => {
                        if !matches!(self.ast.node(pattern), Node::Ident { .. }) {
                            return false;
                        }
                        from_param = Some(pattern);
                    }
                    Node::FunctionDecl { return_type, .. } => {
                        if let Some(param) = from_param {
                            break 'found (
                                4078,
                                format!(
                                    "Parameter '{}' of exported function has or is using private name '{name}'.",
                                    self.raw(param)
                                ),
                            );
                        }
                        if return_type == Some(cur) {
                            break 'found (
                                4060,
                                format!(
                                    "Return type of exported function has or is using private name '{name}'."
                                ),
                            );
                        }
                        return false;
                    }
                    Node::PropertyDef {
                        modifiers,
                        key,
                        type_ann,
                        ..
                    } if type_ann == Some(cur)
                        && matches!(self.ast.node(key), Node::Ident { .. }) =>
                    {
                        let (code, what) = if modifiers.intersects(ts_r_ast::Modifiers::STATIC) {
                            (4028, "Public static property")
                        } else {
                            (4031, "Public property")
                        };
                        break 'found (
                            code,
                            format!(
                                "{what} '{}' of exported class has or is using private name '{name}'.",
                                self.raw(key)
                            ),
                        );
                    }
                    Node::PropertySignature { key, type_ann, .. }
                        if type_ann == Some(cur)
                            && matches!(self.ast.node(key), Node::Ident { .. })
                            && self.parents.parent(p).is_some_and(|i| {
                                matches!(self.ast.node(i), Node::Interface { .. })
                            }) =>
                    {
                        break 'found (
                            4033,
                            format!(
                                "Property '{}' of exported interface has or is using private name '{name}'.",
                                self.raw(key)
                            ),
                        );
                    }
                    Node::Program { .. }
                    | Node::ModuleBlock { .. }
                    | Node::Block { .. }
                    | Node::ClassDecl { .. }
                    | Node::ClassExpr { .. }
                    | Node::Interface { .. }
                    | Node::Arrow { .. }
                    | Node::FunctionExpr { .. }
                    | Node::MethodDef { .. } => return false,
                    _ => {}
                }
                cur = p;
            }
            return false;
        };
        let span = self.ast.span(n);
        if !self.failed.iter().any(|f| f.0 == code && f.1 == span) {
            self.failed.push((code, span, message, None));
        }
        true
    }

    /// Records the references of an entity name (`A.B.C`; the leftmost identifier).
    fn reference_entity(&mut self, n: NodeId, m: u8) {
        match *self.ast.node(n) {
            Node::Ident { .. } => self.reference_ident(n, m),
            Node::QualifiedName { left, .. } | Node::Member { obj: left, .. } => {
                self.reference_entity(left, meaning::NAMESPACE | meaning::VALUE)
            }
            _ => {}
        }
    }

    // ---- comments ---------------------------------------------------------------------

    /// JSDoc comments (`/** ... */`) before `n` (after `prev_end`). They keep their
    /// source lines: comments on one line stay on one line, and a comment on the line where
    /// the previous (written) declaration ends follows it there.
    fn leading_jsdoc(&mut self, n: NodeId, prev_end: u32) {
        if self.is_synth(n) {
            return;
        }
        self.leading_comments(n, prev_end);
        // The declaration maps its start.
        self.start(n);
    }

    /// The JSDoc comments of [`Emitter::leading_jsdoc`] without the declaration's start
    /// mark (for declarations tsc rebuilds).
    fn leading_comments(&mut self, n: NodeId, prev_end: u32) {
        if self.is_synth(n) {
            return;
        }
        let span = self.ast.span(n);
        let start = span.start;
        let prev_written = self.last_written_end == Some(prev_end);
        self.last_written_end = Some(span.end);
        if self.opts.remove_comments {
            return;
        }
        let comments: Vec<_> = self.ast.comments_between(prev_end, start).to_vec();
        let mut last_end: Option<u32> = None;
        for c in comments {
            if c.kind != CommentKind::Block {
                continue;
            }
            let text = &self.src[c.span.start as usize..c.span.end as usize];
            if !text.starts_with("/**") || text == "/**/" {
                continue;
            }
            let from = last_end.unwrap_or(prev_end);
            let same_line = !self
                .src
                .get(from as usize..c.span.start as usize)
                .is_some_and(|t| t.contains('\n'));
            match last_end {
                Some(_) if same_line => {
                    self.w(" ");
                }
                Some(_) => self.nl(),
                None if same_line && prev_written && self.out.ends_with('\n') => {
                    // Back onto the previous declaration's line.
                    let nl = self.opts.newline.len();
                    self.out.truncate(self.out.len() - nl);
                    self.line_start = false;
                    self.w(" ");
                }
                None => {}
            }
            self.write_comment(text, c.span.start);
            last_end = Some(c.span.end);
        }
        if let Some(e) = last_end {
            // A comment on the declaration's own line stays on it.
            let same_line = !self
                .src
                .get(e as usize..start as usize)
                .is_some_and(|t| t.contains('\n'));
            if same_line {
                self.w(" ");
            } else {
                self.nl();
            }
        }
    }

    /// The JSDoc comments of object literal member `decl` (from the source) before the
    /// synthesized member written for it (tsc copies them onto members of object literal
    /// types): on their own line, or on the member's line when the source has them on the
    /// declaration's line.
    fn object_member_jsdoc(&mut self, decl: NodeId) {
        if self.opts.remove_comments {
            return;
        }
        let start = self.ast.span(decl).start;
        let Some(obj) = self.parents.parent(decl) else {
            return;
        };
        let (Node::ObjectLit { props: list } | Node::ClassExpr { members: list, .. }) =
            *self.ast.node(obj)
        else {
            return;
        };
        let list = self.ast.list(list);
        let prev_end = match list.iter().position(|&p| p == decl) {
            Some(0) | None => self.ast.span(obj).start + 1,
            Some(i) => self.ast.span(list[i - 1]).end,
        };
        // Leading comments start on a line after the previous token (a comment on its
        // line, as in `{ /** c */ a: 1 }`, is that token's trailing comment).
        let Some(line) = self
            .src
            .get(prev_end as usize..start as usize)
            .and_then(|t| t.find('\n'))
        else {
            return;
        };
        let from = prev_end + line as u32;
        let comments: Vec<_> = self.ast.comments_between(from, start).to_vec();
        let mut last_end: Option<u32> = None;
        for c in comments {
            if c.kind != CommentKind::Block {
                continue;
            }
            let text = &self.src[c.span.start as usize..c.span.end as usize];
            if !text.starts_with("/**") || text == "/**/" {
                continue;
            }
            if let Some(e) = last_end {
                let same_line = !self
                    .src
                    .get(e as usize..c.span.start as usize)
                    .is_some_and(|t| t.contains('\n'));
                if same_line {
                    self.w(" ");
                } else {
                    self.nl();
                }
            }
            self.write_comment(text, c.span.start);
            last_end = Some(c.span.end);
        }
        if let Some(e) = last_end {
            let same_line = !self
                .src
                .get(e as usize..start as usize)
                .is_some_and(|t| t.contains('\n'));
            if same_line {
                self.w(" ");
            } else {
                self.nl();
            }
        }
    }

    /// `stripInternal`: whether the last comment before `n` (after `prev_end`) says
    /// `@internal` (any comment kind, as tsc reads it).
    fn is_internal(&self, n: NodeId, prev_end: u32) -> bool {
        if !self.opts.strip_internal || self.is_synth(n) {
            return false;
        }
        let start = self.ast.span(n).start;
        // Observed: any leading comment (block or line, JSDoc or not) marks it; comments on
        // the line the previous token ends on are that token's trailing comments.
        let from = if prev_end == 0 {
            0
        } else {
            match self
                .src
                .get(prev_end as usize..start as usize)
                .and_then(|t| t.find('\n'))
            {
                Some(i) => prev_end + i as u32,
                None => return false,
            }
        };
        self.ast.comments_between(from, start).iter().any(|c| {
            self.src
                .get(c.span.start as usize..c.span.end as usize)
                .is_some_and(|t| t.contains("@internal"))
        })
    }

    /// Writes a (possibly multi-line) block comment re-indented to the current level.
    pub(super) fn write_comment(&mut self, text: &str, start: u32) {
        self.mark(start);
        self.write_comment_text(text, start);
        self.mark(start + text.len() as u32);
    }

    fn write_comment_text(&mut self, text: &str, start: u32) {
        let col = self.column_of(start);
        let multi_line = text.contains('\n');
        let mut first = true;
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if first {
                self.w(if multi_line { line.trim_end() } else { line });
                first = false;
                continue;
            }
            self.nl();
            // Remove the source indentation up to the comment's own column.
            let trimmed = strip_columns(line, col);
            self.w(trimmed.trim_end());
        }
    }

    fn column_of(&self, pos: u32) -> usize {
        let before = &self.src[..pos as usize];
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let mut col = 0;
        for ch in before[line_start..].chars() {
            col = if ch == '\t' {
                (col / 4 + 1) * 4
            } else {
                col + 1
            };
        }
        col
    }

    /// `/*! ... */` and `/// <reference ...>` comments at the top of the file, and its
    /// detached JSDoc comments (the comments up to the last one a blank line separates
    /// from what follows): `/** Copyright header */` stays even when the first statement
    /// is elided (observed).
    fn file_header(&mut self, stmts: &[NodeId]) {
        let end = stmts
            .first()
            .map_or(self.src.len() as u32, |&s| self.ast.span(s).start);
        let comments: Vec<_> = self.ast.comments_between(0, end).to_vec();
        // The hashbang line stays (observed, unmapped).
        if let Some(c) = self.ast.comments.first()
            && c.kind == CommentKind::Hashbang
        {
            let text = self.src[c.span.start as usize..c.span.end as usize]
                .trim_end()
                .to_owned();
            self.w(&text);
            self.nl();
        }
        let detached = comments
            .iter()
            .enumerate()
            .rev()
            .find(|&(i, c)| {
                let next = comments.get(i + 1).map_or(end, |n| n.span.start);
                self.src
                    .get(c.span.end as usize..next as usize)
                    .is_some_and(|t| t.matches('\n').count() >= 2)
            })
            .map_or(0, |(i, _)| i + 1);
        self.header_end = comments
            .get(detached.wrapping_sub(1))
            .map_or(0, |c| c.span.end);
        for (i, c) in comments.into_iter().enumerate() {
            let text = &self.src[c.span.start as usize..c.span.end as usize];
            if c.kind == CommentKind::Line && text.starts_with("///") {
                // Only `preserve="true"` references are kept (tsc 7.0.2), re-quoted, with
                // a source path pointing at its declaration output.
                if let Some(line) = preserved_reference(text) {
                    self.w(&line);
                    self.nl();
                }
                continue;
            }
            if c.kind != CommentKind::Block {
                continue;
            }
            let keep = text.starts_with("/*!")
                || (i < detached
                    && !self.opts.remove_comments
                    && text.starts_with("/**")
                    && text != "/**/");
            if keep {
                self.write_comment(text, c.span.start);
                self.nl();
            }
        }
    }

    // ---- statements -------------------------------------------------------------------

    /// Prints the statements of a list. `container` is the namespace body (`{ ... }`) they
    /// are in (comments before it belong to its namespace).
    fn statements(&mut self, stmts: &[NodeId], scope: Scope, container: Option<NodeId>) {
        // In a namespace with a printed non-exported member (or a module with one and no
        // export declaration), `export` modifiers are kept and `export {};` closes it.
        let mut printed_local = false;
        let mut has_export_decl = false;
        let mut has_module_syntax = false;
        let mut prev_end = container.map_or(self.header_end, |b| self.ast.span(b).start + 1);
        let keep_export =
            matches!(scope, Scope::ModuleTop) || self.namespace_keeps_export(stmts, scope);
        for (i, &s) in stmts.iter().enumerate() {
            let start_len = self.out.len();
            match self.ast.node(s) {
                Node::ExportNamed { .. }
                | Node::ExportAssignment { .. }
                | Node::ExportDefaultExpr { .. } => has_export_decl = true,
                _ => {}
            }
            let emitted = !self.is_internal(s, prev_end)
                && self.statement(s, stmts, i, scope, keep_export, prev_end);
            if emitted {
                let mods = self.ast.node(s).modifiers();
                let is_export_stmt = matches!(
                    self.ast.node(s),
                    Node::ImportDecl { .. }
                        | Node::ImportEquals { .. }
                        | Node::ExportNamed { .. }
                        | Node::ExportAll { .. }
                        | Node::ExportAssignment { .. }
                        | Node::ExportDefaultExpr { .. }
                        | Node::NamespaceExport { .. }
                );
                let augmentation = matches!(*self.ast.node(s), Node::Module { kind, name, .. }
                    if kind == ModuleKind::Global
                        || matches!(self.ast.node(name), Node::StringLit { .. }));
                if is_export_stmt || mods.contains(Modifiers::EXPORT) {
                    has_module_syntax = true;
                } else if self.declares(s) && !augmentation {
                    printed_local = true;
                }
                if self.base_expr(s).is_some() {
                    printed_local = true;
                }
            }
            let _ = start_len;
            prev_end = self.ast.span(s).end;
        }
        if self.opts.isolated_declarations {
            let written: Vec<NodeId> = stmts
                .iter()
                .copied()
                .filter(|&s| self.visible(s, scope))
                .collect();
            self.iso_expandos(stmts, &written);
        }
        match scope {
            Scope::ModuleTop => {
                if (printed_local || !has_module_syntax) && !has_export_decl {
                    self.w("export {};");
                    self.nl();
                }
            }
            Scope::Namespace => {
                if keep_export && !has_export_decl {
                    self.w("export {};");
                    self.nl();
                }
            }
            _ => {}
        }
    }

    /// JSDoc comments after the file's last statement (observed: kept at the end of the
    /// declaration file).
    fn eof_comments(&mut self, stmts: &[NodeId]) {
        if self.opts.remove_comments {
            return;
        }
        let from = stmts
            .last()
            .map_or(self.header_end, |&s| self.ast.span(s).end);
        let comments: Vec<_> = self
            .ast
            .comments_between(from, self.src.len() as u32)
            .to_vec();
        for c in comments {
            if c.kind != CommentKind::Block {
                continue;
            }
            let text = self.src[c.span.start as usize..c.span.end as usize].to_owned();
            if !text.starts_with("/**") || text == "/**/" {
                continue;
            }
            self.write_comment(&text, c.span.start);
            // A comment with no line break after it is followed by a space (observed).
            let rest = self.src[c.span.end as usize..].trim_start_matches([' ', '\t']);
            if !rest.starts_with(['\n', '\r']) {
                self.w(" ");
            }
            self.nl();
        }
    }

    /// A declaration statement (not an import or export statement).
    fn declares(&self, s: NodeId) -> bool {
        matches!(
            self.ast.node(s),
            Node::VarDecl { .. }
                | Node::FunctionDecl { .. }
                | Node::ClassDecl { .. }
                | Node::Interface { .. }
                | Node::TypeAlias { .. }
                | Node::Enum { .. }
                | Node::Module { .. }
        )
    }

    /// Whether a namespace body prints a non-exported member (then `export` stays).
    fn namespace_keeps_export(&self, stmts: &[NodeId], scope: Scope) -> bool {
        // An ambient module body with an export declaration (a scope marker such as
        // `export {};`) keeps its `export` modifiers.
        if scope == Scope::Ambient {
            return stmts.iter().any(|&s| {
                matches!(
                    self.ast.node(s),
                    Node::ExportNamed { .. }
                        | Node::ExportAssignment { .. }
                        | Node::ExportDefaultExpr { .. }
                )
            });
        }
        if scope != Scope::Namespace {
            return false;
        }
        stmts.iter().any(|&s| {
            let printed = self.ast.node(s).modifiers().contains(Modifiers::EXPORT)
                || self.needed.contains(&s);
            printed
                && self.declares(s)
                && (!self.ast.node(s).modifiers().contains(Modifiers::EXPORT)
                    || self.base_expr(s).is_some())
        })
    }

    /// The `extends` expression of a class declaration that is not an entity name (it is
    /// written as a `X_base` constant).
    fn base_expr(&self, s: NodeId) -> Option<NodeId> {
        let Node::ClassDecl { heritage, .. } = *self.ast.node(s) else {
            return None;
        };
        for &h in self.ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *self.ast.node(h)
                && let Some(&first) = self.ast.list(types).first()
                && let Node::ExprWithTypeArgs { expr, .. } = *self.ast.node(first)
                && !is_entity_expr(&self.ast, skip_parens(&self.ast, expr))
                && !matches!(self.ast.node(expr), Node::NullLit {})
            {
                return Some(expr);
            }
        }
        None
    }

    fn visible(&self, s: NodeId, scope: Scope) -> bool {
        match scope {
            Scope::ScriptTop | Scope::Ambient => true,
            Scope::ModuleTop | Scope::Namespace => {
                self.ast.node(s).modifiers().contains(Modifiers::EXPORT) || self.needed.contains(&s)
            }
        }
    }

    /// Writes `export ` / `declare ` as tsc places them for a declaration statement, and
    /// then `abstract ` (classes) and `const ` (enums).
    fn decl_prefix(&mut self, s: NodeId, scope: Scope, keep_export: bool, declare_kw: bool) {
        let mods = self.ast.node(s).modifiers();
        let mut words: Vec<(Modifiers, &str)> = Vec::new();
        if mods.contains(Modifiers::DEFAULT) {
            words.push((Modifiers::EXPORT, "export"));
            words.push((Modifiers::DEFAULT, "default"));
        } else {
            if mods.contains(Modifiers::EXPORT) && (scope.top() || keep_export) {
                words.push((Modifiers::EXPORT, "export"));
            }
            if declare_kw && scope.top() {
                words.push((Modifiers::DECLARE, "declare"));
            }
        }
        match self.ast.node(s) {
            Node::ClassDecl { .. } if mods.contains(Modifiers::ABSTRACT) => {
                words.push((Modifiers::ABSTRACT, "abstract"));
            }
            Node::Enum { .. } if mods.contains(Modifiers::CONST) => {
                words.push((Modifiers::CONST, "const"));
            }
            _ => {}
        }
        self.modifiers(s, &words);
    }

    /// Prints one statement; returns whether anything was printed.
    fn statement(
        &mut self,
        s: NodeId,
        list: &[NodeId],
        idx: usize,
        scope: Scope,
        keep_export: bool,
        prev_end: u32,
    ) -> bool {
        let node = *self.ast.node(s);
        match node {
            Node::ImportDecl { .. } => self.import_decl(s, scope, prev_end),
            Node::ImportEquals {
                modifiers,
                type_only,
                name,
                module_ref,
            } => {
                let exported = modifiers.contains(Modifiers::EXPORT);
                if !exported && !self.used_aliases.contains(&s) && !self.needed.contains(&s) {
                    // Kept at the top of a script as written; dropped when unused in
                    // module and namespace bodies (ambient ones too).
                    if scope != Scope::ScriptTop {
                        return false;
                    }
                }
                self.leading_jsdoc(s, prev_end);
                let words: &[(Modifiers, &str)] = if exported {
                    &[(Modifiers::EXPORT, "export")]
                } else {
                    &[]
                };
                self.modifiers(s, words);
                self.w("import ");
                if type_only {
                    self.w("type ");
                }
                self.name_text(name);
                self.w(" = ");
                match *self.ast.node(module_ref) {
                    Node::ExternalModuleRef { expr } => {
                        self.start(module_ref);
                        self.w("require(");
                        self.name_text(expr);
                        self.w(")");
                        self.end(module_ref);
                    }
                    _ => {
                        self.reference_entity(module_ref, meaning::NAMESPACE | meaning::VALUE);
                        self.entity_name(module_ref);
                    }
                }
                self.semi_end(s);
                true
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                attributes,
            } => {
                self.leading_jsdoc(s, prev_end);
                self.w("export ");
                if type_only {
                    self.w("type ");
                }
                let specs = self.ast.list(specifiers).to_vec();
                let (open, close) = self.braces(s, &specs);
                self.mark_opt(open);
                if specs.is_empty() {
                    self.w("{}");
                } else {
                    self.w("{ ");
                    for (i, &sp) in specs.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.specifier_comments(sp);
                        if let Node::ExportSpecifier {
                            type_only,
                            local,
                            exported,
                        } = *self.ast.node(sp)
                        {
                            self.start(sp);
                            if type_only {
                                self.w("type ");
                            }
                            if source.is_none() {
                                self.reference_ident(local, meaning::ALL);
                            }
                            self.name_text(local);
                            if let Some(e) = exported {
                                self.w(" as ");
                                self.name_text(e);
                            }
                            self.end(sp);
                        }
                    }
                    // An export list keeps the source's trailing comma; an import list,
                    // rebuilt from the used specifiers, does not (observed).
                    if self.trailing_comma(&specs, close) {
                        self.w(",");
                    }
                    self.w(" }");
                }
                self.mark_opt(close);
                if let Some(src) = source {
                    self.w(" from ");
                    self.name_text(src);
                }
                self.import_attributes(attributes);
                self.semi_end(s);
                true
            }
            Node::ExportAll {
                type_only,
                exported,
                source,
                attributes,
            } => {
                self.leading_jsdoc(s, prev_end);
                self.w("export ");
                if type_only {
                    self.w("type ");
                }
                if let Some(e) = exported {
                    // `* as x` is a node of its own.
                    let (from, to) = (self.ast.span(s).start, self.ast.span(e).start);
                    let star = self.src[from as usize..to as usize]
                        .find('*')
                        .map(|i| from + i as u32);
                    self.mark_opt(star);
                    self.w("* as ");
                    self.name_text(e);
                } else {
                    self.w("*");
                }
                self.w(" from ");
                self.name_text(source);
                self.import_attributes(attributes);
                self.semi_end(s);
                true
            }
            // tsc rebuilds these statements: only the expression maps.
            Node::ExportAssignment { expr } => {
                if self.export_function(s, expr, "export = ", prev_end) {
                    return true;
                }
                self.leading_comments(s, prev_end);
                self.export_value(s, expr, "export = ")
            }
            Node::ExportDefaultExpr { expr } => {
                if self.export_function(s, expr, "export default ", prev_end) {
                    return true;
                }
                self.leading_comments(s, prev_end);
                self.export_value(s, expr, "export default ")
            }
            Node::NamespaceExport { name } => {
                self.start(s);
                self.w("export as namespace ");
                self.name_text(name);
                self.semi_end(s);
                true
            }
            Node::VarDecl { .. } => {
                if !self.visible(s, scope) {
                    return false;
                }
                self.var_statement(s, scope, keep_export, prev_end)
            }
            Node::FunctionDecl { name, body, .. } => {
                if !self.visible(s, scope) {
                    return false;
                }
                // An implementation after overload signatures is not printed.
                if body.is_some() && self.has_overload_before(list, idx, name) {
                    return false;
                }
                // Expando properties: a namespace after the first signature of a function
                // (not of a namespace member printed only because it is used).
                let by_scope = scope != Scope::Namespace
                    || self.ast.node(s).modifiers().contains(Modifiers::EXPORT);
                let expando = if by_scope && !self.has_overload_before(list, idx, name) {
                    self.expando_function(s)
                } else {
                    None
                };
                self.leading_jsdoc(s, prev_end);
                let default = self.ast.node(s).modifiers().contains(Modifiers::DEFAULT);
                if expando.is_some() && default {
                    // `declare function f(): T; export default f;` and the namespace.
                    if scope.top() {
                        self.w("declare ");
                    }
                    self.function_decl(s);
                    self.w("export default ");
                    self.w(name.map_or("", |n| self.raw(n)));
                    self.w(";");
                    self.nl();
                } else {
                    self.decl_prefix(s, scope, keep_export, true);
                    self.function_decl(s);
                }
                if let (Some(sym), Some(n)) = (expando, name) {
                    let prefix = self.expando_prefix(s, scope, keep_export);
                    self.expando_namespace(s, sym, self.raw(n), &prefix);
                }
                true
            }
            Node::ClassDecl { .. } => {
                if !self.visible(s, scope) {
                    return false;
                }
                self.class_decl(s, scope, keep_export, prev_end);
                true
            }
            Node::Interface {
                name,
                type_params,
                extends,
                body,
                ..
            } => {
                if !self.visible(s, scope) {
                    return false;
                }
                self.leading_jsdoc(s, prev_end);
                // An explicit `declare` is kept on interfaces and type aliases.
                let declare = self.ast.node(s).modifiers().contains(Modifiers::DECLARE);
                self.decl_prefix(s, scope, keep_export, declare);
                self.w("interface ");
                self.name_text(name);
                self.type_params(type_params);
                // Heritage that is not an entity name (an error) is dropped.
                let ext: Vec<NodeId> = self
                    .ast
                    .list(extends)
                    .iter()
                    .copied()
                    .filter(|&h| match *self.ast.node(h) {
                        Node::ExprWithTypeArgs { expr, .. } => is_entity_expr(&self.ast, expr),
                        _ => true,
                    })
                    .collect();
                if !ext.is_empty() {
                    // The `extends` clause is a node from its keyword (its mark precedes
                    // the space tsc writes before it).
                    let first = self.ast.list(extends).first().copied();
                    let kw = first.and_then(|f| {
                        let at = self.ast.span(f).start as usize;
                        self.src[..at].rfind("extends").map(|i| i as u32)
                    });
                    self.mark_opt(kw);
                    self.w(" extends ");
                    for (i, &h) in ext.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.heritage_type(h);
                    }
                    if let Some(&last) = self.ast.list(extends).last() {
                        self.end(last);
                    }
                }
                self.w(" {");
                self.nl();
                self.indent += 1;
                let members = self.ast.list(body).to_vec();
                self.type_members(&members, self.body_open(s));
                self.indent -= 1;
                self.w("}");
                self.end(s);
                self.nl();
                true
            }
            Node::TypeAlias {
                name,
                type_params,
                ty,
                ..
            } => {
                if !self.visible(s, scope) {
                    return false;
                }
                self.leading_jsdoc(s, prev_end);
                let declare = self.ast.node(s).modifiers().contains(Modifiers::DECLARE);
                self.decl_prefix(s, scope, keep_export, declare);
                self.w("type ");
                self.name_text(name);
                self.type_params(type_params);
                self.w(" = ");
                self.type_node(ty);
                self.semi_end(s);
                true
            }
            Node::Enum {
                modifiers,
                name,
                members,
            } => {
                if !self.visible(s, scope) {
                    return false;
                }
                self.leading_jsdoc(s, prev_end);
                self.decl_prefix(s, scope, keep_export, true);
                self.w("enum ");
                self.name_text(name);
                self.w(" {");
                self.nl();
                self.indent += 1;
                let ms = self.ast.list(members).to_vec();
                // Members of an ambient, non-const enum without an initializer have no
                // known value.
                let ambient = (modifiers.contains(Modifiers::DECLARE)
                    || scope == Scope::Ambient
                    || self.c.file(self.file).is_declaration())
                    && !modifiers.contains(Modifiers::CONST);
                self.enum_members(s, &ms, ambient);
                self.indent -= 1;
                self.w("}");
                self.end(s);
                self.nl();
                true
            }
            Node::Module { .. } => {
                if !self.visible(s, scope) && !self.module_always_visible(s) {
                    return false;
                }
                self.leading_jsdoc(s, prev_end);
                self.module_decl(s, scope, keep_export);
                true
            }
            _ => false,
        }
    }

    fn module_always_visible(&self, s: NodeId) -> bool {
        match *self.ast.node(s) {
            Node::Module { kind, name, .. } => {
                kind == ModuleKind::Global || matches!(self.ast.node(name), Node::StringLit { .. })
            }
            _ => false,
        }
    }

    fn has_overload_before(&self, list: &[NodeId], idx: usize, name: Option<NodeId>) -> bool {
        let Some(name) = name else {
            // `export default function` overloads.
            return idx > 0
                && matches!(
                    self.ast.node(list[idx - 1]),
                    Node::FunctionDecl {
                        name: None,
                        body: None,
                        ..
                    }
                );
        };
        let text = self.raw(name);
        list[..idx].iter().any(|&o| match *self.ast.node(o) {
            Node::FunctionDecl {
                name: Some(n),
                body: None,
                ..
            } => self.raw(n) == text,
            _ => false,
        })
    }

    fn import_decl(&mut self, s: NodeId, scope: Scope, prev_end: u32) -> bool {
        let Node::ImportDecl {
            type_only,
            phase: _,
            has_clause,
            specifiers,
            source,
            attributes,
        } = *self.ast.node(s)
        else {
            return false;
        };
        let _ = scope;
        let specs = self.ast.list(specifiers).to_vec();
        if !has_clause {
            self.leading_jsdoc(s, prev_end);
            self.w("import ");
            self.name_text(source);
            self.import_attributes(attributes);
            self.semi_end(s);
            return true;
        }
        let used: Vec<NodeId> = specs
            .iter()
            .copied()
            .filter(|&sp| self.used_aliases.contains(&sp))
            .collect();
        if used.is_empty() {
            // An import of a module that augments others stays as `import "m";`.
            if self.imports_augmenting_module(&specs) {
                // Keeping the bare import needs the other file: refused under
                // isolatedDeclarations (TS9026 at the import).
                if self.iso_on(s) {
                    self.iso_error(9026, s);
                }
                self.leading_jsdoc(s, prev_end);
                self.w("import ");
                self.name_text(source);
                self.import_attributes(attributes);
                self.semi_end(s);
                return true;
            }
            return false;
        }
        self.leading_jsdoc(s, prev_end);
        self.w("import ");
        // The import clause starts at the token after `import`.
        let clause = self.next_token(self.ast.span(s).start + "import".len() as u32);
        self.mark(clause);
        if type_only {
            self.w("type ");
        }
        let mut wrote_default = false;
        let mut named = Vec::new();
        for &sp in &used {
            match *self.ast.node(sp) {
                Node::ImportDefaultSpecifier { local } => {
                    self.name_text(local);
                    wrote_default = true;
                }
                Node::ImportNamespaceSpecifier { local } => {
                    if wrote_default {
                        self.w(", ");
                    }
                    self.start(sp);
                    self.w("* as ");
                    self.name_text(local);
                    self.end(sp);
                    wrote_default = true;
                }
                Node::ImportSpecifier { .. } => named.push(sp),
                _ => {}
            }
        }
        if !named.is_empty() {
            if wrote_default {
                self.w(", ");
            }
            let all_named: Vec<NodeId> = specs
                .iter()
                .copied()
                .filter(|&sp| matches!(self.ast.node(sp), Node::ImportSpecifier { .. }))
                .collect();
            let (open, close) = self.braces(s, &all_named);
            self.mark_opt(open);
            self.w("{ ");
            for (i, &sp) in named.iter().enumerate() {
                if i > 0 {
                    self.w(", ");
                }
                self.specifier_comments(sp);
                if let Node::ImportSpecifier {
                    type_only,
                    imported,
                    local,
                } = *self.ast.node(sp)
                {
                    self.start(sp);
                    if type_only {
                        self.w("type ");
                    }
                    if let Some(im) = imported {
                        self.name_text(im);
                        self.w(" as ");
                    }
                    self.name_text(local);
                    self.end(sp);
                }
            }
            self.w(" }");
            self.mark_opt(close);
        }
        self.w(" from ");
        self.name_text(source);
        self.import_attributes(attributes);
        self.semi_end(s);
        true
    }

    /// Whether the file an import's specifiers come from augments this file's module
    /// (`declare module "./this" { }`). Observed: `declare global { }` and augmentations
    /// of other modules do not keep the import.
    fn imports_augmenting_module(&self, specs: &[NodeId]) -> bool {
        let Some(file) = specs.iter().find_map(|&sp| {
            let alias = self.c.sym_of_decl(self.loc(sp))?;
            let target = self.c.resolve_alias(alias)?;
            Some(self.c.first_decl(target)?.file)
        }) else {
            return false;
        };
        if file == self.file {
            return false;
        }
        let f = self.c.file(file);
        if !f.bound.is_module {
            return false;
        }
        let Node::Program { stmts } = *f.ast.node(f.ast.root) else {
            return false;
        };
        f.ast.list(stmts).iter().any(|&s| {
            matches!(*f.ast.node(s), Node::Module { kind, name, .. }
                if kind != ModuleKind::Global && matches!(f.ast.node(name), Node::StringLit { .. }))
                && self
                    .c
                    .sym_of_decl(Loc::new(file, s))
                    .and_then(|m| self.c.module_target_of(m))
                    == Some(ts_r_binder::program::ModuleTarget::File(self.file))
        })
    }

    /// Import attributes of an import or export declaration: only `resolution-mode` ones
    /// are kept (tsc 7.0.2 drops `with { type: "json" }`).
    fn import_attributes(&mut self, attributes: Option<NodeId>) {
        // Kept only with a valid `resolution-mode` (`"import"` / `"require"`).
        let valid = |e: &Self, entry: NodeId| match *e.ast.node(entry) {
            Node::ImportAttribute { key, value } => {
                let k = e.raw(key).trim_matches(['"', '\'']);
                let v = e.raw(value).trim_matches(['"', '\'']);
                k == "resolution-mode" && (v == "import" || v == "require")
            }
            _ => false,
        };
        if let Some(a) = attributes
            && let Node::ImportAttributes { entries, .. } = *self.ast.node(a)
            && self.ast.list(entries).iter().any(|&en| valid(self, en))
        {
            self.w(" ");
            self.w(self.raw(a));
        }
    }

    /// `export default <function or arrow expression>` (or `export =`): tsc 7 writes the
    /// export first, then a function declaration named by the function expression's name
    /// or `_default`, with the statement's comments. Only the parameters map (observed).
    fn export_function(&mut self, s: NodeId, expr: NodeId, prefix: &str, prev_end: u32) -> bool {
        let f = skip_parens(&self.ast, expr);
        let name = match *self.ast.node(f) {
            Node::FunctionExpr { name: Some(n), .. } => {
                let name = self.ast.name_of(n).unwrap_or(self.raw(n)).to_owned();
                if self
                    .ck
                    .resolve_name_at(self.loc(s), &name, meaning::ALL)
                    .is_some()
                    || self.generated.contains(&name)
                {
                    self.unique_name(&name)
                } else {
                    self.generated.insert(name);
                    self.raw(n).to_owned()
                }
            }
            Node::FunctionExpr { name: None, .. } | Node::Arrow { .. } => {
                self.unique_name("_default")
            }
            _ => return false,
        };
        if prefix.starts_with("export default") {
            self.iso_default(expr);
        }
        self.w(prefix);
        self.w(&name);
        self.w(";");
        self.nl();
        self.leading_comments(s, prev_end);
        self.w("declare ");
        self.function_from_expr(&name, f);
        true
    }

    /// `export = x;` / `export default x;`, or a `_default` constant for an expression.
    fn export_value(&mut self, s: NodeId, expr: NodeId, prefix: &str) -> bool {
        let inner = skip_parens(&self.ast, expr);
        // `export default a.b` writes a `_default` constant; `export = a.b` stays.
        let entity = if prefix.starts_with("export default") {
            matches!(self.ast.node(inner), Node::Ident { .. })
        } else {
            is_entity_expr(&self.ast, inner)
        };
        if entity {
            self.reference_entity(inner, meaning::ALL);
            self.w(prefix);
            self.entity_name(inner);
            self.w(";");
            self.nl();
            return true;
        }
        if prefix.starts_with("export default") {
            self.iso_default(expr);
        }
        let name = self.unique_name("_default");
        self.w("declare const ");
        self.w(&name);
        let t = self.ck.check_expr(self.loc(expr));
        if let Some(text) = self.literal_initializer(t) {
            self.w(" = ");
            self.w(&text);
        } else {
            self.w(": ");
            if !self.reuse_expr_type(expr) {
                let t = self.ck.widened_type(t);
                self.inferred_type(t, s);
            }
        }
        self.w(";");
        self.nl();
        self.w(prefix);
        self.w(&name);
        self.w(";");
        self.nl();
        true
    }

    fn var_statement(&mut self, s: NodeId, scope: Scope, keep_export: bool, prev_end: u32) -> bool {
        let Node::VarDecl { kind, decls, .. } = *self.ast.node(s) else {
            return false;
        };
        let decls = self.ast.list(decls).to_vec();
        // Declarators to print: binding patterns expand to their names.
        let mut items: Vec<NodeId> = Vec::new();
        for &d in &decls {
            if let Node::VarDeclarator { name, .. } = *self.ast.node(d) {
                if matches!(self.ast.node(name), Node::Ident { .. }) {
                    items.push(d);
                } else {
                    collect_binding_names(&self.ast, name, &mut items);
                }
            }
        }
        // A local statement (printed because a name refers to it) keeps only the
        // declarators referred to.
        if matches!(scope, Scope::ModuleTop | Scope::Namespace)
            && !self.ast.node(s).modifiers().contains(Modifiers::EXPORT)
        {
            let kept: Vec<NodeId> = items
                .iter()
                .copied()
                .filter(|&d| {
                    match *self.ast.node(d) {
                        Node::VarDeclarator { name, .. } => {
                            self.needed.contains(&d) || self.needed.contains(&name)
                        }
                        // A name in a binding pattern (or its binding element).
                        _ => {
                            self.needed.contains(&d)
                                || self.parents.parent(d).is_some_and(|p| {
                                    p != s
                                        && !matches!(self.ast.node(p), Node::VarDeclarator { .. })
                                        && self.needed.contains(&p)
                                })
                        }
                    }
                })
                .collect();
            if !kept.is_empty() {
                items = kept;
            }
        }
        if items.is_empty() {
            return false;
        }
        // A `const` function with expando properties is a function and a namespace.
        let by_scope =
            scope != Scope::Namespace || self.ast.node(s).modifiers().contains(Modifiers::EXPORT);
        if by_scope && let Some((name, f, sym)) = self.expando_const(s) {
            // The function declaration is tsc's own: its start maps nothing (observed).
            self.leading_comments(s, prev_end);
            self.decl_prefix(s, scope, keep_export, true);
            let text = self.raw(name).to_owned();
            self.function_from_expr(&text, f);
            let prefix = self.expando_prefix(s, scope, keep_export);
            self.expando_namespace(s, sym, self.raw(name), &prefix);
            return true;
        }
        self.leading_jsdoc(s, prev_end);
        self.decl_prefix(s, scope, keep_export, true);
        // The declaration list starts at its keyword.
        if !self.is_synth(s) {
            let (_, list) = self.modifier_tokens(s);
            self.mark(list);
        }
        self.w(match kind {
            VarKind::Var => "var ",
            VarKind::Let => "let ",
            VarKind::Const | VarKind::Using | VarKind::AwaitUsing => "const ",
        });
        let is_const = matches!(kind, VarKind::Const | VarKind::Using | VarKind::AwaitUsing);
        let mut from = self.ast.span(s).start;
        for (i, &d) in items.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            match *self.ast.node(d) {
                Node::VarDeclarator {
                    name,
                    type_ann,
                    init,
                    ..
                } => {
                    if !self.opts.remove_comments {
                        self.declarator_comments(from, self.ast.span(name).start);
                    }
                    from = self.ast.span(d).end;
                    self.start(d);
                    self.name_text(name);
                    let fresh = type_ann
                        .filter(|_| is_const && init.is_some())
                        .and_then(|t| self.fresh_literal_of(d, name, t));
                    match (type_ann, fresh) {
                        (Some(_), Some(text)) => {
                            self.w(" = ");
                            self.w(&text);
                        }
                        (Some(t), None) => {
                            self.w(": ");
                            self.type_node(t);
                        }
                        (None, _) => {
                            self.iso_variable(d, !is_const);
                            self.inferred_var(d, name, init.is_some() && is_const);
                        }
                    }
                    self.end(d);
                }
                Node::Ident { .. } => {
                    // A name bound by a destructuring pattern (tsc writes a declaration of
                    // its own: only the name maps).
                    self.iso_variable(d, !is_const);
                    self.name_text(d);
                    self.inferred_var(d, d, false);
                }
                _ => {}
            }
        }
        // The end of the declaration list.
        if let Some(&last) = decls.last() {
            self.end(last);
        }
        self.semi_end(s);
        true
    }

    /// `: T` for a variable without annotation, or ` = literal` for a literal constant.
    fn inferred_var(&mut self, decl: NodeId, name: NodeId, const_init: bool) {
        let Some(sym) = self
            .c
            .sym_of_decl(self.loc(decl))
            .or_else(|| self.c.sym_of_decl(self.loc(name)))
        else {
            self.w(": any");
            return;
        };
        let t = self.ck.type_of_symbol(sym);
        // Only a fresh literal (from a literal expression, not `x as 1`) is an initializer.
        let fresh = self
            .ck
            .store()
            .traits(t)
            .contains(ts_r_types::TypeTraits::FRESH);
        let init = match *self.ast.node(decl) {
            Node::VarDeclarator { init, .. } => init,
            _ => None,
        };
        if const_init
            && fresh
            && let Some(text) = self.literal_initializer(t)
        {
            self.w(" = ");
            self.w(&text);
            return;
        }
        // `const s = Symbol()` declares a unique symbol.
        if let TypeKind::UniqueSymbol(u) = self.ck.store().kind(t)
            && u == sym
        {
            self.w(": unique symbol");
            return;
        }
        if let Node::VarDeclarator { init: Some(i), .. } = *self.ast.node(decl)
            && reuse::is_reusable(&self.ast, self.src, i)
        {
            self.w(": ");
            self.reuse_expr_type(i);
            return;
        }
        // A constant's type other than one fresh literal is written with its fresh
        // literals widened (`const c = b ? "x" : "y"` is `string`).
        let t = if const_init {
            self.ck.widen_literal(t)
        } else {
            t
        };
        self.w(": ");
        self.inferred_type_from(t, decl, init);
    }

    /// The initializer tsc writes in place of the annotation of an initialized constant or
    /// readonly property whose declared type is still a fresh literal: an annotation naming
    /// the type of `export default 1` (observed: `const a: MagicNumber = 1` with `type
    /// MagicNumber = typeof import("./constants").default` is `declare const a = 1;`, while
    /// `const e: typeof k = 1` keeps its annotation).
    fn fresh_literal_of(&mut self, decl: NodeId, name: NodeId, ann: NodeId) -> Option<String> {
        if !self.annotation_keeps_freshness(self.loc(ann), 0) {
            return None;
        }
        let sym = self
            .c
            .sym_of_decl(self.loc(decl))
            .or_else(|| self.c.sym_of_decl(self.loc(name)))?;
        let t = self.ck.type_of_symbol(sym);
        if !self
            .ck
            .store()
            .traits(t)
            .contains(ts_r_types::TypeTraits::FRESH)
        {
            return None;
        }
        self.literal_initializer(t)
    }

    /// Whether annotation `ann` can carry a fresh literal type in tsc: a `typeof import()`
    /// type, directly or through type aliases. A `typeof k` query is regular there (so
    /// `const e: typeof k = 1` keeps its annotation), unlike in the checker, whose
    /// narrowing relies on the query keeping `k`'s literal as it is.
    fn annotation_keeps_freshness(&self, ann: Loc, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        match *self.c.node(ann) {
            Node::ImportType {
                is_typeof: true, ..
            } => true,
            Node::ParenType { ty } => {
                self.annotation_keeps_freshness(Loc::new(ann.file, ty), depth + 1)
            }
            Node::TypeRef {
                name,
                type_args: None,
            } => {
                let Some(sym) = self
                    .c
                    .resolve_ident(Loc::new(ann.file, name), ts_r_binder::meaning::TYPE)
                else {
                    return false;
                };
                self.c.decls(sym).iter().any(|d| match *self.c.node(*d) {
                    Node::TypeAlias {
                        ty,
                        type_params: None,
                        ..
                    } => self.annotation_keeps_freshness(Loc::new(d.file, ty), depth + 1),
                    _ => false,
                })
            }
            _ => false,
        }
    }

    /// The initializer text tsc writes for a literal-typed constant (`= 1`, `= "s"`).
    fn literal_initializer(&mut self, t: TypeId) -> Option<String> {
        let s = self.ck.store();
        let f = s.flags(t);
        match s.kind(t) {
            TypeKind::StringLiteral(x) => Some(ts_r_checker::print::quote_string(s.str(x))),
            TypeKind::NumberLiteral(..) => {
                let v = s.number_value(t)?;
                if v.is_nan() || v.is_infinite() {
                    return None;
                }
                Some(number_text(v))
            }
            TypeKind::BigIntLiteral(x) => Some(format!("{}n", s.str(x))),
            TypeKind::Intrinsic if f.contains(TypeFlags::BOOLEAN_LITERAL) => {
                Some(if s.regular(t) == TypeId::TRUE {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                })
            }
            TypeKind::EnumLiteral { member, .. } => {
                let parent = self.c.sym_parent(member)?;
                self.reference_symbol(parent);
                let pn = self.c.sym_name(parent).to_owned();
                let mn = self.c.sym_name(member).to_owned();
                Some(if ts_r_checker::typenode::is_identifier_text(&mn) {
                    format!("{pn}.{mn}")
                } else {
                    format!("{pn}[{}]", ts_r_checker::print::quote_string(&mn))
                })
            }
            _ => None,
        }
    }

    /// Prints a checker type as a type node at declaration `at`.
    fn inferred_type(&mut self, t: TypeId, at: NodeId) {
        self.inferred_type_from(t, at, None);
    }

    /// Prints checker type `t` as a type node at declaration `at`; when `t` is the type of
    /// source expression `expr`, the parts tsc writes from the expression's syntax are
    /// written from it (and map to it).
    fn inferred_type_from(&mut self, t: TypeId, at: NodeId, expr: Option<NodeId>) {
        let expr = expr.filter(|&e| !self.is_synth(e));
        let variable = matches!(self.ast.node(at), Node::VarDeclarator { .. });
        let mut cx = TypeNodeCx::new(&mut self.ast, Loc::new(self.file, at)).with_host(self.host);
        cx.report_unnameable = variable;
        let built = match expr {
            Some(e) => self
                .ck
                .expr_type_to_type_node(Loc::new(self.file, e), t, &mut cx),
            None => self.ck.type_to_type_node(t, &mut cx),
        };
        let refs = std::mem::take(&mut cx.refs);
        let docs = std::mem::take(&mut cx.member_decls);
        let origins = std::mem::take(&mut cx.origins);
        drop(cx);
        self.origins.extend(origins);
        for (m, d) in docs {
            if d.file == self.file {
                self.member_docs.insert(m, d.node);
            }
        }
        for r in refs {
            self.reference_symbol(r);
        }
        match built {
            Ok(n) => self.type_node(n),
            Err(why) => {
                self.fail(at, why);
                self.w("any");
            }
        }
    }

    fn function_decl(&mut self, s: NodeId) {
        let Node::FunctionDecl {
            name,
            type_params,
            params,
            return_type,
            ..
        } = *self.ast.node(s)
        else {
            return;
        };
        self.w("function ");
        if let Some(n) = name {
            self.name_text(n);
        }
        self.type_params(type_params);
        self.params(params, s);
        self.w(": ");
        match return_type {
            Some(r) => self.type_node(r),
            None => {
                self.iso_return(s, 9007, name.unwrap_or(s));
                self.inferred_return(s);
            }
        }
        self.semi_end(s);
    }

    /// The inferred return type of a function-like declaration.
    fn inferred_return(&mut self, f: NodeId) {
        let sig = self.ck.signature_from_decl(self.loc(f));
        let pred = self.ck.predicate_of(sig);
        let r = self.ck.return_type_of(sig);
        if let Some(p) = pred {
            // `x is T` / `asserts x`.
            let mut cx =
                TypeNodeCx::new(&mut self.ast, Loc::new(self.file, f)).with_host(self.host);
            let param = match p.kind {
                ts_r_types::PredicateKind::This | ts_r_types::PredicateKind::AssertsThis => {
                    "this".to_owned()
                }
                _ => self.ck.store().str(p.param_name).to_owned(),
            };
            let asserts = matches!(
                p.kind,
                ts_r_types::PredicateKind::AssertsIdentifier
                    | ts_r_types::PredicateKind::AssertsThis
            );
            let ty = p.ty.map(|t| self.ck.type_to_type_node(t, &mut cx));
            let refs = std::mem::take(&mut cx.refs);
            drop(cx);
            for r in refs {
                self.reference_symbol(r);
            }
            if asserts {
                self.w("asserts ");
            }
            self.w(&param);
            match ty {
                Some(Ok(n)) => {
                    self.w(" is ");
                    self.type_node(n);
                }
                Some(Err(why)) => {
                    self.fail(f, why);
                }
                None => {}
            }
            return;
        }
        if self.reuse_return_type(f) {
            return;
        }
        // A return naming a body-local type is printed structurally, not reused.
        let single = self
            .plain_single_return(f)
            .filter(|&e| self.return_type_accessible(f, e));
        self.inferred_type_from(r, f, single);
    }

    fn enum_members(&mut self, e: NodeId, members: &[NodeId], ambient: bool) {
        if let Node::Enum { name, .. } = *self.ast.node(e) {
            self.iso_enum(name, members);
        }
        let sym = self.c.sym_of_decl(self.loc(e));
        let values = sym.map(|s| self.ck.enum_members(s)).unwrap_or_default();
        let mut prev_end = self.ast.span(e).start;
        for (i, &m) in members.iter().enumerate() {
            let Node::EnumMember { name, init } = *self.ast.node(m) else {
                continue;
            };
            if self.is_internal(m, prev_end) {
                prev_end = self.ast.span(m).end;
                continue;
            }
            self.leading_jsdoc(m, prev_end);
            prev_end = self.ast.span(m).end;
            let span = self.ast.span(name);
            self.mark(span.start);
            self.w(&enum_member_name(&self.ast, self.src, name));
            self.mark(span.end);
            let msym = self.c.sym_of_decl(self.loc(m));
            let value = values
                .iter()
                .find(|(s, _)| Some(*s) == msym)
                .and_then(|(_, v)| *v)
                .filter(|_| !ambient || init.is_some());
            if let Some(v) = value {
                let s = self.ck.store();
                if let Some(text) = s.string_value(v) {
                    let q = ts_r_checker::print::quote_string(text);
                    self.w(" = ");
                    self.w(&q);
                } else if let Some(n) = s.number_value(v) {
                    self.w(" = ");
                    self.w(&number_text(n));
                }
            }
            self.end(m);
            if i + 1 < members.len() {
                self.w(",");
            }
            self.nl();
        }
    }

    fn module_decl(&mut self, s: NodeId, scope: Scope, keep_export: bool) {
        let Node::Module {
            modifiers,
            kind,
            name,
            body,
        } = *self.ast.node(s)
        else {
            return;
        };
        let mut words: Vec<(Modifiers, &str)> = Vec::new();
        if modifiers.contains(Modifiers::EXPORT) && (scope.top() || keep_export) {
            words.push((Modifiers::EXPORT, "export"));
        }
        if scope.top() {
            words.push((Modifiers::DECLARE, "declare"));
        }
        self.modifiers(s, &words);
        match kind {
            ModuleKind::Global => self.name_text(name),
            _ if matches!(self.ast.node(name), Node::StringLit { .. }) => {
                self.w("module ");
                self.name_text(name);
            }
            _ => {
                self.w("namespace ");
                // `namespace A.B { }` is a module `A` whose body is module `B` in our AST
                // when written dotted; print the dotted name.
                self.dotted_module_name(s);
            }
        }
        // Find the innermost body of a dotted namespace.
        let mut body = body;
        let mut inner = s;
        while let Some(b) = body {
            if let Node::Module { body: b2, .. } = *self.ast.node(b) {
                inner = b;
                body = b2;
                continue;
            }
            break;
        }
        let _ = inner;
        let Some(b) = body else {
            self.semi_end(s);
            return;
        };
        let Node::ModuleBlock { stmts } = *self.ast.node(b) else {
            self.w(" { }");
            self.end(s);
            self.nl();
            return;
        };
        let stmts = self.ast.list(stmts).to_vec();
        let ambient = modifiers.contains(Modifiers::DECLARE)
            || kind == ModuleKind::Global
            || matches!(self.ast.node(name), Node::StringLit { .. })
            || scope == Scope::Ambient
            || self.c.file(self.file).is_declaration();
        let inner_scope = if ambient {
            Scope::Ambient
        } else {
            Scope::Namespace
        };
        // A body with nothing to write keeps its source form: `{ }` on one line, else
        // `{` and `}` on their own lines.
        let body = self.ast.span(b);
        let single_line = !self
            .src
            .get(body.start as usize..body.end as usize)
            .is_some_and(|t| t.contains('\n'));
        if stmts.is_empty() {
            self.w(" ");
            self.start(b);
            self.w(if single_line { "{ }" } else { "{" });
            if !single_line {
                self.nl();
                self.w("}");
            }
            self.end(b);
            self.end(s);
            self.nl();
            return;
        }
        let before = self.out.len();
        let marks = self.marks.len();
        self.w(" ");
        self.start(b);
        self.w("{");
        self.nl();
        let opened = self.out.len();
        self.indent += 1;
        self.statements(&stmts, inner_scope, Some(b));
        self.indent -= 1;
        if self.out.len() == opened && single_line {
            self.out.truncate(before);
            self.marks.truncate(marks);
            self.w(" ");
            self.start(b);
            self.w("{ }");
            self.end(b);
            self.end(s);
            self.nl();
            return;
        }
        self.w("}");
        self.end(b);
        self.end(s);
        self.nl();
    }

    /// Writes the name of namespace `s`, dotted through its nested namespaces (each one
    /// maps its start and its name).
    fn dotted_module_name(&mut self, s: NodeId) {
        let mut cur = Some(s);
        while let Some(m) = cur {
            let Node::Module { name, body, .. } = *self.ast.node(m) else {
                break;
            };
            if m != s {
                self.w(".");
                self.start(m);
            }
            if matches!(self.ast.node(name), Node::QualifiedName { .. }) {
                self.entity_name(name);
            } else {
                self.name_text(name);
            }
            cur = body.filter(|&b| matches!(self.ast.node(b), Node::Module { .. }));
        }
    }

    // ---- classes ----------------------------------------------------------------------

    fn class_decl(&mut self, s: NodeId, scope: Scope, keep_export: bool, prev_end: u32) {
        let Node::ClassDecl {
            modifiers,
            name,
            type_params,
            heritage,
            members,
            ..
        } = *self.ast.node(s)
        else {
            return;
        };
        let heritage = self.ast.list(heritage).to_vec();
        // `extends <expression>` needs a `_base` constant.
        let mut base_name: Option<String> = None;
        if let Some(expr) = self.base_expr(s) {
            {
                let cname = name.map_or("default".to_owned(), |n| self.raw(n).to_owned());
                let bname = self.unique_name(&format!("{cname}_base"));
                self.iso_heritage(expr);
                let t = self.ck.check_expr(self.loc(expr));
                self.w(if scope.top() {
                    "declare const "
                } else {
                    "const "
                });
                self.w(&bname);
                self.w(": ");
                self.inferred_type(t, s);
                self.w(";");
                self.nl();
                base_name = Some(bname);
            }
        }
        self.leading_jsdoc(s, prev_end);
        self.decl_prefix(s, scope, keep_export, true);
        let _ = modifiers;
        self.w("class");
        if let Some(n) = name {
            self.w(" ");
            self.name_text(n);
        }
        self.type_params(type_params);
        for &h in &heritage {
            let Node::HeritageClause { kind, types } = *self.ast.node(h) else {
                continue;
            };
            let types = self.ast.list(types).to_vec();
            if types.is_empty() {
                continue;
            }
            self.start(h);
            self.w(match kind {
                ts_r_ast::HeritageKind::Extends => " extends ",
                ts_r_ast::HeritageKind::Implements => " implements ",
            });
            for (i, &t) in types.iter().enumerate() {
                if i > 0 {
                    self.w(", ");
                }
                if kind == ts_r_ast::HeritageKind::Extends
                    && let Some(b) = &base_name
                {
                    self.w(&b.clone());
                    if let Node::ExprWithTypeArgs { type_args, .. } = *self.ast.node(t) {
                        self.type_args(type_args);
                    }
                    continue;
                }
                self.heritage_type(t);
            }
            self.end(h);
        }
        self.w(" {");
        self.nl();
        self.indent += 1;
        let ms = self.ast.list(members).to_vec();
        self.class_members(s, &ms);
        self.indent -= 1;
        self.w("}");
        self.end(s);
        self.nl();
    }

    /// Block comments on the same line between `from` (the statement start or the previous
    /// declarator's end) and a declarator's name: `var /*4*/ point: ...` (observed).
    fn declarator_comments(&mut self, from: u32, name_start: u32) {
        let comments: Vec<_> = self.ast.comments_between(from, name_start).to_vec();
        for c in comments {
            let same_line = self
                .src
                .get(from as usize..c.span.start as usize)
                .is_some_and(|t| !t.contains('\n'));
            if c.kind != ts_r_ast::CommentKind::Block || !same_line {
                continue;
            }
            let text = self.src[c.span.start as usize..c.span.end as usize].to_owned();
            self.mark(c.span.start);
            self.w(&text);
            self.mark(c.span.end);
            self.w(" ");
        }
    }

    /// The offset after the `{` that opens a class or interface body (comments before it,
    /// as on type parameters, are not its members').
    fn body_open(&self, decl: NodeId) -> u32 {
        let start = self.ast.span(decl).start;
        let mut end = start;
        let mut see = |n: Option<NodeId>| {
            if let Some(n) = n {
                end = end.max(self.ast.span(n).end);
            }
        };
        match *self.ast.node(decl) {
            Node::ClassDecl {
                name,
                type_params,
                heritage,
                ..
            }
            | Node::ClassExpr {
                name,
                type_params,
                heritage,
                ..
            } => {
                see(name);
                see(type_params);
                see(self.ast.list(heritage).last().copied());
            }
            Node::Interface {
                name,
                type_params,
                extends,
                ..
            } => {
                see(Some(name));
                see(type_params);
                see(self.ast.list(extends).last().copied());
            }
            _ => return start,
        }
        self.src
            .get(end as usize..)
            .and_then(|t| t.find('{'))
            .map_or(start, |i| end + i as u32 + 1)
    }

    fn class_members(&mut self, class: NodeId, members: &[NodeId]) {
        // `#private;` once for any private-named member.
        let has_private = members.iter().any(|&m| {
            let key = match *self.ast.node(m) {
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                _ => return false,
            };
            matches!(self.ast.node(key), Node::PrivateName { .. })
        });
        if has_private {
            self.w("#private;");
            self.nl();
        }
        let hoisted = self.unbindable_members(class, members);
        // Parameter properties of the constructor implementation.
        let ctor = members.iter().copied().find(|&m| {
            matches!(*self.ast.node(m), Node::MethodDef { kind: MethodKind::Constructor, value, .. }
                if matches!(*self.ast.node(value), Node::FunctionExpr { body: Some(_), .. }))
        });
        if let Some(c) = ctor
            && let Node::MethodDef { value, .. } = *self.ast.node(c)
            && let Node::FunctionExpr { params, .. } = *self.ast.node(value)
        {
            let mut prev_end = self.ast.span(c).start;
            for &p in self.ast.list(params).to_vec().iter() {
                let Node::Param {
                    modifiers,
                    pattern,
                    optional,
                    type_ann,
                    init,
                    ..
                } = *self.ast.node(p)
                else {
                    continue;
                };
                let comments_from = prev_end;
                prev_end = self.ast.span(p).end;
                let acc = Modifiers::PUBLIC
                    | Modifiers::PRIVATE
                    | Modifiers::PROTECTED
                    | Modifiers::READONLY
                    | Modifiers::OVERRIDE;
                if !modifiers.intersects(acc) {
                    continue;
                }
                // A destructured parameter property declares each bound name (observed:
                // `public [x, y]: string[]` gives `x: string; y: string;`).
                if matches!(
                    self.ast.node(pattern),
                    Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                ) {
                    let mut names = Vec::new();
                    collect_binding_names(&self.ast, pattern, &mut names);
                    for n in names {
                        self.member_modifiers(p, Modifiers(modifiers.0 & acc.0));
                        self.name_text(n);
                        if !modifiers.contains(Modifiers::PRIVATE) {
                            self.inferred_var(n, n, false);
                        }
                        self.w(";");
                        self.nl();
                    }
                    continue;
                }
                // Its own JSDoc only, always on a line of its own. tsc builds the property:
                // its name, modifiers and type map, it does not.
                self.last_written_end = None;
                self.leading_comments(p, comments_from);
                self.member_modifiers(p, Modifiers(modifiers.0 & acc.0));
                self.name_text(pattern);
                if optional {
                    self.question(pattern);
                }
                if !modifiers.contains(Modifiers::PRIVATE) {
                    self.w(": ");
                    match type_ann {
                        Some(t) => {
                            let start = self.out.len();
                            self.type_node(t);
                            // `public x?: T` is `x?: T | undefined`.
                            if optional && !self.annotation_includes_undefined(t) {
                                self.append_undefined(start);
                            }
                        }
                        None => {
                            if !init.is_some_and(|i| self.reuse_expr_type(i)) {
                                let t = self.ck.param_type(self.loc(p));
                                self.inferred_type(t, p);
                            }
                        }
                    }
                }
                self.w(";");
                self.nl();
            }
        }
        let mut prev_end = self.body_open(class);
        let mut printed_private: FxHashSet<String> = FxHashSet::default();
        for (i, &m) in members.iter().enumerate() {
            let pe = prev_end;
            prev_end = self.ast.span(m).end;
            if hoisted.contains(&m) || self.is_internal(m, pe) {
                continue;
            }
            match *self.ast.node(m) {
                Node::PropertyDef {
                    modifiers,
                    key,
                    optional,
                    type_ann,
                    value,
                    ..
                } => {
                    if !modifiers.contains(Modifiers::PRIVATE) {
                        self.iso_class_key(key);
                    }
                    if self.skip_key(key) {
                        continue;
                    }
                    self.leading_jsdoc(m, pe);
                    self.member_modifiers(m, modifiers);
                    self.property_key(key);
                    if optional {
                        self.question(key);
                    }
                    if modifiers.contains(Modifiers::PRIVATE) {
                        self.semi_end(m);
                        continue;
                    }
                    let fresh = type_ann
                        .filter(|_| {
                            modifiers.contains(Modifiers::READONLY) && value.is_some() && !optional
                        })
                        .and_then(|t| self.fresh_literal_of(m, key, t));
                    match (type_ann, fresh) {
                        (Some(_), Some(text)) => {
                            self.w(" = ");
                            self.w(&text);
                        }
                        (Some(t), None) => {
                            self.w(": ");
                            self.type_node(t);
                        }
                        (None, _) => {
                            self.iso_property(key, value, modifiers.contains(Modifiers::READONLY));
                            let sym = self.c.sym_of_decl(self.loc(m));
                            let t = sym.map_or(TypeId::ANY, |s| self.ck.type_of_symbol(s));
                            let ro = modifiers.contains(Modifiers::READONLY)
                                && value.is_some()
                                && !optional;
                            if ro && let Some(text) = self.literal_initializer(t) {
                                self.w(" = ");
                                self.w(&text);
                            } else if let Some(v) = value
                                && reuse::is_reusable(&self.ast, self.src, v)
                            {
                                self.w(": ");
                                self.reuse_expr_type(v);
                            } else {
                                self.w(": ");
                                self.inferred_type_from(t, m, value);
                            }
                        }
                    }
                    self.semi_end(m);
                }
                Node::MethodDef {
                    modifiers,
                    kind,
                    key,
                    optional,
                    value,
                    ..
                } => {
                    if !modifiers.contains(Modifiers::PRIVATE) && body_of(&self.ast, value) {
                        self.iso_class_key(key);
                    }
                    if self.skip_key(key) {
                        continue;
                    }
                    let Node::FunctionExpr {
                        type_params,
                        params,
                        return_type,
                        body,
                        ..
                    } = *self.ast.node(value)
                    else {
                        continue;
                    };
                    let has_overloads = members[..i].iter().any(|&o| {
                        self.same_method(o, kind, key)
                            && matches!(*self.ast.node(o), Node::MethodDef { value: v, .. }
                                if matches!(*self.ast.node(v), Node::FunctionExpr { body: None, .. }))
                    });
                    if body.is_some() && has_overloads {
                        continue;
                    }
                    let private = modifiers.contains(Modifiers::PRIVATE);
                    if private && kind == MethodKind::Method {
                        // `private m;` once for an overloaded method.
                        let k = self.raw(key).to_owned();
                        if !printed_private.insert(k) {
                            continue;
                        }
                    }
                    // A private method is written as a new property: it maps its start only
                    // when it keeps the source's modifier list (observed: not for `private
                    // async m()`).
                    let kept = Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::ABSTRACT
                        | Modifiers::STATIC
                        | Modifiers::READONLY
                        | Modifiers::ACCESSOR;
                    if private
                        && kind == MethodKind::Method
                        && !self.is_synth(m)
                        && self
                            .modifier_tokens(m)
                            .0
                            .iter()
                            .any(|&(f, _)| !kept.contains(f))
                    {
                        self.leading_comments(m, pe);
                    } else {
                        self.leading_jsdoc(m, pe);
                    }
                    self.member_modifiers(m, modifiers);
                    match kind {
                        MethodKind::Constructor => {
                            self.w("constructor");
                            if private {
                                self.w("()");
                                self.semi_end(m);
                                continue;
                            }
                            self.params(params, value);
                            self.semi_end(m);
                            continue;
                        }
                        MethodKind::Get => {
                            self.w("get ");
                            self.property_key(key);
                            if private {
                                self.w("()");
                                self.semi_end(m);
                                continue;
                            }
                            self.w("(): ");
                            match return_type {
                                Some(r) => self.type_node(r),
                                None => self.accessor_type(m, key, true),
                            }
                            self.semi_end(m);
                            continue;
                        }
                        MethodKind::Set => {
                            self.w("set ");
                            self.property_key(key);
                            if private {
                                self.w("(value)");
                                self.semi_end(m);
                                continue;
                            }
                            self.setter_params(params, m, key);
                            self.semi_end(m);
                            continue;
                        }
                        MethodKind::Method => {
                            self.property_key(key);
                            if optional {
                                self.question(key);
                            }
                            if private {
                                // tsc writes a new property for a private method: its
                                // end maps nothing (observed).
                                self.w(";");
                                self.nl();
                                continue;
                            }
                            self.type_params(type_params);
                            self.params(params, value);
                            self.w(": ");
                            match return_type {
                                Some(r) => self.type_node(r),
                                None => {
                                    self.iso_return(value, 9008, key);
                                    self.inferred_return(value);
                                }
                            }
                            self.semi_end(m);
                        }
                    }
                }
                Node::IndexSignature { .. } => {
                    self.leading_jsdoc(m, pe);
                    self.type_member(m);
                }
                _ => {}
            }
        }
    }

    fn same_method(&self, o: NodeId, kind: MethodKind, key: NodeId) -> bool {
        match *self.ast.node(o) {
            Node::MethodDef {
                kind: k, key: k2, ..
            } => k == kind && (kind == MethodKind::Constructor || self.raw(k2) == self.raw(key)),
            _ => false,
        }
    }

    /// Class members whose computed name is not late-bindable (`[k]` of a `string`,
    /// `[s]` of a `symbol`) are written first as properties, like index signature
    /// entries (observed): static ones, then instance ones; within each the `string`
    /// keys (number keys too), then the `number` keys, then the `symbol` keys, in source
    /// order (an instance number key appears twice). A method is a property of its function type
    /// (each overload one), an accessor of its type. Returns the members written.
    fn unbindable_members(&mut self, class: NodeId, members: &[NodeId]) -> Vec<NodeId> {
        const STR: u8 = 1;
        const NUM: u8 = 2;
        const SYM: u8 = 4;
        let mut found: Vec<(NodeId, u32, bool, u8)> = Vec::new();
        let mut prev_end = self.body_open(class);
        for &m in members {
            let pe = prev_end;
            prev_end = self.ast.span(m).end;
            let (modifiers, key) = match *self.ast.node(m) {
                Node::PropertyDef { modifiers, key, .. } => (modifiers, key),
                Node::MethodDef {
                    modifiers,
                    key,
                    kind,
                    ..
                } if kind != MethodKind::Constructor => (modifiers, key),
                _ => continue,
            };
            if modifiers.contains(Modifiers::PRIVATE)
                || self.skip_key(key)
                || !self.unbindable_key(key)
                || self.is_internal(m, pe)
            {
                continue;
            }
            let Node::ComputedKey { expr } = *self.ast.node(key) else {
                continue;
            };
            let t = self.ck.check_expr(self.loc(skip_parens(&self.ast, expr)));
            let f = self.ck.store().flags(t);
            let kinds = if f.intersects(TypeFlags::ES_SYMBOL) {
                SYM
            } else if f.intersects(TypeFlags::NUMBER | TypeFlags::ENUM) {
                STR | NUM
            } else {
                STR
            };
            // A static number key appears once (observed).
            let is_static = modifiers.contains(Modifiers::STATIC);
            let kinds = if is_static { kinds & !NUM } else { kinds };
            found.push((m, pe, is_static, kinds));
        }
        for is_static in [true, false] {
            for kind in [STR, NUM, SYM] {
                for &(m, pe, st, kinds) in &found {
                    if st == is_static && kinds & kind != 0 {
                        self.unbindable_member(m, pe);
                    }
                }
            }
        }
        found.into_iter().map(|(m, ..)| m).collect()
    }

    fn unbindable_member(&mut self, m: NodeId, pe: u32) {
        let (modifiers, key, optional) = match *self.ast.node(m) {
            Node::PropertyDef {
                modifiers,
                key,
                optional,
                ..
            }
            | Node::MethodDef {
                modifiers,
                key,
                optional,
                ..
            } => (modifiers, key, optional),
            _ => return,
        };
        self.iso_class_key(key);
        self.leading_jsdoc(m, pe);
        self.member_modifiers(m, modifiers);
        self.property_key(key);
        if optional {
            self.w("?");
        }
        self.w(": ");
        match *self.ast.node(m) {
            Node::PropertyDef {
                type_ann, value, ..
            } => match (type_ann, value) {
                (Some(t), _) => self.type_node(t),
                (None, Some(v)) if reuse::is_reusable(&self.ast, self.src, v) => {
                    self.reuse_expr_type(v);
                }
                (None, Some(v)) => {
                    let t = self.ck.check_expr(self.loc(v));
                    let t = self.ck.widen_literal(t);
                    self.inferred_type_from(t, m, Some(v));
                }
                (None, None) => self.w("any"),
            },
            Node::MethodDef { kind, value, .. } => {
                let Node::FunctionExpr {
                    type_params,
                    params,
                    return_type,
                    ..
                } = *self.ast.node(value)
                else {
                    return;
                };
                match kind {
                    MethodKind::Get => match return_type {
                        Some(r) => self.type_node(r),
                        None => self.accessor_type(m, key, true),
                    },
                    MethodKind::Set => {
                        match self
                            .ast
                            .list(params)
                            .first()
                            .and_then(|&p| match *self.ast.node(p) {
                                Node::Param { type_ann, .. } => type_ann,
                                _ => None,
                            }) {
                            Some(t) => self.type_node(t),
                            None => self.w("any"),
                        }
                    }
                    _ => {
                        let wrap = optional && self.c.opts.strict_null_checks;
                        if wrap {
                            self.w("(");
                        }
                        self.type_params(type_params);
                        self.params(params, value);
                        self.w(" => ");
                        match return_type {
                            Some(r) => self.type_node(r),
                            None => {
                                // Its return is still inferred alongside the name's TS9038.
                                self.iso_return(value, 9008, key);
                                self.inferred_return(value);
                            }
                        }
                        if wrap {
                            self.w(") | undefined");
                        }
                    }
                }
            }
            _ => {}
        }
        self.w(";");
        self.nl();
    }

    /// A computed name whose type is not a string or number literal or a unique symbol
    /// (`[sym]` of a `symbol`, `[k]` of a `string` or `any`).
    pub(super) fn unbindable_key(&mut self, key: NodeId) -> bool {
        let Node::ComputedKey { expr } = *self.ast.node(key) else {
            return false;
        };
        let e = skip_parens(&self.ast, expr);
        if matches!(
            self.ast.node(e),
            Node::StringLit { .. } | Node::NumberLit { .. }
        ) {
            return false;
        }
        let t = self.ck.check_expr(self.loc(e));
        if self.symbol_constructor_key(e) {
            // Unless `SymbolConstructor` lacks the property (observed: `[Symbol.species]`
            // is dropped after TS2339 when the lib has no `species`).
            return t == TypeId::ERROR;
        }
        !self.ck.store().flags(t).intersects(
            TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL | TypeFlags::UNIQUE_ES_SYMBOL,
        )
    }

    /// `Symbol.x`, or a name declared in this file as `typeof Symbol.x`: tsc binds members
    /// named by `SymbolConstructor` properties even when they are typed `symbol`
    /// (observed).
    fn symbol_constructor_key(&self, e: NodeId) -> bool {
        match *self.ast.node(e) {
            Node::Member { obj, .. } => self.raw(obj) == "Symbol",
            Node::Ident { .. } => {
                let Some(sym) = self
                    .c
                    .resolve_ident(self.loc(e), ts_r_binder::meaning::VALUE)
                else {
                    return false;
                };
                let Some(d) = self.c.first_decl(sym) else {
                    return false;
                };
                if d.file != self.file {
                    return false;
                }
                let decl = match *self.ast.node(d.node) {
                    Node::VarDeclarator { .. } => Some(d.node),
                    _ => self.c.file(self.file).parents().parent(d.node),
                };
                match decl.map(|n| *self.ast.node(n)) {
                    Some(Node::VarDeclarator {
                        type_ann: Some(t), ..
                    }) => self.raw(t).starts_with("typeof Symbol."),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Members not printed: private names (covered by `#private;`) and computed names
    /// that are not entity names or literals.
    fn skip_key(&self, key: NodeId) -> bool {
        match *self.ast.node(key) {
            Node::PrivateName { .. } => true,
            Node::ComputedKey { expr } => {
                let e = skip_parens(&self.ast, expr);
                !(is_entity_expr(&self.ast, e)
                    || matches!(
                        self.ast.node(e),
                        Node::StringLit { .. } | Node::NumberLit { .. }
                    ))
            }
            _ => false,
        }
    }

    /// The modifiers `m` of member (or parameter property) `n` that a declaration keeps:
    /// accessibility, `abstract`, `static`, `readonly`, `accessor` in tsc's order (`public`,
    /// `override`, `async` and `declare` are dropped, as tsc 7.0.2 does).
    fn member_modifiers(&mut self, n: NodeId, m: Modifiers) {
        let mut words: Vec<(Modifiers, &str)> = Vec::new();
        for (f, w) in [
            (Modifiers::PRIVATE, "private"),
            (Modifiers::PROTECTED, "protected"),
            (Modifiers::ABSTRACT, "abstract"),
            (Modifiers::STATIC, "static"),
            (Modifiers::READONLY, "readonly"),
            (Modifiers::ACCESSOR, "accessor"),
        ] {
            if m.contains(f) {
                words.push((f, w));
            }
        }
        self.modifiers(n, &words);
    }

    /// Writes the `?` after `name` (a source token with its marks when `name` is the
    /// source's).
    pub(super) fn question(&mut self, name: NodeId) {
        let q = if self.is_synth(name) {
            None
        } else {
            self.char_token(self.ast.span(name).end, b'?')
        };
        self.token("?", q);
    }

    /// The type of an accessor without annotations: the getter's return type, else the
    /// setter's parameter type.
    fn accessor_type(&mut self, m: NodeId, key: NodeId, getter: bool) {
        // The pair's annotations, then the getter's reusable `return`.
        let (get, set) = self.accessor_pair(m, key);
        let get_fn = get.and_then(|g| match *self.ast.node(g) {
            Node::MethodDef { value, .. } => Some(value),
            _ => None,
        });
        if let Some(f) = get_fn
            && let Node::FunctionExpr {
                return_type: Some(r),
                ..
            } = *self.ast.node(f)
        {
            self.type_node(r);
            return;
        }
        if let Some(s) = set
            && let Node::MethodDef { value, .. } = *self.ast.node(s)
            && let Node::FunctionExpr { params, .. } = *self.ast.node(value)
            && let Some(&p) = self.ast.list(params).first()
            && let Node::Param {
                type_ann: Some(t), ..
            } = *self.ast.node(p)
        {
            self.type_node(t);
            return;
        }
        self.iso_accessor(get, set, key);
        if let Some(f) = get_fn
            && self.reuse_return_type(f)
        {
            return;
        }
        let _ = getter;
        let sym = self.c.sym_of_decl(self.loc(m));
        // A computed name (`[Symbol.toPrimitive]`) has no member symbol with the pair's
        // type: the getter's inferred return type.
        if (sym.is_none() || matches!(self.ast.node(key), Node::ComputedKey { .. }))
            && let Some(f) = get_fn
        {
            self.inferred_return(f);
            return;
        }
        let t = sym.map_or(TypeId::ANY, |s| self.ck.type_of_symbol(s));
        self.inferred_type(t, m);
    }

    /// The getter and setter named `key` in the class or object literal holding member `m`.
    fn accessor_pair(&self, m: NodeId, key: NodeId) -> (Option<NodeId>, Option<NodeId>) {
        let members = match self.parents.parent(m).map(|p| *self.ast.node(p)) {
            Some(Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) => members,
            Some(Node::ObjectLit { props }) => props,
            _ => return (None, None),
        };
        let static_of = |n: NodeId| match *self.ast.node(n) {
            Node::MethodDef { modifiers, .. } => modifiers.contains(Modifiers::STATIC),
            _ => false,
        };
        let is_static = static_of(m);
        let (mut get, mut set) = (None, None);
        for &o in self.ast.list(members) {
            if let Node::MethodDef { kind, key: k, .. } = *self.ast.node(o)
                && static_of(o) == is_static
                && self.raw(k) == self.raw(key)
            {
                match kind {
                    MethodKind::Get if get.is_none() => get = Some(o),
                    MethodKind::Set if set.is_none() => set = Some(o),
                    _ => {}
                }
            }
        }
        (get, set)
    }

    fn setter_params(&mut self, params: NodeList, m: NodeId, key: NodeId) {
        let ps = self.ast.list(params).to_vec();
        self.w("(");
        if ps.is_empty() {
            // A setter without its parameter (an error) is written with `value: any`.
            self.w("value: any");
        }
        for (i, &p) in ps.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            let Node::Param {
                rest,
                pattern,
                type_ann,
                ..
            } = *self.ast.node(p)
            else {
                continue;
            };
            if !self.is_synth(p) {
                self.param_comments(p, pattern);
            }
            if rest {
                self.w("...");
            }
            self.binding_name(pattern);
            self.w(": ");
            match type_ann {
                Some(t) => self.type_node(t),
                None => self.accessor_type(m, key, false),
            }
        }
        self.w(")");
    }
}

/// Removes up to `cols` columns of leading whitespace.
fn strip_columns(line: &str, cols: usize) -> &str {
    let mut col = 0;
    let mut idx = 0;
    for (i, ch) in line.char_indices() {
        if col >= cols {
            idx = i;
            break;
        }
        match ch {
            ' ' => col += 1,
            '\t' => col = (col / 4 + 1) * 4,
            _ => {
                idx = i;
                break;
            }
        }
        idx = i + ch.len_utf8();
    }
    &line[idx.min(line.len())..]
}

pub(crate) fn skip_parens(ast: &Ast, mut n: NodeId) -> NodeId {
    for _ in 0..64 {
        match *ast.node(n) {
            Node::Paren { expr } => n = expr,
            _ => break,
        }
    }
    n
}

/// `a`, `a.b.c` (identifiers and property accesses only).
pub(crate) fn is_entity_expr(ast: &Ast, n: NodeId) -> bool {
    match *ast.node(n) {
        Node::Ident { .. } => true,
        Node::Member {
            obj,
            prop,
            optional,
        } => !optional && matches!(ast.node(prop), Node::Ident { .. }) && is_entity_expr(ast, obj),
        _ => false,
    }
}

fn collect_binding_names(ast: &Ast, n: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(n) {
        Node::Ident { .. } => out.push(n),
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => match value {
                        Some(v) => collect_binding_names(ast, v, out),
                        None => collect_binding_names(ast, key, out),
                    },
                    Node::Rest { arg } => collect_binding_names(ast, arg, out),
                    _ => {}
                }
            }
        }
        Node::ArrayPattern { elements } => {
            for &e in ast.list(elements) {
                collect_binding_names(ast, e, out);
            }
        }
        Node::AssignPattern { left, .. } => collect_binding_names(ast, left, out),
        Node::Rest { arg } => collect_binding_names(ast, arg, out),
        _ => {}
    }
}

/// tsc's number text in declarations (`1.5`, `-16`, `1e+21`).
pub(crate) fn number_text(v: f64) -> String {
    if v < 0.0 {
        format!("-{}", ts_r_binder::js_number_to_string(-v))
    } else {
        ts_r_binder::js_number_to_string(v)
    }
}

fn enum_member_name(ast: &Ast, src: &str, name: NodeId) -> String {
    let sp = ast.span(name);
    let raw = src.get(sp.start as usize..sp.end as usize).unwrap_or("");
    // String names keep their quotes (`"Friday" = 0`).
    let _ = ast;
    raw.to_owned()
}

/// The `/// <reference ... preserve="true" />` line tsc writes for a triple-slash
/// directive, or `None` when the directive is not kept.
fn preserved_reference(text: &str) -> Option<String> {
    let body = text
        .strip_prefix("///")?
        .trim_start()
        .strip_prefix("<reference")?;
    let mut attrs: Vec<(&str, &str)> = Vec::new();
    let mut rest = body;
    loop {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else { break };
        let name = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let q = after.chars().next()?;
        if q != '"' && q != '\'' {
            break;
        }
        let end = after[1..].find(q)? + 1;
        attrs.push((name, &after[1..end]));
        rest = &after[end + 1..];
    }
    if !attrs.iter().any(|&(n, v)| n == "preserve" && v == "true") {
        return None;
    }
    let (kind, value) = *attrs
        .iter()
        .find(|(n, _)| matches!(*n, "path" | "types" | "lib"))?;
    let value = if kind == "path" {
        declaration_path(value)
    } else {
        value.to_owned()
    };
    let mode = attrs
        .iter()
        .find(|(n, _)| *n == "resolution-mode")
        .map_or(String::new(), |(_, m)| format!(" resolution-mode=\"{m}\""));
    Some(format!(
        "/// <reference {kind}=\"{value}\"{mode} preserve=\"true\" />"
    ))
}

/// A referenced source path's declaration output (`b.ts` -> `b.d.ts`).
fn declaration_path(p: &str) -> String {
    for (ext, dext) in [
        (".mts", ".d.mts"),
        (".cts", ".d.cts"),
        (".tsx", ".d.ts"),
        (".ts", ".d.ts"),
    ] {
        if let Some(stem) = p.strip_suffix(ext) {
            if stem.ends_with(".d") || stem.contains(".d.") {
                return p.to_owned();
            }
            return format!("{stem}{dext}");
        }
    }
    p.to_owned()
}

/// Whether method value `f` has a body (an implementation, not an overload signature).
fn body_of(ast: &Ast, f: NodeId) -> bool {
    matches!(*ast.node(f), Node::FunctionExpr { body: Some(_), .. })
}
