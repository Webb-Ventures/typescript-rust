//! More helpers for the eslint-plugin-unicorn rules: reference equality (`isSameReference`)
//! and flattening `&&` / `||` chains.

use super::prelude::*;
use super::unicorn_static::{Static, static_value};
use ts_r_ast::Ast;

/// Nesting limit for [`is_same_reference`].
const MAX_DEPTH: u32 = 200;

/// Strips parentheses, TS wrappers (`as`, `satisfies`, `!`, `<T>x`, `f<T>`).
pub(crate) fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
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

/// A string literal's value or a substitution-free template's cooked text.
pub(crate) fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[only] => match *ast.node(only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The static name of a member access: `a.b`, `a["b"]`, `a[0]`, `a[`b`]`.
pub(crate) fn static_property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    match *cx.node(id) {
        Node::Member { prop, .. } => match *cx.node(prop) {
            Node::Ident { name } => Some(cx.ast().atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => static_value(cx, index).and_then(|v| v.to_js_string()),
        _ => None,
    }
}

/// `isSameReference`: both sides (through parentheses and TS wrappers) are the same static
/// string, `this`, `super`, the same name, equal literals, or member accesses with the same
/// object and the same (static, or same computed) property.
pub(crate) fn is_same_reference(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    same_ref(cx, a, b, 0)
}

fn same_ref(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let ast = cx.ast();
    let (a, b) = (unwrap_ts(ast, a), unwrap_ts(ast, b));
    if let Some(s) = static_string(ast, a) {
        return static_string(ast, b).as_ref() == Some(&s);
    }
    let member = |id| matches!(cx.node(id), Node::Member { .. } | Node::Index { .. });
    if member(a) && member(b) {
        let (oa, ob) = match (*cx.node(a), *cx.node(b)) {
            (
                Node::Member { obj: x, .. } | Node::Index { obj: x, .. },
                Node::Member { obj: y, .. } | Node::Index { obj: y, .. },
            ) => (x, y),
            _ => return false,
        };
        if let Some(name) = static_property_name(cx, a) {
            return same_ref(cx, oa, ob, depth + 1)
                && static_property_name(cx, b).as_ref() == Some(&name);
        }
        return match (*cx.node(a), *cx.node(b)) {
            (Node::Member { prop: p, .. }, Node::Member { prop: q, .. })
            | (Node::Index { index: p, .. }, Node::Index { index: q, .. }) => {
                same_ref(cx, oa, ob, depth + 1) && same_ref(cx, p, q, depth + 1)
            }
            _ => false,
        };
    }
    match (*cx.node(a), *cx.node(b)) {
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (Node::Ident { name: x }, Node::Ident { name: y })
        | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => x == y,
        (Node::NumberLit { .. }, Node::NumberLit { .. })
        | (Node::BigIntLit { .. }, Node::BigIntLit { .. })
        | (Node::BoolLit { .. }, Node::BoolLit { .. })
        | (Node::NullLit {}, Node::NullLit {})
        | (Node::RegexLit { .. }, Node::RegexLit { .. }) => literal_eq(cx, a, b),
        _ => false,
    }
}

/// Literal equality as the plugin compares them: regexes and bigints by their text,
/// everything else by value.
fn literal_eq(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    match (*cx.node(a), *cx.node(b)) {
        (
            Node::RegexLit {
                pattern: p,
                flags: f,
            },
            Node::RegexLit {
                pattern: q,
                flags: g,
            },
        ) => p == q && f == g,
        (Node::BigIntLit { value: x }, Node::BigIntLit { value: y }) => x == y,
        _ => match (static_value(cx, a), static_value(cx, b)) {
            (Some(Static::Num(x)), Some(Static::Num(y))) => x == y,
            (Some(Static::Bool(x)), Some(Static::Bool(y))) => x == y,
            (Some(Static::Null), Some(Static::Null)) => true,
            _ => false,
        },
    }
}

/// The operands of a chain of `op` (left to right, through parentheses, not through TS
/// wrappers): `a && (b && c)` gives `[a, b, c]`.
pub(crate) fn logical_operands(ast: &Ast, id: NodeId, op: BinaryOp) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        match *ast.node(skip_parens(ast, n)) {
            Node::Binary { left, op: o, right } if o == op => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(skip_parens(ast, n)),
        }
    }
    out
}

