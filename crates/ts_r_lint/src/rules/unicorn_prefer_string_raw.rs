//! `unicorn/prefer-string-raw`: `String.raw` over escaped backslashes, and no `String.raw`
//! without one.
//!
//! - A one-line string literal whose raw text has `\\`, no `` ` `` or `${`, doesn't end in a
//!   backslash escape, and whose value is the raw text with only `\\` and `\<quote>`
//!   unescaped, becomes ``String.raw`...` ``.
//! - An untagged template where some part's raw text differs from its value and every part's
//!   value is its raw text with `\\` unescaped (and doesn't end in `\`) gets the tag and the
//!   unescaped parts.
//! - Neither is reported where a literal is required (non-computed keys, module sources and
//!   specifiers, import attributes, JSX attribute values, enum members, module names,
//!   `require()` references, literal types, import types, any key of abstract members and
//!   signatures), in a Jest `expect(x).toMatchInlineSnapshot(y)` argument, or under
//!   `as const` / `<const>`. Parsed as TypeScript, the change may widen a literal type, so
//!   it is only a fix for `let`/`var` without an annotation or a `: string` declarator, and a
//!   suggestion elsewhere.
//! - ``String.raw`...` `` (the global, nothing optional) without any backslash: the tag goes
//!   and the template becomes a `'...'` string unless it has expressions or line breaks. A
//!   `;` is added for ASI, a `return`/`throw` argument (or a `yield`ed value) is wrapped in
//!   parentheses when the template starts on a later line, and parentheses around the tag go
//!   too. A comment inside the tag blocks the fix.

use super::prelude::*;
use super::unicorn_util::{space_around_keyword, with_edits};
use super::unicorn_util_h5::{method_call_of, needs_semicolon, parent_through_parens};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-string-raw",
        "Prefer using the `String.raw` tag to avoid escaping `\\`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferStringRaw>,
};

#[derive(Debug, Default)]
struct PreferStringRaw;

impl Rule for PreferStringRaw {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::StringLit,
            NodeKind::TemplateLit,
            NodeKind::TaggedTemplate,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::StringLit { value } => check_string(cx, node, cx.ast().atom(value)),
            Node::TemplateLit { parts } => check_template(cx, node, parts),
            Node::TaggedTemplate { tag, quasi, .. } => check_tagged(cx, node, tag, quasi),
            _ => {}
        }
    }
}

