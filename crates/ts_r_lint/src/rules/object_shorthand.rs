//! `object-shorthand`: method and property shorthand in object literals (`always`,
//! `methods`, `properties`, `never`, `consistent`, `consistent-as-needed`).

use super::prelude::*;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "object-shorthand",
        "Require or disallow method and property shorthand syntax for object literals",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ObjectShorthand>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Apply {
    #[default]
    Always,
    Methods,
    Properties,
    Never,
    Consistent,
    ConsistentAsNeeded,
}

#[derive(Debug, Default)]
struct ObjectShorthand {
    apply: Apply,
    ignore_constructors: bool,
    methods_ignore_pattern: Option<String>,
    avoid_quotes: bool,
    avoid_explicit_return_arrows: bool,
}

impl FromOptions for ObjectShorthand {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let apply = match opts.first().and_then(Value::as_str) {
            Some("methods") => Apply::Methods,
            Some("properties") => Apply::Properties,
            Some("never") => Apply::Never,
            Some("consistent") => Apply::Consistent,
            Some("consistent-as-needed") => Apply::ConsistentAsNeeded,
            _ => Apply::Always,
        };
        let o = obj_opt(opts, 1);
        Ok(ObjectShorthand {
            apply,
            ignore_constructors: bool_field(o, "ignoreConstructors", false),
            methods_ignore_pattern: str_field(o, "methodsIgnorePattern")
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
            avoid_quotes: bool_field(o, "avoidQuotes", false),
            avoid_explicit_return_arrows: bool_field(o, "avoidExplicitReturnArrows", false),
        })
    }
}

/// A name whose first character after leading `_`, `$` and digits is an upper-case letter.
fn is_constructor(name: &str) -> bool {
    name.chars()
        .find(|c| !matches!(c, '_' | '$' | '0'..='9'))
        .is_some_and(|c| c.is_alphabetic() && c.to_uppercase().eq(std::iter::once(c)))
}

/// The key's static name (`a`, `'a'`, `[`a`]`), if any.
fn static_name<'a>(cx: &Ctx<'a, '_>, key: NodeId) -> Option<std::borrow::Cow<'a, str>> {
    static_key_name(cx.ast(), cx.src(), key)
}

/// The key and value of a `key: value` property, or of a shorthand method.
fn key_value(ast: &ts_r_ast::Ast, prop: NodeId) -> Option<(NodeId, NodeId)> {
    match *ast.node(prop) {
        Node::Property { key, value, .. } => Some((key, value)),
        Node::MethodDef { key, value, .. } => Some((key, value)),
        Node::ShorthandProp { name, .. } => Some((name, name)),
        _ => None,
    }
}

fn is_proto_key(cx: &Ctx<'_, '_>, key: NodeId) -> bool {
    !matches!(cx.node(key), Node::ComputedKey { .. })
        && static_name(cx, key).is_some_and(|n| n == "__proto__")
}

/// `this`, `super`, `new.target` or the enclosing function's `arguments` in the arrow
/// (not counting nested non-arrow functions).
fn has_lexical_identifiers(cx: &Ctx<'_, '_>, arrow: NodeId) -> bool {
    let ast = cx.ast();
    let in_function = ancestors_inclusive(cx.file.parents, arrow)
        .skip(1)
        .any(|a| {
            matches!(
                ast.node(a),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. }
            )
        });
    let mut stack = vec![arrow];
    let mut found = false;
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::This {} | Node::Super {} => found = true,
            Node::MetaProperty {
                kind: ts_r_ast::MetaPropKind::NewTarget,
            } => found = true,
            Node::Ident { .. }
                if in_function
                    && ident_name(ast, n) == Some("arguments")
                    && cx.semantic().reference_at(n).is_some() =>
            {
                found = true
            }
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } if n != arrow => continue,
            _ => {}
        }
        if found {
            return true;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    false
}

