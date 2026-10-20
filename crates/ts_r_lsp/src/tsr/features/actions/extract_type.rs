//! "Extract type": a type node to a type alias or an interface, as tsserver 6.0.3 does it
//! (fixtures `refactor-extract-type-*`; observed, never from its source):
//!
//! | Action (kind) | Offered at | Result |
//! |---|---|---|
//! | `Extract to type alias` (`refactor.extract.type`) | a type | `type NewType<P> = <type>;` and a blank line before the enclosing statement; the type becomes `NewType<P>` |
//! | `Extract to interface` (`refactor.extract.interface`) | a type literal | `interface NewType<P> { ... }` the same way |
//!
//! - The type: from the token at the cursor or the selection's start (its leading whitespace
//!   and comments count), the innermost type around it; a cursor touching the end of a type
//!   also selects it. A selection ending past that type takes the first enclosing type that
//!   covers its end, if any (else the type stays: `number = 1` extracts `number`). When that
//!   is a union or intersection, only its members from the one at the start to the one at the
//!   end go (`string | [number | bool]ean | null` extracts `number | boolean`). On a cursor
//!   move (`implicit`) only a selection.
//! - Placement: before the innermost statement holding the type (the one in a block or the
//!   file), above its leading comments and JSDoc, at its indentation.
//! - Type parameters: those the type uses that are declared inside that statement (not
//!   inside the type), in order of first use, with their constraints (a mapped type's key
//!   with its `in` type: `K extends keyof T`).
//! - Name: `NewType`, or `NewType_1`, ... when a value or type of the file has it.
//! - Printing, as tsserver reprints the node: `A<B, C>` with normalized spacing, type
//!   literals and mapped types one member per line ending in `;` (nested literals too, one
//!   level deeper each), construct signatures as `new(...)`, a multi-line union on one line,
//!   literals as written; semicolons follow the file.
//! - Not offered (where tsserver's result would not compile or we can't print it): `this`
//!   types, an `infer` whose conditional type is outside the selection, a type parameter
//!   whose constraint needs one that isn't captured, comments inside the type, type
//!   predicates, named tuple members (tsserver writes `type NewType = a: string;`), the
//!   `const` of `as const`, JSDoc types in JavaScript (tsserver's `Extract to typedef`;
//!   not implemented).

use super::edit::Edit;
use super::exports::{prev_token_end, token_start};
use super::refactor::{leading_start, taken_names, unique, walk};
use super::{Action, Ctx, Request, style};
use ts_r_ast::{
    Ast, KeywordTypeKind, MappedModifier, MethodKind, Node, NodeId, ParentMap, TypeOperatorKind,
};
use ts_r_binder::SymbolFlags;

const REFACTOR: &str = "Extract type";
const ALIAS: &str = "Extract to type alias";
const INTERFACE: &str = "Extract to interface";

/// Node kinds that are always types.
fn is_type_kind(n: &Node) -> bool {
    matches!(
        n,
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::ThisType {}
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::TypeLiteral { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::InferType { .. }
            | Node::MappedType { .. }
            | Node::ParenType { .. }
            | Node::TypePredicate { .. }
            | Node::ImportType { .. }
    )
}

/// Whether `n` is a type: a type kind, or a literal in a type position.
fn is_type(ast: &Ast, parents: &ParentMap, n: NodeId) -> bool {
    let node = ast.node(n);
    if is_type_kind(node) {
        return true;
    }
    if !matches!(
        node,
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::Unary { .. }
    ) {
        return false;
    }
    let Some(p) = parents.parent(n) else {
        return false;
    };
    let slot = |x: Option<NodeId>| x == Some(n);
    match *ast.node(p) {
        Node::ImportType { .. } => false,
        ref pn if is_type_kind(pn) => true,
        Node::TypeArgs { .. } => true,
        Node::VarDeclarator { type_ann, .. }
        | Node::Param { type_ann, .. }
        | Node::PropertyDef { type_ann, .. }
        | Node::PropertySignature { type_ann, .. }
        | Node::IndexSignature { type_ann, .. }
        | Node::CatchClause { type_ann, .. } => slot(type_ann),
        Node::MethodSignature { return_type, .. }
        | Node::CallSignature { return_type, .. }
        | Node::ConstructSignature { return_type, .. }
        | Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => slot(return_type),
        Node::TypeAlias { ty, .. } => ty == n,
        Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } => {
            ty == n
        }
        Node::TypeParam {
            constraint,
            default,
            ..
        } => slot(constraint) || slot(default),
        _ => false,
    }
}

