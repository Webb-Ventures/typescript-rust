//! `unicorn/prefer-at`: index reads that `.at()` expresses better.
//!
//! - `x[x.length - n]` (a positive integer `n`, possibly nested: `x[x.length - 1 - 1]`) is
//!   fixed to `x.at(-n)`; with `checkAllIndexAccess`, any read of a static non-negative
//!   integer index too (`x[0]` → `x.at(0)`), unless the receiver is obviously not an
//!   array (an object, number, function or class literal, or a `const` holding one).
//!   Writes, calls (`x[0]()`), tagged templates and `delete` are skipped, as are
//!   `arguments` and DOM collections (`childNodes`, `children`, `querySelectorAll(...)`
//!   and friends, or a `NodeList`/`HTMLCollection` type).
//! - `s.charAt(s.length - n)`, and with `checkAllIndexAccess` any `charAt(i)` whose index
//!   isn't statically negative: suggestion only.
//! - `s.substring(i, i + 1)` and the other one-character forms: suggestion
//!   `s.at(i)`, none when the arguments hold a comment.
//! - `x.slice(-n)[0]` / `.shift()` / `.pop()` (`n = 1`) and `x.slice(-n, -n + 1)[0]`: fixed to
//!   `x.at(-n)` unless the code holds a comment.
//! - `_.last(x)`, `lodash.last(x)`, `underscore.last(x)` and the `getLastElementFunctions`
//!   option: fixed to `x.at(-1)`.

use super::prelude::*;
use super::unicorn_static::{Static, const_definition, static_value_no_side_effects};
use super::unicorn_text::{
    has_comment_in, is_parenthesized, is_written, member_object_text, needs_semicolon, paren_span,
    paren_text, would_remove_comments,
};
use super::unicorn_types::Checker;
use super::unicorn_util::is_same_reference;
use super::unicorn_util::{space_around_keyword, with_edits};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-at",
        "Prefer `.at()` method for index access and `String#charAt()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferAt>,
};

#[derive(Debug)]
struct PreferAt {
    last_functions: Vec<String>,
    check_all_index_access: bool,
}

impl FromOptions for PreferAt {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut last_functions = str_list(o, "getLastElementFunctions");
        last_functions.extend(["_.last", "lodash.last", "underscore.last"].map(str::to_owned));
        Ok(PreferAt {
            last_functions,
            check_all_index_access: bool_field(o, "checkAllIndexAccess", false),
        })
    }
}

impl Rule for PreferAt {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if matches!(cx.node(node), Node::Index { .. }) {
            self.check_index(cx, node);
        } else {
            self.check_char_at(cx, node);
            check_substring(cx, node);
            check_slice(cx, node);
            self.check_last_function(cx, node);
        }
    }
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

fn is_ts_wrapper(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// The parent past parentheses and TS wrappers, with the child on that path.
fn outer_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(ast.node(p), Node::Paren { .. }) || is_ts_wrapper(ast, p) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

/// The ESTree parent: past parentheses only.
fn plain_parent(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some((p, cur));
        }
    }
}

/// `isReferenceContext`: a call's callee, a tag, or a `delete` operand.
fn is_reference_context(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Some((p, child)) = outer_parent(cx, id) else {
        return false;
    };
    match *cx.node(p) {
        Node::Call { callee, .. } => callee == child,
        Node::TaggedTemplate { tag, .. } => tag == child,
        Node::Unary {
            op: UnaryOp::Delete,
            ..
        } => true,
        _ => false,
    }
}

fn is_number_lit(ast: &Ast, id: NodeId, value: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value: v } if v.get() == value)
}

fn safe_integer(v: f64) -> bool {
    v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0
}

/// `-n` with a positive safe-integer literal `n`: `n`.
fn negative_integer_literal(ast: &Ast, id: NodeId) -> Option<f64> {
    let Node::Unary {
        op: UnaryOp::Minus,
        arg,
    } = *ast.node(skip_parens(ast, id))
    else {
        return None;
    };
    match *ast.node(skip_parens(ast, arg)) {
        Node::NumberLit { value } if safe_integer(value.get()) && value.get() > 0.0 => {
            Some(value.get())
        }
        _ => None,
    }
}

