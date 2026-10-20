//! `@typescript-eslint/prefer-string-starts-ends-with`: `s.startsWith(x)` / `s.endsWith(x)`
//! over `s[0] === x`, `s.indexOf(x) === 0`, `s.slice(0, n) === x`, `/^x/.test(s)`, ...

use super::prelude::*;
use super::ts_prefer_includes::needs_parens_for_member;
use crate::regex::{self, AssertKind, RKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-string-starts-ends-with",
        "Enforce using `String#startsWith` and `String#endsWith` over other equivalent methods of checking substrings",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<PreferStartsEndsWith>,
};

#[derive(Debug)]
struct PreferStartsEndsWith {
    allow_single_element_equality: bool,
}

impl FromOptions for PreferStartsEndsWith {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferStartsEndsWith {
            allow_single_element_equality: str_field(o, "allowSingleElementEquality")
                == Some("always"),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Starts,
    Ends,
}

impl Kind {
    fn method(self) -> &'static str {
        match self {
            Kind::Starts => "startsWith",
            Kind::Ends => "endsWith",
        }
    }

    fn report(self, cx: &mut Ctx<'_, '_>, span: Span, fix: Option<Fix>) {
        let (id, msg) = match self {
            Kind::Starts => (
                "preferStartsWith",
                "Use 'String#startsWith' method instead.",
            ),
            Kind::Ends => (
                "preferEndsWith",
                "Use the 'String#endsWith' method instead.",
            ),
        };
        cx.report(span, msg).id(id).fix_opt(fix);
    }
}

impl Rule for PreferStartsEndsWith {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary, NodeKind::Call]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Binary { left, op, right } => {
                let negate = match op {
                    BinaryOp::StrictEq | BinaryOp::Eq => false,
                    BinaryOp::StrictNotEq | BinaryOp::NotEq => true,
                    _ => return,
                };
                let loose = matches!(op, BinaryOp::Eq | BinaryOp::NotEq);
                self.check_binary(cx, node, left, right, negate, loose);
            }
            Node::Call { .. } => check_test(cx, node),
            _ => {}
        }
    }
}

/// The `Member`/`Index`/`Call` parts of `obj.name(args)` / `obj?.name(args)`.
struct MethodCall<'a> {
    obj: NodeId,
    prop: NodeId,
    name: &'a str,
    args: &'a [NodeId],
}

fn method_call<'a>(cx: &Ctx<'a, '_>, e: NodeId) -> Option<MethodCall<'a>> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *cx.node(e)
    else {
        return None;
    };
    let Node::Member { obj, prop, .. } = *cx.node(callee) else {
        return None;
    };
    Some(MethodCall {
        obj,
        prop,
        name: ident_name(ast, prop)?,
        args: ast.list(args),
    })
}

impl PreferStartsEndsWith {
    fn check_binary(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        left: NodeId,
        right: NodeId,
        negate: bool,
        loose: bool,
    ) {
        let ast = cx.ast();
        let src = cx.src();
        let left = skip_parens(ast, left);
        let bang = if negate { "!" } else { "" };
        let span = cx.span(node);
        let start = cx.span(left).start as usize;
        let rhs = skip_parens(ast, right);
        // `s[0] === x` / `s[s.length - 1] === x`.
        if let Node::Index {
            obj,
            optional,
            index,
        } = *cx.node(left)
        {
            if self.allow_single_element_equality || !is_string(cx, obj) {
                return;
            }
            let Some(kind) = element_kind(cx, obj, index) else {
                return;
            };
            let fix = single_char(cx, rhs).then(|| {
                let dot = if optional { "?." } else { "." };
                Fix::replace(
                    span,
                    format!(
                        "{bang}{}{dot}{}({})",
                        &src[start..cx.span(obj).end as usize],
                        kind.method(),
                        cx.span(rhs).text(src)
                    ),
                )
            });
            kind.report(cx, span, fix);
            return;
        }
        let Some(mc) = method_call(cx, left) else {
            // `s.match(/^x/) !== null`.
            return;
        };
        let prefix = &src[start..cx.span(mc.prop).start as usize];
        let replace_from_prop = |kind: Kind, arg_text: &str, neg: bool| {
            let bang = if neg { "!" } else { "" };
            Fix::replace(span, format!("{bang}{prefix}{}({arg_text})", kind.method()))
        };
        match mc.name {
            "charAt" => {
                if self.allow_single_element_equality || !is_string(cx, mc.obj) {
                    return;
                }
                let [index] = mc.args[..] else {
                    return;
                };
                let Some(kind) = element_kind(cx, mc.obj, index) else {
                    return;
                };
                let fix = single_char(cx, rhs)
                    .then(|| replace_from_prop(kind, cx.span(rhs).text(src), negate));
                kind.report(cx, span, fix);
            }
            "indexOf" | "lastIndexOf" => {
                let [needle] = mc.args[..] else {
                    return;
                };
                if !is_string(cx, mc.obj) {
                    return;
                }
                let kind = if mc.name == "indexOf" {
                    if static_number(cx, rhs) != Some(0.0) || !is_number_lit(ast, rhs) {
                        return;
                    }
                    Kind::Starts
                } else {
                    // `s.length - needle.length` or `s.length - <literal length>`.
                    let Node::Binary {
                        left: l,
                        op: BinaryOp::Sub,
                        right: r,
                    } = *cx.node(rhs)
                    else {
                        return;
                    };
                    if !is_length_of(cx, l, mc.obj) || !is_length_matching(cx, r, needle) {
                        return;
                    }
                    Kind::Ends
                };
                let fix = replace_from_prop(kind, cx.span(needle).text(src), negate);
                kind.report(cx, span, Some(fix));
            }
            "match" => {
                if !is_null_lit(ast, rhs) || !is_string(cx, mc.obj) {
                    return;
                }
                let [re] = mc.args[..] else {
                    return;
                };
                let Some((kind, text)) = anchored_regex(cx, re, 0) else {
                    return;
                };
                let fix = replace_from_prop(kind, &json_string(&text), !negate);
                kind.report(cx, cx.span(left), Some(fix));
            }
            "slice" | "substring" => {
                if !is_string(cx, mc.obj) {
                    return;
                }
                let Some((kind, fixable)) = slice_kind(cx, &mc, rhs) else {
                    return;
                };
                let rhs_is_lit = matches!(cx.node(rhs), Node::StringLit { .. });
                let fix = (fixable && (!loose || rhs_is_lit))
                    .then(|| replace_from_prop(kind, cx.span(rhs).text(src), negate));
                kind.report(cx, span, fix);
            }
            _ => {}
        }
    }
}

