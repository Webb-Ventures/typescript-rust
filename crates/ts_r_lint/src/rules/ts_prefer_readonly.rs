//! `@typescript-eslint/prefer-readonly`: private members (and private parameter properties)
//! never assigned outside the constructor should be `readonly`.

use std::collections::BTreeSet;

use super::prelude::*;
use crate::semantic::DeclKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-readonly",
        "Require private members to be marked as `readonly` if they're never modified outside of the constructor",
        NO_PRESET,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<PreferReadonly>,
};

#[derive(Debug)]
struct PreferReadonly {
    only_inline_lambdas: bool,
}

impl FromOptions for PreferReadonly {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(PreferReadonly {
            only_inline_lambdas: bool_field(obj_opt(opts, 0), "onlyInlineLambdas", false),
        })
    }
}

/// How the object a member is accessed on relates to the class.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    None,
    Class,
    Instance,
    Both,
}

/// One class's members and the writes seen in it (nested classes have their own).
struct Scope {
    class_sym: Option<Sym>,
    class_name: Option<String>,
    /// `Some(0)` directly in the constructor, deeper inside functions there.
    ctor_depth: Option<u32>,
    modified: BTreeSet<String>,
    ctor_modified: BTreeSet<String>,
    static_modified: BTreeSet<String>,
    /// Candidates as `(name, declaration)`, a later declaration of a name replacing it in place.
    members: Vec<(String, NodeId)>,
    statics: Vec<(String, NodeId)>,
}

/// The name a member declaration is tracked by.
fn member_name(ast: &ts_r_ast::Ast, src: &str, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { .. }
        | Node::PrivateName { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. } => key_name(ast, src, key).map(|c| c.into_owned()),
        Node::TemplateLit { .. } => static_value_string(ast, src, key).map(|c| c.into_owned()),
        Node::ComputedKey { expr } => match *ast.node(expr) {
            Node::NumberLit { .. } => key_name(ast, src, expr).map(|c| c.into_owned()),
            Node::Member { obj, .. } if ident_name(ast, obj) == Some("Symbol") => {
                Some(ast.span(expr).text(src).to_owned())
            }
            _ => None,
        },
        _ => None,
    }
}

impl PreferReadonly {
    fn declare(&self, cx: &Ctx<'_, '_>, scope: &mut Scope, decl: NodeId) {
        let ast = cx.ast();
        let (modifiers, key, value, is_static) = match *ast.node(decl) {
            Node::PropertyDef {
                modifiers,
                key,
                value,
                ..
            } => (modifiers, key, value, modifiers.contains(Modifiers::STATIC)),
            Node::Param {
                modifiers, pattern, ..
            } => (modifiers, pattern, None, false),
            _ => return,
        };
        let private = modifiers.contains(Modifiers::PRIVATE)
            || matches!(ast.node(key), Node::PrivateName { .. });
        if !private || modifiers.intersects(Modifiers::ACCESSOR | Modifiers::READONLY) {
            return;
        }
        if self.only_inline_lambdas
            && value.is_some_and(|v| !matches!(ast.node(v), Node::Arrow { .. }))
        {
            return;
        }
        let Some(name) = member_name(ast, cx.src(), key) else {
            return;
        };
        let list = if is_static {
            &mut scope.statics
        } else {
            &mut scope.members
        };
        match list.iter_mut().find(|(n, _)| *n == name) {
            Some(e) => e.1 = decl,
            None => list.push((name, decl)),
        }
    }

    /// The class (or one of its bases) is the type's symbol.
    fn is_class_like(cx: &Ctx<'_, '_>, scope: &Scope, t: Ty) -> bool {
        let ti = cx.types();
        let mut todo = vec![t];
        let mut seen = 0;
        while let Some(t) = todo.pop() {
            seen += 1;
            if seen > 64 {
                return false;
            }
            let target = ti.reference_target(t).unwrap_or(t);
            if let Some(s) = ti.type_symbol(target).or_else(|| ti.type_symbol(t))
                && (Some(s) == scope.class_sym
                    || scope.class_name.as_deref() == Some(ti.symbol_name(s).as_str()))
            {
                return true;
            }
            todo.extend(ti.base_types(target));
        }
        false
    }

