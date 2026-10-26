//! Which JSDoc documents a symbol in hover and completion details, and links in it.
//!
//! The documentation shown comes from one declaration: the one hovered (a function or
//! method overload at its name, the single signature a call resolved to, the constructor
//! at `new C()`), else the symbol's value declaration (or its first interface, enum,
//! namespace, type parameter or type alias declaration). That declaration's own `/** */`
//! comment wins; otherwise
//! - a parameter takes its function's `@param` tag of the same name (a binding pattern,
//!   the `@param` tag at its position) and a type parameter the `@template` tag naming it,
//! - the first declarator of `var a = ..., b` takes the statement's comment,
//! - a function, arrow or class expression initializing a variable or property takes that
//!   variable's or property's documentation,
//! - an overload, method or constructor takes the first signature's documentation,
//! - a non-static class or interface member takes its base types' member of the same name.
//!
//! A comment with `@typedef` or `@callback` documents that type, not the declaration after
//! it. When nothing is found and the symbol is an alias, the aliased symbol's value (or
//! first) declaration is used. Rules from `tsc: tsc/internal/ls/hover.go`
//! (`getQuickInfoAndDocumentationForSymbol`, `getJSDocOrTag`, `writeNameLink`), v7.0.2.

use super::jsdoc::{self, JsDoc, Tag};
use super::quick_info::call_of;
use ts_r_ast::{Node, NodeId};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_types::{SymId, TypeId};

/// Documentation found for a declaration: a whole comment or one of its tags.
enum Found {
    Doc(JsDoc, Loc),
    Tag(Tag, Loc),
}

/// Recursion limit for documentation lookups (inherited members, initializers).
const MAX_DEPTH: u32 = 16;

/// The `/** */` comment right before node `d`.
fn own_raw(ck: &Ck<'_, '_>, d: Loc) -> Option<String> {
    let c = ck.c;
    let ast = c.ast(d.file);
    let text = c.file(d.file).text.as_str();
    if matches!(c.node(d), Node::VarDeclarator { .. }) {
        return None;
    }
    // `namespace a.b {}`: the comment is before the outermost declaration.
    let mut node = d;
    for _ in 0..64 {
        match c.parent(node) {
            Some(p) if matches!(*c.node(p), Node::Module { body: Some(b), .. } if b == node.node) => {
                node = p
            }
            Some(p) if matches!(*c.node(p), Node::ExportDefaultExpr { .. }) => node = p,
            _ => break,
        }
    }
    // A JSDoc `@typedef`/`@callback` alias is built from the comment it sits in.
    let span = ast.span(node.node);
    if matches!(c.node(node), Node::TypeAlias { .. })
        && let Some(comment) = ast
            .comments_between(0, u32::MAX)
            .iter()
            .find(|cm| cm.span.start < span.start && span.end <= cm.span.end)
    {
        let raw = comment.span.text(text);
        return raw.starts_with("/**").then(|| raw.to_string());
    }
    // The last `/** */` of the comments right before the node (other comments may follow).
    let mut pos = span.start;
    for comment in ast.comments_between(0, pos).iter().rev() {
        let between = text.get(comment.span.end as usize..pos as usize)?;
        if !between.trim().is_empty() {
            return None;
        }
        let raw = comment.span.text(text);
        if raw.starts_with("/**") && raw.len() > 4 {
            return Some(raw.to_string());
        }
        pos = comment.span.start;
    }
    None
}

/// The function-like node a parameter or type parameter list belongs to, seen as its
/// declaration (`MethodDef` for a method's `FunctionExpr`).
fn host_of(ck: &Ck<'_, '_>, n: Loc) -> Option<Loc> {
    let c = ck.c;
    let mut p = c.parent(n)?;
    if matches!(c.node(p), Node::TypeParams { .. }) {
        p = c.parent(p)?;
    }
    if matches!(c.node(p), Node::FunctionExpr { .. })
        && let Some(m) = c.parent(p)
        && matches!(*c.node(m), Node::MethodDef { value, .. } if value == p.node)
    {
        return Some(m);
    }
    Some(p)
}

