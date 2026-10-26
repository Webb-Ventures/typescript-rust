//! `unicorn/prefer-regexp-escape`: `RegExp.escape()` over hand-written escaping.
//!
//! - `s.replace(/[<chars>]/g, '\\$&')` (not sticky; a plain character class of at least 8
//!   distinct regex syntax characters, a `-` only at its edges, `]` only escaped or first),
//!   when `s` may be a string: fixed to `RegExp.escape(s)` unless `s` is `super` or a comment
//!   would go.
//! - One-argument calls of the escape helpers: the default export of `escape-string-regexp`,
//!   `lodash.escaperegexp` or `lodash/escapeRegExp` (imported, `{default as x}`, or a
//!   `require()` of a static name), lodash's `escapeRegExp` (named import or destructured
//!   from `require`), and `<lodash>.escapeRegExp(x)` on a default/namespace import or
//!   `require` of `lodash`/`lodash-es`, or an undeclared `_`/`lodash`. Suggested as
//!   `RegExp.escape(x)` unless the argument is a spread, a known non-string (no possibly
//!   mutated member read in it), or a static non-string.

use super::prelude::*;
use super::unicorn_static::{
    Static, mutable_member_access, static_string_literal, static_value_no_side_effects,
};
use super::unicorn_text::{has_comment_in, paren_text};
use super::unicorn_types::is_known_non_string;
use super::unicorn_util_h5::method_call_of;
use crate::semantic::Decl;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-regexp-escape",
        "Prefer `RegExp.escape()` for escaping strings to use in regular expressions.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<PreferRegexpEscape>,
};

#[derive(Debug, Default)]
struct PreferRegexpEscape;

const MESSAGE: &str =
    "Prefer `RegExp.escape()` for escaping strings to use in regular expressions.";
const ESCAPE_PACKAGES: &[&str] = &[
    "escape-string-regexp",
    "lodash.escaperegexp",
    "lodash/escapeRegExp",
];
const LODASH_PACKAGES: &[&str] = &["lodash", "lodash-es"];
const SYNTAX_CHARACTERS: &str = "\\^$.*+?()[]{}|/-";

/// The distinct characters of a `[...]` class made only of regex syntax characters.
fn class_characters(pattern: &str) -> Option<Vec<char>> {
    let inner = pattern.strip_prefix('[')?.strip_suffix(']')?;
    if inner.starts_with('^') {
        return None;
    }
    let chars: Vec<char> = inner.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut c = chars[i];
        let mut escaped = false;
        if c == '\\' {
            i += 1;
            c = *chars.get(i)?;
            escaped = true;
        }
        if !escaped && ((c == '-' && i != 0 && i != chars.len() - 1) || (c == ']' && i != 0)) {
            return None;
        }
        if !SYNTAX_CHARACTERS.contains(c) {
            return None;
        }
        if !found.contains(&c) {
            found.push(c);
        }
        i += 1;
    }
    Some(found)
}

fn is_escape_replace(ast: &Ast, node: NodeId) -> Option<NodeId> {
    let (obj, _, &[pattern, replacement]) =
        method_call_of(ast, node, &["replace"], 2, Some(2), Some(false))?
    else {
        return None;
    };
    let Node::RegexLit { pattern, flags } = *ast.node(skip_parens(ast, pattern)) else {
        return None;
    };
    let flags = ast.atom(flags);
    let escapes = flags.contains('g')
        && !flags.contains('y')
        && class_characters(ast.atom(pattern)).is_some_and(|c| c.len() >= 8)
        && static_string_literal(ast, skip_parens(ast, replacement)).as_deref() == Some("\\$&");
    escapes.then_some(obj)
}

/// `require(<static string>)`: the package name.
fn required_package(ast: &Ast, init: Option<NodeId>) -> Option<String> {
    let Node::Call { callee, args, .. } = *ast.node(skip_parens(ast, init?)) else {
        return None;
    };
    let &[arg] = ast.list(args) else {
        return None;
    };
    if !matches!(ast.node(callee), Node::Ident { .. }) || ident_name(ast, callee) != Some("require")
    {
        return None;
    }
    static_string_literal(ast, arg)
}

fn import_source<'a>(ast: &'a Ast, decl: &Decl) -> Option<&'a str> {
    let Node::ImportDecl { source, .. } = *ast.node(decl.parent) else {
        return None;
    };
    match *ast.node(source) {
        Node::StringLit { value } => Some(ast.atom(value)),
        _ => None,
    }
}

#[derive(PartialEq)]
enum Binding {
    EscapeFunction,
    Lodash,
}

