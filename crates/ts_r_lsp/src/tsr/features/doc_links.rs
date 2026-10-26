//! Names that JSDoc comments refer to: `{@link X}` (also `{@linkcode}` and
//! `{@linkplain}`), `@see X` or `@see {X}`, and `@throws {X}`. Find-all-references and
//! rename count each name of such a path, and both may start on one.
//!
//! A path is `A.B.C`, optionally followed by `#member` parts; after a `#`, a `.` ends the
//! path (`{@link N.C#P.q}` names `N.C#P`). Observed on tsc 7 with the fourslash tests
//! `findAllReferencesLinkTag*`, `findAllReferencesFromLinkTagReference*`,
//! `jsdocLink_findAllReferences1`, `jsDocSee_rename1` and `jsdocThrowsTag_*`:
//!
//! - the first name resolves where the comment is: at the node it documents (inside a
//!   function, its body's scope), else among the members of the enclosing class, interface
//!   or enum (`{@link m}` on a method names the sibling method `m`);
//! - each later name is an export of the one before, else a property of its declared
//!   (instance) type, of its type (statics), or of its instance side through `prototype`
//!   (`C.prototype.m`, `Outer.Inner.m` where `Inner` holds a class expression).

use ts_r_ast::{Ast, CommentKind, Node, NodeId};
use ts_r_binder::meaning;
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

/// Names followed in one path at most.
const MAX_SEGMENTS: usize = 16;
/// Enclosing nodes walked to find the class, interface or enum of a member's comment.
const MAX_CONTAINER_DEPTH: u32 = 8;