impl ObjectShorthand {
    fn check_consistency(
        &self,
        cx: &mut Ctx<'_, '_>,
        obj: NodeId,
        props: &[NodeId],
        redundancy: bool,
    ) {
        let ast = cx.ast();
        let props: Vec<NodeId> = props
            .iter()
            .copied()
            .filter(|&p| match *ast.node(p) {
                Node::MethodDef { kind, .. } => !matches!(kind, MethodKind::Get | MethodKind::Set),
                Node::Property { .. } | Node::ShorthandProp { .. } => true,
                _ => false,
            })
            .collect();
        if props.is_empty() {
            return;
        }
        let short = props
            .iter()
            .filter(|&&p| {
                matches!(
                    ast.node(p),
                    Node::MethodDef { .. } | Node::ShorthandProp { .. }
                )
            })
            .count();
        if short == props.len() {
            return;
        }
        if short > 0 {
            cx.report_node(
                obj,
                "Unexpected mix of shorthand and non-shorthand properties.",
            )
            .id("unexpectedMix");
            return;
        }
        if !redundancy {
            return;
        }
        let redundant = props.iter().all(|&p| {
            let Node::Property { key, value, .. } = *ast.node(p) else {
                return false;
            };
            if is_proto_key(cx, key) {
                return false;
            }
            match *ast.node(value) {
                Node::FunctionExpr { name, .. } => name.is_none(),
                Node::Ident { .. } => {
                    static_name(cx, key).is_some_and(|k| Some(k.as_ref()) == ident_name(ast, value))
                }
                _ => false,
            }
        });
        if redundant {
            cx.report_node(obj, "Expected shorthand for all properties.")
                .id("expectedAllPropertiesShorthanded");
        }
    }

    fn check_property(&self, cx: &mut Ctx<'_, '_>, prop: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let Some((key, value)) = key_value(ast, prop) else {
            return;
        };
        if let Node::MethodDef { kind, .. } = *ast.node(prop)
            && kind != MethodKind::Method
        {
            return;
        }
        if is_proto_key(cx, key) {
            return;
        }
        let value_inner = skip_parens(ast, value);
        let is_fn = matches!(
            ast.node(value_inner),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        );
        let computed = matches!(ast.node(key), Node::ComputedKey { .. });
        if computed && !is_fn {
            return;
        }
        let never = self.apply == Apply::Never;
        let key_expr = match *ast.node(key) {
            Node::ComputedKey { expr } => skip_parens(ast, expr),
            _ => key,
        };
        let is_string_key = matches!(ast.node(key_expr), Node::StringLit { .. });
        match *ast.node(prop) {
            Node::MethodDef { .. } => {
                if never || (self.avoid_quotes && is_string_key) {
                    let (msg, id) = if never {
                        ("Expected longform method syntax.", "expectedMethodLongform")
                    } else {
                        (
                            "Expected longform method syntax for string literal keys.",
                            "expectedLiteralMethodLongform",
                        )
                    };
                    let flags = match *ast.node(value) {
                        Node::FunctionExpr { flags, .. } => flags,
                        _ => FnFlags::default(),
                    };
                    let mut header = String::from("function");
                    if flags.contains(FnFlags::ASYNC) {
                        header = format!("async {header}");
                    }
                    if flags.contains(FnFlags::GENERATOR) {
                        header.push('*');
                    }
                    let ks = cx.span(key);
                    let fix = Fix::replace(
                        Span::new(cx.span(prop).start, ks.end),
                        format!("{}: {header}", ks.text(src)),
                    );
                    cx.report_node(prop, msg).id(id).fix(fix);
                }
                return;
            }
            Node::ShorthandProp {
                init: None, name, ..
            } => {
                if never {
                    let fix = Fix::insert(cx.span(name).end, format!(": {}", cx.text(name)));
                    cx.report_node(prop, "Expected longform property syntax.")
                        .id("expectedPropertyLongform")
                        .fix(fix);
                }
                return;
            }
            Node::Property { .. } => {}
            _ => return,
        }
        let apply_methods = matches!(self.apply, Apply::Always | Apply::Methods);
        let apply_props = matches!(self.apply, Apply::Always | Apply::Properties);
        let anonymous = match *ast.node(value_inner) {
            Node::FunctionExpr { name, .. } => name.is_none(),
            Node::Arrow { .. } => true,
            _ => false,
        };
        if apply_methods && is_fn && anonymous {
            let name = static_name(cx, key);
            if self.ignore_constructors && name.as_deref().is_some_and(is_constructor) {
                return;
            }
            if let (Some(p), Some(n)) = (&self.methods_ignore_pattern, name.as_deref())
                && regex::compile(p).is_some_and(|re| re.is_match(n, true))
            {
                return;
            }
            if self.avoid_quotes && is_string_key {
                return;
            }
            let report = match *ast.node(value_inner) {
                Node::FunctionExpr { .. } => true,
                Node::Arrow { flags, .. } => {
                    !flags.contains(FnFlags::EXPRESSION_BODY)
                        && self.avoid_explicit_return_arrows
                        && !has_lexical_identifiers(cx, value_inner)
                }
                _ => false,
            };
            if report {
                let fix = self.method_shorthand_fix(cx, prop, key, value_inner);
                cx.report_node(prop, "Expected method shorthand.")
                    .id("expectedMethodShorthand")
                    .fix_opt(fix);
            }
            return;
        }
        let Node::Ident { .. } = *ast.node(value_inner) else {
            return;
        };
        let Some(vname) = ident_name(ast, value_inner) else {
            return;
        };
        let same = match *ast.node(key) {
            Node::Ident { .. } => ident_name(ast, key) == Some(vname),
            Node::StringLit { value: v } => {
                if ast.atom(v) != vname {
                    return;
                }
                if self.avoid_quotes {
                    return;
                }
                true
            }
            _ => false,
        };
        if !same || !apply_props {
            return;
        }
        let comments = ast.comments_in(cx.span(prop));
        if comments.iter().any(|c| {
            let t = c.span.text(src);
            t.starts_with("/*") && t[2..].trim_start().starts_with('*') && t.contains("@type")
        }) {
            return;
        }
        let fix = comments
            .is_empty()
            .then(|| Fix::replace(cx.span(prop), vname.to_string()));
        cx.report_node(prop, "Expected property shorthand.")
            .id("expectedPropertyShorthand")
            .fix_opt(fix);
    }

