//! `unicorn/no-instanceof-builtins`: no `instanceof` with built-in constructors, whose
//! checks fail across realms.
//!
//! The right side is a constructor name, or `globalThis`/`window`/`self`/`global` (not a
//! local binding) `.Name`; names in `exclude` are skipped. By name:
//! - `Array` (and `Error` with `useErrorIsError`) is fixed to `Array.isArray(x)` /
//!   `Error.isError(x)`, through the same global object when one was written;
//! - `Function` is fixed to `typeof x === 'function'`;
//! - `String`, `Number`, `Boolean`, `BigInt`, `Symbol` get that `typeof` form as a
//!   suggestion;
//! - anything else is reported only with `strategy: "strict"` (errors, collections, typed
//!   arrays, `Object`, `RegExp`, `Promise`, `Proxy`, `DataView`, `Date`, ...) or when
//!   listed in `include`.
//!
//! Fixes and suggestions are dropped when a comment in the expression lies outside the left
//! operand (with its parentheses). The `typeof` form is parenthesized where a comparison
//! would bind more loosely than `instanceof` did (`a === b instanceof Function`, a member
//! object, `as`), gets a leading `;` when it then starts a statement after one that would
//! continue, and wraps a left operand that binds more loosely than `typeof` (`a + b`).

use super::prelude::*;
use super::unicorn_prefer_date_now::keyword_spacing;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-instanceof-builtins",
        "Disallow `instanceof` with built-in objects",
        NO_PRESET,
        Category::Suspicious,
    )
    .fixable()
    .suggestions(),
    create: create::<NoInstanceofBuiltins>,
};

const STRICT: &[&str] = &[
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
    "SuppressedError",
    "Map",
    "Set",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "ArrayBuffer",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
    "Object",
    "RegExp",
    "Promise",
    "Proxy",
    "DataView",
    "Date",
    "SharedArrayBuffer",
    "FinalizationRegistry",
];

const MESSAGE: &str =
    "Avoid using `instanceof` for type checking as it can lead to unreliable results.";

#[derive(Debug, Default)]
struct NoInstanceofBuiltins {
    use_error_is_error: bool,
    strict: bool,
    include: Vec<String>,
    exclude: Vec<String>,
}

impl FromOptions for NoInstanceofBuiltins {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoInstanceofBuiltins {
            use_error_is_error: bool_field(o, "useErrorIsError", false),
            strict: str_field(o, "strategy") == Some("strict"),
            include: str_list(o, "include"),
            exclude: str_list(o, "exclude"),
        })
    }
}

impl Rule for NoInstanceofBuiltins {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Binary {
            left,
            op: BinaryOp::InstanceOf,
            right,
        } = *cx.node(node)
        else {
            return;
        };
        let Some((name, reference)) = constructor(cx, right) else {
            return;
        };
        if self.exclude.iter().any(|e| e == name) {
            return;
        }
        let keeps_comments = !comments_outside_left(cx, node, left);
        let wrapper = matches!(name, "String" | "Number" | "Boolean" | "BigInt" | "Symbol");
        if name == "Array" || (name == "Error" && self.use_error_is_error) {
            let method = if name == "Array" {
                "isArray"
            } else {
                "isError"
            };
            let fix = keeps_comments
                .then(|| call_fix(cx, node, left, right, &format!("{reference}.{method}")));
            cx.report_node(node, MESSAGE)
                .id("no-instanceof-builtins")
                .fix_opt(fix);
        } else if name == "Function" {
            let fix = keeps_comments.then(|| typeof_fix(cx, node, left, right, "function"));
            cx.report_node(node, MESSAGE)
                .id("no-instanceof-builtins")
                .fix_opt(fix);
        } else if wrapper {
            let ty = name.to_ascii_lowercase();
            let fix = keeps_comments.then(|| typeof_fix(cx, node, left, right, &ty));
            let report = cx.report_node(node, MESSAGE).id("no-instanceof-builtins");
            if let Some(fix) = fix {
                report.suggest(
                    "switch-to-type-of",
                    format!("Switch to `typeof … === '{ty}'`."),
                    fix,
                );
            }
        } else if (self.strict && STRICT.contains(&name)) || self.include.iter().any(|i| i == name)
        {
            cx.report_node(node, MESSAGE).id("no-instanceof-builtins");
        }
    }
}

/// The constructor's name and the text that names it (`Array`, `globalThis.Array`).
fn constructor<'a>(cx: &Ctx<'_, 'a>, right: NodeId) -> Option<(&'a str, &'a str)> {
    let ast = cx.ast();
    let right = skip_parens(ast, right);
    match *ast.node(right) {
        Node::Ident { name } => Some((ast.atom(name), ast.atom(name))),
        Node::Member { obj, prop, .. } => {
            let name = ident_name(ast, prop)?;
            let object = ident_name(ast, obj)?;
            if !matches!(object, "global" | "globalThis" | "self" | "window") || !is_global(cx, obj)
            {
                return None;
            }
            Some((name, cx.span(right).text(cx.src())))
        }
        _ => None,
    }
}

