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
//! - CommonJS (`exports.x = v` mixed with `module.exports = v`, nested or `require`d
//!   exports, a destructured `require`, more than one `module.exports = v`): exported
//!   declarations; one top-level `module.exports = v;` ([`commonjs_export_assignment`]),
//!   top-level `exports.x = v;` alone ([`commonjs_named_exports`]) and `const x =
//!   require("m")` (`import x = require("m")`) are written;
//! - `this["x"] = v` in a class, and `this.x = v` where the checker's type is not yet tsc's
//!   (a property a base class declares, a value that reads the property itself);
//! - `C.x = v` on a class (tsc adds a static);
//! - `@enum`, `@overload`, `@private` / `@protected` on an assignment, `@this` without a
//!   type, `@type` on a function declaration, a dotted `@typedef` name (a namespace), a
//!   `@callback` rest parameter.
//!
//! `@typedef`, `@callback`, `@template` and `@import` are written: the aliases and imports
//! JSDoc desugared into are statements of the tree, exported in a module, and print no
//! comments of their own (the comment holding them stays the comment of the statement it
//! documents; one in a class body is hoisted before the class). A `@property` description
//! becomes the member's JSDoc (`Emitter::property_tag_description`); a name with a `-` is a
//! quoted key (`"data-test-name"`), a `@param` one has `_` for `-`.
//!
//! What tsc 7 does not declare is written as it is: `this.x = v` in a function (tsc 7 has
//! no constructor functions) or with a computed key (`this[k]`), nested targets
//! (`C.m.x = v`), `this.constructor = v`.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, Atom, JsDocTagKind, Modifiers, Node, NodeId, ParentMap, Span};
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
            JsDocTagKind::Enum => "a JSDoc @enum",
            JsDocTagKind::Overload => "a JSDoc @overload",
            JsDocTagKind::Private | JsDocTagKind::Protected => "a JSDoc modifier on an assignment",
            JsDocTagKind::This if t.ty.is_none() => "a JSDoc @this without a type",
            // tsc takes the function's signature from the type (its type parameters too).
            JsDocTagKind::Type if matches!(ast.node(t.host), Node::FunctionDecl { .. }) => {
                "a JSDoc @type on a function declaration"
            }
            _ => continue,
        };
        return Some((t.span, why));
    }
    // `@typedef {T} ns.Name` declares a namespace.
    if !ast.jsdoc_namespaces.is_empty() {
        return Some((Span::EMPTY, "a JSDoc @typedef with a dotted name"));
    }
    let parents = f.parents();
    // `module.exports = v` alone is written (`export = v`); its `module` is no refusal.
    let export_assignment = commonjs_export_assignment(c, file);
    let allowed_module = export_assignment.and_then(|(stmt, _)| match *ast.node(stmt) {
        Node::ExprStmt { expr } => match *ast.node(expr) {
            Node::Assign { left, .. } => match *ast.node(left) {
                Node::Member { obj, .. } => Some(obj),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    });
    // Named exports alone (`exports.x = v`) are written.
    let named = export_assignment.is_none() && commonjs_named_exports(c, file).is_some();
    // Per class, the member names it declares (`(is static, name)`), built on first use.
    let mut members: FxHashMap<NodeId, FxHashSet<(bool, &str)>> = FxHashMap::default();
    for raw in 1..=ast.node_count() as u32 {
        let Some(n) = NodeId::from_raw(raw) else {
            continue;
        };
        let why = match *ast.node(n) {
            // `@callback` with `@param {...T} a`: tsc writes `...a: T` (no array).
            Node::TypeAlias { ty, .. } if ast.is_jsdoc(n) && callback_has_rest(ast, ty) => {
                Some("a JSDoc @callback with a rest parameter")
            }
            Node::Assign { left, right, .. } => {
                assignment_target(c, file, ast, parents, &mut members, left, right)
            }
            Node::Ident { name } if !ast.is_jsdoc(n) => {
                let text = ast.atom(name);
                // `module.exports` / `exports` read as the module object (no local of
                // that name): a CommonJS pattern tsc declares.
                ((text == "exports" || text == "module")
                    && !named
                    && allowed_module != Some(n)
                    && !is_property_name(ast, parents, n)
                    && is_module_object(c, file, n))
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

/// The file's one CommonJS export when it is a top-level `module.exports = v;` statement
/// and the file reads `module`/`exports` nowhere else: `(statement, v)`. tsc 7 writes it
/// as `export = v;` first in the file (an entity name; any other value through a
/// `declare const _exports: T;`), then the declarations as in a module with `export =`.
pub(super) fn commonjs_export_assignment(c: &Checker, file: FileId) -> Option<(NodeId, NodeId)> {
    let f = c.file(file);
    // A package's file under `node_modules` stays refused: tsc 7 emits nothing for one
    // that is also imported through the package (namespaceAssignmentToRequireAlias).
    if !f.bound.commonjs || f.path.contains("/node_modules/") {
        return None;
    }
    let ast = &f.ast;
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return None;
    };
    let is_name = |n: NodeId, text: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == text);
    let mut found = None;
    for &st in ast.list(stmts) {
        let Node::ExprStmt { expr } = *ast.node(st) else {
            continue;
        };
        let Node::Assign {
            op: ts_r_ast::AssignOp::Assign,
            left,
            right,
        } = *ast.node(expr)
        else {
            continue;
        };
        let Node::Member { obj, prop, .. } = *ast.node(left) else {
            continue;
        };
        if is_name(obj, "module") && is_name(prop, "exports") {
            if found.is_some() {
                return None;
            }
            found = Some((st, right));
        }
    }
    let (st, value) = found?;
    // No other `module` / `exports` reference anywhere (`module.exports.x = v`, a second
    // assignment in a function, `exports.x`).
    let parents = f.parents();
    let module = match *ast.node(st) {
        Node::ExprStmt { expr } => match *ast.node(expr) {
            Node::Assign { left, .. } => match *ast.node(left) {
                Node::Member { obj, .. } => obj,
                _ => return None,
            },
            _ => return None,
        },
        _ => return None,
    };
    for raw in 1..=ast.node_count() as u32 {
        let Some(n) = NodeId::from_raw(raw) else {
            continue;
        };
        if n == module || ast.is_jsdoc(n) {
            continue;
        }
        // `const x = require("m")` is written `import x = require("m")`; the emitter
        // refuses a printed destructured `require` (`Emitter::var_statement`).
        if let Node::Ident { name } = *ast.node(n) {
            let text = ast.atom(name);
            if (text == "exports" || text == "module")
                && !is_property_name(ast, parents, n)
                && is_module_object(c, file, n)
            {
                return None;
            }
        }
    }
    Some((st, value))
}

/// One CommonJS named export: `(name, its first assignment target, the assigned name)`.
/// The name is set when the export is assigned once, to a name declared in the file.
pub(super) type NamedExport = (String, NodeId, Option<NodeId>);

/// The file's CommonJS exports when every `exports` / `module` reference is a top-level
/// `exports.x = v;` / `module.exports["x"] = v;` (chains included) or a read of an
/// assigned `x`, and nothing is `require`d: the exports in order of first assignment
/// (pre-order). tsc 7 writes them first in the file: `export { v };` (`export { v as x };`)
/// for an export assigned once to a name declared in the file, `declare const _default:
/// T; export default _default;` for `default`, else `export declare var x: T;` with the
/// export's type.
pub(super) fn commonjs_named_exports(c: &Checker, file: FileId) -> Option<Vec<NamedExport>> {
    let f = c.file(file);
    if !f.bound.commonjs || f.path.contains("/node_modules/") {
        return None;
    }
    let ast = &f.ast;
    let parents = f.parents();
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return None;
    };
    let is_name = |n: NodeId, text: &str| matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == text);
    let string_is = |n: NodeId, text: &str| matches!(*ast.node(n), Node::StringLit { value } if ast.atom(value) == text);
    // `module.exports` / `module["exports"]`.
    let module_exports = |n: NodeId| match *ast.node(n) {
        Node::Member { obj, prop, .. } => is_name(obj, "module") && is_name(prop, "exports"),
        Node::Index { obj, index, .. } => is_name(obj, "module") && string_is(index, "exports"),
        _ => false,
    };
    // `exports.x`, `module.exports["x"]`: the name.
    let export_name = |n: NodeId| -> Option<&str> {
        let (obj, name) = match *ast.node(n) {
            Node::Member { obj, prop, .. } => (obj, ast.name_of(prop)?),
            Node::Index { obj, index, .. } => match *ast.node(index) {
                Node::StringLit { value } => (obj, ast.atom(value)),
                _ => return None,
            },
            _ => return None,
        };
        (is_name(obj, "exports") || module_exports(obj)).then_some(name)
    };
    let mut assigned: Vec<(&str, NodeId, NodeId)> = Vec::new();
    for &st in ast.list(stmts) {
        let Node::ExprStmt { expr } = *ast.node(st) else {
            continue;
        };
        let mut e = expr;
        while let Node::Assign {
            op: ts_r_ast::AssignOp::Assign,
            left,
            right,
        } = *ast.node(e)
        {
            if let Some(name) = export_name(left) {
                assigned.push((name, left, right));
            }
            e = right;
        }
    }
    if assigned.is_empty() {
        return None;
    }
    for raw in 1..=ast.node_count() as u32 {
        let Some(n) = NodeId::from_raw(raw) else {
            continue;
        };
        if ast.is_jsdoc(n) {
            continue;
        }
        if let Node::Call { callee, .. } = *ast.node(n)
            && is_name(callee, "require")
        {
            return None;
        }
        let Node::Ident { name } = *ast.node(n) else {
            continue;
        };
        let text = ast.atom(name);
        if !(text == "exports" || text == "module")
            || is_property_name(ast, parents, n)
            || !is_module_object(c, file, n)
        {
            continue;
        }
        // `exports` declared ambiently (@types/node): not seen written.
        if c.resolve_ident(Loc { file, node: n }, meaning::VALUE)
            .is_some()
        {
            return None;
        }
        let mut access = parents.parent(n)?;
        if text == "module" {
            if !module_exports(access) {
                return None;
            }
            access = parents.parent(access)?;
        }
        let name = export_name(access)?;
        if !assigned.iter().any(|&(a, t, _)| t == access || a == name) {
            return None;
        }
    }
    let mut out: Vec<NamedExport> = Vec::new();
    for &(name, target, value) in &assigned {
        if out.iter().any(|e| e.0 == name) {
            continue;
        }
        let once = assigned.iter().filter(|a| a.0 == name).count() == 1;
        let value = super::skip_parens(ast, value);
        let local = matches!(ast.node(value), Node::Ident { .. })
            && c.resolve_ident(Loc { file, node: value }, meaning::VALUE)
                .is_some_and(|s| c.decls(s).iter().any(|d| d.file == file));
        let alias = (once && local).then_some(value);
        // Not seen written: `exports.default = v` of a name, and names that are not
        // identifiers through a constant.
        let identifier = ts_r_checker::typenode::is_identifier_text(name);
        if (name == "default" && alias.is_some())
            || (!identifier && name != "default" && alias.is_none())
            || c.sym_of_decl(Loc { file, node: target }).is_none()
        {
            return None;
        }
        out.push((name.to_owned(), target, alias));
    }
    Some(out)
}

/// `ty` (a `@callback`'s function type) has a rest parameter.
fn callback_has_rest(ast: &Ast, ty: NodeId) -> bool {
    let Node::FunctionType { params, .. } = *ast.node(ty) else {
        return false;
    };
    ast.list(params)
        .iter()
        .any(|&p| matches!(ast.node(p), Node::Param { rest: true, .. }))
}

/// Identifier `n` is a name rather than a reference: a member access's property
/// (`a.exports`), a property, method or member key (`{ module: 1 }`, `module() {}`), a
/// renamed binding's key (`{ module: m }`), a label, a specifier's foreign name, an enum
/// member, an import attribute key, a JSX attribute or a qualified name's right side.
/// Shorthand properties and computed keys (`{ module }`, `[exports.k]`) are references.
fn is_property_name(ast: &Ast, parents: &ParentMap, n: NodeId) -> bool {
    let Some(p) = parents.parent(n) else {
        return false;
    };
    match *ast.node(p) {
        Node::Member { prop, .. } => prop == n,
        Node::Property { key, .. }
        | Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. }
        | Node::ImportAttribute { key, .. } => key == n,
        Node::BindingProperty {
            key,
            value: Some(_),
            ..
        } => key == n,
        Node::Labeled { label, .. } => label == n,
        Node::Break { label } | Node::Continue { label } => label == Some(n),
        Node::ImportSpecifier { imported, .. } => imported == Some(n),
        Node::ExportSpecifier { exported, .. } => exported == Some(n),
        Node::EnumMember { name, .. } | Node::JsxAttr { name, .. } => name == n,
        Node::JsxNamespacedName { .. } => true,
        Node::QualifiedName { right, .. } => right == n,
        _ => false,
    }
}

