//! Compact, index-based AST shared by every tool.
//!
//! The data model is specified in `docs/design/0001-ast.md`. In short: nodes are a `Vec<Node>`
//! addressed by [`NodeId`] (`u32`), spans are a parallel `Vec<Span>`, child lists live in one
//! shared array addressed by [`NodeList`], identifiers are interned [`Atom`]s, and comments are a
//! sorted side table.

mod dump;
mod interner;
mod node;
pub mod visit;

use std::num::NonZeroU32;

pub use interner::{Atom, Interner};
pub use node::{
    AssignOp, BinaryOp, F64Bits, Field, FieldDump, FnFlags, HeritageKind, ImportPhase,
    KeywordTypeKind, MappedModifier, MetaPropKind, MethodKind, Modifiers, ModuleKind, Node,
    NodeKind, TypeOperatorKind, UnaryOp, UpdateOp, VarKind,
};
pub use ts_r_span::Span;
pub use visit::{ParentMap, Visitor, Walk, walk};

/// Index of a node in [`Ast::nodes`]. Never 0 (slot 0 is a reserved dummy), so
/// `Option<NodeId>` is 4 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(NonZeroU32);

impl NodeId {
    /// Builds an id from a raw index. Returns `None` for 0.
    pub fn from_raw(raw: u32) -> Option<NodeId> {
        NonZeroU32::new(raw).map(NodeId)
    }

    #[inline]
    pub fn index(self) -> usize {
        self.0.get() as usize
    }

    #[inline]
    pub fn raw(self) -> u32 {
        self.0.get()
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A list of nodes stored in [`Ast::list_items`]. `NodeList::EMPTY` is the empty list.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct NodeList(pub u32);

impl NodeList {
    pub const EMPTY: NodeList = NodeList(0);

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// `start..start+len` in [`Ast::list_items`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct ListRange {
    pub start: u32,
    pub len: u32,
}

/// What grammar a file was parsed with.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SourceKind {
    #[default]
    Ts,
    Tsx,
    /// `.d.ts`, `.d.mts`, `.d.cts`: an ambient TypeScript file.
    Dts,
    Js,
    Jsx,
}

impl SourceKind {
    /// Picks the grammar from a file name. Unknown extensions parse as TypeScript.
    pub fn from_path(path: &str) -> SourceKind {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
            return SourceKind::Dts;
        }
        // `foo.d.css.ts`-style declaration files for arbitrary extensions.
        if let Some(stem) = lower.strip_suffix(".ts")
            && stem
                .rsplit_once('/')
                .map_or(stem, |(_, s)| s)
                .contains(".d.")
        {
            return SourceKind::Dts;
        }
        match lower.rsplit_once('.').map(|(_, e)| e) {
            Some("tsx") => SourceKind::Tsx,
            Some("js" | "mjs" | "cjs") => SourceKind::Js,
            Some("jsx") => SourceKind::Jsx,
            _ => SourceKind::Ts,
        }
    }

    /// TypeScript syntax (types, `as`, generics in expressions) is enabled.
    pub fn is_typescript(self) -> bool {
        matches!(self, SourceKind::Ts | SourceKind::Tsx | SourceKind::Dts)
    }

    /// JSX is enabled. As in tsc, plain `.js` files allow JSX.
    pub fn is_jsx(self) -> bool {
        matches!(self, SourceKind::Tsx | SourceKind::Js | SourceKind::Jsx)
    }

    pub fn is_declaration(self) -> bool {
        self == SourceKind::Dts
    }
}

/// Kind of a source comment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CommentKind {
    /// `// ...`
    Line,
    /// `/* ... */`
    Block,
    /// `#!...` on the first line.
    Hashbang,
}

/// A comment, kept out of the tree. Spans include the delimiters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Comment {
    pub span: Span,
    pub kind: CommentKind,
    /// A line terminator precedes the comment (since the previous token).
    pub newline_before: bool,
}

/// One modifier keyword as written (`export`, `static`, ...), kept in [`Ast::modifier_tokens`]
/// so grammar checks can report at the keyword and see order and repeats, which the
/// [`Modifiers`] bitset on the node does not keep.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ModifierToken {
    pub span: Span,
    /// Exactly one bit.
    pub flag: Modifiers,
}

