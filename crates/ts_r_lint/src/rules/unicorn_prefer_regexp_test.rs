//! `unicorn/prefer-regexp-test`: `re.exec(s)`, `s.match(re)` and `s.search(re)` used only
//! to see whether there is a match become `re.test(s)`.
//!
//! - Where: a boolean context (`if`, `?:`, loops, `!`, `Boolean()`, `&&`/`||` chains of
//!   those), a `.length` read of the result in a boolean context (or `> 0` of it), and for
//!   `search` only a comparison with `-1` / `0` (`!== -1`, `>= 0`, `=== -1`, `< 0`, ...).
//! - Skipped when the receiver or argument is known to be the wrong kind (a string where
//!   the regex goes, or the reverse), read from regex literals, `new RegExp`, strings,
//!   annotations (`string`, `RegExp`, literals, unions, intersections), `const`
//!   initializers and static values; and for `match` a receiver with an unparenthesized
//!   optional chain.
//! - The fix swaps receiver and argument for `match`/`search` (adding parentheses or a `;`
//!   when needed) and drops the `.length` / comparison. It is automatic when the regex is
//!   known not to be global or sticky (a literal without `g`/`y`, or a static regex), and a
//!   suggestion otherwise; `.length` checks only get a suggestion. A comment in the removed
//!   code withholds both.

use super::prelude::*;
use super::unicorn_explicit_length_check::{is_boolean_context, is_global_boolean_call};
use super::unicorn_static::{
    Static, mutable_member_access, static_regexp, static_value, static_value_no_side_effects,
};
use super::unicorn_text::{
    has_comment_in, has_unparenthesized_optional_chain, is_parenthesized,
    member_object_needs_parens, needs_semicolon, paren_span, static_string_value,
};
use super::unicorn_types::{binding_annotation, is_string};
use super::unicorn_util::space_around_keyword;
use ts_r_ast::{Ast, KeywordTypeKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-regexp-test",
        "Prefer `RegExp#test()` over `String#match()`, `String#search()`, and `RegExp#exec()`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferRegexpTest>,
};

#[derive(Debug, Default)]
struct PreferRegexpTest;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Str,
    RegExp,
    Other,
    StrOrOther,
    RegExpOrOther,
    Unknown,
}

fn combine_union(types: &[Kind]) -> Kind {
    use Kind::*;
    if types.contains(&Unknown) {
        return Unknown;
    }
    let has = |k: Kind| types.contains(&k);
    let string = has(Str) || has(StrOrOther);
    let regexp = has(RegExp) || has(RegExpOrOther);
    if string && regexp {
        return StrOrOther;
    }
    let other = has(Other) || has(StrOrOther) || has(RegExpOrOther);
    match (string, regexp) {
        (true, _) => {
            if other {
                StrOrOther
            } else {
                Str
            }
        }
        (_, true) => {
            if other {
                RegExpOrOther
            } else {
                RegExp
            }
        }
        _ => Other,
    }
}

fn combine_intersection(types: &[Kind]) -> Kind {
    if types.contains(&Kind::Unknown) {
        Kind::Unknown
    } else if types.contains(&Kind::RegExp) {
        Kind::RegExp
    } else if types.contains(&Kind::Str) {
        Kind::Str
    } else {
        Kind::Other
    }
}

