//! `unicorn/no-useless-undefined`: an `undefined` that changes nothing.
//!
//! - `return undefined` (unless the function declares a return type other than `undefined`
//!   or `void`) and `yield undefined` lose the argument; `() => undefined` becomes
//!   `() => {}` (`checkArrowFunctionBody`, not when the arrow declares a return type).
//! - `let x = undefined` (a plain name in a `let`) and a default value of `undefined` lose
//!   the initializer; a function parameter that had one becomes optional in TypeScript
//!   (`a: T = undefined` -> `a?: T`; in a `.ts` file also `a = undefined` -> `a?`). No fix
//!   when the removed text holds a comment.
//! - `i >= 0 ? a[i] : undefined` and the other bound checks of an index that the access
//!   itself already gives `undefined` for are reported, with a suggestion to keep only the
//!   access (none in a call/tag/`delete` position or with comments inside).
//! - A trailing `undefined` call argument (`checkArguments`, not in `.ts`/`.tsx`/`.mts`/
//!   `.cts` files) is removed, except for callees where it means something (`push`, `set`,
//!   `setState`, assertion helpers...) and `bind` with more than the `this` argument.
//!
//! Removing an `undefined` takes the spaces before it and its parentheses (and the spaces
//! before each) with it, keeping a line break that directly precedes them.

use super::prelude::*;
use super::unicorn_ast::remove_argument;
use super::unicorn_text::{is_same_reference, needs_semicolon};
use super::unicorn_util::{skip_wrappers, space_around_keyword};
use super::unicorn_util_h4::{fix_of, has_side_effect, is_typescript_path};
use ts_r_ast::{Ast, BinaryOp, FnFlags, KeywordTypeKind, UnaryOp, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-useless-undefined",
        "Disallow useless `undefined`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoUselessUndefined>,
};

const MESSAGE: &str = "Do not use useless `undefined`.";
const ID: &str = "no-useless-undefined";

#[derive(Debug)]
struct NoUselessUndefined {
    check_arguments: bool,
    check_arrow_body: bool,
}

impl FromOptions for NoUselessUndefined {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut rule = NoUselessUndefined {
            check_arguments: true,
            check_arrow_body: true,
        };
        if let Some(o) = opts.first().and_then(Value::as_object) {
            if let Some(v) = o.get("checkArguments") {
                rule.check_arguments = v.as_bool().ok_or("`checkArguments` must be a boolean")?;
            }
            if let Some(v) = o.get("checkArrowFunctionBody") {
                rule.check_arrow_body = v
                    .as_bool()
                    .ok_or("`checkArrowFunctionBody` must be a boolean")?;
            }
        }
        Ok(rule)
    }
}

/// Callees whose trailing `undefined` argument is meaningful: assertion helpers comparing
/// against it and built-ins or common APIs where it differs from a missing argument.
const IGNORED_CALLEES: &[&str] = &[
    "is",
    "equal",
    "notEqual",
    "strictEqual",
    "notStrictEqual",
    "propertyVal",
    "notPropertyVal",
    "not",
    "include",
    "property",
    "toBe",
    "toHaveBeenCalledWith",
    "toContain",
    "toContainEqual",
    "toEqual",
    "same",
    "notSame",
    "strictSame",
    "strictNotSame",
    "push",
    "unshift",
    "includes",
    "concat",
    "splice",
    "toSpliced",
    "lastIndexOf",
    "reduce",
    "reduceRight",
    "String",
    "fromCharCode",
    "fromCodePoint",
    "Number",
    "max",
    "min",
    "hypot",
    "Array",
    "of",
    "from",
    "structuredClone",
    "add",
    "has",
    "delete",
    "set",
    "createContext",
    "useRef",
    "ref",
];

fn is_undefined(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(id), Node::Ident { name } if ast.atom(name) == "undefined")
}

