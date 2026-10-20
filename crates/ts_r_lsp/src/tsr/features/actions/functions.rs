//! Rewrites of functions, as tsserver 6.0.3 offers them. Everything here was observed by
//! running tsserver on the `refactor-arrow-*`, `refactor-function-*`, `refactor-params-*` and
//! `refactor-overload-*` fixtures and by asking `getApplicableRefactors` at every offset of
//! probe files (both triggers), never taken from its source.
//!
//! | Refactor / action | Offered at | Result |
//! |---|---|---|
//! | `Add or remove braces in an arrow function` / `Add braces to arrow function` | an arrow with an expression body: its head (from the whitespace before it to `=>`), or its body when invoked | `{ return E; }` on new lines, at the indentation of the body's line |
//! | ... / `Remove braces from arrow function` | an arrow whose block is one `return` (same places) | the returned expression (`void 0` for a bare `return`), parenthesized when it is an object literal or a comma list; comments become `/* */` around it |
//! | `Convert arrow function or function expression` / `Convert to anonymous function` | an arrow's head, the arrow not using `this`/`arguments` | `function(params) { return E; }` |
//! | ... / `Convert to named function` | a `const`/`let`/`var` statement's declarator (not its modifiers) whose initializer is an arrow or function expression, or the head of such an arrow | `function name(...) {...}` replacing the statement (one declarator) or after it (several) |
//! | ... / `Convert to arrow function` | a function expression's head, or the declarator it initializes | `(params) => E` for a body of one `return E;`, else the block |
//! | `Convert parameters to destructured object` | a function's, method's or constructor's head (a token, not the whitespace between them), or that of an arrow or function expression initializing a `const` | `{ a, b = 1 }: { a: T; b?: U; }` (`= {}` when every member is optional); every call becomes `f({ a: x, b })` |
//! | `Convert overload list to single signature` | any declaration of an overloaded function, method, method signature, call or construct signature (an implementation only up to its body) whose overloads return the same type | `(...args: [a: T] \| [b: U])` replacing all of them |
//!
//! Not offered at a bare cursor on a cursor move (`@trigger: implicit`): braces from an arrow's
//! body. With a selection, the spot is the selection's start and the selection must stay in
//! the function; a selected statement offers none of these.
//!
//! Declined although tsserver offers them, because its result is wrong (`@differs` fixtures):
//! a generator to an arrow (`yield` in an arrow), a typed variable to a named function (the
//! type is lost: implicit `any`), parameters to an object when a use is not a plain call
//! (`f.call`, `typeof f`, JSX, spreads, extra arguments; tsserver offers and then changes
//! nothing), overloads into a single signature when the implementation uses its parameters,
//! a reassigned `let`/`var` to a named function (the assignment then fails).
//!
//! Moved code is kept as written except one-line type literals, which tsserver reprints as
//! `{ a: T; b: U; }` ([`emit`]); its other reformatting (`a+b` to `a + b`, `function (` to
//! `function(` in nested functions) is not copied. Calls are found through imports, renaming
//! re-exports and default imports (also of an anonymous `export default function`).

use super::edit::Edit;
use super::{Action, Ctx, Request, style};
use std::collections::HashSet;
use ts_r_ast::{Ast, FnFlags, MethodKind, Modifiers, Node, NodeId, Span, VarKind};
use ts_r_checker::{Ck, Loc};
use ts_r_diagnostics::FileId;
use ts_r_types::{SymId, TypeFlags};

const BRACES: &str = "Add or remove braces in an arrow function";
const ADD_BRACES: &str = "Add braces to arrow function";
const REMOVE_BRACES: &str = "Remove braces from arrow function";
const CONVERT: &str = "Convert arrow function or function expression";
const TO_ANONYMOUS: &str = "Convert to anonymous function";
const TO_NAMED: &str = "Convert to named function";
const TO_ARROW: &str = "Convert to arrow function";
const DESTRUCTURE: &str = "Convert parameters to destructured object";
const OVERLOAD: &str = "Convert overload list to single signature";

/// Where the request points, as far as these refactors care.
#[derive(Clone, Copy, Debug)]
enum Spot {
    /// An arrow, before the end of its `=>`.
    ArrowHead(NodeId),
    /// Inside an arrow's body.
    ArrowBody(NodeId),
    /// A function expression's head (not a method's).
    FnExprHead(NodeId),
    /// A function declaration's head.
    FnDeclHead(NodeId),
    /// A class or object method's head (the `MethodDef`).
    MethodHead(NodeId),
    /// A method, call or construct signature.
    Signature(NodeId),
    /// A variable statement's declarator, before its initializer.
    Var(NodeId, NodeId),
}

