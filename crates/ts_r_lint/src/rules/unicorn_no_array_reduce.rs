//! `unicorn/no-array-reduce`: `x.reduce(fn[, init])` and `x.reduceRight(…)` (also
//! `[].reduce.call(…)`, `Array.prototype.reduceRight.apply(…)`) are reported at the method
//! name. A first argument that can't be a function (literal, `await`, a call other than
//! `.bind()`...) is left alone, as are receivers known not to be arrays, and, with
//! `allowSimpleOperations` (the default), callbacks whose result is one binary expression.
//!
//! `(a, b) => a + b` (no initial value or `0`, no optional chain, no comments, operands not
//! known to be non-numbers, not a BigInt typed array) gets a `Math.sumPrecise(x)`
//! suggestion.
//!
//! `const r = arr.reduce(cb[, init]);`, alone in its declaration at the top level or in a
//! block, is rewritten into a `let r` plus a `for (const [index, element] of
//! arr.entries())` loop that skips holes (and, without an initial value, seeds from the
//! first element through an `isFirst` flag), when `arr` is a `const` declared before it and
//! not used in the callback, `r` is never reassigned, the initial value has no side
//! effects, and the callback is an inline function (or a `const` one declared earlier)
//! whose single returned expression can be inlined with its parameters renamed.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_static::has_side_effect as static_has_side_effect;
use super::unicorn_text::paren_text;
use super::unicorn_types::{
    is_known_bigint_typed_array, is_known_non_number, should_skip_known_non_array_receiver,
};
use super::unicorn_util::is_same_reference;
use super::unicorn_util::{available_name, trailing_comment_same_line};
use super::unicorn_util::{has_optional_chain, skip_wrappers};
use crate::semantic::{DeclKind, RefFlags, ScopeId, SymbolId};
use ts_r_ast::{Ast, BinaryOp, FnFlags, KeywordTypeKind, MethodKind, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-reduce",
        "Disallow `Array#reduce()` and `Array#reduceRight()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoArrayReduce>,
};

#[derive(Debug)]
struct NoArrayReduce {
    allow_simple: bool,
}

impl FromOptions for NoArrayReduce {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut allow_simple = true;
        if let Some(o) = opts.first().and_then(Value::as_object)
            && let Some(v) = o.get("allowSimpleOperations")
        {
            allow_simple = v
                .as_bool()
                .ok_or("`allowSimpleOperations` must be a boolean")?;
        }
        Ok(NoArrayReduce { allow_simple })
    }
}

fn message(method: &str) -> &'static str {
    if method == "reduce" {
        "`Array#reduce()` is not allowed. Prefer other types of loop for readability."
    } else {
        "`Array#reduceRight()` is not allowed. Prefer other types of loop for readability. You may want to call `Array#toReversed()` before looping it."
    }
}

fn is_inline_fn(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Arrow { .. } | Node::FunctionExpr { .. })
}

