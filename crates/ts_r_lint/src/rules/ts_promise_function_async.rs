//! `@typescript-eslint/promise-function-async`: a function whose signatures all return a
//! promise must be `async`.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/promise-function-async",
        "Require any function or method that returns a Promise to be marked async",
        NO_PRESET,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<PromiseFunctionAsync>,
};

#[derive(Debug)]
struct PromiseFunctionAsync {
    allow_any: bool,
    promise_names: Vec<String>,
    arrows: bool,
    declarations: bool,
    expressions: bool,
    methods: bool,
}

impl FromOptions for PromiseFunctionAsync {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut promise_names = vec!["Promise".to_owned()];
        promise_names.extend(str_list(o, "allowedPromiseNames"));
        Ok(PromiseFunctionAsync {
            allow_any: bool_field(o, "allowAny", true),
            promise_names,
            arrows: bool_field(o, "checkArrowFunctions", true),
            declarations: bool_field(o, "checkFunctionDeclarations", true),
            expressions: bool_field(o, "checkFunctionExpressions", true),
            methods: bool_field(o, "checkMethodDeclarations", true),
        })
    }
}

impl PromiseFunctionAsync {
    /// `t` is one of the promise names: by its symbol, or through every union / intersection
    /// member or base type (`any_part`: some member or base instead). `any` and `unknown`
    /// never count.
    fn is_promise(&self, ti: &dyn TypeInfo, t: Ty, any_part: bool, depth: u32) -> bool {
        if depth > 16 || ti.flags(t).intersects(TypeFlags::ANY | TypeFlags::UNKNOWN) {
            return false;
        }
        let target = ti.reference_target(t).unwrap_or(t);
        if let Some(s) = ti.type_symbol(target).or_else(|| ti.type_symbol(t)) {
            let name = ti.symbol_name(s);
            if self.promise_names.contains(&name) {
                return true;
            }
        }
        let f = ti.flags(t);
        let members = if f.contains(TypeFlags::UNION) {
            Some(ti.union_members(t))
        } else if f.contains(TypeFlags::INTERSECTION) {
            Some(ti.intersection_members(t))
        } else {
            None
        };
        let check = |p: &Ty| self.is_promise(ti, *p, any_part, depth + 1);
        if let Some(ms) = members {
            return if any_part {
                ms.iter().any(check)
            } else {
                ms.iter().all(check)
            };
        }
        let bases = ti.base_types(target);
        if any_part {
            bases.iter().any(check)
        } else {
            !bases.is_empty() && bases.iter().all(check)
        }
    }

    fn validate(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let ast = cx.ast();
        let (body, return_type, name) = match *ast.node(f) {
            Node::FunctionDecl {
                body,
                return_type,
                name,
                ..
            }
            | Node::FunctionExpr {
                body,
                return_type,
                name,
                ..
            } => (body.is_some(), return_type, name),
            Node::Arrow { return_type, .. } => (true, return_type, None),
            _ => return,
        };
        if !body {
            return;
        }
        let method = cx
            .parent(f)
            .filter(|&p| matches!(*ast.node(p), Node::MethodDef { value, .. } if value == f));
        if let Some(m) = method
            && let Node::MethodDef {
                kind, modifiers, ..
            } = *ast.node(m)
            && (kind != MethodKind::Method || modifiers.contains(Modifiers::ABSTRACT))
        {
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(f).or_else(|| name.and_then(|n| ti.type_at(n))) else {
            return;
        };
        let sigs = ti.signatures(t, false);
        if sigs.is_empty() {
            return;
        }
        let returns: Vec<Ty> = sigs.iter().map(|&s| ti.return_type(s)).collect();
        let mut head = member_function_head_span(ast, cx.src(), cx.file.parents, f);
        // The head starts after the method's decorators.
        if let Some(m) = method
            && let Node::MethodDef { decorators, .. } = *ast.node(m)
            && let Some(&last) = ast.list(decorators).last()
        {
            let after = skip_trivia_forward(cx.src(), cx.span(last).end as usize) as u32;
            head = Span::new(after.min(head.end), head.end);
        }
        if !self.allow_any
            && returns
                .iter()
                .any(|&r| ti.flags(r).intersects(TypeFlags::ANY | TypeFlags::UNKNOWN))
        {
            cx.report(head, "Functions that return promises must be async.")
                .id("missingAsync");
            return;
        }
        if !returns
            .iter()
            .all(|&r| self.is_promise(ti, r, return_type.is_none(), 0))
        {
            return;
        }
        let hybrid = returns.iter().any(|&r| {
            ti.flags(r).contains(TypeFlags::UNION)
                && !ti
                    .union_members(r)
                    .iter()
                    .all(|&p| self.is_promise(ti, p, false, 0))
        });
        let (msg, id) = if hybrid {
            (
                "Functions that return promises must be async. Consider adding an explicit return type annotation if the function is intended to return a union of promise and non-promise types.",
                "missingAsyncHybridReturn",
            )
        } else {
            (
                "Functions that return promises must be async.",
                "missingAsync",
            )
        };
        let fix = match method {
            Some(m) => method_fix(cx, m),
            None => Fix::insert(cx.span(f).start, "async "),
        };
        cx.report(head, msg).id(id).fix(fix);
    }
}

/// `async ` before a method's key (after its decorators and modifiers, before a `*` or the
/// `[` of a computed key), with a space first when the token before touches it.
fn method_fix(cx: &Ctx<'_, '_>, m: NodeId) -> Fix {
    let src = cx.src().as_bytes();
    let Node::MethodDef { key, .. } = *cx.node(m) else {
        return Fix::insert(cx.span(m).start, "async ");
    };
    let floor = cx.span(m).start as usize;
    let mut at = cx.span(key).start as usize;
    let back = |mut p: usize| {
        while p > floor && src[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        p
    };
    for ch in [b'[', b'*'] {
        let p = back(at);
        if p > floor && src[p - 1] == ch {
            at = p - 1;
        }
    }
    let space = at > 0 && !src[at - 1].is_ascii_whitespace();
    Fix::insert(at as u32, if space { " async " } else { "async " })
}

impl Rule for PromiseFunctionAsync {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let check = match *ast.node(node) {
            Node::Arrow { flags, .. } => !flags.contains(FnFlags::ASYNC) && self.arrows,
            Node::FunctionDecl { flags, .. } => {
                !flags.contains(FnFlags::ASYNC) && self.declarations
            }
            Node::FunctionExpr { flags, .. } => {
                let class_method = cx.parent(node).is_some_and(|p| {
                    matches!(*ast.node(p), Node::MethodDef { value, kind: MethodKind::Method, .. } if value == node)
                        && cx.parent(p).is_some_and(|c| {
                            matches!(ast.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                        })
                });
                !flags.contains(FnFlags::ASYNC)
                    && if class_method {
                        self.methods
                    } else {
                        self.expressions
                    }
            }
            _ => false,
        };
        if check {
            self.validate(cx, node);
        }
    }
}
