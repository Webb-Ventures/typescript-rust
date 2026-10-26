//! `radix`: require the radix argument of `parseInt()` and check that it is valid.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "radix",
        "Enforce the consistent use of the radix argument when using `parseInt()`",
        NO_PRESET,
        Category::Pedantic,
    )
    .suggestions(),
    create: create_default::<Radix>,
};

/// `"always"` and the deprecated `"as-needed"` behave the same.
#[derive(Debug, Default)]
struct Radix;

/// False for a radix that is statically not an integer from 2 to 36.
fn valid_radix(cx: &Ctx<'_, '_>, radix: NodeId) -> bool {
    let ast = cx.ast();
    let radix = skip_parens(ast, radix);
    let number = match *ast.node(radix) {
        Node::NumberLit { value } => Some(value.get()),
        Node::Unary {
            op: op @ (UnaryOp::Minus | UnaryOp::Plus),
            arg,
        } => match *ast.node(skip_parens(ast, arg)) {
            Node::NumberLit { value } if op == UnaryOp::Minus => Some(-value.get()),
            Node::NumberLit { value } => Some(value.get()),
            _ => return true,
        },
        Node::StringLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. } => return false,
        _ => return !cx.is_global_ref(radix, "undefined"),
    };
    number.is_some_and(|n| {
        n.fract() == 0.0 && (2.0..=36.0).contains(&n) && n.to_bits() != (-0.0f64).to_bits()
    })
}

impl Rule for Radix {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Call { callee, args, .. } = *cx.node(node) else {
            return;
        };
        let callee = skip_parens(ast, callee);
        let is_parse_int = cx.is_global_ref(callee, "parseInt")
            || member_parts(ast, cx.src(), callee).is_some_and(|(obj, name)| {
                name == "parseInt"
                    && !matches!(ast.node(callee), Node::Member { prop, .. }
                        if matches!(ast.node(*prop), Node::PrivateName { .. }))
                    && cx.is_global_ref(obj, "Number")
            });
        if !is_parse_int {
            return;
        }
        let args = ast.list(args);
        let spread = |i: usize| {
            args.get(i)
                .is_some_and(|&a| matches!(ast.node(a), Node::Spread { .. }))
        };
        match args.len() {
            0 => {
                cx.report(cx.span(node), "Missing parameters.")
                    .id("missingParameters");
            }
            1 if !spread(0) => {
                let end = cx.span(args[0]).end;
                cx.report(cx.span(node), "Missing radix parameter.")
                    .id("missingRadix")
                    .suggest(
                        "addRadixParameter10",
                        "Add radix parameter `10` for parsing decimal numbers.",
                        Fix::insert(end, ", 10"),
                    );
            }
            1 => {}
            _ if spread(0) || spread(1) => {}
            _ if !valid_radix(cx, args[1]) => {
                cx.report(
                    cx.span(node),
                    "Invalid radix parameter, must be an integer between 2 and 36.",
                )
                .id("invalidRadix");
            }
            _ => {}
        }
    }
}
