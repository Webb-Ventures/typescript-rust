//! `preserve-caught-error`: disallow losing originally caught error when re-throwing custom
//! errors.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "preserve-caught-error",
        "Disallow losing originally caught error when re-throwing custom errors",
        ESLINT_REC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<PreserveCaughtError>,
};

#[derive(Debug, Default)]
struct PreserveCaughtError {
    /// Custom error classes: name and 0-based option positions.
    classes: Vec<(String, Vec<usize>)>,
    require_catch_parameter: bool,
}

impl FromOptions for PreserveCaughtError {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut classes: Vec<(String, Vec<usize>)> = Vec::new();
        if let Some(list) = o.get("errorClassNames").and_then(Value::as_array) {
            for v in list {
                let (name, pos) = match v {
                    Value::String(s) => (s.clone(), 1),
                    Value::Object(m) => {
                        let Some(n) = m.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        let p = m
                            .get("argumentPosition")
                            .and_then(Value::as_u64)
                            .unwrap_or(2);
                        (n.to_owned(), (p.max(1) - 1) as usize)
                    }
                    _ => continue,
                };
                match classes.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, ps)) => ps.push(pos),
                    None => classes.push((name, vec![pos])),
                }
            }
        }
        Ok(PreserveCaughtError {
            classes,
            require_catch_parameter: bool_field(o, "requireCatchParameter", false),
        })
    }
}

const BUILTIN_ERRORS: &[&str] = &[
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
];

const MISSING: &str = "There is no `cause` attached to the symptom error being thrown.";
const INCORRECT: &str = "The symptom error is being thrown with an incorrect `cause`.";
const SUGGEST: &str = "Include the original caught error as the `cause` of the symptom error.";

/// How the thrown error is built.
struct Construct {
    /// 0-based option argument positions.
    positions: Vec<usize>,
    builtin: Option<&'static str>,
    args: Vec<NodeId>,
    has_parens: bool,
    callee: NodeId,
}

impl PreserveCaughtError {
    fn construct(&self, cx: &Ctx<'_, '_>, expr: NodeId) -> Option<Construct> {
        let ast = cx.ast();
        let expr = skip_parens(ast, expr);
        let (callee, args, has_parens) = match *ast.node(expr) {
            Node::New {
                callee,
                args,
                has_args,
                ..
            } => (callee, args, has_args),
            Node::Call { callee, args, .. } => (callee, args, true),
            _ => return None,
        };
        let c = skip_parens(ast, callee);
        let name = match *ast.node(c) {
            Node::Ident { name } => ast.atom(name),
            Node::Member { prop, .. } => ident_name(ast, prop)?,
            _ => return None,
        };
        let builtin = BUILTIN_ERRORS
            .iter()
            .find(|b| **b == name)
            .filter(|_| matches!(ast.node(c), Node::Ident { .. }) && cx.is_global_ref(c, name))
            .copied();
        let positions = if let Some(b) = builtin {
            vec![if b == "AggregateError" { 2 } else { 1 }]
        } else {
            self.classes.iter().find(|(n, _)| n == name)?.1.clone()
        };
        Some(Construct {
            positions,
            builtin,
            args: ast.list(args).to_vec(),
            has_parens,
            callee,
        })
    }

    /// The suggestion for a construct without a `cause` option.
    fn add_cause(cx: &Ctx<'_, '_>, c: &Construct, err: &str) -> Option<Fix> {
        let ast = cx.ast();
        let p = *c.positions.first()?;
        if let Some(&opt) = c.args.get(p) {
            let Node::ObjectLit { props } = *ast.node(opt) else {
                return None;
            };
            let props = ast.list(props);
            return Some(match props.last() {
                None => Fix::replace(cx.span(opt), format!("{{cause: {err}}}")),
                Some(&last) => Fix::insert(cx.span(last).end, format!(", cause: {err}")),
            });
        }
        let have = c.args.len();
        let mut fill: Vec<&str> = Vec::new();
        for i in have..p {
            match (c.builtin, i) {
                (Some("AggregateError"), 0) => fill.push("[]"),
                (Some(_), _) => fill.push("\"\""),
                (None, _) => return None,
            }
        }
        fill.push("");
        let opt = format!("{}{{ cause: {err} }}", fill.join(", "));
        if !c.has_parens {
            return Some(Fix::insert(cx.span(c.callee).end, format!("({opt})")));
        }
        match c.args.last() {
            Some(&last) => {
                // Keep the argument's own parentheses.
                let mut end = cx.span(last).end;
                let mut cur = last;
                while let Some(p) = cx.parent(cur) {
                    if matches!(ast.node(p), Node::Paren { .. }) {
                        end = cx.span(p).end;
                        cur = p;
                    } else {
                        break;
                    }
                }
                Some(Fix::insert(end, format!(", {opt}")))
            }
            None => {
                let open = find_char(cx.src(), cx.span(c.callee).end, b'(')?;
                Some(Fix::insert(open + 1, opt))
            }
        }
    }
}

