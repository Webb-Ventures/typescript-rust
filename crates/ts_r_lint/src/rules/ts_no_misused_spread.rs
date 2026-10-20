//! `@typescript-eslint/no-misused-spread`: spread only values whose spread does what it
//! looks like (no strings into arrays; no arrays, maps, promises, functions or class
//! values into objects).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-misused-spread",
        "Disallow using the spread operator when it might cause unexpected behavior",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create::<NoMisusedSpread>,
};

#[derive(Debug)]
struct NoMisusedSpread {
    allow: Vec<Specifier>,
}

impl FromOptions for NoMisusedSpread {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoMisusedSpread {
            allow: parse_specifiers(obj_opt(opts, 0).get("allow")),
        })
    }
}

const STRING: &str = "Using the spread operator on a string can mishandle special characters, as can `.split(\"\")`.\n- `...` produces Unicode code points, which will decompose complex emojis into individual emojis\n- .split(\"\") produces UTF-16 code units, which breaks rich characters in many languages\nConsider using `Intl.Segmenter` for locale-aware string decomposition.\nOtherwise, if you don't need to preserve emojis or other non-Ascii characters, disable this lint rule on this line or configure the 'allow' rule option.";
const ARRAY: &str =
    "Using the spread operator on an array in an object will result in a list of indices.";
const ITERABLE: &str =
    "Using the spread operator on an Iterable in an object can cause unexpected behavior.";
const CLASS_INSTANCE: &str =
    "Using the spread operator on class instances will lose their class prototype.";
const MAP: &str = "Using the spread operator on a Map in an object will result in an empty object. Did you mean to use `Object.fromEntries(map)` instead?";
const PROMISE: &str = "Using the spread operator on Promise in an object can cause unexpected behavior. Did you forget to await the promise?";
const FUNCTION: &str = "Using the spread operator on a function without additional properties can cause unexpected behavior. Did you forget to call the function?";
const CLASS_DECLARATION: &str = "Using the spread operator on class declarations will spread only their static properties, and will lose their class prototype.";

