//! `@typescript-eslint/consistent-indexed-object-style`: `Record<K, V>` (`record`, the
//! default) or an index signature `{ [key: K]: V }` (`index-signature`).
//!
//! In `record` mode an interface or type literal whose only member is an index signature,
//! and a mapped type whose key isn't used in its value, report. They are left alone when
//! the type refers back to the alias or interface it declares (followed through type
//! literals, unions, intersections, conditional, indexed-access and mapped types, type
//! arguments and other aliases or interfaces it names). A comment the rewrite would drop
//! turns the fix into a suggestion; an interface with `extends` or under `export default`,
//! and a `-readonly` mapped type, get neither.

use super::prelude::*;
use rustc_hash::FxHashSet;
use ts_r_ast::{Ast, MappedModifier, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-indexed-object-style",
        "Require or disallow the `Record` type",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<ConsistentIndexedObjectStyle>,
};

#[derive(Debug, Default)]
struct ConsistentIndexedObjectStyle {
    index_signature: bool,
}

impl FromOptions for ConsistentIndexedObjectStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        match opts.first().and_then(Value::as_str) {
            None | Some("record") => Ok(Self::default()),
            Some("index-signature") => Ok(ConsistentIndexedObjectStyle {
                index_signature: true,
            }),
            Some(o) => Err(format!(
                "@typescript-eslint/consistent-indexed-object-style: unknown option {o:?}"
            )),
        }
    }
}

fn unparen(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::ParenType { ty } => id = ty,
            _ => break,
        }
    }
    id
}

/// How a report repairs the code.
enum Repair {
    Fix,
    Suggest,
    None,
}

/// `child` is a type annotation of `parent` (a `TSTypeAnnotation` in ESTree).
fn is_annotation(ast: &Ast, parent: NodeId, child: NodeId) -> bool {
    match *ast.node(parent) {
        Node::Param { type_ann, .. }
        | Node::VarDeclarator { type_ann, .. }
        | Node::PropertySignature { type_ann, .. }
        | Node::IndexSignature { type_ann, .. }
        | Node::PropertyDef { type_ann, .. } => type_ann == Some(child),
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. }
        | Node::MethodSignature { return_type, .. }
        | Node::CallSignature { return_type, .. }
        | Node::ConstructSignature { return_type, .. } => return_type == Some(child),
        Node::FunctionType { return_type, .. } | Node::ConstructorType { return_type, .. } => {
            return_type == child
        }
        Node::TypePredicate { ty, .. } => ty == Some(child),
        _ => false,
    }
}

struct Circular<'c, 'a> {
    cx: &'c Ctx<'c, 'a>,
    name: &'c str,
    /// The declared name has references (so an identifier of that name is one of them).
    referenced: bool,
    scope: ScopeId,
    visited: FxHashSet<NodeId>,
}

impl Circular<'_, '_> {
    fn deep(&mut self, id: NodeId, depth: u32) -> bool {
        if depth > 200 || !self.visited.insert(id) {
            return false;
        }
        let ast = self.cx.ast();
        let any = |this: &mut Self, ids: &[NodeId]| ids.iter().any(|&t| this.deep(t, depth + 1));
        match *ast.node(id) {
            Node::ParenType { ty } => self.deep(ty, depth + 1),
            Node::TypeLiteral { members } => any(self, ast.list(members)),
            Node::TypeAlias { ty, .. } => self.deep(ty, depth + 1),
            Node::IndexedAccessType { obj, index } => any(self, &[index, obj]),
            Node::MappedType { ty, .. } => ty.is_some_and(|t| self.deep(t, depth + 1)),
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => any(self, &[check, extends, false_ty, true_ty]),
            Node::UnionType { types } | Node::IntersectionType { types } => {
                any(self, ast.list(types))
            }
            Node::Interface { body, .. } => any(self, ast.list(body)),
            Node::IndexSignature { type_ann, .. } => {
                type_ann.is_some_and(|t| self.deep(t, depth + 1))
            }
            Node::TypeArgs { args } => any(self, ast.list(args)),
            Node::TypeRef { name, type_args } => {
                self.deep(name, depth + 1) || type_args.is_some_and(|t| self.deep(t, depth + 1))
            }
            Node::Ident { name } => {
                let n = ast.atom(name);
                if n == self.name && self.referenced {
                    return true;
                }
                let sem = self.cx.semantic();
                let Some(sym) = sem.lookup(self.scope, n) else {
                    return false;
                };
                let decls: Vec<NodeId> = sem.symbol(sym).decls.iter().map(|d| d.node).collect();
                decls.into_iter().any(|d| self.deep(d, depth + 1))
            }
            _ => false,
        }
    }
}

