//! `unicorn/operator-assignment`: ESLint's `operator-assignment` (`"always"`, the default:
//! `x = x + y` becomes `x += y`; `"never"`: `x += y` becomes `x = x + y`) plus, under
//! `"always"`, `x = \`${x}...\`` with a `+= \`...\`` suggestion.
//!
//! - `"always"`: an `=` whose right side is a binary with a shorthand operator. The left
//!   side matching the binary's left operand is fixed (only when the target is a name or a
//!   member of a name or `this` with a static key, and no comment lies between `=` and the
//!   operator); matching the right operand of a commutative operator (`*`, `&`, `|`, `^`) is
//!   reported without a fix.
//! - `"never"`: every compound assignment except the logical ones; fixed for the same
//!   targets, wrapping the right side in parentheses when it binds looser than the operator.
//! - Template form: the target is a name, the template's first part is empty and its first
//!   expression is that name, and something follows it; no suggestion when a comment lies
//!   outside the kept tail.

use super::prelude::*;
use super::unicorn_text::would_remove_comments;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/operator-assignment",
        "Require assignment operator shorthand where possible.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<OperatorAssignment>,
};

#[derive(Debug)]
struct OperatorAssignment {
    never: bool,
}

impl FromOptions for OperatorAssignment {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(OperatorAssignment {
            never: opts.first().and_then(Value::as_str) == Some("never"),
        })
    }
}

fn shorthand(op: BinaryOp) -> Option<bool> {
    use BinaryOp::*;
    match op {
        Mul | BitAnd | BitXor | BitOr => Some(true),
        Add | Sub | Div | Mod | Shl | Shr | UShr | Exp => Some(false),
        _ => None,
    }
}

fn assign_to_binary(op: AssignOp) -> Option<BinaryOp> {
    use AssignOp::*;
    Some(match op {
        AddAssign => BinaryOp::Add,
        SubAssign => BinaryOp::Sub,
        MulAssign => BinaryOp::Mul,
        DivAssign => BinaryOp::Div,
        ModAssign => BinaryOp::Mod,
        ExpAssign => BinaryOp::Exp,
        ShlAssign => BinaryOp::Shl,
        ShrAssign => BinaryOp::Shr,
        UShrAssign => BinaryOp::UShr,
        BitOrAssign => BinaryOp::BitOr,
        BitXorAssign => BinaryOp::BitXor,
        BitAndAssign => BinaryOp::BitAnd,
        _ => return None,
    })
}

/// ESLint's `isSameReference` (through parentheses and optional chains): the same name,
/// `this`/`super`, equal literals, or member accesses with the same object and property.
/// With `static_keys`, `a.b` and `a["b"]` are the same; without, the key forms must match.
pub(crate) fn eslint_same_reference(ast: &Ast, a: NodeId, b: NodeId, static_keys: bool) -> bool {
    same_ref(ast, a, b, static_keys, 0)
}