/// Identifier `n` (`module` or `exports`) is the CommonJS module object: it resolves to
/// nothing, or only to a global variable or function declared ambiently in another file
/// (`declare var module: NodeModule`, as @types/node declares it). A declaration in this
/// file, or an import, is an ordinary local. The checker's rule for `require` and the
/// module object (`is_commonjs_global`), restated with its public API.
fn is_module_object(c: &Checker, file: FileId, n: NodeId) -> bool {
    let Some(s) = c.resolve_ident(Loc { file, node: n }, meaning::VALUE) else {
        return true;
    };
    let f = c.sym_flags(s);
    if !f.intersects(SymbolFlags::FUNCTION | SymbolFlags::VARIABLE)
        || f.intersects(SymbolFlags::ALIAS)
    {
        return false;
    }
    let Some(&d) = c.decls(s).first() else {
        return false;
    };
    d.file != file && ts_r_checker::Ck::new(c, file).is_ambient_node(d)
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
fn assignment_target<'a>(
    c: &Checker,
    file: FileId,
    ast: &'a Ast,
    parents: &ParentMap,
    members: &mut FxHashMap<NodeId, FxHashSet<(bool, &'a str)>>,
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
            let declared = members
                .entry(class)
                .or_insert_with(|| member_names(ast, class));
            if name == "constructor" || declared.contains(&(is_static, name)) {
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

/// The members class `class` declares (properties, methods, accessors): `(is static, key)`.
fn member_keys(ast: &Ast, class: NodeId) -> impl Iterator<Item = (bool, NodeId)> + '_ {
    let members = match *ast.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
        _ => &[],
    };
    members.iter().filter_map(|&m| match *ast.node(m) {
        Node::PropertyDef { key, modifiers, .. } | Node::MethodDef { key, modifiers, .. } => {
            Some((modifiers.contains(Modifiers::STATIC), key))
        }
        _ => None,
    })
}

/// The names class `class` declares as members, per side: `(is static, name)`.
fn member_names(ast: &Ast, class: NodeId) -> FxHashSet<(bool, &str)> {
    member_keys(ast, class)
        .filter_map(|(is_static, key)| Some((is_static, ast.name_of(key)?)))
        .collect()
}

/// One `this.x = v` a JavaScript class declares a property with: `(is static, name, the
/// assignment's statement, its target, where the comments before that statement start)`.
type ThisProperty = (bool, Atom, NodeId, NodeId, u32);

/// The `this.x = v` assignments (`x` an identifier) in the constructor, methods, accessors
/// and static blocks of `members` (arrow functions included, nested functions and classes
/// not), first assignment of each name per side, in source order: the binder's this
/// properties of a JavaScript class. One pass: names seen are kept in a set, and the start
/// of each statement's leading comments (the end of the statement before it in its block,
/// else the block's start) is noted while its block is visited.
fn this_assignments(ast: &Ast, parents: &ParentMap, members: &[NodeId]) -> Vec<ThisProperty> {
    let mut out: Vec<ThisProperty> = Vec::new();
    let mut seen: FxHashSet<(bool, Atom)> = FxHashSet::default();
    let mut comments_from: FxHashMap<NodeId, u32> = FxHashMap::default();
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
                Node::Block { stmts } => {
                    let mut prev = ast.span(n).start;
                    for &st in ast.list(stmts) {
                        comments_from.insert(st, prev);
                        prev = ast.span(st).end;
                    }
                }
                Node::Assign {
                    op: ts_r_ast::AssignOp::Assign,
                    left,
                    ..
                } => {
                    if let Node::Member { obj, prop, .. } = *ast.node(left)
                        && matches!(ast.node(obj), Node::This {})
                        && let Node::Ident { name } = *ast.node(prop)
                        && seen.insert((is_static, name))
                    {
                        let parent = parents.parent(n);
                        let stmt = parent
                            .filter(|&p| matches!(ast.node(p), Node::ExprStmt { .. }))
                            .unwrap_or(n);
                        let from = comments_from.get(&stmt).copied().unwrap_or_else(|| {
                            parents
                                .parent(stmt)
                                .map_or(ast.span(stmt).start, |p| ast.span(p).start)
                        });
                        out.push((is_static, name, stmt, left, from));
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

/// The identifier and string names class `class` declares as members, per side, as atoms.
fn member_atoms(ast: &Ast, class: NodeId) -> FxHashSet<(bool, Atom)> {
    member_keys(ast, class)
        .filter_map(|(is_static, key)| match *ast.node(key) {
            Node::Ident { name } => Some((is_static, name)),
            Node::StringLit { value } => Some((is_static, value)),
            _ => None,
        })
        .collect()
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
        let declared = member_atoms(&self.ast, class);
        for (is_static, atom, stmt, target, comments_from) in props {
            let name = self.ast.atom(atom).to_owned();
            if name == "constructor" || declared.contains(&(is_static, atom)) {
                continue;
            }
            let owner = if is_static { constructor } else { instance };
            let Some(prop) = self.ck.property_of_type(owner, &name) else {
                continue;
            };
            // The statement's own JSDoc, on a line of its own.
            self.last_written_end = None;
            self.leading_comments(stmt, comments_from);
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
}
