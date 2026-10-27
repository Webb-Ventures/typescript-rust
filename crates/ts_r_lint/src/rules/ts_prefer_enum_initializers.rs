//! `@typescript-eslint/prefer-enum-initializers`: every enum member has an explicit value.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/prefer-enum-initializers",
        "Require each enum member value to be explicitly initialized",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferEnumInitializers>,
};

#[derive(Debug, Default)]
struct PreferEnumInitializers;

impl Rule for PreferEnumInitializers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Enum]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Node::Enum { members, .. } = *cx.node(node) else {
            return;
        };
        for (i, &m) in cx.ast().list(members).iter().enumerate() {
            if !matches!(cx.node(m), Node::EnumMember { init: None, .. }) {
                continue;
            }
            let span = cx.span(m);
            let name = span.text(cx.src()).to_owned();
            let mut r = cx
                .report_node(
                    m,
                    format!("The value of the member '{name}' should be explicitly defined."),
                )
                .id("defineInitializer");
            for value in [i.to_string(), (i + 1).to_string(), format!("'{name}'")] {
                r = r.suggest(
                    "defineInitializerSuggestion",
                    format!("Can be fixed to {name} = {value}"),
                    Fix::replace(span, format!("{name} = {value}")),
                );
            }
        }
    }
}