impl Rule for NoMisusedSpread {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Spread, NodeKind::JsxSpreadAttr]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (arg, in_object, object) = match *cx.node(node) {
            Node::JsxSpreadAttr { arg } => (arg, true, None),
            Node::Spread { arg } => match cx.parent(node).map(|p| (p, cx.node(p))) {
                Some((_, Node::ArrayLit { .. } | Node::Call { .. } | Node::New { .. })) => {
                    (arg, false, None)
                }
                Some((p, Node::ObjectLit { .. })) => (arg, true, Some(p)),
                _ => return,
            },
            _ => return,
        };
        let ti = cx.types();
        let Some(t) = ti.type_at(arg) else { return };
        if has_unresolved_part(ti, t) || self.allowed(ti, t) || self.allowed_value(cx, arg) {
            return;
        }
        let leaves = leaves(ti, t);
        if leaves.iter().any(|&p| is_any(ti, p) || is_unknown(ti, p)) && !in_object {
            return;
        }
        if !in_object {
            if leaves
                .iter()
                .any(|&p| ti.flags(p).intersects(TypeFlags::STRING_LIKE))
            {
                cx.report_node(node, STRING).id("noStringSpread");
            }
            return;
        }
        // Union members (constrained); a member counts when it or an intersected type does.
        // The branches of `?:` count too (their union may have been reduced).
        let mut tops = parts(ti, constrained(ti, t));
        branch_types(cx, arg, &mut tops, 0);
        let members: Vec<Vec<Ty>> = tops
            .into_iter()
            .map(|p| {
                let p = constrained(ti, p);
                if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                    ti.intersection_members(p)
                        .into_iter()
                        .map(|m| constrained(ti, m))
                        .collect()
                } else {
                    vec![p]
                }
            })
            .collect();
        if self.allow.is_empty() && is_expando_function(cx, arg) {
            return;
        }
        let some = |f: &dyn Fn(Ty) -> bool| members.iter().any(|m| m.iter().any(|&x| f(x)));
        let usable = |x: Ty| x != Ty::UNRESOLVED && !is_any(ti, x) && !is_unknown(ti, x);
        if some(&|x| usable(x) && is_builtin_symbol_like(ti, x, &["Promise"])) {
            let target = skip_parens(cx.ast(), arg);
            let text = cx.span(target).text(cx.src());
            let fix = if needs_parens(cx.node(target)) {
                Fix::replace(cx.span(target), format!("await ({text})"))
            } else {
                Fix::replace(cx.span(target), format!("await {text}"))
            };
            cx.report_node(node, PROMISE)
                .id("noPromiseSpreadInObject")
                .suggest("addAwait", "Add await operator.", fix);
            return;
        }
        if some(&|x| {
            usable(x)
                && ti.flags(x).contains(TypeFlags::OBJECT)
                && !ti.signatures(x, false).is_empty()
                && ti.signatures(x, true).is_empty()
                && ti.properties_of(x).is_empty()
        }) {
            cx.report_node(node, FUNCTION)
                .id("noFunctionSpreadInObject");
            return;
        }
        let is_map =
            |x: Ty| usable(x) && is_builtin_symbol_like(ti, x, &["Map", "ReadonlyMap", "WeakMap"]);
        if some(&is_map) {
            // `Object.fromEntries` works for a `Map` / `ReadonlyMap`, not a union or `WeakMap`.
            let fixable = members.len() == 1
                && members[0]
                    .iter()
                    .any(|&x| usable(x) && is_builtin_symbol_like(ti, x, &["Map", "ReadonlyMap"]));
            let fix = fixable.then(|| map_fix(cx, arg, object));
            let r = cx.report_node(node, MAP).id("noMapSpreadInObject");
            if let Some(fix) = fix {
                r.suggest(
                    "replaceMapSpreadInObject",
                    "Replace map spread in object with `Object.fromEntries()`",
                    fix,
                );
            }
            return;
        }
        if some(&|x| usable(x) && (ti.is_array(x) || ti.is_tuple(x))) {
            cx.report_node(node, ARRAY).id("noArraySpreadInObject");
            return;
        }
        let iterator = ti.well_known_symbol("iterator");
        if some(&|x| {
            usable(x)
                && !ti.flags(x).intersects(TypeFlags::STRING_LIKE)
                && ti.flags(x).contains(TypeFlags::OBJECT)
                && iterator
                    .as_deref()
                    .is_some_and(|k| ti.property_of_type(x, k).is_some())
        }) {
            cx.report_node(node, ITERABLE)
                .id("noIterableSpreadInObject");
            return;
        }
        if some(&|x| {
            usable(x)
                && ti.flags(x).contains(TypeFlags::OBJECT)
                && !ti.signatures(x, true).is_empty()
        }) {
            cx.report_node(node, CLASS_DECLARATION)
                .id("noClassDeclarationSpreadInObject");
            return;
        }
        if some(&|x| usable(x) && is_class_instance(ti, x)) {
            cx.report_node(node, CLASS_INSTANCE)
                .id("noClassInstanceSpreadInObject");
        }
    }
}

impl NoMisusedSpread {
    fn allowed(&self, ti: &dyn TypeInfo, t: Ty) -> bool {
        if self.allow.is_empty() {
            return false;
        }
        if type_matches(ti, t, &self.allow) {
            return true;
        }
        // Intrinsic names (`string`) have no symbol.
        let name = ti.type_to_string(ti.base_of_literal(t));
        self.allow
            .iter()
            .any(|s| matches!(s.from, SpecFrom::Any | SpecFrom::Lib) && s.names.contains(&name))
    }
}

impl NoMisusedSpread {
    /// The spread names an allowed function or class by its declaration (`...f`,
    /// `...class A {}`).
    fn allowed_value(&self, cx: &Ctx<'_, '_>, arg: NodeId) -> bool {
        if self.allow.is_empty() {
            return false;
        }
        let ti = cx.types();
        let e = skip_parens(cx.ast(), arg);
        let name = match *cx.node(e) {
            Node::Ident { .. } => e,
            // Declared right here, in this file.
            Node::ClassExpr { name: Some(n), .. } | Node::FunctionExpr { name: Some(n), .. } => {
                let text = cx.span(n).text(cx.src());
                return self.allow.iter().any(|s| {
                    matches!(s.from, SpecFrom::Any | SpecFrom::File(None))
                        && s.names.iter().any(|x| x == text)
                });
            }
            _ => return false,
        };
        let Some(s) = ti.symbol_at(name) else {
            return false;
        };
        let facts = ti.symbol_facts(s);
        (facts.function || facts.class) && symbol_matches(ti, s, &ti.symbol_name(s), &self.allow)
    }
}

