//! `unicorn/no-unreadable-object-destructuring`: computed keys, array patterns, more than
//! two levels of object patterns and member targets inside object destructuring.
//!
//! Both pattern forms count: binding patterns (`const {..} =`, parameters, `catch`) and
//! assignment targets (`({..} = x)`, `for ({..} of xs)`), whose object and array literals
//! act as patterns. A pattern's parent pattern is found through defaults, rest elements,
//! property values, parentheses and TypeScript wrappers.
//!
//! A computed key is allowed when the same object pattern has a rest element and the key
//! is not static (a constant without side effects, or `Math.PI`-like constants and
//! `String.raw` tags), since it then removes a dynamic key from the rest.

use super::prelude::*;
use super::unicorn_static::{
    has_side_effectful_const_initializer, is_global_ident, mutable_member_access, skip_ts_only,
    static_value, static_value_no_side_effects,
};
use crate::ast_util::is_assignment_target;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unreadable-object-destructuring",
        "Disallow unreadable object destructuring.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<NoUnreadableObjectDestructuring>,
};

#[derive(Debug, Default)]
struct NoUnreadableObjectDestructuring;

/// Whether `id` is a pattern node: a binding pattern or a literal used as an assignment
/// target. `Some(true)` for object patterns, `Some(false)` for array patterns.
fn pattern_kind(cx: &Ctx<'_, '_>, id: NodeId) -> Option<bool> {
    match cx.ast().kind(id) {
        NodeKind::ObjectPattern => Some(true),
        NodeKind::ArrayPattern => Some(false),
        NodeKind::ObjectLit | NodeKind::ArrayLit
            if is_assignment_target(cx.ast(), cx.file.parents, id) =>
        {
            Some(cx.ast().kind(id) == NodeKind::ObjectLit)
        }
        _ => None,
    }
}

/// The pattern that directly holds `id` (ESLint plugin's `getParentPattern`).
fn parent_pattern(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..4096 {
        let p = cx.parent(cur)?;
        let through = match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Rest { .. }
            | Node::Spread { .. } => true,
            Node::AssignPattern { left, .. } => left == cur,
            Node::Assign { left, .. } => {
                // A default inside an assignment pattern (`{a: b = 1} = x`).
                left == cur
                    && cx
                        .parent(p)
                        .is_some_and(|g| parent_is_pattern_slot(cx, g, p))
            }
            Node::BindingProperty { value, .. } => {
                return (value == Some(cur)).then(|| cx.parent(p)).flatten();
            }
            Node::Property { value, .. } => {
                let g = cx.parent(p)?;
                return (value == cur && pattern_kind(cx, g).is_some()).then_some(g);
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => return Some(p),
            Node::ArrayLit { .. } | Node::ObjectLit { .. } => {
                return pattern_kind(cx, p).map(|_| p);
            }
            _ => false,
        };
        if !through {
            return None;
        }
        cur = p;
    }
    None
}

/// `child` sits where a pattern element goes in `holder` (an array literal element or a
/// property value of a target literal).
fn parent_is_pattern_slot(cx: &Ctx<'_, '_>, holder: NodeId, child: NodeId) -> bool {
    match *cx.ast().node(holder) {
        Node::ArrayLit { .. } => pattern_kind(cx, holder).is_some(),
        Node::Property { value, .. } => {
            value == child
                && cx
                    .parent(holder)
                    .is_some_and(|g| pattern_kind(cx, g).is_some())
        }
        Node::Paren { .. } => cx
            .parent(holder)
            .is_some_and(|g| parent_is_pattern_slot(cx, g, holder)),
        _ => false,
    }
}

/// The number of object patterns from `id` up through its parent patterns (looking at most
/// [`MAX_PATTERN_LEVELS`] levels up).
fn object_depth(cx: &Ctx<'_, '_>, id: NodeId) -> u32 {
    let mut depth = 0;
    let mut cur = Some(id);
    for _ in 0..MAX_PATTERN_LEVELS {
        let Some(n) = cur else { break };
        if pattern_kind(cx, n) == Some(true) {
            depth += 1;
        }
        cur = parent_pattern(cx, n);
    }
    depth
}

const MAX_PATTERN_LEVELS: usize = 64;

/// `Math.PI`-like constants and `String.raw` tags on the real globals.
fn is_known_static_global(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let member = match *ast.node(id) {
        Node::TaggedTemplate { tag, .. } => tag,
        _ => id,
    };
    let Node::Member { obj, prop, .. } = *ast.node(member) else {
        return false;
    };
    let (Some(object), Some(property)) = (ident_name(ast, obj), ident_name(ast, prop)) else {
        return false;
    };
    let known: &[&str] = match object {
        "Math" => &[
            "E", "LN2", "LN10", "LOG2E", "LOG10E", "PI", "SQRT1_2", "SQRT2",
        ],
        "String" => &["raw"],
        _ => return false,
    };
    known.contains(&property) && is_global_ident(cx, obj, object)
}