/// `obj.method(...)` (non-computed). Returns (object, property, optional call, optional
/// member, arguments) when no argument is a spread.
fn member_call(ast: &Ast, call: NodeId) -> Option<(NodeId, NodeId, bool, bool, &[NodeId])> {
    let Node::Call {
        callee,
        optional,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: member_optional,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    if args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some((skip_parens(ast, obj), prop, optional, member_optional, args))
}

/// `isNodeValueNotFunction`.
fn is_not_function(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::ArrayLit { .. }
        | Node::ClassExpr { .. }
        | Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::StringLit { .. }
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Assign { .. }
        | Node::Await { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::This {} => true,
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        Node::Ident { name } => ast.atom(name) == "undefined",
        Node::Call { .. } => !member_call(ast, id)
            .is_some_and(|(_, p, oc, om, _)| !oc && !om && ident_name(ast, p) == Some("bind")),
        _ => false,
    }
}

/// `getFunctionReturnExpression`: a concise arrow body, or the argument of a body that is
/// only `return x;` (parentheses skipped).
fn return_expression(ast: &Ast, f: NodeId) -> Option<NodeId> {
    let body = match *ast.node(f) {
        Node::Arrow { flags, body, .. } => {
            if flags.contains(FnFlags::EXPRESSION_BODY) {
                return Some(skip_parens(ast, body));
            }
            body
        }
        Node::FunctionExpr { body, .. } => body?,
        _ => return None,
    };
    let Node::Block { stmts } = *ast.node(body) else {
        return None;
    };
    match *ast.list(stmts) {
        [s] => match *ast.node(s) {
            Node::Return { arg } => arg.map(|a| skip_parens(ast, a)),
            _ => None,
        },
        _ => None,
    }
}

fn fn_params(ast: &Ast, f: NodeId) -> &[NodeId] {
    match *ast.node(f) {
        Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => ast.list(params),
        _ => &[],
    }
}

fn fn_flags(ast: &Ast, f: NodeId) -> FnFlags {
    match *ast.node(f) {
        Node::Arrow { flags, .. } | Node::FunctionExpr { flags, .. } => flags,
        _ => FnFlags::default(),
    }
}

/// A plain `name` parameter (no default or rest).
fn param_ident(ast: &Ast, p: NodeId) -> Option<NodeId> {
    match *ast.node(p) {
        Node::Param {
            rest: false,
            pattern,
            init: None,
            ..
        } if matches!(ast.node(pattern), Node::Ident { .. }) => Some(pattern),
        _ => None,
    }
}

/// `[].prop` or `Array.prototype.prop`, `prop` in `props`.
fn is_array_prototype_property(ast: &Ast, id: NodeId, props: &[&str]) -> bool {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(id)
    else {
        return false;
    };
    if !ident_name(ast, prop).is_some_and(|p| props.contains(&p)) {
        return false;
    }
    match *ast.node(skip_parens(ast, obj)) {
        Node::Member {
            obj: o,
            optional: false,
            prop: p,
        } => {
            ident_name(ast, skip_parens(ast, o)) == Some("Array")
                && ident_name(ast, p) == Some("prototype")
        }
        Node::ArrayLit { elements } => ast.list(elements).is_empty(),
        _ => false,
    }
}

/// A parameter whose type annotation can't be a number (`string`, `bigint`, `T[]`, a union
/// of those...).
fn annotated_non_number(ast: &Ast, param: NodeId) -> bool {
    fn non_number(ast: &Ast, ty: NodeId, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        match *ast.node(ty) {
            Node::KeywordType { kind } => !matches!(
                kind,
                KeywordTypeKind::Number
                    | KeywordTypeKind::Any
                    | KeywordTypeKind::Unknown
                    | KeywordTypeKind::Never
                    | KeywordTypeKind::Intrinsic
            ),
            Node::StringLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::TemplateLitType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::TypeLiteral { .. }
            | Node::FunctionType { .. } => true,
            Node::UnionType { types } => {
                let types = ast.list(types);
                !types.is_empty() && types.iter().all(|&t| non_number(ast, t, depth + 1))
            }
            _ => false,
        }
    }
    matches!(*ast.node(param), Node::Param { type_ann: Some(t), .. } if non_number(ast, t, 0))
}

fn sum_precise_suggestion(cx: &Ctx<'_, '_>, call: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let (object, _, optional, member_optional, args) = member_call(ast, call)?;
    let callback = skip_parens(ast, *args.first()?);
    if !is_inline_fn(ast, callback) {
        return None;
    }
    let params = fn_params(ast, callback);
    if params.len() < 2 {
        return None;
    }
    let (a, b) = (param_ident(ast, params[0])?, param_ident(ast, params[1])?);
    let expr = return_expression(ast, callback)?;
    let Node::Binary {
        left,
        op: BinaryOp::Add,
        right,
    } = *ast.node(expr)
    else {
        return None;
    };
    let (l, r) = (
        ident_name(ast, skip_parens(ast, left))?,
        ident_name(ast, skip_parens(ast, right))?,
    );
    let (na, nb) = (ident_name(ast, a)?, ident_name(ast, b)?);
    if l == r || !((l == na && r == nb) || (l == nb && r == na)) {
        return None;
    }
    let initial_ok = args.get(1).is_none_or(|&i| {
        matches!(*ast.node(skip_parens(ast, i)), Node::NumberLit { value } if value.get() == 0.0)
    });
    if fn_flags(ast, callback).intersects(FnFlags::ASYNC | FnFlags::GENERATOR)
        || has_optional_chain(ast, object)
        || optional
        || member_optional
        || !initial_ok
        || !ast.comments_in(cx.span(call)).is_empty()
        || [params[0], params[1]]
            .iter()
            .zip([a, b])
            .any(|(&p, ident)| is_known_non_number(cx, ident) || annotated_non_number(ast, p))
        || is_known_bigint_typed_array(cx, object)
    {
        return None;
    }
    Some(Fix::replace(
        cx.span(call),
        format!("Math.sumPrecise({})", paren_text(cx, object)),
    ))
}

fn decl_symbol(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    cx.semantic().symbol_of_decl(ident)
}

fn ref_symbol(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    sem.reference_at(ident)
        .and_then(|r| sem.reference(r).symbol())
}

fn inside(inner: Span, outer: Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

fn has_write_inside(cx: &Ctx<'_, '_>, sym: SymbolId, span: Option<Span>) -> bool {
    let sem = cx.semantic();
    sem.symbol(sym).refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write()
            && !r.flags.contains(RefFlags::INIT)
            && span.is_none_or(|s| inside(cx.span(r.node), s))
    })
}