fn annotation_kind(ast: &Ast, ty: Option<NodeId>, depth: u32) -> Kind {
    let Some(ty) = ty else {
        return Kind::Unknown;
    };
    if depth > 100 {
        return Kind::Unknown;
    }
    match *ast.node(ty) {
        Node::ParenType { ty } => annotation_kind(ast, Some(ty), depth + 1),
        Node::KeywordType { kind } => match kind {
            KeywordTypeKind::String => Kind::Str,
            KeywordTypeKind::BigInt
            | KeywordTypeKind::Boolean
            | KeywordTypeKind::Never
            | KeywordTypeKind::Null
            | KeywordTypeKind::Number
            | KeywordTypeKind::Object
            | KeywordTypeKind::Symbol
            | KeywordTypeKind::Undefined
            | KeywordTypeKind::Void => Kind::Other,
            _ => Kind::Unknown,
        },
        Node::StringLit { .. } => Kind::Str,
        Node::TemplateLit { .. } if static_string_value(ast, ty).is_some() => Kind::Str,
        Node::TemplateLitType { parts } if ast.list(parts).len() == 1 => Kind::Str,
        Node::TemplateLit { .. } | Node::TemplateLitType { .. } => Kind::Other,
        Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::Unary { .. } => Kind::Other,
        Node::TypeRef { name, .. } => match *ast.node(name) {
            Node::Ident { .. } => match ident_name(ast, name) {
                Some("String") => Kind::Str,
                Some("RegExp") => Kind::RegExp,
                Some("Array" | "ReadonlyArray") => Kind::Other,
                _ => Kind::Unknown,
            },
            _ => Kind::Unknown,
        },
        Node::UnionType { types } => {
            let kinds: Vec<Kind> = ast
                .list(types)
                .iter()
                .map(|&t| annotation_kind(ast, Some(t), depth + 1))
                .collect();
            combine_union(&kinds)
        }
        Node::IntersectionType { types } => {
            let kinds: Vec<Kind> = ast
                .list(types)
                .iter()
                .map(|&t| annotation_kind(ast, Some(t), depth + 1))
                .collect();
            combine_intersection(&kinds)
        }
        Node::ArrayType { .. }
        | Node::TypeLiteral { .. }
        | Node::TupleType { .. }
        | Node::FunctionType { .. }
        | Node::ConstructorType { .. } => Kind::Other,
        _ => Kind::Unknown,
    }
}

fn is_regexp_node(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::RegexLit { .. } => true,
        Node::New { callee, .. } => {
            matches!(ast.node(callee), Node::Ident { .. })
                && ident_name(ast, callee) == Some("RegExp")
        }
        _ => false,
    }
}

fn static_kind(cx: &Ctx<'_, '_>, id: NodeId) -> Kind {
    let ast = cx.ast();
    let result = static_value_no_side_effects(cx, id);
    if result.is_none() && mutable_member_access(cx, id, &mut Vec::new()) {
        return match static_value(cx, id) {
            Some(Static::Undefined) | None => Kind::Unknown,
            Some(_) => Kind::Other,
        };
    }
    match result {
        None => Kind::Unknown,
        Some(Static::Str(_)) => Kind::Str,
        Some(Static::RegExp { .. }) => Kind::RegExp,
        Some(Static::Undefined) => {
            let plain = match *ast.node(id) {
                Node::Ident { .. } => ident_name(ast, id) == Some("undefined"),
                Node::Unary {
                    op: UnaryOp::Void, ..
                } => true,
                _ => false,
            };
            if plain { Kind::Other } else { Kind::Unknown }
        }
        Some(Static::Null | Static::BigInt(_) | Static::Bool(_) | Static::Num(_)) => Kind::Other,
        Some(_) => Kind::Unknown,
    }
}