/// The function refactors at the request.
pub fn refactors(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let (s, e) = trim(text, req.start, req.end);
    let Some((spot, deepest)) = spot_at(cx, s) else {
        return;
    };
    let selection = s < e;
    // A selection must stay inside the function it starts in.
    if selection {
        let owner = match spot {
            Spot::ArrowHead(n)
            | Spot::ArrowBody(n)
            | Spot::FnExprHead(n)
            | Spot::FnDeclHead(n)
            | Spot::MethodHead(n)
            | Spot::Signature(n) => n,
            Spot::Var(..) => return,
        };
        if e > ast.span(owner).end {
            return;
        }
    }
    // A cursor on a token, not on the whitespace between a function's own tokens.
    let on_token = |func: NodeId| {
        let ws = text[s as usize..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
        !(ws && (deepest == func
            || parent_of(cx, deepest) == Some(func) && is_fn_value(ast, deepest)))
    };
    match spot {
        Spot::ArrowHead(a) => {
            braces(cx, req, a, out);
            if req.wants("refactor.rewrite.function.named")
                && let Some((vd, d)) = var_of_init(cx, a)
            {
                to_named(cx, vd, d, out);
            }
            if req.wants("refactor.rewrite.function.anonymous")
                && let Some(edits) = to_anonymous(cx, a)
            {
                out.push(action(
                    cx,
                    TO_ANONYMOUS,
                    "refactor.rewrite.function.anonymous",
                    CONVERT,
                    edits,
                ));
            }
            if on_token(a) {
                destructure(cx, ck, req, a, out);
            }
        }
        Spot::ArrowBody(a) => {
            if req.invoked {
                braces(cx, req, a, out);
            }
        }
        Spot::FnExprHead(fe) => {
            if req.wants("refactor.rewrite.function.arrow")
                && let Some(edits) = to_arrow(cx, fe)
            {
                out.push(action(
                    cx,
                    TO_ARROW,
                    "refactor.rewrite.function.arrow",
                    CONVERT,
                    edits,
                ));
            }
            if on_token(fe) {
                destructure(cx, ck, req, fe, out);
            }
        }
        Spot::FnDeclHead(fd) => {
            if on_token(fd) {
                destructure(cx, ck, req, fd, out);
            }
            overload(cx, req, fd, out);
        }
        Spot::MethodHead(md) => {
            if on_token(md) {
                destructure(cx, ck, req, md, out);
            }
            overload(cx, req, md, out);
        }
        Spot::Signature(m) => overload(cx, req, m, out),
        Spot::Var(vd, d) => {
            let Node::VarDeclarator {
                init: Some(init), ..
            } = *ast.node(d)
            else {
                return;
            };
            if req.wants("refactor.rewrite.function.named") {
                to_named(cx, vd, d, out);
            }
            if matches!(ast.node(init), Node::FunctionExpr { .. })
                && req.wants("refactor.rewrite.function.arrow")
                && let Some(edits) = to_arrow(cx, init)
            {
                out.push(action(
                    cx,
                    TO_ARROW,
                    "refactor.rewrite.function.arrow",
                    CONVERT,
                    edits,
                ));
            }
        }
    }
}

fn action(cx: &Ctx<'_, '_>, title: &str, kind: &str, name: &str, edits: Vec<Edit>) -> Action {
    let mut a = Action::new(title, kind);
    a.refactor = Some(format!("{name}/{title}"));
    a.edit(&cx.checker.file(cx.file).path, edits);
    a
}

// ---- positions ----

/// The selection without surrounding whitespace.
fn trim(text: &str, start: u32, end: u32) -> (u32, u32) {
    let end = end.max(start).min(text.len() as u32);
    let start = start.min(end);
    let s = &text[start as usize..end as usize];
    let lead = s.len() - s.trim_start().len();
    if lead == s.len() {
        return (start, start);
    }
    let trail = s.len() - s.trim_end().len();
    (start + lead as u32, end - trail as u32)
}

/// Where `n`'s leading whitespace starts (tsserver's "full start", whitespace only).
fn full_start(text: &str, n: Span) -> u32 {
    let before = &text[..n.start as usize];
    before.trim_end().len() as u32
}

/// The deepest node whose span, with the whitespace before it, contains `pos`.
fn node_at_full(ast: &Ast, text: &str, pos: u32) -> NodeId {
    let mut cur = ast.root;
    for _ in 0..ast.node_count() {
        let mut next = None;
        ast.for_each_child(cur, |c| {
            let sp = ast.span(c);
            if next.is_none() && sp.start < sp.end && full_start(text, sp) <= pos && pos < sp.end {
                next = Some(c);
            }
        });
        match next {
            Some(c) => cur = c,
            None => break,
        }
    }
    cur
}

fn parent_of(cx: &Ctx<'_, '_>, n: NodeId) -> Option<NodeId> {
    cx.checker.file(cx.file).parents().parent(n)
}

/// A method's function expression (its value).
fn is_fn_value(ast: &Ast, n: NodeId) -> bool {
    matches!(ast.node(n), Node::FunctionExpr { .. })
}

/// End of an arrow's `=>`.
fn arrow_token(ast: &Ast, text: &str, arrow: NodeId) -> Option<(u32, u32)> {
    let Node::Arrow { body, .. } = *ast.node(arrow) else {
        return None;
    };
    let a = ast.span(arrow).start;
    let i = text[a as usize..ast.span(body).start as usize].rfind("=>")? as u32;
    Some((a + i, a + i + 2))
}

fn spot_at(cx: &Ctx<'_, '_>, pos: u32) -> Option<(Spot, NodeId)> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    let n = node_at_full(ast, text, pos);
    for a in std::iter::once(n).chain(parents.ancestors(n)) {
        let spot = match *ast.node(a) {
            Node::Arrow { .. } => {
                let (_, end) = arrow_token(ast, text, a)?;
                if pos < end {
                    Spot::ArrowHead(a)
                } else {
                    Spot::ArrowBody(a)
                }
            }
            Node::FunctionExpr { body, .. } => {
                let body = body?;
                if pos >= full_start(text, ast.span(body)) {
                    return None;
                }
                match parents.parent(a).map(|p| (p, ast.node(p))) {
                    Some((p, Node::MethodDef { .. })) => Spot::MethodHead(p),
                    _ => Spot::FnExprHead(a),
                }
            }
            Node::FunctionDecl { body, .. } => {
                if body.is_some_and(|b| pos >= full_start(text, ast.span(b))) {
                    return None;
                }
                Spot::FnDeclHead(a)
            }
            Node::MethodDef { value, .. } => {
                let Node::FunctionExpr { body, .. } = *ast.node(value) else {
                    return None;
                };
                if body.is_some_and(|b| pos >= full_start(text, ast.span(b))) {
                    return None;
                }
                Spot::MethodHead(a)
            }
            Node::MethodSignature { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. } => Spot::Signature(a),
            Node::VarDeclarator {
                init: Some(init), ..
            } => {
                if pos >= full_start(text, ast.span(init)) {
                    return None;
                }
                Spot::Var(parents.parent(a)?, a)
            }
            Node::VarDecl {
                modifiers, decls, ..
            } => {
                // Not on the modifiers (`export`).
                let first = *ast.list(decls).first()?;
                if modifiers != Modifiers::NONE {
                    let head = &text[ast.span(a).start as usize..ast.span(first).start as usize];
                    let kind_at = ["const", "let", "var"]
                        .iter()
                        .filter_map(|k| head.rfind(k))
                        .max()?;
                    let kw_start = ast.span(a).start + kind_at as u32;
                    if pos < full_start(text, Span::new(kw_start, kw_start)) {
                        return None;
                    }
                }
                let d = ast
                    .list(decls)
                    .iter()
                    .copied()
                    .find(|&d| pos < ast.span(d).end)?;
                Spot::Var(a, d)
            }
            Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Interface { .. }
            | Node::ObjectLit { .. } => return None,
            ref x if x.is_statement() => return None,
            _ => continue,
        };
        return Some((spot, n));
    }
    None
}

// ---- shared ----

/// Whether `root` uses `this`, `super`, `arguments` or `new.target` of its own function (not
/// of a nested non-arrow function or class).
fn uses_this(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let b = &f.bound;
    let mut found = false;
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if found {
            break;
        }
        match *ast.node(n) {
            Node::This {} | Node::Super {} | Node::MetaProperty { .. } => found = true,
            Node::Ident { name } if ast.atom(name) == "arguments" && b.resolved(n).is_none() => {
                found = true
            }
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
                if n != root => {}
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    found
}

/// Whether the name of function expression `fe` is used inside it.
fn self_referenced(cx: &Ctx<'_, '_>, fe: NodeId) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let b = &f.bound;
    let Node::FunctionExpr {
        name: Some(name), ..
    } = *ast.node(fe)
    else {
        return false;
    };
    let syms: Vec<_> = [b.symbol_of_decl(fe), b.symbol_of_decl(name)]
        .into_iter()
        .flatten()
        .collect();
    let sp = ast.span(fe);
    b.refs.resolved.iter().any(|&(n, s)| {
        let ns = ast.span(n);
        syms.contains(&s) && sp.start <= ns.start && ns.end <= sp.end && n != name
    })
}

/// Template literal spans under `root` (their lines are never re-indented).
fn templates(ast: &Ast, root: NodeId) -> Vec<Span> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(ast.node(n), Node::TemplateLit { .. }) {
            out.push(ast.span(n));
            continue;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    out
}