/// `text` with `\\` (and `\<quote>`) unescaped.
fn unescape_backslash(text: &str, quote: Option<char>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && (next == '\\' || Some(next) == quote)
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

fn has_line_break(s: &str) -> bool {
    s.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

/// The node is in a slot that holds a type.
fn in_type_slot(ast: &ts_r_ast::Ast, parent: NodeId, child: NodeId) -> bool {
    match *ast.node(parent) {
        Node::TypeAlias { ty, .. }
        | Node::As { ty, .. }
        | Node::Satisfies { ty, .. }
        | Node::TypeAssertion { ty, .. } => ty == child,
        Node::Param { type_ann, .. }
        | Node::VarDeclarator { type_ann, .. }
        | Node::PropertyDef { type_ann, .. }
        | Node::IndexSignature { type_ann, .. } => type_ann == Some(child),
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type == Some(child),
        Node::KeywordType { .. }
        | Node::TypeRef { .. }
        | Node::TemplateLitType { .. }
        | Node::ArrayType { .. }
        | Node::TupleType { .. }
        | Node::NamedTupleMember { .. }
        | Node::OptionalType { .. }
        | Node::RestType { .. }
        | Node::UnionType { .. }
        | Node::IntersectionType { .. }
        | Node::FunctionType { .. }
        | Node::ConstructorType { .. }
        | Node::TypeLiteral { .. }
        | Node::TypeQuery { .. }
        | Node::TypeOperator { .. }
        | Node::IndexedAccessType { .. }
        | Node::ConditionalType { .. }
        | Node::InferType { .. }
        | Node::MappedType { .. }
        | Node::ParenType { .. }
        | Node::TypePredicate { .. }
        | Node::TypeArgs { .. }
        | Node::TypeParam { .. } => true,
        _ => false,
    }
}

fn is_abstract_or_signature(ast: &ts_r_ast::Ast, member: NodeId) -> bool {
    match *ast.node(member) {
        Node::PropertySignature { .. } | Node::MethodSignature { .. } => true,
        Node::PropertyDef { modifiers, .. } | Node::MethodDef { modifiers, .. } => {
            modifiers.contains(Modifiers::ABSTRACT)
        }
        _ => false,
    }
}

/// unicorn's `isStringLiteralRequired`.
fn is_literal_required(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some((parent, child)) = parent_through_parens(cx, node) else {
        return false;
    };
    match *ast.node(parent) {
        Node::Property { key, .. }
        | Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. } => key == child,
        Node::ComputedKey { .. } => cx
            .parent(parent)
            .is_some_and(|m| is_abstract_or_signature(ast, m)),
        Node::ImportDecl { source, .. } => source == child,
        Node::ExportAll {
            source, exported, ..
        } => source == child || exported == Some(child),
        Node::ExportNamed { source, .. } => source == Some(child),
        Node::ImportAttribute { .. }
        | Node::ImportSpecifier { .. }
        | Node::ExportSpecifier { .. }
        | Node::JsxAttr { .. }
        | Node::EnumMember { .. }
        | Node::ExternalModuleRef { .. }
        | Node::ImportType { .. } => true,
        Node::Module { name, .. } => name == child,
        _ => in_type_slot(ast, parent, child),
    }
}

/// unicorn's `isJestInlineSnapshot`: the argument of `expect(x).toMatchInlineSnapshot(y)`.
fn is_jest_inline_snapshot(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some((call, child)) = parent_through_parens(cx, node) else {
        return false;
    };
    let Some((obj, _, &[arg])) = method_call_of(
        ast,
        call,
        &["toMatchInlineSnapshot"],
        1,
        Some(1),
        Some(false),
    ) else {
        return false;
    };
    if arg != child {
        return false;
    }
    matches!(*ast.node(skip_parens(ast, obj)), Node::Call { callee, optional: false, args, .. }
        if ident_name(ast, callee) == Some("expect") && ast.list(args).len() == 1)
}

fn check_string(cx: &mut Ctx<'_, '_>, node: NodeId, value: &str) {
    if is_literal_required(cx, node) || is_jest_inline_snapshot(cx, node) {
        return;
    }
    let raw = cx.text(node);
    let bytes = raw.as_bytes();
    if raw.len() < 2
        || bytes.get(raw.len() - 2) == Some(&b'\\')
        || !raw.contains("\\\\")
        || raw.contains('`')
        || raw.contains("${")
        || has_line_break(raw)
    {
        return;
    }
    let quote = raw.chars().next();
    let unescaped = unescape_backslash(&raw[1..raw.len() - 1], quote);
    if unescaped != value {
        return;
    }
    let fix = with_edits(
        Fix::replace(cx.span(node), format!("String.raw`{unescaped}`")),
        space_around_keyword(cx, node),
    );
    report_string_raw(cx, node, fix);
}

fn check_template(cx: &mut Ctx<'_, '_>, node: NodeId, parts: ts_r_ast::NodeList) {
    let ast = cx.ast();
    if let Some((Node::TaggedTemplate { quasi, .. }, _)) =
        cx.parent(node).map(|p| (*ast.node(p), p))
        && quasi == node
    {
        return;
    }
    if is_literal_required(cx, node) {
        return;
    }
    let mut elements = Vec::new();
    for &p in ast.list(parts) {
        if let Node::TemplateElement {
            cooked,
            invalid_escape,
        } = *ast.node(p)
        {
            if invalid_escape {
                return;
            }
            elements.push((p, ast.atom(cooked), cx.text(p)));
        }
    }
    if elements.iter().all(|(_, cooked, raw)| cooked == raw)
        || elements.iter().any(|(_, cooked, raw)| {
            cooked.ends_with('\\') || unescape_backslash(raw, None) != *cooked
        })
        || is_jest_inline_snapshot(cx, node)
    {
        return;
    }
    let mut fix = with_edits(Fix::default(), space_around_keyword(cx, node));
    fix = fix.and(Span::at(cx.span(node).start), "String.raw");
    for (p, cooked, raw) in &elements {
        if cooked != raw {
            fix = fix.and(cx.span(*p), *cooked);
        }
    }
    report_string_raw(cx, node, fix);
}

fn report_string_raw(cx: &mut Ctx<'_, '_>, node: NodeId, fix: Fix) {
    let ast = cx.ast();
    let parent = parent_through_parens(cx, node);
    if let Some((p, _)) = parent
        && let Node::As { ty, .. } | Node::TypeAssertion { ty, .. } = *ast.node(p)
        && matches!(
            ast.node(ty),
            Node::KeywordType {
                kind: KeywordTypeKind::Const
            }
        )
    {
        return;
    }
    let message = "`String.raw` should be used to avoid escaping `\\`.";
    if !cx.file.env.typescript && !is_typescript_file(cx.file.path) {
        cx.report_node(node, message)
            .id("prefer-string-raw")
            .fix(fix);
        return;
    }
    let can_fix = match parent {
        Some((p, child)) => match *ast.node(p) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                type_ann,
                ..
            } if init == child && matches!(ast.node(name), Node::Ident { .. }) => match type_ann {
                Some(t) => matches!(
                    ast.node(t),
                    Node::KeywordType {
                        kind: KeywordTypeKind::String
                    }
                ),
                None => cx.parent(p).is_some_and(|d| {
                    matches!(
                        ast.node(d),
                        Node::VarDecl {
                            kind: VarKind::Let | VarKind::Var,
                            ..
                        }
                    )
                }),
            },
            _ => false,
        },
        None => false,
    };
    let r = cx.report_node(node, message).id("prefer-string-raw");
    if can_fix {
        r.fix(fix);
    } else {
        r.suggest(
            "prefer-string-raw/suggestion",
            "Use `String.raw`. This may widen the string literal type to `string`.",
            fix,
        );
    }
}

