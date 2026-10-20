//! `no-array-constructor`: disallow `Array` constructors. Also hosts
//! `@typescript-eslint/no-array-constructor` (`ts: true`).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-array-constructor",
        "Disallow `Array` constructors",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NoArrayConstructor>,
};

pub(crate) fn create_ts(_: &[Value]) -> Result<Box<dyn Rule>, String> {
    Ok(Box::new(NoArrayConstructor { ts: true }))
}

#[derive(Debug, Default)]
struct NoArrayConstructor {
    ts: bool,
}

const MSG: &str = "The array literal notation [] is preferable.";

impl Rule for NoArrayConstructor {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (callee, optional, type_args, args, has_args) = match *cx.node(node) {
            Node::Call {
                callee,
                optional,
                type_args,
                args,
            } => (callee, optional, type_args, args, true),
            Node::New {
                callee,
                type_args,
                has_args,
                args,
            } => (callee, false, type_args, args, has_args),
            _ => return,
        };
        if type_args.is_some() {
            return;
        }
        let inner = skip_parens(ast, callee);
        if ident_name(ast, inner) != Some("Array") {
            return;
        }
        let sem = cx.semantic();
        let shadowed = sem
            .reference_at(inner)
            .is_some_and(|r| matches!(sem.reference(r).resolution, Resolution::Symbol(_)));
        // ESLint needs the `Array` global to exist; the TS rule only needs no shadowing.
        if shadowed || (!self.ts && !cx.is_global_ref(inner, "Array")) {
            return;
        }
        let args = ast.list(args);
        let spread = |a: &NodeId| matches!(cx.node(*a), Node::Spread { .. });
        if args.len() == 1 && !spread(&args[0]) {
            return;
        }
        let span = cx.span(node);
        let src = cx.src();
        let open = if has_args {
            let mut p = cx.span(callee).end as usize;
            loop {
                p = skip_trivia_forward(src, p);
                match src.as_bytes().get(p) {
                    Some(b'(') => break,
                    Some(b'?') | Some(b'.') => p += 1,
                    _ => return,
                }
            }
            p as u32
        } else {
            span.end
        };
        let inside = if has_args {
            &src[open as usize + 1..span.end as usize - 1]
        } else {
            ""
        };
        let semi = if needs_leading_semicolon(ast, src, cx.file.parents, node) {
            ";"
        } else {
            ""
        };
        let fix = Fix::replace(span, format!("{semi}[{inside}]"));
        if self.ts {
            cx.report(span, MSG).id("useLiteral").fix(fix);
            return;
        }
        let non_spread = args.iter().filter(|a| !spread(a)).count();
        let unsafe_spread = args.iter().any(spread) && non_spread <= 1;
        let comments = !ast.comments_in(Span::new(span.start, open)).is_empty();
        let r = cx.report(span, MSG).id("preferLiteral");
        if optional || unsafe_spread || comments {
            let _ = r.suggest("useLiteral", "Replace with an array literal.", fix);
        } else {
            let _ = r.fix(fix);
        }
    }
}
