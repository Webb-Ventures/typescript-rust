//! Declarations of JavaScript inputs (record 0012, E2): which files the declaration
//! emitter writes, and why it refuses the others (TSR2006).
//!
//! A JavaScript file's declarations are printed by the same emitter as TypeScript's: the
//! checker's tree carries the types its JSDoc desugared into (`@param`, `@returns`,
//! `@type`, `@this`, `@implements`, `@extends` become annotations, see
//! `ts_r_parser::attach_jsdoc`), so a file whose declarations are ordinary statements
//! prints like the TypeScript it stands for; `types.rs` writes JSDoc type syntax as tsc
//! does (`Emitter::jsdoc_type`). Beyond that the emitter writes the properties a class
//! declares with `this.x = v` ([`Emitter::this_properties`]) and a function's expando
//! namespace (`expando.rs`). tsc 7 also declares from other JavaScript patterns that the
//! emitter does not write yet, so a file using one is refused rather than written
//! without them (invariant 2; observed with tsc 7.0.2 on the emit baselines):
//!
//! - CommonJS (`module.exports = v`, `exports.x = v`: `export =` and exported declarations);
//! - `this["x"] = v` in a class, and `this.x = v` where the checker's type is not yet tsc's
//!   (a property a base class declares, a value that reads the property itself);
//! - `C.x = v` on a class (tsc adds a static);
//! - `@typedef`, `@callback`, `@template`, `@enum`, `@overload`, `@import`, `@private` /
//!   `@protected` on an assignment, `@this` without a type.
//!
//! What tsc 7 does not declare is written as it is: `this.x = v` in a function (tsc 7 has
//! no constructor functions) or with a computed key (`this[k]`), nested targets
//! (`C.m.x = v`), `this.constructor = v`.

use ts_r_ast::{Ast, JsDocTagKind, Modifiers, Node, NodeId, ParentMap, Span};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_checker::{Checker, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::TypeId;

/// Why the declarations of JavaScript file `file` are not written, with where: `None` when
/// the declaration emitter can write them.
pub fn unsupported(c: &Checker, file: FileId) -> Option<(Span, &'static str)> {
    let f = c.file(file);
    let ast = &f.ast;
    for t in &ast.jsdoc_tags {
        let why = match t.kind {
            JsDocTagKind::Typedef => "a JSDoc @typedef or @callback",
            JsDocTagKind::Template => "a JSDoc @template",
            JsDocTagKind::Enum => "a JSDoc @enum",
            JsDocTagKind::Overload => "a JSDoc @overload",
            JsDocTagKind::Private | JsDocTagKind::Protected => "a JSDoc modifier on an assignment",
            JsDocTagKind::This if t.ty.is_none() => "a JSDoc @this without a type",
            _ => continue,
        };
        return Some((t.span, why));
    }
    if !ast.jsdoc_namespaces.is_empty() {
        return Some((Span::EMPTY, "a JSDoc @typedef"));
    }
    let parents = f.parents();
    for raw in 1..=ast.node_count() as u32 {
        let Some(n) = NodeId::from_raw(raw) else {
            continue;
        };
        let why = match *ast.node(n) {
            Node::ImportDecl { .. } if ast.is_jsdoc(n) => Some("a JSDoc @import"),
            Node::TypeAlias { .. } if ast.is_jsdoc(n) => Some("a JSDoc @typedef or @callback"),
            Node::Assign { left, right, .. } => {
                assignment_target(c, file, ast, parents, left, right)
            }
            Node::Ident { name } if !ast.is_jsdoc(n) => {
                let text = ast.atom(name);
                // `module.exports` / `exports` read as the module object (no local of
                // that name): a CommonJS pattern tsc declares.
                ((text == "exports" || text == "module")
                    && c.resolve_ident(Loc { file, node: n }, meaning::VALUE)
                        .is_none()
                    && !is_member_name(ast, parents, n))
                .then_some("a CommonJS module")
            }
            _ => None,
        };
        if let Some(why) = why {
            return Some((ast.span(n), why));
        }
    }
    None
}

/// `n` is the property name of a member access (`a.exports`), not a reference.
fn is_member_name(ast: &Ast, parents: &ParentMap, n: NodeId) -> bool {
    parents
        .parent(n)
        .is_some_and(|p| matches!(*ast.node(p), Node::Member { prop, .. } if prop == n))
}

/// The name a property access or a literal element access assigns (`o.x`, `o["x"]`), with
/// the object. `None` for a computed key (`o[k]`), which declares nothing.
fn assigned_name(ast: &Ast, target: NodeId) -> Option<(NodeId, &str)> {
    match *ast.node(target) {
        Node::Member { obj, prop, .. } => Some((obj, ast.name_of(prop)?)),
        Node::Index { obj, index, .. } => match *ast.node(index) {
            Node::StringLit { value } => Some((obj, ast.atom(value))),
            Node::NumberLit { .. } => Some((obj, "")),
            _ => None,
        },
        _ => None,
    }
}

/// Why an assignment to `target` declares something tsc 7 writes and this emitter does not
/// (observed: tsc 7 declares a class's members from `this.x = v` in its constructor and
/// methods, and a class's statics from `C.x = v`; a function's `this.x = v` and nested
/// targets such as `C.m.x = v` declare nothing).
fn assignment_target(
    c: &Checker,
    file: FileId,
    ast: &Ast,
    parents: &ParentMap,
    target: NodeId,
    value: NodeId,
) -> Option<&'static str> {
    let (obj, name) = assigned_name(ast, target)?;
    let mut obj = obj;
    while let Node::Paren { expr } = *ast.node(obj) {
        obj = expr;
    }
    match *ast.node(obj) {
        Node::This {} => {
            let (class, is_static) = this_class(ast, parents, obj)?;
            if name == "constructor" || class_declares(ast, class, name, is_static) {
                return None;
            }
            // `this["x"] = v` is not written.
            if !matches!(ast.node(target), Node::Member { .. }) {
                return Some("a property assignment to `this` in a class");
            }
            // `this.x = v` is written by `Emitter::this_properties`, except where the
            // checker's type of `x` is not tsc's yet: a property a base class declares, a
            // property assigned from itself (`this.x = this.x.slice()`).
            if reads_this_property(ast, value, name) {
                return Some("a property assigned to `this` from itself");
            }
            if !is_static && base_declares(c, file, class, name) {
                return Some("a property assignment to `this` that a base class declares");
            }
            None
        }
        Node::Ident { .. } => {
            let sym = c.resolve_ident(Loc { file, node: obj }, meaning::VALUE)?;
            let flags = c.sym_flags(sym);
            (flags.intersects(SymbolFlags::CLASS) && !flags.intersects(SymbolFlags::FUNCTION))
                .then_some("a property assigned to a class")
        }
        _ => None,
    }
}

