//! `unicorn/prefer-await`: a call of `.then()`, `.catch()` or `.finally()` (also a static
//! computed name such as `["then"]` or a `const` holding one) is reported on its property,
//! unless the chain's value is discarded with `void`. Without type information every
//! receiver counts.
//!
//! A statement `x.then(arrow)` starting its line, with one plain-name (or no) parameter
//! and no return type, type parameters, directives, `await` identifiers (outside a JS
//! module or in TS), multi-line strings, templates, JSX text or comments in an expression
//! body, comments outside the pieces, an optional chain, `super`, a suspension point or
//! another promise method in `x`, and no binding of the callback that `x` refers to, gets
//! the suggestion `void (async () => { const v = await x; ...body })();` (`let` when the
//! parameter is reassigned; an expression body becomes `return ...;`). The body's
//! indentation comes from its first line, else one level deeper than the closest enclosing
//! line-starting node, else the file's indent unit.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_conditions::{
    has_optional_chain_element, idents_in, indent_if_line_start, is_function_node, reindent_text,
};
use super::unicorn_prefer_spread::is_ts_file;
use super::unicorn_static::static_value;
use super::unicorn_text::{paren_span, paren_text};
use super::unicorn_util::indent_before;
use super::unicorn_util::indent_unit;
use crate::env::SourceType;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-await",
        "Prefer `await` over promise chaining.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferAwait>,
};

#[derive(Debug, Default)]
struct PreferAwait;

const METHODS: &[&str] = &["then", "catch", "finally"];