fn is_ignored_callee(ast: &Ast, callee: NodeId) -> bool {
    let name = match *ast.node(callee) {
        Node::Ident { name } => name,
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => name,
            _ => return false,
        },
        _ => return false,
    };
    let name = ast.atom(name);
    IGNORED_CALLEES.contains(&name)
        || name
            .strip_prefix("set")
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_uppercase())
}

/// The nearest enclosing function (`getFunction` of the node's scope).
fn enclosing_function(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    ancestors_inclusive(cx.file.parents, id).skip(1).find(|&a| {
        matches!(
            ast.node(a),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
        )
    })
}

fn return_type(ast: &Ast, f: NodeId) -> Option<NodeId> {
    match *ast.node(f) {
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type,
        _ => None,
    }
}

/// `replaceNodeOrTokenAndSpacesBefore`: `id` and each of its parentheses replaced, together
/// with the whitespace before it, by the line break that whitespace starts with (and
/// `replacement` for the node itself).
fn replace_with_spaces_before(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    replacement: &str,
) -> Vec<(Span, String)> {
    let src = cx.src();
    let piece = |span: Span, text: &str| {
        let before = &src[..span.start as usize];
        let ws = before.len() - before.trim_end().len();
        let ws_text = &before[before.len() - ws..];
        let lb = if ws_text.starts_with("\r\n") {
            "\r\n"
        } else if ws_text.starts_with(['\n', '\r']) {
            &ws_text[..1]
        } else {
            ""
        };
        (
            Span::new(span.start - ws as u32, span.end),
            format!("{lb}{text}"),
        )
    };
    let mut edits = Vec::new();
    let mut cur = id;
    while let Some(p) = cx.parent(cur) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        let s = cx.span(p);
        edits.push(piece(Span::new(s.start, s.start + 1), ""));
        edits.push(piece(Span::new(s.end - 1, s.end), ""));
        cur = p;
    }
    edits.push(piece(cx.span(id), replacement));
    edits
}

fn has_comment(cx: &Ctx<'_, '_>, span: Span) -> bool {
    cx.ast()
        .comments_in(span)
        .iter()
        .any(|c| c.span.start >= span.start && c.span.end <= span.end)
}

/// `getStaticNumberValue`: a number literal, maybe signed, through TS wrappers.
fn static_number(ast: &Ast, id: NodeId, depth: u32) -> Option<f64> {
    if depth > 64 {
        return None;
    }
    let id = skip_wrappers(ast, id);
    match *ast.node(id) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: op @ (UnaryOp::Plus | UnaryOp::Minus),
            arg,
        } => {
            let v = static_number(ast, arg, depth + 1)?;
            Some(if op == UnaryOp::Minus { -v } else { v })
        }
        _ => None,
    }
}

fn safe_integer(v: Option<f64>) -> Option<i64> {
    let v = v?;
    (v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0).then_some(v as i64)
}

