//! `@typescript-eslint/no-empty-object-type`: disallow accidentally using the "empty
//! object" type (`{}` and empty interfaces).

use super::prelude::*;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-empty-object-type",
        "Disallow accidentally using the \"empty object\" type",
        TS_REC,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<NoEmptyObjectType>,
};

#[derive(Debug)]
struct NoEmptyObjectType {
    /// "never", "always" or "with-single-extends".
    interfaces: String,
    objects_always: bool,
    allow_with_name: Option<String>,
}

impl FromOptions for NoEmptyObjectType {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoEmptyObjectType {
            interfaces: str_field(o, "allowInterfaces")
                .unwrap_or("never")
                .to_owned(),
            objects_always: str_field(o, "allowObjectTypes") == Some("always"),
            allow_with_name: str_field(o, "allowWithName").map(str::to_owned),
        })
    }
}

const EMPTY_INTERFACE: &str = "An empty interface declaration allows any non-nullish value, including literals like `0` and `\"\"`.\n- If that's what you want, disable this lint rule with an inline comment or configure the 'allowInterfaces' rule option.\n- If you want a type meaning \"any object\", you probably want `object` instead.\n- If you want a type meaning \"any value\", you probably want `unknown` instead.";
const EMPTY_OBJECT: &str = "The `{}` (\"empty object\") type allows any non-nullish value, including literals like `0` and `\"\"`.\n- If that's what you want, disable this lint rule with an inline comment or configure the 'allowObjectTypes' rule option.\n- If you want a type meaning \"any object\", you probably want `object` instead.\n- If you want a type meaning \"any value\", you probably want `unknown` instead.";

impl NoEmptyObjectType {
    fn name_allowed(&self, name: &str) -> bool {
        self.allow_with_name
            .as_deref()
            .is_some_and(|p| regex::compile(p).is_some_and(|r| r.is_match(name, false)))
    }

    fn interface(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Interface {
            name,
            type_params,
            extends,
            body,
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !ast.list(body).is_empty() || self.interfaces == "always" {
            return;
        }
        let supers = ast.list(extends);
        if supers.len() > 1 || (supers.len() == 1 && self.interfaces == "with-single-extends") {
            return;
        }
        if self.name_allowed(cx.text(name)) {
            return;
        }
        // Merged declarations (another interface or a class) cannot become a type alias.
        let sem = cx.semantic();
        let merged = sem.symbol_of_decl(name).is_some_and(|s| {
            sem.symbol(s).decls.iter().any(|d| {
                d.name != name
                    && (d.kind == DeclKind::Interface
                        || (d.kind == DeclKind::Class
                            && matches!(cx.node(d.node), Node::ClassDecl { .. })))
            })
        });
        let default_export = matches!(*cx.node(node), Node::Interface { modifiers, .. } if modifiers.contains(Modifiers::DEFAULT));
        let can_fix = !merged && !default_export;
        let head_end = type_params.map_or(cx.span(name).end, |t| cx.span(t).end);
        let head = cx.src()[cx.span(name).start as usize..head_end as usize].to_owned();
        let span = super::ts_prefer_namespace_keyword::span_without_export(cx, node);
        let start = if cx.text(node).starts_with("export") {
            span.start
        } else {
            cx.span(node).start
        };
        // `declare`/`export` stay; the replacement starts at `interface`.
        let kw = cx.src()[start as usize..]
            .find("interface")
            .map_or(start, |o| start + o as u32);
        let whole = Span::new(kw, cx.span(node).end);
        match supers {
            [] => {
                let r = cx.report_node(name, EMPTY_INTERFACE).id("noEmptyInterface");
                if can_fix {
                    let _ = r
                        .suggest(
                            "replaceEmptyInterface",
                            "Replace empty interface with `object`.",
                            Fix::replace(whole, format!("type {head} = object")),
                        )
                        .suggest(
                            "replaceEmptyInterface",
                            "Replace empty interface with `unknown`.",
                            Fix::replace(whole, format!("type {head} = unknown")),
                        );
                }
            }
            [sup] => {
                let sup_text = cx.text(*sup).to_owned();
                let r = cx
                    .report_node(
                        name,
                        "An interface declaring no members is equivalent to its supertype.",
                    )
                    .id("noEmptyInterfaceWithSuper");
                if can_fix {
                    let _ = r.suggest(
                        "replaceEmptyInterfaceWithSuper",
                        "Replace empty interface with a type alias.",
                        Fix::replace(whole, format!("type {head} = {sup_text}")),
                    );
                }
            }
            _ => {}
        }
    }

    fn object(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::TypeLiteral { members } = *cx.node(node) else {
            return;
        };
        if !cx.ast().list(members).is_empty() || self.objects_always {
            return;
        }
        let parent = cx.parent(node);
        if let Some(p) = parent {
            match *cx.node(p) {
                Node::IntersectionType { .. } => return,
                Node::TypeAlias { name, .. } if self.name_allowed(cx.text(name)) => return,
                _ => {}
            }
        }
        let span = cx.span(node);
        cx.report(span, EMPTY_OBJECT)
            .id("noEmptyObject")
            .suggest(
                "replaceEmptyObjectType",
                "Replace `{}` with `object`.",
                Fix::replace(span, "object"),
            )
            .suggest(
                "replaceEmptyObjectType",
                "Replace `{}` with `unknown`.",
                Fix::replace(span, "unknown"),
            );
    }
}

impl Rule for NoEmptyObjectType {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Interface, NodeKind::TypeLiteral]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match cx.ast().kind(node) {
            NodeKind::Interface => self.interface(node, cx),
            _ => self.object(node, cx),
        }
    }
}