    fn relation(cx: &Ctx<'_, '_>, scope: &Scope, t: Ty, depth: u32) -> Relation {
        let ti = cx.types();
        if depth > 16 {
            return Relation::None;
        }
        let f = ti.flags(t);
        if f.contains(TypeFlags::INTERSECTION) {
            let mut out = Relation::None;
            for m in ti.intersection_members(t) {
                match (Self::relation(cx, scope, m, depth + 1), out) {
                    (Relation::Class, Relation::Instance)
                    | (Relation::Instance, Relation::Class) => {
                        return Relation::Both;
                    }
                    (r @ (Relation::Class | Relation::Instance), _) => out = r,
                    _ => {}
                }
            }
            return out;
        }
        if f.contains(TypeFlags::UNION) {
            return match ti.union_members(t).first() {
                Some(&m) => Self::relation(cx, scope, m, depth + 1),
                None => Relation::None,
            };
        }
        if ti.type_symbol(t).is_none() || !Self::is_class_like(cx, scope, t) {
            return Relation::None;
        }
        if ti.object_flags(t).contains(ObjectFlags::ANONYMOUS) {
            Relation::Class
        } else {
            Relation::Instance
        }
    }

    fn modify(cx: &Ctx<'_, '_>, scope: &mut Scope, obj: NodeId, name: String) {
        let Some(t) = cx.types().type_at(obj) else {
            return;
        };
        let rel = Self::relation(cx, scope, t, 0);
        if rel == Relation::Instance && scope.ctor_depth == Some(0) {
            scope.ctor_modified.insert(name);
            return;
        }
        if matches!(rel, Relation::Instance | Relation::Both) {
            scope.modified.insert(name.clone());
        }
        if matches!(rel, Relation::Class | Relation::Both) {
            scope.static_modified.insert(name);
        }
    }

    fn walk(&self, cx: &Ctx<'_, '_>, scope: &mut Scope, node: NodeId, depth: u32) {
        if depth > 4096 {
            return;
        }
        let ast = cx.ast();
        match *ast.node(node) {
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => return,
            Node::MethodDef {
                kind: MethodKind::Constructor,
                key,
                value,
                ..
            } => {
                scope.ctor_depth = Some(0);
                if let Node::FunctionExpr { params, .. } = *ast.node(value) {
                    for &p in ast.list(params) {
                        if let Node::Param { modifiers, .. } = *ast.node(p)
                            && modifiers.contains(Modifiers::PRIVATE)
                        {
                            self.declare(cx, scope, p);
                        }
                    }
                }
                self.walk(cx, scope, key, depth + 1);
                ast.for_each_child(value, |c| self.walk(cx, scope, c, depth + 1));
                scope.ctor_depth = None;
                return;
            }
            Node::Arrow { .. }
            | Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::MethodDef { .. } => {
                let saved = scope.ctor_depth;
                scope.ctor_depth = saved.map(|d| d + 1);
                ast.for_each_child(node, |c| self.walk(cx, scope, c, depth + 1));
                scope.ctor_depth = saved;
                return;
            }
            Node::Member { obj, prop, .. } if written(cx, node) => {
                if let Some(name) = key_name(ast, cx.src(), prop) {
                    Self::modify(cx, scope, obj, name.into_owned());
                }
            }
            Node::Index { obj, index, .. } => {
                if let Some(Node::Assign { left, .. }) = cx.parent(node).map(|p| cx.node(p))
                    && *left == node
                    && let Some(name) = static_value_string(ast, cx.src(), index)
                {
                    Self::modify(cx, scope, obj, name.into_owned());
                }
            }
            _ => {}
        }
        ast.for_each_child(node, |c| self.walk(cx, scope, c, depth + 1));
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, scope: &Scope, decl: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let (decorators, name, end, insert_at) = match *ast.node(decl) {
            Node::PropertyDef {
                decorators, key, ..
            } => (decorators, key, cx.span(key).end, cx.span(key).start),
            Node::Param {
                decorators,
                pattern,
                ..
            } => {
                let p = cx.span(pattern);
                let len = ident_name(ast, pattern).map_or(0, |n| n.len() as u32);
                (decorators, pattern, p.start + len, p.start)
            }
            _ => return,
        };
        let start = match ast.list(decorators).last() {
            Some(&d) => skip_trivia_forward(src, cx.span(d).end as usize) as u32,
            None => cx.span(decl).start,
        };
        // ESTree's name of a computed key is the expression inside the brackets.
        let name = match *ast.node(name) {
            Node::ComputedKey { expr } => expr,
            _ => name,
        };
        let text = cx.span(name).text(src).to_owned();
        let mut fix = Fix::insert(insert_at, "readonly ");
        if let Some(annotation) = self.annotation(cx, scope, decl) {
            fix = fix.and(
                Span::new(cx.span(name).end, cx.span(name).end),
                format!(": {annotation}"),
            );
        }
        cx.report(
            Span::new(start.min(end), end),
            format!("Member '{text}' is never reassigned; mark it as `readonly`."),
        )
        .id("preferReadonly")
        .fix(fix);
    }

