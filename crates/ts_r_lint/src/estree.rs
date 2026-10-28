//! An ESTree view of a file, the tree ESLint selectors are written against
//! (`@typescript-eslint/typescript-estree` node types, field names and visitor-key order).
//!
//! Our AST is close to ESTree but not the same: declarations carry `export` as a modifier,
//! parentheses are nodes, type annotations are bare types and parameters are one node. This
//! module builds the ESTree shape once, as a flat array, so [`crate::esquery`] selectors can
//! walk parents, fields and list positions the way ESLint does. Every ESTree node remembers
//! the AST node it came from; wrappers that have no AST node of their own (`ChainExpression`,
//! `TSTypeAnnotation`, `ClassBody`, ...) point at the node they wrap and are marked
//! [`Origin::Synthetic`].

use std::borrow::Cow;

use ts_r_ast::{
    Ast, FnFlags, HeritageKind, KeywordTypeKind, MappedModifier, MetaPropKind, MethodKind,
    Modifiers, ModuleKind, Node, NodeId, TypeOperatorKind, VarKind,
};
use ts_r_span::Span;

use crate::esquery::{Tree, Val};

const NONE: u32 = u32::MAX;
/// Subtrees deeper than this are left out (the builder recurses).
const MAX_DEPTH: u32 = 400;

/// Where an ESTree node came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The ESTree counterpart of the AST node.
    Real,
    /// The `export` declaration around an exported declaration.
    Export,
    /// A wrapper or second copy with no AST node of its own.
    Synthetic,
}

#[derive(Debug, Clone, Copy)]
enum Slot {
    Node(u32),
    Null,
    List(u32, u32),
}

#[derive(Debug)]
pub struct ENode {
    pub ty: &'static str,
    pub ast: NodeId,
    pub origin: Origin,
    pub span: Span,
    parent: u32,
    key: &'static str,
    idx: u32,
    /// Length of the list holding the node; 0 when it is not in a list.
    len: u32,
    fields: (u32, u32),
    kids: (u32, u32),
}

/// The ESTree view of one file.
#[derive(Debug)]
pub struct EsTree<'a> {
    ast: &'a Ast,
    src: &'a str,
    pub nodes: Vec<ENode>,
    fields: Vec<(&'static str, Slot)>,
    items: Vec<u32>,
    kids: Vec<u32>,
    /// AST node index to its `Real` ESTree node.
    real: Vec<u32>,
    /// AST node index to its `export` wrapper.
    export: Vec<u32>,
}

impl<'a> EsTree<'a> {
    pub fn build(ast: &'a Ast, src: &'a str) -> EsTree<'a> {
        let mut b = Builder {
            t: EsTree {
                ast,
                src,
                nodes: Vec::with_capacity(ast.node_count()),
                fields: Vec::new(),
                items: Vec::new(),
                kids: Vec::new(),
                real: vec![NONE; ast.node_count() + 1],
                export: vec![NONE; ast.node_count() + 1],
            },
            pending: Vec::new(),
            pitems: Vec::new(),
            cur: NONE,
            key: "",
            idx: 0,
            len: 0,
            depth: 0,
            chain: false,
            decos: None,
        };
        b.program(ast.root);
        b.t
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The ESTree node of an AST node (`export` wrapper when `export` is true).
    pub fn of_ast(&self, id: NodeId, export: bool) -> Option<u32> {
        let v = if export { &self.export } else { &self.real };
        v.get(id.index()).copied().filter(|&n| n != NONE)
    }

    pub fn span(&self, n: u32) -> Span {
        self.nodes[n as usize].span
    }

    /// The node in field `key` of `n` (`None` for null, a list or no such field).
    pub fn node_field(&self, n: u32, key: &str) -> Option<u32> {
        match self.field(n, key)? {
            Slot::Node(c) => Some(c),
            _ => None,
        }
    }