/// The text of `sp` (inside `root`) as tsserver prints it: one-line type literals as
/// `{ a: T; }` (see [`print_type`]); everything else as written.
fn emit(ast: &Ast, text: &str, root: NodeId, sp: Span) -> String {
    let mut lits = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let nsp = ast.span(n);
        if nsp.end <= sp.start || nsp.start >= sp.end {
            continue;
        }
        if matches!(ast.node(n), Node::TypeLiteral { .. })
            && sp.start <= nsp.start
            && nsp.end <= sp.end
            && !nsp.text(text).contains('\n')
        {
            lits.push(n);
            continue;
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    lits.sort_by_key(|&n| ast.span(n).start);
    let mut out = String::new();
    let mut cur = sp.start;
    for n in lits {
        let nsp = ast.span(n);
        out.push_str(&text[cur as usize..nsp.start as usize]);
        out.push_str(&print_type(ast, text, n));
        cur = nsp.end;
    }
    out.push_str(&text[cur as usize..sp.end as usize]);
    out
}

/// [`emit`] of node `n`.
fn emit_node(ast: &Ast, text: &str, n: NodeId) -> String {
    emit(ast, text, n, ast.span(n))
}

/// `text[sp]` as emitted (`src`, the same lines) with each line after the first re-indented
/// by `f(old indentation, line)`; lines starting inside a template literal are kept.
fn reindent_with(
    text: &str,
    sp: Span,
    src: &str,
    keep: &[Span],
    mut f: impl FnMut(&str, &str) -> String,
) -> String {
    let mut out = String::new();
    let mut at = sp.start;
    let orig: Vec<&str> = text[sp.start as usize..sp.end as usize]
        .split('\n')
        .collect();
    for (i, line) in src.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            if keep.iter().any(|k| k.start < at && at < k.end) || line.trim().is_empty() {
                out.push_str(line);
            } else {
                let body = line.trim_start_matches([' ', '\t']);
                let ind = &line[..line.len() - body.len()];
                out.push_str(&f(ind, body));
                out.push_str(body);
            }
        } else {
            out.push_str(line);
        }
        at += orig.get(i).map_or(0, |l| l.len() as u32 + 1);
    }
    out
}

/// `{ return E; }` for an expression body: on new lines, `return` one level in from
/// `indent`. Continuation lines move in by one level; those level with the expression's
/// first line (and not closing a bracket) by two, as tsserver's formatter does.
fn block_of(cx: &Ctx<'_, '_>, body: NodeId, indent: &str) -> String {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let unit = style::indent_unit(text);
    let nl = style::newline(text);
    let semi = style::semi(ast, text);
    let sp = ast.span(body);
    let base = style::line_indent(text, sp.start).to_string();
    let keep = templates(ast, body);
    let expr = reindent_with(text, sp, &emit_node(ast, text, body), &keep, |old, line| {
        let rel = old.strip_prefix(base.as_str()).unwrap_or("");
        let closer = line.starts_with([')', '}', ']']);
        let extra = if rel.is_empty() && !closer {
            unit.as_str()
        } else {
            ""
        };
        format!("{indent}{unit}{rel}{extra}")
    });
    format!("{{{nl}{indent}{unit}return {expr}{semi}{nl}{indent}}}")
}

/// Whether an expression needs parentheses as an arrow's body.
fn needs_parens(ast: &Ast, text: &str, e: NodeId) -> bool {
    matches!(ast.node(e), Node::Seq { .. } | Node::ObjectLit { .. })
        || ast.span(e).text(text).starts_with('{')
}

/// `/* x*/` for `// x`; block comments as written.
fn as_block_comment(text: &str, c: &ts_r_ast::Comment) -> String {
    let t = c.span.text(text);
    match t.strip_prefix("//") {
        Some(rest) => format!("/*{}*/", rest.trim_end()),
        None => t.to_string(),
    }
}

// ---- braces ----

fn braces(cx: &Ctx<'_, '_>, req: &Request<'_>, a: NodeId, out: &mut Vec<Action>) {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::Arrow { flags, body, .. } = *ast.node(a) else {
        return;
    };
    if flags.contains(FnFlags::EXPRESSION_BODY) {
        if !req.wants("refactor.rewrite.arrow.braces.add") {
            return;
        }
        let sp = ast.span(body);
        let indent = style::line_indent(text, sp.start).to_string();
        let edits = vec![Edit::new(sp.start, sp.end, block_of(cx, body, &indent))];
        out.push(action(
            cx,
            ADD_BRACES,
            "refactor.rewrite.arrow.braces.add",
            BRACES,
            edits,
        ));
    } else if req.wants("refactor.rewrite.arrow.braces.remove")
        && let Some(e) = single_return(cx, body, false)
    {
        let sp = ast.span(body);
        out.push(action(
            cx,
            REMOVE_BRACES,
            "refactor.rewrite.arrow.braces.remove",
            BRACES,
            vec![Edit::new(sp.start, sp.end, e)],
        ));
    }
}

/// The expression a block of one `return` becomes: `void 0` for a bare `return` (only when
/// `bare_ok`... tsserver keeps the block when converting a function), parenthesized when
/// needed, comments around the `return` turned into block comments.
fn single_return(cx: &Ctx<'_, '_>, block: NodeId, keep_bare: bool) -> Option<String> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::Block { stmts } = *ast.node(block) else {
        return None;
    };
    let &[ret] = ast.list(stmts) else {
        return None;
    };
    let Node::Return { arg } = *ast.node(ret) else {
        return None;
    };
    if keep_bare && arg.is_none() {
        return None;
    }
    let bsp = ast.span(block);
    let rsp = ast.span(ret);
    let mut lead = Vec::new();
    let mut trail = Vec::new();
    for c in ast.comments_in(bsp) {
        if c.span.end <= rsp.start {
            lead.push(as_block_comment(text, c));
        } else if c.span.start >= rsp.end {
            // Only a comment on the `return`'s line.
            if text[rsp.end as usize..c.span.start as usize].contains('\n') {
                return None;
            }
            trail.push(as_block_comment(text, c));
        }
    }
    let mut out = String::new();
    for c in &lead {
        out.push_str(c);
        out.push(' ');
    }
    match arg {
        None => out.push_str("void 0"),
        Some(e) => {
            let esp = ast.span(e);
            let from = style::line_indent(text, rsp.start).to_string();
            let to = style::line_indent(text, bsp.start).to_string();
            let keep = templates(ast, e);
            let et = reindent_with(
                text,
                esp,
                &emit_node(ast, text, e),
                &keep,
                |old, _| match old.strip_prefix(from.as_str()) {
                    Some(rel) => format!("{to}{rel}"),
                    None => old.to_string(),
                },
            );
            if needs_parens(ast, text, e) {
                out.push_str(&format!("({et})"));
            } else {
                out.push_str(&et);
            }
        }
    }
    for c in &trail {
        out.push(' ');
        out.push_str(c);
    }
    Some(out)
}

// ---- arrow <-> function ----

/// The arrow's head without `async` and `=>`: type parameters, parameters (parenthesized),
/// return type.
fn arrow_head(cx: &Ctx<'_, '_>, a: NodeId) -> Option<String> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::Arrow {
        flags,
        type_params,
        params,
        ..
    } = *ast.node(a)
    else {
        return None;
    };
    let mut start = ast.span(a).start;
    if flags.contains(FnFlags::ASYNC) {
        let rest = &text[start as usize..];
        start += 5 + (rest[5..].len() - rest[5..].trim_start().len()) as u32;
    }
    if let Some(tp) = type_params {
        start = start.min(ast.span(tp).start);
    }
    let (arrow_start, _) = arrow_token(ast, text, a)?;
    let head = emit(ast, text, a, Span::new(start, arrow_start));
    let head = head.trim_end();
    if head.starts_with('(') || head.starts_with('<') {
        return Some(head.to_string());
    }
    // `v => ...`
    let &[p] = ast.list(params) else {
        return None;
    };
    Some(format!("({})", emit_node(ast, text, p)))
}