/// `obj.length` (plain member, TS wrappers ignored).
fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, obj: NodeId) -> bool {
    let ast = cx.ast();
    matches!(*ast.node(unwrap_ts(ast, id)), Node::Member { obj: o, optional: false, prop }
        if ident_name(ast, prop) == Some("length") && is_same_reference(cx, o, obj))
}

/// The `obj.length` in `obj.length - n` (or `obj.length - n - m`, ...).
fn negative_index_length(cx: &Ctx<'_, '_>, index: NodeId, obj: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut node = index;
    for _ in 0..1000 {
        let Node::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } = *ast.node(unwrap_ts(ast, node))
        else {
            return None;
        };
        let positive = matches!(*ast.node(unwrap_ts(ast, right)), Node::NumberLit { value }
            if value.get() > 0.0 && safe_integer(value.get()));
        if !positive {
            return None;
        }
        if is_length_of(cx, left, obj) {
            return Some(unwrap_ts(ast, left));
        }
        node = left;
    }
    None
}

/// Removes the `.length` node (with its parentheses and the whitespace after it); `None`
/// when that would drop a comment.
fn remove_length(cx: &Ctx<'_, '_>, length: NodeId) -> Option<(Span, String)> {
    let span = paren_span(cx, length);
    if has_comment_in(cx, span) {
        return None;
    }
    let rest = &cx.src()[span.end as usize..];
    let ws = rest.len() - rest.trim_start().len();
    Some((Span::new(span.start, span.end + ws as u32), String::new()))
}