fn is_ts_wrapper(node: &Node) -> bool {
    matches!(
        node,
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// `getPropertyName` of a member callee.
fn property_name(cx: &Ctx<'_, '_>, callee: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(callee) {
        Node::Member { prop, .. } => match ast.node(prop) {
            Node::Ident { .. } => ident_name(ast, prop).map(str::to_owned),
            _ => None,
        },
        Node::Index { index, .. } => static_value(cx, index)?.to_js_string(),
        _ => None,
    }
}

fn discarded_with_void(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let mut expr = call;
    for _ in 0..100_000 {
        let Some(parent) = parent_skip_parens(ast, cx.file.parents, expr) else {
            return false;
        };
        match *ast.node(parent) {
            Node::Member { obj, .. } | Node::Index { obj, .. } if skip_parens(ast, obj) == expr => {
                match parent_skip_parens(ast, cx.file.parents, parent) {
                    Some(g) if matches!(*ast.node(g), Node::Call { callee, .. } if skip_parens(ast, callee) == parent) =>
                    {
                        expr = g;
                    }
                    _ => return false,
                }
            }
            ref n if is_ts_wrapper(n) => expr = parent,
            Node::Unary {
                op: UnaryOp::Void, ..
            } => return true,
            _ => return false,
        }
    }
    false
}

fn contains(
    ast: &Ast,
    root: NodeId,
    enter_functions: bool,
    test: impl Fn(&Ast, NodeId) -> bool,
) -> bool {
    struct Find<F> {
        test: F,
        enter_functions: bool,
        found: bool,
    }
    impl<F: Fn(&Ast, NodeId) -> bool> Visitor for Find<F> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if (self.test)(ast, id) {
                self.found = true;
                return Walk::Stop;
            }
            if !self.enter_functions && is_function_node(ast.node(id)) {
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        test,
        enter_functions,
        found: false,
    };
    walk(ast, root, &mut f);
    f.found
}

fn contains_suspension_point(ast: &Ast, root: NodeId) -> bool {
    contains(ast, root, false, |ast, id| {
        matches!(
            ast.node(id),
            Node::Await { .. } | Node::Yield { .. } | Node::ForOf { is_await: true, .. }
        )
    })
}

fn contains_await_identifier(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let can_be_identifier = !matches!(cx.env().source_type, SourceType::Module) || is_ts_file(cx);
    can_be_identifier
        && contains(cx.ast(), root, true, |ast, id| {
            matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some("await")
        })
}

fn has_promise_method_call_in_chain(cx: &Ctx<'_, '_>, mut id: NodeId) -> bool {
    let ast = cx.ast();
    for _ in 0..100_000 {
        id = super::unicorn_conditions::unwrap_ts_and_parens(ast, id);
        match *ast.node(id) {
            Node::Call { callee, .. } => {
                let callee = skip_parens(ast, callee);
                if property_name(cx, callee).is_some_and(|n| METHODS.contains(&n.as_str())) {
                    return true;
                }
                id = callee;
            }
            Node::Member { obj, .. } | Node::Index { obj, .. } => id = obj,
            _ => return false,
        }
    }
    false
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn multiline(cx: &Ctx<'_, '_>, span: Span) -> bool {
    span.text(cx.src()).contains(is_line_break)
}

fn has_multiline_whitespace_sensitive_content(cx: &Ctx<'_, '_>, body: NodeId) -> bool {
    let ast = cx.ast();
    contains(ast, body, true, |ast, id| {
        matches!(
            ast.node(id),
            Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText {}
        ) && multiline(cx, cx.span(id))
    }) || ast
        .comments_in(cx.span(body))
        .iter()
        .any(|c| multiline(cx, c.span))
}

/// The callback declares a name the promise expression refers to.
fn has_binding_conflict(cx: &Ctx<'_, '_>, object: NodeId, callback: NodeId, body: NodeId) -> bool {
    let sem = cx.semantic();
    let ast = cx.ast();
    let names: Vec<&str> = idents_in(ast, object)
        .into_iter()
        .filter(|&i| sem.reference_at(i).is_some())
        .filter_map(|i| ident_name(ast, i))
        .collect();
    [sem.scope_of_node(callback), sem.scope_of_node(body)]
        .into_iter()
        .flatten()
        .any(|scope| {
            sem.scope(scope)
                .symbols
                .iter()
                .any(|&s| names.contains(&sem.name(s)))
        })
}

fn child_indent(cx: &Ctx<'_, '_>, call: NodeId, body: NodeId, indent: &str) -> Option<String> {
    let ast = cx.ast();
    let src = cx.src();
    let call_line = cx.file.lines.line_of(cx.span(call).start);
    if let Node::Block { .. } = ast.node(body) {
        let span = cx.span(body);
        let open = span.start;
        let close = span.end - 1;
        let first = {
            let mut pos = open + 1;
            let bytes = src.as_bytes();
            while (pos as usize) < close as usize
                && (bytes[pos as usize] as char).is_ascii_whitespace()
            {
                pos += 1;
            }
            pos
        };
        if first < close {
            let open_line = cx.file.lines.line_of(open);
            if cx.file.lines.line_of(first) > open_line {
                return Some(indent_before(cx, first).to_owned());
            }
            if cx.file.lines.line_of(close) > open_line {
                return None;
            }
        }
    } else {
        let start = paren_span(cx, body).start;
        if let Some(range_indent) = indent_if_line_start(src, start as usize)
            && range_indent.starts_with(indent)
            && range_indent.len() > indent.len()
        {
            return Some(range_indent.to_owned());
        }
        if cx.file.lines.line_of(cx.span(body).start) > call_line {
            return Some(indent_before(cx, cx.span(body).start).to_owned());
        }
    }
    let stmt = cx.parent(call)?;
    let mut ancestor = cx.parent(stmt);
    for _ in 0..100_000 {
        let Some(a) = ancestor else {
            break;
        };
        let start = cx.span(a).start;
        if indent_if_line_start(src, start as usize).is_some() {
            let ancestor_indent = indent_before(cx, start);
            if ancestor_indent.len() < indent.len() && indent.starts_with(ancestor_indent) {
                return Some(format!("{indent}{}", &indent[ancestor_indent.len()..]));
            }
        }
        ancestor = cx.parent(a);
    }
    Some(format!("{indent}{}", indent_unit(cx)))
}

fn suggestion(cx: &Ctx<'_, '_>, call: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj: object,
        prop,
        optional: false,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let [callback] = ast.list(args) else {
        return None;
    };
    let callback = skip_parens(ast, *callback);
    if !matches!(ast.node(prop), Node::Ident { .. })
        || ident_name(ast, prop) != Some("then")
        || !parent_skip_parens(ast, cx.file.parents, call)
            .is_some_and(|p| matches!(ast.node(p), Node::ExprStmt { .. }))
        || matches!(ast.node(skip_parens(ast, object)), Node::Super {})
        || indent_if_line_start(src, cx.span(call).start as usize).is_none()
        || has_optional_chain_element(ast, object)
        || contains_await_identifier(cx, object)
        || contains_suspension_point(ast, object)
        || has_promise_method_call_in_chain(cx, object)
    {
        return None;
    }
    let Node::Arrow {
        params,
        body,
        type_params: None,
        return_type: None,
        ..
    } = *ast.node(callback)
    else {
        return None;
    };
    let params = ast.list(params);
    let param = match params {
        [] => None,
        [p] => {
            let Node::Param {
                pattern,
                optional: false,
                rest: false,
                init: None,
                ..
            } = *ast.node(*p)
            else {
                return None;
            };
            if !matches!(ast.node(pattern), Node::Ident { .. })
                || ident_name(ast, pattern) == Some("let")
            {
                return None;
            }
            Some((*p, pattern))
        }
        _ => return None,
    };
    let block = matches!(ast.node(body), Node::Block { .. });
    if contains_await_identifier(cx, callback)
        || (block && has_directive(ast, body))
        || (!block && has_multiline_whitespace_sensitive_content(cx, body))
    {
        return None;
    }
    let mut kept = vec![cx.span(object)];
    kept.extend(params.iter().map(|&p| cx.span(p)));
    kept.push(cx.span(body));
    if super::unicorn_text::would_remove_comments(cx, cx.span(call), &kept)
        || has_binding_conflict(cx, object, callback, body)
    {
        return None;
    }
    let awaited = paren_text(cx, object);
    let initial = match param {
        Some((p, name)) => {
            let sem = cx.semantic();
            let sym = sem.symbol_of_decl(name)?;
            let symbol = sem.symbol(sym);
            if symbol.decls.len() > 1 {
                return None;
            }
            let reassigned = symbol.refs.iter().any(|&r| sem.reference(r).is_write());
            let kind = if reassigned { "let" } else { "const" };
            format!("{kind} {} = await {awaited};", cx.text(p))
        }
        None => format!("await {awaited};"),
    };
    let indent = indent_before(cx, cx.span(call).start);
    let child = child_indent(cx, call, body, indent)?;
    let body_text = if block {
        let t = cx.text(body);
        t[1..t.len() - 1].trim().to_owned()
    } else {
        let range = paren_span(cx, body);
        let source_indent = indent_if_line_start(src, range.start as usize).unwrap_or(indent);
        format!(
            "return {};",
            reindent_text(range.text(src), source_indent, &child)
        )
    };
    let lb = linebreak(src);
    let mut lines = vec![
        "void (async () => {".to_owned(),
        format!("{child}{initial}"),
    ];
    if !body_text.is_empty() {
        lines.push(format!("{child}{body_text}"));
    }
    lines.push(format!("{indent}}})()"));
    Some(Fix::replace(cx.span(call), lines.join(lb)))
}

fn has_directive(ast: &Ast, body: NodeId) -> bool {
    let Node::Block { stmts } = *ast.node(body) else {
        return false;
    };
    ast.list(stmts)
        .iter()
        .any(|&s| matches!(ast.node(s), Node::Directive { .. }))
}

impl Rule for PreferAwait {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let prop = match *ast.node(callee) {
            Node::Member { prop, .. } => prop,
            Node::Index { index, .. } => index,
            _ => return,
        };
        let Some(method) = property_name(cx, callee).filter(|m| METHODS.contains(&m.as_str()))
        else {
            return;
        };
        if discarded_with_void(cx, node) {
            return;
        }
        let fix = suggestion(cx, node);
        let report = cx
            .report(
                cx.span(prop),
                format!("Prefer `await` over promise chaining with `.{method}()`."),
            )
            .id("prefer-await");
        if let Some(fix) = fix {
            report.suggest("prefer-await/suggestion", "Rewrite as an async IIFE.", fix);
        }
    }
}
