//! `@typescript-eslint/dot-notation`: `a.b` over `a['b']` (and, with
//! `allowKeywords: false`, `a['while']` over `a.while`), with the TypeScript options for
//! private, protected and index-signature members.

use super::prelude::*;
use crate::regex;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/dot-notation",
        "Enforce dot notation whenever possible",
        TS_STYLISTIC_TC,
        Category::Style,
    )
    .typed()
    .fixable(),
    create: create::<DotNotation>,
};

#[derive(Debug)]
struct DotNotation {
    allow_keywords: bool,
    allow_pattern: Option<String>,
    allow_private: bool,
    allow_protected: bool,
    allow_index_signature: bool,
}

impl FromOptions for DotNotation {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(DotNotation {
            allow_keywords: bool_field(o, "allowKeywords", true),
            allow_pattern: str_field(o, "allowPattern")
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
            allow_private: bool_field(o, "allowPrivateClassPropertyAccess", false),
            allow_protected: bool_field(o, "allowProtectedClassPropertyAccess", false),
            allow_index_signature: bool_field(o, "allowIndexSignaturePropertyAccess", false),
        })
    }
}

/// ES3 reserved words: not allowed after `.` with `allowKeywords: false`.
const KEYWORDS: &[&str] = &[
    "abstract",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "function",
    "goto",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "volatile",
    "while",
    "with",
];

fn is_identifier_name(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c == '$' || c == '_' || ts_r_lexer::is_id_start(c))
        && it.all(|c| c == '$' || c == '_' || ts_r_lexer::is_id_part(c))
}

impl Rule for DotNotation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Index, NodeKind::Member]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::Index {
                obj,
                optional,
                index,
            } => self.check_index(cx, node, obj, optional, index),
            Node::Member { obj, prop, .. } if !self.allow_keywords => {
                self.check_keyword(cx, obj, prop)
            }
            _ => {}
        }
    }
}

impl DotNotation {
    fn check_index(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        obj: NodeId,
        optional: bool,
        index: NodeId,
    ) {
        let ast = cx.ast();
        let src = cx.src();
        let (key, shown) = match *cx.node(index) {
            Node::StringLit { value } => {
                let v = ast.atom(value);
                (v.to_owned(), json_string(v))
            }
            Node::TemplateLit { parts } => {
                let p = ast.list(parts);
                let [el] = p[..] else {
                    return;
                };
                let Node::TemplateElement { cooked, .. } = *cx.node(el) else {
                    return;
                };
                let v = ast.atom(cooked);
                (v.to_owned(), json_string(v))
            }
            Node::NullLit {} | Node::BoolLit { .. } => {
                let t = cx.span(index).text(src).to_owned();
                (t.clone(), t)
            }
            _ => return,
        };
        if !is_identifier_name(&key) {
            return;
        }
        if !self.allow_keywords && KEYWORDS.contains(&key.as_str()) {
            return;
        }
        if let Some(p) = &self.allow_pattern
            && regex::compile(p).is_some_and(|re| re.is_match(&key, false))
        {
            return;
        }
        if self.allowed_by_type(cx, obj, &key) {
            return;
        }
        // The `[`: after the object (and `?.`).
        let mut open = skip_trivia_forward(src, cx.span(obj).end as usize);
        if optional && src[open..].starts_with("?.") {
            open = skip_trivia_forward(src, open + 2);
        }
        let end = cx.span(node).end;
        let has_comment = ast
            .comments
            .iter()
            .any(|c| c.span.start >= open as u32 && c.span.end <= end);
        let fix = if has_comment || !src[open..].starts_with('[') {
            None
        } else {
            let mut text = String::new();
            if !optional {
                // `5['a']` -> `5 .a`.
                if let Node::NumberLit { .. } = *cx.node(skip_parens(ast, obj))
                    && cx.span(obj).text(src).bytes().all(|b| b.is_ascii_digit())
                    && cx.span(obj).end as usize == open
                {
                    text.push(' ');
                }
                text.push('.');
            }
            text.push_str(&key);
            Some(Fix::replace(Span::new(open as u32, end), text))
        };
        cx.report_node(
            index,
            format!("[{shown}] is better written in dot notation."),
        )
        .id("useDot")
        .fix_opt(fix);
    }

    fn check_keyword(&self, cx: &mut Ctx<'_, '_>, obj: NodeId, prop: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let Some(name) = ident_name(ast, prop) else {
            return;
        };
        if !KEYWORDS.contains(&name) {
            return;
        }
        let start = skip_trivia_forward(src, cx.span(obj).end as usize) as u32;
        let end = cx.span(prop).end;
        let has_comment = ast
            .comments
            .iter()
            .any(|c| c.span.start >= start && c.span.end <= end);
        let is_let = ident_name(ast, skip_parens(ast, obj)) == Some("let")
            && matches!(cx.node(obj), Node::Ident { .. });
        let fix = if has_comment || is_let {
            None
        } else {
            let optional = src[start as usize..].starts_with("?.");
            let text = if optional {
                format!("?.[\"{name}\"]")
            } else {
                format!("[\"{name}\"]")
            };
            Some(Fix::replace(Span::new(start, end), text))
        };
        cx.report_node(prop, format!(".{name} is a syntax error."))
            .id("useBrackets")
            .fix_opt(fix);
    }

    /// The TypeScript exemptions: private/protected members and index-signature reads.
    fn allowed_by_type(&self, cx: &Ctx<'_, '_>, obj: NodeId, key: &str) -> bool {
        let ti = cx.types();
        let no_prop_access_from_index =
            ti.option("noPropertyAccessFromIndexSignature") == Some(true);
        if !self.allow_private
            && !self.allow_protected
            && !self.allow_index_signature
            && !no_prop_access_from_index
        {
            return false;
        }
        let Some(t) = ti.type_at(obj) else {
            return false;
        };
        if t == Ty::UNRESOLVED {
            return false;
        }
        let t = ti.apparent_type(constrained(ti, ti.non_nullable(t)));
        let Some(sym) = ti.property_of_type(t, key) else {
            // Not a declared member: read through an index signature.
            return self.allow_index_signature || no_prop_access_from_index;
        };
        if self.allow_private || self.allow_protected {
            for d in ti.symbol_decls(sym) {
                let Some((ast, _)) = ti.decl_file(&d) else {
                    continue;
                };
                let modifiers = match *ast.node(d.node) {
                    Node::PropertyDef { modifiers, .. }
                    | Node::MethodDef { modifiers, .. }
                    | Node::Param { modifiers, .. } => modifiers,
                    _ => continue,
                };
                if (self.allow_private && modifiers.contains(Modifiers::PRIVATE))
                    || (self.allow_protected && modifiers.contains(Modifiers::PROTECTED))
                {
                    return true;
                }
            }
        }
        false
    }
}

/// `JSON.stringify` of a string.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
}