/// unicorn's `isTypeScriptFile`.
fn is_typescript_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// The `return` / `throw` / non-delegating `yield` whose unparenthesized argument starts
/// with `node`.
fn keyword_argument_parent(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = node;
    for _ in 0..100_000 {
        let parent = cx.parent(cur)?;
        match *cx.node(parent) {
            Node::Paren { .. } => return None,
            Node::Return { arg: Some(a) } | Node::Throw { arg: a } if a == cur => {
                return Some(parent);
            }
            Node::Yield {
                delegate: false,
                arg: Some(a),
            } if a == cur => return Some(parent),
            _ => {}
        }
        if cx.span(parent).start != cx.span(cur).start {
            return None;
        }
        cur = parent;
    }
    None
}

/// Where the token before `pos` ends (whitespace and comments skipped).
fn token_end_before(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    let src = cx.src().as_bytes();
    let comments = cx.ast().comments_in(Span::new(0, pos));
    let mut p = pos;
    for _ in 0..100_000 {
        while p > 0 && src[p as usize - 1].is_ascii_whitespace() {
            p -= 1;
        }
        match comments.iter().find(|c| c.span.end == p) {
            Some(c) => p = c.span.start,
            None => break,
        }
    }
    p
}

fn check_tagged(cx: &mut Ctx<'_, '_>, node: NodeId, tag: NodeId, quasi: NodeId) {
    let ast = cx.ast();
    let member = skip_parens(ast, tag);
    let Node::Member {
        obj,
        prop,
        optional: false,
    } = *ast.node(member)
    else {
        return;
    };
    if ident_name(ast, prop) != Some("raw") || !cx.is_global_ref(obj, "String") {
        return;
    }
    let Node::TemplateLit { parts } = *ast.node(quasi) else {
        return;
    };
    let parts = ast.list(parts);
    if parts
        .iter()
        .any(|&p| matches!(ast.node(p), Node::TemplateElement { .. }) && cx.text(p).contains('\\'))
    {
        return;
    }
    let raw_quasi = cx.text(quasi);
    let suggestion = if parts.len() > 1 || has_line_break(raw_quasi) {
        raw_quasi.to_owned()
    } else {
        format!(
            "'{}'",
            raw_quasi[1..raw_quasi.len() - 1].replace('\'', "\\'")
        )
    };
    let fix = (cx.ast().comments_in(cx.span(member)).is_empty()).then(|| {
        let start = cx.span(node).start;
        let mut fix = Fix::default();
        if needs_semicolon(cx, start, &suggestion) {
            fix = fix.and(Span::at(start), ";");
        }
        let src = cx.src();
        let before = token_end_before(cx, start);
        let same_line = !has_line_break(&src[before as usize..cx.span(quasi).start as usize]);
        if let Some(k) = keyword_argument_parent(cx, node)
            && !same_line
        {
            match *ast.node(k) {
                Node::Return { .. } | Node::Throw { .. } => {
                    let ks = cx.span(k);
                    let keyword = if matches!(ast.node(k), Node::Return { .. }) {
                        "return"
                    } else {
                        "throw"
                    };
                    fix = fix.and(Span::at(ks.start + keyword.len() as u32), " (");
                    let text = cx.text(k);
                    if text.ends_with(';') {
                        fix = fix.and(Span::at(ks.end - 1), ")");
                    } else {
                        fix = fix.and(Span::at(ks.end), ")");
                    }
                }
                _ => {
                    fix = fix
                        .and(Span::at(start), "(")
                        .and(Span::at(cx.span(node).end), ")");
                }
            }
        }
        fix = fix.and(cx.span(quasi), suggestion.clone());
        let mut cur = member;
        while let Some(p) = cx.parent(cur) {
            if !matches!(cx.node(p), Node::Paren { .. }) {
                break;
            }
            let s = cx.span(p);
            fix = fix
                .and(Span::new(s.start, s.start + 1), "")
                .and(Span::new(s.end - 1, s.end), "");
            cur = p;
        }
        fix.and(cx.span(member), "")
    });
    cx.report_node(
        member,
        "Using `String.raw` is unnecessary as the string does not contain any `\\`.",
    )
    .id("unnecessary-string-raw")
    .fix_opt(fix);
}