impl ConsistentIndexedObjectStyle {
    /// Whether the type declared by `name` (an identifier) is referenced from `node`.
    fn circular(cx: &Ctx<'_, '_>, name: NodeId, node: NodeId) -> bool {
        let sem = cx.semantic();
        let Some(n) = ident_name(cx.ast(), name) else {
            return false;
        };
        let Some(sym) = sem.lookup(sem.enclosing_scope(name), n) else {
            return false;
        };
        let s = sem.symbol(sym);
        let mut c = Circular {
            cx,
            name: n,
            referenced: !s.refs.is_empty(),
            scope: s.scope,
            visited: FxHashSet::default(),
        };
        c.visited.insert(name);
        c.deep(node, 0)
    }

    /// A comment inside `node` but outside every kept part.
    fn drops_comment(cx: &Ctx<'_, '_>, node: Span, kept: &[Span]) -> bool {
        cx.ast().comments.iter().any(|c| {
            c.span.start >= node.start
                && c.span.end <= node.end
                && kept
                    .iter()
                    .all(|k| c.span.start < k.start || c.span.end > k.end)
        })
    }

    fn report(cx: &mut Ctx<'_, '_>, span: Span, record: bool, repair: Repair, text: String) {
        let (id, msg, sid, sdesc) = if record {
            (
                "preferRecord",
                "A record is preferred over an index signature.",
                "preferRecordSuggestion",
                "Change into a record instead of an index signature.",
            )
        } else {
            (
                "preferIndexSignature",
                "An index signature is preferred over a record.",
                "preferIndexSignatureSuggestion",
                "Change into an index signature instead of a record.",
            )
        };
        let r = cx.report(span, msg).id(id);
        match repair {
            Repair::Fix => {
                r.fix(Fix::replace(span, text));
            }
            Repair::Suggest => {
                r.suggest(sid, sdesc, Fix::replace(span, text));
            }
            Repair::None => {}
        }
    }

    /// An interface body or type literal with a single index signature.
    #[allow(clippy::too_many_arguments)]
    fn members(
        cx: &mut Ctx<'_, '_>,
        members: &[NodeId],
        node: NodeId,
        span: Span,
        parent_name: Option<NodeId>,
        prefix: &str,
        postfix: &str,
        safe: bool,
    ) {
        let ast = cx.ast();
        let &[member] = members else { return };
        let Node::IndexSignature {
            modifiers,
            params,
            type_ann: Some(value),
        } = *ast.node(member)
        else {
            return;
        };
        let Some(&param) = ast.list(params).first() else {
            return;
        };
        let Node::Param {
            pattern,
            type_ann: Some(key),
            ..
        } = *ast.node(param)
        else {
            return;
        };
        if !matches!(ast.node(pattern), Node::Ident { .. }) {
            return;
        }
        if let Some(n) = parent_name
            && Self::circular(cx, n, node)
        {
            return;
        }
        let key_span = cx.span(unparen(ast, key));
        let value_span = cx.span(unparen(ast, value));
        let repair = if !safe {
            Repair::None
        } else if Self::drops_comment(cx, span, &[key_span, value_span]) {
            Repair::Suggest
        } else {
            Repair::Fix
        };
        let src = cx.src();
        let (k, v) = (key_span.text(src), value_span.text(src));
        let record = if modifiers.contains(Modifiers::READONLY) {
            format!("Readonly<Record<{k}, {v}>>")
        } else {
            format!("Record<{k}, {v}>")
        };
        Self::report(cx, span, true, repair, format!("{prefix}{record}{postfix}"));
    }

    /// The type alias a type sits in, unless a type annotation comes first.
    fn alias_name(cx: &Ctx<'_, '_>, mut id: NodeId) -> Option<NodeId> {
        let ast = cx.ast();
        for _ in 0..10_000 {
            let p = cx.parent(id)?;
            if is_annotation(ast, p, id) {
                return None;
            }
            if let Node::TypeAlias { name, .. } = *ast.node(p) {
                return Some(name);
            }
            id = p;
        }
        None
    }

