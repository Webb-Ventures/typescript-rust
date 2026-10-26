//! `unicorn/dom-node-dataset`: one style for `data-*` attributes.
//!
//! By default `el.setAttribute('data-x', v)` (two arguments), `el.removeAttribute('data-x')`
//! / `el.hasAttribute('data-x')` (one; no `?.` on the member for these three) and
//! `el.getAttribute('data-x')` (one, not awaited: Playwright's returns a promise) with a
//! static name are reported, unless the receiver is known not to be a DOM node, is the global
//! `document`, or the key is a name `Object.prototype` has. The rewrite to `.dataset` (`.x`,
//! `['x-']` when not an identifier name, `= v`, `delete`, `Object.hasOwn(…)`) is a fix, and
//! only a suggestion for `getAttribute` (missing attributes read `undefined`, not `null`). It
//! is skipped when the call's value is used (`setAttribute`), the call is not a statement
//! (`removeAttribute`), `setAttribute` has an optional chain, or a comment would go.
//!
//! With `preferAttributes` the other way round: `'k' in el.dataset`, `Object.hasOwn(el.dataset,
//! 'k')` and `el.dataset.hasOwnProperty('k')` (fix to `hasAttribute`), `el.dataset.k` reads
//! (suggestion), `= v` statements (fix to `setAttribute`), `delete` statements (fix to
//! `removeAttribute`), `const {a, b} = el.dataset` (suggestion: one declaration per key) and
//! `const data = el.dataset` (suggestion when every use is a plain `data.k` read: inline them
//! and drop the declaration). Keys with a dash, characters an attribute name can't have, or
//! inherited names are left alone.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_ast::remove_statements;
use super::unicorn_static::{is_global_ident, static_value};
use super::unicorn_types::Checker;
use super::unicorn_util::{
    escape_string, has_possible_side_effect, has_unparenthesized_optional_chain,
    in_unparenthesized_new_callee, indent_before, is_identifier_name, is_left_hand_side,
    is_parenthesized, is_value_not_usable, member_object_text, method_call_of, needs_semicolon,
    parent_through_parens, parenthesized_text, static_string, would_remove_comments,
};
use crate::semantic::RefFlags;
use ts_r_ast::{AssignOp, BinaryOp, UnaryOp, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/dom-node-dataset",
        "Enforce consistent style for DOM element dataset access.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<DomNodeDataset>,
};

#[derive(Debug)]
struct DomNodeDataset {
    prefer_attributes: bool,
}

impl FromOptions for DomNodeDataset {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(DomNodeDataset {
            prefer_attributes: bool_field(obj_opt(opts, 0), "preferAttributes", false),
        })
    }
}

/// unicorn's `isKnownNonDomNode`.
static DOM_NODE: Checker = Checker {
    target_names: &[
        "CharacterData",
        "Document",
        "DocumentFragment",
        "Element",
        "HTMLElement",
        "Node",
        "SVGElement",
        "Text",
    ],
    ..Checker::DEFAULT
};

/// Names every object inherits, never a `data-*` attribute.
const INHERITED: &[&str] = &[
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

impl Rule for DomNodeDataset {
    fn kinds(&self) -> &'static [NodeKind] {
        if self.prefer_attributes {
            &[
                NodeKind::Binary,
                NodeKind::Call,
                NodeKind::VarDeclarator,
                NodeKind::Member,
                NodeKind::Index,
            ]
        } else {
            &[NodeKind::Call]
        }
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !self.prefer_attributes {
            prefer_dataset(cx, node);
            return;
        }
        match *cx.node(node) {
            Node::Binary {
                left,
                op: BinaryOp::In,
                right,
            } => {
                let right = skip_parens(cx.ast(), right);
                if static_string(cx.ast(), left).is_some()
                    && is_whole_dataset_access(cx.ast(), right)
                {
                    has_attribute(cx, node, left, right);
                }
            }
            Node::Call { .. } => {
                let ast = cx.ast();
                if let Some((obj, _, &[target, key])) =
                    method_call_of(ast, node, &["hasOwn"], 2, Some(2), Some(false))
                {
                    let target = skip_parens(ast, target);
                    if ident_name(ast, obj) == Some("Object")
                        && is_whole_dataset_access(ast, target)
                        && static_string(ast, key).is_some()
                    {
                        has_attribute(cx, node, key, target);
                    }
                    return;
                }
                if let Some((obj, _, &[key])) =
                    method_call_of(ast, node, &["hasOwnProperty"], 1, Some(1), Some(false))
                {
                    let obj = skip_parens(ast, obj);
                    if is_dataset_access(ast, obj) && static_string(ast, key).is_some() {
                        has_attribute(cx, node, key, obj);
                    }
                }
            }
            Node::VarDeclarator { .. } => declarator(cx, node),
            Node::Member { .. } | Node::Index { .. } => member(cx, node),
            _ => {}
        }
    }
}