fn is_unsupported_receiver(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(unwrap_ts(ast, id)),
        Node::ObjectLit { .. }
            | Node::Arrow { .. }
            | Node::FunctionExpr { .. }
            | Node::ClassExpr { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

fn is_obviously_non_array(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts(ast, id);
    is_unsupported_receiver(ast, id)
        || const_definition(cx, id).is_some_and(|(_, init)| is_unsupported_receiver(ast, init))
}

fn is_arguments(ast: &Ast, id: NodeId) -> bool {
    ident_name(ast, skip_parens(ast, id)) == Some("arguments")
}

const DOM_PROPERTIES: &[&str] = &["childNodes", "children"];
const DOM_METHODS: &[&str] = &[
    "getElementsByClassName",
    "getElementsByName",
    "getElementsByTagName",
    "getElementsByTagNameNS",
    "querySelectorAll",
];

fn is_dom_collection_expression(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(unwrap_ts(ast, id)) {
        Node::Member { prop, .. } => {
            ident_name(ast, prop).is_some_and(|n| DOM_PROPERTIES.contains(&n))
        }
        Node::Call { callee, args, .. } => {
            !ast.list(args)
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
                && matches!(*ast.node(skip_parens(ast, callee)), Node::Member { prop, .. }
                    if ident_name(ast, prop).is_some_and(|n| DOM_METHODS.contains(&n)))
        }
        _ => false,
    }
}

fn dom_collection_node(cx: &Ctx<'_, '_>, id: NodeId, _r: &dyn Fn(NodeId) -> bool) -> bool {
    is_dom_collection_expression(cx.ast(), id)
}

static DOM_COLLECTION: Checker = Checker {
    target_names: &[
        "NodeList",
        "NodeListOf",
        "HTMLCollection",
        "HTMLCollectionOf",
    ],
    nullish_in_mixed_union: true,
    target_node: Some(dom_collection_node),
    ..Checker::DEFAULT
};

fn is_dom_collection_receiver(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    is_dom_collection_expression(cx.ast(), id) || DOM_COLLECTION.is_target(cx, id)
}

/// `x.name(...)` (non-computed member) with no spread argument: the member, receiver,
/// property and arguments.
fn method_call<'a>(
    cx: &Ctx<'a, '_>,
    call: NodeId,
    name: &str,
) -> Option<(NodeId, NodeId, NodeId, &'a [NodeId], bool)> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        args,
        optional,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let member = skip_parens(ast, callee);
    let Node::Member {
        obj,
        prop,
        optional: optional_member,
    } = *ast.node(member)
    else {
        return None;
    };
    let args = ast.list(args);
    if ident_name(ast, prop) != Some(name)
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some((member, obj, prop, args, optional || optional_member))
}

impl PreferAt {
    fn check_index(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::Index {
            obj,
            index,
            optional,
        } = *cx.node(node)
        else {
            return;
        };
        if is_written(cx, node) || is_reference_context(cx, node) {
            return;
        }
        let length = negative_index_length(cx, index, obj);
        if length.is_none() {
            if !self.check_all_index_access {
                return;
            }
            let non_negative = matches!(static_value_no_side_effects(cx, index),
                Some(Static::Num(n)) if safe_integer(n) && n >= 0.0);
            if !non_negative || is_obviously_non_array(cx, obj) {
                return;
            }
        }
        if is_arguments(ast, obj) || is_dom_collection_receiver(cx, obj) {
            return;
        }
        let fix = index_fix(cx, node, index, optional, length);
        let report = match length {
            Some(_) => cx
                .report_node(
                    skip_parens(ast, index),
                    "Prefer `.at(…)` over `[….length - index]`.",
                )
                .id("negative-index"),
            None => cx
                .report_node(
                    skip_parens(ast, index),
                    "Prefer `.at(…)` over index access.",
                )
                .id("index"),
        };
        report.fix_opt(fix);
    }

    fn check_char_at(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Some((_, obj, prop, args, _)) = method_call(cx, node, "charAt") else {
            return;
        };
        if matches!(*ast.node(node), Node::Call { optional: true, .. }) {
            return;
        }
        let &[index] = args else {
            return;
        };
        let length = negative_index_length(cx, index, obj);
        if length.is_none() {
            if !self.check_all_index_access {
                return;
            }
            let negative = match static_value_no_side_effects(cx, index) {
                Some(v) => v.to_number().is_some_and(|n| n < 0.0),
                None => false,
            };
            if negative {
                return;
            }
        }
        let fix = match length {
            Some(l) => remove_length(cx, l)
                .map(|edit| with_edits(Fix::replace(cx.span(prop), "at"), vec![edit])),
            None => Some(Fix::replace(cx.span(prop), "at")),
        };
        let (id, msg) = match length {
            Some(_) => (
                "string-char-at-negative",
                "Prefer `String#at(…)` over `String#charAt(….length - index)`.",
            ),
            None => (
                "string-char-at",
                "Prefer `String#at(…)` over `String#charAt(…)`.",
            ),
        };
        let report = cx.report_node(skip_parens(ast, index), msg).id(id);
        if let Some(fix) = fix {
            report.suggest("use-at", "Use `.at(…)`.", fix);
        }
    }

    fn check_last_function(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            args,
            optional: false,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let &[array] = ast.list(args) else {
            return;
        };
        if matches!(ast.node(array), Node::Spread { .. }) {
            return;
        }
        let callee = skip_parens(ast, callee);
        let Some(matched) = self
            .last_functions
            .iter()
            .find(|path| matches_name_or_path(ast, callee, path))
        else {
            return;
        };
        if is_arguments(ast, array) || is_dom_collection_receiver(cx, array) {
            return;
        }
        let fix =
            (!would_remove_comments(cx, cx.span(node), &[paren_span(cx, array)])).then(|| {
                let mut fixed = format!(
                    "{}.at(-1)",
                    member_object_text(cx, skip_parens(ast, array), node)
                );
                if needs_semicolon(cx, cx.span(node).start, &fixed) {
                    fixed.insert(0, ';');
                }
                with_edits(
                    Fix::replace(cx.span(node), fixed),
                    space_around_keyword(cx, node),
                )
            });
        let msg = format!(
            "Prefer `.at(-1)` over `{}(…)` to get the last element.",
            matched.trim()
        );
        cx.report_node(callee, msg)
            .id("get-last-function")
            .fix_opt(fix);
    }
}

fn index_fix(
    cx: &Ctx<'_, '_>,
    node: NodeId,
    index: NodeId,
    optional: bool,
    length: Option<NodeId>,
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let mut edits = Vec::new();
    if let Some(l) = length {
        edits.push(remove_length(cx, l)?);
    }
    // `x[x.length - 1]` → `x.at(-1)`: drop the space between `-` and the number.
    if let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *ast.node(skip_parens(ast, index))
        && length == Some(skip_parens(ast, left))
        && matches!(ast.node(right), Node::NumberLit { .. })
        && cx.text(right).bytes().all(|b| b.is_ascii_digit())
    {
        let start = cx.span(right).start as usize;
        let gap = src[..start].len() - src[..start].trim_end().len();
        if gap > 0 && src[..start - gap].ends_with('-') {
            edits.push((Span::new((start - gap) as u32, start as u32), String::new()));
        }
    }
    let inner = paren_span(cx, index);
    let open = skip_trivia_backward(ast, src, inner.start as usize);
    if open == 0 || src.as_bytes()[open - 1] != b'[' {
        return None;
    }
    let open = (open - 1) as u32;
    let close = cx.span(node).end - 1;
    edits.push((
        Span::new(open, open + 1),
        if optional { "at(" } else { ".at(" }.to_owned(),
    ));
    edits.push((Span::new(close, close + 1), ")".to_owned()));
    if in_unparenthesized_new_callee(cx, node) {
        let span = cx.span(node);
        edits.push((Span::at(span.start), "(".to_owned()));
        edits.push((Span::at(span.end), ")".to_owned()));
    }
    Some(with_edits(Fix { edits: Vec::new() }, edits))
}

/// `isInUnparenthesizedNewExpressionCallee`.
fn in_unparenthesized_new_callee(cx: &Ctx<'_, '_>, mut node: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..10_000 {
        if is_parenthesized(cx, node) {
            return false;
        }
        let Some(p) = cx.parent(node) else {
            return false;
        };
        match *ast.node(p) {
            Node::New { callee, .. } if callee == node => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == node => node = p,
            Node::TaggedTemplate { tag, .. } if tag == node => node = p,
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. }
                if expr == node =>
            {
                node = p
            }
            _ => return false,
        }
    }
    false
}

