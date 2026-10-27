//! `@typescript-eslint/method-signature-style`: function members of types are written as
//! properties (`f: () => void`, the default) or as methods (`f(): void`).

use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/method-signature-style",
        "Enforce using a particular method signature syntax",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<MethodSignatureStyle>,
};

#[derive(Debug)]
struct MethodSignatureStyle {
    method: bool,
}

impl FromOptions for MethodSignatureStyle {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(MethodSignatureStyle {
            method: opts.first().and_then(Value::as_str) == Some("method"),
        })
    }
}

struct FindThis(bool);

impl Visitor for FindThis {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        if matches!(ast.node(id), Node::ThisType {}) {
            self.0 = true;
            return Walk::Stop;
        }
        Walk::Continue
    }
}

/// `key`, `[key]` or `key?` as written.
fn method_key(cx: &Ctx<'_, '_>, key: NodeId, optional: bool) -> String {
    let src = cx.src();
    let mut k = match *cx.node(key) {
        Node::ComputedKey { expr } => format!("[{}]", cx.span(expr).text(src)),
        _ => cx.span(key).text(src).to_owned(),
    };
    if optional {
        k.push('?');
    }
    k
}

/// `<T>(a, b)` as written, or `()`.
fn method_params(
    cx: &Ctx<'_, '_>,
    type_params: Option<NodeId>,
    params: ts_r_ast::NodeList,
) -> String {
    let src = cx.src();
    let list = cx.ast().list(params);
    let mut out = String::new();
    if let Some(tp) = type_params {
        out.push_str(cx.span(tp).text(src));
    }
    match (list.first(), list.last()) {
        (Some(&first), Some(&last)) => {
            let before = skip_trivia_backward(cx.ast(), src, cx.span(first).start as usize);
            let open =
                (src.as_bytes().get(before.wrapping_sub(1)) == Some(&b'(')).then(|| before - 1);
            let close = find_char(src, cx.span(last).end, b')');
            match (open, close) {
                (Some(o), Some(c)) => out.push_str(&src[o..c as usize + 1]),
                _ => out.push_str("()"),
            }
        }
        _ => out.push_str("()"),
    }
    out
}

fn return_text(cx: &Ctx<'_, '_>, rt: Option<NodeId>) -> String {
    rt.map_or_else(
        || "any".to_owned(),
        |t| cx.span(t).text(cx.src()).to_owned(),
    )
}

/// The member's own `;` or `,`, if its text ends with one.
fn delimiter(cx: &Ctx<'_, '_>, node: NodeId) -> &'static str {
    let t = cx.span(node).text(cx.src()).trim_end();
    if t.ends_with(';') {
        ";"
    } else if t.ends_with(',') {
        ","
    } else {
        ""
    }
}

fn in_module(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = cx.parent(node);
    while let Some(p) = cur {
        if matches!(cx.node(p), Node::Module { .. }) {
            return true;
        }
        cur = cx.parent(p);
    }
    false
}

impl MethodSignatureStyle {
    /// `dups`: the other method signatures of the same type with the same key.
    fn check_method(&self, node: NodeId, cx: &mut Ctx<'_, '_>, dups: Vec<NodeId>) {
        let Node::MethodSignature {
            kind: MethodKind::Method,
            key,
            optional,
            type_params,
            params,
            return_type,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        let ast = cx.ast();
        let skip_fix = return_type.is_some_and(|rt| {
            let mut f = FindThis(false);
            walk(ast, rt, &mut f);
            f.0
        });
        let my_key = method_key(cx, key, optional);
        let fix = if in_module(cx, node) || skip_fix {
            None
        } else {
            let sig = |m: NodeId| match *cx.node(m) {
                Node::MethodSignature {
                    type_params,
                    params,
                    return_type,
                    ..
                } => format!(
                    "({} => {})",
                    method_params(cx, type_params, params),
                    return_text(cx, return_type)
                ),
                _ => String::new(),
            };
            let delim = delimiter(cx, node);
            if dups.is_empty() {
                Some(Fix::replace(
                    cx.span(node),
                    format!(
                        "{my_key}: {} => {}{delim}",
                        method_params(cx, type_params, params),
                        return_text(cx, return_type)
                    ),
                ))
            } else {
                let mut all = dups.clone();
                all.push(node);
                all.sort_by_key(|&m| cx.span(m).start);
                let ty = all.iter().map(|&m| sig(m)).collect::<Vec<_>>().join(" & ");
                let mut fix = Fix::replace(cx.span(node), format!("{my_key}: {ty}{delim}"));
                let src = cx.src();
                for &d in &dups {
                    let end = cx.span(d).end;
                    let next = skip_trivia_forward(src, end as usize);
                    if next < src.len() {
                        fix = fix.and(Span::new(cx.span(d).start, next as u32), "");
                    }
                }
                Some(fix)
            }
        };
        cx.report_node(
            node,
            "Shorthand method signature is forbidden. Use a function property instead.",
        )
        .id("errorMethod")
        .fix_opt(fix);
    }

    fn check_property(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::PropertySignature {
            modifiers,
            key,
            optional,
            type_ann: Some(t),
        } = *cx.node(node)
        else {
            return;
        };
        let Node::FunctionType {
            type_params,
            params,
            return_type,
        } = *cx.node(t)
        else {
            return;
        };
        let text = format!(
            "{}{}: {}{}",
            method_key(cx, key, optional),
            method_params(cx, type_params, params),
            return_text(cx, Some(return_type)),
            delimiter(cx, node)
        );
        let fix = Fix::replace(cx.span(node), text);
        let r = cx
            .report_node(
                node,
                "Function property signature is forbidden. Use a method shorthand instead.",
            )
            .id("errorProperty");
        if modifiers.contains(Modifiers::READONLY) {
            r.suggest(
                "convertToMethodSignature",
                "Convert to a method signature. This removes the `readonly` modifier, allowing the member to be reassigned.",
                fix,
            );
        } else {
            r.fix(fix);
        }
    }
}

impl Rule for MethodSignatureStyle {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Interface, NodeKind::TypeLiteral]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let members = match *cx.node(node) {
            Node::Interface { body: members, .. } | Node::TypeLiteral { members } => {
                cx.ast().list(members)
            }
            _ => return,
        };
        if self.method {
            for &m in members {
                self.check_property(m, cx);
            }
            return;
        }
        // Method signatures grouped by key, in source order.
        let mut by_key: rustc_hash::FxHashMap<String, Vec<NodeId>> = Default::default();
        for &m in members {
            if let Node::MethodSignature { key, optional, .. } = *cx.node(m) {
                by_key
                    .entry(method_key(cx, key, optional))
                    .or_default()
                    .push(m);
            }
        }
        for &m in members {
            let Node::MethodSignature { key, optional, .. } = *cx.node(m) else {
                continue;
            };
            let same = &by_key[&method_key(cx, key, optional)];
            let dups = if same.len() > 1 {
                same.iter().copied().filter(|&d| d != m).collect()
            } else {
                Vec::new()
            };
            self.check_method(m, cx, dups);
        }
    }
}
