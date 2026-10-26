//! `unicorn/text-encoding-identifier-case`: write `utf8` and `ascii` in lowercase, as `utf8`
//! (or `utf-8` with `withDash`). `<meta charset>`, `<form accept-charset>` and the first
//! argument of `new TextDecoder()` always take `utf-8`.
//!
//! Strings and substitution-free untagged templates are compared on their raw text, so
//! escapes and surrounding spaces don't match. Module specifiers (imports, re-exports,
//! `require(x)`, `import(x)`, `import x = require(x)`, `import("x")` types and
//! `declare module "x"`) are skipped. The second argument of a plain `x.readFile()` or
//! `x.readFileSync()` call (first argument not spread) is fixed; elsewhere the replacement is
//! a suggestion.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/text-encoding-identifier-case",
        "Enforce consistent case for text encoding identifiers.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<TextEncodingIdentifierCase>,
};

#[derive(Debug, Default)]
struct TextEncodingIdentifierCase {
    with_dash: bool,
}

impl FromOptions for TextEncodingIdentifierCase {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(TextEncodingIdentifierCase {
            with_dash: bool_field(obj_opt(opts, 0), "withDash", false),
        })
    }
}

impl Rule for TextEncodingIdentifierCase {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::Directive,
            NodeKind::TemplateLit,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let span = string_literal_span(ast, cx.src(), node);
        let (raw, inner) = match *cx.node(node) {
            Node::StringLit { .. } | Node::Directive { .. } => {
                let inner = Span::new(
                    span.start + 1,
                    span.end.saturating_sub(1).max(span.start + 1),
                );
                (inner.text(cx.src()), inner)
            }
            Node::TemplateLit { parts } => {
                let [part] = ast.list(parts) else {
                    return;
                };
                if cx
                    .parent(node)
                    .is_some_and(|p| matches!(cx.node(p), Node::TaggedTemplate { .. }))
                {
                    return;
                }
                let inner = cx.span(*part);
                (inner.text(cx.src()), inner)
            }
            _ => return,
        };
        if raw.is_empty() || is_module_specifier(cx, node) {
            return;
        }
        let with_dash = self.with_dash || enforces_dash(cx, node);
        let replacement = match raw.to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" if with_dash => "utf-8",
            "utf-8" | "utf8" => "utf8",
            "ascii" => "ascii",
            _ => return,
        };
        if replacement == raw {
            return;
        }
        let fix = Fix::replace(inner, replacement);
        let fixable = is_read_file_encoding(cx, node);
        let report = cx
            .report(span, format!("Prefer `{replacement}` over `{raw}`."))
            .id("text-encoding-identifier/error");
        if fixable {
            report.fix(fix);
        } else {
            report.suggest(
                "text-encoding-identifier/suggestion",
                format!("Replace `{raw}` with `{replacement}`."),
                fix,
            );
        }
    }
}

fn is_module_specifier(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let Some(parent) = cx.parent(node) else {
        return false;
    };
    let ast = cx.ast();
    match *cx.node(parent) {
        Node::ImportDecl { source, .. }
        | Node::ExportAll { source, .. }
        | Node::ExportNamed {
            source: Some(source),
            ..
        } => source == node,
        Node::ExternalModuleRef { expr } => expr == node,
        Node::ImportType { arg, .. } | Node::ImportCall { arg, .. } => arg == node,
        Node::Module { name, .. } => name == node,
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        } => {
            ast.list(args) == [node] && ident_name(ast, skip_parens(ast, callee)) == Some("require")
        }
        _ => false,
    }
}

/// The second argument of `x.readFile(a, enc)` / `x.readFileSync(a, enc)`.
fn is_read_file_encoding(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(Node::Call {
        callee,
        optional: false,
        args,
        ..
    }) = cx.parent(node).map(|p| cx.node(p))
    else {
        return false;
    };
    let args = ast.list(*args);
    if args.get(1) != Some(&node) || matches!(ast.node(args[0]), Node::Spread { .. }) {
        return false;
    }
    match *ast.node(skip_parens(ast, *callee)) {
        Node::Member {
            optional: false,
            prop,
            ..
        } => matches!(ident_name(ast, prop), Some("readFile" | "readFileSync")),
        _ => false,
    }
}

/// `<meta charset>`, `<form accept-charset>` / `acceptCharset`, `new TextDecoder(x)`.
fn enforces_dash(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(parent) = cx.parent(node) else {
        return false;
    };
    match *cx.node(parent) {
        Node::JsxAttr { name, value } if value == Some(node) => {
            let Some(opening) = cx.parent(parent) else {
                return false;
            };
            let Node::JsxOpening { name: tag, .. } = *cx.node(opening) else {
                return false;
            };
            let (Some(attr), Some(tag)) = (jsx_ident(cx, name), jsx_ident(cx, tag)) else {
                return false;
            };
            match tag.as_str() {
                "meta" => attr == "charset",
                "form" => attr == "acceptcharset" || attr == "accept-charset",
                _ => false,
            }
        }
        Node::New { callee, args, .. } => {
            ast.list(args).first() == Some(&node) && ident_name(ast, callee) == Some("TextDecoder")
        }
        _ => false,
    }
}

/// A plain JSX name (no namespace or member), lowercased.
fn jsx_ident(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    matches!(cx.node(id), Node::Ident { .. })
        .then(|| cx.span(id).text(cx.src()).to_ascii_lowercase())
}