/// A logical expression that is not an operand of the same operator around it.
pub(crate) fn is_outermost_logical(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Node::Binary { op, .. } = *cx.node(id) else {
        return true;
    };
    let ast = cx.ast();
    let parent = parent_skip_parens(ast, cx.file.parents, id);
    !parent.is_some_and(|p| matches!(*ast.node(p), Node::Binary { op: o, .. } if o == op))
}

/// The runs of non-space text in `span` outside comments: removing each one removes the
/// tokens there while keeping the spaces and comments between them (as a fix removing
/// tokens one by one does).
pub(crate) fn token_runs(ast: &Ast, src: &str, span: Span) -> Vec<Span> {
    let comments = ast.comments_in(span);
    let mut out = Vec::new();
    let mut run: Option<u32> = None;
    let mut ci = 0;
    let mut pos = span.start as usize;
    let end = (span.end as usize).min(src.len());
    while pos < end {
        if let Some(c) = comments.get(ci)
            && c.span.start as usize <= pos
        {
            if let Some(s) = run.take() {
                out.push(Span::new(s, pos as u32));
            }
            pos = (c.span.end as usize).max(pos + 1);
            ci += 1;
            continue;
        }
        let Some(ch) = src[pos..].chars().next() else {
            break;
        };
        if super::unicorn_ast::is_js_space(ch) {
            if let Some(s) = run.take() {
                out.push(Span::new(s, pos as u32));
            }
        } else if run.is_none() {
            run = Some(pos as u32);
        }
        pos += ch.len_utf8();
    }
    if let Some(s) = run {
        out.push(Span::new(s, end as u32));
    }
    out
}

/// A fix deleting each of `spans`.
pub(crate) fn delete_all(spans: impl IntoIterator<Item = Span>) -> Option<Fix> {
    let mut it = spans.into_iter();
    let mut fix = Fix::delete(it.next()?);
    for s in it {
        fix = fix.and(s, "");
    }
    Some(fix)
}

/// A JSX attribute string's value as ESLint's parser gives it: numeric character references
/// (`&#49;`, `&#x31;`, at most 10 characters before the `;`) decoded. Named references are
/// kept as written.
pub(crate) fn decode_jsx_numeric_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find("&#") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let (hex, digits) = match after.strip_prefix(['x', 'X']) {
            Some(d) => (true, d),
            None => (false, after),
        };
        let len = digits
            .bytes()
            .take(10)
            .take_while(|b| {
                if hex {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
            .count();
        let decoded = (len > 0 && digits[len..].starts_with(';'))
            .then(|| u32::from_str_radix(&digits[..len], if hex { 16 } else { 10 }).ok())
            .flatten()
            .and_then(char::from_u32);
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &digits[len + 1..];
            }
            None => {
                out.push_str("&#");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `containsNode` over what evaluating `root` runs: some node satisfies `pred`, not looking
/// inside functions (other than the function node itself) or the values of non-static class
/// fields, whose computed keys still count.
pub(crate) fn evaluates_node(ast: &Ast, root: NodeId, pred: &dyn Fn(&Ast, NodeId) -> bool) -> bool {
    struct Find<'p> {
        pred: &'p dyn Fn(&Ast, NodeId) -> bool,
        found: bool,
        /// Nesting of instance-field keys walked from inside the walk.
        depth: u32,
    }
    impl ts_r_ast::visit::Visitor for Find<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            use ts_r_ast::visit::Walk;
            if (self.pred)(ast, id) {
                self.found = true;
                return Walk::Stop;
            }
            match *ast.node(id) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    Walk::SkipChildren
                }
                Node::PropertyDef { modifiers, key, .. }
                    if !modifiers.contains(Modifiers::STATIC) =>
                {
                    if matches!(ast.node(key), Node::ComputedKey { .. }) && self.depth < 64 {
                        self.depth += 1;
                        ts_r_ast::visit::walk(ast, key, self);
                        self.depth -= 1;
                        if self.found {
                            return Walk::Stop;
                        }
                    }
                    Walk::SkipChildren
                }
                _ => Walk::Continue,
            }
        }
    }
    let mut f = Find {
        pred,
        found: false,
        depth: 0,
    };
    ts_r_ast::visit::walk(ast, root, &mut f);
    f.found
}

/// A direct `eval(...)` call (not `eval?.()`; parentheses and TS wrappers around the callee
/// allowed), which sees the calling scope.
pub(crate) fn is_direct_eval_call(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            ..
        } => ident_name(ast, unwrap_ts(ast, callee)) == Some("eval"),
        _ => false,
    }
}