fn has_read_inside(cx: &Ctx<'_, '_>, sym: SymbolId, span: Span) -> bool {
    let sem = cx.semantic();
    sem.symbol(sym).refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_read() && inside(cx.span(r.node), span)
    })
}

/// The `const` declarator initializer an identifier refers to (single plain declaration).
pub(crate) fn const_initializer(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sym = ref_symbol(cx, id)?;
    let s = cx.semantic().symbol(sym);
    let [d] = s.decls.as_slice() else {
        return None;
    };
    let Node::VarDeclarator { name, init, .. } = *ast.node(d.node) else {
        return None;
    };
    if d.kind != DeclKind::Const
        || !matches!(
            ast.node(d.parent),
            Node::VarDecl {
                kind: VarKind::Const,
                ..
            }
        )
        || name != d.name
    {
        return None;
    }
    init
}

/// `this` or `arguments` used by a `function` itself, or its own name referenced.
fn self_used_inside(cx: &Ctx<'_, '_>, f: NodeId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Node::FunctionExpr { name, .. } = *ast.node(f) else {
        return false;
    };
    let mut stack = Vec::new();
    ast.for_each_child(f, |c| stack.push(c));
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::This {} => return true,
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {}
            Node::ClassDecl { .. } | Node::ClassExpr { .. } => {}
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    let Some(fscope) = sem.scope_of_node(f) else {
        return false;
    };
    if sem
        .references()
        .iter()
        .any(|r| r.scope == fscope && ast.atom(r.name) == "arguments")
    {
        return true;
    }
    name.and_then(|n| decl_symbol(cx, n))
        .is_some_and(|s| !sem.symbol(s).refs.is_empty())
}

/// References of the parameter `param` (an identifier) made directly from `scope` inside
/// `expr`, one of which satisfies `pred`.
fn param_ref_matching(
    cx: &Ctx<'_, '_>,
    scope: ScopeId,
    param: Option<NodeId>,
    expr: NodeId,
    pred: &dyn Fn(NodeId) -> bool,
) -> bool {
    let sem = cx.semantic();
    let Some(sym) = param.and_then(|p| decl_symbol(cx, p)) else {
        return false;
    };
    let span = cx.span(expr);
    sem.symbol(sym).refs.iter().any(|&r| {
        let r = sem.reference(r);
        inside(cx.span(r.node), span) && r.scope == scope && pred(r.node)
    })
}

fn is_member_object(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ast = cx.ast();
    parent_skip_parens(ast, cx.file.parents, ident).is_some_and(|p| {
        matches!(*ast.node(p), Node::Member { obj, .. } | Node::Index { obj, .. } if skip_parens(ast, obj) == ident)
    })
}

fn is_call_argument(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ast = cx.ast();
    // ESTree has no parentheses, but `f((a))`'s argument is still `a`.
    parent_skip_parens(ast, cx.file.parents, ident).is_some_and(|p| {
        matches!(*ast.node(p), Node::Call { args, .. } if ast.list(args).iter().any(|&a| skip_parens(ast, a) == ident))
    })
}

fn inside_call(cx: &Ctx<'_, '_>, ident: NodeId, boundary: NodeId) -> bool {
    ancestors_inclusive(cx.file.parents, ident)
        .skip(1)
        .take_while(|&a| a != boundary)
        .any(|a| matches!(cx.node(a), Node::Call { .. }))
}