/// A comment a transform attaches to a (usually synthesized) node, printed by codegen just
/// before (`trailing: false`) or after the node: `0 /* E.A */` for an inlined const enum
/// member. Kept in [`Ast::synthetic_comments`]; the parser never adds one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SyntheticComment {
    pub node: NodeId,
    /// The whole comment, delimiters included (`/* E.A */`).
    pub text: Atom,
    pub trailing: bool,
}

/// Lengths of every array, for rolling back speculative parsing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Checkpoint {
    nodes: u32,
    list_items: u32,
    list_ranges: u32,
    comments: u32,
    modifier_tokens: u32,
    jsdoc_syntax: u32,
    class_semicolons: u32,
}

/// A parsed file. See the module docs.
#[derive(Clone, Debug)]
pub struct Ast {
    pub nodes: Vec<Node>,
    pub spans: Vec<Span>,
    pub list_items: Vec<NodeId>,
    pub list_ranges: Vec<ListRange>,
    pub atoms: Interner,
    /// Sorted by `span.start`.
    pub comments: Vec<Comment>,
    /// Every keyword the parser took as a modifier, sorted by `span.start`. Misplaced and
    /// repeated modifiers are included; see [`Ast::modifier_tokens_of`].
    pub modifier_tokens: Vec<ModifierToken>,
    /// Comments attached by transforms, sorted by `node` (insertion order among equal ids).
    /// Empty after parsing; see [`Ast::add_synthetic_comment`].
    pub synthetic_comments: Vec<SyntheticComment>,
    /// The empty `;` members of class bodies (tsc's semicolon class elements, which have no
    /// node here): `(end of the class, position of the ;)`, sorted by position. The class's
    /// end is its node's `span.end`, so a printer finds a class's own semicolons by it.
    pub class_semicolons: Vec<(u32, u32)>,
    /// The `Program` node. Equal to the dummy id until the parser finishes.
    pub root: NodeId,
    pub source_kind: SourceKind,
    /// Every node with an id of at least `jsdoc_start` was built from a JSDoc comment
    /// (`ts_r_parser::attach_jsdoc`, JavaScript files): JSDoc types, `@template` parameters,
    /// `@typedef`/`@callback` aliases. `u32::MAX` when there are none.
    pub jsdoc_start: u32,
    /// The JSDoc tags that apply to a node, sorted by `host` (tags of one host in source
    /// order). Types that also fill a field of the tree (`@param` into a parameter's
    /// annotation) are listed too; the others (`@type` of a function or an assignment)
    /// get their host as parent in [`ParentMap`].
    pub jsdoc_tags: Vec<JsDocTag>,
    /// The original value of every parsed node JSDoc rewrote (a parameter given a type, a
    /// statement list given aliases), in edit order: [`Ast::strip_jsdoc`] undoes them.
    pub jsdoc_undo: Vec<(NodeId, Node)>,
    /// Roots of dotted `@typedef`/`@callback` names (`NS` of `NS.Name`), which are not
    /// modelled: JSDoc type names through them are left unresolved.
    pub jsdoc_namespaces: Vec<Atom>,
    /// JSDoc type syntax written in a TypeScript type (`?T`, `T!`, `*`), in source order:
    /// accepted by the parser, reported by the checker (TS17019, TS17020, TS8020).
    pub jsdoc_syntax: Vec<JsDocSyntax>,
}

/// One piece of JSDoc type syntax in a TypeScript type (see [`Ast::jsdoc_syntax`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct JsDocSyntax {
    /// The whole construct (`?T`, `T!`, `*`).
    pub span: Span,
    pub kind: JsDocSyntaxKind,
    /// The type it wraps (`T`); `None` for `*`.
    pub ty: Option<NodeId>,
}

/// The kinds of [`JsDocSyntax`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JsDocSyntaxKind {
    /// `?T`: `T | null`.
    PrefixNullable,
    /// `T?`: `T | null`.
    PostfixNullable,
    /// `!T`: `T`.
    PrefixNonNull,
    /// `T!`: `T`.
    PostfixNonNull,
    /// `*`: `any`.
    All,
}

