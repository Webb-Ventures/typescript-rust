//! Class-related completions: the members a class body may declare next (inherited ones not
//! written yet), the `this` members offered among the names in scope inside a class, and
//! which members a position may see.
//!
//! Rules learned from tsc 7 (`tsc: tsc/internal/ls/completions.go`: the class-like
//! completion symbols, `filterClassMembersList`, the object-type-declaration container and
//! the `this`-type members added to global completions), written here on our AST.

use super::position::token_before;
use std::collections::HashSet;
use ts_r_ast::{HeritageKind, MethodKind, Modifiers, Node, NodeId};
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{SymId, TypeId};

/// The keywords that may start a class member (offered with the member names).
pub const CLASS_ELEMENT_KEYWORDS: &[&str] = &[
    "abstract",
    "accessor",
    "async",
    "constructor",
    "declare",
    "get",
    "override",
    "private",
    "protected",
    "public",
    "readonly",
    "set",
    "static",
];

/// The keywords that may start an interface or type literal member.
pub const TYPE_ELEMENT_KEYWORDS: &[&str] = &["get", "readonly", "set"];

/// Words that may precede a member name as modifiers.
fn is_member_modifier(w: &str) -> bool {
    matches!(
        w,
        "public"
            | "private"
            | "protected"
            | "static"
            | "override"
            | "readonly"
            | "abstract"
            | "async"
            | "declare"
            | "accessor"
            | "get"
            | "set"
    )
}

fn is_class(n: &Node) -> bool {
    matches!(n, Node::ClassDecl { .. } | Node::ClassExpr { .. })
}

fn is_object_type_decl(n: &Node) -> bool {
    is_class(n) || matches!(n, Node::Interface { .. } | Node::TypeLiteral { .. })
}

fn members_of(n: &Node) -> Option<ts_r_ast::NodeList> {
    match *n {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => Some(members),
        Node::Interface { body, .. } => Some(body),
        Node::TypeLiteral { members } => Some(members),
        _ => None,
    }
}

/// A member-name position in a class, interface or type literal body.
pub struct MemberSlot {
    pub container: NodeId,
    pub is_class: bool,
    /// Modifiers written before the cursor (`static |`, `private |`, `override |`).
    pub modifiers: Modifiers,
}

/// The class, interface or type literal body whose next member name the cursor is typing:
/// after its `{`, after a member (`;`, `}`, `,`, or on a later line), or after member
/// modifiers. `word` is where the word being typed starts.
pub fn member_slot(c: &Checker<'_>, file: FileId, anchor: NodeId, word: u32) -> Option<MemberSlot> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (container, open) = body_position(c, file, anchor, word)?;
    let node = ast.node(container);
    let list = members_of(node)?;
    // Modifier words written before the cursor, then the token they follow.
    let mut modifiers = Modifiers::NONE;
    let mut pos = word;
    let mut tok = token_before(ast, text, pos);
    for _ in 0..16 {
        let Some((at, w)) = tok else { break };
        if !is_member_modifier(w) || at <= open {
            break;
        }
        modifiers |= match w {
            "private" => Modifiers::PRIVATE,
            "static" => Modifiers::STATIC,
            "override" => Modifiers::OVERRIDE,
            _ => Modifiers::NONE,
        };
        pos = at;
        tok = token_before(ast, text, pos);
    }
    // A member may follow `{`, `;`, `}`, `,`, modifiers, or a complete member that ends a
    // line (not an operator still waiting for its operand, as in `x =⏎|`).
    let ok = match tok {
        Some((at, t)) => {
            let end = at + t.len() as u32;
            at == open
                || matches!(t, ";" | ",")
                || (t == "}" && !closes_object(ast, at))
                || pos != word
                || (text[end as usize..pos as usize].contains('\n')
                    && ends_operand(t)
                    && ast.list(list).iter().any(|&m| ast.span(m).end == end))
        }
        None => false,
    };
    ok.then_some(MemberSlot {
        container,
        is_class: is_class(node),
        modifiers,
    })
}