/// The parameters of a function-like declaration.
fn params_of(ck: &Ck<'_, '_>, host: Loc) -> Vec<NodeId> {
    let ast = ck.c.ast(host.file);
    let n = match *ast.node(host.node) {
        Node::MethodDef { value, .. } => value,
        _ => host.node,
    };
    let list = match *ast.node(n) {
        Node::CallSignature { params, .. }
        | Node::ConstructSignature { params, .. }
        | Node::MethodSignature { params, .. } => params,
        _ => match ts_r_checker::nodes::fn_parts(ast, n) {
            Some(p) => p.params,
            None => return Vec::new(),
        },
    };
    ast.list(list).to_vec()
}

fn ident_text(ck: &Ck<'_, '_>, l: Loc) -> Option<String> {
    let ast = ck.c.ast(l.file);
    match *ast.node(l.node) {
        Node::Ident { name } => Some(ast.atom(name).to_string()),
        _ => None,
    }
}

fn is_function_like(n: &Node) -> bool {
    matches!(
        n,
        Node::FunctionDecl { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
    )
}

/// A declaration recorded as its name (`x` of `var x`, of a parameter `x`) or a method's
/// function: the declarator, parameter or method.
fn declaration_node(ck: &Ck<'_, '_>, d: Loc) -> Loc {
    let c = ck.c;
    // A method's `FunctionExpr` (a signature's declaration): the method.
    if matches!(c.node(d), Node::FunctionExpr { .. })
        && let Some(p) = c.parent(d)
        && matches!(*c.node(p), Node::MethodDef { value, .. } if value == d.node)
    {
        return p;
    }
    if matches!(c.node(d), Node::Ident { .. })
        && let Some(p) = c.parent(d)
    {
        match *c.node(p) {
            Node::VarDeclarator { name, .. } if name == d.node => return p,
            Node::Param { pattern, .. } if pattern == d.node => return p,
            Node::BindingProperty { key, value, .. }
                if value == Some(d.node) || (value.is_none() && key == d.node) =>
            {
                return p;
            }
            _ => {}
        }
    }
    // `m2` of `namespace m2.m3 {}`: the namespace declaration (`m3` is a namespace of its
    // own, declared inside it, with no comment).
    if matches!(c.node(d), Node::Ident { .. }) {
        let mut cur = d;
        for _ in 0..64 {
            match c.parent(cur) {
                Some(p) if matches!(*c.node(p), Node::QualifiedName { left, .. } if left == cur.node) => {
                    cur = p
                }
                Some(p) if matches!(*c.node(p), Node::Module { name, .. } if name == cur.node) => {
                    return p;
                }
                _ => break,
            }
        }
    }
    d
}

fn doc_or_tag(ck: &mut Ck<'_, '_>, d: Loc, depth: u32) -> Option<Found> {
    if depth > MAX_DEPTH {
        return None;
    }
    let d = declaration_node(ck, d);
    if let Some(raw) = own_raw(ck, d) {
        return Some(Found::Doc(jsdoc::parse(&raw), d));
    }
    let c = ck.c;
    match *c.node(d) {
        Node::Param { pattern, .. } => {
            let host = host_of(ck, d)?;
            let doc = match doc_or_tag(ck, host, depth + 1)? {
                Found::Doc(doc, at) => (doc, at),
                Found::Tag(..) => return None,
            };
            let tag = match ident_text(ck, Loc::new(d.file, pattern)) {
                Some(name) => doc.0.param(&name).cloned(),
                None => {
                    let i = params_of(ck, host).iter().position(|&p| p == d.node)?;
                    doc.0.param_at(i).cloned()
                }
            };
            return tag.map(|t| Found::Tag(t, doc.1));
        }
        Node::TypeParam { name, .. } => {
            let host = host_of(ck, d)?;
            let name = ident_text(ck, Loc::new(d.file, name))?;
            return match doc_or_tag(ck, host, depth + 1)? {
                Found::Doc(doc, at) => doc.template(&name).cloned().map(|t| Found::Tag(t, at)),
                Found::Tag(..) => None,
            };
        }
        Node::VarDeclarator { .. } => {
            let p = c.parent(d)?;
            if let Node::VarDecl { decls, .. } = *c.node(p)
                && c.ast(d.file).list(decls).first() == Some(&d.node)
            {
                return doc_or_tag(ck, p, depth + 1);
            }
        }
        // `{ a }` / `{ a: b }`: the documentation of property `a` of the destructured type.
        Node::BindingProperty { key, .. } => {
            let name = ident_text(ck, Loc::new(d.file, key))?;
            let pattern = c.parent(d)?;
            if !matches!(c.node(pattern), Node::ObjectPattern { .. }) {
                return None;
            }
            let t = ck.pattern_parent_type(pattern);
            let prop = ck.property_of_type(t, &name)?;
            return ck
                .c
                .decls(prop)
                .into_iter()
                .find_map(|pd| own_raw(ck, pd).map(|raw| Found::Doc(jsdoc::parse(&raw), pd)));
        }
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => {
            if let Some(p) = c.parent(d) {
                let init = match *c.node(p) {
                    Node::VarDeclarator { init, .. } => init == Some(d.node),
                    Node::PropertyDef { value, .. } => value == Some(d.node),
                    Node::Property { value, .. } => value == d.node,
                    _ => false,
                };
                if init {
                    return doc_or_tag(ck, p, depth + 1);
                }
            }
        }
        _ => {}
    }
    let s = c.sym_of_decl(d)?;
    if matches!(
        c.node(d),
        Node::FunctionDecl { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. }
            | Node::ConstructSignature { .. }
    ) {
        let first = c
            .decls(s)
            .into_iter()
            .find(|&x| is_function_like(c.node(x)));
        if let Some(f) = first
            && f != d
            && let Some(found) = doc_or_tag(ck, f, depth + 1)
        {
            return Some(found);
        }
    }
    // Members: the same-named member of the container's base types.
    let p = c.parent(d)?;
    let is_member_of = matches!(
        c.node(p),
        Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
    );
    if !is_member_of || is_static(c.node(d)) {
        return None;
    }
    let container = c.sym_of_decl(p)?;
    let name = c.sym_name(s).to_string();
    for b in ck.base_types(container).iter() {
        if let Some(prop) = ck.property_of_type(*b, &name)
            && let Some(vd) = ck.c.value_decl(prop)
            && let Some(found) = doc_or_tag(ck, vd, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

fn is_static(n: &Node) -> bool {
    match *n {
        Node::PropertyDef { modifiers, .. } | Node::MethodDef { modifiers, .. } => {
            modifiers.contains(ts_r_ast::Modifiers::STATIC)
        }
        _ => false,
    }
}

/// The declaration quick info describes `s` with at `at` (see the module docs).
fn hover_declaration(ck: &mut Ck<'_, '_>, s: SymId, at: Option<Loc>) -> Option<Loc> {
    let c = ck.c;
    let f = c.sym_flags(s);
    let decls = c.decls(s);
    let nodes: Vec<Loc> = decls.iter().map(|&d| declaration_node(ck, d)).collect();
    let find = |pred: fn(&Node) -> bool| nodes.iter().copied().find(|&d| pred(c.node(d)));
    if f.intersects(
        SymbolFlags::VARIABLE
            | SymbolFlags::PROPERTY
            | SymbolFlags::ACCESSOR
            | SymbolFlags::ENUM_MEMBER,
    ) && !f.intersects(SymbolFlags::FUNCTION | SymbolFlags::METHOD | SymbolFlags::CLASS)
    {
        // A union or intersection property declared in several places has no one
        // declaration.
        if let Some(x) = c.synth_sym(s)
            && x.decl.is_none()
            && x.origins.len() > 1
        {
            let first = c.value_decl(x.origins[0]);
            if x.origins.iter().any(|&o| c.value_decl(o) != first) {
                return None;
            }
        }
        return c.value_decl(s).or_else(|| c.first_decl(s));
    }
    if f.intersects(SymbolFlags::FUNCTION | SymbolFlags::METHOD | SymbolFlags::CONSTRUCTOR) {
        if let Some(at) = at {
            // On an overload's name: that overload.
            if let Some(p) = c.parent(at)
                && decls.contains(&p)
                && is_function_like(c.node(p))
                && ts_r_binder::decl_name_node(c.ast(p.file), p.node) == Some(at.node)
            {
                return Some(p);
            }
            if let Some(d) = single_signature_decl(ck, s, at, SigKind::Call) {
                return Some(d);
            }
        }
        return c.value_decl(s).or_else(|| c.first_decl(s));
    }
    if f.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE) {
        if f.intersects(SymbolFlags::CLASS)
            && let Some(at) = at
            && call_of(ck, at).is_some()
            && let Some(d) = single_signature_decl(ck, s, at, SigKind::Construct)
        {
            return Some(d);
        }
        if f.intersects(SymbolFlags::CLASS) {
            return c.value_decl(s);
        }
        return find(|n| matches!(n, Node::Interface { .. }));
    }
    if f.intersects(SymbolFlags::ENUM) {
        return find(|n| matches!(n, Node::Enum { .. }));
    }
    if f.intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE) {
        // Of `namespace a.b {}`, the comment documents `a`.
        let d = find(|n| matches!(n, Node::Module { .. }))?;
        if let Node::Module { name, .. } = *c.node(d)
            && matches!(c.node(Loc::new(d.file, name)), Node::QualifiedName { .. })
        {
            let mut left = name;
            while let Node::QualifiedName { left: l, .. } = *c.ast(d.file).node(left) {
                left = l;
            }
            if ident_text(ck, Loc::new(d.file, left)).as_deref() != Some(c.sym_name(s)) {
                return None;
            }
        }
        return Some(d);
    }
    if f.intersects(SymbolFlags::TYPE_PARAMETER) {
        return find(|n| matches!(n, Node::TypeParam { .. }));
    }
    if f.intersects(SymbolFlags::TYPE_ALIAS) {
        return find(|n| matches!(n, Node::TypeAlias { .. }));
    }
    c.value_decl(s).or_else(|| c.first_decl(s))
}

/// The declaration of the one signature quick info shows for `s` at `at`.
fn single_signature_decl(ck: &mut Ck<'_, '_>, s: SymId, at: Loc, kind: SigKind) -> Option<Loc> {
    let sigs = super::quick_info::signatures_at(ck, s, kind, at);
    if sigs.len() != 1 {
        return None;
    }
    ck.store().sig(sigs[0]).decl.and_then(Loc::unpack)
}

/// Markdown for a found comment or tag.
fn render(ck: &mut Ck<'_, '_>, found: Found, decl: Loc) -> String {
    match found {
        Found::Doc(doc, at) => {
            // A `@typedef` comment documents the type, unless `decl` is that type.
            if doc.declares_type() && !matches!(ck.c.node(decl), Node::TypeAlias { .. }) {
                return String::new();
            }
            // The type's `@property`, `@param` and `@returns` tags make it; tsc does not list
            // them.
            let mut doc = doc;
            if doc.declares_type() {
                doc.tags.retain(|t| {
                    !matches!(
                        t.name.as_str(),
                        "property" | "prop" | "param" | "arg" | "argument" | "returns" | "return"
                    )
                });
            }
            jsdoc::render(&doc, &mut |name| link_target(ck, at, name))
        }
        Found::Tag(tag, at) => {
            let mut out = String::new();
            jsdoc::write_parts(&mut out, &tag.comment, &mut |name| {
                link_target(ck, at, name)
            });
            out
        }
    }
}

/// The documentation of declaration `d` as markdown.
fn documentation_of(ck: &mut Ck<'_, '_>, d: Loc) -> String {
    match doc_or_tag(ck, d, 0) {
        Some(found) => render(ck, found, d),
        None => String::new(),
    }
}

/// The documentation hover shows for `s` at `at` (`None`: no location, as for completion
/// details).
pub fn documentation(ck: &mut Ck<'_, '_>, s: SymId, at: Option<Loc>) -> String {
    let c = ck.c;
    // At a call through a call or construct signature of an interface: that signature.
    if let Some(at) = at
        && let Some(call) = call_of(ck, at)
        && let Some(sig) = ck.resolved_signature(call)
        && let Some(d) = ck.store().sig(sig).decl.and_then(Loc::unpack)
        && matches!(
            c.node(d),
            Node::CallSignature { .. } | Node::ConstructSignature { .. }
        )
    {
        let doc = documentation_of(ck, d);
        if !doc.is_empty() {
            return doc;
        }
    }
    let target = if super::aliases::is_alias(c, s) {
        c.resolve_alias(s).unwrap_or(s)
    } else {
        s
    };
    if let Some(d) = hover_declaration(ck, target, at) {
        let doc = documentation_of(ck, d);
        if !doc.is_empty() {
            return doc;
        }
    }
    if target != s {
        for d in [c.value_decl(target), c.first_decl(target)]
            .into_iter()
            .flatten()
        {
            let doc = documentation_of(ck, d);
            if !doc.is_empty() {
                return doc;
            }
        }
    }
    String::new()
}

/// Where `{@link name}` in a comment attached to `at` leads, as a `file#l,c-l,c` URI.
fn link_target(ck: &mut Ck<'_, '_>, at: Loc, name: &str) -> Option<String> {
    let c = ck.c;
    let mut parts = name.split(['.', '#']);
    let first = parts.next()?;
    // Names resolve where the comment is: inside a function, its parameters are in scope.
    let ast = c.ast(at.file);
    let inner = match *ast.node(at.node) {
        Node::MethodDef { value, .. } => Some(value),
        _ => Some(at.node),
    }
    .and_then(|n| ts_r_checker::nodes::fn_parts(ast, n))
    .and_then(|p| p.body.or_else(|| ast.list(p.params).first().copied()));
    let mut s = inner
        .and_then(|n| ck.resolve_name_at(Loc::new(at.file, n), first, meaning::ALL))
        .or_else(|| ck.resolve_name_at(at, first, meaning::ALL))?;
    for part in parts {
        let base = c.resolve_alias(s).unwrap_or(s);
        s = match c.export_of(base, part) {
            Some(x) => x,
            None => {
                let t = ck.declared_type_of_symbol(base);
                match ck.property_of_type(t, part) {
                    Some(x) => x,
                    None => {
                        let t = ck.type_of_symbol(base);
                        ck.property_of_type(t, part)?
                    }
                }
            }
        };
    }
    let s = c.resolve_alias(s).unwrap_or(s);
    let d = c.first_decl(s)?;
    let (start, end) = super::decl_span(c, d);
    let f = c.file(d.file);
    let lines = super::LspLines::new(&f.text);
    let r = lines.range(&f.text, start, end);
    let n = |v: &serde_json::Value| v.as_u64().unwrap_or(0) + 1;
    Some(format!(
        "{}#{},{}-{},{}",
        crate::uri::path_to_uri(&f.path),
        n(&r["start"]["line"]),
        n(&r["start"]["character"]),
        n(&r["end"]["line"]),
        n(&r["end"]["character"]),
    ))
}

/// The documentation of the index signature a property access `name` on `t` goes
/// through: the first applicable one other than a `string` index, else the `string` one.
pub fn index_documentation(ck: &mut Ck<'_, '_>, t: TypeId, name: &str) -> String {
    let t = ck.apparent_type(t);
    let infos: Vec<_> = ck.resolved_members(t).index.to_vec();
    let numeric = name.parse::<f64>().is_ok_and(|n| n.is_finite());
    let lit = ck.store().string_literal(name, false);
    let mut string_info = None;
    let mut chosen = None;
    for i in infos {
        if i.key == TypeId::STRING {
            string_info = string_info.or(Some(i));
            continue;
        }
        let applies = if i.key == TypeId::NUMBER {
            numeric
        } else if i.key == TypeId::SYMBOL {
            false
        } else {
            ck.is_assignable(lit, i.key)
        };
        if applies {
            chosen = Some(i);
            break;
        }
    }
    let Some(info) = chosen.or(string_info) else {
        return String::new();
    };
    match info.decl.and_then(|(f, n)| Loc::unpack((f, n))) {
        Some(d) => documentation_of(ck, d),
        None => String::new(),
    }
}