/// The innermost node whose span holds `pos`.
fn innermost(ast: &Ast, pos: u32) -> Option<NodeId> {
    let mut cur = ast.root;
    let sp = ast.span(cur);
    if pos < sp.start || pos >= sp.end {
        return None;
    }
    for _ in 0..ast.node_count() {
        let mut inside = None;
        ast.for_each_child(cur, |c| {
            let s = ast.span(c);
            if inside.is_none() && s.start <= pos && pos < s.end {
                inside = Some(c);
            }
        });
        match inside {
            Some(c) if c != cur => cur = c,
            _ => break,
        }
    }
    Some(cur)
}

/// The first type at or above the innermost node holding `pos`.
fn type_around(ast: &Ast, parents: &ParentMap, pos: u32) -> Option<NodeId> {
    let n = innermost(ast, pos)?;
    std::iter::once(n)
        .chain(parents.ancestors(n))
        .find(|&a| is_type(ast, parents, a))
}

/// The type a request selects, and the members of a union or intersection it selects
/// (all of them, or the run from the one at its start to the one at its end).
fn selected_type(
    ast: &Ast,
    parents: &ParentMap,
    text: &str,
    s: u32,
    e: u32,
) -> Option<(NodeId, Vec<NodeId>)> {
    let q = token_start(ast, text, s);
    let mut n = type_around(ast, parents, q);
    if n.is_none() && s == e && s > 0 && prev_token_end(ast, text, s) == s {
        // A cursor touching the end of a type.
        n = type_around(ast, parents, s - 1);
    }
    let mut n = n?;
    let e = prev_token_end(ast, text, e).max(s);
    if e > ast.span(n).end {
        let mut cur = n;
        while let Some(p) = parents.parent(cur) {
            if !is_type(ast, parents, p) {
                break;
            }
            cur = p;
            if ast.span(cur).end >= e {
                n = cur;
                if let Node::UnionType { types } | Node::IntersectionType { types } = *ast.node(n) {
                    let parts: Vec<NodeId> = ast
                        .list(types)
                        .iter()
                        .copied()
                        .filter(|&m| ast.span(m).end > q && ast.span(m).start < e)
                        .collect();
                    if !parts.is_empty() && parts.len() < ast.list(types).len() {
                        return Some((n, parts));
                    }
                }
                break;
            }
        }
    }
    Some((n, vec![n]))
}

/// "Extract type" actions at the request's selection.
pub fn extract_type(cx: &Ctx<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    let want_alias = req.wants("refactor.extract.type");
    let want_interface = req.wants("refactor.extract.interface");
    if !want_alias && !want_interface {
        return;
    }
    if !req.invoked && req.start == req.end {
        return;
    }
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    if !ast.source_kind.is_typescript() {
        return;
    }
    let text = f.text.as_str();
    let parents = f.parents();
    let Some((n, parts)) = selected_type(ast, parents, text, req.start, req.end) else {
        return;
    };
    if matches!(
        ast.node(n),
        Node::TypePredicate { .. }
            | Node::NamedTupleMember { .. }
            | Node::OptionalType { .. }
            | Node::RestType { .. }
    ) {
        return;
    }
    // `as const` is not a type.
    if let Node::KeywordType {
        kind: KeywordTypeKind::Const | KeywordTypeKind::Intrinsic,
    } = ast.node(n)
    {
        return;
    }
    let Some(x) = Extraction::new(cx, n, parts) else {
        return;
    };
    if want_alias && let Some(edits) = x.edits(false) {
        out.push(action(cx, ALIAS, "refactor.extract.type", edits));
    }
    if want_interface
        && matches!(ast.node(x.node), Node::TypeLiteral { .. })
        && let Some(edits) = x.edits(true)
    {
        out.push(action(cx, INTERFACE, "refactor.extract.interface", edits));
    }
}

fn action(cx: &Ctx<'_, '_>, title: &str, kind: &str, edits: Vec<Edit>) -> Action {
    let mut a = Action::new(title, kind);
    a.refactor = Some(format!("{REFACTOR}/{title}"));
    a.edit(&cx.checker.file(cx.file).path, edits);
    a
}