/// `isNodeMatchesNameOrPath`: `name` or `a.b.c` (plain members; `import.meta` / `new.target`
/// as the head).
fn matches_name_or_path(ast: &Ast, mut node: NodeId, path: &str) -> bool {
    let names: Vec<&str> = path.trim().split('.').collect();
    for i in (1..names.len()).rev() {
        let name = names[i];
        if name.is_empty() {
            return false;
        }
        node = skip_parens(ast, node);
        if i == 1
            && let Node::MetaProperty { kind } = *ast.node(node)
        {
            let (meta, property) = match kind {
                ts_r_ast::MetaPropKind::ImportMeta => ("import", "meta"),
                _ => ("new", "target"),
            };
            return property == name && names[0] == meta;
        }
        match *ast.node(node) {
            Node::Member {
                obj,
                prop,
                optional: false,
            } if ident_name(ast, prop) == Some(name) => node = obj,
            _ => return false,
        }
    }
    let node = skip_parens(ast, node);
    match (names[0], ast.node(node)) {
        (_, Node::Ident { .. }) => ident_name(ast, node) == Some(names[0]),
        ("this", Node::This {}) | ("super", Node::Super {}) => true,
        _ => false,
    }
}

/// `getSubstringSingleCharacterIndex`: the index a one-character `substring` reads.
fn substring_index(cx: &Ctx<'_, '_>, call: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let (_, _, _, args, _) = method_call(cx, call, "substring")?;
    let &[first, second] = args else {
        return None;
    };
    let non_negative = |id: NodeId| match *ast.node(skip_parens(ast, id)) {
        Node::NumberLit { value } if safe_integer(value.get()) && value.get() >= 0.0 => {
            Some(value.get())
        }
        _ => None,
    };
    if let (Some(a), Some(b)) = (non_negative(first), non_negative(second))
        && (a - b).abs() == 1.0
    {
        return Some(if a < b { first } else { second });
    }
    let plus_one = |sum: NodeId, index: NodeId| {
        matches!(*ast.node(skip_parens(ast, sum)), Node::Binary { left, op: BinaryOp::Add, right }
            if (is_same_reference(cx, left, index) && is_number_lit(ast, right, 1.0))
                || (is_number_lit(ast, left, 1.0) && is_same_reference(cx, right, index)))
    };
    if plus_one(second, first) {
        return Some(first);
    }
    if plus_one(first, second) {
        return Some(second);
    }
    match *ast.node(skip_parens(ast, first)) {
        Node::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } if is_number_lit(ast, right, 1.0) && is_same_reference(cx, left, second) => Some(first),
        _ => None,
    }
}