/// What the single declaration of the name `id` reads binds, as far as this rule cares.
fn binding(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Option<Binding>> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let sym = sem
        .reference_at(id)
        .and_then(|r| sem.reference(r).symbol())?;
    let decl = sem.symbol(sym).decls.first()?;
    let is = |pkgs: &[&str], name: Option<&str>| name.is_some_and(|n| pkgs.contains(&n));
    Some(match *ast.node(decl.node) {
        Node::ImportDefaultSpecifier { .. } => {
            let source = import_source(ast, decl);
            if is(ESCAPE_PACKAGES, source) {
                Some(Binding::EscapeFunction)
            } else {
                is(LODASH_PACKAGES, source).then_some(Binding::Lodash)
            }
        }
        Node::ImportNamespaceSpecifier { .. } => {
            is(LODASH_PACKAGES, import_source(ast, decl)).then_some(Binding::Lodash)
        }
        Node::ImportSpecifier {
            imported, local, ..
        } => {
            let source = import_source(ast, decl);
            let imported = imported.unwrap_or(local);
            let name = match *ast.node(imported) {
                Node::Ident { .. } => ident_name(ast, imported),
                _ => None,
            };
            ((name == Some("default") && is(ESCAPE_PACKAGES, source))
                || (name == Some("escapeRegExp") && is(LODASH_PACKAGES, source)))
            .then_some(Binding::EscapeFunction)
        }
        Node::VarDeclarator { name, init, .. } => {
            let package = required_package(ast, init);
            let package = package.as_deref();
            match *ast.node(name) {
                Node::Ident { .. } if name == decl.name => {
                    if is(ESCAPE_PACKAGES, package) {
                        Some(Binding::EscapeFunction)
                    } else {
                        is(LODASH_PACKAGES, package).then_some(Binding::Lodash)
                    }
                }
                Node::ObjectPattern { props } if is(LODASH_PACKAGES, package) => ast
                    .list(props)
                    .iter()
                    .any(|&p| {
                        matches!(*ast.node(p), Node::BindingProperty { key, value, init: None }
                        if matches!(ast.node(key), Node::Ident { .. })
                            && ident_name(ast, key) == Some("escapeRegExp")
                            && match value {
                                Some(v) => v == decl.name,
                                None => key == decl.name,
                            })
                    })
                    .then_some(Binding::EscapeFunction),
                _ => None,
            }
        }
        _ => None,
    })
}

fn is_escape_function(cx: &Ctx<'_, '_>, callee: NodeId) -> bool {
    matches!(cx.node(callee), Node::Ident { .. })
        && binding(cx, callee) == Some(Some(Binding::EscapeFunction))
}

fn is_lodash(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    match binding(cx, id) {
        Some(b) => b == Some(Binding::Lodash),
        None => matches!(ident_name(ast, id), Some("_" | "lodash")),
    }
}

impl Rule for PreferRegexpEscape {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if let Some(obj) = is_escape_replace(ast, node) {
            if is_known_non_string(cx, obj) {
                return;
            }
            let span = cx.span(node);
            let inner = skip_parens(ast, obj);
            let fix = (!matches!(ast.node(inner), Node::Super {}) && !has_comment_in(cx, span))
                .then(|| Fix::replace(span, format!("RegExp.escape({})", paren_text(cx, inner))));
            cx.report(span, MESSAGE)
                .id("prefer-regexp-escape")
                .fix_opt(fix);
            return;
        }
        let Node::Call {
            callee,
            optional,
            args,
            ..
        } = *ast.node(node)
        else {
            return;
        };
        let args = ast.list(args);
        let helper = (!optional && args.len() == 1 && is_escape_function(cx, callee))
            || method_call_of(ast, node, &["escapeRegExp"], 1, Some(1), Some(false))
                .is_some_and(|(obj, _, _)| is_lodash(cx, obj));
        let &[arg] = args else {
            return;
        };
        if !helper || matches!(ast.node(arg), Node::Spread { .. }) {
            return;
        }
        if (!mutable_member_access(cx, arg, &mut Vec::new()) && is_known_non_string(cx, arg))
            || static_value_no_side_effects(cx, arg).is_some_and(|v| !matches!(v, Static::Str(_)))
        {
            return;
        }
        let span = cx.span(node);
        let suggestion = (!has_comment_in(cx, span))
            .then(|| Fix::replace(span, format!("RegExp.escape({})", paren_text(cx, arg))));
        let r = cx.report(span, MESSAGE).id("prefer-regexp-escape");
        if let Some(fix) = suggestion {
            r.suggest(
                "replaceWithRegExpEscape",
                "Replace with `RegExp.escape()`.",
                fix,
            );
        }
    }
}
