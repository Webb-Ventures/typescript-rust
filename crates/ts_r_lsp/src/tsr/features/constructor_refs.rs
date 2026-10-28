//! Find-all-references on a class's `constructor` keyword (or a modifier before it).
//!
//! Rules observed on tsc 7's answers to `constructorFindAllReferences*`,
//! `findAllReferencesOfConstructor` and `findAllRefsOfConstructor*` (the constructor case
//! in `tsc: tsc/internal/ls/findallreferences.go` explains them):
//!
//! - The class's `constructor` keywords (every overload) are the declarations.
//! - The constructor builds its own class and every class that extends one it builds
//!   without declaring a constructor of its own (`class B extends A {}`), followed through
//!   any number of such classes.
//! - The references are the callee name of each `new` of one of those classes (`new C()`,
//!   `new a.C()`, through imports, but not through a variable holding the class), `this`
//!   in `new this()` inside their static members, and `super` in each `super(...)` call
//!   made by the constructor of a class that extends one of them. A `super(...)` in a
//!   method, or in a class that only `implements` one, is not a reference.

use super::Ctx;
use super::keywords::{self, Doc};
use super::locate::meaning_at;
use super::nav::Ref;
use std::collections::HashMap;
use ts_r_ast::{Ast, HeritageKind, MethodKind, Node, NodeId};
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::SymId;

/// Levels walked up from a node at most.
const MAX_UP: usize = 4096;

/// A class declaration in the project.
struct Class {
    at: Loc,
    sym: Option<SymId>,
    /// What its `extends` clause names, resolved through aliases.
    base: Option<SymId>,
    has_ctor: bool,
}

/// The references of the constructor whose key is `key` in the current file, or `None`
/// when `key` is not a constructor's.
pub fn constructor_references(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    files: &[FileId],
    key: NodeId,
) -> Option<Vec<Ref>> {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let parents = c.file(cx.file).parents();
    let m = parents.parent(key)?;
    let Node::MethodDef {
        kind: MethodKind::Constructor,
        key: k,
        ..
    } = *ast.node(m)
    else {
        return None;
    };
    if k != key {
        return None;
    }
    let class = parents.parent(m)?;
    let own = c.sym_of_decl(Loc::new(cx.file, class))?;

    // Every class, `new` and `super(...)` in the project.
    let mut classes: Vec<Class> = Vec::new();
    let mut news: Vec<(FileId, NodeId)> = Vec::new();
    let mut supers: Vec<(FileId, NodeId)> = Vec::new();
    for &f in files {
        let ast = c.ast(f);
        let mut v = Collect::default();
        ts_r_ast::visit::walk(ast, ast.root, &mut v);
        for n in v.classes {
            let base = match base_name(ast, n) {
                Some(b) => resolved(ck, Loc::new(f, b)),
                None => None,
            };
            classes.push(Class {
                at: Loc::new(f, n),
                sym: c.sym_of_decl(Loc::new(f, n)),
                base,
                has_ctor: has_constructor(ast, n),
            });
        }
        news.extend(v.news.into_iter().map(|n| (f, n)));
        supers.extend(v.supers.into_iter().map(|n| (f, n)));
    }

    // The classes this constructor builds.
    let mut built: Vec<SymId> = vec![own];
    let mut built_at: Vec<Loc> = vec![Loc::new(cx.file, class)];
    let mut added = true;
    let mut rounds = 0;
    while added && rounds <= classes.len() {
        added = false;
        rounds += 1;
        for k in &classes {
            if k.has_ctor || !k.base.is_some_and(|b| built.contains(&b)) {
                continue;
            }
            if let Some(s) = k.sym
                && !built.contains(&s)
            {
                built.push(s);
                built_at.push(k.at);
                added = true;
            }
        }
    }
    let base_of: HashMap<Loc, Option<SymId>> = classes.iter().map(|k| (k.at, k.base)).collect();

    let mut out: Vec<Ref> = Vec::new();
    let mut push = |file: FileId, start: u32, end: u32, is_decl: bool| {
        out.push(Ref {
            file,
            start,
            end,
            is_decl,
            is_write: false,
            shorthand: None,
        });
    };
    let d = Doc::new(c, cx.file);
    for (start, end) in keywords::constructors(&d, m) {
        push(cx.file, start, end, true);
    }
    for (f, n) in news {
        let ast = c.ast(f);
        let Node::New { callee, .. } = *ast.node(n) else {
            continue;
        };
        let name = match *ast.node(callee) {
            Node::Ident { .. } => callee,
            Node::Member { prop, .. } => prop,
            _ => continue,
        };
        if resolved(ck, Loc::new(f, name)).is_some_and(|s| built.contains(&s)) {
            let s = ast.span(name);
            push(f, s.start, s.end, false);
        }
    }
    for at in &built_at {
        let doc = Doc::new(c, at.file);
        let ast = c.ast(at.file);
        let parents = c.file(at.file).parents();
        for (f, start, end) in keywords::static_this_refs(&doc, at.node) {
            let Some(this) = super::locate::node_at(ast, start) else {
                continue;
            };
            let is_new_callee = parents.parent(this).is_some_and(
                |p| matches!(*ast.node(p), Node::New { callee, .. } if callee == this),
            );
            if is_new_callee {
                push(f, start, end, false);
            }
        }
    }
    for (f, n) in supers {
        let ast = c.ast(f);
        let Node::Call { callee, .. } = *ast.node(n) else {
            continue;
        };
        let Some(k) = constructor_class(ast, c.file(f).parents(), n) else {
            continue;
        };
        if base_of
            .get(&Loc::new(f, k))
            .copied()
            .flatten()
            .is_some_and(|b| built.contains(&b))
        {
            let s = ast.span(callee);
            push(f, s.start, s.end, false);
        }
    }
    out.sort_by_key(|r| (r.file.0, r.start));
    out.dedup_by_key(|r| (r.file.0, r.start));
    Some(out)
}

