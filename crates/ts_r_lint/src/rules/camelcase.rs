//! `camelcase`: names with an inner `_` (that aren't all upper case): declarations, their
//! references, undeclared and configured globals, assigned and defined properties,
//! re-exported names and labels.

use rustc_hash::FxHashSet;

use super::prelude::*;
use crate::regex;
use crate::semantic::{DeclKind, RefFlags, Resolution, SymbolFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "camelcase",
        "Enforce camelcase naming convention",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<Camelcase>,
};

#[derive(Debug, Default)]
struct Camelcase {
    allow: Vec<String>,
    ignore_destructuring: bool,
    ignore_globals: bool,
    ignore_imports: bool,
    properties_never: bool,
}

impl FromOptions for Camelcase {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(Camelcase {
            allow: o
                .get("allow")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            ignore_destructuring: bool_field(o, "ignoreDestructuring", false),
            ignore_globals: bool_field(o, "ignoreGlobals", false),
            ignore_imports: bool_field(o, "ignoreImports", false),
            properties_never: str_field(o, "properties") == Some("never"),
        })
    }
}

fn is_underscored(name: &str) -> bool {
    let body = name.trim_matches('_');
    body.contains('_') && body != body.to_uppercase()
}

/// Reports, one per start position.
struct Out {
    seen: FxHashSet<u32>,
    hits: Vec<(Span, String, bool)>,
}

impl Out {
    fn add(&mut self, cx: &Ctx<'_, '_>, id: NodeId, private: bool) {
        let span = cx.span(id);
        if !self.seen.insert(span.start) {
            return;
        }
        let name = match *cx.node(id) {
            Node::PrivateName { name } => cx.ast().atom(name).to_string(),
            _ => ident_name(cx.ast(), id).unwrap_or_default().to_string(),
        };
        self.hits.push((span, name, private));
    }
}

impl Camelcase {
    fn good(&self, name: &str) -> bool {
        !is_underscored(name)
            || self.allow.iter().any(|a| {
                a == name || regex::compile(a).is_some_and(|re| re.is_match_flags(name, "u"))
            })
    }

    /// A reference to report unless it's a call's callee or argument, a default value, or a
    /// destructured name kept as is (`ignoreDestructuring`).
    fn report_ref(&self, cx: &Ctx<'_, '_>, out: &mut Out, id: NodeId) {
        if let Some(p) = cx.parent(id) {
            match *cx.node(p) {
                Node::Call { .. } | Node::New { .. } => return,
                Node::AssignPattern { right, .. } if right == id => return,
                Node::BindingProperty { init: Some(i), .. } if i == id => return,
                Node::ShorthandProp { init: Some(i), .. } if i == id => return,
                Node::Param { init: Some(i), .. } if i == id => return,
                _ => {}
            }
        }
        if self.ignore_destructuring && equals_original_name(cx, id) {
            return;
        }
        if let Some(p) = cx.parent(id)
            && matches!(cx.node(p), Node::ShorthandProp { .. })
            && is_import_attr_key(cx, p)
        {
            return;
        }
        out.add(cx, id, false);
    }
}

/// `{ a_b } = x`, `{ a_b: a_b }`, `import { a_b }`: the local name repeats the key.
fn equals_original_name(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Some(name) = ident_name(ast, id) else {
        return false;
    };
    let Some(mut p) = cx.parent(id) else {
        return false;
    };
    let mut value = id;
    if let Node::AssignPattern { left, .. } = *cx.node(p)
        && left == id
    {
        value = p;
        let Some(pp) = cx.parent(p) else { return false };
        p = pp;
    }
    match *cx.node(p) {
        Node::BindingProperty { key, value: v, .. } => {
            (v.is_none() && key == id) || (v == Some(value) && ident_name(ast, key) == Some(name))
        }
        Node::ShorthandProp { name: n, .. } => n == id,
        Node::Property { key, value: v, .. } => {
            v == value
                && matches!(cx.node(key), Node::Ident { .. })
                && ident_name(ast, key) == Some(name)
        }
        Node::ImportSpecifier {
            imported, local, ..
        } => {
            local == id
                && match imported {
                    None => true,
                    Some(i) => match *cx.node(i) {
                        Node::StringLit { value } => ast.atom(value) == name,
                        _ => ident_name(ast, i) == Some(name),
                    },
                }
        }
        _ => false,
    }
}