/// `x.dataset` (optional or not, never computed).
fn is_dataset_access(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::Member { prop, .. } if ident_name(ast, prop) == Some("dataset"))
}

/// A `x.dataset` that ends no optional chain (ESTree wraps `x?.dataset` in a chain node).
fn is_whole_dataset_access(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    is_dataset_access(ast, id) && !has_unparenthesized_optional_chain(ast, id)
}

fn dataset_object(ast: &ts_r_ast::Ast, dataset: NodeId) -> (NodeId, bool) {
    match *ast.node(dataset) {
        Node::Member { obj, optional, .. } => (skip_parens(ast, obj), optional),
        _ => (dataset, false),
    }
}

fn is_unsafe_key(key: &str) -> bool {
    INHERITED.contains(&key)
        || key.contains('-')
        || key.chars().any(|c| {
            c.is_whitespace()
                || (c as u32) <= 0x1f
                || matches!(c, '"' | '\'' | '/' | '<' | '=' | '>')
        })
}

/// `foo-bar` → `fooBar` (a dash before a lowercase letter).
fn dash_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '-'
            && let Some(&n) = chars.peek()
            && n.is_ascii_lowercase()
        {
            out.push(n.to_ascii_uppercase());
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// `fooBar` → `data-foo-bar`.
fn camel_to_dash(s: &str) -> String {
    let mut out = String::from("data-");
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The quote of a string-literal key, so the rewrite keeps it.
fn key_quote(cx: &Ctx<'_, '_>, key: NodeId) -> Option<char> {
    let key = skip_parens(cx.ast(), key);
    if matches!(cx.node(key), Node::StringLit { .. }) {
        cx.src()[cx.span(key).start as usize..].chars().next()
    } else {
        None
    }
}

/// unicorn's `getStaticPropertyName` for a member: its name, or the static value of a
/// computed key without side effects.
fn property_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => {
            if has_possible_side_effect(cx, index) || contains_identifier(ast, index) {
                return None;
            }
            static_value(cx, index)?.to_js_string()
        }
        _ => None,
    }
}

/// ESLint evaluates the key without a scope, so a variable is never known.
fn contains_identifier(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let mut stack = vec![id];
    let mut seen = 0;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > 10_000 {
            return true;
        }
        match *ast.node(n) {
            Node::Ident { .. } => return true,
            Node::Paren { expr } => stack.push(expr),
            Node::Binary { left, right, .. } => stack.extend([left, right]),
            Node::Unary { arg, .. } => stack.push(arg),
            Node::TemplateLit { parts } => stack.extend(ast.list(parts).iter().copied()),
            _ => {}
        }
    }
    false
}

// ---- default: prefer `.dataset` ----

