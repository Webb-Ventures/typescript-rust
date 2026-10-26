//! More helpers for the eslint-plugin-unicorn rules (round 5): method-call shapes, picking a
//! free variable name, renaming a binding everywhere, and ESTree-shaped identifier ranges.

use super::prelude::*;
use crate::semantic::Semantic;
use ts_r_ast::Ast;

/// `obj.method(args)`: a call whose callee is a non-computed member named `method`.
/// `args` is the exact argument count (a spread among those arguments fails the match, as
/// in the plugin); `optional_call: Some(false)` rejects `obj.method?.()`.
pub(crate) fn is_method_call(
    ast: &Ast,
    call: NodeId,
    methods: &[&str],
    args: Option<usize>,
    optional_call: Option<bool>,
) -> bool {
    let Node::Call {
        callee,
        optional,
        args: list,
        ..
    } = *ast.node(call)
    else {
        return false;
    };
    let Node::Member { prop, .. } = *ast.node(callee) else {
        return false;
    };
    if !methods.is_empty() && !ident_name(ast, prop).is_some_and(|n| methods.contains(&n)) {
        return false;
    }
    if !methods.is_empty() && matches!(ast.node(prop), Node::PrivateName { .. }) {
        return false;
    }
    if let Some(o) = optional_call
        && o != optional
    {
        return false;
    }
    let items = ast.list(list);
    if let Some(n) = args {
        if items.len() != n {
            return false;
        }
        if items
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return false;
        }
    }
    true
}

/// Words the plugin never uses as a generated name: ECMAScript reserved words, the
/// restricted `arguments`/`eval`, the global value properties, and TypeScript's reserved
/// words.
const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "arguments",
    "eval",
    "globalThis",
    "Infinity",
    "NaN",
    "undefined",
    // TypeScript
    "as",
    "any",
    "boolean",
    "constructor",
    "declare",
    "get",
    "module",
    "require",
    "number",
    "set",
    "string",
    "symbol",
    "type",
    "from",
    "of",
];

/// A syntactically valid identifier that is not a reserved word.
pub(crate) fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '$' || first == '_' || ts_r_lexer::is_id_start(first)) {
        return false;
    }
    if !chars.all(|c| {
        c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}' || ts_r_lexer::is_id_part(c)
    }) {
        return false;
    }
    !RESERVED.contains(&name)
}

/// True when `inner` is `outer` or nested in it.
pub(crate) fn scope_within(sem: &dyn Semantic, mut inner: ScopeId, outer: ScopeId) -> bool {
    for _ in 0..10_000 {
        if inner == outer {
            return true;
        }
        match sem.scope(inner).parent {
            Some(p) => inner = p,
            None => return false,
        }
    }
    false
}

/// The plugin's choice of a new variable name: `name` (or `name_` when `name` is not a
/// valid identifier), with `_` appended until it is free in every scope of `scopes` and
/// `extra_ok` accepts it. `None` when even `name_` is not an identifier.
pub(crate) fn available_name(
    cx: &Ctx<'_, '_>,
    name: &str,
    scopes: &[ScopeId],
    extra_ok: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let mut name = name.to_owned();
    if !is_valid_identifier(&name) {
        name.push('_');
        if !is_valid_identifier(&name) {
            return None;
        }
    }
    let sem = cx.semantic();
    let ast = cx.ast();
    let unresolved: Vec<(ScopeId, &str)> = sem
        .references()
        .iter()
        .filter(|r| r.resolution == Resolution::Unresolved)
        .map(|r| (r.scope, ast.atom(r.name)))
        .collect();
    for _ in 0..1000 {
        let taken = sem.is_global(&name)
            || scopes.iter().any(|&s| sem.lookup(s, &name).is_some())
            || unresolved
                .iter()
                .any(|&(rs, n)| n == name && scopes.iter().any(|&s| scope_within(sem, rs, s)));
        if !taken && extra_ok(&name) {
            return Some(name);
        }
        name.push('_');
    }
    None
}