/// What a JSDoc tag says about its host (see [`Ast::jsdoc_tags`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JsDocTagKind {
    /// `@type {T}`.
    Type,
    /// `@param {T} name` (also `@arg`, `@argument`), matched to a parameter of the host.
    Param,
    /// `@returns {T}` / `@return {T}`.
    Returns,
    /// `@this {T}`.
    This,
    /// `@satisfies {T}`.
    Satisfies,
    /// `@template`.
    Template,
    /// `@typedef` / `@callback` (the alias is a synthesized statement).
    Typedef,
    /// `@enum {T}`.
    Enum,
    /// `@extends {T}` / `@augments {T}`.
    Extends,
    /// `@implements {T}`.
    Implements,
    /// `@constructor` / `@class`.
    Constructor,
    /// `@overload`.
    Overload,
    /// `@deprecated`.
    Deprecated,
    /// `@private` on an assignment statement (`this.x = v` in JavaScript); class members
    /// take it as a modifier instead.
    Private,
    /// `@protected` on an assignment statement, as `Private`.
    Protected,
    /// Any other tag.
    Other,
}

/// One JSDoc tag on a node. `ty` is the tag's type expression, if it has one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JsDocTag {
    pub host: NodeId,
    pub kind: JsDocTagKind,
    pub ty: Option<NodeId>,
    /// The span of the tag, from `@` to the end of its text.
    pub span: Span,
}

impl Ast {
    /// An empty AST. `source_len` sizes the arrays (about one node per 6 source bytes).
    pub fn new(source_kind: SourceKind, source_len: usize) -> Ast {
        let est = source_len / 6 + 16;
        let mut nodes = Vec::with_capacity(est);
        let mut spans = Vec::with_capacity(est);
        nodes.push(Node::Dummy {});
        spans.push(Span::EMPTY);
        let mut list_ranges = Vec::with_capacity(est / 4);
        list_ranges.push(ListRange::default());
        Ast {
            nodes,
            spans,
            list_items: Vec::with_capacity(est / 2),
            list_ranges,
            atoms: Interner::with_capacity(source_len / 40 + 16),
            comments: Vec::new(),
            modifier_tokens: Vec::new(),
            synthetic_comments: Vec::new(),
            class_semicolons: Vec::new(),
            root: NodeId(NonZeroU32::MIN),
            source_kind,
            jsdoc_start: u32::MAX,
            jsdoc_tags: Vec::new(),
            jsdoc_undo: Vec::new(),
            jsdoc_namespaces: Vec::new(),
            jsdoc_syntax: Vec::new(),
        }
    }

    /// Appends a node and returns its id.
    #[inline]
    pub fn add(&mut self, node: Node, span: Span) -> NodeId {
        let id = self.nodes.len() as u32;
        self.nodes.push(node);
        self.spans.push(span);
        // `nodes` always holds the dummy, so `id >= 1`.
        NodeId(NonZeroU32::new(id).unwrap_or(NonZeroU32::MIN))
    }

    /// Stores a list. Empty slices return `NodeList::EMPTY` without allocating.
    #[inline]
    pub fn add_list(&mut self, items: &[NodeId]) -> NodeList {
        if items.is_empty() {
            return NodeList::EMPTY;
        }
        let start = self.list_items.len() as u32;
        self.list_items.extend_from_slice(items);
        let id = self.list_ranges.len() as u32;
        self.list_ranges.push(ListRange {
            start,
            len: items.len() as u32,
        });
        NodeList(id)
    }

    #[inline]
    pub fn list(&self, list: NodeList) -> &[NodeId] {
        match self.list_ranges.get(list.0 as usize) {
            Some(r) => self
                .list_items
                .get(r.start as usize..(r.start + r.len) as usize)
                .unwrap_or(&[]),
            None => &[],
        }
    }

    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    #[inline]
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    #[inline]
    pub fn kind(&self, id: NodeId) -> NodeKind {
        self.nodes[id.index()].kind()
    }

    #[inline]
    pub fn span(&self, id: NodeId) -> Span {
        self.spans[id.index()]
    }