fn variable_kind(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> Kind {
    let ast = cx.ast();
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_ident(id) else {
        return Kind::Unknown;
    };
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls.as_slice() else {
        return Kind::Unknown;
    };
    if visited.contains(&sym) {
        return Kind::Unknown;
    }
    visited.push(sym);
    let from_annotation = annotation_kind(ast, binding_annotation(cx, decl.name).0, 0);
    let kind = if from_annotation != Kind::Unknown {
        from_annotation
    } else {
        match *ast.node(decl.node) {
            Node::VarDeclarator {
                init: Some(init), ..
            } if decl.kind == DeclKind::Const => expression_kind(cx, init, visited),
            _ => Kind::Unknown,
        }
    };
    visited.retain(|s| *s != sym);
    kind
}

/// `getExpressionType`: what an expression is known to be.
fn expression_kind(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> Kind {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if visited.len() > 100 {
        return Kind::Unknown;
    }
    if is_regexp_node(ast, id) {
        return Kind::RegExp;
    }
    if is_string(cx, id) {
        return Kind::Str;
    }
    match *ast.node(id) {
        Node::Ident { .. } => {
            let k = variable_kind(cx, id, visited);
            if k != Kind::Unknown {
                return k;
            }
        }
        Node::As { expr, ty } | Node::Satisfies { expr, ty } | Node::TypeAssertion { expr, ty } => {
            let k = annotation_kind(ast, Some(ty), 0);
            return if k == Kind::Unknown {
                expression_kind(cx, expr, visited)
            } else {
                k
            };
        }
        Node::NonNull { expr } => return expression_kind(cx, expr, visited),
        Node::Seq { exprs } => {
            if let Some(&last) = ast.list(exprs).last() {
                return expression_kind(cx, last, visited);
            }
        }
        Node::Conditional { cons, alt, .. } => {
            let a = expression_kind(cx, cons, visited);
            let b = expression_kind(cx, alt, visited);
            return combine_union(&[a, b]);
        }
        _ => {}
    }
    let k = static_kind(cx, id);
    if k != Kind::Unknown {
        return k;
    }
    match ast.node(id) {
        Node::ArrayLit { .. }
        | Node::Arrow { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionExpr { .. }
        | Node::ObjectLit { .. } => Kind::Other,
        _ => Kind::Unknown,
    }
}

fn known_not_string(k: Kind) -> bool {
    matches!(k, Kind::RegExp | Kind::RegExpOrOther | Kind::Other)
}

fn known_not_regexp(k: Kind) -> bool {
    matches!(k, Kind::Str | Kind::StrOrOther | Kind::Other)
}

/// The ESTree parent (past parentheses) and the child on that path.
fn parent_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
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

fn is_logical(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
            ..
        }
    )
}

/// `!` around the outermost `&&`/`||`/`Boolean()` holding `node`.
fn is_negated_boolean_value(cx: &Ctx<'_, '_>, mut node: NodeId) -> bool {
    for _ in 0..10_000 {
        let Some((p, _)) = parent_of(cx, node) else {
            return false;
        };
        if is_logical(cx.ast(), p) || is_global_boolean_call(cx, p) {
            node = p;
        } else {
            break;
        }
    }
    parent_of(cx, node).is_some_and(|(p, _)| {
        matches!(
            cx.node(p),
            Node::Unary {
                op: UnaryOp::Not,
                ..
            }
        )
    })
}

fn is_number(ast: &Ast, id: NodeId, v: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == v)
}

fn is_negative_one(ast: &Ast, id: NodeId) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::Unary { op: UnaryOp::Minus, arg } if is_number(ast, arg, 1.0))
}

struct LengthCheck {
    length: NodeId,
    node: NodeId,
    comparison_left: Option<NodeId>,
}

fn length_check(cx: &Ctx<'_, '_>, call: NodeId) -> Option<LengthCheck> {
    let ast = cx.ast();
    let (length, child) = parent_of(cx, call)?;
    match *ast.node(length) {
        Node::Member { obj, prop, .. }
            if obj == child && ident_name(ast, prop) == Some("length") => {}
        _ => return None,
    }
    if is_negated_boolean_value(cx, length) {
        return None;
    }
    if is_boolean_context(cx, length) {
        return Some(LengthCheck {
            length,
            node: length,
            comparison_left: None,
        });
    }
    let (parent, child) = parent_of(cx, length)?;
    match *ast.node(parent) {
        Node::Binary {
            left,
            op: BinaryOp::Gt,
            right,
        } if left == child
            && is_number(ast, right, 0.0)
            && !is_negated_boolean_value(cx, parent)
            && is_boolean_context(cx, parent) =>
        {
            Some(LengthCheck {
                length,
                node: parent,
                comparison_left: Some(length),
            })
        }
        _ => None,
    }
}

struct SearchCheck {
    node: NodeId,
    negate: bool,
}

fn search_check(cx: &Ctx<'_, '_>, call: NodeId) -> Option<SearchCheck> {
    let ast = cx.ast();
    let (parent, child) = parent_of(cx, call)?;
    let Node::Binary { left, op, right } = *ast.node(parent) else {
        return None;
    };
    if left != child {
        return None;
    }
    use BinaryOp::*;
    let negate = if (matches!(op, StrictNotEq | NotEq | Gt) && is_negative_one(ast, right))
        || (op == GtEq && is_number(ast, right, 0.0))
    {
        false
    } else if (matches!(op, StrictEq | Eq) && is_negative_one(ast, right))
        || (op == Lt && is_number(ast, right, 0.0))
    {
        true
    } else {
        return None;
    };
    Some(SearchCheck {
        node: parent,
        negate,
    })
}