fn contains_optional_chain(ast: &Ast, id: NodeId) -> bool {
    let mut stack = vec![id];
    let mut steps = 0u32;
    while let Some(n) = stack.pop() {
        steps += 1;
        if steps > 100_000 {
            return true;
        }
        match *ast.node(n) {
            Node::Member { optional: true, .. }
            | Node::Index { optional: true, .. }
            | Node::Call { optional: true, .. } => return true,
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    false
}

/// `obj.length`, non-optional.
fn is_length_of(cx: &Ctx<'_, '_>, id: NodeId, obj: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(skip_wrappers(ast, id)) {
        Node::Member {
            obj: o,
            optional: false,
            prop,
        } => {
            matches!(*ast.node(prop), Node::Ident { name } if ast.atom(name) == "length")
                && is_same_reference(cx, o, obj)
        }
        _ => false,
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Bound {
    Length,
    LengthMinusOne,
}

fn length_bound(cx: &Ctx<'_, '_>, id: NodeId, obj: NodeId) -> Option<Bound> {
    let ast = cx.ast();
    if is_length_of(cx, id, obj) {
        return Some(Bound::Length);
    }
    match *ast.node(skip_wrappers(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Sub,
            right,
        } if static_number(ast, right, 0) == Some(1.0) && is_length_of(cx, left, obj) => {
            Some(Bound::LengthMinusOne)
        }
        _ => None,
    }
}

struct Access {
    obj: NodeId,
    index: NodeId,
    offset: i64,
}

fn indexed_access(ast: &Ast, branch: NodeId) -> Option<Access> {
    let Node::Index {
        obj,
        optional: false,
        index,
    } = *ast.node(skip_wrappers(ast, branch))
    else {
        return None;
    };
    if contains_optional_chain(ast, skip_wrappers(ast, branch))
        || has_side_effect(ast, obj, true, false)
        || has_side_effect(ast, index, true, false)
    {
        return None;
    }
    let inner = skip_wrappers(ast, index);
    if let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *ast.node(inner)
        && let Some(offset) = safe_integer(static_number(ast, right, 0)).filter(|&o| o > 0)
    {
        return Some(Access {
            obj,
            index: left,
            offset,
        });
    }
    Some(Access {
        obj,
        index: inner,
        offset: 0,
    })
}

/// Whether the test guarantees the index is in bounds (`Some(true)`), out of bounds
/// (`Some(false)`), or neither.
fn test_validity(cx: &Ctx<'_, '_>, test: NodeId, access: &Access) -> Option<bool> {
    let ast = cx.ast();
    let test = skip_wrappers(ast, test);
    let Node::Binary { left, op, right } = *ast.node(test) else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq
    ) || contains_optional_chain(ast, test)
    {
        return None;
    }
    // Lower bound: `index >= 0`, `index > -1`, `0 <= index`...
    let lower = |number: i64, index_on_left: bool| -> Option<bool> {
        let (valid, addend) = match (index_on_left, op) {
            (true, BinaryOp::GtEq) | (false, BinaryOp::LtEq) => (true, 0),
            (true, BinaryOp::Gt) | (false, BinaryOp::Lt) => (true, -1),
            (true, BinaryOp::Lt) | (false, BinaryOp::Gt) => (false, 0),
            _ => (false, -1),
        };
        (number == access.offset + addend).then_some(valid)
    };
    let lower_result = if let Some(n) = safe_integer(static_number(ast, right, 0))
        && is_same_reference(cx, left, access.index)
    {
        lower(n, true)
    } else if let Some(n) = safe_integer(static_number(ast, left, 0))
        && is_same_reference(cx, right, access.index)
    {
        lower(n, false)
    } else {
        None
    };
    if lower_result.is_some() {
        return lower_result;
    }
    if access.offset != 0 {
        return None;
    }
    // Upper bound: `index < array.length`, `index <= array.length - 1`...
    let upper = |bound: Bound, index_on_left: bool| -> Option<bool> {
        let (valid, kind) = match (index_on_left, op) {
            (true, BinaryOp::Lt) | (false, BinaryOp::Gt) => (true, Bound::Length),
            (true, BinaryOp::LtEq) | (false, BinaryOp::GtEq) => (true, Bound::LengthMinusOne),
            (true, BinaryOp::GtEq) | (false, BinaryOp::LtEq) => (false, Bound::Length),
            _ => (false, Bound::LengthMinusOne),
        };
        (bound == kind).then_some(valid)
    };
    if let Some(b) = length_bound(cx, right, access.obj)
        && is_same_reference(cx, left, access.index)
    {
        return upper(b, true);
    }
    if let Some(b) = length_bound(cx, left, access.obj)
        && is_same_reference(cx, right, access.index)
    {
        return upper(b, false);
    }
    None
}

/// The value position calls, tags or deletes it, so it can't become a plain reference.
pub(crate) fn is_reference_context(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = id;
    loop {
        let Some(p) = parent_skip_parens(ast, cx.file.parents, cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            Node::Call { callee, .. } => return skip_parens(ast, callee) == cur,
            Node::TaggedTemplate { tag, .. } => return skip_parens(ast, tag) == cur,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            _ => return false,
        }
    }
}

/// End of the `:` before a type annotation that starts at `ty_start`.
fn colon_before(cx: &Ctx<'_, '_>, ty_start: u32) -> Option<u32> {
    let at = skip_trivia_backward(cx.ast(), cx.src(), ty_start as usize);
    (at > 0 && cx.src().as_bytes()[at - 1] == b':').then(|| at as u32 - 1)
}

impl NoUselessUndefined {
    fn identifier(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, node) else {
            return;
        };
        let is = |child: Option<NodeId>| child.map(|c| skip_parens(ast, c)) == Some(node);
        let span = cx.span(node);
        match *ast.node(parent) {
            Node::Return { arg } if is(arg) => {
                if let Some(f) = enclosing_function(cx, node)
                    && let Some(rt) = return_type(ast, f)
                    && !matches!(
                        ast.node(rt),
                        Node::KeywordType {
                            kind: KeywordTypeKind::Undefined | KeywordTypeKind::Void
                        }
                    )
                {
                    return;
                }
                let fix = fix_of(replace_with_spaces_before(cx, node, ""));
                cx.report(span, MESSAGE).id(ID).fix(fix);
            }
            Node::Yield {
                delegate: false,
                arg,
            } if is(arg) => {
                let fix = fix_of(replace_with_spaces_before(cx, node, ""));
                cx.report(span, MESSAGE).id(ID).fix(fix);
            }
            Node::Arrow { flags, body, .. }
                if self.check_arrow_body
                    && flags.contains(FnFlags::EXPRESSION_BODY)
                    && is(Some(body)) =>
            {
                if return_type(ast, parent).is_some() {
                    return;
                }
                let fix = fix_of(replace_with_spaces_before(cx, node, " {}"));
                cx.report(span, MESSAGE).id(ID).fix(fix);
            }
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } if is(init) => {
                let Some(decl) = cx.parent(parent) else {
                    return;
                };
                if !matches!(
                    ast.node(decl),
                    Node::VarDecl {
                        kind: VarKind::Let,
                        ..
                    }
                ) || !matches!(ast.node(name), Node::Ident { .. })
                {
                    return;
                }
                let start = cx.span(type_ann.unwrap_or(name)).end;
                let end = super::unicorn_util::parenthesized_span(cx, node).end;
                let range = Span::new(start, end);
                let fix = (!has_comment(cx, range)).then(|| Fix::replace(range, ""));
                cx.report(span, MESSAGE).id(ID).fix_opt(fix);
            }
            Node::Param {
                modifiers,
                pattern,
                optional,
                type_ann,
                init,
                ..
            } if is(init) => {
                let left_end = match type_ann {
                    Some(t) => cx.span(t).end,
                    None if optional => {
                        let at = skip_trivia_forward(cx.src(), cx.span(pattern).end as usize);
                        if cx.src().as_bytes().get(at) == Some(&b'?') {
                            at as u32 + 1
                        } else {
                            cx.span(pattern).end
                        }
                    }
                    None => cx.span(pattern).end,
                };
                let end = super::unicorn_util::parenthesized_span(cx, node).end;
                let range = Span::new(left_end, end);
                if has_comment(cx, range) {
                    cx.report(span, MESSAGE).id(ID);
                    return;
                }
                let mut edits = vec![(range, String::new())];
                let is_property = modifiers.intersects(
                    ts_r_ast::Modifiers::PUBLIC
                        | ts_r_ast::Modifiers::PRIVATE
                        | ts_r_ast::Modifiers::PROTECTED
                        | ts_r_ast::Modifiers::READONLY
                        | ts_r_ast::Modifiers::OVERRIDE,
                );
                if !optional && !is_property {
                    if let Some(t) = type_ann {
                        if let Some(colon) = colon_before(cx, cx.span(t).start) {
                            edits.push((Span::new(colon, colon), "?".into()));
                        }
                    } else if is_typescript_path(cx.file.path) {
                        let at = cx.span(pattern).end;
                        edits.push((Span::new(at, at), "?".into()));
                    }
                }
                cx.report(span, MESSAGE).id(ID).fix(fix_of(edits));
            }
            Node::BindingProperty {
                key, value, init, ..
            } if is(init) => self.drop_default(cx, node, cx.span(value.unwrap_or(key)).end),
            Node::ShorthandProp {
                name,
                init: Some(init),
                ..
            } if skip_parens(ast, init) == node => self.drop_default(cx, node, cx.span(name).end),
            Node::AssignPattern { left, right } if skip_parens(ast, right) == node => {
                self.drop_default(cx, node, cx.span(left).end);
            }
            _ => {}
        }
    }

    fn drop_default(&self, cx: &mut Ctx<'_, '_>, node: NodeId, left_end: u32) {
        let end = super::unicorn_util::parenthesized_span(cx, node).end;
        let range = Span::new(left_end, end);
        let fix = (!has_comment(cx, range)).then(|| Fix::replace(range, ""));
        let span = cx.span(node);
        cx.report(span, MESSAGE).id(ID).fix_opt(fix);
    }

    fn conditional(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Conditional { test, cons, alt } = *ast.node(node) else {
            return;
        };
        let (cons, alt) = (skip_parens(ast, cons), skip_parens(ast, alt));
        let cons_undefined = is_undefined(ast, cons);
        if cons_undefined == is_undefined(ast, alt) {
            return;
        }
        let (access_node, undefined_node) = if cons_undefined {
            (alt, cons)
        } else {
            (cons, alt)
        };
        let Some(access) = indexed_access(ast, access_node) else {
            return;
        };
        if test_validity(cx, test, &access) != Some(!cons_undefined) {
            return;
        }
        let span = cx.span(node);
        let suggestion = (!is_reference_context(cx, node) && !has_comment(cx, span)).then(|| {
            let text = cx.text(access_node).to_owned();
            let semi = if needs_semicolon(cx, span.start, &text) {
                ";"
            } else {
                ""
            };
            let mut edits = vec![(span, format!("{semi}{text}"))];
            edits.extend(space_around_keyword(cx, node));
            fix_of(edits)
        });
        let report = cx.report(cx.span(undefined_node), MESSAGE).id(ID);
        if let Some(fix) = suggestion {
            report.suggest(
                "no-useless-undefined/suggestion",
                "Use the indexed access directly.",
                fix,
            );
        }
    }

    fn call(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional,
            args,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let args = ast.list(args);
        let Some(&last) = args.last() else {
            return;
        };
        let last_inner = skip_parens(ast, last);
        let callee = skip_parens(ast, callee);
        if !is_undefined(ast, last_inner) || is_ignored_callee(ast, callee) {
            return;
        }
        let is_bind = !optional
            && matches!(*ast.node(callee), Node::Member { prop, .. }
                if matches!(*ast.node(prop), Node::Ident { name } if ast.atom(name) == "bind"));
        if is_bind && args.len() != 1 {
            return;
        }
        // Only a plain removal: with a comment in the removed range there is no fix.
        let fix = remove_argument(cx, node, args.len() - 1)
            .filter(|f| f.edits.iter().all(|e| e.text.is_empty()));
        let span = cx.span(last_inner);
        cx.report(span, MESSAGE).id(ID).fix_opt(fix);
    }
}

impl Rule for NoUselessUndefined {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Ident, NodeKind::Conditional, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.node(node) {
            Node::Ident { .. } => {
                if is_undefined(cx.ast(), node) {
                    self.identifier(node, cx);
                }
            }
            Node::Conditional { .. } => self.conditional(node, cx),
            Node::Call { .. } => {
                if self.check_arguments && !is_typescript_path(cx.file.path) {
                    self.call(node, cx);
                }
            }
            _ => {}
        }
    }
}