fn prefer_dataset(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let found = method_call_of(ast, node, &["setAttribute"], 2, Some(2), Some(false))
        .or_else(|| {
            method_call_of(
                ast,
                node,
                &["removeAttribute", "hasAttribute"],
                1,
                Some(1),
                Some(false),
            )
        })
        .or_else(|| method_call_of(ast, node, &["getAttribute"], 1, Some(1), None));
    let Some((receiver, prop, args)) = found else {
        return;
    };
    let receiver = skip_parens(ast, receiver);
    let Some(value) = static_string(ast, args[0]) else {
        return;
    };
    if DOM_NODE.is_known_non_target(cx, receiver) || is_global_ident(cx, receiver, "document") {
        return;
    }
    let method = ident_name(ast, prop).unwrap_or_default();
    if method == "getAttribute"
        && parent_through_parens(cx, node)
            .is_some_and(|(p, _)| matches!(cx.node(p), Node::Await { .. }))
    {
        return;
    }
    let name = value.to_lowercase();
    let Some(rest) = name.strip_prefix("data-") else {
        return;
    };
    let key = dash_to_camel(rest);
    if INHERITED.contains(&key.as_str()) {
        return;
    }
    let message = format!("Prefer `.dataset` over `{method}(…)`.");
    let fix = dataset_fix(cx, node, receiver, method, args, &key);
    let r = cx.report_node(node, message).id("prefer-dataset");
    match fix {
        Some(fix) if method == "getAttribute" => {
            r.suggest("prefer-dataset/suggestion", "Switch to `.dataset`.", fix);
        }
        Some(fix) => {
            r.fix(fix);
        }
        None => {}
    }
}

fn dataset_fix(
    cx: &Ctx<'_, '_>,
    call: NodeId,
    receiver: NodeId,
    method: &str,
    args: &[NodeId],
    key: &str,
) -> Option<Fix> {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *cx.node(call) else {
        return None;
    };
    let callee = skip_parens(ast, callee);
    let optional = matches!(*cx.node(callee), Node::Member { optional: true, .. });
    if method == "setAttribute"
        && (has_unparenthesized_optional_chain(ast, callee) || !is_value_not_usable(cx, call))
    {
        return None;
    }
    if method == "removeAttribute"
        && !parent_through_parens(cx, call)
            .is_some_and(|(p, _)| matches!(cx.node(p), Node::ExprStmt { .. }))
    {
        return None;
    }
    let mut kept = vec![cx.span(receiver)];
    if method == "setAttribute" {
        kept.push(cx.span(args[1]));
    }
    if would_remove_comments(cx, cx.span(call), &kept) {
        return None;
    }
    let quote = key_quote(cx, args[0]);
    let replacement = if matches!(method, "setAttribute" | "getAttribute") {
        call
    } else {
        receiver
    };
    let object = member_object_text(cx, receiver, replacement);
    let dataset = format!("{object}{}.dataset", if optional { "?" } else { "" });
    let text = if method == "hasAttribute" {
        format!("Object.hasOwn({dataset}, {})", escape_string(key, quote))
    } else {
        let access = if is_identifier_name(key) {
            format!("{dataset}.{key}")
        } else {
            format!("{dataset}[{}]", escape_string(key, quote))
        };
        match method {
            "setAttribute" => {
                let value = args[1];
                let value_text = parenthesized_text(cx, value);
                if needs_parens_for_in(cx, value, call) {
                    format!("{access} = ({value_text})")
                } else {
                    format!("{access} = {value_text}")
                }
            }
            "removeAttribute" => format!("delete {access}"),
            _ => access,
        }
    };
    Some(Fix::replace(cx.span(call), text))
}

/// Moving `value` out of the call's arguments exposes an `in` in a `for` initializer:
/// `for (el.setAttribute('data-a', 'b' in c), x; …)`.
fn needs_parens_for_in(cx: &Ctx<'_, '_>, value: NodeId, call: NodeId) -> bool {
    if is_parenthesized(cx, call) {
        return false;
    }
    let Some(seq) = cx.parent(call) else {
        return false;
    };
    if !matches!(cx.node(seq), Node::Seq { .. }) {
        return false;
    }
    matches!(cx.parent(seq).map(|p| *cx.node(p)), Some(Node::For { init: Some(i), .. }) if i == seq)
        && exposes_in(cx, value, 0)
}

fn exposes_in(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 200 || is_parenthesized(cx, id) {
        return false;
    }
    let ast = cx.ast();
    let children: Vec<NodeId> = match *ast.node(id) {
        Node::Binary {
            op: BinaryOp::In, ..
        } => return true,
        Node::Binary { left, right, .. } => vec![left, right],
        Node::Conditional { test, alt, .. } => vec![test, alt],
        Node::Assign { right, .. } => vec![right],
        Node::Seq { exprs } => ast.list(exprs).to_vec(),
        Node::Arrow { body, .. } => vec![body],
        Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::NonNull { expr }
        | Node::TypeAssertion { expr, .. }
        | Node::Instantiation { expr, .. } => vec![expr],
        _ => return false,
    };
    children.into_iter().any(|c| exposes_in(cx, c, depth + 1))
}