/// `/^x/.test(s)` / `/x$/.test(s)`.
fn check_test(cx: &mut Ctx<'_, '_>, node: NodeId) {
    let ast = cx.ast();
    let src = cx.src();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *cx.node(node)
    else {
        return;
    };
    let Node::Member {
        obj,
        prop,
        optional,
    } = *cx.node(callee)
    else {
        return;
    };
    if ident_name(ast, prop) != Some("test") {
        return;
    }
    let [arg] = ast.list(args)[..] else {
        return;
    };
    let Some((kind, text)) = anchored_regex(cx, obj, 0) else {
        return;
    };
    let inner = skip_parens(ast, arg);
    let arg_text = cx.span(inner).text(src);
    let target = if needs_parens_for_member(ast, inner) {
        format!("({arg_text})")
    } else {
        arg_text.to_string()
    };
    let dot = if optional { "?." } else { "." };
    let fix = Fix::replace(
        cx.span(node),
        format!("{target}{dot}{}({})", kind.method(), json_string(&text)),
    );
    let span = cx.span(node);
    kind.report(cx, span, Some(fix));
}

/// `0` (starts) or `s.length - 1` (ends) as the index of an element read of `s`.
fn element_kind(cx: &Ctx<'_, '_>, s: NodeId, index: NodeId) -> Option<Kind> {
    let ast = cx.ast();
    let index = skip_parens(ast, index);
    if is_number_lit(ast, index) && static_number(cx, index) == Some(0.0) {
        return Some(Kind::Starts);
    }
    if let Node::Binary {
        left,
        op: BinaryOp::Sub,
        right,
    } = *cx.node(index)
        && is_length_of(cx, left, s)
        && is_number_lit(ast, right)
        && static_number(cx, right) == Some(1.0)
    {
        return Some(Kind::Ends);
    }
    None
}

