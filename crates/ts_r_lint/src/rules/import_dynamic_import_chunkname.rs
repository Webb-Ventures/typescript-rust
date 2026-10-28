//! `import/dynamic-import-chunkname`: `import()` (and calls to the `importFunctions`)
//! need a leading `/* webpackChunkName: "name" */` comment before the specifier. Every
//! comment there must be a block comment padded with one space, valid as the body of an
//! object literal (webpack evaluates it; here it must parse and hold only constant
//! values), and made of known webpack magic comments. `webpackMode: "eager"` with a chunk
//! name is reported with suggestions to drop either. `allowEmpty` accepts no comment;
//! `webpackChunknameFormat` replaces the name pattern.

use super::prelude::*;
use crate::regex_exec::match_all;
use ts_r_ast::{Ast, CommentKind, SourceKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/dynamic-import-chunkname",
        "Enforce a leading comment with the webpackChunkName for dynamic imports.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<DynamicImportChunkname>,
};

#[derive(Debug)]
struct DynamicImportChunkname {
    import_functions: Vec<String>,
    allow_empty: bool,
    chunk_substr: String,
}

impl FromOptions for DynamicImportChunkname {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let format = str_field(o, "webpackChunknameFormat")
            .unwrap_or(r"([0-9a-zA-Z-_/.]|\[(request|index)\])+");
        Ok(DynamicImportChunkname {
            import_functions: str_list(o, "importFunctions"),
            allow_empty: bool_field(o, "allowEmpty", false),
            chunk_substr: format!("webpackChunkName: [\"']{format}[\"'],? "),
        })
    }
}

const PADDED: &str = r"^ (\S[\s\S]+\S) $";
const STYLE: &str = r#"^( ((webpackChunkName: .+)|((webpackPrefetch|webpackPreload): (true|false|-?[0-9]+))|(webpackIgnore: (true|false))|((webpackInclude|webpackExclude): \/.*\/)|(webpackMode: ["'](lazy|lazy-once|eager|weak)["'])|(webpackExports: (['"]\w+['"]|\[(['"]\w+['"], *)+(['"]\w+['"]*)\]))),?)+ $"#;
const EAGER: &str = r#"webpackMode: ["']eager["'],? "#;

/// The first match of `pattern` in `text`.
fn first_match(pattern: &str, text: &str) -> Option<String> {
    match_all(pattern, "", text)?
        .into_iter()
        .next()
        .and_then(|groups| groups.into_iter().next().flatten())
}

/// Whether `{body}` is an object literal webpack can evaluate: it parses, and its values
/// are constants (no names other than `undefined`, `NaN` and `Infinity`, no calls).
fn evaluates(body: &str) -> bool {
    let text = format!("({{{body}}})");
    let r = ts_r_parser::parse(&text, SourceKind::Js);
    if !r.syntax_errors().is_empty() {
        return false;
    }
    let ast = &r.ast;
    let mut ok = true;
    let mut stack = vec![(ast.root, false)];
    let mut kids = Vec::new();
    while let Some((id, is_key)) = stack.pop() {
        match ast.node(id) {
            Node::Program { .. }
            | Node::ExprStmt { .. }
            | Node::Paren { .. }
            | Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::TemplateLit { .. }
            | Node::TemplateElement { .. }
            | Node::Unary { .. }
            | Node::Binary { .. }
            | Node::Conditional { .. }
            | Node::Spread { .. }
            | Node::Hole {}
            | Node::Seq { .. } => {}
            Node::Property { key, .. } => {
                let key = *key;
                kids.clear();
                ast.for_each_child(id, |c| kids.push(c));
                stack.extend(kids.iter().rev().map(|&c| (c, c == key)));
                continue;
            }
            Node::Ident { .. } => {
                if !is_key && !matches!(ast.name_of(id), Some("undefined" | "NaN" | "Infinity")) {
                    ok = false;
                }
            }
            Node::ComputedKey { .. } => {}
            _ => ok = false,
        }
        if !ok {
            return false;
        }
        kids.clear();
        ast.for_each_child(id, |c| kids.push(c));
        stack.extend(kids.iter().rev().map(|&c| (c, false)));
    }
    ok
}