// ---- preferAttributes ----

fn attributes_message(method: &str) -> String {
    format!("Prefer `.{method}(…)` over `.dataset`.")
}

fn has_attribute(cx: &mut Ctx<'_, '_>, report: NodeId, key: NodeId, dataset: NodeId) {
    let ast = cx.ast();
    let Some(value) = static_string(ast, key) else {
        return;
    };
    let (obj, optional) = dataset_object(ast, dataset);
    if is_unsafe_key(value) || DOM_NODE.is_known_non_target(cx, obj) {
        return;
    }
    let object = member_object_text(cx, obj, report);
    let chain = if optional { "?." } else { "." };
    let attribute = escape_string(&camel_to_dash(value), key_quote(cx, key));
    let span = cx.span(report);
    let fix = (!would_remove_comments(cx, span, &[cx.span(obj)])).then(|| {
        let mut text = format!("{object}{chain}hasAttribute({attribute})");
        if needs_semicolon(cx, span.start, &text) {
            text.insert(0, ';');
        }
        Fix::replace(span, text)
    });
    let r = cx
        .report_node(report, attributes_message("hasAttribute"))
        .id("prefer-attributes");
    if let Some(fix) = fix {
        r.fix(fix);
    }
}

/// The parent of `decl` when it is a whole statement we may rewrite: one declarator, not a
/// `for` head and not exported.
fn rewritable_declaration(cx: &Ctx<'_, '_>, decl: NodeId) -> Option<(NodeId, VarKind)> {
    let declaration = cx.parent(decl)?;
    let Node::VarDecl {
        modifiers,
        kind,
        decls,
    } = *cx.node(declaration)
    else {
        return None;
    };
    if cx.ast().list(decls).len() != 1 || modifiers.contains(Modifiers::EXPORT) {
        return None;
    }
    let parent = cx.parent(declaration)?;
    if matches!(
        cx.node(parent),
        Node::For { .. } | Node::ForIn { .. } | Node::ForOf { .. } | Node::ExportNamed { .. }
    ) {
        return None;
    }
    Some((declaration, kind))
}

fn kind_text(kind: VarKind) -> &'static str {
    match kind {
        VarKind::Var => "var",
        VarKind::Let => "let",
        VarKind::Const => "const",
        VarKind::Using => "using",
        VarKind::AwaitUsing => "await using",
    }
}

