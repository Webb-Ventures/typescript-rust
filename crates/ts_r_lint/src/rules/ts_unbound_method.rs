//! `@typescript-eslint/unbound-method`: class and object methods taken off their object
//! lose `this`.

use super::prelude::*;
use ts_r_ast::{Ast, KeywordTypeKind, Modifiers, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/unbound-method",
        "Enforce unbound methods are called with their expected scope",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<UnboundMethod>,
};

#[derive(Debug)]
struct UnboundMethod {
    ignore_static: bool,
}

impl FromOptions for UnboundMethod {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(UnboundMethod {
            ignore_static: bool_field(o, "ignoreStatic", false),
        })
    }
}

const MSG: &str = "A method that is not declared with `this: void` may cause unintentional scoping of `this` when separated from its object.\nConsider using an arrow function or explicitly `.bind()`ing the method to avoid calling the method with an unintended `this` value. ";
const MSG_HINT: &str =
    "\nIf a function does not access `this`, it can be annotated with `this: void`.";

/// Lib globals whose methods don't use `this` (or are bound): the types of `console`,
/// `Math`, `Number`, ... and of `window`.
const BOUND_GLOBAL_TYPES: &[&str] = &[
    "Console",
    "Math",
    "NumberConstructor",
    "StringConstructor",
    "ObjectConstructor",
    "SymbolConstructor",
    "ArrayConstructor",
    "ProxyConstructor",
    "DateConstructor",
    "Atomics",
    "Reflect",
    "JSON",
    "Window",
];

/// How a method uses `this`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Danger {
    /// No `this` parameter.
    Unannotated,
    /// A `this` parameter other than `void`.
    Annotated,
}

/// The `this` parameter of a parameter list: `None` when absent, `Some(true)` for
/// `this: void`.
fn this_param(ast: &Ast, params: ts_r_ast::NodeList) -> Option<bool> {
    let &first = ast.list(params).first()?;
    let Node::Param {
        pattern, type_ann, ..
    } = *ast.node(first)
    else {
        return None;
    };
    if ident_name(ast, pattern) != Some("this") {
        return None;
    }
    Some(type_ann.is_some_and(|t| {
        matches!(
            ast.node(t),
            Node::KeywordType {
                kind: KeywordTypeKind::Void
            }
        )
    }))
}

fn danger_of_params(ast: &Ast, params: ts_r_ast::NodeList) -> Option<Danger> {
    match this_param(ast, params) {
        None => Some(Danger::Unannotated),
        Some(false) => Some(Danger::Annotated),
        Some(true) => None,
    }
}

/// The parameters of a non-arrow function expression (through parentheses).
fn function_params(ast: &Ast, value: NodeId) -> Option<ts_r_ast::NodeList> {
    match *ast.node(skip_parens(ast, value)) {
        Node::FunctionExpr { params, .. } => Some(params),
        _ => None,
    }
}

impl UnboundMethod {
    /// Whether the property symbol `s` is a method that needs its `this`.
    fn symbol_danger(&self, ti: &dyn TypeInfo, s: Sym) -> Option<Danger> {
        // `Intl.Collator`'s `compare` is a bound getter.
        if ti.symbol_name(s) == "compare"
            && is_lib_symbol(ti, s)
            && ti
                .symbol_parent(s)
                .is_some_and(|p| ti.symbol_name(p) == "Collator")
        {
            return None;
        }
        for d in ti.symbol_decls(s) {
            let Some((ast, _)) = ti.decl_file(&d) else {
                continue;
            };
            let danger = match *ast.node(d.node) {
                Node::MethodDef {
                    modifiers,
                    kind: MethodKind::Method,
                    value,
                    ..
                } => {
                    if self.ignore_static && modifiers.contains(Modifiers::STATIC) {
                        continue;
                    }
                    match function_params(ast, value) {
                        Some(params) => danger_of_params(ast, params),
                        None => None,
                    }
                }
                Node::MethodSignature {
                    kind: MethodKind::Method,
                    params,
                    ..
                } => danger_of_params(ast, params),
                Node::PropertyDef {
                    modifiers,
                    value: Some(value),
                    ..
                } => {
                    if self.ignore_static && modifiers.contains(Modifiers::STATIC) {
                        continue;
                    }
                    function_params(ast, value).and_then(|p| danger_of_params(ast, p))
                }
                Node::Property { value, .. } => {
                    function_params(ast, value).and_then(|p| danger_of_params(ast, p))
                }
                _ => None,
            };
            if danger.is_some() {
                return danger;
            }
        }
        None
    }