fn to_anonymous(cx: &Ctx<'_, '_>, a: NodeId) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::Arrow { flags, body, .. } = *ast.node(a) else {
        return None;
    };
    if uses_this(cx, a) {
        return None;
    }
    let head = arrow_head(cx, a)?;
    let sp = ast.span(a);
    let body_text = if flags.contains(FnFlags::EXPRESSION_BODY) {
        let mut indent = style::line_indent(text, sp.start).to_string();
        // tsserver's formatter indents one more level when the parameter had no parentheses.
        if !text[sp.start as usize..]
            .trim_start_matches("async")
            .trim_start()
            .starts_with(['(', '<'])
        {
            indent.push_str(&style::indent_unit(text));
        }
        block_of(cx, body, &indent)
    } else {
        emit_node(ast, text, body)
    };
    let asyn = if flags.contains(FnFlags::ASYNC) {
        "async "
    } else {
        ""
    };
    Some(vec![Edit::new(
        sp.start,
        sp.end,
        format!("{asyn}function{head} {body_text}"),
    )])
}

/// A function expression's text from its type parameters or `(` to its end.
fn fn_expr_rest(cx: &Ctx<'_, '_>, fe: NodeId) -> Option<(String, NodeId, FnFlags)> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::FunctionExpr {
        flags,
        name,
        type_params,
        body: Some(body),
        ..
    } = *ast.node(fe)
    else {
        return None;
    };
    let sp = ast.span(fe);
    let from = match (type_params, name) {
        (Some(tp), _) => ast.span(tp).start,
        (None, Some(n)) => ast.span(n).end + text[ast.span(n).end as usize..].find('(')? as u32,
        (None, None) => {
            let kw = sp.start + text[sp.start as usize..].find("function")? as u32 + 8;
            kw + text[kw as usize..].find('(')? as u32
        }
    };
    Some((emit(ast, text, fe, Span::new(from, sp.end)), body, flags))
}

fn to_arrow(cx: &Ctx<'_, '_>, fe: NodeId) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let (rest, body, flags) = fn_expr_rest(cx, fe)?;
    if flags.contains(FnFlags::GENERATOR) || uses_this(cx, fe) || self_referenced(cx, fe) {
        return None;
    }
    let sp = ast.span(fe);
    let bsp = ast.span(body);
    let body_emitted = emit_node(ast, text, body);
    let head = rest.strip_suffix(body_emitted.as_str())?.trim_end();
    let asyn = if flags.contains(FnFlags::ASYNC) {
        "async "
    } else {
        ""
    };
    // An unclosed body at the end of the file is only `{`.
    let block = bsp.text(text);
    let block = block.strip_prefix('{').unwrap_or(block);
    let inner = block.strip_suffix('}').unwrap_or(block);
    let body_text = if let Some(e) = arrow_body_of(cx, body) {
        e
    } else if inner.trim().is_empty() && ast.comments_in(bsp).is_empty() {
        " { }".to_string()
    } else {
        format!(" {body_emitted}")
    };
    Some(vec![Edit::new(
        sp.start,
        sp.end,
        format!("{asyn}{head} =>{body_text}"),
    )])
}

/// ` E` for a function body of one `return E;` (leading comments on lines of their own
/// before `E`), `None` when the block stays.
fn arrow_body_of(cx: &Ctx<'_, '_>, block: NodeId) -> Option<String> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::Block { stmts } = *ast.node(block) else {
        return None;
    };
    let &[ret] = ast.list(stmts) else {
        return None;
    };
    let Node::Return { arg: Some(e) } = *ast.node(ret) else {
        return None;
    };
    let bsp = ast.span(block);
    let rsp = ast.span(ret);
    let comments = ast.comments_in(bsp);
    if comments.iter().any(|c| c.span.start >= rsp.start) {
        return None;
    }
    let esp = ast.span(e);
    let from = style::line_indent(text, rsp.start).to_string();
    let to = style::line_indent(text, bsp.start).to_string();
    let keep = templates(ast, e);
    let et = reindent_with(
        text,
        esp,
        &emit_node(ast, text, e),
        &keep,
        |old, _| match old.strip_prefix(from.as_str()) {
            Some(rel) => format!("{to}{rel}"),
            None => old.to_string(),
        },
    );
    let et = if needs_parens(ast, text, e) {
        format!("({et})")
    } else {
        et
    };
    if comments.is_empty() {
        return Some(format!(" {et}"));
    }
    let nl = style::newline(text);
    let ind = format!("{to}{}", style::indent_unit(text));
    let mut out = String::new();
    for c in comments {
        out.push_str(&format!("{nl}{ind}{}", c.span.text(text)));
    }
    out.push_str(&format!("{nl}{ind}{et}"));
    Some(out)
}

/// The variable statement and declarator whose initializer is `init` (an arrow or function
/// expression), when it can become a function declaration.
fn var_of_init(cx: &Ctx<'_, '_>, init: NodeId) -> Option<(NodeId, NodeId)> {
    let d = parent_of(cx, init)?;
    let Node::VarDeclarator { init: Some(i), .. } = *cx.checker.file(cx.file).ast.node(d) else {
        return None;
    };
    if i != init {
        return None;
    }
    Some((parent_of(cx, d)?, d))
}

fn to_named(cx: &Ctx<'_, '_>, vd: NodeId, d: NodeId, out: &mut Vec<Action>) {
    if let Some(edits) = named_edits(cx, vd, d) {
        out.push(action(
            cx,
            TO_NAMED,
            "refactor.rewrite.function.named",
            CONVERT,
            edits,
        ));
    }
}

/// Whether something in this file assigns to a variable spelled `name` (by name: a
/// shadowing variable counts too).
fn assigned(cx: &Ctx<'_, '_>, name: &str) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    super::super::locate::names_in(ast).into_iter().any(|id| {
        if ast.name_of(id) != Some(name) {
            return false;
        }
        // Up through destructuring targets to what writes them.
        let mut n = id;
        while let Some(p) = parents.parent(n) {
            match *ast.node(p) {
                Node::Assign { left, .. } | Node::ForIn { left, .. } | Node::ForOf { left, .. } => {
                    return left == n;
                }
                Node::Update { .. } => return true,
                Node::ArrayLit { .. }
                | Node::ObjectLit { .. }
                | Node::Spread { .. }
                | Node::Property { .. }
                | Node::ShorthandProp { .. }
                | Node::AssignPattern { .. }
                | Node::Paren { .. } => n = p,
                _ => return false,
            }
        }
        false
    })
}

