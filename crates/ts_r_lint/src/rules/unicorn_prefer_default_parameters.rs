//! `unicorn/prefer-default-parameters`: default parameters and destructuring defaults over
//! reassignment and fallback reads.
//!
//! - Reassignment: `x = x || v`, `x = x ?? v`, `x ||= v`, `x ??= v` as a statement directly
//!   in a function body (no `"use strict"`), `x` the function's last parameter (a plain
//!   identifier with a single declaration, not a TypeScript setter's), `v` a static literal
//!   (`-n` too) or an earlier binding of the same declaration that is never reassigned, no
//!   statement before it containing a call, `new`, `import()` or a tagged template, and the
//!   assignment the parameter's first reference. Suggestion: `x = v` as the parameter
//!   (keeping its type annotation; parentheses for a bare arrow parameter) and the statement
//!   removed (its line when alone on it, a `;` when ASI needs one).
//! - Fallback reads (`checkFallbackExpressions`, on by default): a function's last plain
//!   parameter, or a binding directly in a destructuring pattern of a parameter or a
//!   non-`var`, non-exported declarator, whose every non-initializing reference reads it
//!   as the left of `x ?? v` / `x || v`, all with the same operator and the same default (a
//!   static literal compared by value, or the same earlier binding). Suggestion: the default
//!   moves to the declaration and every fallback becomes `x`.
//!
//! Comments inside the binding, the statement or a fallback block the suggestion. Without
//! type information every default is treated as compatible.

use super::prelude::*;
use super::unicorn_util_h5::{needs_semicolon, parent_through_parens, static_string};
use crate::semantic::{DeclKind, SymbolId};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-default-parameters",
        "Prefer default parameters and destructuring defaults over reassignment and fallback expressions.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<PreferDefaultParameters>,
};

#[derive(Debug, Default)]
struct PreferDefaultParameters {
    check_fallback_expressions: bool,
}

impl FromOptions for PreferDefaultParameters {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferDefaultParameters {
            check_fallback_expressions: bool_field(o, "checkFallbackExpressions", true),
        })
    }
}

impl Rule for PreferDefaultParameters {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
            NodeKind::VarDeclarator,
            NodeKind::ExprStmt,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::ExprStmt { expr } => check_reassignment(cx, node, expr),
            Node::VarDeclarator { name, .. } => {
                if self.check_fallback_expressions {
                    let mut bindings = Vec::new();
                    collect_bindings(cx.ast(), name, &mut bindings, 0);
                    for b in bindings {
                        check_fallback_reads(cx, b, node, None);
                    }
                }
            }
            _ => {
                if self.check_fallback_expressions
                    && let Some((params, _)) = function_parts(cx.ast(), node)
                {
                    for &p in cx.ast().list(params) {
                        let Node::Param { pattern, .. } = *cx.node(p) else {
                            continue;
                        };
                        let mut bindings = Vec::new();
                        collect_bindings(cx.ast(), pattern, &mut bindings, 0);
                        for b in bindings {
                            check_fallback_reads(cx, b, node, Some(p));
                        }
                    }
                }
            }
        }
    }
}

fn function_parts(ast: &ts_r_ast::Ast, f: NodeId) -> Option<(ts_r_ast::NodeList, Option<NodeId>)> {
    match *ast.node(f) {
        Node::FunctionDecl { params, body, .. } | Node::FunctionExpr { params, body, .. } => {
            Some((params, body))
        }
        Node::Arrow { params, body, .. } => Some((params, Some(body))),
        _ => None,
    }
}

/// Every binding identifier in a pattern.
fn collect_bindings(ast: &ts_r_ast::Ast, id: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    if depth > 200 {
        return;
    }
    match *ast.node(id) {
        Node::Ident { .. } => out.push(id),
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => {
                        collect_bindings(ast, value.unwrap_or(key), out, depth + 1);
                    }
                    Node::Rest { arg } => collect_bindings(ast, arg, out, depth + 1),
                    _ => {}
                }
            }
        }
        Node::ArrayPattern { elements } => {
            for &e in ast.list(elements) {
                collect_bindings(ast, e, out, depth + 1);
            }
        }
        Node::AssignPattern { left, .. } => collect_bindings(ast, left, out, depth + 1),
        Node::Rest { arg } => collect_bindings(ast, arg, out, depth + 1),
        _ => {}
    }
}