    fn mapped(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::MappedType {
            readonly,
            type_param,
            optional,
            ty,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let Node::TypeParam {
            name,
            constraint: Some(constraint),
            ..
        } = *ast.node(type_param)
        else {
            return;
        };
        let sem = cx.semantic();
        if sem.symbol_of_decl(name).is_some_and(|s| {
            sem.symbol(s)
                .refs
                .iter()
                .any(|&r| sem.reference(r).is_type())
        }) {
            return;
        }
        if matches!(
            ast.node(constraint),
            Node::TypeOperator {
                op: TypeOperatorKind::KeyOf,
                ..
            }
        ) {
            return;
        }
        if let Some(alias) = Self::alias_name(cx, node)
            && let Some(p) = cx.parent(node)
            && Self::circular(cx, alias, p)
        {
            return;
        }
        let span = cx.span(node);
        let key_span = cx.span(unparen(ast, constraint));
        let value_span = ty.map(|t| cx.span(unparen(ast, t)));
        let repair = if readonly == MappedModifier::Remove {
            Repair::None
        } else {
            let kept: Vec<Span> = [Some(key_span), value_span].into_iter().flatten().collect();
            if Self::drops_comment(cx, span, &kept) {
                Repair::Suggest
            } else {
                Repair::Fix
            }
        };
        let src = cx.src();
        let mut text = format!(
            "Record<{}, {}>",
            key_span.text(src),
            value_span.map_or("any", |s| s.text(src))
        );
        match optional {
            MappedModifier::Add | MappedModifier::Present => text = format!("Partial<{text}>"),
            MappedModifier::Remove => text = format!("Required<{text}>"),
            MappedModifier::None => {}
        }
        if matches!(readonly, MappedModifier::Add | MappedModifier::Present) {
            text = format!("Readonly<{text}>");
        }
        Self::report(cx, span, true, repair, text);
    }

    fn record_ref(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: NodeId, ta: Option<NodeId>) {
        let ast = cx.ast();
        if ident_name(ast, name) != Some("Record") {
            return;
        }
        let Some(Node::TypeArgs { args }) = ta.map(|t| *ast.node(t)) else {
            return;
        };
        let &[k, v] = ast.list(args) else { return };
        let (k, v) = (unparen(ast, k), unparen(ast, v));
        let keyword = matches!(
            ast.node(k),
            Node::KeywordType {
                kind: KeywordTypeKind::String | KeywordTypeKind::Number | KeywordTypeKind::Symbol
            }
        );
        let span = cx.span(node);
        let (ks, vs) = (cx.span(k), cx.span(v));
        let repair = if keyword && !Self::drops_comment(cx, span, &[ks, vs]) {
            Repair::Fix
        } else {
            Repair::Suggest
        };
        let src = cx.src();
        let text = format!("{{ [key: {}]: {} }}", ks.text(src), vs.text(src));
        Self::report(cx, span, false, repair, text);
    }
}

/// The interface's ESTree range: after `export` / `default`.
fn interface_span(src: &str, span: Span) -> Span {
    let mut pos = span.start as usize;
    for _ in 0..4 {
        let at = skip_trivia_forward(src, pos);
        let rest = src.get(at..span.end as usize).unwrap_or("");
        let word = rest.split(|c: char| !is_ident_char(c)).next().unwrap_or("");
        if matches!(word, "export" | "default") {
            pos = at + word.len();
        } else {
            return Span::new(at as u32, span.end);
        }
    }
    Span::new(pos as u32, span.end)
}

impl Rule for ConsistentIndexedObjectStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::TypeRef,
            NodeKind::Interface,
            NodeKind::MappedType,
            NodeKind::TypeLiteral,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::TypeRef { name, type_args } if self.index_signature => {
                self.record_ref(cx, node, name, type_args);
            }
            _ if self.index_signature => {}
            Node::Interface {
                modifiers,
                name,
                type_params,
                extends,
                body,
            } => {
                let src = cx.src();
                let generics = match type_params.map(|t| *ast.node(t)) {
                    Some(Node::TypeParams { params }) if !ast.list(params).is_empty() => format!(
                        "<{}>",
                        ast.list(params)
                            .iter()
                            .map(|&p| cx.text(p))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    _ => String::new(),
                };
                let prefix = format!("type {}{generics} = ", cx.text(name));
                let safe = ast.list(extends).is_empty() && !modifiers.contains(Modifiers::DEFAULT);
                let span = interface_span(src, cx.span(node));
                let members = ast.list(body).to_vec();
                Self::members(cx, &members, node, span, Some(name), &prefix, ";", safe);
            }
            Node::MappedType { .. } => self.mapped(cx, node),
            Node::TypeLiteral { members } => {
                let parent = Self::alias_name(cx, node);
                let members = ast.list(members).to_vec();
                let span = cx.span(node);
                Self::members(cx, &members, node, span, parent, "", "", true);
            }
            _ => {}
        }
    }
}