impl Rule for PreferRegexpTest {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let length = length_check(cx, node);
        let search = search_check(cx, node);
        if length.is_none() && search.is_none() && !is_boolean_context(cx, node) {
            return;
        }
        let Node::Call {
            callee,
            args,
            optional: false,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        let &[arg] = ast.list(args) else {
            return;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        let method = match ident_name(ast, prop) {
            Some(m @ ("exec" | "match")) if search.is_none() => m,
            Some("search") if search.is_some() => "search",
            _ => return,
        };
        let obj = skip_parens(ast, obj);
        let arg_inner = skip_parens(ast, arg);
        let (string, regexp) = if method == "exec" {
            (arg_inner, obj)
        } else {
            (obj, arg_inner)
        };
        if method == "match" && has_unparenthesized_optional_chain(cx, string) {
            return;
        }
        let static_re = static_regexp(cx, regexp);
        let safe_regexp = match *ast.node(regexp) {
            Node::RegexLit { flags, .. } => !ast.atom(flags).contains(['g', 'y']),
            _ => static_re
                .as_ref()
                .is_some_and(|(_, f)| !f.contains(['g', 'y'])),
        };
        let string_kind = expression_kind(cx, string, &mut Vec::new());
        let regexp_kind = expression_kind(cx, regexp, &mut Vec::new());
        if known_not_string(string_kind) || known_not_regexp(regexp_kind) {
            return;
        }
        let mut edits: Vec<(Span, String)> = vec![(cx.span(prop), "test".to_owned())];
        if method != "exec" {
            let mut string_text = cx.text(string).to_owned();
            if !is_parenthesized(cx, regexp) && matches!(ast.node(string), Node::Seq { .. }) {
                string_text = format!("({string_text})");
            }
            edits.push((cx.span(regexp), string_text));
            let mut regexp_text = cx.text(regexp).to_owned();
            let negate = search.as_ref().is_some_and(|s| s.negate);
            if !is_parenthesized(cx, string)
                && member_object_needs_parens(cx, regexp, if negate { regexp } else { string })
            {
                regexp_text = format!("({regexp_text})");
            }
            if needs_semicolon(cx, cx.span(string).start, &regexp_text) {
                edits.push((Span::at(cx.span(string).start), ";".to_owned()));
            }
            edits.push((cx.span(string), regexp_text));
            edits.extend(space_around_keyword(cx, string));
        }
        let call_end = paren_span(cx, node).end;
        let mut removed = Vec::new();
        if let Some(l) = &length {
            removed.push(Span::new(call_end, cx.span(l.length).end));
            if let Some(left) = l.comparison_left {
                removed.push(Span::new(paren_span(cx, left).end, cx.span(l.node).end));
            }
        } else if let Some(s) = &search {
            if s.negate {
                edits.push((Span::at(cx.span(s.node).start), "!".to_owned()));
            }
            removed.push(Span::new(call_end, cx.span(s.node).end));
        }
        let comment = removed.iter().any(|&r| has_comment_in(cx, r));
        let mut fix = Fix { edits: Vec::new() };
        for (span, text) in edits
            .into_iter()
            .chain(removed.iter().map(|&r| (r, String::new())))
        {
            fix = fix.and(span, text);
        }
        let auto = length.is_none()
            && (safe_regexp
                || static_re.as_ref().is_some_and(|(_, f)| {
                    !f.contains('g') && (search.is_none() || !f.contains('y'))
                }));
        let (id, msg, target) = match method {
            "exec" => ("regexp-exec", "Prefer `.test(…)` over `.exec(…)`.", prop),
            "match" => (
                "string-match",
                "Prefer `RegExp#test(…)` over `String#match(…)`.",
                node,
            ),
            _ => (
                "string-search",
                "Prefer `RegExp#test(…)` over `String#search(…)` when checking for existence.",
                node,
            ),
        };
        let report = cx.report_node(target, msg).id(id);
        if comment {
            return;
        }
        if auto {
            report.fix(fix);
        } else {
            report.suggest("suggestion", "Switch to `RegExp#test(…)`.", fix);
        }
    }
}