/// Classes, `new` expressions and `super(...)` calls of one file.
#[derive(Default)]
struct Collect {
    classes: Vec<NodeId>,
    news: Vec<NodeId>,
    supers: Vec<NodeId>,
}

impl ts_r_ast::visit::Visitor for Collect {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
        match *ast.node(id) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => self.classes.push(id),
            Node::New { .. } => self.news.push(id),
            Node::Call { callee, .. } if matches!(ast.node(callee), Node::Super {}) => {
                self.supers.push(id)
            }
            _ => {}
        }
        ts_r_ast::visit::Walk::Continue
    }
}

/// The symbol the name at `l` stands for, through import aliases.
fn resolved(ck: &mut Ck<'_, '_>, l: Loc) -> Option<SymId> {
    let s = meaning_at(ck, l).primary?;
    Some(ck.c.resolve_alias(s).unwrap_or(s))
}

/// The name a class's `extends` clause gives (the `C` of `extends C` or `extends a.C`).
fn base_name(ast: &Ast, class: NodeId) -> Option<NodeId> {
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return None;
    };
    let h = ast.list(heritage).iter().copied().find(|&h| {
        matches!(
            ast.node(h),
            Node::HeritageClause {
                kind: HeritageKind::Extends,
                ..
            }
        )
    })?;
    let Node::HeritageClause { types, .. } = *ast.node(h) else {
        return None;
    };
    let mut e = *ast.list(types).first()?;
    if let Node::ExprWithTypeArgs { expr, .. } = *ast.node(e) {
        e = expr;
    }
    match *ast.node(e) {
        Node::Ident { .. } => Some(e),
        Node::Member { prop, .. } => Some(prop),
        _ => None,
    }
}

/// Whether a class declares a constructor.
fn has_constructor(ast: &Ast, class: NodeId) -> bool {
    let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *ast.node(class)
    else {
        return false;
    };
    ast.list(members).iter().any(|&m| {
        matches!(
            ast.node(m),
            Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            }
        )
    })
}

/// The class whose constructor makes the `super(...)` call `call` (arrow functions in
/// between are fine; any other function or method is not).
fn constructor_class(
    ast: &Ast,
    parents: &ts_r_ast::visit::ParentMap,
    call: NodeId,
) -> Option<NodeId> {
    let mut cur = parents.parent(call);
    for _ in 0..MAX_UP {
        let n = cur?;
        match *ast.node(n) {
            Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            } => return parents.parent(n),
            Node::MethodDef { .. } | Node::FunctionDecl { .. } => return None,
            Node::FunctionExpr { .. } => {
                let p = parents.parent(n)?;
                if !matches!(ast.node(p), Node::MethodDef { .. }) {
                    return None;
                }
            }
            _ => {}
        }
        cur = parents.parent(n);
    }
    None
}