    /// The items of list field `key` of `n` (holes are `None`); `None` when it isn't a list.
    pub fn list_field(&self, n: u32, key: &str) -> Option<Vec<Option<u32>>> {
        match self.field(n, key)? {
            Slot::List(s, l) => Some(
                self.items[s as usize..(s + l) as usize]
                    .iter()
                    .map(|&m| (m != NONE).then_some(m))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Whether `n` has a field `key` holding a node, a list or null.
    pub fn has_field(&self, n: u32, key: &str) -> bool {
        self.field(n, key).is_some()
    }

    fn field(&self, n: u32, key: &str) -> Option<Slot> {
        let (s, l) = self.nodes[n as usize].fields;
        self.fields[s as usize..(s + l) as usize]
            .iter()
            .find(|(k, _)| *k == key)
            .map(|&(_, slot)| slot)
    }

    fn atom(&self, a: ts_r_ast::Atom) -> Val<'a> {
        Val::Str(Cow::Borrowed(self.ast.atom(a)))
    }

    fn text(&self, span: Span) -> &'a str {
        span.text(self.src)
    }

    /// The scalar properties of a node.
    fn scalar(&self, n: u32, key: &str) -> Val<'a> {
        let e = &self.nodes[n as usize];
        let ast = self.ast;
        let node = ast.node(e.ast);
        let b = Val::Bool;
        let s = |v: &'static str| Val::Str(Cow::Borrowed(v));
        match (e.ty, key) {
            ("Identifier" | "JSXIdentifier" | "PrivateIdentifier", "name") => match *node {
                Node::Ident { name } | Node::PrivateName { name } => self.atom(name),
                Node::This { .. } => s("this"),
                Node::MetaProperty { kind } => s(match (kind, e.key) {
                    (MetaPropKind::NewTarget, "meta") => "new",
                    (MetaPropKind::NewTarget, _) => "target",
                    (MetaPropKind::ImportMeta, "meta") => "import",
                    (MetaPropKind::ImportMeta, _) => "meta",
                }),
                _ => Val::Str(Cow::Borrowed(self.text(e.span))),
            },
            ("Identifier", "optional") => b(self.ident_optional(n)),
            ("Literal", "value") => match *node {
                Node::NullLit {} => Val::Null,
                Node::BoolLit { value } => b(value),
                Node::NumberLit { value } => Val::Num(value.get()),
                Node::StringLit { value } => self.atom(value),
                Node::Directive { value } => self.atom(value),
                Node::BigIntLit { value } => self.atom(value),
                Node::RegexLit { .. } => Val::Node,
                _ => Val::Undef,
            },
            ("Literal", "raw") | ("JSXText", "raw") => Val::Str(Cow::Borrowed(self.text(e.span))),
            ("JSXText", "value") => Val::Str(Cow::Borrowed(self.text(e.span))),
            ("Literal", "regex") => match node {
                Node::RegexLit { .. } => Val::Node,
                _ => Val::Undef,
            },
            ("Literal", "bigint") => match *node {
                Node::BigIntLit { value } => self.atom(value),
                _ => Val::Undef,
            },
            ("ExpressionStatement", "directive") => match node {
                Node::Directive { .. } => {
                    let raw = self.text(self.literal_span(e.span));
                    Val::Str(Cow::Borrowed(
                        raw.get(1..raw.len().saturating_sub(1)).unwrap_or(""),
                    ))
                }
                _ => Val::Undef,
            },
            ("TemplateElement", "tail") => b(self.next_is_backtick(e.span)),
            ("TemplateElement", "value") => Val::Node,
            (_, "operator") => match *node {
                Node::Unary { op, .. } => s(op.as_str()),
                Node::Update { op, .. } => s(op.as_str()),
                Node::Binary { op, .. } => s(op.as_str()),
                Node::Assign { op, .. } => s(op.as_str()),
                Node::TypeOperator { op, .. } => s(match op {
                    TypeOperatorKind::KeyOf => "keyof",
                    TypeOperatorKind::Unique => "unique",
                    TypeOperatorKind::Readonly => "readonly",
                }),
                _ => Val::Undef,
            },
            ("UnaryExpression", "prefix") => b(true),
            ("UpdateExpression", "prefix") => match *node {
                Node::Update { prefix, .. } => b(prefix),
                _ => Val::Undef,
            },
            ("MemberExpression", "computed") => b(matches!(node, Node::Index { .. })),
            ("MemberExpression" | "CallExpression", "optional") => match *node {
                Node::Member { optional, .. }
                | Node::Index { optional, .. }
                | Node::Call { optional, .. } => b(optional),
                _ => Val::Undef,
            },
            ("VariableDeclaration", "kind") => match *node {
                Node::VarDecl { kind, .. } => s(match kind {
                    VarKind::Var => "var",
                    VarKind::Let => "let",
                    VarKind::Const => "const",
                    VarKind::Using => "using",
                    VarKind::AwaitUsing => "await using",
                }),
                _ => Val::Undef,
            },
            ("VariableDeclarator", "definite") => match *node {
                Node::VarDeclarator { definite, .. } => b(definite),
                _ => Val::Undef,
            },
            (
                "FunctionDeclaration"
                | "FunctionExpression"
                | "ArrowFunctionExpression"
                | "TSDeclareFunction"
                | "TSEmptyBodyFunctionExpression",
                "async" | "generator" | "expression",
            ) => {
                let flags = match *node {
                    Node::FunctionDecl { flags, .. }
                    | Node::FunctionExpr { flags, .. }
                    | Node::Arrow { flags, .. } => flags,
                    _ => FnFlags::NONE,
                };
                b(flags.contains(match key {
                    "async" => FnFlags::ASYNC,
                    "generator" => FnFlags::GENERATOR,
                    _ => FnFlags::EXPRESSION_BODY,
                }))
            }
            ("Property", "kind") => s(match *node {
                Node::MethodDef {
                    kind: MethodKind::Get,
                    ..
                } => "get",
                Node::MethodDef {
                    kind: MethodKind::Set,
                    ..
                } => "set",
                _ => "init",
            }),
            ("Property", "method") => b(matches!(
                *node,
                Node::MethodDef {
                    kind: MethodKind::Method,
                    ..
                }
            )),
            ("Property", "shorthand") => b(match *node {
                Node::ShorthandProp { .. } => true,
                Node::BindingProperty { value, .. } => value.is_none(),
                _ => false,
            }),
            ("MethodDefinition" | "TSAbstractMethodDefinition", "kind") => match *node {
                Node::MethodDef { kind, .. } => s(match kind {
                    MethodKind::Method => "method",
                    MethodKind::Get => "get",
                    MethodKind::Set => "set",
                    MethodKind::Constructor => "constructor",
                }),
                _ => Val::Undef,
            },
            ("TSMethodSignature", "kind") => match *node {
                Node::MethodSignature { kind, .. } => s(match kind {
                    MethodKind::Get => "get",
                    MethodKind::Set => "set",
                    _ => "method",
                }),
                _ => Val::Undef,
            },
            ("TSMappedType", "optional" | "readonly") => match *node {
                Node::MappedType {
                    optional, readonly, ..
                } => match if key == "optional" {
                    optional
                } else {
                    readonly
                } {
                    MappedModifier::None => Val::Undef,
                    MappedModifier::Present => b(true),
                    MappedModifier::Add => s("+"),
                    MappedModifier::Remove => s("-"),
                },
                _ => Val::Undef,
            },
            (_, "computed") => match *node {
                Node::Property { key, .. }
                | Node::MethodDef { key, .. }
                | Node::PropertyDef { key, .. }
                | Node::PropertySignature { key, .. }
                | Node::MethodSignature { key, .. } => {
                    b(matches!(ast.node(key), Node::ComputedKey { .. }))
                }
                Node::BindingProperty { key, .. } => {
                    b(matches!(ast.node(key), Node::ComputedKey { .. }))
                }
                Node::ShorthandProp { .. } => b(false),
                _ => Val::Undef,
            },
            (_, "optional") => match *node {
                Node::MethodDef { optional, .. }
                | Node::PropertyDef { optional, .. }
                | Node::PropertySignature { optional, .. }
                | Node::MethodSignature { optional, .. }
                | Node::NamedTupleMember { optional, .. } => b(optional),
                _ => Val::Undef,
            },
            (_, "static" | "declare" | "readonly" | "abstract" | "override" | "const") => {
                let m = node.modifiers();
                let flag = match key {
                    "static" => Modifiers::STATIC,
                    "declare" => Modifiers::DECLARE,
                    "readonly" => Modifiers::READONLY,
                    "abstract" => Modifiers::ABSTRACT,
                    "override" => Modifiers::OVERRIDE,
                    _ => Modifiers::CONST,
                };
                match *node {
                    Node::ConstructorType { is_abstract, .. } if key == "abstract" => {
                        b(is_abstract)
                    }
                    _ => b(m.contains(flag)),
                }
            }
            (_, "accessibility") => {
                let m = node.modifiers();
                if m.contains(Modifiers::PUBLIC) {
                    s("public")
                } else if m.contains(Modifiers::PRIVATE) {
                    s("private")
                } else if m.contains(Modifiers::PROTECTED) {
                    s("protected")
                } else {
                    Val::Undef
                }
            }
            (_, "definite") => match *node {
                Node::PropertyDef { definite, .. } => b(definite),
                _ => Val::Undef,
            },
            ("ForOfStatement", "await") => match *node {
                Node::ForOf { is_await, .. } => b(is_await),
                _ => Val::Undef,
            },
            ("YieldExpression", "delegate") => match *node {
                Node::Yield { delegate, .. } => b(delegate),
                _ => Val::Undef,
            },
            (_, "importKind" | "exportKind") => {
                let type_only = match *node {
                    Node::ImportDecl { type_only, .. }
                    | Node::ImportSpecifier { type_only, .. }
                    | Node::ImportEquals { type_only, .. }
                    | Node::ExportNamed { type_only, .. }
                    | Node::ExportSpecifier { type_only, .. }
                    | Node::ExportAll { type_only, .. } => type_only,
                    Node::Interface { .. } | Node::TypeAlias { .. } => true,
                    _ => false,
                };
                s(if type_only { "type" } else { "value" })
            }
            ("JSXOpeningElement", "selfClosing") => match *node {
                Node::JsxOpening { self_closing, .. } => b(self_closing),
                _ => Val::Undef,
            },
            ("TSModuleDeclaration", "kind") => match *node {
                Node::Module { kind, .. } => s(match kind {
                    ModuleKind::Namespace => "namespace",
                    ModuleKind::Module => "module",
                    ModuleKind::Global => "global",
                }),
                _ => Val::Undef,
            },
            ("TSModuleDeclaration", "global") => b(matches!(
                node,
                Node::Module {
                    kind: ModuleKind::Global,
                    ..
                }
            )),
            ("TSTypePredicate", "asserts") => match *node {
                Node::TypePredicate { asserts, .. } => b(asserts),
                _ => Val::Undef,
            },
            ("TSTypeParameter", "in" | "out") => b(node.modifiers().contains(if key == "in" {
                Modifiers::IN
            } else {
                Modifiers::OUT
            })),
            ("ImportDeclaration" | "ImportExpression", "phase") => match *node {
                Node::ImportDecl { phase, .. } | Node::ImportCall { phase, .. } => s(match phase {
                    ts_r_ast::ImportPhase::Evaluation => return Val::Null,
                    ts_r_ast::ImportPhase::Defer => "defer",
                    ts_r_ast::ImportPhase::Source => "source",
                }),
                _ => Val::Undef,
            },
            _ => Val::Undef,
        }
    }

    /// `regex.pattern`, `regex.flags`, `value.raw`, `value.cooked`.
    fn sub_scalar(&self, n: u32, key: &str, sub: &str) -> Val<'a> {
        let e = &self.nodes[n as usize];
        match (self.ast.node(e.ast), key, sub) {
            (Node::RegexLit { pattern, .. }, "regex", "pattern") => self.atom(*pattern),
            (Node::RegexLit { flags, .. }, "regex", "flags") => self.atom(*flags),
            (Node::TemplateElement { .. }, "value", "raw") => {
                Val::Str(Cow::Borrowed(self.text(self.ast.span(e.ast))))
            }
            (
                Node::TemplateElement {
                    cooked,
                    invalid_escape,
                },
                "value",
                "cooked",
            ) => {
                if *invalid_escape {
                    Val::Null
                } else {
                    self.atom(*cooked)
                }
            }
            _ => Val::Undef,
        }
    }

    fn ident_optional(&self, n: u32) -> bool {
        let e = &self.nodes[n as usize];
        if e.parent == NONE {
            return false;
        }
        let p = &self.nodes[e.parent as usize];
        matches!(*self.ast.node(p.ast), Node::Param { optional: true, init: None, rest: false, pattern, .. } if pattern == e.ast)
    }

    fn literal_span(&self, stmt: Span) -> Span {
        let text = self.text(stmt);
        let mut chars = text.char_indices();
        let Some((_, q)) = chars.next() else {
            return stmt;
        };
        let mut escaped = false;
        for (i, c) in chars {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                return Span::new(stmt.start, stmt.start + i as u32 + 1);
            }
        }
        stmt
    }

    fn next_is_backtick(&self, span: Span) -> bool {
        self.src.as_bytes().get(span.end as usize - 1) == Some(&b'`')
    }
}

impl Tree for EsTree<'_> {
    fn ty(&self, n: u32) -> &str {
        self.nodes[n as usize].ty
    }

    fn parent(&self, n: u32) -> Option<u32> {
        let p = self.nodes[n as usize].parent;
        (p != NONE).then_some(p)
    }

    fn key(&self, n: u32) -> &str {
        self.nodes[n as usize].key
    }

    fn list_pos(&self, n: u32) -> Option<(u32, u32)> {
        let e = &self.nodes[n as usize];
        (e.len > 0).then_some((e.idx, e.len))
    }

    fn list_item(&self, n: u32, index: u32) -> Option<u32> {
        let e = &self.nodes[n as usize];
        let p = self.parent(n)?;
        match self.field(p, e.key)? {
            Slot::List(s, l) if index < l => {
                Some(self.items[(s + index) as usize]).filter(|&m| m != NONE)
            }
            _ => None,
        }
    }

    fn children(&self, n: u32) -> &[u32] {
        let (s, l) = self.nodes[n as usize].kids;
        &self.kids[s as usize..(s + l) as usize]
    }

    fn attr(&self, n: u32, path: &[&str]) -> Val<'_> {
        let mut cur = n;
        let mut i = 0;
        while i < path.len() {
            let seg = path[i];
            let last = i + 1 == path.len();
            if seg == "type" {
                return if last {
                    Val::Str(Cow::Borrowed(self.nodes[cur as usize].ty))
                } else {
                    str_prop(
                        self.nodes[cur as usize].ty,
                        path[i + 1],
                        i + 2 == path.len(),
                    )
                };
            }
            if seg == "parent" {
                match self.parent(cur) {
                    Some(p) => {
                        cur = p;
                        i += 1;
                        if last {
                            return Val::Node;
                        }
                        continue;
                    }
                    None => return Val::Undef,
                }
            }
            match self.field(cur, seg) {
                Some(Slot::Node(c)) => {
                    if last {
                        return Val::Node;
                    }
                    cur = c;
                }
                Some(Slot::Null) => return Val::Null,
                Some(Slot::List(s, l)) => {
                    if last {
                        return Val::List(l);
                    }
                    let next = path[i + 1];
                    if next == "length" {
                        return if i + 2 == path.len() {
                            Val::Num(f64::from(l))
                        } else {
                            Val::Undef
                        };
                    }
                    let Ok(k) = next.parse::<u32>() else {
                        return Val::Undef;
                    };
                    if k >= l {
                        return Val::Undef;
                    }
                    let item = self.items[(s + k) as usize];
                    if item == NONE {
                        return Val::Null;
                    }
                    if i + 2 == path.len() {
                        return Val::Node;
                    }
                    cur = item;
                    i += 1;
                }
                None => {
                    let v = self.scalar(cur, seg);
                    if last {
                        return v;
                    }
                    return match v {
                        Val::Node if i + 2 == path.len() => self.sub_scalar(cur, seg, path[i + 1]),
                        Val::Str(s) => str_prop(&s, path[i + 1], i + 2 == path.len()),
                        Val::Null => Val::Null,
                        _ => Val::Undef,
                    };
                }
            }
            i += 1;
        }
        Val::Node
    }
}