fn check_substring(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some(index) = substring_index(cx, node) else {
        return;
    };
    let Some((_, _, prop, &[first, second], _)) = method_call(cx, node, "substring") else {
        return;
    };
    let range = Span::new(paren_span(cx, first).start, paren_span(cx, second).end);
    let fix = (!has_comment_in(cx, range)).then(|| {
        Fix::replace(cx.span(prop), "at").and(range, paren_text(cx, skip_parens(ast, index)))
    });
    let report = cx
        .report_node(
            node,
            "Prefer `String#at(…)` over `String#substring(…)` when getting one character.",
        )
        .id("string-substring");
    if let Some(fix) = fix {
        report.suggest("use-at", "Use `.at(…)`.", fix);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FirstElement {
    ZeroIndex,
    Shift,
    Pop,
}

/// How the slice's first element is read: `[0]`, `.shift()` or `.pop()`.
fn first_element_get(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(FirstElement, NodeId)> {
    let ast = cx.ast();
    let (parent, child) = plain_parent(cx, call)?;
    match *ast.node(parent) {
        Node::Index {
            obj,
            index,
            optional: false,
        } if obj == child && is_number_lit(ast, index, 0.0) => {
            if is_written(cx, parent) || is_reference_context(cx, parent) {
                return None;
            }
            Some((FirstElement::ZeroIndex, parent))
        }
        Node::Member {
            obj,
            prop,
            optional: false,
        } if obj == child => {
            let method = match ident_name(ast, prop) {
                Some("shift") => FirstElement::Shift,
                Some("pop") => FirstElement::Pop,
                _ => return None,
            };
            let (outer, callee) = plain_parent(cx, parent)?;
            match *ast.node(outer) {
                Node::Call {
                    callee: c,
                    args,
                    optional: false,
                    ..
                } if c == callee && ast.list(args).is_empty() => Some((method, outer)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn slice_result(cx: &Ctx<'_, '_>, call: NodeId, args: &[NodeId]) -> Option<(FirstElement, NodeId)> {
    let ast = cx.ast();
    let start = negative_integer_literal(ast, *args.first()?)?;
    let (method, get) = first_element_get(cx, call)?;
    match args {
        [_] => (start == 1.0).then_some((method, get)),
        [_, end] => (negative_integer_literal(ast, *end).map(|e| -e) == Some(-start + 1.0))
            .then_some((method, get)),
        _ => None,
    }
}

fn check_slice(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let Some((_, obj, prop, args, _)) = method_call(cx, node, "slice") else {
        return;
    };
    if matches!(*ast.node(node), Node::Call { optional: true, .. }) || args.is_empty() {
        return;
    }
    let Some((method, get)) = slice_result(cx, node, args) else {
        return;
    };
    if is_dom_collection_receiver(cx, obj) {
        return;
    }
    let call_span = cx.span(node);
    let fix = (!has_comment_in(cx, Span::new(call_span.start, cx.span(get).end))).then(|| {
        let mut fix = Fix::replace(cx.span(prop), "at");
        if args.len() != 1 {
            let start = paren_span(cx, args[0]).end;
            fix = fix.and(Span::new(start, call_span.end - 1), "");
        }
        match method {
            FirstElement::ZeroIndex => {
                fix.and(Span::new(paren_span(cx, node).end, cx.span(get).end), "")
            }
            FirstElement::Shift | FirstElement::Pop => {
                let Node::Call { callee, .. } = *ast.node(get) else {
                    return fix;
                };
                let get_member = skip_parens(ast, callee);
                fix.and(
                    Span::new(paren_span(cx, node).end, cx.span(get_member).end),
                    "",
                )
                .and(
                    Span::new(paren_span(cx, get_member).end, cx.span(get).end),
                    "",
                )
            }
        }
    });
    cx.report_node(
        prop,
        "Prefer `.at(…)` over the first element from `.slice(…)`.",
    )
    .id("slice")
    .fix_opt(fix);
}