impl Rule for PreserveCaughtError {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::CatchClause]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::CatchClause { param, body, .. } = *cx.node(node) else {
            return;
        };
        // Throw statements of this catch body (not in nested functions or catch clauses).
        let mut throws = Vec::new();
        let mut stack = vec![body];
        while let Some(n) = stack.pop() {
            match ast.kind(n) {
                k if is_function(k) => continue,
                NodeKind::ClassDecl | NodeKind::ClassExpr | NodeKind::CatchClause => continue,
                NodeKind::Throw => throws.push(n),
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        throws.sort_by_key(|&t| cx.span(t).start);
        let mut lost_reported = false;
        for t in throws {
            let Node::Throw { arg } = *ast.node(t) else {
                continue;
            };
            let Some(c) = self.construct(cx, arg) else {
                continue;
            };
            let Some(param) = param else {
                if self.require_catch_parameter {
                    cx.report_node(t, "The caught error is not accessible because the catch clause lacks the error parameter. Start referencing the caught error using the catch parameter.")
                        .id("missingCatchErrorParam");
                }
                continue;
            };
            let Some(err_name) = ident_name(ast, param) else {
                if !lost_reported {
                    lost_reported = true;
                    cx.report_node(node, "Re-throws cannot preserve the caught error as a part of it is being lost due to destructuring.")
                        .id("partiallyLostError");
                }
                continue;
            };
            if c.args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            {
                continue;
            }
            let sem = cx.semantic();
            let err_sym = sem.symbol_of_decl(param);
            // Find the `cause` property among the option objects.
            let mut found: Option<(NodeId, Vec<NodeId>)> = None;
            let mut unknown = false;
            for &p in &c.positions {
                let Some(&opt) = c.args.get(p) else { continue };
                let Node::ObjectLit { props } = *ast.node(skip_parens(ast, opt)) else {
                    unknown = true;
                    continue;
                };
                let props = ast.list(props);
                if props
                    .iter()
                    .any(|&q| matches!(ast.node(q), Node::Spread { .. }))
                {
                    unknown = true;
                    continue;
                }
                let causes: Vec<NodeId> = props
                    .iter()
                    .copied()
                    .filter(|&q| {
                        let key = match *ast.node(q) {
                            Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
                            Node::ShorthandProp { name, .. } => name,
                            _ => return false,
                        };
                        static_key_name(ast, cx.src(), key).as_deref() == Some("cause")
                    })
                    .collect();
                if !causes.is_empty() {
                    found = Some((opt, causes));
                    break;
                }
            }
            if unknown && found.is_none() {
                continue;
            }
            let Some((_, causes)) = found else {
                let fix = Self::add_cause(cx, &c, err_name);
                let mut r = cx.report_node(t, MISSING).id("missingCause");
                if let Some(f) = fix {
                    r = r.suggest("includeCause", SUGGEST, f);
                }
                let _ = r;
                continue;
            };
            let last = *causes.last().unwrap_or(&causes[0]);
            let value = match *ast.node(last) {
                Node::Property { value, .. } => Some(value),
                Node::ShorthandProp { name, .. } => Some(name),
                _ => None,
            };
            if let Some(v) = value {
                let v = skip_parens(ast, v);
                if ident_name(ast, v).is_some() {
                    let s = sem.symbol_of_ident(v);
                    if s.is_some() && s == err_sym {
                        continue;
                    }
                    if ident_name(ast, v) == Some(err_name)
                        && !matches!(ast.node(last), Node::ShorthandProp { .. })
                    {
                        cx.report_node(t, "The caught error is being attached as `cause`, but is shadowed by a closer scoped redeclaration.")
                            .id("caughtErrorShadowed");
                        continue;
                    }
                }
            }
            let (span, fix) = match *ast.node(last) {
                Node::Property { value, .. } => (
                    cx.span(value),
                    Fix::replace(cx.span(skip_parens(ast, value)), err_name),
                ),
                Node::ShorthandProp { .. } => (
                    cx.span(last),
                    Fix::replace(cx.span(last), format!("cause: {err_name}")),
                ),
                Node::MethodDef { key, value, .. } => {
                    let start =
                        find_char(cx.src(), cx.span(key).end, b'(').unwrap_or(cx.span(value).start);
                    (
                        Span::new(start, cx.span(value).end),
                        Fix::replace(cx.span(last), format!("cause: {err_name}")),
                    )
                }
                _ => continue,
            };
            let mut r = cx.report(span, INCORRECT).id("incorrectCause");
            if causes.len() == 1 {
                r = r.suggest("includeCause", SUGGEST, fix);
            }
            let _ = r;
        }
    }
}
