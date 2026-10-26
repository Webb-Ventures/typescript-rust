//! Helpers shared by the eslint-plugin-unicorn rules: following references to globals
//! (`Intl.Collator`, `globalThis.Array`, `const {Array} = globalThis`) and the fixes that
//! turn a call into a `new` expression and back.

use super::prelude::*;
use ts_r_ast::Ast;

/// Names that stand for the global object itself.
const GLOBAL_OBJECTS: &[&str] = &["globalThis", "global", "self", "window"];

/// The dotted global path `expr` refers to (`Array`, `Intl.Collator`), when it reads a
/// global that the file neither declares nor writes, possibly through `globalThis`-like
/// objects, property accesses, and variables initialised from such a path (`const A =
/// Array`, `const {Array: A} = globalThis`). Like ESLint's reference tracker, values pass
/// through `?:` branches, `||`/`&&`/`??`, the last expression of a sequence and TS
/// wrappers. `None` for the global object itself and for anything else.
pub(crate) fn global_path(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<String> {
    let mut out = None;
    path_of(cx, expr, 0, &mut |p| {
        if out.is_none() && !p.is_empty() {
            out = Some(p.join("."));
        }
    });
    out
}

/// True when `expr` can be the global object itself (`globalThis`, `window`, an alias
/// `const g = globalThis`), as [`global_path`] follows it.
pub(crate) fn is_global_object(cx: &Ctx<'_, '_>, expr: NodeId) -> bool {
    let mut out = false;
    path_of(cx, expr, 0, &mut |p| out |= p.is_empty());
    out
}

/// Calls `found` with each path `expr` can carry (empty for the global object).
fn path_of(cx: &Ctx<'_, '_>, expr: NodeId, depth: u32, found: &mut dyn FnMut(&[String])) {
    if depth > 16 {
        return;
    }
    let ast = cx.ast();
    let expr = skip_wrappers(ast, expr);
    match *ast.node(expr) {
        Node::Conditional { cons, alt, .. } => {
            path_of(cx, cons, depth + 1, found);
            path_of(cx, alt, depth + 1, found);
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            right,
        } => {
            path_of(cx, left, depth + 1, found);
            path_of(cx, right, depth + 1, found);
        }
        Node::Seq { exprs } => {
            if let Some(&last) = ast.list(exprs).last() {
                path_of(cx, last, depth + 1, found);
            }
        }
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            let Some(name) = member_name(ast, cx.src(), expr) else {
                return;
            };
            if matches!(ast.node(expr), Node::Index { index, .. }
                if !matches!(ast.node(skip_parens(ast, *index)), Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. }))
            {
                return;
            }
            path_of(cx, obj, depth + 1, &mut |p| {
                let mut p = p.to_vec();
                p.push(name.to_string());
                found(&p);
            });
        }
        Node::Ident { .. } => ident_path(cx, expr, depth, found),
        _ => {}
    }
}

fn ident_path(cx: &Ctx<'_, '_>, ident: NodeId, depth: u32, found: &mut dyn FnMut(&[String])) {
    let ast = cx.ast();
    let Some(name) = ident_name(ast, ident) else {
        return;
    };
    let sem = cx.semantic();
    let Some(r) = sem.reference_at(ident) else {
        return;
    };
    let reference = sem.reference(r);
    if !reference.is_read() {
        return;
    }
    let local = match reference.resolution {
        Resolution::Global => None,
        Resolution::Symbol(s) if sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL) => None,
        Resolution::Symbol(s) => Some(s),
        Resolution::Unresolved => return,
    };
    let Some(sym) = local else {
        if is_modified_global(cx, name) {
            return;
        }
        if GLOBAL_OBJECTS.contains(&name) {
            found(&[]);
        } else {
            found(&[name.to_owned()]);
        }
        return;
    };
    // A local: follow the values written to it.
    let s = sem.symbol(sym);
    for decl in &s.decls {
        let Node::VarDeclarator {
            init: Some(init), ..
        } = *ast.node(decl.node)
        else {
            continue;
        };
        let Some(keys) = pattern_keys(cx, decl.name, decl.node) else {
            continue;
        };
        path_of(cx, init, depth + 1, &mut |p| {
            let mut p = p.to_vec();
            p.extend(keys.iter().cloned());
            found(&p);
        });
    }
    for &rid in &s.refs {
        let rf = sem.reference(rid);
        let Some(parent) = cx.parent(rf.node) else {
            continue;
        };
        if let Node::Assign { left, right, .. } = *ast.node(parent)
            && left == rf.node
        {
            path_of(cx, right, depth + 1, found);
        }
    }
}

