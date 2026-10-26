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