fn static_prop(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => match *ast.node(skip_parens(ast, index)) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            Node::NumberLit { value } => Some(number_to_string(value.get())),
            Node::BoolLit { value } => Some(value.to_string()),
            Node::NullLit {} => Some("null".to_owned()),
            Node::BigIntLit { value } => parse_bigint(ast.atom(value)),
            Node::TemplateLit { parts } => match ast.list(parts) {
                [only] => match *ast.node(*only) {
                    Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn same_ref(ast: &Ast, a: NodeId, b: NodeId, static_keys: bool, depth: u32) -> bool {
    if depth > 200 {
        return false;
    }
    let a = skip_parens(ast, a);
    let b = skip_parens(ast, b);
    if static_keys
        && matches!(ast.node(a), Node::Member { .. } | Node::Index { .. })
        && let Some(name) = static_prop(ast, a)
    {
        let (Node::Member { obj: o1, .. } | Node::Index { obj: o1, .. }) = *ast.node(a) else {
            return false;
        };
        let (Node::Member { obj: o2, .. } | Node::Index { obj: o2, .. }) = *ast.node(b) else {
            return false;
        };
        return same_ref(ast, o1, o2, static_keys, depth + 1)
            && static_prop(ast, b).as_deref() == Some(name.as_str());
    }
    match (*ast.node(a), *ast.node(b)) {
        (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
        (Node::Ident { name: x }, Node::Ident { name: y })
        | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => {
            ast.atom(x) == ast.atom(y)
        }
        (Node::NumberLit { value: x }, Node::NumberLit { value: y }) => x.get() == y.get(),
        (Node::StringLit { value: x }, Node::StringLit { value: y }) => ast.atom(x) == ast.atom(y),
        (Node::BoolLit { value: x }, Node::BoolLit { value: y }) => x == y,
        (Node::NullLit {}, Node::NullLit {}) => true,
        (Node::BigIntLit { value: x }, Node::BigIntLit { value: y }) => {
            parse_bigint(ast.atom(x)) == parse_bigint(ast.atom(y))
        }
        (
            Node::RegexLit {
                pattern: p1,
                flags: f1,
            },
            Node::RegexLit {
                pattern: p2,
                flags: f2,
            },
        ) => ast.atom(p1) == ast.atom(p2) && ast.atom(f1) == ast.atom(f2),
        (
            Node::Member {
                obj: o1, prop: p1, ..
            },
            Node::Member {
                obj: o2, prop: p2, ..
            },
        )
        | (
            Node::Index {
                obj: o1, index: p1, ..
            },
            Node::Index {
                obj: o2, index: p2, ..
            },
        ) => {
            same_ref(ast, o1, o2, static_keys, depth + 1)
                && same_ref(ast, p1, p2, static_keys, depth + 1)
        }
        _ => false,
    }
}

/// ESLint's `canBeFixed`: a name, or a member of a name or `this` whose key is static.
fn can_be_fixed(ast: &Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => true,
        Node::Member { obj, .. } => matches!(
            ast.node(skip_parens(ast, obj)),
            Node::Ident { .. } | Node::This {}
        ),
        Node::Index { obj, index, .. } => {
            matches!(
                ast.node(skip_parens(ast, obj)),
                Node::Ident { .. } | Node::This {}
            ) && matches!(
                ast.node(skip_parens(ast, index)),
                Node::StringLit { .. }
                    | Node::NumberLit { .. }
                    | Node::BoolLit { .. }
                    | Node::NullLit {}
                    | Node::RegexLit { .. }
                    | Node::BigIntLit { .. }
            )
        }
        _ => false,
    }
}

/// The end of `id` with its parentheses.
fn outer_end(cx: &Ctx<'_, '_>, id: NodeId) -> u32 {
    super::unicorn_text::paren_span(cx, id).end
}

fn binary_op_precedence(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        BitOr => 6,
        BitXor => 7,
        BitAnd => 8,
        Shl | Shr | UShr => 11,
        Add | Sub => 12,
        Mul | Div | Mod => 13,
        _ => 14,
    }
}

impl Rule for OperatorAssignment {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::Assign { left, op, right } = *cx.node(node) else {
            return;
        };
        let span = cx.span(node);
        let eq_at = skip_trivia_forward(src, outer_end(cx, left) as usize) as u32;
        if self.never {
            let Some(bop) = assign_to_binary(op) else {
                return;
            };
            let msg = format!(
                "Unexpected operator assignment ({}) shorthand.",
                op.as_str()
            );
            let mut fix = None;
            let op_end = eq_at + op.as_str().len() as u32;
            let after = skip_trivia_forward(src, op_end as usize) as u32;
            if can_be_fixed(ast, left) && ast.comments_in(Span::new(op_end, after)).is_empty() {
                let left_text = &src[span.start as usize..eq_at as usize];
                let new_op = bop.as_str();
                let r = skip_parens(ast, right);
                let right_text = if r == right && precedence(ast, r) <= binary_op_precedence(bop) {
                    format!(
                        "{}({})",
                        &src[op_end as usize..cx.span(r).start as usize],
                        cx.text(r)
                    )
                } else {
                    let mut prefix = "";
                    if op_end == after {
                        let next = src[after as usize..].chars().next().unwrap_or(' ');
                        let last = new_op.chars().last().unwrap_or(' ');
                        if (last == '+' && next == '+')
                            || (last == '-' && next == '-')
                            || (new_op == "/" && next == '/')
                        {
                            prefix = " ";
                        }
                    }
                    format!("{prefix}{}", &src[op_end as usize..span.end as usize])
                };
                fix = Some(Fix::replace(
                    span,
                    format!("{left_text}= {left_text}{new_op}{right_text}"),
                ));
            }
            cx.report(span, msg).id("unexpected").fix_opt(fix);
            return;
        }
        if op != AssignOp::Assign {
            return;
        }
        let expr = skip_parens(ast, right);
        let msg = "Assignment (=) can be replaced with operator assignment (+=).";
        match *ast.node(expr) {
            Node::Binary {
                left: bl,
                op: bop,
                right: br,
            } => {
                let Some(commutative) = shorthand(bop) else {
                    return;
                };
                let msg = format!(
                    "Assignment (=) can be replaced with operator assignment ({}=).",
                    bop.as_str()
                );
                if same_ref(ast, left, bl, false, 0) {
                    let mut fix = None;
                    if can_be_fixed(ast, left) {
                        let op_at = skip_trivia_forward(src, outer_end(cx, bl) as usize) as u32;
                        let op_end = op_at + bop.as_str().len() as u32;
                        if ast.comments_in(Span::new(eq_at + 1, op_at)).is_empty() {
                            let left_text = &src[span.start as usize..eq_at as usize];
                            let right_text = &src[op_end as usize..cx.span(right).end as usize];
                            fix = Some(Fix::replace(
                                span,
                                format!("{left_text}{}={right_text}", bop.as_str()),
                            ));
                        }
                    }
                    cx.report(span, msg).id("replaced").fix_opt(fix);
                } else if commutative && same_ref(ast, left, br, false, 0) {
                    cx.report(span, msg).id("replaced");
                }
            }
            Node::TemplateLit { parts } => {
                let target = skip_parens(ast, left);
                let Node::Ident { name } = *ast.node(target) else {
                    return;
                };
                let parts = ast.list(parts);
                if parts.len() < 3 || !cx.span(parts[0]).is_empty() {
                    return;
                }
                let Node::Ident { name: first } = *ast.node(skip_parens(ast, parts[1])) else {
                    return;
                };
                if ast.atom(first) != ast.atom(name) {
                    return;
                }
                let tail = Span::new(cx.span(parts[2]).start, cx.span(expr).end);
                let tail_text = format!("`{}", tail.text(src));
                if tail_text == "``" {
                    return;
                }
                let keep = !would_remove_comments(cx, span, &[tail]);
                let text = format!("{} += {tail_text}", cx.text(target));
                let r = cx.report(span, msg).id("replaced");
                if keep {
                    r.suggest(
                        "operator-assignment/suggestion",
                        "Use `+=` assignment.",
                        Fix::replace(span, text),
                    );
                }
            }
            _ => {}
        }
    }
}