fn declarator(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *cx.node(node)
    else {
        return;
    };
    let dataset = skip_parens(ast, init);
    if !is_whole_dataset_access(ast, dataset) {
        return;
    }
    let is_ident = matches!(ast.node(name), Node::Ident { .. });
    let Node::ObjectPattern { props } = *ast.node(name) else {
        if is_ident {
            let (obj, _) = dataset_object(ast, dataset);
            if DOM_NODE.is_known_non_target(cx, obj) {
                return;
            }
            let fix = inline_fix(cx, node, name, dataset);
            let r = cx
                .report_node(dataset, attributes_message("getAttribute"))
                .id("prefer-attributes");
            if let Some(fix) = fix {
                r.suggest(
                    "prefer-attributes/suggestion",
                    "Switch to `getAttribute()`.",
                    fix,
                );
            }
        }
        return;
    };
    let (obj, _) = dataset_object(ast, dataset);
    if DOM_NODE.is_known_non_target(cx, obj) {
        return;
    }
    let props = ast.list(props);
    let unsafe_key = props.iter().any(|&p| {
        let Node::BindingProperty { key, .. } = *ast.node(p) else {
            return false;
        };
        let key = match *ast.node(key) {
            Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
            Node::ComputedKey { expr } => static_string(ast, expr).map(str::to_owned),
            _ => static_string(ast, key).map(str::to_owned),
        };
        key.is_none_or(|k| is_unsafe_key(&k))
    });
    if unsafe_key {
        return;
    }
    // `{ key }` or `{ key: name }`: no default, rest or computed key.
    let simple: Option<Vec<(&str, &str)>> = props
        .iter()
        .map(|&p| match *ast.node(p) {
            Node::BindingProperty {
                key,
                value,
                init: None,
            } => {
                let k = ident_name(ast, key)?;
                match value {
                    None => Some((k, k)),
                    Some(v) => Some((k, ident_name(ast, v)?)),
                }
            }
            _ => None,
        })
        .collect();
    let suggestion = (|| {
        let simple = simple?;
        let (declaration, kind) = rewritable_declaration(cx, node)?;
        if simple.is_empty()
            || would_remove_comments(cx, cx.span(declaration), &[cx.span(obj)])
            || (simple.len() > 1 && !matches!(ast.node(obj), Node::Ident { .. }))
        {
            return None;
        }
        let object = member_object_text(cx, obj, obj);
        let span = cx.span(declaration);
        let separator = format!(";{}{}", linebreak(cx.src()), indent_before(cx, span.start));
        let text = simple
            .iter()
            .map(|(key, value)| {
                format!(
                    "{} {value} = {object}.getAttribute({})",
                    kind_text(kind),
                    escape_string(&camel_to_dash(key), Some('\''))
                )
            })
            .collect::<Vec<_>>()
            .join(&separator);
        Some(Fix::replace(span, format!("{text};")))
    })();
    let message = attributes_message("getAttribute");
    match suggestion {
        Some(fix) => {
            cx.report_node(node, message)
                .id("prefer-attributes")
                .suggest(
                    "prefer-attributes/suggestion",
                    "Switch to `getAttribute()`.",
                    fix,
                );
        }
        None => {
            cx.report_node(dataset, message).id("prefer-attributes");
        }
    }
}

/// For `const data = el.dataset`: rewrite every `data.k` read to `el.getAttribute('data-k')`
/// and drop the declaration.
fn inline_fix(cx: &Ctx<'_, '_>, decl: NodeId, name: NodeId, dataset: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let (declaration, kind) = rewritable_declaration(cx, decl)?;
    if kind != VarKind::Const {
        return None;
    }
    let (obj, _) = dataset_object(ast, dataset);
    let element = ident_name(ast, obj)?;
    let sym = sem.symbol_of_decl(name)?;
    let mut members = Vec::new();
    for &r in &sem.symbol(sym).refs {
        let r = sem.reference(r);
        if r.flags.contains(RefFlags::INIT) || r.is_type() {
            continue;
        }
        let (member, outer) = parent_through_parens(cx, r.node)?;
        let (Node::Member {
            obj: o,
            optional: false,
            ..
        }
        | Node::Index {
            obj: o,
            optional: false,
            ..
        }) = *cx.node(member)
        else {
            return None;
        };
        if o != outer {
            return None;
        }
        let key = property_name(cx, member)?;
        if is_unsafe_key(&key) || is_left_hand_side(cx, member) {
            return None;
        }
        if let Some((p, m)) = parent_through_parens(cx, member)
            && matches!(*cx.node(p), Node::Call { callee, .. } | Node::TaggedTemplate { tag: callee, .. } if callee == m)
        {
            return None;
        }
        members.push((member, key));
    }
    if members.is_empty() {
        return None;
    }
    // The element must name the same value at every use.
    let element_sym = |at: NodeId| sem.lookup(sem.enclosing_scope(at), element);
    let original = element_sym(obj);
    if members.iter().any(|&(m, _)| element_sym(m) != original)
        || original.is_some_and(|s| {
            sem.symbol(s).refs.iter().any(|&r| {
                let r = sem.reference(r);
                r.is_write() && !r.flags.contains(RefFlags::INIT)
            })
        })
    {
        return None;
    }
    if would_remove_comments(cx, cx.span(declaration), &[])
        || members
            .iter()
            .any(|&(m, _)| would_remove_comments(cx, cx.span(m), &[]))
    {
        return None;
    }
    let object = member_object_text(cx, obj, obj);
    let mut fix = remove_statements(cx, declaration, declaration, false);
    for (member, key) in members {
        let quote = match *cx.node(member) {
            Node::Index { index, .. } => key_quote(cx, index),
            _ => None,
        };
        let mut text = format!(
            "{object}.getAttribute({})",
            escape_string(&camel_to_dash(&key), quote)
        );
        if in_unparenthesized_new_callee(cx, member) {
            text = format!("({text})");
        }
        fix = fix.and(cx.span(member), text);
    }
    Some(fix)
}