    /// Whether reading property `name` off a value of type `t` takes a method that
    /// needs its `this`.
    fn property_danger(&self, ti: &dyn TypeInfo, t: Ty, name: &str) -> Option<Danger> {
        let mut found = None;
        for p in parts(ti, constrained(ti, t)) {
            let p = constrained(ti, p);
            if p == Ty::UNRESOLVED || is_any(ti, p) || bound_global(ti, p) {
                continue;
            }
            let members = if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                ti.intersection_members(p)
            } else {
                vec![p]
            };
            for m in members {
                let at = ti.apparent_type(m);
                if at == Ty::UNRESOLVED {
                    continue;
                }
                let Some(s) = ti.property_of_type(at, name) else {
                    continue;
                };
                match self.symbol_danger(ti, s) {
                    Some(Danger::Unannotated) => return Some(Danger::Unannotated),
                    Some(Danger::Annotated) => found = Some(Danger::Annotated),
                    None => {}
                }
            }
        }
        found
    }

    fn report(cx: &mut Ctx<'_, '_>, node: NodeId, danger: Danger) {
        match danger {
            Danger::Unannotated => {
                cx.report_node(node, format!("{MSG}{MSG_HINT}"))
                    .id("unboundWithoutThisAnnotation");
            }
            Danger::Annotated => {
                cx.report_node(node, MSG).id("unbound");
            }
        }
    }

    /// Checks each property read by an object pattern from values of the `sources` types.
    fn check_pattern(&self, cx: &mut Ctx<'_, '_>, pattern: NodeId, sources: &[Ty]) {
        let ast = cx.ast();
        let src = cx.src();
        let props: Vec<NodeId> = match *cx.node(pattern) {
            Node::ObjectPattern { props } | Node::ObjectLit { props } => ast.list(props).to_vec(),
            _ => return,
        };
        for prop in props {
            let key = match *cx.node(prop) {
                Node::BindingProperty { key, .. } | Node::Property { key, .. } => key,
                Node::ShorthandProp { name, .. } => name,
                _ => continue,
            };
            let Some(name) = static_key_name(ast, src, key).map(|n| n.into_owned()) else {
                continue;
            };
            let ti = cx.types();
            let danger = sources
                .iter()
                .filter_map(|&t| self.property_danger(ti, t, &name))
                .min_by_key(|d| *d != Danger::Unannotated);
            if let Some(d) = danger {
                Self::report(cx, key, d);
            }
        }
    }

    /// The property names a member or element access may read: `None` when unknown.
    fn accessed_names(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(NodeId, Vec<String>)> {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { obj, prop, .. } => {
                if matches!(cx.node(prop), Node::PrivateName { .. }) {
                    return None;
                }
                Some((obj, vec![ident_name(ast, prop)?.to_string()]))
            }
            Node::Index { obj, index, .. } => {
                let index = skip_parens(ast, index);
                if let Some(s) = static_string_eval(cx, index) {
                    return Some((obj, vec![s]));
                }
                let ti = cx.types();
                let t = ti.type_at(index)?;
                if t == Ty::UNRESOLVED {
                    return None;
                }
                let mut names = Vec::new();
                for p in parts(ti, constrained(ti, t)) {
                    names.push(ti.literal_text(p)?);
                }
                Some((obj, names))
            }
            _ => None,
        }
    }
}

/// `t` is the type of one of the lib globals whose methods may be passed around.
fn bound_global(ti: &dyn TypeInfo, t: Ty) -> bool {
    let check = |m: Ty| {
        ti.type_symbol(m).is_some_and(|s| {
            is_lib_symbol(ti, s) && BOUND_GLOBAL_TYPES.contains(&ti.symbol_name(s).as_str())
        })
    };
    if ti.flags(t).contains(TypeFlags::INTERSECTION) {
        return ti.intersection_members(t).into_iter().any(check);
    }
    check(t)
}

/// The parent of `node`, skipping parentheses and `!`.
fn real_parent(cx: &Ctx<'_, '_>, node: NodeId) -> Option<(NodeId, NodeId)> {
    let mut child = node;
    let mut p = cx.parent(child)?;
    while matches!(cx.node(p), Node::Paren { .. } | Node::NonNull { .. }) {
        child = p;
        p = cx.parent(child)?;
    }
    Some((p, child))
}

/// The value is used where losing `this` does not matter: called, tested, assigned to, ...
fn is_safe_use(cx: &Ctx<'_, '_>, node: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return true;
    }
    let Some((p, node)) = real_parent(cx, node) else {
        return false;
    };
    match *cx.node(p) {
        Node::Call { callee, .. } => callee == node,
        Node::TaggedTemplate { tag, .. } => tag == node,
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == node,
        Node::Unary { op, .. } => matches!(
            op,
            UnaryOp::TypeOf | UnaryOp::Void | UnaryOp::Delete | UnaryOp::Not
        ),
        Node::Update { .. } => true,
        Node::If { test, .. } | Node::While { test, .. } | Node::DoWhile { test, .. } => {
            test == node
        }
        Node::For { test, .. } => test == Some(node),
        Node::Switch { disc, .. } => disc == node,
        Node::Conditional { test, .. } => test == node,
        Node::Binary { op, .. } => match op {
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                let left = matches!(*cx.node(p), Node::Binary { left, .. } if left == node);
                if left
                    && real_parent(cx, p).is_some_and(|(gp, _)| {
                        matches!(
                            cx.node(gp),
                            Node::Binary {
                                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                                ..
                            }
                        )
                    })
                {
                    return true;
                }
                is_safe_use(cx, p, depth + 1)
            }
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::StrictEq
            | BinaryOp::StrictNotEq
            | BinaryOp::InstanceOf => true,
            _ => false,
        },
        Node::Assign { left, op, .. } => {
            if left == node {
                return op == AssignOp::Assign;
            }
            // `this.x = super.method` in a class keeps the method on an instance.
            op == AssignOp::Assign
                && matches!(*cx.node(left), Node::Member { obj, .. } if matches!(cx.node(obj), Node::This {}))
        }
        _ => false,
    }
}