/// What extracting type `n` needs: where the declaration goes, its name and type parameters.
struct Extraction<'a> {
    ast: &'a Ast,
    text: &'a str,
    node: NodeId,
    /// The selected members of a union or intersection `node`, or just `node`.
    parts: Vec<NodeId>,
    /// What the reference replaces.
    span: ts_r_ast::Span,
    /// Where the declaration is inserted (a line start) and its indentation.
    at: u32,
    indent: String,
    name: String,
    /// Captured type parameters: (name, constraint).
    params: Vec<(String, Option<NodeId>)>,
}

impl<'a> Extraction<'a> {
    fn new(cx: &'a Ctx<'_, '_>, n: NodeId, parts: Vec<NodeId>) -> Option<Extraction<'a>> {
        let f = cx.checker.file(cx.file);
        let ast = &f.ast;
        let text = f.text.as_str();
        let b = &f.bound;
        let parents = f.parents();
        let (first, last) = (*parts.first()?, *parts.last()?);
        let nsp = ts_r_ast::Span::new(ast.span(first).start, ast.span(last).end);
        if !ast.comments_in(nsp).is_empty() {
            return None;
        }
        // The statement it goes before.
        let mut stmt = None;
        let mut cur = n;
        for p in parents.ancestors(n) {
            if matches!(
                ast.node(p),
                Node::Program { .. }
                    | Node::Block { .. }
                    | Node::ModuleBlock { .. }
                    | Node::SwitchCase { .. }
            ) {
                stmt = Some(cur);
                break;
            }
            cur = p;
        }
        let stmt = stmt?;
        // Not between an overloaded function's declarations: before the first one.
        let stmt = match ast.node(stmt) {
            Node::FunctionDecl { .. } => f
                .bound
                .symbol_of_decl(stmt)
                .and_then(|s| {
                    f.bound
                        .decls(s)
                        .iter()
                        .copied()
                        .filter(|&d| {
                            matches!(ast.node(d), Node::FunctionDecl { .. })
                                && parents.parent(d) == parents.parent(stmt)
                        })
                        .min_by_key(|&d| ast.span(d).start)
                })
                .unwrap_or(stmt),
            _ => stmt,
        };
        if !ast.node(stmt).is_statement() {
            return None;
        }
        let ssp = ast.span(stmt);
        let lead = leading_start(ast, text, stmt);
        let at = style::line_start(text, lead);
        if !style::blank(text, at, lead) {
            return None;
        }
        let indent = style::line_indent(text, ssp.start).to_string();
        // Types we can't move, and the type parameters it needs.
        let mut ok = true;
        let mut params: Vec<(String, Option<NodeId>)> = Vec::new();
        let mut captured: Vec<NodeId> = Vec::new();
        let inside = |d: NodeId, sp: ts_r_ast::Span| {
            let ds = ast.span(d);
            sp.start <= ds.start && ds.start < sp.end
        };
        let mut visit = |c: NodeId| {
            match *ast.node(c) {
                Node::ThisType {} => ok = false,
                Node::InferType { .. } => {
                    // Its conditional type must be extracted too.
                    let owner = parents
                        .ancestors(c)
                        .find(|&a| matches!(ast.node(a), Node::ConditionalType { .. }));
                    if owner.is_none_or(|o| !inside(o, nsp)) {
                        ok = false;
                    }
                }
                Node::TypeRef { name, .. } if matches!(ast.node(name), Node::Ident { .. }) => {
                    if let Some(sym) = b.resolved(name)
                        && b.symbol(sym).flags.intersects(SymbolFlags::TYPE_PARAMETER)
                        && let Some(&d) = b.decls(sym).first()
                        && inside(d, ssp)
                        && !inside(d, nsp)
                        && !captured.contains(&d)
                    {
                        captured.push(d);
                    }
                }
                _ => {}
            }
            true
        };
        for &part in &parts {
            walk(ast, part, &mut visit);
        }
        if !ok {
            return None;
        }
        for &d in &captured {
            let (name, constraint) = match *ast.node(d) {
                Node::TypeParam {
                    name, constraint, ..
                } => (name, constraint),
                _ => (d, None),
            };
            let name = ast.name_of(name)?.to_string();
            // A constraint naming a type parameter we don't pass can't be written.
            if let Some(c) = constraint {
                let mut bad = false;
                walk(ast, c, &mut |x| {
                    if let Node::TypeRef { name, .. } = *ast.node(x)
                        && let Some(sym) = b.resolved(name)
                        && b.symbol(sym).flags.intersects(SymbolFlags::TYPE_PARAMETER)
                        && let Some(&dd) = b.decls(sym).first()
                        && inside(dd, ssp)
                        && !captured.contains(&dd)
                    {
                        bad = true;
                    }
                    true
                });
                if bad {
                    return None;
                }
            }
            params.push((name, constraint));
        }
        let name = unique("NewType", &taken_names(ast, b));
        Some(Extraction {
            ast,
            text,
            node: n,
            parts,
            span: nsp,
            at,
            indent,
            name,
            params,
        })
    }

    fn edits(&self, interface: bool) -> Option<Vec<Edit>> {
        let ast = self.ast;
        let text = self.text;
        let unit = style::indent_unit(text);
        let nl = style::newline(text);
        let semi = style::semi(ast, text);
        let p = Printer {
            ast,
            text,
            unit: &unit,
            base: &self.indent,
            nl,
        };
        let mut tparams = Vec::new();
        for (name, c) in &self.params {
            match c {
                Some(c) => tparams.push(format!("{name} extends {}", p.ty(*c, 0)?)),
                None => tparams.push(name.clone()),
            }
        }
        let tp = if tparams.is_empty() {
            String::new()
        } else {
            format!("<{}>", tparams.join(", "))
        };
        let ind = &self.indent;
        let decl = if interface {
            let Node::TypeLiteral { members } = *ast.node(self.node) else {
                return None;
            };
            let mut s = format!("{ind}interface {}{tp} {{{nl}", self.name);
            for &m in ast.list(members) {
                s.push_str(&format!("{ind}{unit}{};{nl}", p.member(m, 1)?));
            }
            s.push_str(&format!("{ind}}}"));
            s
        } else {
            let sep = match ast.node(self.node) {
                Node::IntersectionType { .. } => " & ",
                _ => " | ",
            };
            format!(
                "{ind}type {}{tp} = {}{semi}",
                self.name,
                p.list(&self.parts, 0, sep)?
            )
        };
        let args: Vec<&str> = self.params.iter().map(|(n, _)| n.as_str()).collect();
        let use_text = if args.is_empty() {
            self.name.clone()
        } else {
            format!("{}<{}>", self.name, args.join(", "))
        };
        let sp = self.span;
        Some(vec![
            Edit::insert(self.at, format!("{decl}{nl}{nl}")),
            Edit::new(sp.start, sp.end, use_text),
        ])
    }
}

/// Prints type nodes the way tsserver reprints them.
struct Printer<'a> {
    ast: &'a Ast,
    text: &'a str,
    unit: &'a str,
    /// Indentation of the declaration's line.
    base: &'a str,
    nl: &'a str,
}