/// Where the word starting at `word` sits in a class, interface or type literal body (between
/// members, or as the name of the member being typed): the body's declaration and the offset
/// of its `{`.
fn body_position(
    c: &Checker<'_>,
    file: FileId,
    anchor: NodeId,
    word: u32,
) -> Option<(NodeId, u32)> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    // The body node: the anchor itself (the cursor between members), or the declaration
    // whose member name the cursor is on.
    let mut own = None;
    let container = if is_object_type_decl(ast.node(anchor)) {
        anchor
    } else {
        let member = if matches!(ast.node(anchor), Node::Ident { .. }) {
            let p = parents.parent(anchor)?;
            let key = match *ast.node(p) {
                Node::PropertyDef { key, .. }
                | Node::MethodDef { key, .. }
                | Node::PropertySignature { key, .. }
                | Node::MethodSignature { key, .. } => key,
                _ => return None,
            };
            if key != anchor {
                return None;
            }
            own = Some(p);
            p
        } else {
            return None;
        };
        let decl = parents.parent(member)?;
        if !is_object_type_decl(ast.node(decl)) {
            return None;
        }
        decl
    };
    // The cursor must be inside the body's braces, past every name, type parameter and
    // heritage clause.
    let node = ast.node(container);
    let mut head_end = ast.span(container).start;
    let mut bump = |n: Option<NodeId>| {
        if let Some(n) = n {
            head_end = head_end.max(ast.span(n).end);
        }
    };
    match *node {
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
            bump(name);
            bump(type_params);
            for &h in ast.list(heritage) {
                bump(Some(h));
            }
        }
        Node::Interface {
            name,
            type_params,
            extends,
            ..
        } => {
            bump(Some(name));
            bump(type_params);
            for &h in ast.list(extends) {
                bump(Some(h));
            }
        }
        _ => {}
    }
    let open = brace_after(ast, text, head_end)?;
    let end = ast.span(container).end;
    // A body the parser closed at the end of the file without its `}` (`Bar<{|`) holds the
    // position right after it too.
    let unclosed = text[end as usize..].trim().is_empty()
        && !(end > open + 1 && text.as_bytes()[end as usize - 1] == b'}');
    let trailing = text.get(end as usize..word as usize).unwrap_or("x");
    if word <= open || (word >= end && !(unclosed && trailing.trim().is_empty())) {
        return None;
    }
    // No other member holds the word.
    let list = members_of(node)?;
    if ast.list(list).iter().any(|&m| {
        let s = ast.span(m);
        Some(m) != own && s.start < word && word < s.end
    }) {
        return None;
    }
    Some((container, open))
}

/// The first `{` at or after `from` that is not inside a comment.
fn brace_after(ast: &ts_r_ast::Ast, text: &str, from: u32) -> Option<u32> {
    let mut at = from as usize;
    // Each round steps past one comment, so this ends after one round per comment.
    for _ in 0..=ast.comments.len() {
        let i = text.get(at..)?.find('{')? + at;
        let k = ast.comments.partition_point(|c| (c.span.end as usize) <= i);
        match ast.comments.get(k) {
            Some(c) if (c.span.start as usize) <= i => at = c.span.end as usize,
            _ => return Some(i as u32),
        }
    }
    None
}

/// Whether the word starting at `word` is in a class or interface body outside any member
/// (where only member names may be written).
pub fn in_body(c: &Checker<'_>, file: FileId, anchor: NodeId, word: u32) -> bool {
    let ast = c.ast(file);
    let Some((n, _)) = body_position(c, file, anchor, word) else {
        return false;
    };
    if !matches!(
        ast.node(n),
        Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::Interface { .. }
    ) {
        return false;
    }
    // Right after a complete member (not after an operator waiting for its operand).
    let text = c.file(file).text.as_str();
    let Some((at, t)) = token_before(ast, text, word) else {
        return false;
    };
    let end = at + t.len() as u32;
    ends_operand(t)
        && members_of(ast.node(n))
            .is_some_and(|l| ast.list(l).iter().any(|&m| ast.span(m).end == end))
}

/// Whether the `}` at `at` closes an object literal or object type (a property's value),
/// not a method body or static block.
fn closes_object(ast: &ts_r_ast::Ast, at: u32) -> bool {
    super::position::node_covering(ast, at).is_some_and(|n| {
        matches!(
            ast.node(n),
            Node::ObjectLit { .. } | Node::TypeLiteral { .. } | Node::ObjectPattern { .. }
        )
    })
}

/// Whether a token can end an expression or type (a word, a closing bracket, a literal).
fn ends_operand(t: &str) -> bool {
    t.bytes()
        .last()
        .is_some_and(|b| b.is_ascii_alphanumeric() || b"_$)]}'\"`".contains(&b))
}