/// `prop` (a non-computed `Property` or a `ShorthandProp`) is a key of the options of an
/// `import()`, at any depth of plain keys.
fn is_import_attr_key(cx: &Ctx<'_, '_>, mut prop: NodeId) -> bool {
    for _ in 0..10_000 {
        match *cx.node(prop) {
            Node::Property { key, .. }
                if matches!(
                    cx.node(key),
                    Node::Ident { .. } | Node::StringLit { .. } | Node::NumberLit { .. }
                ) => {}
            Node::ShorthandProp { .. } => {}
            _ => return false,
        }
        let Some(obj) = cx.parent(prop) else {
            return false;
        };
        if !matches!(cx.node(obj), Node::ObjectLit { .. }) {
            return false;
        }
        let Some(op) = cx.parent(obj) else {
            return false;
        };
        match *cx.node(op) {
            Node::ImportCall { options, .. } => return options == Some(obj),
            Node::Property { value, .. } if value == obj => prop = op,
            _ => return false,
        }
    }
    false
}

/// `node` (a member) is written by an assignment or destructuring.
fn is_assignment_target(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let Some(p) = cx.parent(node) else {
        return false;
    };
    match *cx.node(p) {
        Node::Assign { left, .. } | Node::AssignPattern { left, .. } => left == node,
        Node::Property { value, .. } => value == node && in_target(cx, p),
        Node::BindingProperty { value, .. } => value == Some(node),
        Node::ArrayPattern { .. } | Node::Rest { .. } => true,
        Node::ArrayLit { .. } | Node::Spread { .. } => in_target(cx, node),
        _ => false,
    }
}

/// `n` sits inside a destructuring assignment target (object or array literal on the left).
fn in_target(cx: &Ctx<'_, '_>, mut n: NodeId) -> bool {
    for _ in 0..10_000 {
        let Some(p) = cx.parent(n) else { return false };
        match *cx.node(p) {
            Node::Assign { left, .. } => return left == n,
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == n,
            Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::Property { .. }
            | Node::Spread { .. }
            | Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::Rest { .. }
            | Node::AssignPattern { .. } => n = p,
            _ => return false,
        }
    }
    false
}

/// `prop` belongs to an object literal that is an expression (not a destructuring target)
/// and isn't `import()` options.
fn in_object_expr(cx: &Ctx<'_, '_>, prop: NodeId) -> bool {
    cx.parent(prop)
        .is_some_and(|o| matches!(cx.node(o), Node::ObjectLit { .. }) && !in_target(cx, o))
        && !is_import_attr_key(cx, prop)
}