    #[inline]
    pub fn set_span(&mut self, id: NodeId, span: Span) {
        self.spans[id.index()] = span;
    }

    /// Whether `id` was built from a JSDoc comment (see [`Ast::jsdoc_start`]).
    #[inline]
    pub fn is_jsdoc(&self, id: NodeId) -> bool {
        id.raw() >= self.jsdoc_start
    }

    /// `node_mut` for JSDoc desugaring: a parsed node's original value is logged in
    /// [`Ast::jsdoc_undo`] first.
    pub fn jsdoc_node_mut(&mut self, id: NodeId) -> &mut Node {
        if !self.is_jsdoc(id) {
            let old = self.nodes[id.index()];
            self.jsdoc_undo.push((id, old));
        }
        &mut self.nodes[id.index()]
    }

    /// The tree as parsed, without what JSDoc desugaring added: rewritten nodes get their
    /// original values back and the JSDoc nodes and tags are dropped (emit prints this).
    pub fn strip_jsdoc(&mut self) {
        if self.jsdoc_start == u32::MAX {
            return;
        }
        while let Some((id, node)) = self.jsdoc_undo.pop() {
            self.nodes[id.index()] = node;
        }
        let n = (self.jsdoc_start as usize).min(self.nodes.len());
        self.nodes.truncate(n);
        self.spans.truncate(n);
        self.jsdoc_tags.clear();
        self.jsdoc_namespaces.clear();
        self.jsdoc_start = u32::MAX;
    }

    /// The JSDoc tags of `host`, in source order.
    pub fn jsdoc_tags_of(&self, host: NodeId) -> &[JsDocTag] {
        if self.jsdoc_tags.is_empty() {
            return &[];
        }
        let lo = self.jsdoc_tags.partition_point(|t| t.host < host);
        let hi = self.jsdoc_tags.partition_point(|t| t.host <= host);
        &self.jsdoc_tags[lo..hi]
    }

    /// Number of real nodes (excluding the dummy).
    pub fn node_count(&self) -> usize {
        self.nodes.len() - 1
    }

    #[inline]
    pub fn atom(&self, atom: Atom) -> &str {
        self.atoms.get(atom)
    }

    #[inline]
    pub fn intern(&mut self, s: &str) -> Atom {
        self.atoms.intern(s)
    }

    /// Calls `f` for each direct child of `id` in source order.
    #[inline]
    pub fn for_each_child<F: FnMut(NodeId)>(&self, id: NodeId, mut f: F) {
        self.nodes[id.index()].for_each_child(self, &mut f)
    }

    /// A copy of node `id` with every child `c` replaced by `f(self, c)`, in source order
    /// (see `Node::map_children`). The result is not added to the arena.
    #[inline]
    pub fn map_children<F: FnMut(&mut Ast, NodeId) -> NodeId>(
        &mut self,
        id: NodeId,
        f: &mut F,
    ) -> Node {
        let node = self.nodes[id.index()];
        node.map_children(self, f)
    }