/// The property keys from a declarator's pattern down to the binding `name`
/// (`{Array: A}` gives `["Array"]`), or `None` through array patterns or computed keys.
fn pattern_keys(cx: &Ctx<'_, '_>, name: NodeId, declarator: NodeId) -> Option<Vec<String>> {
    let ast = cx.ast();
    let mut keys = Vec::new();
    let mut cur = name;
    for _ in 0..64 {
        let parent = cx.parent(cur)?;
        if parent == declarator {
            keys.reverse();
            return Some(keys);
        }
        match *ast.node(parent) {
            Node::BindingProperty { key, .. } => {
                if matches!(ast.node(key), Node::ComputedKey { .. }) {
                    let Node::ComputedKey { expr } = *ast.node(key) else {
                        return None;
                    };
                    if !matches!(
                        ast.node(skip_parens(ast, expr)),
                        Node::StringLit { .. } | Node::NumberLit { .. }
                    ) {
                        return None;
                    }
                }
                keys.push(key_name(ast, cx.src(), key)?.into_owned());
            }
            Node::ObjectPattern { .. } | Node::AssignPattern { .. } => {}
            _ => return None,
        }
        cur = parent;
    }
    None
}

/// A global the file writes to (`Array = x`) doesn't count as the built-in.
pub(crate) fn is_modified_global(cx: &Ctx<'_, '_>, name: &str) -> bool {
    let sem = cx.semantic();
    let ast = cx.ast();
    sem.references().iter().any(|r| {
        r.is_write()
            && !matches!(r.resolution, Resolution::Symbol(s) if !sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL))
            && ast.atom(r.name) == name
    })
}

/// Strips parentheses and the TS wrappers values pass through.
pub(crate) fn skip_wrappers(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// An optional link (`?.`) anywhere in the chain `id` ends (parentheses and TS wrappers
/// are looked through).
pub(crate) fn has_optional_chain(ast: &Ast, id: NodeId) -> bool {
    let mut id = id;
    for _ in 0..100_000 {
        id = skip_wrappers(ast, id);
        match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            _ => return false,
        }
    }
    false
}

/// The span of `id` with all the parentheses around it.
pub(crate) fn parenthesized_span(cx: &Ctx<'_, '_>, id: NodeId) -> Span {
    let mut id = id;
    while let Some(p) = cx.parent(id) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        id = p;
    }
    cx.span(id)
}

/// Words a token can't be glued to: lowercase keywords plus `of`, `await`, `as`,
/// `satisfies`.
fn is_keyword_like(word: &str) -> bool {
    matches!(
        word,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "let"
            | "static"
            | "of"
            | "await"
            | "as"
            | "satisfies"
    )
}

/// Edits adding a space between `node` (with its parentheses) and a keyword glued to it
/// (`return(a)` / `(a)in b`), so replacing the node can't merge them.
pub(crate) fn space_around_keyword(cx: &Ctx<'_, '_>, node: NodeId) -> Vec<(Span, String)> {
    let src = cx.src();
    let range = parenthesized_span(cx, node);
    let mut edits = Vec::new();
    let before = &src[..range.start as usize];
    if before.ends_with(is_ident_char) {
        let word_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident_char(c))
            .last()
            .map_or(0, |(i, _)| i);
        if is_keyword_like(&before[word_start..]) {
            edits.push((Span::at(range.start), " ".to_owned()));
        }
    }
    let after = &src[range.end as usize..];
    if after.starts_with(is_ident_char) {
        let len = after
            .char_indices()
            .find(|&(_, c)| !is_ident_char(c))
            .map_or(after.len(), |(i, _)| i);
        if is_keyword_like(&after[..len]) {
            edits.push((Span::at(range.end), " ".to_owned()));
        }
    }
    edits
}

/// Adds `edits` to `fix`.
pub(crate) fn with_edits(mut fix: Fix, edits: Vec<(Span, String)>) -> Fix {
    for (span, text) in edits {
        fix = fix.and(span, text);
    }
    fix
}

/// `Foo()` -> `new Foo()`: inserts `new `, spaces it from a glued keyword, and wraps a
/// member callee whose object chain contains a call (`a().b()` -> `new (a().b)()`).
pub(crate) fn call_to_new(cx: &Ctx<'_, '_>, call: NodeId) -> Fix {
    let ast = cx.ast();
    let start = cx.span(call).start;
    let mut fix = Fix::insert(start, "new ");
    let edits = space_around_keyword(cx, call);
    // A space glued before the node goes before `new `.
    let mut fix_edits = Vec::new();
    for (span, text) in edits {
        if span.start == start {
            fix = Fix::insert(start, format!("{text}new "));
        } else {
            fix_edits.push((span, text));
        }
    }
    fix = with_edits(fix, fix_edits);
    if let Node::Call { callee, .. } = *ast.node(call)
        && !matches!(ast.node(callee), Node::Paren { .. })
        && member_contains_call(ast, callee)
    {
        let s = cx.span(callee);
        fix = fix.and(Span::at(s.start), "(").and(Span::at(s.end), ")");
    }
    fix
}