/// The parts of a `slice`/`substring` comparison: the kind and whether it can be fixed.
fn slice_kind(cx: &Ctx<'_, '_>, mc: &MethodCall<'_>, rhs: NodeId) -> Option<(Kind, bool)> {
    let ast = cx.ast();
    let is_slice = mc.name == "slice";
    match *mc.args {
        [a, b] => {
            let a = skip_parens(ast, a);
            if is_number_lit(ast, a) && static_number(cx, a) == Some(0.0) {
                // `(0, n)`.
                return is_length_matching(cx, b, rhs).then_some((Kind::Starts, true));
            }
            // `(s.length - n, s.length)`.
            if is_length_of(cx, b, mc.obj)
                && let Node::Binary {
                    left,
                    op: BinaryOp::Sub,
                    right,
                } = *cx.node(a)
                && is_length_of(cx, left, mc.obj)
                && is_length_matching(cx, right, rhs)
            {
                return Some((Kind::Ends, true));
            }
            None
        }
        [a] => {
            let a = skip_parens(ast, a);
            match *cx.node(a) {
                // `(-n)`: `substring` treats it as 0, probably a mistake: no fix.
                Node::Unary {
                    op: UnaryOp::Minus,
                    arg,
                } if is_length_matching(cx, arg, rhs) => Some((Kind::Ends, is_slice)),
                // `(s.length - n)`.
                Node::Binary {
                    left,
                    op: BinaryOp::Sub,
                    right,
                } if is_length_of(cx, left, mc.obj) && is_length_matching(cx, right, rhs) => {
                    Some((Kind::Ends, true))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `e` is `s.length` for the same `s`.
fn is_length_of(cx: &Ctx<'_, '_>, e: NodeId, s: NodeId) -> bool {
    let ast = cx.ast();
    match *cx.node(skip_parens(ast, e)) {
        Node::Member { obj, prop, .. } => {
            ident_name(ast, prop) == Some("length")
                && same_tokens(ast, cx.src(), skip_parens(ast, obj), skip_parens(ast, s))
        }
        _ => false,
    }
}

/// `n` is the length of `x`: `x.length`, or a number equal to the length of the string
/// literal `x`.
fn is_length_matching(cx: &Ctx<'_, '_>, n: NodeId, x: NodeId) -> bool {
    let ast = cx.ast();
    let x = skip_parens(ast, x);
    if is_length_of(cx, n, x) {
        return true;
    }
    let n = skip_parens(ast, n);
    if let Node::StringLit { value } = *cx.node(x)
        && is_number_lit(ast, n)
        && let Some(v) = static_number(cx, n)
    {
        return v == ast.atom(value).encode_utf16().count() as f64;
    }
    false
}

fn is_number_lit(ast: &ts_r_ast::Ast, e: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, e)), Node::NumberLit { .. })
}

fn is_null_lit(ast: &ts_r_ast::Ast, e: NodeId) -> bool {
    matches!(ast.node(skip_parens(ast, e)), Node::NullLit {})
}

/// A string literal of one UTF-16 code unit.
fn single_char(cx: &Ctx<'_, '_>, e: NodeId) -> bool {
    match *cx.node(e) {
        Node::StringLit { value } => cx.ast().atom(value).encode_utf16().count() == 1,
        _ => false,
    }
}

/// The value's type (without `null`/`undefined`) is string-like.
fn is_string(cx: &Ctx<'_, '_>, e: NodeId) -> bool {
    let ti = cx.types();
    match ti.type_at(e) {
        Some(t) if t != Ty::UNRESOLVED => is_string_like_type(ti, ti.non_nullable(t)),
        _ => false,
    }
}

/// A regex (literal, `new RegExp('...')`, or a `const` bound to one) that is `^literal` or
/// `literal$`, with the literal text.
fn anchored_regex(cx: &Ctx<'_, '_>, e: NodeId, depth: u32) -> Option<(Kind, String)> {
    if depth > 8 {
        return None;
    }
    let ast = cx.ast();
    let e = skip_parens(ast, e);
    let (pattern, flags) = match *cx.node(e) {
        Node::RegexLit { pattern, flags } => {
            (ast.atom(pattern).to_owned(), ast.atom(flags).to_owned())
        }
        Node::New { .. } | Node::Call { .. } => {
            let args = regexp_call_args(cx, e, false, false)?;
            let p = static_string_eval(cx, *args.first()?)?;
            let f = match args.get(1) {
                None => String::new(),
                Some(&f) => static_string_eval(cx, f)?,
            };
            if args.len() > 2 {
                return None;
            }
            (p, f)
        }
        Node::Ident { .. } => return anchored_regex(cx, const_init(cx, e)?, depth + 1),
        _ => return None,
    };
    if flags.chars().any(|c| !matches!(c, 'u' | 'v')) {
        return None;
    }
    let re = regex::parse(&pattern, regex::Flags::new(&flags)).ok()?;
    let [alt] = re.node(re.root).children[..] else {
        return None;
    };
    let terms = &re.node(alt).children;
    let (kind, chars) = match (terms.first(), terms.last()) {
        (Some(&f), _) if *re.kind(f) == RKind::Assertion(AssertKind::Start) => {
            (Kind::Starts, &terms[1..])
        }
        (_, Some(&l)) if *re.kind(l) == RKind::Assertion(AssertKind::End) => {
            (Kind::Ends, &terms[..terms.len() - 1])
        }
        _ => return None,
    };
    let mut units: Vec<u16> = Vec::new();
    for &c in chars {
        let RKind::Char { value } = *re.kind(c) else {
            return None;
        };
        if let Some(ch) = char::from_u32(value)
            && value > 0xFFFF
        {
            let mut buf = [0u16; 2];
            units.extend_from_slice(ch.encode_utf16(&mut buf));
        } else {
            units.push(value as u16);
        }
    }
    if units.is_empty() {
        return None;
    }
    Some((kind, String::from_utf16(&units).ok()?))
}

/// `JSON.stringify` of a string.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}