    /// Direct children of `id` in source order.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut v = Vec::new();
        self.for_each_child(id, |c| v.push(c));
        v
    }

    /// The name of an `Ident`/`PrivateName`, or the value of a `StringLit`.
    pub fn name_of(&self, id: NodeId) -> Option<&str> {
        match *self.node(id) {
            Node::Ident { name } | Node::PrivateName { name } => Some(self.atom(name)),
            Node::StringLit { value } => Some(self.atom(value)),
            _ => None,
        }
    }

    /// Saves array lengths for rollback.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            nodes: self.nodes.len() as u32,
            list_items: self.list_items.len() as u32,
            list_ranges: self.list_ranges.len() as u32,
            comments: self.comments.len() as u32,
            modifier_tokens: self.modifier_tokens.len() as u32,
            jsdoc_syntax: self.jsdoc_syntax.len() as u32,
            class_semicolons: self.class_semicolons.len() as u32,
        }
    }

    /// Drops everything added after `cp`. Interned atoms are kept.
    pub fn rollback(&mut self, cp: Checkpoint) {
        self.nodes.truncate(cp.nodes as usize);
        self.spans.truncate(cp.nodes as usize);
        self.list_items.truncate(cp.list_items as usize);
        self.list_ranges.truncate(cp.list_ranges as usize);
        self.comments.truncate(cp.comments as usize);
        self.modifier_tokens.truncate(cp.modifier_tokens as usize);
        self.jsdoc_syntax.truncate(cp.jsdoc_syntax as usize);
        self.class_semicolons.truncate(cp.class_semicolons as usize);
    }

    /// Modifier keywords whose start lies inside `span`.
    pub fn modifiers_in(&self, span: Span) -> &[ModifierToken] {
        let lo = self
            .modifier_tokens
            .partition_point(|m| m.span.start < span.start);
        let hi = self
            .modifier_tokens
            .partition_point(|m| m.span.start < span.end);
        &self.modifier_tokens[lo..hi.max(lo)]
    }

    /// The modifier keywords written on `id`, in source order (repeats included). They lie
    /// between the node's decorators (or its start) and its first other child.
    pub fn modifier_tokens_of(&self, id: NodeId) -> &[ModifierToken] {
        let span = self.span(id);
        let (mut lo, mut hi) = (span.start, span.end);
        let mut found = false;
        self.for_each_child(id, |c| {
            if found {
                return;
            }
            let cs = self.span(c);
            if self.kind(c) == NodeKind::Decorator {
                lo = lo.max(cs.end);
            } else {
                hi = cs.start;
                found = true;
            }
        });
        self.modifiers_in(Span::new(lo, hi.max(lo)))
    }

    /// Comments whose start lies inside `span`.
    pub fn comments_in(&self, span: Span) -> &[Comment] {
        let lo = self.comments.partition_point(|c| c.span.start < span.start);
        let hi = self.comments.partition_point(|c| c.span.start < span.end);
        &self.comments[lo..hi.max(lo)]
    }

    /// Comments that end at or before `pos` and start at or after `after`: the comments
    /// between a previous token ending at `after` and a node starting at `pos`.
    pub fn comments_between(&self, after: u32, pos: u32) -> &[Comment] {
        let lo = self.comments.partition_point(|c| c.span.start < after);
        let hi = self
            .comments
            .partition_point(|c| c.span.end <= pos && c.span.start < pos);
        &self.comments[lo..hi.max(lo)]
    }

    /// Attaches `text` (a whole comment, delimiters included) to `node`, printed before it or,
    /// with `trailing`, after it. Several comments on one node print in insertion order.
    pub fn add_synthetic_comment(&mut self, node: NodeId, text: &str, trailing: bool) {
        let text = self.intern(text);
        let at = self.synthetic_comments.partition_point(|c| c.node <= node);
        self.synthetic_comments.insert(
            at,
            SyntheticComment {
                node,
                text,
                trailing,
            },
        );
    }

    /// The comments attached to `node` by [`Ast::add_synthetic_comment`], in insertion order.
    pub fn synthetic_comments_of(&self, node: NodeId) -> &[SyntheticComment] {
        if self.synthetic_comments.is_empty() {
            return &[];
        }
        let lo = self.synthetic_comments.partition_point(|c| c.node < node);
        let hi = self.synthetic_comments.partition_point(|c| c.node <= node);
        &self.synthetic_comments[lo..hi]
    }

    /// An indented text dump of the tree under `root`, for tests and debugging.
    pub fn dump(&self) -> String {
        dump::dump(self, self.root, None)
    }

    /// Like [`Ast::dump`], with each leaf's source text appended.
    pub fn dump_with_source(&self, src: &str) -> String {
        dump::dump(self, self.root, Some(src))
    }

    /// Dumps one subtree.
    pub fn dump_node(&self, id: NodeId) -> String {
        dump::dump(self, id, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_is_compact() {
        assert_eq!(std::mem::size_of::<Node>(), 24);
        assert_eq!(std::mem::size_of::<Option<NodeId>>(), 4);
        assert_eq!(std::mem::size_of::<Span>(), 8);
        assert!(NodeKind::ALL.len() < 256);
    }

    #[test]
    fn build_and_walk() {
        let mut ast = Ast::new(SourceKind::Ts, 10);
        let a = ast.intern("a");
        let x = ast.add(Node::Ident { name: a }, Span::new(0, 1));
        let one = ast.add(
            Node::NumberLit {
                value: F64Bits::new(1.0),
            },
            Span::new(4, 5),
        );
        let bin = ast.add(
            Node::Binary {
                left: x,
                op: BinaryOp::Add,
                right: one,
            },
            Span::new(0, 5),
        );
        let stmt = ast.add(Node::ExprStmt { expr: bin }, Span::new(0, 6));
        let stmts = ast.add_list(&[stmt]);
        ast.root = ast.add(Node::Program { stmts }, Span::new(0, 6));
        assert_eq!(ast.children(bin), vec![x, one]);
        assert_eq!(ast.kind(ast.root), NodeKind::Program);
        assert_eq!(ast.name_of(x), Some("a"));
        assert_eq!(
            ast.dump(),
            "Program 0..6\n  stmts[0]: ExprStmt 0..6\n    expr: Binary 0..5 op=Add\n      left: Ident 0..1 name=\"a\"\n      right: NumberLit 4..5 value=1\n"
        );
        let parents = ParentMap::build(&ast);
        assert_eq!(parents.parent(x), Some(bin));
        assert_eq!(parents.parent(ast.root), None);
    }

    /// `x + 1; f(a, b);` as a Program; returns the root.
    fn small_program(ast: &mut Ast) -> NodeId {
        let name = |ast: &mut Ast, s: &str, at: u32| {
            let a = ast.intern(s);
            ast.add(Node::Ident { name: a }, Span::new(at, at + 1))
        };
        let x = name(ast, "x", 0);
        let one = ast.add(
            Node::NumberLit {
                value: F64Bits::new(1.0),
            },
            Span::new(4, 5),
        );
        let bin = ast.add(
            Node::Binary {
                left: x,
                op: BinaryOp::Add,
                right: one,
            },
            Span::new(0, 5),
        );
        let s1 = ast.add(Node::ExprStmt { expr: bin }, Span::new(0, 6));
        let f = name(ast, "f", 7);
        let a = name(ast, "a", 9);
        let b = name(ast, "b", 12);
        let args = ast.add_list(&[a, b]);
        let call = ast.add(
            Node::Call {
                callee: f,
                optional: false,
                type_args: None,
                args,
            },
            Span::new(7, 14),
        );
        let s2 = ast.add(Node::ExprStmt { expr: call }, Span::new(7, 15));
        let stmts = ast.add_list(&[s1, s2]);
        ast.add(Node::Program { stmts }, Span::new(0, 15))
    }

    fn deep_clone(ast: &mut Ast, id: NodeId) -> NodeId {
        let node = ast.map_children(id, &mut deep_clone);
        let span = ast.span(id);
        ast.add(node, span)
    }

    #[test]
    fn map_children_identity_keeps_lists() {
        let mut ast = Ast::new(SourceKind::Ts, 15);
        ast.root = small_program(&mut ast);
        let lists = ast.list_ranges.len();
        for i in 1..=ast.node_count() {
            let id = NodeId::from_raw(i as u32).unwrap();
            let node = *ast.node(id);
            let mut seen = Vec::new();
            let mapped = node.map_children(&mut ast, &mut |_, c| {
                seen.push(c);
                c
            });
            assert_eq!(mapped, node);
            assert_eq!(seen, ast.children(id), "source order for {:?}", node.kind());
        }
        assert_eq!(
            ast.list_ranges.len(),
            lists,
            "unchanged lists are not copied"
        );
    }

    #[test]
    fn map_children_deep_clone() {
        let mut ast = Ast::new(SourceKind::Ts, 15);
        let root = small_program(&mut ast);
        ast.root = root;
        let before = ast.dump();
        let count = ast.node_count();
        let copy = deep_clone(&mut ast, root);
        assert_eq!(ast.node_count(), 2 * count);
        // The copy is a fresh tree (children before parents) that dumps the same.
        ast.root = copy;
        assert_eq!(ast.dump(), before);
        for c in ast.children(copy) {
            assert!(c.index() > count && c < copy);
        }
        // Only the changed child is replaced; the list is copied once.
        let Node::Call { args, .. } = *ast.node(NodeId::from_raw(8).unwrap()) else {
            panic!("expected the call");
        };
        let first = ast.list(args)[0];
        let lists = ast.list_ranges.len();
        let node = *ast.node(NodeId::from_raw(8).unwrap());
        let this = ast.add(Node::This {}, Span::new(9, 10));
        let mapped = node.map_children(&mut ast, &mut |_, c| if c == first { this } else { c });
        let Node::Call { args: new_args, .. } = mapped else {
            panic!()
        };
        assert_eq!(ast.list_ranges.len(), lists + 1);
        assert_eq!(ast.list(new_args)[0], this);
        assert_eq!(ast.list(new_args)[1], ast.list(args)[1]);
    }

    #[test]
    fn synthetic_comments_by_node() {
        let mut ast = Ast::new(SourceKind::Ts, 15);
        ast.root = small_program(&mut ast);
        let id = |i| NodeId::from_raw(i).unwrap();
        assert!(ast.synthetic_comments_of(id(3)).is_empty());
        ast.add_synthetic_comment(id(5), "/* b */", true);
        ast.add_synthetic_comment(id(3), "/* a1 */", false);
        ast.add_synthetic_comment(id(9), "/* c */", true);
        ast.add_synthetic_comment(id(3), "/* a2 */", true);
        let texts = |ast: &Ast, n| -> Vec<(String, bool)> {
            ast.synthetic_comments_of(id(n))
                .iter()
                .map(|c| (ast.atom(c.text).to_owned(), c.trailing))
                .collect()
        };
        assert_eq!(
            texts(&ast, 3),
            [
                ("/* a1 */".to_owned(), false),
                ("/* a2 */".to_owned(), true)
            ]
        );
        assert_eq!(texts(&ast, 5), [("/* b */".to_owned(), true)]);
        assert!(texts(&ast, 4).is_empty());
        let nodes: Vec<u32> = ast
            .synthetic_comments
            .iter()
            .map(|c| c.node.raw())
            .collect();
        assert_eq!(nodes, [3, 3, 5, 9]);
    }

    #[test]
    fn rollback() {
        let mut ast = Ast::new(SourceKind::Ts, 0);
        let cp = ast.checkpoint();
        let id = ast.add(Node::This {}, Span::new(0, 4));
        ast.add_list(&[id]);
        ast.rollback(cp);
        assert_eq!(ast.node_count(), 0);
        assert_eq!(ast.list_ranges.len(), 1);
        assert!(ast.list(NodeList::EMPTY).is_empty());
    }

    #[test]
    fn comments_lookup() {
        let mut ast = Ast::new(SourceKind::Ts, 0);
        for (s, e) in [(0, 5), (10, 15), (20, 25)] {
            ast.comments.push(Comment {
                span: Span::new(s, e),
                kind: CommentKind::Block,
                newline_before: false,
            });
        }
        assert_eq!(ast.comments_in(Span::new(9, 21)).len(), 2);
        assert_eq!(ast.comments_between(6, 20).len(), 1);
        assert_eq!(ast.comments_between(0, 30).len(), 3);
        assert!(ast.comments_between(16, 18).is_empty());
    }

    #[test]
    fn source_kind_from_path() {
        assert_eq!(SourceKind::from_path("a/b.d.ts"), SourceKind::Dts);
        assert_eq!(SourceKind::from_path("x.d.mts"), SourceKind::Dts);
        assert_eq!(SourceKind::from_path("styles.d.css.ts"), SourceKind::Dts);
        assert_eq!(SourceKind::from_path("a.d/b.ts"), SourceKind::Ts);
        assert_eq!(SourceKind::from_path("a.tsx"), SourceKind::Tsx);
        assert_eq!(SourceKind::from_path("a.mjs"), SourceKind::Js);
        assert_eq!(SourceKind::from_path("a.jsx"), SourceKind::Jsx);
        assert_eq!(SourceKind::from_path("a.mts"), SourceKind::Ts);
        assert!(SourceKind::Js.is_jsx());
        assert!(!SourceKind::Js.is_typescript());
    }
}