/// A member expression whose object chain (through members, tags and TS wrappers, not
/// parentheses) reaches a call.
fn member_contains_call(ast: &Ast, callee: NodeId) -> bool {
    let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
        return false;
    };
    let mut cur = obj;
    for _ in 0..100_000 {
        match *ast.node(cur) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => cur = expr,
            Node::Member { obj, .. } | Node::Index { obj, .. } => cur = obj,
            Node::TaggedTemplate { tag, .. } => cur = tag,
            Node::Call { .. } => return true,
            _ => return false,
        }
    }
    false
}

/// `new Foo()` -> `Foo()`: removes `new` and the spaces after it, adds `()` when the `new`
/// had no argument list, and wraps the result when removing `new` would change how it
/// parses (inside an unparenthesized `new` callee, or a `return`/`throw`/`yield` argument
/// whose callee starts on a later line). Adds a `;` when the statement would otherwise join
/// the previous one.
pub(crate) fn new_to_call(cx: &Ctx<'_, '_>, new: NodeId) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(new);
    let Node::New {
        callee, has_args, ..
    } = *ast.node(new)
    else {
        return Fix::default();
    };
    let new_kw = Span::new(span.start, span.start + 3);
    let callee_start = cx.span(skip_parens(ast, callee)).start;
    let same_line = !has_newline(src, new_kw.end, callee_start);
    let parenthesize =
        in_unparenthesized_new_callee(cx, new) || (!same_line && keyword_argument_parent(cx, new));
    let after = &src[new_kw.end as usize..];
    let spaces = (after.len() - after.trim_start().len()) as u32;
    let mut fix = Fix::replace(
        Span::new(new_kw.start, new_kw.end + spaces),
        if parenthesize { "(" } else { "" },
    );
    if !has_args {
        fix = fix.and(Span::at(span.end), "()");
    }
    if parenthesize {
        fix = fix.and(Span::at(span.end), ")");
    }
    let next = src[(new_kw.end + spaces) as usize..].chars().next();
    if matches!(
        next,
        Some('[' | '(' | '/' | '`' | '+' | '-' | '*' | ',' | '.' | '<')
    ) && joins_previous_statement(cx, new)
    {
        fix = fix.and(Span::at(span.start), ";");
    }
    fix
}

/// `node` (not parenthesized) is, through members, tags and TS wrappers, the callee of a
/// `new`.
pub(crate) fn in_unparenthesized_new_callee(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..100_000 {
        let Some(parent) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(parent) {
            Node::New { callee, .. } if callee == cur => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == cur => {}
            Node::TaggedTemplate { tag, .. } if tag == cur => {}
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. }
                if expr == cur => {}
            _ => return false,
        }
        cur = parent;
    }
    false
}

/// `node` starts the (unparenthesized) argument of a `return`, `throw` or non-delegating
/// `yield`.
fn keyword_argument_parent(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..100_000 {
        let Some(parent) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Paren { .. } => return false,
            Node::Return { arg: Some(a) } | Node::Throw { arg: a } if a == cur => return true,
            Node::Yield {
                delegate: false,
                arg: Some(a),
            } if a == cur => return true,
            _ => {}
        }
        if cx.span(parent).start != cx.span(cur).start {
            return false;
        }
        cur = parent;
    }
    false
}

/// `node` starts an expression statement that follows a statement ending without `;` in
/// an expression a `(`/`[`/... could continue.
pub(crate) fn joins_previous_statement(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let parents = cx.file.parents;
    if expr_start(ast, parents, node) != Some(ExprStart::Statement) {
        return false;
    }
    let mut stmt = node;
    while let Some(p) = parents.parent(stmt) {
        stmt = p;
        if matches!(ast.node(p), Node::ExprStmt { .. }) {
            break;
        }
    }
    prev_statement(ast, parents, stmt).is_some_and(|p| stmt_continuable(ast, cx.src(), p))
}

/// An error class name: capitalised words then `Error` (`^(?:[A-Z][\da-z]*)*Error$`).
pub(crate) fn is_error_name(name: &str) -> bool {
    let Some(prefix) = name.strip_suffix("Error") else {
        return false;
    };
    prefix.bytes().all(|c| c.is_ascii_alphanumeric())
        && prefix.bytes().next().is_none_or(|c| c.is_ascii_uppercase())
}