/// `node` is inside a type, an ambient declaration or a body-less signature.
fn in_type_position(cx: &Ctx<'_, '_>, param: NodeId) -> bool {
    let Some(f) = cx.parent(param) else {
        return true;
    };
    match *cx.node(f) {
        Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => {
            body.is_none() || in_ambient(cx, f)
        }
        Node::Arrow { .. } => in_ambient(cx, f),
        _ => true,
    }
}

/// Inside a `declare` declaration or a type.
fn in_ambient(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut n = node;
    for _ in 0..256 {
        let Some(p) = cx.parent(n) else {
            return false;
        };
        match cx.node(p) {
            Node::ClassDecl { modifiers, .. }
            | Node::VarDecl { modifiers, .. }
            | Node::Module { modifiers, .. }
            | Node::FunctionDecl { modifiers, .. }
                if modifiers.contains(Modifiers::DECLARE) =>
            {
                return true;
            }
            Node::TypeAlias { .. }
            | Node::Interface { .. }
            | Node::TypeLiteral { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. } => return true,
            _ => {}
        }
        n = p;
    }
    true
}

impl Rule for UnboundMethod {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Member,
            NodeKind::Index,
            NodeKind::VarDeclarator,
            NodeKind::Assign,
            NodeKind::Param,
            NodeKind::ForOf,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::Member { .. } | Node::Index { .. } => {
                if is_safe_use(cx, node, 0) {
                    return;
                }
                let Some((obj, names)) = Self::accessed_names(cx, node) else {
                    return;
                };
                let ti = cx.types();
                let Some(t) = ti.type_at(skip_parens(ast, obj)) else {
                    return;
                };
                let danger = names
                    .iter()
                    .filter_map(|n| self.property_danger(ti, t, n))
                    .min_by_key(|d| *d != Danger::Unannotated);
                if let Some(d) = danger {
                    Self::report(cx, node, d);
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                if !matches!(cx.node(name), Node::ObjectPattern { .. }) || in_ambient(cx, node) {
                    return;
                }
                let ti = cx.types();
                let sources: Vec<Ty> = ti.type_at(skip_parens(ast, init)).into_iter().collect();
                self.check_pattern(cx, name, &sources);
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => {
                let left = skip_parens(ast, left);
                if !matches!(
                    cx.node(left),
                    Node::ObjectLit { .. } | Node::ObjectPattern { .. }
                ) {
                    return;
                }
                let ti = cx.types();
                let sources: Vec<Ty> = ti.type_at(skip_parens(ast, right)).into_iter().collect();
                self.check_pattern(cx, left, &sources);
            }
            Node::Param { pattern, init, .. } => {
                if !matches!(cx.node(pattern), Node::ObjectPattern { .. })
                    || in_type_position(cx, node)
                {
                    return;
                }
                let ti = cx.types();
                let mut sources: Vec<Ty> = ti.type_at(node).into_iter().collect();
                if let Some(init) = init
                    && let Some(t) = ti.type_at(skip_parens(ast, init))
                {
                    sources.push(t);
                }
                self.check_pattern(cx, pattern, &sources);
            }
            Node::ForOf {
                is_await,
                left,
                right,
                ..
            } => {
                let pattern = match *cx.node(left) {
                    Node::VarDecl { decls, .. } => match ast.list(decls) {
                        [d] => match *cx.node(*d) {
                            Node::VarDeclarator { name, .. } => name,
                            _ => return,
                        },
                        _ => return,
                    },
                    _ => left,
                };
                if !matches!(
                    cx.node(pattern),
                    Node::ObjectPattern { .. } | Node::ObjectLit { .. }
                ) {
                    return;
                }
                let ti = cx.types();
                let Some(t) = ti.type_at(skip_parens(ast, right)) else {
                    return;
                };
                let Some(elem) = (if t == Ty::UNRESOLVED {
                    None
                } else {
                    ti.iterated_type(t, is_await)
                }) else {
                    return;
                };
                self.check_pattern(cx, pattern, &[elem]);
            }
            _ => {}
        }
    }
}
