//! `unicorn/prefer-object-destructuring-defaults`: `const {a, b} = {a: 1, b: 2, ...options}`
//! builds an object only to supply defaults; `const {a = 1, b = 2} = {...options}` says the
//! same with destructuring defaults. Every pattern property must be a plain name binding with
//! exactly one side-effect-free default (no `Object.prototype` names, no default reading the
//! bindings), and the spread must come last. The suggestion is withheld when the declarator
//! contains a comment.

use super::prelude::*;
use super::unicorn_text::{paren_text, would_remove_comments};
use super::unicorn_util_h2::{has_side_effect_strict, plain_key_name};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-object-destructuring-defaults",
        "Prefer object destructuring defaults over default object literals with spread.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferDestructuringDefaults>,
};

#[derive(Debug, Default)]
struct PreferDestructuringDefaults;

const OBJECT_PROTOTYPE: &[&str] = &[
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "__proto__",
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

/// One pattern property: its name, binding identifier, and key when not shorthand.
struct PatternProp {
    name: String,
    binding: NodeId,
    key: Option<NodeId>,
}

fn pattern_props(ast: &Ast, props: &[NodeId]) -> Option<Vec<PatternProp>> {
    let mut out = Vec::new();
    for &p in props {
        let Node::BindingProperty {
            key,
            value,
            init: None,
        } = *ast.node(p)
        else {
            return None;
        };
        let name = plain_key_name(ast, key)?;
        if OBJECT_PROTOTYPE.contains(&name.as_str()) {
            return None;
        }
        let (binding, key) = match value {
            None if matches!(ast.node(key), Node::Ident { .. }) => (key, None),
            Some(v) if matches!(ast.node(v), Node::Ident { .. }) => (v, Some(key)),
            _ => return None,
        };
        out.push(PatternProp { name, binding, key });
    }
    Some(out)
}

/// The defaults by name (value node) and the final spread, when every other property is a
/// plain `key: value` or shorthand without side effects and no name repeats.
fn default_props(ast: &Ast, props: &[NodeId]) -> Option<(Vec<(String, NodeId)>, NodeId)> {
    let (&spread, rest) = props.split_last()?;
    let Node::Spread { arg } = *ast.node(spread) else {
        return None;
    };
    if has_side_effect_strict(ast, arg) {
        return None;
    }
    let mut out: Vec<(String, NodeId)> = Vec::new();
    for &p in rest {
        let (name, value) = match *ast.node(p) {
            Node::Property { key, value, .. } => (plain_key_name(ast, key)?, value),
            Node::ShorthandProp {
                name, init: None, ..
            } => (plain_key_name(ast, name)?, name),
            _ => return None,
        };
        if has_side_effect_strict(ast, value) || out.iter().any(|(n, _)| *n == name) {
            return None;
        }
        out.push((name, value));
    }
    Some((out, spread))
}

/// Some identifier reference inside `root` uses one of `names`.
fn references_any(cx: &Ctx<'_, '_>, root: NodeId, names: &[&str]) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if let Node::Ident { .. } = ast.node(id)
            && sem.reference_at(id).is_some()
            && ident_name(ast, id).is_some_and(|n| names.contains(&n))
        {
            return true;
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    false
}

impl Rule for PreferDestructuringDefaults {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let (Node::ObjectPattern { props: pattern }, Node::ObjectLit { props: object }) =
            (ast.node(name), ast.node(skip_parens(ast, init)))
        else {
            return;
        };
        let Some(pattern) = pattern_props(ast, ast.list(*pattern)) else {
            return;
        };
        let Some((defaults, spread)) = default_props(ast, ast.list(*object)) else {
            return;
        };
        if pattern.is_empty() || pattern.len() != defaults.len() {
            return;
        }
        let names: Vec<&str> = pattern
            .iter()
            .filter_map(|p| ident_name(ast, p.binding))
            .collect();
        let mut parts = Vec::new();
        for p in &pattern {
            let Some(&(_, value)) = defaults.iter().find(|(n, _)| *n == p.name) else {
                return;
            };
            if references_any(cx, value, &names) {
                return;
            }
            let binding = cx.text(p.binding);
            let default = paren_text(cx, value);
            parts.push(match p.key {
                None => format!("{binding} = {default}"),
                Some(key) => format!("{}: {binding} = {default}", cx.text(key)),
            });
        }
        let span = cx.span(node);
        let fix = (!would_remove_comments(cx, span, &[])).then(|| {
            Fix::replace(
                span,
                format!("{{{}}} = {{{}}}", parts.join(", "), cx.text(spread)),
            )
        });
        let report = cx
            .report(
                span,
                "Prefer object destructuring defaults over default object literals with spread.",
            )
            .id("prefer-object-destructuring-defaults");
        if let Some(fix) = fix {
            report.suggest(
                "prefer-object-destructuring-defaults/suggestion",
                "Use object destructuring defaults.",
                fix,
            );
        }
    }
}