/// The edit that renames one identifier of a binding to `new`: shorthand properties and
/// patterns become `old: new`, a shorthand import `old as new`, a shorthand export
/// `new as old`.
pub(crate) fn rename_identifier_edit(cx: &Ctx<'_, '_>, ident: NodeId, new: &str) -> (Span, String) {
    let ast = cx.ast();
    let span = cx.span(ident);
    let old = cx.text(ident);
    if let Some(p) = cx.parent(ident) {
        match *ast.node(p) {
            Node::ShorthandProp { name, .. } if name == ident => {
                return (span, format!("{old}: {new}"));
            }
            Node::BindingProperty {
                key, value: None, ..
            } if key == ident => {
                return (span, format!("{old}: {new}"));
            }
            Node::ImportSpecifier {
                imported: None,
                local,
                ..
            } if local == ident => {
                return (span, format!("{old} as {new}"));
            }
            Node::ExportSpecifier {
                exported: None,
                local,
                ..
            } if local == ident => {
                return (span, format!("{new} as {old}"));
            }
            _ => {}
        }
    }
    (span, new.to_owned())
}

/// A fix renaming every declaration and reference of `sym` to `new`.
pub(crate) fn rename_symbol_fix(cx: &Ctx<'_, '_>, sym: SymbolId, new: &str) -> Fix {
    let sem = cx.semantic();
    let s = sem.symbol(sym);
    let mut idents: Vec<NodeId> = s.decls.iter().map(|d| d.name).collect();
    for &r in &s.refs {
        let n = sem.reference(r).node;
        if !idents.contains(&n) {
            idents.push(n);
        }
    }
    let mut edits: Vec<_> = idents
        .into_iter()
        .map(|i| rename_identifier_edit(cx, i, new))
        .collect();
    edits.sort_by_key(|(s, _)| (s.start, s.end));
    edits.dedup_by_key(|(s, _)| (s.start, s.end));
    Fix {
        edits: edits
            .into_iter()
            .map(|(span, text)| crate::diagnostic::Edit { span, text })
            .collect(),
    }
}

/// The ESTree range of a binding identifier: typescript-estree's `Identifier` also covers
/// a `?` and the type annotation of a parameter or catch binding.
pub(crate) fn estree_ident_span(cx: &Ctx<'_, '_>, ident: NodeId) -> Span {
    let ast = cx.ast();
    let span = cx.span(ident);
    let Some(p) = cx.parent(ident) else {
        return span;
    };
    let end = match *ast.node(p) {
        Node::Param {
            pattern,
            type_ann,
            optional,
            ..
        } if pattern == ident => match type_ann {
            Some(t) => cx.span(t).end,
            None if optional => find_char(cx.src(), span.end, b'?').map_or(span.end, |q| q + 1),
            None => span.end,
        },
        Node::CatchClause {
            param: Some(param),
            type_ann: Some(t),
            ..
        } if param == ident => cx.span(t).end,
        Node::VarDeclarator {
            name,
            type_ann: Some(t),
            ..
        } if name == ident => cx.span(t).end,
        _ => span.end,
    };
    Span::new(span.start, end)
}

/// The last comment after `end` (before the next token) that starts on the line `end` is
/// on: the plugin's "trailing comment on the same line".
pub(crate) fn trailing_comment_same_line(cx: &Ctx<'_, '_>, end: u32) -> Option<Span> {
    let next = skip_trivia_forward(cx.src(), end as usize) as u32;
    let line = cx.file.lines.line_of(end);
    cx.ast()
        .comments_between(end, next)
        .iter()
        .rev()
        .find(|c| cx.file.lines.line_of(c.span.start) == line)
        .map(|c| c.span)
}

/// The whitespace right before `pos` on its line.
pub(crate) fn indent_before<'a>(cx: &Ctx<'_, 'a>, pos: u32) -> &'a str {
    let lines = cx.file.lines;
    let start = lines.line_start(lines.line_of(pos)).unwrap_or(0);
    let before = cx.src().get(start as usize..pos as usize).unwrap_or("");
    let trimmed = before.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    &before[trimmed.len()..]
}

/// The file's first line break, or `\n`.
pub(crate) fn linebreak(src: &str) -> &'static str {
    for (i, c) in src.char_indices() {
        match c {
            '\r' if src[i + 1..].starts_with('\n') => return "\r\n",
            '\r' => return "\r",
            '\n' => return "\n",
            '\u{2028}' => return "\u{2028}",
            '\u{2029}' => return "\u{2029}",
            _ => {}
        }
    }
    "\n"
}

/// `@eslint-community/eslint-utils`' `hasSideEffect`: calls, `new`, assignments, updates,
/// `await`, `yield`, `import()` and `delete` anywhere in `id` (not inside nested functions).
/// `getters` makes every member access count; `conversions` makes implicit type
/// conversions count (operators on non-literals, computed non-literal keys).
pub(crate) fn has_side_effect(ast: &Ast, id: NodeId, getters: bool, conversions: bool) -> bool {
    side_effect_in(ast, id, getters, conversions, 0)
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::RegexLit { .. }
    )
}

