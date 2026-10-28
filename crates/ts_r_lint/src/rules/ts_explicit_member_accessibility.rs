//! `@typescript-eslint/explicit-member-accessibility`: class members and parameter properties
//! say `public`, `private` or `protected` (or, with `no-public`, never `public`).

use std::borrow::Cow;

use super::prelude::*;
use super::ts_adjacent_overload_signatures::member_key_name;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/explicit-member-accessibility",
        "Require explicit accessibility modifiers on class properties and methods",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<ExplicitMemberAccessibility>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Explicit,
    NoPublic,
    Off,
}

fn level(v: Option<&str>, default: Level) -> Level {
    match v {
        Some("explicit") => Level::Explicit,
        Some("no-public") => Level::NoPublic,
        Some("off") => Level::Off,
        _ => default,
    }
}

#[derive(Debug)]
struct ExplicitMemberAccessibility {
    ctor: Level,
    accessor: Level,
    method: Level,
    prop: Level,
    param_prop: Level,
    ignored: Vec<String>,
}

impl FromOptions for ExplicitMemberAccessibility {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let base = level(str_field(o, "accessibility"), Level::Explicit);
        let ov = o.get("overrides").and_then(Value::as_object);
        let get = |k: &str| level(ov.and_then(|m| m.get(k)).and_then(Value::as_str), base);
        Ok(ExplicitMemberAccessibility {
            ctor: get("constructors"),
            accessor: get("accessors"),
            method: get("methods"),
            prop: get("properties"),
            param_prop: get("parameterProperties"),
            ignored: str_list(o, "ignoredMethodNames"),
        })
    }
}

const ACCESS: Modifiers = Modifiers::PUBLIC
    .union(Modifiers::PRIVATE)
    .union(Modifiers::PROTECTED);

/// Where a member's own text starts: after its decorators.
fn start_after_decorators(cx: &Ctx<'_, '_>, node: NodeId, decorators: ts_r_ast::NodeList) -> u32 {
    match cx.ast().list(decorators).last() {
        Some(&d) => skip_trivia_forward(cx.src(), cx.span(d).end as usize) as u32,
        None => cx.span(node).start,
    }
}

/// The `public` keyword among the modifiers in `src[from..to]`, and the range removing it
/// (up to the next token or comment).
fn public_keyword(src: &str, from: u32, to: u32) -> Option<(Span, Span)> {
    let mut pos = from as usize;
    let to = (to as usize).min(src.len());
    while pos < to {
        pos = skip_trivia_forward(src, pos);
        let rest = src.get(pos..to)?;
        let len = rest
            .find(|c: char| c != '$' && !ts_r_lexer::is_id_part(c))
            .unwrap_or(rest.len());
        if len == 0 {
            return None;
        }
        if &rest[..len] == "public" {
            let end = pos + len;
            let after = &src[end..];
            let ws = after.len() - after.trim_start().len();
            return Some((
                Span::new(pos as u32, end as u32),
                Span::new(pos as u32, (end + ws) as u32),
            ));
        }
        pos += len;
    }
    None
}

impl ExplicitMemberAccessibility {
    fn suggestions(cx: &mut Ctx<'_, '_>, at: u32, head: Span, msg: String) {
        let r = cx.report(head, msg).id("missingAccessibility");
        let mut r = r;
        for acc in ["public", "private", "protected"] {
            r = r.suggest(
                "addExplicitAccessibility",
                format!("Add '{acc}' accessibility modifier"),
                Fix::insert(at, format!("{acc} ")),
            );
        }
    }

    fn check_member(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (decorators, modifiers, key, kind) = match *cx.node(node) {
            Node::MethodDef {
                decorators,
                modifiers,
                key,
                kind,
                ..
            } => (decorators, modifiers, key, Some(kind)),
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                ..
            } => (decorators, modifiers, key, None),
            _ => return,
        };
        if matches!(cx.node(key), Node::PrivateName { .. }) {
            return;
        }
        let ast = cx.ast();
        let name: Cow<'_, str> = match *ast.node(key) {
            Node::ComputedKey { expr }
                if matches!(ast.node(skip_parens(ast, expr)), Node::Ident { .. }) =>
            {
                ident_name(ast, skip_parens(ast, expr))
                    .unwrap_or_default()
                    .into()
            }
            _ => member_key_name(cx, key).0,
        };
        let (check, ty) = match kind {
            Some(MethodKind::Constructor) => (self.ctor, "method definition"),
            Some(MethodKind::Get) => (self.accessor, "get property accessor"),
            Some(MethodKind::Set) => (self.accessor, "set property accessor"),
            Some(_) => (self.method, "method definition"),
            None => (self.prop, "class property"),
        };
        if check == Level::Off || (kind.is_some() && self.ignored.iter().any(|n| *n == *name)) {
            return;
        }
        let start = start_after_decorators(cx, node, decorators);
        if check == Level::NoPublic && modifiers.contains(Modifiers::PUBLIC) {
            if let Some((kw, remove)) = public_keyword(cx.src(), start, cx.span(key).start) {
                cx.report(kw, format!("Public accessibility modifier on {ty} {name}."))
                    .id("unwantedPublicAccessibility")
                    .fix(Fix::delete(remove));
            }
        } else if check == Level::Explicit && !modifiers.intersects(ACCESS) {
            let head = Span::new(start, cx.span(key).end);
            Self::suggestions(
                cx,
                start,
                head,
                format!("Missing accessibility modifier on {ty} {name}."),
            );
        }
    }

    fn check_param(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Param {
            decorators,
            modifiers,
            pattern,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !modifiers.intersects(ACCESS.union(Modifiers::READONLY).union(Modifiers::OVERRIDE)) {
            return;
        }
        let Some(name) = ident_name(cx.ast(), pattern) else {
            return;
        };
        let name = name.to_owned();
        let start = start_after_decorators(cx, node, decorators);
        match self.param_prop {
            Level::Explicit if !modifiers.intersects(ACCESS) => {
                let p = cx.span(pattern).start;
                let head = Span::new(start, p + name.len() as u32);
                Self::suggestions(
                    cx,
                    start,
                    head,
                    format!("Missing accessibility modifier on parameter property {name}."),
                );
            }
            Level::NoPublic
                if modifiers.contains(Modifiers::PUBLIC)
                    && modifiers.contains(Modifiers::READONLY) =>
            {
                if let Some((kw, remove)) = public_keyword(cx.src(), start, cx.span(pattern).start)
                {
                    cx.report(
                        kw,
                        format!("Public accessibility modifier on parameter property {name}."),
                    )
                    .id("unwantedPublicAccessibility")
                    .fix(Fix::delete(remove));
                }
            }
            _ => {}
        }
    }
}

impl Rule for ExplicitMemberAccessibility {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MethodDef, NodeKind::PropertyDef, NodeKind::Param]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Param { .. } => self.check_param(node, cx),
            Node::MethodDef { .. } => {
                // Object literal methods are not class members.
                if cx.parent(node).is_some_and(|p| {
                    matches!(cx.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                }) {
                    self.check_member(node, cx);
                }
            }
            _ => self.check_member(node, cx),
        }
    }
}