/// Types of the branches of a (parenthesized, nested) conditional.
fn branch_types(cx: &Ctx<'_, '_>, e: NodeId, out: &mut Vec<Ty>, depth: u32) {
    if depth > 16 {
        return;
    }
    let ti = cx.types();
    if let Node::Conditional { cons, alt, .. } = *cx.node(skip_parens(cx.ast(), e)) {
        for b in [cons, alt] {
            if let Some(t) = ti.type_at(b)
                && !has_unresolved_part(ti, t)
            {
                out.extend(parts(ti, constrained(ti, t)));
            }
            branch_types(cx, b, out, depth + 1);
        }
    }
}

/// `f` is a function this file gives properties (`f.prop = 1`).
fn is_expando_function(cx: &Ctx<'_, '_>, arg: NodeId) -> bool {
    let ast = cx.ast();
    let e = skip_parens(ast, arg);
    if !matches!(cx.node(e), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(s) = sem.reference_at(e).and_then(|r| sem.reference(r).symbol()) else {
        return false;
    };
    let sym = sem.symbol(s);
    let is_function = sym.decls.iter().any(|d| match *cx.node(d.node) {
        Node::FunctionDecl { .. } => true,
        Node::VarDeclarator { init: Some(i), .. } => matches!(
            cx.node(skip_parens(ast, i)),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        ),
        _ => false,
    });
    is_function
        && sym.refs.iter().any(|&r| {
            let id = sem.reference(r).node;
            cx.parent(id).is_some_and(|m| {
                matches!(*cx.node(m), Node::Member { obj, .. } if obj == id)
                    && cx.parent(m).is_some_and(
                        |a| matches!(*cx.node(a), Node::Assign { left, .. } if left == m),
                    )
            })
        })
}

/// Union members and intersected types, constrained, flattened.
fn leaves(ti: &dyn TypeInfo, t: Ty) -> Vec<Ty> {
    let mut out = Vec::new();
    collect_leaves(ti, t, &mut out, 0);
    out
}

fn collect_leaves(ti: &dyn TypeInfo, t: Ty, out: &mut Vec<Ty>, depth: u32) {
    if depth > 16 || t == Ty::UNRESOLVED {
        return;
    }
    let t = constrained(ti, t);
    let f = ti.flags(t);
    if f.contains(TypeFlags::UNION) {
        for m in ti.union_members(t) {
            collect_leaves(ti, m, out, depth + 1);
        }
    } else if f.contains(TypeFlags::INTERSECTION) {
        for m in ti.intersection_members(t) {
            collect_leaves(ti, m, out, depth + 1);
        }
    } else {
        out.push(t);
    }
}

/// An instance of a class: a class type, or a library interface with a constructor value of
/// the same name (`Date`, `RegExp`, `WeakSet`).
fn is_class_instance(ti: &dyn TypeInfo, t: Ty) -> bool {
    if !ti.flags(t).contains(TypeFlags::OBJECT) || !ti.signatures(t, true).is_empty() {
        return false;
    }
    let Some(s) = ti.type_symbol(t) else {
        return false;
    };
    let facts = ti.symbol_facts(s);
    if facts.class {
        return true;
    }
    facts.interface && facts.variable && !ti.signatures(ti.type_of_symbol(s), true).is_empty()
}

/// Expressions that bind looser than `await x`.
fn needs_parens(n: &Node) -> bool {
    matches!(
        n,
        Node::Binary { .. }
            | Node::Assign { .. }
            | Node::Conditional { .. }
            | Node::Seq { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::Arrow { .. }
            | Node::Yield { .. }
    )
}

/// `{ ...map }` -> `Object.fromEntries(map)`; `{ a, ...map }` -> `{ a, ...Object.fromEntries(map) }`.
fn map_fix(cx: &Ctx<'_, '_>, arg: NodeId, object: Option<NodeId>) -> Fix {
    let ast = cx.ast();
    let inner = skip_parens(ast, arg);
    let text = cx.span(inner).text(cx.src());
    let call = if matches!(cx.node(inner), Node::Seq { .. }) {
        format!("Object.fromEntries(({text}))")
    } else {
        format!("Object.fromEntries({text})")
    };
    if let Some(o) = object
        && let Node::ObjectLit { props } = *cx.node(o)
        && ast.list(props).len() == 1
    {
        return Fix::replace(cx.span(o), call);
    }
    Fix::replace(cx.span(arg), call)
}