impl Printer<'_> {
    fn src(&self, n: NodeId) -> &str {
        self.ast.span(n).text(self.text)
    }

    fn indent(&self, level: usize) -> String {
        format!("{}{}", self.base, self.unit.repeat(level))
    }

    fn list(&self, items: &[NodeId], level: usize, sep: &str) -> Option<String> {
        let mut v = Vec::with_capacity(items.len());
        for &i in items {
            v.push(self.ty(i, level)?);
        }
        Some(v.join(sep))
    }

    fn entity(&self, n: NodeId) -> Option<String> {
        match *self.ast.node(n) {
            Node::Ident { .. } => Some(self.src(n).to_string()),
            Node::QualifiedName { left, right } => {
                Some(format!("{}.{}", self.entity(left)?, self.entity(right)?))
            }
            _ => None,
        }
    }

    fn type_args(&self, ta: Option<NodeId>, level: usize) -> Option<String> {
        let Some(ta) = ta else {
            return Some(String::new());
        };
        let Node::TypeArgs { args } = *self.ast.node(ta) else {
            return None;
        };
        Some(format!(
            "<{}>",
            self.list(self.ast.list(args), level, ", ")?
        ))
    }

    fn type_params(&self, tp: Option<NodeId>, level: usize) -> Option<String> {
        let Some(tp) = tp else {
            return Some(String::new());
        };
        let Node::TypeParams { params } = *self.ast.node(tp) else {
            return None;
        };
        let mut v = Vec::new();
        for &p in self.ast.list(params) {
            let Node::TypeParam {
                name,
                constraint,
                default,
                ..
            } = *self.ast.node(p)
            else {
                return None;
            };
            let mut s = self.src(name).to_string();
            if let Some(c) = constraint {
                s.push_str(&format!(" extends {}", self.ty(c, level)?));
            }
            if let Some(d) = default {
                s.push_str(&format!(" = {}", self.ty(d, level)?));
            }
            v.push(s);
        }
        Some(format!("<{}>", v.join(", ")))
    }

    fn params(&self, params: &[NodeId], level: usize) -> Option<String> {
        let mut v = Vec::new();
        for &p in params {
            let Node::Param {
                rest,
                pattern,
                optional,
                type_ann,
                init: None,
                ..
            } = *self.ast.node(p)
            else {
                return None;
            };
            let mut s = String::new();
            if rest {
                s.push_str("...");
            }
            s.push_str(self.src(pattern));
            if optional {
                s.push('?');
            }
            if let Some(t) = type_ann {
                s.push_str(&format!(": {}", self.ty(t, level)?));
            }
            v.push(s);
        }
        Some(v.join(", "))
    }

    fn ret(&self, r: Option<NodeId>, level: usize) -> Option<String> {
        match r {
            Some(r) => Some(format!(": {}", self.ty(r, level)?)),
            None => Some(String::new()),
        }
    }

    fn key(&self, k: NodeId) -> Option<String> {
        match *self.ast.node(k) {
            Node::Ident { .. } | Node::StringLit { .. } | Node::NumberLit { .. } => {
                Some(self.src(k).to_string())
            }
            Node::ComputedKey { expr } => Some(format!("[{}]", self.src(expr))),
            _ => None,
        }
    }

    /// A type literal member, without its `;`.
    fn member(&self, m: NodeId, level: usize) -> Option<String> {
        let ast = self.ast;
        let readonly = |mods: ts_r_ast::Modifiers| {
            if mods.contains(ts_r_ast::Modifiers::READONLY) {
                "readonly "
            } else {
                ""
            }
        };
        match *ast.node(m) {
            Node::PropertySignature {
                modifiers,
                key,
                optional,
                type_ann,
            } => Some(format!(
                "{}{}{}{}",
                readonly(modifiers),
                self.key(key)?,
                if optional { "?" } else { "" },
                self.ret(type_ann, level)?
            )),
            Node::MethodSignature {
                kind,
                key,
                optional,
                type_params,
                params,
                return_type,
                ..
            } => Some(format!(
                "{}{}{}{}({}){}",
                match kind {
                    MethodKind::Get => "get ",
                    MethodKind::Set => "set ",
                    _ => "",
                },
                self.key(key)?,
                if optional { "?" } else { "" },
                self.type_params(type_params, level)?,
                self.params(ast.list(params), level)?,
                self.ret(return_type, level)?
            )),
            Node::CallSignature {
                type_params,
                params,
                return_type,
            } => Some(format!(
                "{}({}){}",
                self.type_params(type_params, level)?,
                self.params(ast.list(params), level)?,
                self.ret(return_type, level)?
            )),
            Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => Some(format!(
                "new{}({}){}",
                self.type_params(type_params, level)?,
                self.params(ast.list(params), level)?,
                self.ret(return_type, level)?
            )),
            Node::IndexSignature {
                modifiers,
                params,
                type_ann,
            } => Some(format!(
                "{}[{}]{}",
                readonly(modifiers),
                self.params(ast.list(params), level)?,
                self.ret(type_ann, level)?
            )),
            _ => None,
        }
    }

    /// Type `n` at nesting `level` (members of a type literal go one level deeper).
    fn ty(&self, n: NodeId, level: usize) -> Option<String> {
        let ast = self.ast;
        let nl = self.nl;
        Some(match *ast.node(n) {
            Node::KeywordType { .. }
            | Node::ThisType {}
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::TemplateLitType { .. } => self.src(n).to_string(),
            Node::Unary { arg, .. } => format!("-{}", self.src(arg)),
            Node::TypeRef { name, type_args } => {
                format!(
                    "{}{}",
                    self.entity(name)?,
                    self.type_args(type_args, level)?
                )
            }
            Node::ArrayType { elem } => format!("{}[]", self.ty(elem, level)?),
            Node::TupleType { elems } => format!("[{}]", self.list(ast.list(elems), level, ", ")?),
            Node::NamedTupleMember {
                rest,
                name,
                optional,
                ty,
            } => format!(
                "{}{}{}: {}",
                if rest { "..." } else { "" },
                self.src(name),
                if optional { "?" } else { "" },
                self.ty(ty, level)?
            ),
            Node::OptionalType { ty } => format!("{}?", self.ty(ty, level)?),
            Node::RestType { ty } => format!("...{}", self.ty(ty, level)?),
            Node::UnionType { types } => self.list(ast.list(types), level, " | ")?,
            Node::IntersectionType { types } => self.list(ast.list(types), level, " & ")?,
            Node::FunctionType {
                type_params,
                params,
                return_type,
            } => format!(
                "{}({}) => {}",
                self.type_params(type_params, level)?,
                self.params(ast.list(params), level)?,
                self.ty(return_type, level)?
            ),
            Node::ConstructorType {
                is_abstract,
                type_params,
                params,
                return_type,
            } => format!(
                "{}new {}({}) => {}",
                if is_abstract { "abstract " } else { "" },
                self.type_params(type_params, level)?,
                self.params(ast.list(params), level)?,
                self.ty(return_type, level)?
            ),
            Node::TypeLiteral { members } => {
                let ms = ast.list(members);
                if ms.is_empty() {
                    return Some("{}".into());
                }
                let mut s = format!("{{{nl}");
                for &m in ms {
                    s.push_str(&format!(
                        "{}{};{nl}",
                        self.indent(level + 1),
                        self.member(m, level + 1)?
                    ));
                }
                s.push_str(&format!("{}}}", self.indent(level)));
                s
            }
            Node::MappedType {
                readonly,
                type_param,
                name_type,
                optional,
                ty,
            } => {
                let Node::TypeParam {
                    name,
                    constraint: Some(c),
                    ..
                } = *ast.node(type_param)
                else {
                    return None;
                };
                let ro = match readonly {
                    MappedModifier::None => "",
                    MappedModifier::Present => "readonly ",
                    MappedModifier::Add => "+readonly ",
                    MappedModifier::Remove => "-readonly ",
                };
                let opt = match optional {
                    MappedModifier::None => "",
                    MappedModifier::Present => "?",
                    MappedModifier::Add => "+?",
                    MappedModifier::Remove => "-?",
                };
                let as_ = match name_type {
                    Some(t) => format!(" as {}", self.ty(t, level + 1)?),
                    None => String::new(),
                };
                let body = match ty {
                    Some(t) => format!(": {}", self.ty(t, level + 1)?),
                    None => String::new(),
                };
                format!(
                    "{{{nl}{}{ro}[{} in {}{as_}]{opt}{body};{nl}{}}}",
                    self.indent(level + 1),
                    self.src(name),
                    self.ty(c, level + 1)?,
                    self.indent(level)
                )
            }
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                let e = match *ast.node(expr_name) {
                    Node::ImportType { .. } => self.ty(expr_name, level)?,
                    _ => self.entity(expr_name)?,
                };
                format!("typeof {e}{}", self.type_args(type_args, level)?)
            }
            Node::TypeOperator { op, ty } => format!(
                "{}{}",
                match op {
                    TypeOperatorKind::KeyOf => "keyof ",
                    TypeOperatorKind::Unique => "unique ",
                    TypeOperatorKind::Readonly => "readonly ",
                },
                self.ty(ty, level)?
            ),
            Node::IndexedAccessType { obj, index } => {
                format!("{}[{}]", self.ty(obj, level)?, self.ty(index, level)?)
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => format!(
                "{} extends {} ? {} : {}",
                self.ty(check, level)?,
                self.ty(extends, level)?,
                self.ty(true_ty, level)?,
                self.ty(false_ty, level)?
            ),
            Node::InferType { type_param } => {
                let Node::TypeParam {
                    name, constraint, ..
                } = *ast.node(type_param)
                else {
                    return None;
                };
                match constraint {
                    Some(c) => format!("infer {} extends {}", self.src(name), self.ty(c, level)?),
                    None => format!("infer {}", self.src(name)),
                }
            }
            Node::ParenType { ty } => format!("({})", self.ty(ty, level)?),
            Node::TypePredicate { asserts, param, ty } => {
                let mut s = String::new();
                if asserts {
                    s.push_str("asserts ");
                }
                s.push_str(self.src(param));
                if let Some(t) = ty {
                    s.push_str(&format!(" is {}", self.ty(t, level)?));
                }
                s
            }
            Node::ImportType {
                is_typeof,
                arg,
                attributes: None,
                qualifier,
                type_args,
            } => {
                let mut s = String::new();
                if is_typeof {
                    s.push_str("typeof ");
                }
                s.push_str(&format!("import({})", self.src(arg)));
                if let Some(q) = qualifier {
                    s.push('.');
                    s.push_str(&self.entity(q)?);
                }
                s.push_str(&self.type_args(type_args, level)?);
                s
            }
            _ => return None,
        })
    }
}