/// The members of the base class and implemented interfaces a class body may declare at
/// `slot`: not private, not `#private`, not already written (with the same `static`-ness).
/// `static` lists the base constructor's statics; `override` only the base class's; a
/// `private` member gets no list.
pub fn inherited_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    slot: &MemberSlot,
    offset: u32,
) -> Vec<(String, SymId)> {
    let c = ck.c;
    let ast = c.ast(file);
    if !slot.is_class || slot.modifiers.contains(Modifiers::PRIVATE) {
        return Vec::new();
    }
    let Some(class) = c.sym_of_decl(Loc::new(file, slot.container)) else {
        return Vec::new();
    };
    let is_static = slot.modifiers.contains(Modifiers::STATIC);
    let heritage = match *ast.node(slot.container) {
        Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
        _ => return Vec::new(),
    };
    let mut bases: Vec<TypeId> = Vec::new();
    if is_static {
        if let Some(t) = ck.base_constructor_type(class) {
            bases.push(t);
        }
    } else {
        bases.extend(ck.base_types(class).iter().copied());
    }
    if !slot.modifiers.contains(Modifiers::OVERRIDE) {
        for &h in ast.list(heritage) {
            let Node::HeritageClause {
                kind: HeritageKind::Implements,
                types,
            } = *ast.node(h)
            else {
                continue;
            };
            for &t in ast.list(types) {
                let ty = ck.quietly(|ck| ck.type_from_node(Loc::new(file, t)));
                if is_static {
                    // An interface has no static side.
                    continue;
                }
                bases.push(ty);
            }
        }
    }
    // Names the body already declares (not the member being typed, not private ones, only
    // those of the same static-ness).
    let mut written: HashSet<String> = HashSet::new();
    if let Some(list) = members_of(ast.node(slot.container)) {
        for &m in ast.list(list) {
            let (mods, key) = match *ast.node(m) {
                Node::PropertyDef { modifiers, key, .. } => (modifiers, key),
                Node::MethodDef {
                    modifiers,
                    key,
                    kind,
                    ..
                } if kind != MethodKind::Constructor => (modifiers, key),
                _ => continue,
            };
            let s = ast.span(m);
            if s.start <= offset && offset <= s.end {
                continue;
            }
            if mods.contains(Modifiers::PRIVATE) || mods.contains(Modifiers::STATIC) != is_static {
                continue;
            }
            if let Some(name) = ts_r_checker::nodes::key_text(ast, key) {
                written.insert(name);
            }
        }
    }
    let mut out: Vec<(String, SymId)> = Vec::new();
    for b in bases {
        let a = ck.apparent_type(b);
        for (n, s) in ck.properties_of(a) {
            let name = ck.store().str(n).to_string();
            if name.starts_with("__@") || name.starts_with('#') || written.contains(&name) {
                continue;
            }
            let decls = c.decls(s);
            if decls.is_empty()
                || decls.iter().any(|d| {
                    c.ast(d.file)
                        .node(d.node)
                        .modifiers()
                        .contains(Modifiers::PRIVATE)
                })
            {
                continue;
            }
            if !out.iter().any(|(x, _)| *x == name) {
                out.push((name, s));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The class whose `this` the code at `l` sees, and whether it is the static side: inside a
/// method, accessor, constructor body, property initializer or static block (arrow
/// functions see their parent's `this`; other functions and object literal methods end the
/// search).
fn this_class(c: &Checker<'_>, l: Loc) -> Option<(NodeId, bool)> {
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut prev = l.node;
    let mut cur = Some(l.node);
    let mut is_static = None;
    for _ in 0..100_000 {
        let n = cur?;
        match *ast.node(n) {
            Node::FunctionDecl { .. } | Node::Module { .. } | Node::Program { .. } => {
                return None;
            }
            Node::FunctionExpr { .. } => {
                let p = parents.parent(n)?;
                let Node::MethodDef { modifiers, .. } = *ast.node(p) else {
                    return None;
                };
                let class = parents.parent(p)?;
                if !is_class(ast.node(class)) {
                    return None;
                }
                is_static = Some(modifiers.contains(Modifiers::STATIC));
            }
            Node::PropertyDef {
                modifiers, value, ..
            } => {
                // In the initializer (or the cursor in the property after its name).
                if n != l.node && value != Some(prev) {
                    return None;
                }
                is_static = Some(modifiers.contains(Modifiers::STATIC));
            }
            Node::StaticBlock { .. } => is_static = Some(true),
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                return is_static.map(|s| (n, s));
            }
            _ => {}
        }
        prev = n;
        cur = parents.parent(n);
    }
    None
}

/// The members of `this` inside a class (offered among the names in scope, inserted as
/// `this.name`).
pub fn this_items(ck: &mut Ck<'_, '_>, l: Loc) -> Vec<(String, SymId)> {
    let c = ck.c;
    let t = if let Some((class, is_static)) = this_class(c, l) {
        let Some(sym) = c.sym_of_decl(Loc::new(l.file, class)) else {
            return Vec::new();
        };
        if is_static {
            ck.type_of_symbol(sym)
        } else {
            ck.declared_type_of_symbol(sym)
        }
    } else if let Some(ann) = this_param_type(c, l) {
        ck.type_from_node(Loc::new(l.file, ann))
    } else {
        return Vec::new();
    };
    let a = ck.apparent_type(t);
    let mut out: Vec<(String, SymId)> = ck
        .properties_of(a)
        .into_iter()
        .map(|(n, s)| (ck.store().str(n).to_string(), s))
        .filter(|(n, _)| !n.starts_with("__@") && !n.starts_with('#'))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The type annotation of the `this` parameter of the function holding `l` (arrow functions
/// see through to the function around them).
fn this_param_type(c: &Checker<'_>, l: Loc) -> Option<NodeId> {
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut cur = Some(l.node);
    for _ in 0..100_000 {
        let n = cur?;
        match *ast.node(n) {
            Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => {
                let &first = ast.list(params).first()?;
                let Node::Param {
                    pattern, type_ann, ..
                } = *ast.node(first)
                else {
                    return None;
                };
                return match *ast.node(pattern) {
                    Node::Ident { name } if ast.atom(name) == "this" => type_ann,
                    _ => None,
                };
            }
            Node::Module { .. } | Node::Program { .. } | Node::ClassDecl { .. } => return None,
            _ => {}
        }
        cur = parents.parent(n);
    }
    None
}

/// The classes enclosing `at`, innermost first.
fn enclosing_classes(c: &Checker<'_>, at: Loc) -> Vec<NodeId> {
    let ast = c.ast(at.file);
    let parents = c.file(at.file).parents();
    let mut out = Vec::new();
    let mut cur = Some(at.node);
    for _ in 0..100_000 {
        let Some(n) = cur else { break };
        if is_class(ast.node(n)) {
            out.push(n);
        }
        cur = parents.parent(n);
    }
    out
}

/// Whether member `s` may be named at `at`: a `private` (or `#private`) member only inside
/// the class that declares it, a `protected` one inside that class or a class deriving from
/// it.
pub fn member_visible(ck: &mut Ck<'_, '_>, s: SymId, name: &str, at: Loc) -> bool {
    let c = ck.c;
    let decls = c.decls(s);
    for d in decls.iter() {
        let m = c.ast(d.file).node(d.node).modifiers();
        let private = m.contains(Modifiers::PRIVATE) || (name.starts_with('#') && name.len() > 1);
        let protected = m.contains(Modifiers::PROTECTED);
        if !private && !protected {
            continue;
        }
        // The declaring class (a parameter property's is the constructor's class).
        let mut owner = c.parent(*d);
        for _ in 0..4 {
            match owner {
                Some(o) if !is_class(c.ast(o.file).node(o.node)) => owner = c.parent(o),
                _ => break,
            }
        }
        let Some(owner) = owner.filter(|o| is_class(c.ast(o.file).node(o.node))) else {
            return false;
        };
        let classes = if at.file == owner.file || protected {
            enclosing_classes(c, at)
        } else {
            Vec::new()
        };
        if classes
            .iter()
            .any(|&k| at.file == owner.file && k == owner.node)
        {
            continue;
        }
        if private {
            return false;
        }
        let Some(owner_sym) = c.sym_of_decl(owner) else {
            return false;
        };
        // Outside the owner, a protected member is read through an instance of the
        // deriving class (`this`, `super` or a `D` in `D`), not of the base.
        let through = match c.ast(at.file).node(at.node) {
            _ if m.contains(Modifiers::STATIC) => None,
            Node::Super {} | Node::This {} | Node::ImportType { .. } => None,
            _ => {
                let t = ck.check_expr(at);
                Some(ck.class_symbol_of_instance(t))
            }
        };
        let derives = classes.iter().any(|&k| {
            c.sym_of_decl(Loc::new(at.file, k)).is_some_and(|ks| {
                derives_from(ck, ks, owner_sym, 0)
                    && match through {
                        None => true,
                        Some(o) => o.is_some_and(|o| derives_from(ck, o, ks, 0)),
                    }
            })
        });
        if !derives {
            return false;
        }
    }
    true
}

/// Whether class `sub` is `base` or derives from it.
fn derives_from(ck: &mut Ck<'_, '_>, sub: SymId, base: SymId, depth: u32) -> bool {
    if sub == base {
        return true;
    }
    if depth > 32 {
        return false;
    }
    let bases = ck.base_types(sub);
    bases.iter().any(|&b| {
        let s = ck.class_symbol_of_instance(b);
        s.is_some_and(|s| derives_from(ck, s, base, depth + 1))
    })
}