/// One name path in a doc comment.
pub(super) struct Link {
    /// The node the comment documents (the root when none).
    pub host: NodeId,
    /// The byte span of each name, in order.
    pub segs: Vec<(u32, u32)>,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn skip_blanks(b: &[u8], mut q: usize, end: usize) -> usize {
    while q < end && matches!(b[q], b' ' | b'\t') {
        q += 1;
    }
    q
}

/// The names of the path starting at `q` (none when no identifier starts there).
fn path(b: &[u8], mut q: usize, end: usize) -> Vec<(u32, u32)> {
    let mut segs = Vec::new();
    let mut hashed = false;
    while segs.len() < MAX_SEGMENTS {
        let st = q;
        while q < end && is_ident_byte(b[q]) {
            q += 1;
        }
        if q == st || b[st].is_ascii_digit() {
            break;
        }
        segs.push((st as u32, q as u32));
        match b.get(q) {
            Some(b'#') if q + 1 < end => hashed = true,
            Some(b'.') if !hashed && q + 1 < end => {}
            _ => break,
        }
        q += 1;
    }
    segs
}

/// Every name path in the doc comments of file `f`, in source order.
pub(super) fn links_in(c: &Checker<'_>, f: FileId) -> Vec<Link> {
    let ast = c.ast(f);
    let text = c.file(f).text.as_str();
    let b = text.as_bytes();
    let mut out = Vec::new();
    for cm in &ast.comments {
        if cm.kind != CommentKind::Block {
            continue;
        }
        let (s, e) = (cm.span.start as usize, cm.span.end as usize);
        if e > b.len() || e < s + 5 || !b[s..].starts_with(b"/**") {
            continue;
        }
        let body_end = e - 2;
        let mut host = None;
        let mut p = s + 3;
        // Each round moves past one `@`.
        while let Some(k) = memchr_at(b, p, body_end) {
            let at = k;
            p = at + 1;
            let mut ne = at + 1;
            while ne < body_end && is_ident_byte(b[ne]) {
                ne += 1;
            }
            let inline = b[at - 1] == b'{';
            let tag = &b[at + 1..ne];
            let start = match (inline, tag) {
                (true, b"link" | b"linkcode" | b"linkplain") => {
                    let q = skip_blanks(b, ne, body_end);
                    (q > ne).then_some(q)
                }
                (false, b"see") => {
                    let q = skip_blanks(b, ne, body_end);
                    if q < body_end && b[q] == b'{' {
                        Some(skip_blanks(b, q + 1, body_end))
                    } else {
                        (q > ne).then_some(q)
                    }
                }
                (false, b"throws" | b"exception") => {
                    let q = skip_blanks(b, ne, body_end);
                    (q < body_end && b[q] == b'{').then(|| skip_blanks(b, q + 1, body_end))
                }
                _ => None,
            };
            let Some(q) = start else { continue };
            let segs = path(b, q, body_end);
            if segs.is_empty() {
                continue;
            }
            p = segs.last().map_or(p, |&(_, e)| e as usize);
            let host =
                *host.get_or_insert_with(|| super::jsdoc_items::documented(ast, b, cm.span.end));
            out.push(Link { host, segs });
        }
    }
    out
}

/// The position of the next `@` in `b[from..end]`.
fn memchr_at(b: &[u8], from: usize, end: usize) -> Option<usize> {
    b.get(from..end)?
        .iter()
        .position(|&x| x == b'@')
        .map(|k| from + k)
}

/// The symbol each name of `link` refers to; `None` from the first name that resolves to
/// nothing on.
pub(super) fn resolve(ck: &mut Ck<'_, '_>, f: FileId, link: &Link) -> Vec<Option<SymId>> {
    let c = ck.c;
    let text = c.file(f).text.as_str();
    let mut out = Vec::with_capacity(link.segs.len());
    let mut cur: Option<SymId> = None;
    for (i, &(s, e)) in link.segs.iter().enumerate() {
        let name = &text[s as usize..e as usize];
        cur = if i == 0 {
            first(ck, Loc::new(f, link.host), name)
        } else {
            cur.and_then(|base| member(ck, base, name))
        };
        out.push(cur);
    }
    out
}

/// What the first name of a path at `host` refers to.
fn first(ck: &mut Ck<'_, '_>, host: Loc, name: &str) -> Option<SymId> {
    let c = ck.c;
    let ast = c.ast(host.file);
    let inner = match *ast.node(host.node) {
        // A property typed by a function type: that type's parameters are in scope.
        Node::PropertySignature {
            type_ann: Some(t), ..
        } => match *ast.node(t) {
            Node::FunctionType { params, .. } => ast.list(params).first().copied(),
            _ => None,
        },
        Node::MethodDef { value, .. } => ts_r_checker::nodes::fn_parts(ast, value)
            .and_then(|p| p.body.or_else(|| ast.list(p.params).first().copied())),
        _ => ts_r_checker::nodes::fn_parts(ast, host.node)
            .and_then(|p| p.body.or_else(|| ast.list(p.params).first().copied())),
    };
    // In an enum, its members come first (`{@link Foo}` on a member is the member `Foo`,
    // not an interface `Foo` outside).
    let container = container_of(c, ast, host);
    let owner = container.and_then(|d| c.sym_of_decl(d));
    if let (Some(d), Some(s)) = (container, owner)
        && matches!(ast.node(d.node), Node::Enum { .. })
        && let Some(x) = c.export_of(s, name)
    {
        return Some(x);
    }
    if let Some(s) = inner
        .and_then(|n| ck.resolve_name_at(Loc::new(host.file, n), name, meaning::ALL))
        .or_else(|| ck.resolve_name_at(host, name, meaning::ALL))
    {
        return Some(s);
    }
    member(ck, owner?, name)
}

/// The class, interface or enum whose member `host` is.
fn container_of(c: &Checker<'_>, ast: &Ast, host: Loc) -> Option<Loc> {
    let mut n = host;
    for _ in 0..MAX_CONTAINER_DEPTH {
        n = c.parent(n)?;
        match ast.node(n.node) {
            Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Interface { .. }
            | Node::Enum { .. } => return Some(n),
            _ => {}
        }
    }
    None
}

/// Name `name` after `base` in a path (see the module docs).
fn member(ck: &mut Ck<'_, '_>, base: SymId, name: &str) -> Option<SymId> {
    let c = ck.c;
    let base = c.resolve_alias(base).unwrap_or(base);
    if let Some(x) = c.export_of(base, name) {
        return Some(x);
    }
    let declared = ck.declared_type_of_symbol(base);
    if let Some(x) = ck.property_of_type(declared, name) {
        return Some(x);
    }
    let t = ck.type_of_symbol(base);
    if let Some(x) = ck.property_of_type(t, name) {
        return Some(x);
    }
    let proto = ck.property_of_type(t, "prototype")?;
    let it = ck.type_of_symbol(proto);
    ck.property_of_type(it, name)
}

/// The name under `offset` in a doc comment of file `f`, its span and the symbol it
/// refers to.
pub(super) fn at(ck: &mut Ck<'_, '_>, f: FileId, offset: u32) -> Option<(u32, u32, SymId)> {
    let c = ck.c;
    let ast = c.ast(f);
    let i = ast.comments.partition_point(|cm| cm.span.end <= offset);
    let cm = ast.comments.get(i)?;
    if cm.span.start > offset {
        return None;
    }
    for link in links_in(c, f) {
        let Some(k) = link
            .segs
            .iter()
            .position(|&(s, e)| s <= offset && offset <= e)
        else {
            continue;
        };
        let (s, e) = link.segs[k];
        let sym = resolve(ck, f, &link).get(k).copied().flatten()?;
        return Some((s, e, sym));
    }
    None
}