fn is_shorthand_value(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    cx.parent(ident)
        .is_some_and(|p| matches!(cx.node(p), Node::ShorthandProp { .. }))
}

struct Names {
    result: String,
    element: String,
    index: String,
    is_first: String,
    array: String,
}

/// The callback's returned expression with its parameters renamed, or `None`.
fn inline_expression_text(cx: &Ctx<'_, '_>, f: NodeId, names: &Names) -> Option<String> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let params = fn_params(ast, f);
    let mut idents = Vec::with_capacity(params.len());
    for &p in params {
        let Node::Param {
            type_ann, optional, ..
        } = *ast.node(p)
        else {
            return None;
        };
        if type_ann.is_some() || optional {
            return None;
        }
        idents.push(param_ident(ast, p)?);
    }
    let (return_type, type_params) = match *ast.node(f) {
        Node::Arrow {
            return_type,
            type_params,
            ..
        }
        | Node::FunctionExpr {
            return_type,
            type_params,
            ..
        } => (return_type, type_params),
        _ => return None,
    };
    let text = cx.text(f);
    if fn_flags(ast, f).intersects(FnFlags::ASYNC | FnFlags::GENERATOR)
        || params.len() > 4
        || return_type.is_some()
        || type_params.is_some()
        || idents
            .iter()
            .any(|&i| decl_symbol(cx, i).is_some_and(|s| has_write_inside(cx, s, None)))
        || has_word(text, "arguments")
        || has_word(text, "this")
    {
        return None;
    }
    let expr = return_expression(ast, f)?;
    if matches!(ast.node(expr), Node::Seq { .. })
        || has_object_method(ast, expr)
        || contains_direct_eval(ast, expr)
    {
        return None;
    }
    let expr_text = cx.text(expr);
    if expr_text.contains("=>")
        || has_word(expr_text, "class")
        || has_word(expr_text, "function")
        || has_new_target(expr_text)
    {
        return None;
    }
    let scope = sem.scope_of_node(f)?;
    if self_used_inside(cx, f)
        || param_ref_matching(cx, scope, idents.first().copied(), expr, &|i| {
            is_member_object(cx, i)
        })
    {
        return None;
    }
    let replacements = [
        names.result.as_str(),
        names.element.as_str(),
        names.index.as_str(),
        names.array.as_str(),
    ];
    let mut edits: Vec<(Span, &str)> = Vec::new();
    for (i, &ident) in idents.iter().enumerate() {
        let replacement = replacements[i];
        let sym = decl_symbol(cx, ident)?;
        let name = ident_name(ast, ident)?;
        for &r in &sem.symbol(sym).refs {
            let r = sem.reference(r);
            if r.scope != scope {
                return None;
            }
            if replacement != name && is_shorthand_value(cx, r.node) {
                return None;
            }
            if replacement == names.array
                && (is_member_object(cx, r.node)
                    || is_call_argument(cx, r.node)
                    || inside_call(cx, r.node, expr))
            {
                return None;
            }
            edits.push((cx.span(r.node), replacement));
        }
    }
    let span = cx.span(expr);
    edits.sort_by_key(|(s, _)| s.start);
    let mut out = String::new();
    let mut pos = span.start;
    for (s, t) in edits {
        if s.start < pos || s.end > span.end {
            return None;
        }
        out.push_str(&cx.src()[pos as usize..s.start as usize]);
        out.push_str(t);
        pos = s.end;
    }
    out.push_str(&cx.src()[pos as usize..span.end as usize]);
    Some(out)
}