    fn method_shorthand_fix(
        &self,
        cx: &Ctx<'_, '_>,
        prop: NodeId,
        key: NodeId,
        value: NodeId,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let ks = cx.span(key);
        let vspan = cx.span(value);
        if !ast.comments_in(Span::new(ks.end, vspan.start)).is_empty() {
            return None;
        }
        let (flags, params) = match *ast.node(value) {
            Node::FunctionExpr { flags, params, .. } | Node::Arrow { flags, params, .. } => {
                (flags, params)
            }
            _ => return None,
        };
        let mut text = String::new();
        if flags.contains(FnFlags::ASYNC) {
            text.push_str("async ");
        }
        if flags.contains(FnFlags::GENERATOR) {
            text.push('*');
        }
        text.push_str(ks.text(src));
        let pend = cx.span(prop).end;
        let vtext = vspan.text(src);
        match *ast.node(value) {
            Node::FunctionExpr { .. } => {
                let kw = vtext.find("function")? + "function".len();
                let mut from = vspan.start as usize + kw;
                if flags.contains(FnFlags::GENERATOR) {
                    from = skip_trivia_forward(src, from);
                    if !src[from..].starts_with('*') {
                        return None;
                    }
                    from += 1;
                }
                text.push_str(&src[from..vspan.end as usize]);
            }
            Node::Arrow { body, .. } => {
                // Only trivia separates `=>` from the body, so step back over it (comments
                // included: a `=>` inside one is not the arrow).
                let arrow = skip_trivia_backward(ast, src, cx.span(body).start as usize)
                    .checked_sub(2)
                    .filter(|&a| src.as_bytes()[a..].starts_with(b"=>"))?;
                let mut first = vspan.start as usize;
                if flags.contains(FnFlags::ASYNC) {
                    first = skip_trivia_forward(src, first + "async".len());
                }
                let before_arrow = skip_trivia_backward(ast, src, arrow);
                let old = &src[first..before_arrow];
                let list = ast.list(params);
                if list.len() == 1 && cx.span(list[0]).start as usize == first {
                    text.push('(');
                    text.push_str(old);
                    text.push(')');
                } else {
                    text.push_str(old);
                }
                text.push_str(&src[arrow + 2..vspan.end as usize]);
            }
            _ => return None,
        }
        Some(Fix::replace(Span::new(ks.start, pend), text))
    }
}

impl Rule for ObjectShorthand {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ObjectLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::ObjectLit { props } = *cx.node(node) else {
            return;
        };
        if is_assignment_target(ast, cx.file.parents, node) {
            return;
        }
        let props = ast.list(props);
        match self.apply {
            Apply::Consistent => self.check_consistency(cx, node, props, false),
            Apply::ConsistentAsNeeded => self.check_consistency(cx, node, props, true),
            _ => {}
        }
        for &p in props {
            self.check_property(cx, p);
        }
    }
}