fn is_static_computed_key(cx: &Ctx<'_, '_>, key: NodeId) -> bool {
    let ast = cx.ast();
    if has_side_effectful_const_initializer(cx, key) {
        return false;
    }
    let inner = skip_ts_only(ast, key);
    if let Node::Member { obj, .. } | Node::Index { obj, .. } = *ast.node(inner)
        && !ident_name(ast, obj).is_some_and(|n| is_global_ident(cx, obj, n))
        && mutable_member_access(cx, inner, &mut Vec::new())
    {
        return false;
    }
    if static_value_no_side_effects(cx, key).is_some() {
        return true;
    }
    is_known_static_global(cx, inner) && static_value(cx, key).is_some()
}

impl Rule for NoUnreadableObjectDestructuring {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::BindingProperty,
            NodeKind::Property,
            NodeKind::ObjectPattern,
            NodeKind::ArrayPattern,
            NodeKind::ObjectLit,
            NodeKind::ArrayLit,
            NodeKind::Member,
            NodeKind::Index,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::BindingProperty { key, .. } | Node::Property { key, .. } => {
                let Node::ComputedKey { expr } = *ast.node(key) else {
                    return;
                };
                let Some(pattern) = cx.parent(node) else {
                    return;
                };
                if pattern_kind(cx, pattern) != Some(true) {
                    return;
                }
                let props = match *ast.node(pattern) {
                    Node::ObjectPattern { props } | Node::ObjectLit { props } => ast.list(props),
                    _ => return,
                };
                let has_rest = props
                    .iter()
                    .any(|&p| matches!(ast.kind(p), NodeKind::Rest | NodeKind::Spread));
                if has_rest && !is_static_computed_key(cx, expr) {
                    return;
                }
                cx.report_node(node, "Do not use computed keys in object destructuring.")
                    .id("computed-key");
            }
            Node::ArrayPattern { .. } | Node::ArrayLit { .. } => {
                if pattern_kind(cx, node).is_none() {
                    return;
                }
                if parent_pattern(cx, node).is_none_or(|p| object_depth(cx, p) == 0) {
                    return;
                }
                cx.report_node(
                    node,
                    "Do not use array destructuring inside object destructuring.",
                )
                .id("nested-array");
            }
            Node::ObjectPattern { .. } | Node::ObjectLit { .. } => {
                if pattern_kind(cx, node).is_none() || object_depth(cx, node) != 3 {
                    return;
                }
                cx.report_node(
                    node,
                    "Do not use object destructuring deeper than two levels.",
                )
                .id("deep-object");
            }
            Node::Member { .. } | Node::Index { .. } => {
                if parent_pattern(cx, node).and_then(|p| pattern_kind(cx, p)) != Some(true) {
                    return;
                }
                cx.report_node(
                    node,
                    "Do not assign destructured values to object properties.",
                )
                .id("property-assignment");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};

    fn lint(src: &str) -> usize {
        let reg = crate::Registry::global();
        let json = serde_json::json!([{ "files": ["**/*.js"], "rules": {
            "unicorn/no-unreadable-object-destructuring": "error" } }]);
        let objs = objects_from_eslint_json(&json, reg).unwrap();
        let file = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = file.file_config("a.js", &ResolveOptions::new()).unwrap();
        crate::lint_source(
            "/p/a.js",
            src,
            &fc,
            &crate::NoTypeInfo,
            crate::types::FileKey(0),
        )
        .diagnostics
        .len()
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let depth = 5_000;
        let src = format!("({}a{} = x);", "{a:[".repeat(depth), "]}".repeat(depth));
        let start = std::time::Instant::now();
        let _ = lint(&src);
        let src = format!("const {}a{} = x;", "{a:".repeat(depth), "}".repeat(depth));
        let _ = lint(&src);
        assert!(start.elapsed().as_secs() < 30);
    }

    #[test]
    fn malformed_patterns_do_not_panic() {
        for src in [
            "({[a]: b, ...",
            "const {a: {b: {c",
            "({a: b.c} = ",
            "for ({a: [b",
        ] {
            let _ = lint(src);
        }
        assert_eq!(lint("const {a: {b: {c}}} = x; ({a: this.b} = x);"), 2);
    }
}