fn has_object_method(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if let Node::ObjectLit { props } = *ast.node(n)
            && ast.list(props).iter().any(|&p| {
                matches!(
                    ast.node(p),
                    Node::MethodDef {
                        kind: MethodKind::Method,
                        ..
                    }
                )
            })
        {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn contains_direct_eval(ast: &Ast, root: NodeId) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if let Node::Call {
            callee,
            optional: false,
            ..
        } = *ast.node(n)
            && ident_name(ast, skip_wrappers(ast, callee)) == Some("eval")
        {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `\bword\b` in `text`.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

/// `new\s*\.\s*target\b`.
fn has_new_target(text: &str) -> bool {
    text.match_indices("new").any(|(i, _)| {
        let rest = text[i + 3..].trim_start();
        let Some(rest) = rest.strip_prefix('.') else {
            return false;
        };
        let rest = rest.trim_start();
        rest.starts_with("target") && !rest[6..].chars().next().is_some_and(is_word_char)
    })
}

/// The file's indentation step: the most common increase between consecutive indented
/// lines when spaces dominate, a tab otherwise (lines inside multi-line tokens skipped).
fn indent_unit(cx: &Ctx<'_, '_>) -> String {
    let src = cx.src();
    let ast = cx.ast();
    let mut skip: Vec<(u32, u32)> = ast
        .comments
        .iter()
        .map(|c| (c.span.start, c.span.end))
        .collect();
    skip.sort_unstable();
    let (mut tabs, mut spaces) = (0usize, 0usize);
    let mut diffs: Vec<(usize, usize)> = Vec::new();
    let mut prev = 0usize;
    let mut pos = 0usize;
    for line in src.split('\n') {
        let start = pos as u32;
        pos += line.len() + 1;
        if skip.iter().any(|&(s, e)| s < start && start < e) {
            continue;
        }
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let indent: &str = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        if indent.starts_with('\t') {
            tabs += 1;
        } else if indent.starts_with(' ') {
            spaces += 1;
        }
        let n = indent.chars().filter(|&c| c == ' ').count();
        if !indent.contains('\t') && n > prev {
            let d = n - prev;
            match diffs.iter_mut().find(|(k, _)| *k == d) {
                Some(e) => e.1 += 1,
                None => diffs.push((d, 1)),
            }
        }
        prev = if indent.contains('\t') { 0 } else { n };
    }
    if spaces > tabs
        && let Some(&(d, _)) = diffs
            .iter()
            .max_by_key(|&&(d, c)| (c, std::cmp::Reverse(d)))
    {
        return " ".repeat(d);
    }
    "\t".into()
}

impl NoArrayReduce {
    fn fix(&self, cx: &Ctx<'_, '_>, call: NodeId) -> Option<Fix> {
        let ast = cx.ast();
        let sem = cx.semantic();
        let src = cx.src();
        let (object, prop, optional, member_optional, args) = member_call(ast, call)?;
        if ident_name(ast, prop) != Some("reduce") || optional || member_optional {
            return None;
        }
        // `const x = arr.reduce(...)`, the only declarator, at the top level or in a block.
        let declarator = parent_skip_parens(ast, cx.file.parents, call)?;
        let Node::VarDeclarator {
            name,
            type_ann,
            init: Some(init),
            ..
        } = *ast.node(declarator)
        else {
            return None;
        };
        let decl = cx.parent(declarator)?;
        let Node::VarDecl {
            kind: VarKind::Const,
            decls,
            ..
        } = *ast.node(decl)
        else {
            return None;
        };
        if skip_parens(ast, init) != call
            || !matches!(ast.node(name), Node::Ident { .. })
            || ast.list(decls).len() != 1
            || !cx
                .parent(decl)
                .is_some_and(|p| matches!(cx.node(p), Node::Program { .. } | Node::Block { .. }))
        {
            return None;
        }
        let decl_span = cx.span(decl);
        if !ast.comments_in(decl_span).is_empty()
            || trailing_comment_same_line(cx, decl_span.end).is_some()
        {
            return None;
        }
        let callback = skip_parens(ast, args[0]);
        let initial = args.get(1).copied();
        let has_type_args = matches!(
            *ast.node(call),
            Node::Call {
                type_args: Some(_),
                ..
            }
        );
        if !matches!(ast.node(object), Node::Ident { .. })
            || !(is_inline_fn(ast, callback) || matches!(ast.node(callback), Node::Ident { .. }))
            || type_ann.is_some()
            || has_type_args
            || initial.is_some_and(|i| static_has_side_effect(ast, i))
        {
            return None;
        }
        let result_sym = decl_symbol(cx, name)?;
        if has_write_inside(cx, result_sym, None) || has_read_inside(cx, result_sym, cx.span(call))
        {
            return None;
        }
        let array_sym = ref_symbol(cx, object)?;
        let array_decl = sem.symbol(array_sym).decls.first().copied()?;
        if !matches!(ast.node(array_decl.node), Node::VarDeclarator { .. })
            || !matches!(
                ast.node(array_decl.parent),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            )
            || cx.span(array_decl.node).end > decl_span.start
            || has_write_inside(cx, array_sym, Some(decl_span))
            || has_read_inside(cx, array_sym, cx.span(callback))
            || escaped_accumulator(cx, object, callback, initial)
        {
            return None;
        }
        if initial.is_none()
            && const_initializer(cx, object).is_some_and(|a| {
                matches!(*ast.node(a), Node::ArrayLit { elements }
                    if ast.list(elements).iter().all(|&e| matches!(ast.node(e), Node::Hole {})))
            })
        {
            return None;
        }
        let result = ident_name(ast, name)?.to_owned();
        let scope = sem.enclosing_scope(call);
        let mut used = vec![result.clone()];
        let mut pick = |preferred: &str| -> Option<String> {
            let taken = used.clone();
            let n = available_name(cx, preferred, &[scope], &|n| !taken.iter().any(|t| t == n))?;
            used.push(n.clone());
            Some(n)
        };
        let preferred = |i: usize, fallback: &'static str| -> String {
            if is_inline_fn(ast, callback)
                && let Some(&p) = fn_params(ast, callback).get(i)
                && let Some(id) = param_ident(ast, p)
                && let Some(n) = ident_name(ast, id)
            {
                return n.to_owned();
            }
            fallback.to_owned()
        };
        let element = pick(&preferred(1, "element"))?;
        let index = pick(&preferred(2, "index"))?;
        let is_first = pick("isFirst")?;
        let names = Names {
            result,
            element,
            index,
            is_first,
            array: paren_text(cx, object).to_owned(),
        };
        let expression = if matches!(ast.node(callback), Node::Ident { .. }) {
            if !safe_callback_identifier(
                cx, callback, &names, array_sym, result_sym, object, initial,
            ) {
                return None;
            }
            format!(
                "{}({}, {}, {}, {})",
                cx.text(callback),
                names.result,
                names.element,
                names.index,
                names.array
            )
        } else {
            inline_expression_text(cx, callback, &names)?
        };
        let lines = cx.file.lines;
        let line_start = lines
            .line_start(lines.line_of(decl_span.start))
            .unwrap_or(0) as usize;
        let line = &src[line_start..];
        let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        let unit = indent_unit(cx);
        let nl = linebreak(src);
        let body = format!("{indent}{unit}");
        let nested = format!("{body}{unit}");
        let Names {
            result,
            element,
            index,
            is_first,
            array,
        } = &names;
        let mut out: Vec<String> = Vec::new();
        match initial {
            Some(i) => out.push(format!(
                "let {result} = {};",
                paren_text(cx, skip_parens(ast, i))
            )),
            None => {
                out.push(format!("let {result};"));
                out.push(format!("{indent}let {is_first} = true;"));
            }
        }
        out.push(String::new());
        out.push(format!(
            "{indent}for (const [{index}, {element}] of {array}.entries()) {{"
        ));
        out.push(format!("{body}if (!({index} in {array})) {{"));
        out.push(format!("{nested}continue;"));
        out.push(format!("{body}}}"));
        out.push(String::new());
        if initial.is_none() {
            out.push(format!("{body}if ({is_first}) {{"));
            out.push(format!("{nested}{result} = {element};"));
            out.push(format!("{nested}{is_first} = false;"));
            out.push(format!("{nested}continue;"));
            out.push(format!("{body}}}"));
            out.push(String::new());
        }
        out.push(format!("{body}{result} = {expression};"));
        out.push(format!("{indent}}}"));
        let mut end = decl_span.end;
        let next = skip_trivia_forward(src, end as usize);
        if src.as_bytes().get(next) == Some(&b';') {
            end = next as u32 + 1;
        }
        Some(Fix::replace(Span::new(decl_span.start, end), out.join(nl)))
    }
}

/// The initial value is the array itself (or a `const` bound to it) and the callback's
/// accumulator escapes through a member access or a call argument.
fn escaped_accumulator(
    cx: &Ctx<'_, '_>,
    object: NodeId,
    callback: NodeId,
    initial: Option<NodeId>,
) -> bool {
    let ast = cx.ast();
    let Some(initial) = initial.map(|i| skip_parens(ast, i)) else {
        return false;
    };
    if !is_inline_fn(ast, callback) {
        return false;
    }
    let Some(expr) = return_expression(ast, callback) else {
        return false;
    };
    let is_array_ref = is_same_reference(cx, initial, object)
        || const_initializer(cx, initial).is_some_and(|i| is_same_reference(cx, i, object));
    if !is_array_ref {
        return false;
    }
    let Some(scope) = cx.semantic().scope_of_node(callback) else {
        return false;
    };
    let first = fn_params(ast, callback)
        .first()
        .and_then(|&p| match *ast.node(p) {
            Node::Param { pattern, .. } => Some(pattern),
            _ => None,
        });
    param_ref_matching(cx, scope, first, expr, &|i| {
        is_member_object(cx, i) || is_call_argument(cx, i)
    })
}

#[allow(clippy::too_many_arguments)]
fn safe_callback_identifier(
    cx: &Ctx<'_, '_>,
    callback: NodeId,
    names: &Names,
    array_sym: SymbolId,
    result_sym: SymbolId,
    object: NodeId,
    initial: Option<NodeId>,
) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    // A `const` bound to an inline function declared before the call.
    let Some(sym) = ref_symbol(cx, callback) else {
        return false;
    };
    let Some(d) = sem.symbol(sym).decls.first().copied() else {
        return false;
    };
    let Node::VarDeclarator {
        init: Some(init), ..
    } = *ast.node(d.node)
    else {
        return false;
    };
    let f = skip_parens(ast, init);
    if !matches!(
        ast.node(d.parent),
        Node::VarDecl {
            kind: VarKind::Const,
            ..
        }
    ) || !is_inline_fn(ast, f)
        || cx.span(d.node).end > cx.span(callback).start
    {
        return false;
    }
    let fspan = cx.span(f);
    if has_read_inside(cx, array_sym, fspan)
        || has_read_inside(cx, result_sym, fspan)
        || escaped_accumulator(cx, object, f, initial)
    {
        return false;
    }
    if let Some(expr) = return_expression(ast, f)
        && let Some(&p3) = fn_params(ast, f).get(3)
        && let Some(scope) = sem.scope_of_node(f)
        && let Node::Param { pattern, .. } = *ast.node(p3)
        && param_ref_matching(cx, scope, Some(pattern), expr, &|_| true)
    {
        return false;
    }
    inline_expression_text(cx, f, names).is_some()
}

impl Rule for NoArrayReduce {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some((object, prop, optional, member_optional, args)) = member_call(ast, node) else {
            return;
        };
        let Some(method) = ident_name(ast, prop) else {
            return;
        };
        if matches!(method, "reduce" | "reduceRight") {
            if optional || !matches!(args.len(), 1 | 2) || is_not_function(ast, args[0]) {
                return;
            }
            let callback = skip_parens(ast, args[0]);
            let simple = is_inline_fn(ast, callback)
                && return_expression(ast, callback).is_some_and(|e| {
                    matches!(*ast.node(e), Node::Binary { op, .. }
                        if !matches!(op, BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce))
                });
            if (self.allow_simple && simple) || should_skip_known_non_array_receiver(cx, object) {
                return;
            }
            let fix = self.fix(cx, node);
            let suggestion = sum_precise_suggestion(cx, node);
            let span = cx.span(prop);
            let mut report = cx.report(span, message(method)).id(if method == "reduce" {
                "reduce"
            } else {
                "reduceRight"
            });
            report = report.fix_opt(fix);
            if let Some(s) = suggestion {
                report.suggest("sum-precise", "Switch to `Math.sumPrecise()`.", s);
            }
            return;
        }
        if !matches!(method, "call" | "apply") || optional || member_optional {
            return;
        }
        if !is_array_prototype_property(ast, object, &["reduce", "reduceRight"]) {
            return;
        }
        if method == "call" && args.get(1).is_some_and(|&a| is_not_function(ast, a)) {
            return;
        }
        let Node::Member { prop: inner, .. } = *ast.node(object) else {
            return;
        };
        let Some(name) = ident_name(ast, inner) else {
            return;
        };
        let id = if name == "reduce" {
            "reduce"
        } else {
            "reduceRight"
        };
        let span = cx.span(inner);
        cx.report(span, message(name)).id(id);
    }
}