/// A reference to a global: built in, configured, or unresolved.
fn is_global(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let sem = cx.semantic();
    match sem.reference_at(ident).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Symbol(s)) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
        _ => true,
    }
}

fn comments_outside_left(cx: &Ctx<'_, '_>, node: NodeId, left: NodeId) -> bool {
    let keep = cx.span(left);
    cx.ast()
        .comments_in(cx.span(node))
        .iter()
        .any(|c| !(c.span.start >= keep.start && c.span.end <= keep.end))
}

fn with_spacing(cx: &Ctx<'_, '_>, node: NodeId, mut fix: Fix) -> Fix {
    for (at, text) in keyword_spacing(cx, node) {
        fix = fix.and(Span::at(at), text);
    }
    fix
}

/// The `instanceof` keyword after the left operand (no comments can come between).
fn instanceof_span(cx: &Ctx<'_, '_>, left: NodeId) -> Span {
    let start = skip_trivia_forward(cx.src(), cx.span(left).end as usize) as u32;
    Span::new(start, start + "instanceof".len() as u32)
}

/// `fn(left)`: drops `instanceof` and the right side with the spaces before each of their
/// tokens, keeping a line break that starts those spaces.
fn call_fix(cx: &Ctx<'_, '_>, node: NodeId, left: NodeId, right: NodeId, function: &str) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let left_span = cx.span(left);
    let mut tokens = vec![instanceof_span(cx, left)];
    let mut closes = Vec::new();
    let mut r = right;
    while let Node::Paren { expr } = *ast.node(r) {
        let s = cx.span(r);
        tokens.push(Span::new(s.start, s.start + 1));
        closes.push(Span::new(s.end - 1, s.end));
        r = expr;
    }
    tokens.push(cx.span(r));
    tokens.extend(closes.into_iter().rev());
    let mut kept = String::from(")");
    let mut pos = left_span.end;
    for t in tokens {
        let gap = src.get(pos as usize..t.start as usize).unwrap_or("");
        if gap.starts_with("\r\n") {
            kept.push_str("\r\n");
        } else if gap.starts_with(['\n', '\r']) {
            kept.push_str(&gap[..1]);
        }
        pos = t.end;
    }
    // Spacing goes first so that a space before the expression precedes `fn(`.
    with_spacing(cx, node, Fix::default())
        .and(Span::at(left_span.start), format!("{function}("))
        .and(Span::new(left_span.end, cx.span(node).end), kept)
}

/// `typeof left === 'type'`.
fn typeof_fix(cx: &Ctx<'_, '_>, node: NodeId, left: NodeId, right: NodeId, ty: &str) -> Fix {
    let ast = cx.ast();
    let parens = comparison_needs_parens(cx, node);
    let mut prefix = if parens { "(" } else { "" }.to_owned();
    if parens && starts_continued_statement(cx, node) {
        prefix.insert(0, ';');
    }
    let left_span = cx.span(left);
    let inner = ast.node(left);
    let wrap_left = !matches!(inner, Node::Paren { .. })
        && (precedence(ast, left) < 16
            || matches!(
                inner,
                Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
            ));
    let mut fix = with_spacing(cx, node, Fix::default());
    if wrap_left {
        fix = fix
            .and(Span::at(left_span.start), format!("{prefix}typeof ("))
            .and(Span::at(left_span.end), ")");
    } else {
        fix = fix.and(Span::at(left_span.start), format!("{prefix}typeof "));
    }
    let close = if parens { ")" } else { "" };
    fix.and(instanceof_span(cx, left), "===")
        .and(cx.span(right), format!("'{ty}'{close}"))
}

/// Replacing `node` with `a === b` changes how it groups with its parent.
fn comparison_needs_parens(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(parent) = cx.parent(node) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Paren { .. } => false,
        Node::Member { obj, .. } | Node::Index { obj, .. } => obj == node,
        Node::Call { callee, .. } | Node::New { callee, .. } => callee == node,
        Node::TaggedTemplate { tag, .. } => tag == node,
        Node::Binary { right, .. } => {
            let p = precedence(ast, parent);
            p > 9 || (p == 9 && right == node)
        }
        Node::As { .. } | Node::Satisfies { .. } => precedence(ast, parent) > 9,
        Node::Await { .. }
        | Node::NonNull { .. }
        | Node::TypeAssertion { .. }
        | Node::Unary { .. } => true,
        _ => false,
    }
}

/// `node` starts an expression statement that follows one a `(` (or `[`, ...) would
/// continue.
pub(crate) fn starts_continued_statement(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let start = cx.span(node).start;
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if cx.span(p).start != start {
            return false;
        }
        if matches!(ast.node(p), Node::ExprStmt { .. }) {
            return prev_statement(ast, cx.file.parents, p)
                .is_some_and(|prev| stmt_continuable(ast, cx.src(), prev));
        }
        cur = p;
    }
    false
}