fn member(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let (object, optional, quote_of) = match *cx.node(node) {
        Node::Member { obj, optional, .. } => (obj, optional, None),
        Node::Index {
            obj,
            optional,
            index,
        } => (obj, optional, Some(index)),
        _ => return,
    };
    let dataset = skip_parens(ast, object);
    if !is_dataset_access(ast, dataset) {
        return;
    }
    let (obj, dataset_optional) = dataset_object(ast, dataset);
    if DOM_NODE.is_known_non_target(cx, obj) {
        return;
    }
    let Some(key) = property_name(cx, node) else {
        return;
    };
    if is_unsafe_key(&key) || optional {
        return;
    }
    let Some((parent, outer)) = parent_through_parens(cx, node) else {
        return;
    };
    let (is_write, is_delete) = match *cx.node(parent) {
        Node::Call { callee, .. } | Node::TaggedTemplate { tag: callee, .. } if callee == outer => {
            return;
        }
        Node::Update { .. } => return,
        Node::Assign { left, op, .. } if left == outer => {
            // `[el.dataset.k = 1] = a` is a default in a pattern, not an assignment.
            if op != AssignOp::Assign || is_left_hand_side(cx, parent) {
                return;
            }
            (true, false)
        }
        Node::Unary {
            op: UnaryOp::Delete,
            ..
        } => (false, true),
        _ => (false, false),
    };
    if !is_write && !is_delete && is_left_hand_side(cx, node) {
        return;
    }
    let method = if is_write {
        "setAttribute"
    } else if is_delete {
        "removeAttribute"
    } else {
        "getAttribute"
    };
    let object_text =
        member_object_text(cx, obj, if is_write || is_delete { parent } else { node });
    let chain = if dataset_optional { "?." } else { "." };
    let attribute = escape_string(
        &camel_to_dash(&key),
        quote_of.and_then(|k| key_quote(cx, k)),
    );
    let message = attributes_message(method);
    let parent_span = cx.span(parent);
    if is_write {
        let Node::Assign { right, .. } = *cx.node(parent) else {
            return;
        };
        let fix = (is_value_not_usable(cx, parent)
            && !would_remove_comments(cx, parent_span, &[cx.span(obj), cx.span(right)]))
        .then(|| {
            Fix::replace(
                parent_span,
                format!(
                    "{object_text}{chain}setAttribute({attribute}, {})",
                    parenthesized_text(cx, right)
                ),
            )
        });
        let r = cx.report_node(node, message).id("prefer-attributes");
        if let Some(fix) = fix {
            r.fix(fix);
        }
    } else if is_delete {
        let statement = parent_through_parens(cx, parent)
            .is_some_and(|(p, _)| matches!(cx.node(p), Node::ExprStmt { .. }));
        let fix =
            (statement && !would_remove_comments(cx, parent_span, &[cx.span(obj)])).then(|| {
                let mut text = format!("{object_text}{chain}removeAttribute({attribute})");
                if needs_semicolon(cx, parent_span.start, &text) {
                    text.insert(0, ';');
                }
                Fix::replace(parent_span, text)
            });
        let r = cx.report_node(node, message).id("prefer-attributes");
        if let Some(fix) = fix {
            r.fix(fix);
        }
    } else {
        let span = cx.span(node);
        let suggestion = (!would_remove_comments(cx, span, &[cx.span(obj)])).then(|| {
            let mut text = format!("{object_text}{chain}getAttribute({attribute})");
            if in_unparenthesized_new_callee(cx, node) {
                text = format!("({text})");
            }
            Fix::replace(span, text)
        });
        let r = cx.report_node(node, message).id("prefer-attributes");
        if let Some(fix) = suggestion {
            r.suggest(
                "prefer-attributes/suggestion",
                "Switch to `getAttribute()`.",
                fix,
            );
        }
    }
}