fn named_edits(cx: &Ctx<'_, '_>, vd: NodeId, d: NodeId) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let Node::VarDecl {
        modifiers,
        kind,
        decls,
    } = *ast.node(vd)
    else {
        return None;
    };
    if modifiers.contains(Modifiers::DECLARE)
        || !matches!(kind, VarKind::Const | VarKind::Let | VarKind::Var)
    {
        return None;
    }
    let p = parent_of(cx, vd)?;
    if !matches!(
        ast.node(p),
        Node::Program { .. } | Node::Block { .. } | Node::ModuleBlock { .. }
    ) {
        return None;
    }
    let Node::VarDeclarator {
        name,
        type_ann: None,
        init: Some(init),
        ..
    } = *ast.node(d)
    else {
        return None;
    };
    let name_text = super::super::locate::name_text(ast, name)?;
    if !matches!(ast.node(name), Node::Ident { .. })
        || uses_this(cx, init)
        || kind != VarKind::Const && assigned(cx, name_text)
    {
        return None;
    }
    let vsp = ast.span(vd);
    let sind = style::line_indent(text, vsp.start).to_string();
    let func = match *ast.node(init) {
        Node::Arrow { flags, body, .. } => {
            let head = arrow_head(cx, init)?;
            let body_text = if flags.contains(FnFlags::EXPRESSION_BODY) {
                block_of(cx, body, &sind)
            } else {
                emit_node(ast, text, body)
            };
            let asyn = if flags.contains(FnFlags::ASYNC) {
                "async "
            } else {
                ""
            };
            format!("{asyn}function {name_text}{head} {body_text}")
        }
        Node::FunctionExpr { .. } => {
            if self_referenced(cx, init) {
                return None;
            }
            let (rest, _, flags) = fn_expr_rest(cx, init)?;
            let asyn = if flags.contains(FnFlags::ASYNC) {
                "async "
            } else {
                ""
            };
            let star = if flags.contains(FnFlags::GENERATOR) {
                "*"
            } else {
                ""
            };
            format!("{asyn}function{star} {name_text}{rest}")
        }
        _ => return None,
    };
    // `export ` and other modifiers, as written.
    let list = ast.list(decls);
    let head = &text[vsp.start as usize..ast.span(list[0]).start as usize];
    let kw = ["const", "let", "var"]
        .iter()
        .filter_map(|k| head.rfind(k))
        .max()?;
    let mods = &head[..kw];
    if list.len() == 1 {
        return Some(vec![Edit::new(vsp.start, vsp.end, format!("{mods}{func}"))]);
    }
    let i = list.iter().position(|&x| x == d)?;
    let nl = style::newline(text);
    let mut edits = super::unused::delete_element(ast, list, i);
    edits.push(Edit::insert(vsp.end, format!("{nl}{sind}{mods}{func}")));
    Some(edits)
}

// ---- parameters to a destructured object ----

/// One parameter turned into a member.
struct Member {
    name: String,
    binding: String,
    ty: Option<String>,
    optional: bool,
    rest: bool,
}

fn destructure(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    req: &Request<'_>,
    func: NodeId,
    out: &mut Vec<Action>,
) {
    if !req.wants("refactor.rewrite.parameters.toDestructured") {
        return;
    }
    if let Some(a) = destructure_action(cx, ck, func) {
        out.push(a);
    }
}

/// The function node (`FunctionDecl`, `FunctionExpr` or `Arrow`), its parameter list and the
/// name node its callers use (`None` for an anonymous default export).
fn destructure_target(cx: &Ctx<'_, '_>, func: NodeId) -> Option<(NodeId, Option<NodeId>, Kind)> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let b = &f.bound;
    match *ast.node(func) {
        Node::FunctionDecl {
            name,
            modifiers,
            body: Some(_),
            ..
        } => {
            if name.is_none() && !modifiers.contains(Modifiers::DEFAULT) {
                return None;
            }
            // Overloaded functions are not offered.
            if let Some(sym) = b.symbol_of_decl(func)
                && b.decls(sym)
                    .iter()
                    .filter(|&&d| matches!(ast.node(d), Node::FunctionDecl { .. }))
                    .count()
                    > 1
            {
                return None;
            }
            Some((func, name, Kind::Function))
        }
        Node::MethodDef {
            kind, key, value, ..
        } => {
            let p = parent_of(cx, func)?;
            if !matches!(ast.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. }) {
                return None;
            }
            let Node::FunctionExpr { body: Some(_), .. } = *ast.node(value) else {
                return None;
            };
            let same_name = |m: NodeId| match *ast.node(m) {
                Node::MethodDef {
                    key: k, kind: mk, ..
                } => mk == kind && ast.name_of(k) == ast.name_of(key),
                _ => false,
            };
            let members = match *ast.node(p) {
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
                _ => return None,
            };
            if ast.list(members).iter().filter(|&&m| same_name(m)).count() > 1 {
                return None;
            }
            match kind {
                MethodKind::Method if matches!(ast.node(key), Node::Ident { .. }) => {
                    Some((value, Some(key), Kind::Method(p)))
                }
                MethodKind::Constructor => {
                    let cname = match *ast.node(p) {
                        Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
                        _ => None,
                    };
                    Some((value, Some(cname?), Kind::Constructor(p)))
                }
                _ => None,
            }
        }
        Node::Arrow { .. } | Node::FunctionExpr { .. } => {
            let (vd, d) = var_of_init(cx, func)?;
            let Node::VarDecl {
                kind: VarKind::Const,
                ..
            } = *ast.node(vd)
            else {
                return None;
            };
            let Node::VarDeclarator {
                name,
                type_ann: None,
                ..
            } = *ast.node(d)
            else {
                return None;
            };
            if !matches!(ast.node(name), Node::Ident { .. }) {
                return None;
            }
            Some((func, Some(name), Kind::Function))
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Function,
    /// A method of the class.
    Method(NodeId),
    /// The constructor of the class.
    Constructor(NodeId),
}

fn destructure_action(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, func: NodeId) -> Option<Action> {
    let f = cx.checker.file(cx.file);
    if !f.kind.is_typescript() {
        return None;
    }
    let ast = &f.ast;
    let text = f.text.as_str();
    let (fnode, name, kind) = destructure_target(cx, func)?;
    let params = match *ast.node(fnode) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. } => params,
        _ => return None,
    };
    let all = ast.list(params);
    let mut this_param = None;
    let mut members: Vec<Member> = Vec::new();
    for (i, &p) in all.iter().enumerate() {
        let Node::Param {
            decorators,
            modifiers,
            rest,
            pattern,
            optional,
            type_ann,
            init,
        } = *ast.node(p)
        else {
            return None;
        };
        if !ast.list(decorators).is_empty() || modifiers != Modifiers::NONE {
            return None;
        }
        let pname = super::super::locate::name_text(ast, pattern)?;
        if i == 0 && pname == "this" {
            this_param = Some((p, type_ann));
            continue;
        }
        if !matches!(ast.node(pattern), Node::Ident { .. }) || rest && type_ann.is_none() {
            return None;
        }
        let ty = match (type_ann, init) {
            // Comments up to the next parameter stay with the type.
            (Some(t), _) => Some(match all.get(i + 1) {
                Some(&next) => {
                    let mut ty = print_type(ast, text, t);
                    for c in ast.comments_in(Span::new(ast.span(p).end, ast.span(next).start)) {
                        ty.push(' ');
                        ty.push_str(&as_block_comment(text, c));
                    }
                    ty
                }
                None => print_type(ast, text, t),
            }),
            (None, Some(e)) => {
                let t = ck.check_expr(Loc::new(cx.file, e));
                let t = if ck.store().flags(t).intersects(TypeFlags::LITERAL) {
                    ck.base_of_literal(t)
                } else {
                    t
                };
                Some(ck.type_to_string(t))
            }
            (None, None) => None,
        };
        let binding = match (init, rest) {
            (Some(e), _) => format!("{pname} = {}", ast.span(e).text(text)),
            (None, true) => format!("{pname} = []"),
            (None, false) => pname.to_string(),
        };
        members.push(Member {
            name: pname.to_string(),
            binding,
            ty,
            optional: optional || init.is_some() || rest,
            rest,
        });
    }
    if members.is_empty() {
        return None;
    }
    // Every use must be a call we can rewrite.
    let calls = call_sites(cx, ck, fnode, name, kind)?;
    let mut a = Action::new(DESTRUCTURE, "refactor.rewrite.parameters.toDestructured");
    a.refactor = Some(format!("{DESTRUCTURE}/{DESTRUCTURE}"));
    // The declaration.
    let bindings: Vec<&str> = members.iter().map(|m| m.binding.as_str()).collect();
    let types: Vec<String> = members
        .iter()
        .map(|m| match &m.ty {
            Some(t) => format!("{}{}: {t}; ", m.name, if m.optional { "?" } else { "" }),
            None => format!("{}{}; ", m.name, if m.optional { "?" } else { "" }),
        })
        .collect();
    let mut param = format!("{{ {} }}: {{ {}}}", bindings.join(", "), types.concat());
    if members.iter().all(|m| m.optional) {
        param.push_str(" = {}");
    }
    let first = usize::from(this_param.is_some());
    let mut start = ast.span(all[first]).start;
    let ls = style::line_start(text, start);
    if style::blank(text, ls, start) && !text[..ls as usize].trim_end().ends_with(',') {
        start = ls;
    }
    let end = ast.span(*all.last()?).end;
    let mut edits = Vec::new();
    match this_param {
        Some((p, Some(t))) => {
            let psp = ast.span(p);
            let this_text = format!("this: {}", print_type(ast, text, t));
            edits.push(Edit::new(psp.start, end, format!("{this_text}, {param}")));
        }
        _ => edits.push(Edit::new(start, end, param)),
    }
    a.edit(&f.path, edits);
    for (file, call) in calls {
        let e = call_edit(cx, file, call, &members)?;
        a.edit(&cx.checker.file(file).path, e);
    }
    Some(a)
}

