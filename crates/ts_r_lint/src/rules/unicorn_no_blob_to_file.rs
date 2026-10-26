//! `unicorn/no-blob-to-file`: `const file = new File([blob], "name")` used once, only to be
//! passed to `URL.createObjectURL(file)` or `formData.append|set(key, file[, name])`, where
//! the original `Blob` would do.
//!
//! The declaration is a lone `const` declarator (plain name, not exported) directly in the
//! program or a block. `File` (global) gets exactly two arguments: a one-element array
//! (no hole, no spread) and a static string. The element is a `const` declared before the
//! `new File`, initialized with the global `new Blob()` (at most one argument) or
//! `new File(a, b)`. The variable has exactly one reference: the only argument of a plain
//! `URL.createObjectURL()` (global `URL`), or the second of two or three arguments of a
//! plain `<x>.append()`/`.set()` whose receiver is a `const` initialized with the global
//! `new FormData()` (no arguments). The blob's name must mean the same binding where the
//! file is used.
//!
//! Reported on the `new File(...)`, with a suggestion that removes the declaration,
//! replaces the use with the blob's name and, for a two-argument `FormData` call, appends
//! the file name. No suggestion when a comment is in the declaration, on its line, or on
//! the line just before it.

use super::prelude::*;
use super::unicorn_ast::remove_statements;
use super::unicorn_no_array_reduce::const_initializer;
use super::unicorn_util_h1::static_string;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-blob-to-file",
        "Disallow unnecessary `Blob` to `File` conversion.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<NoBlobToFile>,
};

#[derive(Debug, Default)]
struct NoBlobToFile;

/// `new <name>(...)` with the global `name` and an argument count in `min..=max` (no
/// spread): the arguments.
fn global_new<'a>(
    cx: &Ctx<'a, '_>,
    id: NodeId,
    name: &str,
    min: usize,
    max: usize,
) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    let Node::New { callee, args, .. } = *ast.node(skip_parens(ast, id)) else {
        return None;
    };
    let args = ast.list(args);
    if !cx.is_global_ref(callee, name)
        || args.len() < min
        || args.len() > max
        || args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some(args)
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut cur = id;
    loop {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
        } else {
            return Some(p);
        }
    }
}

/// The number of line breaks in `text` (`\r\n` counts once).
fn line_breaks(text: &str) -> usize {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|&c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
        .count()
}

enum Use {
    ObjectUrl,
    FormData { two_args: bool },
}

impl Rule for NoBlobToFile {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = *ast.node(node)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(decl) = cx.parent(node) else {
            return;
        };
        let Node::VarDecl {
            modifiers,
            kind: VarKind::Const,
            decls,
        } = *ast.node(decl)
        else {
            return;
        };
        if modifiers.contains(Modifiers::EXPORT)
            || ast.list(decls).len() != 1
            || !cx
                .parent(decl)
                .is_some_and(|p| matches!(cx.node(p), Node::Program { .. } | Node::Block { .. }))
        {
            return;
        }
        let Some(blob) = blob_identifier(cx, init) else {
            return;
        };
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        let refs: Vec<NodeId> = sem
            .symbol(sym)
            .refs
            .iter()
            .map(|&r| sem.reference(r).node)
            .filter(|&n| n != name)
            .collect();
        let &[reference] = refs.as_slice() else {
            return;
        };
        let Some((call, kind)) = supported_call(cx, reference) else {
            return;
        };
        // The blob's name means the same binding where the file is used.
        let blob_sym = sem.symbol_of_ident(blob);
        let blob_name = ident_name(ast, blob).unwrap_or("");
        if blob_sym.is_none() || sem.lookup(sem.enclosing_scope(reference), blob_name) != blob_sym {
            return;
        }
        let span = cx.span(init);
        let message = "Use the original `Blob` instead of converting it to a `File`.";
        if comment_near(cx, decl) {
            cx.report(span, message).id("no-blob-to-file");
            return;
        }
        let mut fix = remove_statements(cx, decl, decl, false);
        fix = fix.and(cx.span(reference), cx.text(blob));
        if let Use::FormData { two_args: true } = kind
            && let Node::New { args, .. } = *ast.node(skip_parens(ast, init))
            && let Some(&file_name) = ast.list(args).get(1)
        {
            let src = cx.src();
            let end = cx.span(call).end;
            let close = end - 1;
            let before = skip_trivia_backward(ast, src, close as usize);
            let text = cx.text(file_name);
            let insert = if src.as_bytes().get(before.wrapping_sub(1)) == Some(&b',') {
                format!(" {text},")
            } else {
                format!(", {text}")
            };
            fix = fix.and(Span::at(close), insert);
        }
        cx.report(span, message).id("no-blob-to-file").suggest(
            "suggestion",
            "Replace the `File` with the original `Blob`.",
            fix,
        );
    }
}

/// The `blob` of `new File([blob], "name")` when it is a suitable earlier `const`.
fn blob_identifier(cx: &Ctx<'_, '_>, init: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let args = global_new(cx, init, "File", 2, 2)?;
    let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, args[0])) else {
        return None;
    };
    let &[element] = ast.list(elements) else {
        return None;
    };
    if matches!(ast.node(element), Node::Spread { .. } | Node::Hole {}) {
        return None;
    }
    static_string(ast, skip_parens(ast, args[1]))?;
    let blob = skip_parens(ast, element);
    let blob_init = const_initializer(cx, blob)?;
    if cx.span(blob_init).start > cx.span(init).start {
        return None;
    }
    (global_new(cx, blob_init, "Blob", 0, 1).is_some()
        || global_new(cx, blob_init, "File", 2, 2).is_some())
    .then_some(blob)
}

fn supported_call(cx: &Ctx<'_, '_>, reference: NodeId) -> Option<(NodeId, Use)> {
    let ast = cx.ast();
    let call = parent_np(cx, reference)?;
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let args = ast.list(args);
    if args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    let is_arg = |i: usize| {
        args.get(i)
            .is_some_and(|&a| skip_parens(ast, a) == reference)
    };
    match ident_name(ast, prop)? {
        "createObjectURL" if args.len() == 1 && is_arg(0) && cx.is_global_ref(obj, "URL") => {
            Some((call, Use::ObjectUrl))
        }
        "append" | "set" if (2..=3).contains(&args.len()) && is_arg(1) => {
            let init = const_initializer(cx, skip_parens(ast, obj))?;
            global_new(cx, init, "FormData", 0, 0)?;
            Some((
                call,
                Use::FormData {
                    two_args: args.len() == 2,
                },
            ))
        }
        _ => None,
    }
}

/// A comment inside the declaration, after it on its last line, or before it ending on its
/// first line or the line above.
fn comment_near(cx: &Ctx<'_, '_>, decl: NodeId) -> bool {
    let ast: &Ast = cx.ast();
    let src = cx.src();
    let span = cx.span(decl);
    if !ast.comments_in(span).is_empty() {
        return true;
    }
    let prev = skip_trivia_backward(ast, src, span.start as usize) as u32;
    let before = ast
        .comments_in(Span::new(prev, span.start))
        .iter()
        .any(|c| line_breaks(&src[c.span.end as usize..span.start as usize]) <= 1);
    let next = skip_trivia_forward(src, span.end as usize) as u32;
    let after = ast
        .comments_in(Span::new(span.end, next))
        .iter()
        .any(|c| line_breaks(&src[span.end as usize..c.span.start as usize]) == 0);
    before || after
}
