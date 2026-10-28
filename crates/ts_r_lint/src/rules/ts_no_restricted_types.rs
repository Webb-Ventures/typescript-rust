//! `@typescript-eslint/no-restricted-types`: configured type names (keywords, references,
//! `implements` / interface `extends` entries, `{}` and `[]`), with an optional fix and
//! suggestions.

use rustc_hash::FxHashMap;
use ts_r_ast::HeritageKind;

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-restricted-types",
        "Disallow certain types",
        NO_PRESET,
        Category::Restriction,
    )
    .fixable()
    .suggestions(),
    create: create::<NoRestrictedTypes>,
};

#[derive(Debug, Default)]
struct Ban {
    message: Option<String>,
    fix_with: Option<String>,
    suggest: Vec<String>,
}

#[derive(Debug, Default)]
struct NoRestrictedTypes {
    /// Keyed by the type text without whitespace.
    banned: FxHashMap<String, Ban>,
}

fn no_spaces(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

impl FromOptions for NoRestrictedTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut out = Self::default();
        let Some(types) = obj_opt(opts, 0).get("types").and_then(Value::as_object) else {
            return Ok(out);
        };
        for (k, v) in types {
            let ban = match v {
                Value::Bool(true) => Ban::default(),
                Value::String(s) => Ban {
                    message: Some(s.clone()),
                    ..Ban::default()
                },
                Value::Object(o) => Ban {
                    message: str_field(o, "message").map(String::from),
                    fix_with: str_field(o, "fixWith").map(String::from),
                    suggest: str_list(o, "suggest"),
                },
                _ => continue,
            };
            out.banned.insert(no_spaces(k), ban);
        }
        Ok(out)
    }
}

fn keyword(kind: KeywordTypeKind) -> Option<&'static str> {
    Some(match kind {
        KeywordTypeKind::BigInt => "bigint",
        KeywordTypeKind::Boolean => "boolean",
        KeywordTypeKind::Never => "never",
        KeywordTypeKind::Null => "null",
        KeywordTypeKind::Number => "number",
        KeywordTypeKind::Object => "object",
        KeywordTypeKind::String => "string",
        KeywordTypeKind::Symbol => "symbol",
        KeywordTypeKind::Undefined => "undefined",
        KeywordTypeKind::Unknown => "unknown",
        KeywordTypeKind::Void => "void",
        _ => return None,
    })
}

impl NoRestrictedTypes {
    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: Option<&str>) {
        let span = cx.span(node);
        let name = name.map_or_else(|| no_spaces(span.text(cx.src())), String::from);
        let Some(ban) = self.banned.get(&name) else {
            return;
        };
        let custom = match &ban.message {
            Some(m) if !m.is_empty() => format!(" {m}"),
            _ => String::new(),
        };
        let mut r = cx
            .report_node(node, format!("Don't use `{name}` as a type.{custom}"))
            .id("bannedTypeMessage")
            .fix_opt(
                ban.fix_with
                    .as_ref()
                    .filter(|f| !f.is_empty())
                    .map(|f| Fix::replace(span, f.clone())),
            );
        for s in &ban.suggest {
            r = r.suggest(
                "bannedTypeReplacement",
                format!("Replace `{name}` with `{s}`."),
                Fix::replace(span, s.clone()),
            );
        }
    }

    fn check_heritage(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        if let Node::ExprWithTypeArgs { expr, type_args } = *cx.node(node) {
            self.check(cx, expr, None);
            if type_args.is_some() {
                self.check(cx, node, None);
            }
        }
    }
}

impl Rule for NoRestrictedTypes {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::KeywordType,
            NodeKind::TypeRef,
            NodeKind::TupleType,
            NodeKind::TypeLiteral,
            NodeKind::HeritageClause,
            NodeKind::Interface,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if self.banned.is_empty() {
            return;
        }
        let ast = cx.ast();
        match *cx.node(node) {
            Node::KeywordType { kind } => {
                if let Some(k) = keyword(kind)
                    && self.banned.contains_key(k)
                {
                    self.check(cx, node, Some(k));
                }
            }
            Node::TypeRef { name, type_args } => {
                self.check(cx, name, None);
                if type_args.is_some() {
                    self.check(cx, node, None);
                }
            }
            Node::TupleType { elems } if ast.list(elems).is_empty() => self.check(cx, node, None),
            Node::TypeLiteral { members } if ast.list(members).is_empty() => {
                self.check(cx, node, None)
            }
            Node::HeritageClause {
                kind: HeritageKind::Implements,
                types,
            } => {
                for &t in ast.list(types) {
                    self.check_heritage(cx, t);
                }
            }
            Node::Interface { extends, .. } => {
                for &t in ast.list(extends) {
                    self.check_heritage(cx, t);
                }
            }
            _ => {}
        }
    }
}