/// A type annotation as tsserver prints it: type literals as `{ a: T; b: U; }` (one line),
/// everything else as written.
fn print_type(ast: &Ast, text: &str, t: NodeId) -> String {
    let sp = ast.span(t);
    match *ast.node(t) {
        Node::TypeLiteral { members } if !sp.text(text).contains('\n') => {
            let ms = ast.list(members);
            if ms.is_empty() {
                return "{}".into();
            }
            let mut out = String::from("{ ");
            for &m in ms {
                let msp = ast.span(m);
                let mt = match *ast.node(m) {
                    Node::PropertySignature {
                        type_ann: Some(ta), ..
                    } => format!(
                        "{}{}",
                        &text[msp.start as usize..ast.span(ta).start as usize],
                        print_type(ast, text, ta)
                    ),
                    _ => msp.text(text).to_string(),
                };
                out.push_str(mt.trim_end_matches([';', ',']).trim_end());
                out.push_str("; ");
            }
            out.push('}');
            out
        }
        _ => sp.text(text).to_string(),
    }
}

/// Calls of the function to rewrite (file, `Call`/`New` node); `None` when a use is
/// something else.
fn call_sites(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    fnode: NodeId,
    name: Option<NodeId>,
    kind: Kind,
) -> Option<Vec<(FileId, NodeId)>> {
    let c = cx.checker;
    let f = c.file(cx.file);
    let ast = &f.ast;
    let mut out = Vec::new();
    let Some(name) = name else {
        // An anonymous default export: followed through `import x from`.
        let Kind::Function = kind else {
            return None;
        };
        let target = c.sym_of_decl(Loc::new(cx.file, fnode))?;
        let names = HashSet::from(["default".to_string()]);
        for (file, id) in references_of(cx, ck, target, names, true) {
            function_use(cx, file, id, &mut out)?;
        }
        return Some(out);
    };
    match kind {
        Kind::Function => {
            for (file, id) in references(cx, ck, name)? {
                function_use(cx, file, id, &mut out)?;
            }
        }
        Kind::Method(class) => {
            if has_heritage(ast, class) {
                return None;
            }
            // Subclasses could override it.
            let cname = match *ast.node(class) {
                Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
                _ => None,
            };
            if let Some(cn) = cname {
                for (file, id) in references(cx, ck, cn)? {
                    let p = c.file(file).parents().parent(id)?;
                    if matches!(c.ast(file).node(p), Node::ExprWithTypeArgs { .. }) {
                        return None;
                    }
                }
            }
            for (file, id) in references(cx, ck, name)? {
                let fa = c.ast(file);
                let p = c.file(file).parents().parent(id)?;
                let Node::Member { prop, .. } = *fa.node(p) else {
                    return None;
                };
                if prop != id {
                    return None;
                }
                let call = c.file(file).parents().parent(p)?;
                match *fa.node(call) {
                    Node::Call { callee, .. } if callee == p => out.push((file, call)),
                    _ => return None,
                }
            }
        }
        Kind::Constructor(class) => {
            if has_heritage(ast, class) {
                return None;
            }
            constructor_calls(cx, ck, Loc::new(cx.file, name), 0, &mut out)?;
        }
    }
    Some(out)
}

/// Records the call that reference `id` makes; `None` when it is not a call (imports and
/// re-exports are fine).
fn function_use(
    cx: &Ctx<'_, '_>,
    file: FileId,
    id: NodeId,
    out: &mut Vec<(FileId, NodeId)>,
) -> Option<()> {
    let c = cx.checker;
    let fa = c.ast(file);
    let p = c.file(file).parents().parent(id)?;
    match *fa.node(p) {
        Node::Call { callee, .. } if callee == id => out.push((file, p)),
        Node::Member { prop, .. } if prop == id => {
            let call = c.file(file).parents().parent(p)?;
            match *fa.node(call) {
                Node::Call { callee, .. } if callee == p => out.push((file, call)),
                _ => return None,
            }
        }
        Node::ImportSpecifier { .. }
        | Node::ExportSpecifier { .. }
        | Node::ImportDefaultSpecifier { .. } => {}
        _ => return None,
    }
    Some(())
}

fn has_heritage(ast: &Ast, class: NodeId) -> bool {
    match *ast.node(class) {
        Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => {
            !ast.list(heritage).is_empty()
        }
        _ => true,
    }
}

/// `new C(...)` of class name `cname`, and of subclasses that inherit its constructor;
/// `super(...)` in subclasses with their own.
fn constructor_calls(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    cname: Loc,
    depth: u32,
    out: &mut Vec<(FileId, NodeId)>,
) -> Option<()> {
    if depth > 8 {
        return None;
    }
    let c = cx.checker;
    let refs = references_in(cx, ck, cname)?;
    for (file, id) in refs {
        let fa = c.ast(file);
        let parents = c.file(file).parents();
        let p = parents.parent(id)?;
        match *fa.node(p) {
            Node::New {
                callee, has_args, ..
            } if callee == id => {
                if !has_args {
                    return None;
                }
                out.push((file, p));
            }
            Node::TypeRef { .. }
            | Node::QualifiedName { .. }
            | Node::ImportSpecifier { .. }
            | Node::ExportSpecifier { .. } => {}
            Node::Member { obj, .. } if obj == id => {}
            Node::ExprWithTypeArgs { expr, .. } if expr == id => {
                // `class Q extends C`.
                let hc = parents.parent(p)?;
                let Node::HeritageClause {
                    kind: ts_r_ast::HeritageKind::Extends,
                    ..
                } = *fa.node(hc)
                else {
                    continue;
                };
                let sub = parents.parent(hc)?;
                let (sname, members) = match *fa.node(sub) {
                    Node::ClassDecl { name, members, .. }
                    | Node::ClassExpr { name, members, .. } => (name, members),
                    _ => return None,
                };
                let ctor = fa.list(members).iter().copied().find(|&m| {
                    matches!(
                        fa.node(m),
                        Node::MethodDef {
                            kind: MethodKind::Constructor,
                            ..
                        }
                    )
                });
                match ctor {
                    Some(m) => super_calls(fa, file, m, out),
                    None => constructor_calls(cx, ck, Loc::new(file, sname?), depth + 1, out)?,
                }
            }
            _ => return None,
        }
    }
    Some(())
}