fn has_use_strict(ast: &ts_r_ast::Ast, body: Option<NodeId>) -> bool {
    match body.map(|b| *ast.node(b)) {
        Some(Node::Block { stmts }) => ast.list(stmts).iter().any(|&s| {
            matches!(*ast.node(s), Node::Directive { value } if ast.atom(value) == "use strict")
        }),
        _ => false,
    }
}

fn is_typescript_setter(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    cx.file.env.typescript
        && cx.parent(f).is_some_and(|m| {
            matches!(
                cx.node(m),
                Node::MethodDef {
                    kind: MethodKind::Set,
                    ..
                }
            )
        })
}

/// A value compared with `Object.is`.
#[derive(PartialEq, Debug)]
enum StaticValue {
    Number(u64),
    BigInt(bool, String),
    Str(String),
    Bool(bool),
    Null,
    Regex,
}

fn bigint_value(text: &str) -> String {
    let t: String = text
        .trim_end_matches('n')
        .chars()
        .filter(|&c| c != '_')
        .collect();
    let (radix, digits) = match t.get(..2) {
        Some("0x" | "0X") => (16, &t[2..]),
        Some("0o" | "0O") => (8, &t[2..]),
        Some("0b" | "0B") => (2, &t[2..]),
        _ => (10, t.as_str()),
    };
    match u128::from_str_radix(digits, radix) {
        Ok(v) => v.to_string(),
        Err(_) => t.clone(),
    }
}

/// unicorn's `isStaticDefaultValue` plus its value.
fn static_default(cx: &Ctx<'_, '_>, id: NodeId) -> Option<StaticValue> {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if let Some(s) = static_string(ast, id) {
        return Some(StaticValue::Str(s.to_owned()));
    }
    match *ast.node(id) {
        Node::NumberLit { value } => Some(StaticValue::Number(normalize(value.get()))),
        Node::BigIntLit { .. } => Some(StaticValue::BigInt(false, bigint_value(cx.text(id)))),
        Node::BoolLit { value } => Some(StaticValue::Bool(value)),
        Node::NullLit {} => Some(StaticValue::Null),
        Node::RegexLit { .. } => Some(StaticValue::Regex),
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => match *ast.node(skip_parens(ast, arg)) {
            Node::NumberLit { value } => Some(StaticValue::Number(normalize(-value.get()))),
            Node::BigIntLit { .. } => {
                let v = bigint_value(cx.text(skip_parens(ast, arg)));
                Some(StaticValue::BigInt(v != "0", v))
            }
            _ => None,
        },
        _ => None,
    }
}