impl Rule for Camelcase {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let mut out = Out {
            seen: FxHashSet::default(),
            hits: Vec::new(),
        };
        // Globals: configured ones (unless `ignoreGlobals`) and undeclared ones.
        for r in sem.references() {
            if r.flags.contains(RefFlags::IMPLICIT) {
                continue;
            }
            let name = ast.atom(r.name);
            if self.good(name) {
                continue;
            }
            let configured = match r.resolution {
                Resolution::Global => true,
                Resolution::Symbol(s) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
                Resolution::Unresolved => false,
            };
            match r.resolution {
                Resolution::Unresolved => self.report_ref(cx, &mut out, r.node),
                _ if configured && !self.ignore_globals => self.report_ref(cx, &mut out, r.node),
                _ => {}
            }
        }
        // Declarations and their references.
        for s in sem.symbols() {
            let Some(d) = s.decls.first() else { continue };
            let import = matches!(
                d.kind,
                DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed
            );
            let declared = matches!(
                d.kind,
                DeclKind::Var
                    | DeclKind::Let
                    | DeclKind::Const
                    | DeclKind::Using
                    | DeclKind::Function
                    | DeclKind::FunctionExprName
                    | DeclKind::Class
                    | DeclKind::ClassExprName
                    | DeclKind::CatchParam
            ) || (d.kind == DeclKind::Param
                && !s.flags.contains(SymbolFlags::SIGNATURE_PARAM));
            if !(import || declared) || self.good(ast.atom(s.name)) {
                continue;
            }
            let ignore = if import {
                self.ignore_imports
            } else {
                self.ignore_destructuring
            };
            if !(ignore && equals_original_name(cx, d.name)) {
                out.add(cx, d.name, false);
            }
            for &r in &s.refs {
                let r = sem.reference(r);
                if (declared && r.flags.contains(RefFlags::INIT))
                    || r.flags.contains(RefFlags::IMPLICIT)
                {
                    continue;
                }
                self.report_ref(cx, &mut out, r.node);
            }
        }
        // Properties, re-exports and labels.
        let root = sem.scope(sem.global_scope()).node;
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            ast.for_each_child(n, |c| stack.push(c));
            match *cx.node(n) {
                Node::Property { key, .. }
                    if !self.properties_never
                        && matches!(cx.node(key), Node::Ident { .. })
                        && in_object_expr(cx, n) =>
                {
                    if ident_name(ast, key).is_some_and(|k| !self.good(k)) {
                        out.add(cx, key, false);
                    }
                }
                Node::ShorthandProp { name, .. }
                    if !self.properties_never && in_object_expr(cx, n) =>
                {
                    if ident_name(ast, name).is_some_and(|k| !self.good(k)) {
                        out.add(cx, name, false);
                    }
                }
                Node::MethodDef { key, modifiers, .. }
                | Node::PropertyDef { key, modifiers, .. }
                    if !self.properties_never
                        && !modifiers.intersects(Modifiers::ABSTRACT | Modifiers::ACCESSOR) =>
                {
                    let k = match *cx.node(key) {
                        Node::Ident { .. } => ident_name(ast, key),
                        Node::PrivateName { name } => Some(ast.atom(name)),
                        _ => None,
                    };
                    if k.is_some_and(|k| !self.good(k)) {
                        out.add(cx, key, matches!(cx.node(key), Node::PrivateName { .. }));
                    }
                }
                Node::Member { prop, .. }
                    if !self.properties_never
                        && matches!(cx.node(prop), Node::Ident { .. })
                        && is_assignment_target(cx, n) =>
                {
                    if ident_name(ast, prop).is_some_and(|k| !self.good(k)) {
                        out.add(cx, prop, false);
                    }
                }
                Node::ExportSpecifier {
                    local, exported, ..
                } => {
                    let e = exported.unwrap_or(local);
                    if matches!(cx.node(e), Node::Ident { .. })
                        && ident_name(ast, e).is_some_and(|k| !self.good(k))
                    {
                        out.add(cx, e, false);
                    }
                }
                Node::ExportAll {
                    exported: Some(e), ..
                } => {
                    if matches!(cx.node(e), Node::Ident { .. })
                        && ident_name(ast, e).is_some_and(|k| !self.good(k))
                    {
                        out.add(cx, e, false);
                    }
                }
                Node::Labeled { label, .. }
                | Node::Break { label: Some(label) }
                | Node::Continue { label: Some(label) } => {
                    if ident_name(ast, label).is_some_and(|k| !self.good(k)) {
                        out.add(cx, label, false);
                    }
                }
                _ => {}
            }
        }
        for (span, name, private) in out.hits {
            if private {
                cx.report(span, format!("#{name} is not in camel case."))
                    .id("notCamelCasePrivate");
            } else {
                cx.report(span, format!("Identifier '{name}' is not in camel case."))
                    .id("notCamelCase");
            }
        }
    }
}