/// JavaScript whitespace and line terminators.
fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn str_prop(s: &str, key: &str, last: bool) -> Val<'static> {
    if key == "length" && last {
        Val::Num(s.encode_utf16().count() as f64)
    } else {
        Val::Undef
    }
}

enum PSlot {
    Node(u32),
    Null,
    List(u32, u32),
}

struct Builder<'a> {
    t: EsTree<'a>,
    pending: Vec<(&'static str, PSlot, u32)>,
    pitems: Vec<u32>,
    cur: u32,
    key: &'static str,
    idx: u32,
    len: u32,
    depth: u32,
    /// The node being built continues an optional chain (no `ChainExpression` of its own).
    chain: bool,
    /// Parameter decorators for the pattern about to be built.
    decos: Option<ts_r_ast::NodeList>,
}

type Built = Option<u32>;

impl<'a> Builder<'a> {
    fn ast(&self) -> &'a Ast {
        self.t.ast
    }

    fn sp(&self, id: NodeId) -> Span {
        self.t.ast.span(id)
    }

    fn src(&self) -> &'a str {
        self.t.src
    }

    fn open(&mut self, ty: &'static str, ast: NodeId, span: Span, origin: Origin) -> u32 {
        let id = self.t.nodes.len() as u32;
        self.t.nodes.push(ENode {
            ty,
            ast,
            origin,
            span,
            parent: self.cur,
            key: self.key,
            idx: self.idx,
            len: self.len,
            fields: (self.pending.len() as u32, self.pitems.len() as u32),
            kids: (0, 0),
        });
        let slot = match origin {
            Origin::Real => self.t.real.get_mut(ast.index()),
            Origin::Export => self.t.export.get_mut(ast.index()),
            Origin::Synthetic => None,
        };
        if let Some(s) = slot
            && *s == NONE
        {
            *s = id;
        }
        self.cur = id;
        self.depth += 1;
        id
    }

    fn close(&mut self, id: u32) {
        let e = &self.t.nodes[id as usize];
        let (fmark, imark) = e.fields;
        let parent = e.parent;
        let fstart = self.t.fields.len() as u32;
        let kstart = self.t.kids.len() as u32;
        for (key, slot, _) in self.pending.drain(fmark as usize..) {
            let s = match slot {
                PSlot::Node(c) => {
                    self.t.kids.push(c);
                    Slot::Node(c)
                }
                PSlot::Null => Slot::Null,
                PSlot::List(s, l) => {
                    let gs = self.t.items.len() as u32;
                    let items = &self.pitems[s as usize..(s + l) as usize];
                    self.t.items.extend_from_slice(items);
                    self.t
                        .kids
                        .extend(items.iter().copied().filter(|&c| c != NONE));
                    Slot::List(gs, l)
                }
            };
            self.t.fields.push((key, s));
        }
        self.pitems.truncate(imark as usize);
        let e = &mut self.t.nodes[id as usize];
        e.fields = (fstart, self.t.fields.len() as u32 - fstart);
        e.kids = (kstart, self.t.kids.len() as u32 - kstart);
        self.cur = parent;
        self.depth -= 1;
    }

    /// Builds a node with `body` adding its fields.
    fn n(
        &mut self,
        ty: &'static str,
        ast: NodeId,
        span: Span,
        origin: Origin,
        body: impl FnOnce(&mut Self),
    ) -> Built {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        let id = self.open(ty, ast, span, origin);
        body(self);
        self.close(id);
        Some(id)
    }

    fn real(&mut self, ty: &'static str, ast: NodeId, body: impl FnOnce(&mut Self)) -> Built {
        let span = self.sp(ast);
        self.n(ty, ast, span, Origin::Real, body)
    }

    fn leaf(&mut self, ty: &'static str, ast: NodeId) -> Built {
        self.real(ty, ast, |_| {})
    }

    fn one(&mut self, key: &'static str, f: impl FnOnce(&mut Self) -> Built) {
        let saved = (self.key, self.idx, self.len);
        self.key = key;
        self.len = 0;
        let r = f(self);
        (self.key, self.idx, self.len) = saved;
        self.pending
            .push((key, r.map_or(PSlot::Null, PSlot::Node), 0));
    }

    fn opt(&mut self, key: &'static str, id: Option<NodeId>) {
        self.one(key, |b| id.and_then(|i| b.node(i)));
    }

    fn req(&mut self, key: &'static str, id: NodeId) {
        self.one(key, |b| b.node(id));
    }

    fn many(
        &mut self,
        key: &'static str,
        ids: &[NodeId],
        mut f: impl FnMut(&mut Self, NodeId) -> Built,
    ) {
        let saved = (self.key, self.idx, self.len);
        let start = self.pitems.len() as u32;
        let len = ids.len() as u32;
        for (i, &id) in ids.iter().enumerate() {
            self.key = key;
            self.idx = i as u32;
            self.len = len;
            let r = f(self, id);
            self.pitems.push(r.unwrap_or(NONE));
        }
        (self.key, self.idx, self.len) = saved;
        self.pending.push((key, PSlot::List(start, len), 0));
    }

    fn list(&mut self, key: &'static str, l: ts_r_ast::NodeList) {
        let ids = self.ast().list(l);
        self.many(key, ids, |b, id| b.node(id));
    }

    fn empty(&mut self, key: &'static str) {
        self.many(key, &[], |_, _| None);
    }

    fn null(&mut self, key: &'static str) {
        self.one(key, |_| None);
    }

    /// The start of the first token at or after `pos` (skips whitespace and comments).
    fn skip_trivia(&self, mut pos: u32) -> u32 {
        let comments = &self.ast().comments;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 100_000 {
                return pos;
            }
            while let Some(c) = self
                .src()
                .get(pos as usize..)
                .and_then(|t| t.chars().next())
                && is_js_space(c)
            {
                pos += c.len_utf8() as u32;
            }
            let i = comments.partition_point(|c| c.span.start < pos);
            match comments.get(i) {
                Some(c) if c.span.start == pos && c.span.end > pos => pos = c.span.end,
                _ => return pos,
            }
        }
    }

    /// The end of the last token before `pos` (skips whitespace and comments backwards).
    fn prev_token_end(&self, mut pos: u32) -> u32 {
        let comments = &self.ast().comments;
        for _ in 0..100_000 {
            while let Some(c) = self
                .src()
                .get(..pos as usize)
                .and_then(|t| t.chars().next_back())
                && is_js_space(c)
            {
                pos -= c.len_utf8() as u32;
            }
            let i = comments.partition_point(|c| c.span.end < pos);
            match comments.get(i) {
                Some(c) if c.span.end == pos && c.span.start < pos => pos = c.span.start,
                _ => return pos,
            }
        }
        pos
    }

    /// The first `{` at or after `pos`.
    fn brace_from(&self, pos: u32, end: u32) -> u32 {
        let src = self.src().as_bytes();
        let mut p = pos;
        while p < end {
            p = self.skip_trivia(p);
            if p >= end {
                break;
            }
            if src.get(p as usize) == Some(&b'{') {
                return p;
            }
            p += 1;
        }
        pos.min(end)
    }

    /// The span of a type annotation: from the `:` (or `=>`) before the type.
    fn ann_span(&self, ty: NodeId) -> Span {
        let sp = self.sp(ty);
        let end = self.prev_token_end(sp.start) as usize;
        let head = &self.src()[..end];
        let start = if head.ends_with("=>") {
            end - 2
        } else if head.ends_with(':') {
            end - 1
        } else {
            sp.start as usize
        };
        Span::new(start as u32, sp.end)
    }

    fn ann(&mut self, key: &'static str, ty: Option<NodeId>) {
        self.one(key, |b| {
            let ty = ty?;
            let span = b.ann_span(ty);
            b.n("TSTypeAnnotation", ty, span, Origin::Synthetic, |b| {
                b.one("typeAnnotation", |b| b.ty(ty));
            })
        });
    }

    fn type_params(&mut self, id: Option<NodeId>) {
        self.opt("typeParameters", id);
    }

    fn type_args(&mut self, id: Option<NodeId>) {
        self.opt("typeArguments", id);
    }

    fn decorators(&mut self, l: ts_r_ast::NodeList) {
        self.list("decorators", l);
    }

    // ---- Program and statements ----

    fn program(&mut self, root: NodeId) {
        let Node::Program { stmts } = *self.ast().node(root) else {
            return;
        };
        let len = self.src().len() as u32;
        let mut start = 0;
        if self.src().starts_with('\u{feff}') {
            start = 3;
        }
        if self.src()[start as usize..].starts_with("#!") {
            start = self.src()[start as usize..]
                .find('\n')
                .map_or(len, |i| start + i as u32);
        }
        let span = Span::new(self.skip_trivia(start).min(len), len);
        self.n("Program", root, span, Origin::Real, |b| {
            b.list("body", stmts);
        });
    }

    fn decl_mods(&self, id: NodeId) -> Option<Modifiers> {
        match *self.ast().node(id) {
            Node::FunctionDecl { modifiers, .. }
            | Node::ClassDecl { modifiers, .. }
            | Node::VarDecl { modifiers, .. }
            | Node::Interface { modifiers, .. }
            | Node::TypeAlias { modifiers, .. }
            | Node::Enum { modifiers, .. }
            | Node::Module { modifiers, .. }
            | Node::ImportEquals { modifiers, .. } => Some(modifiers),
            _ => None,
        }
    }

    /// The declaration inside its `export` wrapper starts after `export` / `default`.
    fn inner_span(&self, id: NodeId) -> Span {
        let sp = self.sp(id);
        let end = self
            .ast()
            .modifier_tokens_of(id)
            .iter()
            .filter(|m| m.flag.intersects(Modifiers::EXPORT | Modifiers::DEFAULT))
            .map(|m| m.span.end)
            .max();
        match end {
            Some(e) => Span::new(self.skip_trivia(e).min(sp.end), sp.end),
            None => sp,
        }
    }

    fn node(&mut self, id: NodeId) -> Built {
        let chained = std::mem::take(&mut self.chain);
        if let Some(m) = self.decl_mods(id)
            && m.intersects(Modifiers::EXPORT | Modifiers::DEFAULT)
        {
            let ty = if m.contains(Modifiers::DEFAULT) {
                "ExportDefaultDeclaration"
            } else {
                "ExportNamedDeclaration"
            };
            let mut span = self.sp(id);
            if let Some(e) = self
                .ast()
                .modifier_tokens_of(id)
                .iter()
                .find(|m| m.flag.contains(Modifiers::EXPORT))
            {
                span = Span::new(e.span.start.max(span.start), span.end);
            }
            return self.n(ty, id, span, Origin::Export, |b| {
                let inner = b.inner_span(id);
                b.one("declaration", |b| b.decl(id, inner));
                if ty == "ExportNamedDeclaration" {
                    b.empty("specifiers");
                    b.null("source");
                    b.empty("attributes");
                }
            });
        }
        let ast = self.ast();
        let span = self.sp(id);
        match *ast.node(id) {
            Node::Dummy {} | Node::Missing {} | Node::Hole {} => None,
            Node::Program { .. } => None,
            Node::Paren { expr } => self.node(expr),
            Node::ParenType { ty } => self.ty(ty),
            Node::ComputedKey { expr } => self.node(expr),
            Node::Directive { .. } => self.real("ExpressionStatement", id, |b| {
                b.one("expression", |b| {
                    let lit = b.t.literal_span(span);
                    b.n("Literal", id, lit, Origin::Synthetic, |_| {})
                });
            }),
            Node::Block { stmts } => self.real("BlockStatement", id, |b| b.list("body", stmts)),
            Node::Empty {} => self.leaf("EmptyStatement", id),
            Node::ExprStmt { expr } => self.real("ExpressionStatement", id, |b| {
                b.req("expression", expr);
            }),
            Node::If { test, cons, alt } => self.real("IfStatement", id, |b| {
                b.req("test", test);
                b.req("consequent", cons);
                b.opt("alternate", alt);
            }),
            Node::For {
                init,
                test,
                update,
                body,
            } => self.real("ForStatement", id, |b| {
                b.opt("init", init);
                b.opt("test", test);
                b.opt("update", update);
                b.req("body", body);
            }),
            Node::ForIn { left, right, body } => self.real("ForInStatement", id, |b| {
                b.one("left", |b| b.for_left(left));
                b.req("right", right);
                b.req("body", body);
            }),
            Node::ForOf {
                left, right, body, ..
            } => self.real("ForOfStatement", id, |b| {
                b.one("left", |b| b.for_left(left));
                b.req("right", right);
                b.req("body", body);
            }),
            Node::While { test, body } => self.real("WhileStatement", id, |b| {
                b.req("test", test);
                b.req("body", body);
            }),
            Node::DoWhile { body, test } => self.real("DoWhileStatement", id, |b| {
                b.req("body", body);
                b.req("test", test);
            }),
            Node::Return { arg } => self.real("ReturnStatement", id, |b| b.opt("argument", arg)),
            Node::Break { label } => self.real("BreakStatement", id, |b| b.opt("label", label)),
            Node::Continue { label } => {
                self.real("ContinueStatement", id, |b| b.opt("label", label))
            }
            Node::Throw { arg } => self.real("ThrowStatement", id, |b| b.req("argument", arg)),
            Node::Labeled { label, body } => self.real("LabeledStatement", id, |b| {
                b.req("label", label);
                b.req("body", body);
            }),
            Node::Switch { disc, cases } => self.real("SwitchStatement", id, |b| {
                b.req("discriminant", disc);
                b.list("cases", cases);
            }),
            Node::SwitchCase { test, body } => self.real("SwitchCase", id, |b| {
                b.opt("test", test);
                b.list("consequent", body);
            }),
            Node::Try {
                block,
                handler,
                finalizer,
            } => self.real("TryStatement", id, |b| {
                b.req("block", block);
                b.opt("handler", handler);
                b.opt("finalizer", finalizer);
            }),
            Node::CatchClause {
                param,
                type_ann,
                body,
            } => self.real("CatchClause", id, |b| {
                b.one("param", |b| param.and_then(|p| b.pat(p, type_ann, false)));
                b.req("body", body);
            }),
            Node::Debugger {} => self.leaf("DebuggerStatement", id),
            Node::With { obj, body } => self.real("WithStatement", id, |b| {
                b.req("object", obj);
                b.req("body", body);
            }),
            Node::VarDecl { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassDecl { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::Enum { .. }
            | Node::Module { .. } => self.decl(id, span),
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } => self.real("VariableDeclarator", id, |b| {
                b.one("id", |b| b.pat(name, type_ann, false));
                b.opt("init", init);
            }),
            Node::FunctionExpr { .. } => self.function(id, span, false),
            Node::Arrow {
                type_params,
                params,
                return_type,
                body,
                ..
            } => self.real("ArrowFunctionExpression", id, |b| {
                b.type_params(type_params);
                b.params(params);
                b.ann("returnType", return_type);
                b.req("body", body);
            }),
            Node::Param { .. } => self.param(id),
            Node::Decorator { expr } => self.real("Decorator", id, |b| b.req("expression", expr)),
            Node::ClassExpr { .. } => self.class(id, span, "ClassExpression"),
            Node::HeritageClause { .. } => None,
            Node::ExprWithTypeArgs { expr, type_args } => {
                self.real("TSInterfaceHeritage", id, |b| {
                    b.req("expression", expr);
                    b.type_args(type_args);
                })
            }
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                type_ann,
                value,
                ..
            } => {
                let ty = if modifiers.contains(Modifiers::ABSTRACT) {
                    if modifiers.contains(Modifiers::ACCESSOR) {
                        "TSAbstractAccessorProperty"
                    } else {
                        "TSAbstractPropertyDefinition"
                    }
                } else if modifiers.contains(Modifiers::ACCESSOR) {
                    "AccessorProperty"
                } else {
                    "PropertyDefinition"
                };
                self.real(ty, id, |b| {
                    b.decorators(decorators);
                    b.req("key", key);
                    b.ann("typeAnnotation", type_ann);
                    b.opt("value", value);
                })
            }
            Node::MethodDef {
                decorators,
                modifiers,
                key,
                value,
                ..
            } => {
                let in_object = self.cur != NONE
                    && matches!(
                        *ast.node(self.t.nodes[self.cur as usize].ast),
                        Node::ObjectLit { .. }
                    );
                let ty = if in_object {
                    "Property"
                } else if modifiers.contains(Modifiers::ABSTRACT) {
                    "TSAbstractMethodDefinition"
                } else {
                    "MethodDefinition"
                };
                self.real(ty, id, |b| {
                    if !in_object {
                        b.decorators(decorators);
                    }
                    b.req("key", key);
                    b.req("value", value);
                })
            }
            Node::StaticBlock { body } => self.real("StaticBlock", id, |b| b.list("body", body)),
            Node::IndexSignature {
                params, type_ann, ..
            } => self.real("TSIndexSignature", id, |b| {
                b.params_named("parameters", params);
                b.ann("typeAnnotation", type_ann);
            }),
            Node::EnumMember { name, init } => self.real("TSEnumMember", id, |b| {
                b.req("id", name);
                b.opt("initializer", init);
            }),
            Node::ModuleBlock { stmts } => {
                self.real("TSModuleBlock", id, |b| b.list("body", stmts))
            }
            Node::ImportDecl {
                specifiers,
                source,
                attributes,
                ..
            } => self.real("ImportDeclaration", id, |b| {
                b.list("specifiers", specifiers);
                b.req("source", source);
                b.attributes(attributes);
            }),
            Node::ImportDefaultSpecifier { local } => {
                self.real("ImportDefaultSpecifier", id, |b| b.req("local", local))
            }
            Node::ImportNamespaceSpecifier { local } => {
                self.real("ImportNamespaceSpecifier", id, |b| b.req("local", local))
            }
            Node::ImportSpecifier {
                imported, local, ..
            } => self.real("ImportSpecifier", id, |b| {
                match imported {
                    Some(i) => b.req("imported", i),
                    None => b.one("imported", |b| b.dup(local)),
                }
                b.req("local", local);
            }),
            Node::ImportAttributes { .. } => None,
            Node::ImportAttribute { key, value } => self.real("ImportAttribute", id, |b| {
                b.req("key", key);
                b.req("value", value);
            }),
            Node::ImportEquals { .. } => self.decl(id, span),
            Node::ExternalModuleRef { expr } => self.real("TSExternalModuleReference", id, |b| {
                b.req("expression", expr)
            }),
            Node::ExportNamed {
                specifiers,
                source,
                attributes,
                ..
            } => self.real("ExportNamedDeclaration", id, |b| {
                b.null("declaration");
                b.list("specifiers", specifiers);
                b.opt("source", source);
                b.attributes(attributes);
            }),
            Node::ExportSpecifier {
                local, exported, ..
            } => self.real("ExportSpecifier", id, |b| {
                b.req("local", local);
                match exported {
                    Some(e) => b.req("exported", e),
                    None => b.one("exported", |b| b.dup(local)),
                }
            }),
            Node::ExportAll {
                exported,
                source,
                attributes,
                ..
            } => self.real("ExportAllDeclaration", id, |b| {
                b.opt("exported", exported);
                b.req("source", source);
                b.attributes(attributes);
            }),
            Node::ExportDefaultExpr { expr } => self.real("ExportDefaultDeclaration", id, |b| {
                b.req("declaration", expr)
            }),
            Node::ExportAssignment { expr } => {
                self.real("TSExportAssignment", id, |b| b.req("expression", expr))
            }
            Node::NamespaceExport { name } => {
                self.real("TSNamespaceExportDeclaration", id, |b| b.req("id", name))
            }

            // ---- Expressions ----
            Node::Ident { .. } => self.leaf("Identifier", id),
            Node::PrivateName { .. } => self.leaf("PrivateIdentifier", id),
            Node::This {} => self.leaf("ThisExpression", id),
            Node::Super {} => self.leaf("Super", id),
            Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. } => self.leaf("Literal", id),
            Node::TemplateLit { parts } => self.real("TemplateLiteral", id, |b| {
                let ps = b.ast().list(parts);
                let quasis: Vec<NodeId> = ps.iter().copied().step_by(2).collect();
                let exprs: Vec<NodeId> = ps.iter().copied().skip(1).step_by(2).collect();
                b.many("quasis", &quasis, |b, q| b.node(q));
                b.many("expressions", &exprs, |b, e| b.node(e));
            }),
            Node::TemplateElement { .. } => {
                let s = self.src().as_bytes();
                let end = match s.get(span.end as usize) {
                    Some(b'`') => span.end + 1,
                    Some(b'$') => span.end + 2,
                    _ => span.end,
                };
                let sp = Span::new(span.start.saturating_sub(1), end.min(s.len() as u32));
                self.n("TemplateElement", id, sp, Origin::Real, |_| {})
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => self.real("TaggedTemplateExpression", id, |b| {
                b.req("tag", tag);
                b.type_args(type_args);
                b.req("quasi", quasi);
            }),
            Node::ArrayLit { elements } => self.real("ArrayExpression", id, |b| {
                b.list("elements", elements);
            }),
            Node::ObjectLit { props } => {
                self.real("ObjectExpression", id, |b| b.list("properties", props))
            }
            Node::Property { key, value, .. } => self.real("Property", id, |b| {
                b.req("key", key);
                b.req("value", value);
            }),
            Node::ShorthandProp { name, init, .. } => self.real("Property", id, |b| {
                b.req("key", name);
                b.one("value", |b| b.shorthand_value(name, init));
            }),
            Node::Spread { arg } => self.real("SpreadElement", id, |b| b.req("argument", arg)),
            Node::Seq { exprs } => {
                self.real("SequenceExpression", id, |b| b.list("expressions", exprs))
            }
            Node::Unary { arg, .. } => self.real("UnaryExpression", id, |b| b.req("argument", arg)),
            Node::Update { arg, .. } => {
                self.real("UpdateExpression", id, |b| b.req("argument", arg))
            }
            Node::Await { arg } => self.real("AwaitExpression", id, |b| b.req("argument", arg)),
            Node::Yield { arg, .. } => self.real("YieldExpression", id, |b| b.opt("argument", arg)),
            Node::Binary { left, op, right } => {
                use ts_r_ast::BinaryOp::*;
                let ty = if matches!(op, LogicalOr | LogicalAnd | Coalesce) {
                    "LogicalExpression"
                } else {
                    "BinaryExpression"
                };
                self.real(ty, id, |b| {
                    b.req("left", left);
                    b.req("right", right);
                })
            }
            Node::Assign { left, right, .. } => self.real("AssignmentExpression", id, |b| {
                b.one("left", |b| b.target(left));
                b.req("right", right);
            }),
            Node::Conditional { test, cons, alt } => self.real("ConditionalExpression", id, |b| {
                b.req("test", test);
                b.req("consequent", cons);
                b.req("alternate", alt);
            }),
            Node::Call { .. } | Node::Member { .. } | Node::Index { .. } => {
                if !chained && self.has_optional(id) {
                    self.n("ChainExpression", id, span, Origin::Synthetic, |b| {
                        b.one("expression", |b| b.chain_elem(id));
                    })
                } else {
                    self.chain_elem(id)
                }
            }
            Node::New {
                callee,
                type_args,
                args,
                ..
            } => self.real("NewExpression", id, |b| {
                b.req("callee", callee);
                b.type_args(type_args);
                b.list("arguments", args);
            }),
            Node::MetaProperty { kind } => self.real("MetaProperty", id, |b| {
                let (m, p) = match kind {
                    MetaPropKind::NewTarget => (3, 6),
                    MetaPropKind::ImportMeta => (6, 4),
                };
                let pstart = span.end.saturating_sub(p);
                b.one("meta", |b| {
                    let s = Span::new(span.start, (span.start + m).min(span.end));
                    b.n("Identifier", id, s, Origin::Synthetic, |_| {})
                });
                b.one("property", |b| {
                    let s = Span::new(pstart, span.end);
                    b.n("Identifier", id, s, Origin::Synthetic, |_| {})
                });
            }),
            Node::ImportCall { arg, options, .. } => self.real("ImportExpression", id, |b| {
                b.req("source", arg);
                b.opt("options", options);
            }),
            Node::As { expr, ty } => self.real("TSAsExpression", id, |b| {
                b.req("expression", expr);
                b.one("typeAnnotation", |b| b.ty(ty));
            }),
            Node::Satisfies { expr, ty } => self.real("TSSatisfiesExpression", id, |b| {
                b.req("expression", expr);
                b.one("typeAnnotation", |b| b.ty(ty));
            }),
            Node::TypeAssertion { ty, expr } => self.real("TSTypeAssertion", id, |b| {
                b.one("typeAnnotation", |b| b.ty(ty));
                b.req("expression", expr);
            }),
            Node::NonNull { .. } if !chained && self.has_optional(id) => {
                self.n("ChainExpression", id, span, Origin::Synthetic, |b| {
                    b.one("expression", |b| {
                        b.chain = true;
                        b.node(id)
                    });
                })
            }
            Node::NonNull { expr } => self.real("TSNonNullExpression", id, |b| {
                b.chain = chained;
                b.one("expression", |b| {
                    b.chain = chained;
                    b.node(expr)
                });
            }),
            Node::Instantiation { expr, type_args } => {
                self.real("TSInstantiationExpression", id, |b| {
                    b.req("expression", expr);
                    b.type_args(Some(type_args));
                })
            }
            Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. }
            | Node::BindingProperty { .. } => self.pat(id, None, false),

            // ---- JSX ----
            Node::JsxElement {
                opening,
                children,
                closing,
            } => self.real("JSXElement", id, |b| {
                b.req("openingElement", opening);
                b.list("children", children);
                b.opt("closingElement", closing);
            }),
            Node::JsxOpening {
                name,
                type_args,
                attrs,
                ..
            } => self.real("JSXOpeningElement", id, |b| {
                b.one("name", |b| b.jsx_name(name));
                b.type_args(type_args);
                b.list("attributes", attrs);
            }),
            Node::JsxClosing { name } => self.real("JSXClosingElement", id, |b| {
                b.one("name", |b| b.jsx_name(name));
            }),
            Node::JsxFragment { children } => self.real("JSXFragment", id, |b| {
                let s = b.src().as_bytes();
                let open_end = (span.start..span.end)
                    .find(|&p| s.get(p as usize) == Some(&b'>'))
                    .map_or(span.end, |p| p + 1);
                b.one("openingFragment", |b| {
                    let sp = Span::new(span.start, open_end);
                    b.n("JSXOpeningFragment", id, sp, Origin::Synthetic, |_| {})
                });
                b.list("children", children);
                b.one("closingFragment", |b| {
                    let close = b.src()[..span.end as usize]
                        .rfind("</")
                        .map_or(span.end, |p| p as u32)
                        .max(open_end);
                    let sp = Span::new(close, span.end);
                    b.n("JSXClosingFragment", id, sp, Origin::Synthetic, |_| {})
                });
            }),
            Node::JsxAttr { name, value } => self.real("JSXAttribute", id, |b| {
                b.one("name", |b| b.jsx_name(name));
                b.opt("value", value);
            }),
            Node::JsxSpreadAttr { arg } => {
                self.real("JSXSpreadAttribute", id, |b| b.req("argument", arg))
            }
            Node::JsxExprContainer { expr } => self.real("JSXExpressionContainer", id, |b| {
                b.one("expression", |b| match expr {
                    Some(e) => b.node(e),
                    None => {
                        let sp = Span::new(
                            (span.start + 1).min(span.end),
                            span.end.saturating_sub(1).max(span.start),
                        );
                        b.n("JSXEmptyExpression", id, sp, Origin::Synthetic, |_| {})
                    }
                });
            }),
            Node::JsxSpreadChild { expr } => {
                self.real("JSXSpreadChild", id, |b| b.req("expression", expr))
            }
            Node::JsxText {} => self.leaf("JSXText", id),
            Node::JsxNamespacedName { .. } | Node::JsxMemberExpr { .. } => self.jsx_name(id),

            // Types reached through a generic slot.
            _ => self.ty(id),
        }
    }

    fn attributes(&mut self, attrs: Option<NodeId>) {
        let ast = self.ast();
        let ids: &[NodeId] = match attrs.map(|a| ast.node(a)) {
            Some(&Node::ImportAttributes { entries, .. }) => ast.list(entries),
            _ => &[],
        };
        self.many("attributes", ids, |b, id| b.node(id));
    }

    fn has_optional(&self, mut id: NodeId) -> bool {
        let ast = self.ast();
        for _ in 0..100_000 {
            match *ast.node(id) {
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional {
                        return true;
                    }
                    id = obj;
                }
                Node::Call {
                    callee, optional, ..
                } => {
                    if optional {
                        return true;
                    }
                    id = callee;
                }
                Node::NonNull { expr } => id = expr,
                _ => return false,
            }
        }
        false
    }

    /// A member or call that is part of an optional chain (or not in one at all).
    fn chain_elem(&mut self, id: NodeId) -> Built {
        let in_chain = self.has_optional(id);
        match *self.ast().node(id) {
            Node::Member { obj, prop, .. } => self.real("MemberExpression", id, |b| {
                b.one("object", |b| {
                    b.chain = in_chain;
                    b.node(obj)
                });
                b.req("property", prop);
            }),
            Node::Index { obj, index, .. } => self.real("MemberExpression", id, |b| {
                b.one("object", |b| {
                    b.chain = in_chain;
                    b.node(obj)
                });
                b.req("property", index);
            }),
            Node::Call {
                callee,
                type_args,
                args,
                ..
            } => self.real("CallExpression", id, |b| {
                b.one("callee", |b| {
                    b.chain = in_chain;
                    b.node(callee)
                });
                b.type_args(type_args);
                b.list("arguments", args);
            }),
            _ => None,
        }
    }

    /// A second ESTree node for the same AST node (shorthand values, unaliased specifiers).
    fn dup(&mut self, id: NodeId) -> Built {
        let span = self.sp(id);
        let ty = match self.ast().node(id) {
            Node::StringLit { .. } => "Literal",
            _ => "Identifier",
        };
        self.n(ty, id, span, Origin::Synthetic, |_| {})
    }

    fn shorthand_value(&mut self, name: NodeId, init: Option<NodeId>) -> Built {
        match init {
            None => self.dup(name),
            Some(init) => {
                let span = Span::new(self.sp(name).start, self.sp(init).end);
                self.n("AssignmentPattern", init, span, Origin::Synthetic, |b| {
                    b.one("left", |b| b.dup(name));
                    b.req("right", init);
                })
            }
        }
    }

    fn decl(&mut self, id: NodeId, span: Span) -> Built {
        let ast = self.ast();
        match *ast.node(id) {
            Node::VarDecl { decls, .. } => {
                self.n("VariableDeclaration", id, span, Origin::Real, |b| {
                    b.list("declarations", decls);
                })
            }
            Node::FunctionDecl { .. } => self.function(id, span, true),
            Node::ClassDecl { .. } => self.class(id, span, "ClassDeclaration"),
            Node::Interface {
                name,
                type_params,
                extends,
                body,
                ..
            } => self.n("TSInterfaceDeclaration", id, span, Origin::Real, |b| {
                b.req("id", name);
                b.type_params(type_params);
                b.list("extends", extends);
                b.one("body", |b| {
                    let mut from = b.sp(name).end;
                    if let Some(tp) = type_params {
                        from = from.max(b.sp(tp).end);
                    }
                    if let Some(&last) = ast.list(extends).last() {
                        from = from.max(b.sp(last).end);
                    }
                    let start = b.brace_from(from, span.end);
                    let sp = Span::new(start, span.end);
                    b.n("TSInterfaceBody", id, sp, Origin::Synthetic, |b| {
                        b.list("body", body);
                    })
                });
            }),
            Node::TypeAlias {
                name,
                type_params,
                ty,
                ..
            } => self.n("TSTypeAliasDeclaration", id, span, Origin::Real, |b| {
                b.req("id", name);
                b.type_params(type_params);
                b.one("typeAnnotation", |b| b.ty(ty));
            }),
            Node::Enum { name, members, .. } => {
                self.n("TSEnumDeclaration", id, span, Origin::Real, |b| {
                    b.req("id", name);
                    b.one("body", |b| {
                        let start = b.brace_from(b.sp(name).end, span.end);
                        let sp = Span::new(start, span.end);
                        b.n("TSEnumBody", id, sp, Origin::Synthetic, |b| {
                            b.list("members", members);
                        })
                    });
                })
            }
            Node::Module { name, body, .. } => {
                self.n("TSModuleDeclaration", id, span, Origin::Real, |b| {
                    b.req("id", name);
                    b.opt("body", body);
                })
            }
            Node::ImportEquals {
                name, module_ref, ..
            } => self.n("TSImportEqualsDeclaration", id, span, Origin::Real, |b| {
                b.req("id", name);
                b.req("moduleReference", module_ref);
            }),
            _ => None,
        }
    }

    fn function(&mut self, id: NodeId, span: Span, decl: bool) -> Built {
        let (name, type_params, params, return_type, body) = match *self.ast().node(id) {
            Node::FunctionDecl {
                name,
                type_params,
                params,
                return_type,
                body,
                ..
            }
            | Node::FunctionExpr {
                name,
                type_params,
                params,
                return_type,
                body,
                ..
            } => (name, type_params, params, return_type, body),
            _ => return None,
        };
        let ty = match (decl, body.is_some()) {
            (true, true) => "FunctionDeclaration",
            (true, false) => "TSDeclareFunction",
            (false, true) => "FunctionExpression",
            (false, false) => "TSEmptyBodyFunctionExpression",
        };
        self.n(ty, id, span, Origin::Real, |b| {
            b.opt("id", name);
            b.type_params(type_params);
            b.params(params);
            b.ann("returnType", return_type);
            b.opt("body", body);
        })
    }

    fn class(&mut self, id: NodeId, span: Span, ty: &'static str) -> Built {
        let ast = self.ast();
        let (decorators, name, type_params, heritage, members) = match *ast.node(id) {
            Node::ClassDecl {
                decorators,
                name,
                type_params,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                decorators,
                name,
                type_params,
                heritage,
                members,
                ..
            } => (decorators, name, type_params, heritage, members),
            _ => return None,
        };
        let mut sup = None;
        let mut implements: &[NodeId] = &[];
        for &h in ast.list(heritage) {
            if let Node::HeritageClause { kind, types } = *ast.node(h) {
                match kind {
                    HeritageKind::Extends => sup = ast.list(types).first().copied(),
                    HeritageKind::Implements => implements = ast.list(types),
                }
            }
        }
        let (sup_expr, sup_args) = match sup.map(|s| ast.node(s)) {
            Some(&Node::ExprWithTypeArgs { expr, type_args }) => (Some(expr), type_args),
            _ => (sup, None),
        };
        self.n(ty, id, span, Origin::Real, |b| {
            b.decorators(decorators);
            b.opt("id", name);
            b.type_params(type_params);
            b.opt("superClass", sup_expr);
            b.opt("superTypeArguments", sup_args);
            b.many("implements", implements, |b, i| match *ast.node(i) {
                Node::ExprWithTypeArgs { expr, type_args } => b.real("TSClassImplements", i, |b| {
                    b.req("expression", expr);
                    b.type_args(type_args);
                }),
                _ => b.node(i),
            });
            b.one("body", |b| {
                let mut from = span.start;
                for x in [name, type_params].into_iter().flatten() {
                    from = from.max(b.sp(x).end);
                }
                if let Some(&h) = ast.list(heritage).last() {
                    from = from.max(b.sp(h).end);
                }
                let start = b.brace_from(from, span.end);
                let sp = Span::new(start, span.end);
                b.n("ClassBody", id, sp, Origin::Synthetic, |b| {
                    b.list("body", members);
                })
            });
        })
    }

    fn params(&mut self, l: ts_r_ast::NodeList) {
        self.params_named("params", l);
    }

    fn params_named(&mut self, key: &'static str, l: ts_r_ast::NodeList) {
        let ids = self.ast().list(l);
        self.many(key, ids, |b, id| b.node(id));
    }

    fn param(&mut self, id: NodeId) -> Built {
        let Node::Param {
            decorators,
            modifiers,
            rest,
            pattern,
            optional,
            type_ann,
            init,
        } = *self.ast().node(id)
        else {
            return None;
        };
        let span = self.sp(id);
        let property = modifiers.intersects(
            Modifiers::PUBLIC
                | Modifiers::PRIVATE
                | Modifiers::PROTECTED
                | Modifiers::READONLY
                | Modifiers::OVERRIDE,
        );
        let inner = move |b: &mut Self, with_decorators: bool, span: Span| -> Built {
            if rest {
                b.n("RestElement", id, span, Origin::Synthetic, |b| {
                    if with_decorators {
                        b.decorators(decorators);
                    }
                    b.one("argument", |b| b.pat(pattern, None, false));
                    b.ann("typeAnnotation", type_ann);
                })
            } else if let Some(init) = init {
                b.n("AssignmentPattern", id, span, Origin::Synthetic, |b| {
                    if with_decorators {
                        b.decorators(decorators);
                    }
                    b.one("left", |b| b.pat(pattern, type_ann, optional));
                    b.req("right", init);
                })
            } else {
                if with_decorators {
                    b.decos = Some(decorators);
                }
                b.pat(pattern, type_ann, optional)
            }
        };
        // The parameter starts at its pattern (or `...`), after decorators and modifiers.
        // A rest parameter keeps its decorators in its range (typescript-estree does this).
        let start = if rest && !property {
            span.start
        } else if rest {
            self.prev_token_end(self.sp(pattern).start)
                .saturating_sub(3)
        } else {
            self.sp(pattern).start
        };
        let inner_span = Span::new(start.max(span.start), span.end);
        if property {
            self.n("TSParameterProperty", id, span, Origin::Real, |b| {
                b.decorators(decorators);
                b.one("parameter", |b| inner(b, false, inner_span));
            })
        } else {
            inner(self, true, inner_span)
        }
    }

    /// A binding pattern with an optional type annotation.
    fn pat(&mut self, id: NodeId, ann: Option<NodeId>, optional: bool) -> Built {
        let ast = self.ast();
        let sp = self.sp(id);
        let mut end = sp.end;
        if let Some(a) = ann {
            end = end.max(self.sp(a).end);
        } else if optional {
            let after = self.skip_trivia(sp.end);
            if self.src().as_bytes().get(after as usize) == Some(&b'?') {
                end = after + 1;
            }
        }
        let span = Span::new(sp.start, end);
        let decos = self.decos.take();
        let decorators = move |b: &mut Self| {
            if let Some(d) = decos {
                b.decorators(d);
            }
        };
        match *ast.node(id) {
            Node::Ident { .. } | Node::This {} => {
                self.n("Identifier", id, span, Origin::Real, |b| {
                    decorators(b);
                    b.ann("typeAnnotation", ann);
                })
            }
            Node::ObjectPattern { props } => self.n("ObjectPattern", id, span, Origin::Real, |b| {
                decorators(b);
                let ids = ast.list(props);
                b.many("properties", ids, |b, p| b.pat(p, None, false));
                b.ann("typeAnnotation", ann);
            }),
            Node::ArrayPattern { elements } => {
                self.n("ArrayPattern", id, span, Origin::Real, |b| {
                    decorators(b);
                    let ids = ast.list(elements);
                    b.many("elements", ids, |b, p| b.pat(p, None, false));
                    b.ann("typeAnnotation", ann);
                })
            }
            Node::AssignPattern { left, right } => {
                self.n("AssignmentPattern", id, span, Origin::Real, |b| {
                    b.one("left", |b| b.pat(left, None, false));
                    b.req("right", right);
                })
            }
            Node::Rest { arg } => self.n("RestElement", id, span, Origin::Real, |b| {
                b.one("argument", |b| b.pat(arg, None, false));
            }),
            Node::BindingProperty { key, value, init } => {
                self.n("Property", id, span, Origin::Real, |b| {
                    b.req("key", key);
                    b.one("value", |b| match value {
                        None => b.shorthand_value(key, init),
                        Some(v) => match init {
                            None => b.pat(v, None, false),
                            Some(init) => {
                                let s = Span::new(b.sp(v).start, b.sp(init).end);
                                b.n("AssignmentPattern", init, s, Origin::Synthetic, |b| {
                                    b.one("left", |b| b.pat(v, None, false));
                                    b.req("right", init);
                                })
                            }
                        },
                    });
                })
            }
            Node::Hole {} => None,
            _ => self.target(id),
        }
    }

    fn for_left(&mut self, left: NodeId) -> Built {
        match self.ast().node(left) {
            Node::VarDecl { .. } => self.node(left),
            _ => self.target(left),
        }
    }

    /// An expression in an assignment-target position, as a pattern.
    fn target(&mut self, id: NodeId) -> Built {
        let ast = self.ast();
        match *ast.node(id) {
            Node::Paren { expr } => self.target(expr),
            Node::ArrayLit { elements } => self.real("ArrayPattern", id, |b| {
                let ids = ast.list(elements);
                b.many("elements", ids, |b, e| b.target(e));
            }),
            Node::ObjectLit { props } => self.real("ObjectPattern", id, |b| {
                let ids = ast.list(props);
                b.many("properties", ids, |b, p| match *ast.node(p) {
                    Node::Property { key, value, .. } => b.real("Property", p, |b| {
                        b.req("key", key);
                        b.one("value", |b| b.target(value));
                    }),
                    _ => b.target(p),
                });
            }),
            Node::Spread { arg } => self.real("RestElement", id, |b| {
                b.one("argument", |b| b.target(arg));
            }),
            Node::Assign {
                left,
                op: ts_r_ast::AssignOp::Assign,
                right,
            } => self.real("AssignmentPattern", id, |b| {
                b.one("left", |b| b.target(left));
                b.req("right", right);
            }),
            Node::Hole {} => None,
            Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. }
            | Node::BindingProperty { .. } => self.pat(id, None, false),
            _ => self.node(id),
        }
    }

    fn jsx_name(&mut self, id: NodeId) -> Built {
        match *self.ast().node(id) {
            Node::Ident { .. } | Node::This {} => self.leaf("JSXIdentifier", id),
            Node::JsxNamespacedName { ns, name } => self.real("JSXNamespacedName", id, |b| {
                b.one("namespace", |b| b.jsx_name(ns));
                b.one("name", |b| b.jsx_name(name));
            }),
            Node::JsxMemberExpr { obj, prop } => self.real("JSXMemberExpression", id, |b| {
                b.one("object", |b| b.jsx_name(obj));
                b.one("property", |b| b.jsx_name(prop));
            }),
            _ => self.node(id),
        }
    }

    /// A node in a type position.
    fn ty(&mut self, id: NodeId) -> Built {
        let ast = self.ast();
        match *ast.node(id) {
            Node::ParenType { ty } => self.ty(ty),
            Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BoolLit { .. }
            | Node::BigIntLit { .. }
            | Node::Unary { .. }
            | Node::TemplateLit { .. } => {
                let span = self.sp(id);
                self.n("TSLiteralType", id, span, Origin::Synthetic, |b| {
                    b.req("literal", id);
                })
            }
            Node::NullLit {} => self.leaf("TSNullKeyword", id),
            Node::KeywordType { kind } => {
                use KeywordTypeKind::*;
                let ty = match kind {
                    Any => "TSAnyKeyword",
                    Unknown => "TSUnknownKeyword",
                    Number => "TSNumberKeyword",
                    BigInt => "TSBigIntKeyword",
                    Boolean => "TSBooleanKeyword",
                    String => "TSStringKeyword",
                    Symbol => "TSSymbolKeyword",
                    Object => "TSObjectKeyword",
                    Never => "TSNeverKeyword",
                    Void => "TSVoidKeyword",
                    Undefined => "TSUndefinedKeyword",
                    Null => "TSNullKeyword",
                    Intrinsic => "TSIntrinsicKeyword",
                    Const => {
                        // `as const`: a reference to a type named `const`.
                        return self.real("TSTypeReference", id, |b| {
                            b.one("typeName", |b| {
                                let span = b.sp(id);
                                b.n("Identifier", id, span, Origin::Synthetic, |_| {})
                            });
                        });
                    }
                };
                self.leaf(ty, id)
            }
            Node::TypeRef { name, type_args } => self.real("TSTypeReference", id, |b| {
                b.req("typeName", name);
                b.type_args(type_args);
            }),
            Node::QualifiedName { left, right } => self.real("TSQualifiedName", id, |b| {
                b.req("left", left);
                b.req("right", right);
            }),
            Node::ThisType {} => self.leaf("TSThisType", id),
            Node::TemplateLitType { parts } if ast.list(parts).len() == 1 => {
                let span = self.sp(id);
                self.n("TSLiteralType", id, span, Origin::Synthetic, |b| {
                    b.one("literal", |b| {
                        b.real("TemplateLiteral", id, |b| {
                            b.list("quasis", parts);
                            b.empty("expressions");
                        })
                    });
                })
            }
            Node::TemplateLitType { parts } => self.real("TSTemplateLiteralType", id, |b| {
                let ps = ast.list(parts);
                let quasis: Vec<NodeId> = ps.iter().copied().step_by(2).collect();
                let types: Vec<NodeId> = ps.iter().copied().skip(1).step_by(2).collect();
                b.many("quasis", &quasis, |b, q| b.node(q));
                b.many("types", &types, |b, t| b.ty(t));
            }),
            Node::ArrayType { elem } => self.real("TSArrayType", id, |b| {
                b.one("elementType", |b| b.ty(elem));
            }),
            Node::TupleType { elems } => self.real("TSTupleType", id, |b| {
                let ids = ast.list(elems);
                b.many("elementTypes", ids, |b, t| b.ty(t));
            }),
            Node::NamedTupleMember { rest, name, ty, .. } => {
                let member = |b: &mut Self| {
                    b.real("TSNamedTupleMember", id, |b| {
                        b.req("label", name);
                        b.one("elementType", |b| b.ty(ty));
                    })
                };
                if rest {
                    let span = self.sp(id);
                    self.n("TSRestType", id, span, Origin::Synthetic, |b| {
                        b.one("typeAnnotation", member);
                    })
                } else {
                    member(self)
                }
            }
            Node::OptionalType { ty } => self.real("TSOptionalType", id, |b| {
                b.one("typeAnnotation", |b| b.ty(ty));
            }),
            Node::RestType { ty } => self.real("TSRestType", id, |b| {
                b.one("typeAnnotation", |b| b.ty(ty));
            }),
            Node::UnionType { types } | Node::IntersectionType { types } => {
                let ty = if matches!(ast.node(id), Node::UnionType { .. }) {
                    "TSUnionType"
                } else {
                    "TSIntersectionType"
                };
                self.real(ty, id, |b| {
                    let ids = ast.list(types);
                    b.many("types", ids, |b, t| b.ty(t));
                })
            }
            Node::FunctionType {
                type_params,
                params,
                return_type,
            }
            | Node::ConstructorType {
                type_params,
                params,
                return_type,
                ..
            } => {
                let ty = if matches!(ast.node(id), Node::FunctionType { .. }) {
                    "TSFunctionType"
                } else {
                    "TSConstructorType"
                };
                self.real(ty, id, |b| {
                    b.type_params(type_params);
                    b.params(params);
                    b.ann("returnType", Some(return_type));
                })
            }
            Node::TypeLiteral { members } => self.real("TSTypeLiteral", id, |b| {
                b.list("members", members);
            }),
            Node::PropertySignature { key, type_ann, .. } => {
                self.real("TSPropertySignature", id, |b| {
                    b.req("key", key);
                    b.ann("typeAnnotation", type_ann);
                })
            }
            Node::MethodSignature {
                key,
                type_params,
                params,
                return_type,
                ..
            } => self.real("TSMethodSignature", id, |b| {
                b.req("key", key);
                b.type_params(type_params);
                b.params(params);
                b.ann("returnType", return_type);
            }),
            Node::CallSignature {
                type_params,
                params,
                return_type,
            }
            | Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => {
                let ty = if matches!(ast.node(id), Node::CallSignature { .. }) {
                    "TSCallSignatureDeclaration"
                } else {
                    "TSConstructSignatureDeclaration"
                };
                self.real(ty, id, |b| {
                    b.type_params(type_params);
                    b.params(params);
                    b.ann("returnType", return_type);
                })
            }
            Node::IndexSignature { .. } => self.node(id),
            Node::TypeQuery {
                expr_name,
                type_args,
            } => self.real("TSTypeQuery", id, |b| {
                b.one("exprName", |b| b.ty(expr_name));
                b.type_args(type_args);
            }),
            Node::TypeOperator { ty, .. } => self.real("TSTypeOperator", id, |b| {
                b.one("typeAnnotation", |b| b.ty(ty));
            }),
            Node::IndexedAccessType { obj, index } => self.real("TSIndexedAccessType", id, |b| {
                b.one("objectType", |b| b.ty(obj));
                b.one("indexType", |b| b.ty(index));
            }),
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => self.real("TSConditionalType", id, |b| {
                b.one("checkType", |b| b.ty(check));
                b.one("extendsType", |b| b.ty(extends));
                b.one("trueType", |b| b.ty(true_ty));
                b.one("falseType", |b| b.ty(false_ty));
            }),
            Node::InferType { type_param } => self.real("TSInferType", id, |b| {
                b.req("typeParameter", type_param);
            }),
            Node::MappedType {
                type_param,
                name_type,
                ty,
                ..
            } => self.real("TSMappedType", id, |b| {
                let (name, constraint) = match *ast.node(type_param) {
                    Node::TypeParam {
                        name, constraint, ..
                    } => (Some(name), constraint),
                    _ => (None, None),
                };
                b.opt("key", name);
                b.one("constraint", |b| constraint.and_then(|c| b.ty(c)));
                b.one("nameType", |b| name_type.and_then(|n| b.ty(n)));
                b.one("typeAnnotation", |b| ty.and_then(|t| b.ty(t)));
            }),
            Node::TypePredicate { param, ty, .. } => self.real("TSTypePredicate", id, |b| {
                b.one("parameterName", |b| match ast.node(param) {
                    Node::ThisType {} => b.leaf("TSThisType", param),
                    _ => b.node(param),
                });
                b.one("typeAnnotation", |b| {
                    let t = ty?;
                    let span = b.sp(t);
                    b.n("TSTypeAnnotation", t, span, Origin::Synthetic, |b| {
                        b.one("typeAnnotation", |b| b.ty(t));
                    })
                });
            }),
            Node::ImportType {
                is_typeof,
                arg,
                qualifier,
                type_args,
                ..
            } => {
                let import = move |b: &mut Self| {
                    b.real("TSImportType", id, |b| {
                        b.one("argument", |b| match b.ast().node(arg) {
                            Node::StringLit { .. } => b.leaf("Literal", arg),
                            _ => b.ty(arg),
                        });
                        b.opt("qualifier", qualifier);
                        b.type_args(type_args);
                    })
                };
                if is_typeof {
                    let span = self.sp(id);
                    self.n("TSTypeQuery", id, span, Origin::Synthetic, |b| {
                        b.one("exprName", import);
                    })
                } else {
                    import(self)
                }
            }
            Node::TypeParams { params } => self.real("TSTypeParameterDeclaration", id, |b| {
                b.list("params", params);
            }),
            Node::TypeParam {
                name,
                constraint,
                default,
                ..
            } => self.real("TSTypeParameter", id, |b| {
                b.req("name", name);
                b.one("constraint", |b| constraint.and_then(|c| b.ty(c)));
                b.one("default", |b| default.and_then(|d| b.ty(d)));
            }),
            Node::TypeArgs { args } => self.real("TSTypeParameterInstantiation", id, |b| {
                let ids = ast.list(args);
                b.many("params", ids, |b, t| b.ty(t));
            }),
            Node::ExprWithTypeArgs { expr, type_args } => {
                self.real("TSInterfaceHeritage", id, |b| {
                    b.req("expression", expr);
                    b.type_args(type_args);
                })
            }
            _ => {
                // A plain expression in a type slot (`typeof x` names, qualifiers).
                match ast.node(id) {
                    Node::Ident { .. } => self.leaf("Identifier", id),
                    Node::This {} => self.leaf("ThisExpression", id),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esquery::parse;

    fn types_matching(src: &str, sel: &str) -> Vec<String> {
        let parsed = ts_r_parser::parse(src, ts_r_ast::SourceKind::Tsx);
        let t = EsTree::build(&parsed.ast, src);
        let s = parse(sel).unwrap();
        let p = s.prepare();
        (0..t.len() as u32)
            .filter(|&n| p.matches_listener(&t, n))
            .map(|n| format!("{}:{}", t.ty(n), t.span(n).text(src)))
            .collect()
    }

    #[test]
    fn shapes() {
        assert_eq!(
            types_matching(
                "export function f(a: T, b = 1) {}",
                "ExportNamedDeclaration > FunctionDeclaration"
            ),
            ["FunctionDeclaration:function f(a: T, b = 1) {}"]
        );
        assert_eq!(
            types_matching(
                "function f(a: T) {}",
                "Identifier > TSTypeAnnotation > TSTypeReference"
            ),
            ["TSTypeReference:T"]
        );
        assert_eq!(
            types_matching(
                "function f(a, b, c) {}",
                "FunctionDeclaration[params.length>2]"
            ),
            ["FunctionDeclaration:function f(a, b, c) {}"]
        );
        assert_eq!(
            types_matching("a?.b.c; x.y;", "ChainExpression"),
            ["ChainExpression:a?.b.c"]
        );
        assert_eq!(
            types_matching("a?.b.c;", "[optional=true]"),
            ["MemberExpression:a?.b"]
        );
        assert_eq!(
            types_matching(
                "interface I { f(): string; g: () => void }",
                "TSPropertySignature > TSTypeAnnotation > TSFunctionType"
            ),
            ["TSFunctionType:() => void"]
        );
        assert_eq!(
            types_matching("const {a, b: c = 1} = o;", "Property > AssignmentPattern"),
            ["AssignmentPattern:c = 1"]
        );
        assert_eq!(
            types_matching("var x = /a/gu;", "Literal[regex.flags=/u/]"),
            ["Literal:/a/gu"]
        );
        assert_eq!(
            types_matching("const f = [<div/>, <div/>]", "* ~ *"),
            ["JSXElement:<div/>"]
        );
    }

    #[test]
    fn deep_input_does_not_overflow() {
        let src = "(".repeat(3000)
            + "1"
            + &")".repeat(3000)
            + ";"
            + &"[".repeat(3000)
            + &"]".repeat(3000);
        let parsed = ts_r_parser::parse(&src, ts_r_ast::SourceKind::Ts);
        let t = EsTree::build(&parsed.ast, &src);
        assert!(!t.is_empty());
    }
}