/// `super(...)` calls directly in constructor `m` (not in nested functions).
fn super_calls(ast: &Ast, file: FileId, m: NodeId, out: &mut Vec<(FileId, NodeId)>) {
    let mut stack = vec![m];
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::Call { callee, .. } if matches!(ast.node(callee), Node::Super {}) => {
                out.push((file, n));
            }
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::ClassDecl { .. }
                if n != m
                    && !matches!(ast.node(m), Node::MethodDef { value, .. } if *value == n) => {}
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
}

/// References (not the declaration) to the symbol declared by name `name` in this file.
fn references(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    name: NodeId,
) -> Option<Vec<(FileId, NodeId)>> {
    references_in(cx, ck, Loc::new(cx.file, name))
}

/// References to the symbol declared at `decl` (a name), in every source file of the
/// program, following imports that rename it. `None` when there is no symbol.
fn references_in(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    decl: Loc,
) -> Option<Vec<(FileId, NodeId)>> {
    use super::super::locate::{meaning_at, name_text};
    let c = cx.checker;
    let target = meaning_at(ck, decl).primary?;
    let target = c.resolve_alias(target).unwrap_or(target);
    let mut names: HashSet<String> = HashSet::new();
    names.insert(name_text(c.ast(decl.file), decl.node)?.to_string());
    // A default export is also imported under any name, and as `default`.
    let is_default = c
        .file(decl.file)
        .parents()
        .parent(decl.node)
        .is_some_and(|d| {
            c.ast(decl.file)
                .node(d)
                .modifiers()
                .contains(Modifiers::DEFAULT)
        });
    if is_default {
        names.insert("default".into());
    }
    Some(references_of(cx, ck, target, names, is_default))
}

/// References to symbol `target` (not its declaration) under any of `names`, or the names
/// imports and re-exports give it (`is_default`: default imports too).
fn references_of(
    cx: &Ctx<'_, '_>,
    ck: &mut Ck<'_, '_>,
    target: SymId,
    mut names: HashSet<String>,
    is_default: bool,
) -> Vec<(FileId, NodeId)> {
    use super::super::locate::{meaning_at, name_text, names_in};
    let c = cx.checker;
    let target_decls: HashSet<Loc> = c.decls(target).into_iter().collect();
    let files: Vec<FileId> = (0..c.files.len())
        .map(|i| FileId(i as u32))
        .filter(|&f| {
            let sf = c.file(f);
            !sf.is_lib && !sf.path.contains("/node_modules/")
        })
        .collect();
    // Names it is imported or re-exported as (`import { a as b }`).
    loop {
        let before = names.len();
        for &f in &files {
            let ast = c.ast(f);
            for id in names_in(ast) {
                let Some(p) = c.file(f).parents().parent(id) else {
                    continue;
                };
                let alias = match *ast.node(p) {
                    // `import x from` names a default export.
                    Node::ImportDefaultSpecifier { local } if local == id && is_default => {
                        names.insert(name_text(ast, id).unwrap_or("").to_string());
                        continue;
                    }
                    Node::ImportSpecifier {
                        imported: Some(i),
                        local,
                        ..
                    } if i == id => local,
                    Node::ExportSpecifier {
                        local,
                        exported: Some(e),
                        ..
                    } if local == id => e,
                    _ => continue,
                };
                if name_text(ast, id).is_some_and(|n| names.contains(n))
                    && let Some(a) = name_text(ast, alias)
                {
                    names.insert(a.to_string());
                }
            }
        }
        if names.len() == before {
            break;
        }
    }
    let mut out = Vec::new();
    for &f in &files {
        let sf = c.file(f);
        if !names.iter().any(|n| sf.text.contains(n.as_str())) {
            continue;
        }
        let ast = c.ast(f);
        for id in names_in(ast) {
            if !name_text(ast, id).is_some_and(|n| names.contains(n)) {
                continue;
            }
            let l = Loc::new(f, id);
            let m = meaning_at(ck, l);
            let hit = m.iter().any(|s| {
                let s = c.resolve_alias(s).unwrap_or(s);
                s == target || c.decls(s).iter().any(|d| target_decls.contains(d))
            });
            // Not the declaration itself.
            if !hit || m.is_decl && m.primary == Some(target) {
                continue;
            }
            out.push((f, id));
        }
    }
    out
}

/// The edit turning call `call`'s arguments into an object.
fn call_edit(
    cx: &Ctx<'_, '_>,
    file: FileId,
    call: NodeId,
    members: &[Member],
) -> Option<Vec<Edit>> {
    let sf = cx.checker.file(file);
    let ast = &sf.ast;
    let text = sf.text.as_str();
    let args = match *ast.node(call) {
        Node::Call { args, .. } | Node::New { args, .. } => ast.list(args),
        _ => return None,
    };
    let fixed = members.iter().filter(|m| !m.rest).count();
    let has_rest = members.iter().any(|m| m.rest);
    if args
        .iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        || (!has_rest && args.len() > members.len())
    {
        return None;
    }
    let mut props = Vec::new();
    for (i, m) in members.iter().enumerate() {
        if m.rest {
            if args.len() > fixed {
                let items: Vec<&str> = args[fixed..]
                    .iter()
                    .map(|&a| ast.span(a).text(text))
                    .collect();
                props.push(format!("{}: [{}]", m.name, items.join(", ")));
            }
            continue;
        }
        let Some(&arg) = args.get(i) else {
            continue;
        };
        let at = ast.span(arg).text(text);
        if matches!(ast.node(arg), Node::Ident { .. }) && at == m.name {
            props.push(m.name.clone());
        } else {
            props.push(format!("{}: {at}", m.name));
        }
    }
    if args.is_empty() {
        return Some(Vec::new());
    }
    let obj = format!("{{ {} }}", props.join(", "));
    Some(vec![Edit::new(
        ast.span(args[0]).start,
        ast.span(*args.last()?).end,
        obj,
    )])
}

// ---- overloads ----

fn overload(cx: &Ctx<'_, '_>, req: &Request<'_>, at: NodeId, out: &mut Vec<Action>) {
    if !req.wants("refactor.rewrite.function.overloadList") {
        return;
    }
    let f = cx.checker.file(cx.file);
    if !f.kind.is_typescript() {
        return;
    }
    if let Some(edits) = overload_edits(cx, at) {
        out.push(action(
            cx,
            OVERLOAD,
            "refactor.rewrite.function.overloadList",
            OVERLOAD,
            edits,
        ));
    }
}