fn normalize(v: f64) -> u64 {
    if v.is_nan() {
        f64::NAN.to_bits()
    } else {
        v.to_bits()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefKind {
    Param,
    Variable,
    Other,
}

fn def_kind(kind: DeclKind) -> DefKind {
    match kind {
        DeclKind::Param => DefKind::Param,
        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => DefKind::Variable,
        _ => DefKind::Other,
    }
}

/// The node a declaration belongs to in eslint-scope's sense: the function for a
/// parameter, the declarator for a variable.
fn def_owner(cx: &Ctx<'_, '_>, decl: &crate::semantic::Decl) -> NodeId {
    if decl.kind == DeclKind::Param {
        cx.parent(decl.node).unwrap_or(decl.node)
    } else {
        decl.node
    }
}

/// unicorn's `isEarlierBinding`: `fallback` names another binding of the same declaration,
/// declared before `symbol` and never reassigned.
fn is_earlier_binding(cx: &Ctx<'_, '_>, fallback: NodeId, symbol: SymbolId) -> bool {
    let ast = cx.ast();
    let fallback = skip_parens(ast, fallback);
    if !matches!(ast.node(fallback), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let var = sem.symbol(symbol);
    let [decl] = var.decls.as_slice() else {
        return false;
    };
    let Some(other) = sem
        .reference_at(fallback)
        .and_then(|r| sem.reference(r).symbol())
    else {
        return false;
    };
    let other = sem.symbol(other);
    let [other_decl] = other.decls.as_slice() else {
        return false;
    };
    def_kind(other_decl.kind) == def_kind(decl.kind)
        && def_kind(decl.kind) != DefKind::Other
        && def_owner(cx, other_decl) == def_owner(cx, decl)
        && cx.span(other_decl.name).start < cx.span(decl.name).start
        && other.refs.iter().all(|&r| {
            let r = sem.reference(r);
            !r.is_write() || r.flags.contains(crate::semantic::RefFlags::INIT)
        })
}

/// The binding's span the way typescript-eslint gives it: a parameter identifier with its
/// `?` and type annotation.
fn binding_span(cx: &Ctx<'_, '_>, binding: NodeId) -> Span {
    let span = cx.span(binding);
    match cx.parent(binding).map(|p| *cx.node(p)) {
        Some(Node::Param {
            type_ann: Some(t), ..
        }) => Span::new(span.start, cx.span(t).end),
        Some(Node::Param { optional: true, .. }) => {
            let rest = &cx.src()[span.end as usize..];
            let q = rest.find('?').map_or(0, |i| i as u32 + 1);
            Span::new(span.start, span.end + q)
        }
        _ => span,
    }
}

/// `: Type` of a parameter, from its colon.
fn annotation_text<'a>(cx: &Ctx<'a, '_>, binding: NodeId) -> &'a str {
    let Some(Node::Param {
        type_ann: Some(t), ..
    }) = cx.parent(binding).map(|p| *cx.node(p))
    else {
        return "";
    };
    let src = cx.src();
    let start = cx.span(binding).end as usize;
    let end = cx.span(t).start as usize;
    let colon = src[start..end].rfind(':').map_or(end, |i| start + i);
    &src[colon..cx.span(t).end as usize]
}

/// An arrow whose single parameter isn't written in parentheses.
fn needs_parentheses(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Arrow { params, .. } = *ast.node(f) else {
        return false;
    };
    let [param] = *ast.list(params) else {
        return false;
    };
    let src = cx.src().as_bytes();
    let mut p = cx.span(param).start as usize;
    let comments = ast.comments_in(Span::new(cx.span(f).start, p as u32));
    for _ in 0..10_000 {
        while p > 0 && src[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        match comments.iter().find(|c| c.span.end as usize == p) {
            Some(c) => p = c.span.start as usize,
            None => break,
        }
    }
    p == 0 || src[p - 1] != b'('
}

fn has_comment_inside(cx: &Ctx<'_, '_>, span: Span) -> bool {
    cx.ast()
        .comments_in(span)
        .iter()
        .any(|c| c.span.start >= span.start && c.span.end <= span.end)
}

fn enclosing_function(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = node;
    for _ in 0..100_000 {
        cur = cx.parent(cur)?;
        if function_parts(cx.ast(), cur).is_some() {
            return Some(cur);
        }
    }
    None
}

fn is_destructuring_binding(cx: &Ctx<'_, '_>, binding: NodeId) -> bool {
    matches!(
        cx.parent(binding).map(|p| *cx.node(p)),
        Some(Node::ArrayPattern { .. } | Node::BindingProperty { init: None, .. })
    )
}

/// The fallback reads of `binding` (declared by `owner`; `param` when it is a parameter's).
fn check_fallback_reads(
    cx: &mut Ctx<'_, '_>,
    binding: NodeId,
    owner: NodeId,
    param: Option<NodeId>,
) {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Some(symbol) = sem.symbol_of_decl(binding) else {
        return;
    };
    let var = sem.symbol(symbol);
    let [decl] = var.decls.as_slice() else {
        return;
    };
    if def_owner(cx, decl) != owner {
        return;
    }
    match def_kind(decl.kind) {
        DefKind::Param => {}
        DefKind::Variable => {
            let Some(var_decl) = cx.parent(owner) else {
                return;
            };
            if matches!(
                cx.node(var_decl),
                Node::VarDecl {
                    kind: VarKind::Var,
                    ..
                }
            ) || cx
                .parent(var_decl)
                .is_some_and(|e| matches!(cx.node(e), Node::ExportNamed { .. }))
                || matches!(*cx.node(var_decl), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT))
            {
                return;
            }
        }
        DefKind::Other => return,
    }
    let destructuring = is_destructuring_binding(cx, binding);
    if !destructuring {
        let Some(param) = param else {
            return;
        };
        let Some((params, body)) = function_parts(ast, owner) else {
            return;
        };
        let last_ok = ast.list(params).last() == Some(&param)
            && matches!(*ast.node(param), Node::Param { pattern, init: None, rest: false, modifiers, .. }
                if pattern == binding && modifiers.is_empty());
        if !last_ok || has_use_strict(ast, body) || is_typescript_setter(cx, owner) {
            return;
        }
    }
    let refs: Vec<_> = var
        .refs
        .iter()
        .map(|&r| *sem.reference(r))
        .filter(|r| !r.flags.contains(crate::semantic::RefFlags::INIT))
        .collect();
    if refs.is_empty() || refs.iter().any(|r| r.is_write()) {
        return;
    }
    let mut expressions = Vec::with_capacity(refs.len());
    let mut first: Option<(BinaryOp, NodeId)> = None;
    for r in &refs {
        let Some((expr, child)) = parent_through_parens(cx, r.node) else {
            return;
        };
        let Node::Binary { left, op, right } = *ast.node(expr) else {
            return;
        };
        if !matches!(op, BinaryOp::LogicalOr | BinaryOp::Coalesce) || left != child {
            return;
        }
        let static_value = static_default(cx, right);
        if static_value.is_none() && !is_earlier_binding(cx, right, symbol) {
            return;
        }
        if static_value == Some(StaticValue::Regex) {
            return;
        }
        let right_inner = skip_parens(ast, right);
        match first {
            None => first = Some((op, right_inner)),
            Some((first_op, first_right)) => {
                let both_ident = matches!(ast.node(right_inner), Node::Ident { .. });
                if op != first_op
                    || both_ident != matches!(ast.node(first_right), Node::Ident { .. })
                {
                    return;
                }
                if both_ident {
                    if ident_name(ast, right_inner) != ident_name(ast, first_right) {
                        return;
                    }
                } else if static_value != static_default(cx, first_right) {
                    return;
                }
            }
        }
        expressions.push((expr, left));
    }
    let Some((_, first_right)) = first else {
        return;
    };
    let span = binding_span(cx, binding);
    let blocked = has_comment_inside(cx, span)
        || expressions
            .iter()
            .any(|&(e, _)| has_comment_inside(cx, cx.span(e)));
    let fix = (!blocked).then(|| {
        let name = cx.text(binding);
        let mut replacement = format!(
            "{name}{} = {}",
            annotation_text(cx, binding),
            cx.text(first_right)
        );
        if !destructuring && needs_parentheses(cx, owner) {
            replacement = format!("({replacement})");
        }
        let mut fix = Fix::replace(span, replacement);
        for &(e, left) in &expressions {
            fix = fix.and(cx.span(e), cx.text(skip_parens(ast, left)));
        }
        fix
    });
    let (id, message) = if destructuring {
        (
            "preferDestructuringDefaultOverFallback",
            "Prefer a destructuring default over fallback expressions.",
        )
    } else {
        (
            "preferDefaultParameterOverFallback",
            "Prefer a default parameter over fallback expressions.",
        )
    };
    let r = cx.report(span, message).id(id);
    if let Some(fix) = fix {
        r.suggest(
            "moveDefaultToDeclaration",
            "Move the default value to the declaration. This changes fallback behavior to apply only to undefined.",
            fix,
        );
    }
}

/// `x = x || v` / `x = x ?? v` / `x ||= v` / `x ??= v`: the target and the default.
fn default_value(ast: &ts_r_ast::Ast, expr: NodeId) -> Option<(NodeId, NodeId)> {
    let Node::Assign { left, op, right } = *ast.node(expr) else {
        return None;
    };
    let name = ident_name(ast, left)?;
    match op {
        AssignOp::Assign => {
            let Node::Binary {
                left: l,
                op: BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right: r,
            } = *ast.node(skip_parens(ast, right))
            else {
                return None;
            };
            (ident_name(ast, skip_parens(ast, l)) == Some(name)).then_some((left, r))
        }
        AssignOp::OrAssign | AssignOp::CoalesceAssign => Some((left, right)),
        _ => None,
    }
}

fn contains_call(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return true;
    }
    if matches!(
        ast.node(id),
        Node::Call { .. }
            | Node::New { .. }
            | Node::ImportCall { .. }
            | Node::TaggedTemplate { .. }
    ) {
        return true;
    }
    let mut found = false;
    ast.for_each_child(id, |c| found = found || contains_call(ast, c, depth + 1));
    found
}

fn check_reassignment(cx: &mut Ctx<'_, '_>, stmt: NodeId, expr: NodeId) {
    let ast = cx.ast();
    let Some((left, default)) = default_value(ast, expr) else {
        return;
    };
    let Some(f) = enclosing_function(cx, stmt) else {
        return;
    };
    let Some((params, Some(body))) = function_parts(ast, f) else {
        return;
    };
    if cx.parent(stmt) != Some(body) || has_use_strict(ast, Some(body)) {
        return;
    }
    let Node::Block { stmts } = *ast.node(body) else {
        return;
    };
    let sem = cx.semantic();
    let Some(symbol) = sem
        .reference_at(left)
        .and_then(|r| sem.reference(r).symbol())
    else {
        return;
    };
    let var = sem.symbol(symbol);
    let Some(&param) = ast.list(params).last() else {
        return;
    };
    let Node::Param {
        pattern,
        init: None,
        rest: false,
        modifiers,
        ..
    } = *ast.node(param)
    else {
        return;
    };
    if !modifiers.is_empty()
        || ident_name(ast, pattern) != ident_name(ast, left)
        || var.decls.len() != 1
        || var.decls[0].name != pattern
        || (static_default(cx, default).is_none() && !is_earlier_binding(cx, default, symbol))
        || is_typescript_setter(cx, f)
    {
        return;
    }
    for &s in ast.list(stmts) {
        if s == stmt {
            break;
        }
        if contains_call(ast, s, 0) {
            return;
        }
    }
    if var.refs.first().map(|&r| sem.reference(r).node) != Some(left) {
        return;
    }
    let name = cx.text(pattern);
    let mut replacement = format!(
        "{name}{} = {}",
        annotation_text(cx, pattern),
        cx.text(skip_parens(ast, default))
    );
    if needs_parentheses(cx, f) {
        replacement = format!("({replacement})");
    }
    let param_span = binding_span(cx, pattern);
    let stmt_span = cx.span(stmt);
    let blocked = has_comment_inside(cx, stmt_span) || has_comment_inside(cx, param_span);
    let fix = (!blocked).then(|| {
        let fix = Fix::replace(param_span, replacement);
        if needs_semicolon(cx, stmt_span.start, "(") {
            fix.and(stmt_span, ";")
        } else {
            fix.and(statement_removal(cx, stmt_span), "")
        }
    });
    let r = cx
        .report_node(stmt, "Prefer default parameters over reassignment.")
        .id("preferDefaultParameters");
    if let Some(fix) = fix {
        r.suggest(
            "preferDefaultParametersSuggest",
            "Replace reassignment with a default parameter. This changes fallback behavior to apply only to undefined.",
            fix,
        );
    }
}

/// The range removed with the statement: its whole line when nothing else is on it, else
/// the statement and one following space.
fn statement_removal(cx: &Ctx<'_, '_>, stmt: Span) -> Span {
    let src = cx.src();
    let lines = cx.file.lines;
    let line = lines.line_of(stmt.start);
    if let Some(line_span) = lines.line_span(line) {
        let text = &src[line_span.start as usize..line_span.end as usize];
        if text.trim() == &src[stmt.start as usize..stmt.end as usize] {
            return line_span;
        }
    }
    if src.as_bytes().get(stmt.end as usize) == Some(&b' ') {
        Span::new(stmt.start, stmt.end + 1)
    } else {
        stmt
    }
}