/// Expression `e` reads `this.<name>`.
fn reads_this_property(ast: &Ast, e: NodeId, name: &str) -> bool {
    let mut stack = vec![e];
    let mut budget = 100_000u32;
    while let Some(n) = stack.pop() {
        budget = budget.saturating_sub(1);
        if budget == 0 {
            return true;
        }
        if let Node::Member { obj, prop, .. } = *ast.node(n)
            && matches!(ast.node(obj), Node::This {})
            && ast.name_of(prop) == Some(name)
        {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

/// A base class of `class` has a property `name`.
fn base_declares(c: &Checker, file: FileId, class: NodeId, name: &str) -> bool {
    let Some(sym) = c.sym_of_decl(Loc { file, node: class }) else {
        return false;
    };
    let mut ck = ts_r_checker::Ck::new(c, file);
    ck.no_report += 1;
    let bases = ck.base_types(sym);
    bases
        .iter()
        .any(|&b| ck.property_of_type(b, name).is_some())
}

/// The class whose instance (or, in a static member, constructor) `this` at `n` is: the
/// class of the nearest enclosing non-arrow function when that is one of its members (or a
/// property initializer), with whether the member is static.
fn this_class(ast: &Ast, parents: &ParentMap, n: NodeId) -> Option<(NodeId, bool)> {
    let mut cur = n;
    for _ in 0..4096 {
        let p = parents.parent(cur)?;
        match *ast.node(p) {
            Node::FunctionDecl { .. } => return None,
            Node::FunctionExpr { .. } => {
                // A method's function: its parent is the member, whose parent is the class.
                let m = parents.parent(p)?;
                let Node::MethodDef { modifiers, .. } = *ast.node(m) else {
                    return None;
                };
                let k = parents.parent(m)?;
                return matches!(ast.node(k), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                    .then_some((k, modifiers.contains(Modifiers::STATIC)));
            }
            Node::PropertyDef { modifiers, .. } => {
                let k = parents.parent(p)?;
                return matches!(ast.node(k), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                    .then_some((k, modifiers.contains(Modifiers::STATIC)));
            }
            Node::StaticBlock { .. } => {
                let k = parents.parent(p)?;
                return matches!(ast.node(k), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                    .then_some((k, true));
            }
            _ => cur = p,
        }
    }
    None
}

/// Class `class` declares a member named `name` (a property, method or accessor) on the
/// side `is_static` says.
fn class_declares(ast: &Ast, class: NodeId, name: &str, is_static: bool) -> bool {
    let (Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. }) = *ast.node(class)
    else {
        return false;
    };
    ast.list(members).iter().any(|&m| {
        let (key, modifiers) = match *ast.node(m) {
            Node::PropertyDef { key, modifiers, .. } | Node::MethodDef { key, modifiers, .. } => {
                (key, modifiers)
            }
            _ => return false,
        };
        modifiers.contains(Modifiers::STATIC) == is_static && ast.name_of(key) == Some(name)
    })
}

/// One `this.x = v` a JavaScript class declares a property with: `(is static, name, the
/// assignment's statement, its target)`.
type ThisProperty = (bool, String, NodeId, NodeId);

/// The `this.x = v` assignments (`x` an identifier) in the constructor, methods, accessors
/// and static blocks of `members` (arrow functions included, nested functions and classes
/// not), first assignment of each name per side, in source order: the binder's this
/// properties of a JavaScript class.
fn this_assignments(ast: &Ast, parents: &ParentMap, members: &[NodeId]) -> Vec<ThisProperty> {
    let mut out: Vec<ThisProperty> = Vec::new();
    let mut stack: Vec<NodeId> = Vec::new();
    for &m in members {
        let (is_static, body) = match *ast.node(m) {
            Node::MethodDef {
                modifiers, value, ..
            } => match *ast.node(value) {
                Node::FunctionExpr { body: Some(b), .. } => {
                    (modifiers.contains(Modifiers::STATIC), b)
                }
                _ => continue,
            },
            Node::StaticBlock { .. } => (true, m),
            _ => continue,
        };
        stack.clear();
        stack.push(body);
        while let Some(n) = stack.pop() {
            match *ast.node(n) {
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => continue,
                Node::Assign {
                    op: ts_r_ast::AssignOp::Assign,
                    left,
                    ..
                } => {
                    if let Node::Member { obj, prop, .. } = *ast.node(left)
                        && matches!(ast.node(obj), Node::This {})
                        && let Node::Ident { name } = *ast.node(prop)
                    {
                        let name = ast.atom(name).to_owned();
                        if !out.iter().any(|o| o.0 == is_static && o.1 == name) {
                            let stmt = parents
                                .parent(n)
                                .filter(|&p| matches!(ast.node(p), Node::ExprStmt { .. }))
                                .unwrap_or(n);
                            out.push((is_static, name, stmt, left));
                        }
                    }
                }
                _ => {}
            }
            let at = stack.len();
            ast.for_each_child(n, |c| stack.push(c));
            stack[at..].reverse();
        }
    }
    out
}

impl super::Emitter<'_, '_> {
    /// The properties a JavaScript class declares with `this.x = v` (observed in tsc 7:
    /// written first in the class body, in the order of their first assignments, with the
    /// JSDoc of that assignment's statement, `static` for assignments in static members;
    /// none for a name the class or a base class declares, or `constructor`).
    pub(super) fn this_properties(&mut self, class: NodeId, members: &[NodeId]) {
        let props = this_assignments(&self.ast, self.parents, members);
        if props.is_empty() {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(self.loc(class)) else {
            return;
        };
        let instance = self.ck.declared_type_of_symbol(sym);
        let constructor = self.ck.type_of_symbol(sym);
        for (is_static, name, stmt, target) in props {
            if name == "constructor" || class_declares(&self.ast, class, &name, is_static) {
                continue;
            }
            let owner = if is_static { constructor } else { instance };
            let Some(prop) = self.ck.property_of_type(owner, &name) else {
                continue;
            };
            // The statement's own JSDoc, on a line of its own.
            let prev = self.previous_statement_end(stmt);
            self.last_written_end = None;
            self.leading_comments(stmt, prev);
            if is_static {
                self.w("static ");
            }
            self.w(&name);
            self.w(": ");
            let mut t = self.ck.type_of_symbol(prop);
            // Without strictNullChecks `null` and `undefined` widen to `any` (observed:
            // `this.field = null` is `field: any`).
            if !self.c.opts.strict_null_checks && matches!(t, TypeId::NULL | TypeId::UNDEFINED) {
                t = TypeId::ANY;
            }
            // A static one is never assigned in the constructor: it may be unset
            // (`| undefined` under strictNullChecks, as the checker does for instance
            // properties assigned only in methods; observed in tsc 7).
            if is_static && self.c.opts.strict_null_checks {
                t = self.ck.add_undefined(t);
            }
            self.inferred_type(t, target);
            self.w(";");
            self.nl();
        }
    }

    /// Where the comments before statement `s` start: the end of the statement before it
    /// in its block, else the block's start.
    fn previous_statement_end(&self, s: NodeId) -> u32 {
        let Some(block) = self.parents.parent(s) else {
            return self.ast.span(s).start;
        };
        if let Node::Block { stmts } = *self.ast.node(block) {
            let list = self.ast.list(stmts);
            if let Some(i) = list.iter().position(|&x| x == s)
                && i > 0
            {
                return self.ast.span(list[i - 1]).end;
            }
        }
        self.ast.span(block).start
    }
}