/// A declaration's name text and kind, for grouping overloads.
fn overload_key(ast: &Ast, n: NodeId) -> Option<(u8, Option<&str>)> {
    match *ast.node(n) {
        Node::FunctionDecl { name, .. } => Some((0, Some(ast.name_of(name?)?))),
        Node::MethodDef {
            kind: MethodKind::Method,
            key,
            ..
        } => Some((1, Some(ast.name_of(key)?))),
        Node::MethodSignature {
            kind: MethodKind::Method,
            key,
            ..
        } => Some((2, Some(ast.name_of(key)?))),
        Node::CallSignature { .. } => Some((3, None)),
        Node::ConstructSignature { .. } => Some((4, None)),
        _ => None,
    }
}

/// Type params, params, return type and body of a declaration in an overload list.
type SigParts = (
    Option<NodeId>,
    ts_r_ast::NodeList,
    Option<NodeId>,
    Option<NodeId>,
);

fn sig_parts(ast: &Ast, n: NodeId) -> Option<SigParts> {
    match *ast.node(n) {
        Node::FunctionDecl {
            type_params,
            params,
            return_type,
            body,
            ..
        } => Some((type_params, params, return_type, body)),
        Node::MethodDef { value, .. } => match *ast.node(value) {
            Node::FunctionExpr {
                type_params,
                params,
                return_type,
                body,
                ..
            } => Some((type_params, params, return_type, body)),
            _ => None,
        },
        Node::MethodSignature {
            type_params,
            params,
            return_type,
            ..
        }
        | Node::CallSignature {
            type_params,
            params,
            return_type,
        }
        | Node::ConstructSignature {
            type_params,
            params,
            return_type,
        } => Some((type_params, params, return_type, None)),
        _ => None,
    }
}

fn overload_edits(cx: &Ctx<'_, '_>, at: NodeId) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    let key = overload_key(ast, at)?;
    let owner = parent_of(cx, at)?;
    let list = match *ast.node(owner) {
        Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
        Node::Interface { body, .. } => body,
        Node::TypeLiteral { members } => members,
        _ => return None,
    };
    let items = ast.list(list);
    let i = items.iter().position(|&x| x == at)?;
    // The consecutive run of declarations with the same key around `at`.
    let mut lo = i;
    while lo > 0 && overload_key(ast, items[lo - 1]) == Some(key) {
        lo -= 1;
    }
    let mut hi = i;
    while hi + 1 < items.len() && overload_key(ast, items[hi + 1]) == Some(key) {
        hi += 1;
    }
    let group = &items[lo..=hi];
    let parts: Vec<_> = group
        .iter()
        .map(|&n| sig_parts(ast, n))
        .collect::<Option<_>>()?;
    let (sigs, imp): (Vec<usize>, Vec<usize>) =
        (0..group.len()).partition(|&k| parts[k].3.is_none());
    // An implementation, if any, comes last; at least one overload besides it, or two.
    match imp.as_slice() {
        [] if sigs.len() >= 2 => {}
        [k] if *k == group.len() - 1 && !sigs.is_empty() => {}
        _ => return None,
    }
    // Same modifiers (`export`, `static`) on every declaration.
    let mods: HashSet<Modifiers> = group.iter().map(|&n| ast.node(n).modifiers()).collect();
    if mods.len() > 1 {
        return None;
    }
    let mut ret: Option<String> = None;
    let mut tuples = Vec::new();
    for &k in &sigs {
        let (tp, params, rt, _) = parts[k];
        if tp.is_some() {
            return None;
        }
        let r = rt
            .map(|t| ast.span(t).text(text).to_string())
            .unwrap_or_default();
        match &ret {
            None => ret = Some(r),
            Some(x) if *x == r => {}
            Some(_) => return None,
        }
        let mut elems = Vec::new();
        for &p in ast.list(params) {
            let Node::Param {
                rest,
                pattern,
                optional,
                type_ann: Some(t),
                init: None,
                modifiers: Modifiers::NONE,
                ..
            } = *ast.node(p)
            else {
                return None;
            };
            let pn = super::super::locate::name_text(ast, pattern)?;
            if pn == "this" || !matches!(ast.node(pattern), Node::Ident { .. }) {
                return None;
            }
            let ty = print_type(ast, text, t);
            elems.push(if rest {
                format!("...{pn}: {ty}")
            } else {
                format!("{pn}{}: {ty}", if optional { "?" } else { "" })
            });
        }
        tuples.push(format!("[{}]", elems.join(", ")));
    }
    let args = format!("...args: {}", tuples.join(" | "));
    let first = group[0];
    let last = *group.last()?;
    let nl_end = |end: u32| {
        let rest = &text[end as usize..];
        let t = rest.trim_start_matches([' ', '\t']);
        if t.starts_with([';', ',']) {
            end + (rest.len() - t.len()) as u32 + 1
        } else {
            end
        }
    };
    let new_text = match imp.first() {
        Some(&k) => {
            let n = group[k];
            let (tp, params, rt, body) = parts[k];
            if tp.is_some() {
                return None;
            }
            // The implementation's parameters go: its body must not use them.
            let body = body?;
            let bsp = ast.span(body);
            for &p in ast.list(params) {
                let Node::Param { pattern, .. } = *ast.node(p) else {
                    return None;
                };
                let sym = b.symbol_of_decl(p).or_else(|| b.symbol_of_decl(pattern))?;
                if b.refs.resolved.iter().any(|&(r, s)| {
                    s == sym && {
                        let rs = ast.span(r);
                        bsp.start <= rs.start && rs.end <= bsp.end
                    }
                }) {
                    return None;
                }
            }
            let ps = ast.list(params);
            let open = head_paren(ast, text, n, ps)?;
            let prefix = text[ast.span(n).start as usize..open as usize].trim_end();
            let rt = rt
                .map(|t| format!(": {}", ast.span(t).text(text)))
                .unwrap_or_default();
            let body_text = if bsp.text(text)[1..bsp.text(text).len() - 1]
                .trim()
                .is_empty()
                && ast.comments_in(bsp).is_empty()
            {
                "{ }".to_string()
            } else {
                bsp.text(text).to_string()
            };
            format!("{prefix}({args}){rt} {body_text}")
        }
        None => {
            let n = group[sigs[0]];
            let ps = ast.list(parts[sigs[0]].1);
            let open = head_paren(ast, text, n, ps)?;
            let prefix = text[ast.span(n).start as usize..open as usize].trim_end();
            let r = ret.unwrap_or_default();
            let r = if r.is_empty() {
                String::new()
            } else {
                format!(": {r}")
            };
            format!("{prefix}({args}){r};")
        }
    };
    let end = if imp.is_empty() {
        nl_end(ast.span(last).end)
    } else {
        ast.span(last).end
    };
    Some(vec![Edit::new(ast.span(first).start, end, new_text)])
}

/// The `(` opening the parameter list of declaration `n`.
fn head_paren(ast: &Ast, text: &str, n: NodeId, params: &[NodeId]) -> Option<u32> {
    let sp = ast.span(n);
    let limit = params.first().map_or(sp.end, |&p| ast.span(p).start);
    let after = match *ast.node(n) {
        Node::FunctionDecl { name: Some(nm), .. } => ast.span(nm).end,
        Node::MethodDef { key, .. } | Node::MethodSignature { key, .. } => ast.span(key).end,
        _ => sp.start,
    };
    if limit < after {
        return None;
    }
    let i = text[after as usize..limit as usize].find('(')?;
    Some(after + i as u32)
}