/// The comment's text without its delimiters.
fn comment_value(src: &str, span: Span, kind: CommentKind) -> &str {
    let text = span.text(src);
    match kind {
        CommentKind::Line => text.get(2..).unwrap_or(""),
        _ => text.get(2..text.len().saturating_sub(2)).unwrap_or(""),
    }
}

impl DynamicImportChunkname {
    fn removal(&self, src: &str, comments: &[(Span, &str)], pattern: &str) -> Option<Fix> {
        let (span, value) = comments
            .iter()
            .find(|(_, v)| first_match(pattern, v).is_some())?;
        let m = first_match(pattern, value)?;
        let rest = value.replacen(&m, "", 1);
        let rest = rest.trim();
        let rest = rest.strip_suffix(',').unwrap_or(rest);
        let _ = src;
        Some(if rest.is_empty() {
            Fix::delete(*span)
        } else {
            Fix::replace(*span, format!("/* {rest} */"))
        })
    }

    fn run(&self, cx: &mut Ctx<'_, '_>, node: NodeId, arg: NodeId, ast: &Ast) {
        let src = cx.src();
        let arg_start = cx.span(arg).start;
        let prev = skip_trivia_backward(ast, src, arg_start as usize) as u32;
        let comments: Vec<(Span, CommentKind)> = ast
            .comments
            .iter()
            .filter(|c| c.span.start >= prev && c.span.end <= arg_start)
            .map(|c| (c.span, c.kind))
            .collect();
        let span = cx.span(node);
        if comments.is_empty() && !self.allow_empty {
            cx.report(
                span,
                "dynamic imports require a leading comment with the webpack chunkname",
            );
            return;
        }
        let mut chunk = false;
        let mut eager = false;
        let mut values = Vec::new();
        for &(c, kind) in &comments {
            if kind != CommentKind::Block {
                cx.report(
                    span,
                    "dynamic imports require a /* foo */ style comment, not a // foo comment",
                );
                return;
            }
            let value = comment_value(src, c, kind);
            if first_match(PADDED, value).is_none() {
                cx.report(
                    span,
                    "dynamic imports require a block comment padded with spaces - /* foo */",
                );
                return;
            }
            if !evaluates(value) || first_match(STYLE, value).is_none() {
                cx.report(
                    span,
                    "dynamic imports require a \"webpack\" comment with valid syntax",
                );
                return;
            }
            eager |= first_match(EAGER, value).is_some();
            chunk |= first_match(&self.chunk_substr, value).is_some();
            values.push((c, value));
        }
        if chunk && eager {
            let drop_name = self.removal(src, &values, &self.chunk_substr);
            let drop_mode = self.removal(src, &values, EAGER);
            let mut r = cx.report(
                span,
                "dynamic imports using eager mode do not need a webpackChunkName",
            );
            if let Some(f) = drop_name {
                r = r.suggest("removeChunkName", "Remove webpackChunkName", f);
            }
            if let Some(f) = drop_mode {
                r.suggest("removeMode", "Remove webpackMode", f);
            }
        }
        if !chunk && !self.allow_empty && !eager {
            cx.report(
                span,
                format!(
                    "dynamic imports require a leading comment in the form /*{}*/",
                    self.chunk_substr
                ),
            );
        }
    }
}

impl Rule for DynamicImportChunkname {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportCall, NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *cx.node(node) {
            Node::ImportCall { arg, .. } => self.run(cx, node, arg, ast),
            Node::Call { callee, args, .. } => {
                let Some(name) = ident_name(ast, callee) else {
                    return;
                };
                if !self.import_functions.iter().any(|f| f == name) {
                    return;
                }
                if let Some(&arg) = ast.list(args).first() {
                    self.run(cx, node, arg, ast);
                }
            }
            _ => {}
        }
    }
}