fn side_effect_in(ast: &Ast, id: NodeId, getters: bool, conversions: bool, depth: u32) -> bool {
    if depth > 256 {
        return true;
    }
    match *ast.node(id) {
        Node::Arrow { .. } | Node::FunctionExpr { .. } => return false,
        Node::Assign { .. }
        | Node::Await { .. }
        | Node::Call { .. }
        | Node::ImportCall { .. }
        | Node::New { .. }
        | Node::Update { .. }
        | Node::Yield { .. } => return true,
        Node::Unary { op, arg } => {
            if op == UnaryOp::Delete {
                return true;
            }
            if conversions
                && matches!(
                    op,
                    UnaryOp::Minus | UnaryOp::Plus | UnaryOp::Not | UnaryOp::BitNot
                )
                && !is_literal(ast, arg)
            {
                return true;
            }
        }
        Node::Binary {
            left, op, right, ..
        } => {
            use BinaryOp::*;
            if conversions
                && matches!(
                    op,
                    Eq | NotEq
                        | Lt
                        | LtEq
                        | Gt
                        | GtEq
                        | Shl
                        | Shr
                        | UShr
                        | Add
                        | Sub
                        | Mul
                        | Div
                        | Mod
                        | BitOr
                        | BitXor
                        | BitAnd
                        | In
                )
                && !(is_literal(ast, left) && is_literal(ast, right))
            {
                return true;
            }
        }
        Node::Member { .. } if getters => return true,
        Node::Index { index, .. } => {
            if getters || (conversions && !is_literal(ast, index)) {
                return true;
            }
        }
        Node::ComputedKey { expr } if conversions && !is_literal(ast, expr) => return true,
        _ => {}
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        if !found && side_effect_in(ast, c, getters, conversions, depth + 1) {
            found = true;
        }
    });
    found
}

/// A `case`/`default` clause's head: from its keyword to the end of its colon.
pub(crate) fn switch_case_head(cx: &Ctx<'_, '_>, case: NodeId) -> Span {
    let src = cx.src();
    let span = cx.span(case);
    let test = match *cx.node(case) {
        Node::SwitchCase { test, .. } => test,
        _ => None,
    };
    let mut pos = match test {
        Some(t) => cx.span(t).end as usize,
        None => (span.start as usize + "default".len()).min(src.len()),
    };
    for _ in 0..src.len() {
        pos = skip_trivia_forward(src, pos);
        match src.as_bytes().get(pos) {
            Some(b':') => return Span::new(span.start, pos as u32 + 1),
            Some(_) => pos += 1,
            None => break,
        }
    }
    span
}

/// The `return`, `throw` or non-delegating `yield` whose unparenthesized argument starts
/// with `node` (through enclosing expressions that start where it does).
pub(crate) fn keyword_argument_parent(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
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

/// Edits wrapping the argument of a `return`/`throw` in parentheses: ` (` after the
/// keyword and `)` before the statement's `;` (or after it).
pub(crate) fn parenthesize_keyword_argument(cx: &Ctx<'_, '_>, stmt: NodeId) -> Vec<(Span, String)> {
    let span = cx.span(stmt);
    let kw = match cx.node(stmt) {
        Node::Return { .. } => "return",
        Node::Throw { .. } => "throw",
        _ => return Vec::new(),
    };
    let close = if cx.text(stmt).ends_with(';') {
        span.end - 1
    } else {
        span.end
    };
    vec![
        (Span::at(span.start + kw.len() as u32), " (".to_owned()),
        (Span::at(close), ")".to_owned()),
    ]
}

/// The end of the token before `pos` (comments skipped).
pub(crate) fn prev_token_end(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    skip_trivia_backward(cx.ast(), cx.src(), pos as usize) as u32
}

/// The start of the token at or after `pos` (comments skipped).
pub(crate) fn next_token_start(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    skip_trivia_forward(cx.src(), pos as usize) as u32
}

/// A fix made of `edits` (kept in source order).
pub(crate) fn fix_of(mut edits: Vec<(Span, String)>) -> Fix {
    edits.sort_by_key(|(s, _)| (s.start, s.end));
    Fix {
        edits: edits
            .into_iter()
            .map(|(span, text)| crate::diagnostic::Edit { span, text })
            .collect(),
    }
}

/// `isTypeScriptFile`: the path ends in `.ts`, `.mts`, `.cts` or `.tsx` (any case).
pub(crate) fn is_typescript_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|e| lower.ends_with(e))
}