    /// The type to write on a field whose literal initializer would otherwise narrow it once
    /// `readonly` (only when the constructor assigns it).
    fn annotation(&self, cx: &Ctx<'_, '_>, scope: &Scope, decl: NodeId) -> Option<String> {
        let ast = cx.ast();
        let Node::PropertyDef {
            key,
            type_ann: None,
            value: Some(value),
            ..
        } = *ast.node(decl)
        else {
            return None;
        };
        let name = ident_name(ast, key)?;
        if !scope.ctor_modified.contains(name) {
            return None;
        }
        let ti = cx.types();
        let declared = ti.type_at(decl)?;
        let init = ti.type_at(value)?;
        if declared == init
            || !ti.flags(init).intersects(
                TypeFlags::STRING_LITERAL
                    | TypeFlags::NUMBER_LITERAL
                    | TypeFlags::BIGINT_LITERAL
                    | TypeFlags::BOOLEAN_LITERAL,
            )
        {
            return None;
        }
        let annotation = ti.type_to_string(declared);
        if ti.flags(init).contains(TypeFlags::ENUM_LITERAL) {
            // The name must reach a type declaration of that same type from here.
            let sem = cx.semantic();
            let local = sem.lookup(sem.enclosing_scope(decl), &annotation)?;
            let d = sem.symbol(local).decls.iter().find(|d| {
                matches!(
                    d.kind,
                    DeclKind::Enum | DeclKind::TypeAlias | DeclKind::Interface | DeclKind::Class
                )
            })?;
            let ty = match cx.parent(d.name).map(|p| cx.node(p)) {
                Some(&Node::TypeAlias { ty, .. }) => ti.type_of_type_node(ty),
                _ => ti
                    .symbol_at(d.name)
                    .and_then(|s| ti.declared_type_of_symbol(s)),
            };
            if ty != Some(declared) {
                return None;
            }
        }
        Some(annotation)
    }
}

/// The member access is written: assigned (`=`, `+=`, ...), deleted, incremented, or a
/// target of a destructuring assignment. Parentheses around it hide the write, as upstream.
fn written(cx: &Ctx<'_, '_>, m: NodeId) -> bool {
    let Some(first) = cx.parent(m) else {
        return false;
    };
    match *cx.node(first) {
        Node::Assign { left, .. } => return left == m,
        Node::Unary {
            op: UnaryOp::Delete,
            ..
        }
        | Node::Update { .. } => return true,
        _ => {}
    }
    // Destructuring: up through object and array literals (and spreads in them) to the
    // left side of an `=`.
    let mut cur = first;
    for _ in 0..256 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(p) {
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => {}
            Node::Spread { .. }
                if cx.parent(p).is_some_and(|g| {
                    matches!(cx.node(g), Node::ObjectLit { .. } | Node::ArrayLit { .. })
                }) => {}
            Node::Assign { left, op, .. } if !matches!(cx.node(cur), Node::Member { .. }) => {
                return left == cur && op == AssignOp::Assign;
            }
            _ => return false,
        }
        cur = p;
    }
    false
}

impl Rule for PreferReadonly {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (name, members) = match *cx.node(node) {
            Node::ClassDecl { name, members, .. } | Node::ClassExpr { name, members, .. } => {
                (name, members)
            }
            _ => return,
        };
        let ti = cx.types();
        let class_sym = name
            .and_then(|n| ti.symbol_at(n))
            .or_else(|| ti.symbol_at(node));
        let mut scope = Scope {
            class_sym,
            class_name: class_sym.map(|s| ti.symbol_name(s)),
            ctor_depth: None,
            modified: BTreeSet::new(),
            ctor_modified: BTreeSet::new(),
            static_modified: BTreeSet::new(),
            members: Vec::new(),
            statics: Vec::new(),
        };
        for &m in ast.list(members) {
            if matches!(ast.node(m), Node::PropertyDef { .. }) {
                self.declare(cx, &mut scope, m);
            }
        }
        ast.for_each_child(node, |c| self.walk(cx, &mut scope, c, 0));
        let found: Vec<NodeId> = scope
            .members
            .iter()
            .filter(|(n, _)| !scope.modified.contains(n))
            .chain(
                scope
                    .statics
                    .iter()
                    .filter(|(n, _)| !scope.static_modified.contains(n)),
            )
            .map(|&(_, d)| d)
            .collect();
        for d in found {
            self.report(cx, &scope, d);
        }
    }
}
