//! Helpers for the round-6 eslint-plugin-unicorn control-flow and condition rules: negated
//! condition text, subtree searches that stop at function and loop boundaries, and edits
//! that splice a removal fix into a larger one.

#![allow(dead_code)] // A shared toolkit: each rule uses part of it.

use super::prelude::*;
use super::unicorn_text::{expression_text, is_parenthesized, paren_text};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

/// `shouldAddParenthesesToUnaryExpressionArgument`: `expr` needs parentheses as the operand
/// of a prefix unary operator (looser precedence, or an update, `x!` or `<T>x`, which read
/// ambiguously there).
pub(crate) fn unary_argument_needs_parens(ast: &Ast, expr: NodeId) -> bool {
    precedence(ast, expr) < 16
        || matches!(
            ast.node(expr),
            Node::Update { .. } | Node::NonNull { .. } | Node::TypeAssertion { .. }
        )
}

/// `getNegatedExpressionText`: `!expr`, with parentheses when needed; redundant parentheses
/// around `expr` are dropped unless they hold comments.
pub(crate) fn negated_expression_text(cx: &Ctx<'_, '_>, expr: NodeId) -> String {
    let expr = skip_parens(cx.ast(), expr);
    format!(
        "!{}",
        expression_text(cx, expr, unary_argument_needs_parens(cx.ast(), expr))
    )
}

/// A loop statement.
pub(crate) fn is_loop(node: &Node) -> bool {
    matches!(
        node,
        Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
    )
}

/// A function of any kind.
pub(crate) fn is_function_node(node: &Node) -> bool {
    is_function(node.kind())
}

/// An unlabeled `break` under `root` (inclusive) that would leave the enclosing loop: none
/// inside a function, loop or `switch` (`root` itself included) counts.
pub(crate) fn has_unlabeled_break(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let node = ast.node(id);
            if matches!(node, Node::Break { label: None }) {
                self.found = true;
                return Walk::Stop;
            }
            if is_function_node(node) || is_loop(node) || matches!(node, Node::Switch { .. }) {
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

/// The identifiers under `root` (inclusive) in source order, not entering type nodes.
pub(crate) fn idents_in(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    struct Collect {
        out: Vec<NodeId>,
    }
    impl Visitor for Collect {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(ast.node(id), Node::Ident { .. }) {
                self.out.push(id);
            }
            Walk::Continue
        }
    }
    let mut c = Collect { out: Vec::new() };
    walk(ast, root, &mut c);
    c.out
}

/// `fix` followed by the edits of `more`.
pub(crate) fn join_fixes(mut fix: Fix, more: Fix) -> Fix {
    for e in more.edits {
        fix = fix.and(e.span, e.text);
    }
    fix
}

/// `isGlobalNameAvailable`: the global `name` is not shadowed by a value binding at `at`
/// (a type-only declaration such as `type Boolean = ...` doesn't hide it).
pub(crate) fn global_value_available(cx: &Ctx<'_, '_>, at: NodeId, name: &str) -> bool {
    let sem = cx.semantic();
    match sem.lookup(sem.enclosing_scope(at), name) {
        Some(s) => {
            let sym = sem.symbol(s);
            sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) || !sym.is_value()
        }
        None => true,
    }
}

/// `id` without parentheses and TS expression wrappers.
pub(crate) fn unwrap_ts_and_parens(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..100_000 {
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
    id
}

/// An optional link in the chain `id` ends (TS wrappers and parentheses looked through).
pub(crate) fn has_optional_chain_element(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..100_000 {
        id = unwrap_ts_and_parens(ast, id);
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

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `reindentText`: every line after the first that starts with `from` gets `to` in its
/// place; blank lines become empty; other lines and the line breaks are kept.
pub(crate) fn reindent_text(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut first = true;
    for _ in 0..text.len() + 1 {
        let brk = rest.char_indices().find(|&(_, c)| is_line_break(c));
        let (line, next) = match brk {
            Some((i, c)) => {
                let len = if rest[i..].starts_with("\r\n") {
                    2
                } else {
                    c.len_utf8()
                };
                (&rest[..i], Some((&rest[i..i + len], &rest[i + len..])))
            }
            None => (rest, None),
        };
        if first {
            out.push_str(line);
            first = false;
        } else if line.trim().is_empty() {
        } else if let Some(stripped) = line.strip_prefix(from) {
            out.push_str(to);
            out.push_str(stripped);
        } else {
            out.push_str(line);
        }
        let Some((brk, after)) = next else {
            break;
        };
        out.push_str(brk);
        rest = after;
    }
    out
}

/// The text before `pos` on its line, when it is only whitespace (`pos` starts its line).
pub(crate) fn indent_if_line_start(src: &str, pos: usize) -> Option<&str> {
    let start = src[..pos]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_line_break(c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let before = &src[start..pos];
    before.trim().is_empty().then_some(before)
}

/// A TS expression wrapper: `as`, `satisfies`, `!`, `<T>x`, `f<T>`.
pub(crate) fn is_ts_wrapper(node: &Node) -> bool {
    matches!(
        node,
        Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. }
    )
}

/// `getOutermostTypeScriptExpression`: the outermost TS wrapper (through parentheses)
/// around `id`, or `id`.
pub(crate) fn outermost_ts_wrapper(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let mut cur = id;
    for _ in 0..100_000 {
        match parent_skip_parens(cx.ast(), cx.file.parents, cur) {
            Some(p) if is_ts_wrapper(cx.node(p)) => cur = p,
            _ => break,
        }
    }
    cur
}

/// `getLogicalExpressionChildText`: `id` with its parentheses, wrapped in new ones when it
/// would bind looser than an operand of `op` (any binary or a different logical operator,
/// `await`, `?:`, assignments, arrows, `yield`, sequences).
pub(crate) fn logical_child_text(cx: &Ctx<'_, '_>, id: NodeId, op: BinaryOp) -> String {
    let text = paren_text(cx, id);
    let needs = !is_parenthesized(cx, id)
        && match *cx.node(id) {
            Node::Binary { op: o, .. } => o != op,
            Node::Await { .. }
            | Node::Conditional { .. }
            | Node::Assign { .. }
            | Node::Arrow { .. }
            | Node::Yield { .. }
            | Node::Seq { .. } => true,
            _ => false,
        };
    if needs {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// A comment is the token right before `start` or right after `end`.
pub(crate) fn comment_next_to(cx: &Ctx<'_, '_>, start: u32, end: u32) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let len = src.len() as u32;
    let before = ast
        .comments_in(Span::new(0, start))
        .last()
        .is_some_and(|c| {
            c.span.end <= start && src[c.span.end as usize..start as usize].trim().is_empty()
        });
    before
        || ast
            .comments_in(Span::new(end, len))
            .first()
            .is_some_and(|c| src[end as usize..c.span.start as usize].trim().is_empty())
}

/// A comment is the token right before `start`.
pub(crate) fn comment_before(cx: &Ctx<'_, '_>, start: u32) -> bool {
    let src = cx.src();
    cx.ast()
        .comments_in(Span::new(0, start))
        .last()
        .is_some_and(|c| {
            c.span.end <= start && src[c.span.end as usize..start as usize].trim().is_empty()
        })
}

/// A comment is the token right after `end` (ESLint's `getCommentsAfter`).
pub(crate) fn comment_after(cx: &Ctx<'_, '_>, end: u32) -> bool {
    let src = cx.src();
    cx.ast()
        .comments_in(Span::new(end, src.len() as u32))
        .first()
        .is_some_and(|c| src[end as usize..c.span.start as usize].trim().is_empty())
}

/// `hasMultilineToken`: a string, template piece or JSX text under `root` spans lines.
pub(crate) fn has_multiline_token(cx: &Ctx<'_, '_>, root: NodeId) -> bool {
    struct Find<'s> {
        src: &'s str,
        found: bool,
    }
    impl Visitor for Find<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(
                ast.node(id),
                Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText { .. }
            ) && ast.span(id).text(self.src).contains(is_line_break)
            {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        src: cx.src(),
        found: false,
    };
    walk(cx.ast(), root, &mut f);
    f.found
}

/// `isBlockScopedDeclaration`: `let`/`const`/`using` declarations, classes, functions and
/// TS declarations (enums, `import x =`, interfaces, namespaces, type aliases).
pub(crate) fn is_block_scoped_declaration(node: &Node) -> bool {
    match node {
        Node::VarDecl { kind, .. } => *kind != VarKind::Var,
        Node::ClassDecl { .. }
        | Node::FunctionDecl { .. }
        | Node::Enum { .. }
        | Node::ImportEquals { .. }
        | Node::Interface { .. }
        | Node::Module { .. }
        | Node::TypeAlias { .. } => true,
        _ => false,
    }
}

/// The statements of a block, or the statement itself.
pub(crate) fn branch_statements(ast: &Ast, id: NodeId) -> Vec<NodeId> {
    match *ast.node(id) {
        Node::Block { stmts } => ast.list(stmts).to_vec(),
        _ => vec![id],
    }
}

/// ESLint's `getLineIndent` of a node or position.
pub(crate) fn line_indent_at<'a>(cx: &Ctx<'a, '_>, pos: u32) -> &'a str {
    super::unicorn_util_h2::line_indent(cx.src(), pos as usize)
}

/// `getUnwrappedBranchText`: an `if` branch without its braces, reindented to the `if`
/// line and starting with that indentation (empty for an empty block).
pub(crate) fn unwrapped_branch_text(cx: &Ctx<'_, '_>, if_stmt: NodeId, branch: NodeId) -> String {
    let src = cx.src();
    let indent = line_indent_at(cx, cx.span(if_stmt).start);
    let span = cx.span(branch);
    if !matches!(cx.node(branch), Node::Block { .. }) {
        return format!("{indent}{}", cx.text(branch));
    }
    let (open, close) = (span.start as usize + 1, span.end as usize - 1);
    let first = skip_trivia_forward_keep_comments(src, open);
    let body_indent = if first >= close {
        ""
    } else {
        super::unicorn_util_h2::line_indent(src, first)
    };
    let text = reindent_text(&src[open..close], body_indent, indent);
    let text = text.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!("{indent}{text}")
    }
}

/// The first byte at or after `pos` that isn't whitespace (comments count as tokens).
fn skip_trivia_forward_keep_comments(src: &str, pos: usize) -> usize {
    src[pos..]
        .char_indices()
        .find(|&(_, c)| !c.is_whitespace() && c != '\u{feff}')
        .map_or(src.len(), |(i, _)| pos + i)
}

/// The text inside the parentheses of `if (..)`: from after `(` to before `)`.
pub(crate) fn if_condition_range(cx: &Ctx<'_, '_>, if_stmt: NodeId) -> Option<Span> {
    let Node::If { cons, .. } = *cx.node(if_stmt) else {
        return None;
    };
    let src = cx.src();
    let open = skip_trivia_forward(src, cx.span(if_stmt).start as usize + 2);
    let close = skip_trivia_backward(cx.ast(), src, cx.span(cons).start as usize);
    (src.as_bytes().get(open) == Some(&b'(')
        && close > open + 1
        && src.as_bytes()[close - 1] == b')')
        .then(|| Span::new(open as u32 + 1, close as u32 - 1))
}

const LEXICAL: [VarKind; 2] = [VarKind::Let, VarKind::Const];

fn is_unsupported_block_scoped(node: &Node) -> bool {
    is_block_scoped_declaration(node)
        && !matches!(node, Node::VarDecl { kind, .. } if LEXICAL.contains(kind))
}

/// The names declared by `let`/`const` statements directly in the block `id`.
fn direct_lexical_names(cx: &Ctx<'_, '_>, id: NodeId) -> Vec<NodeId> {
    let ast = cx.ast();
    let Node::Block { stmts } = *ast.node(id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &s in ast.list(stmts) {
        if let Node::VarDecl { kind, decls, .. } = *ast.node(s)
            && LEXICAL.contains(&kind)
        {
            for &d in ast.list(decls) {
                if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                    out.extend(binding_idents(ast, name));
                }
            }
        }
    }
    out
}

/// The identifiers a binding pattern declares (not default values or computed keys).
fn binding_idents(ast: &Ast, pattern: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![pattern];
    while let Some(p) = stack.pop() {
        if out.len() + stack.len() > 100_000 {
            break;
        }
        match *ast.node(p) {
            Node::Ident { .. } => out.push(p),
            Node::ArrayPattern { elements } => {
                stack.extend(ast.list(elements).iter().rev().copied());
            }
            Node::ObjectPattern { props } => {
                stack.extend(ast.list(props).iter().rev().copied());
            }
            Node::AssignPattern { left, .. } => stack.push(left),
            Node::Rest { arg } => stack.push(arg),
            Node::BindingProperty { key, value, .. } => stack.push(value.unwrap_or(key)),
            _ => {}
        }
    }
    out
}

/// A name in `names` read in `root`, other than one declared inside `root`.
fn references_name(cx: &Ctx<'_, '_>, root: NodeId, names: &[&str]) -> bool {
    let sem = cx.semantic();
    let ast = cx.ast();
    let span = cx.span(root);
    idents_in(ast, root).into_iter().any(|id| {
        let Some(r) = sem.reference_at(id) else {
            return false;
        };
        ident_name(ast, id).is_some_and(|n| names.contains(&n))
            && !sem.reference(r).symbol().is_some_and(|s| {
                sem.symbol(s).decls.iter().any(|d| {
                    let ds = cx.span(d.name);
                    ds.start >= span.start && ds.end <= span.end
                })
            })
    })
}

/// A direct `eval(..)` call under `root`.
fn contains_direct_eval(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if super::unicorn_util_h1::is_direct_eval_call(ast, id) {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

/// `canRewriteToEarlyExit` for the `if` statement `if_stmt` that ends the statement list
/// `owner_stmts` of `owner` (a loop body or a function body): moving its consequent out
/// keeps the behavior and every comment (comments after the `if` aren't checked).
/// `function_scope_names` tells whether a function's own scope (its parameters) already
/// declares one of the given names.
pub(crate) fn can_rewrite_to_early_exit(
    cx: &Ctx<'_, '_>,
    if_stmt: NodeId,
    owner_len: usize,
    function_scope_has: &dyn Fn(&[&str]) -> bool,
) -> bool {
    let ast = cx.ast();
    let Node::If { test, cons, .. } = *ast.node(if_stmt) else {
        return false;
    };
    if is_unsupported_block_scoped(ast.node(cons))
        || matches!(*ast.node(cons), Node::Block { stmts } if ast.list(stmts).iter().any(|&s| is_unsupported_block_scoped(ast.node(s))))
    {
        return false;
    }
    let declared = direct_lexical_names(cx, cons);
    if !declared.is_empty() {
        let names: Vec<&str> = declared
            .iter()
            .filter_map(|&n| ident_name(ast, n))
            .collect();
        if owner_len > 1
            || contains_direct_eval(ast, test)
            || function_scope_has(&names)
            || references_name(cx, test, &names)
        {
            return false;
        }
    }
    let Some(condition) = if_condition_range(cx, if_stmt) else {
        return false;
    };
    let is_block = matches!(ast.node(cons), Node::Block { .. });
    if has_multiline_token(cx, cons)
        || super::unicorn_text::would_remove_comments(
            cx,
            cx.span(if_stmt),
            &[condition, cx.span(cons)],
        )
        || (!is_block && cx.text(cons).contains('\n'))
    {
        return false;
    }
    if is_block {
        // A line comment ending the body would swallow a token after the `}` on its line.
        let close = cx.span(cons).end - 1;
        let src = cx.src();
        let before_close = src[..close as usize]
            .trim_end_matches(char::is_whitespace)
            .len() as u32;
        let line_comment_last = ast
            .comments_in(Span::new(0, close))
            .last()
            .is_some_and(|c| {
                c.span.end == before_close && src[c.span.start as usize..].starts_with("//")
            });
        if line_comment_last {
            let next = skip_trivia_forward_keep_comments(src, close as usize + 1);
            if next < src.len()
                && cx.file.lines.line_of(next as u32) == cx.file.lines.line_of(close)
            {
                return false;
            }
        }
    }
    true
}

/// The negated `if` condition: `!x` loses its `!` (keeping a comment after it), comments
/// in the parentheses around the test keep the whole condition text in `!(..)`.
pub(crate) fn negated_if_condition(cx: &Ctx<'_, '_>, if_stmt: NodeId) -> Option<String> {
    let ast = cx.ast();
    let Node::If { test, .. } = *ast.node(if_stmt) else {
        return None;
    };
    let range = if_condition_range(cx, if_stmt)?;
    let src = cx.src();
    let test = skip_parens(ast, test);
    if super::unicorn_text::would_remove_comments(cx, range, &[cx.span(test)]) {
        return Some(format!(
            "!({})",
            src[range.start as usize..range.end as usize].trim_start()
        ));
    }
    if let Node::Unary {
        op: UnaryOp::Not,
        arg,
    } = *ast.node(test)
    {
        let op_end = cx.span(test).start + 1;
        let arg_start = super::unicorn_text::paren_span(cx, arg).start;
        if !ast.comments_in(Span::new(op_end, arg_start)).is_empty() {
            return Some(
                src[op_end as usize..cx.span(test).end as usize]
                    .trim()
                    .to_owned(),
            );
        }
        return Some(super::unicorn_text::paren_text(cx, arg).to_owned());
    }
    Some(negated_expression_text(cx, test))
}

/// `getEarlyExitReplacementText`: `if (<condition>) { <exit>; }`, a blank line, then the
/// unwrapped consequent.
pub(crate) fn early_exit_text(
    cx: &Ctx<'_, '_>,
    if_stmt: NodeId,
    keyword: &str,
    condition: &str,
) -> String {
    let Node::If { cons, .. } = *cx.node(if_stmt) else {
        return String::new();
    };
    let indent = line_indent_at(cx, cx.span(if_stmt).start);
    let unit = super::unicorn_util_h2::indent_unit(cx);
    let lb = super::unicorn_ast::linebreak(cx.src());
    let body = unwrapped_branch_text(cx, if_stmt, cons);
    format!("if ({condition}) {{{lb}{indent}{unit}{keyword};{lb}{indent}}}{lb}{lb}{body}")
}

/// `getEarlyExitOppositeComparisonFix`'s condition: a relational test (TS wrappers looked
/// through) with its operator flipped (`<` to `>=`, ...), the rest of the condition text
/// kept.
pub(crate) fn opposite_comparison_condition(cx: &Ctx<'_, '_>, if_stmt: NodeId) -> Option<String> {
    let ast = cx.ast();
    let Node::If { test, .. } = *ast.node(if_stmt) else {
        return None;
    };
    let Node::Binary { left, op, .. } = *ast.node(unwrap_ts_and_parens(ast, test)) else {
        return None;
    };
    let flipped = match op {
        BinaryOp::Lt => ">=",
        BinaryOp::LtEq => ">",
        BinaryOp::Gt => "<=",
        BinaryOp::GtEq => "<",
        _ => return None,
    };
    let range = if_condition_range(cx, if_stmt)?;
    let src = cx.src();
    // The operator: the first token after the left operand that isn't a `)`.
    let mut pos = super::unicorn_text::paren_span(cx, left).end as usize;
    for _ in 0..src.len() {
        pos = skip_trivia_forward(src, pos);
        if src.as_bytes().get(pos) == Some(&b')') {
            pos += 1;
        } else {
            break;
        }
    }
    let op_text = op.as_str();
    if !src[pos..].starts_with(op_text) {
        return None;
    }
    Some(format!(
        "{}{flipped}{}",
        src[range.start as usize..pos].trim_start(),
        &src[pos + op_text.len()..range.end as usize]
    ))
}

/// `if (..) continue;` / `if (..) return;` (no label, no value; the only statement of its
/// branch), not followed by `else if`.
fn is_exit_guard(ast: &Ast, id: Option<NodeId>, is_continue: bool) -> bool {
    let Some(id) = id else {
        return false;
    };
    let Node::If { cons, alt, .. } = *ast.node(id) else {
        return false;
    };
    if alt.is_some_and(|a| matches!(ast.node(a), Node::If { .. })) {
        return false;
    }
    match branch_statements(ast, cons)[..] {
        [only] => match *ast.node(only) {
            Node::Continue { label: None } => is_continue,
            Node::Return { arg: None } => !is_continue,
            _ => false,
        },
        _ => false,
    }
}

fn is_labeled_function(ast: &Ast, mut id: NodeId) -> bool {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Labeled { body, .. } => {
                if matches!(ast.node(body), Node::FunctionDecl { .. }) {
                    return true;
                }
                id = body;
            }
            _ => return false,
        }
    }
    false
}

/// The `shared/short-body-conditional` check: the last exit guard of `block` (the body of a
/// loop or function) whose remaining body, or `else` body, has 1 to `maximum` statements,
/// and the fix wrapping that body in the opposite condition. `None` when there's no such
/// guard; the fix is `None` when comments or declarations prevent it.
pub(crate) fn short_body_problem(
    cx: &Ctx<'_, '_>,
    block: NodeId,
    maximum: usize,
    is_continue: bool,
) -> Option<(NodeId, Option<Fix>)> {
    let ast = cx.ast();
    let Node::Block { stmts } = *ast.node(block) else {
        return None;
    };
    let body = ast.list(stmts);
    if maximum == 0 {
        return None;
    }
    let index = body
        .iter()
        .rposition(|&s| is_exit_guard(ast, Some(s), is_continue))?;
    let guard = body[index];
    let Node::If { test, alt, .. } = *ast.node(guard) else {
        return None;
    };
    let previous = body[..index]
        .iter()
        .rev()
        .find(|&&s| !matches!(ast.node(s), Node::Empty {}))
        .copied();
    if (alt.is_some() && index != body.len() - 1) || is_exit_guard(ast, previous, is_continue) {
        return None;
    }
    let statements = match alt {
        Some(a) => branch_statements(ast, a),
        None => body[index + 1..].to_vec(),
    };
    let count = statements
        .iter()
        .filter(|&&s| !matches!(ast.node(s), Node::Empty {}))
        .count();
    if count == 0 || count > maximum {
        return None;
    }
    Some((
        guard,
        short_body_fix(cx, block, guard, test, alt, &statements),
    ))
}

fn short_body_fix(
    cx: &Ctx<'_, '_>,
    block: NodeId,
    guard: NodeId,
    test: NodeId,
    alt: Option<NodeId>,
    statements: &[NodeId],
) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let last = if alt.is_some() {
        guard
    } else {
        *statements.last()?
    };
    let range = Span::new(cx.span(guard).start, cx.span(last).end);
    if !ast.comments_in(range).is_empty() || comment_after(cx, range.end) {
        return None;
    }
    let test = skip_parens(ast, test);
    let condition = match *ast.node(test) {
        Node::Unary {
            op: UnaryOp::Not,
            arg,
        } => super::unicorn_text::paren_text(cx, arg).to_owned(),
        _ if unary_argument_needs_parens(ast, test) => format!("!({})", cx.text(test)),
        _ => format!("!{}", cx.text(test)),
    };
    let text = if let Some(a) = alt.filter(|&a| matches!(ast.node(a), Node::Block { .. })) {
        format!("if ({condition}) {}", cx.text(a))
    } else {
        if statements.iter().any(|&s| {
            is_block_scoped_declaration(ast.node(s))
                || is_labeled_function(ast, s)
                || has_multiline_token(cx, s)
        }) {
            return None;
        }
        let indent = line_indent_at(cx, cx.span(guard).start);
        let body_indent = format!("{indent}{}", super::unicorn_util_h2::indent_unit(cx));
        let first = cx.span(statements[0]).start;
        let end = cx.span(*statements.last()?).end;
        let body = reindent_text(
            &src[first as usize..end as usize],
            line_indent_at(cx, first),
            &body_indent,
        );
        let lb = super::unicorn_ast::linebreak(src);
        format!("if ({condition}) {{{lb}{body_indent}{body}{lb}{indent}}}")
    };
    // The empty insert at the owner's start keeps other fixes in that body out of this pass.
    let owner = cx.parent(block).unwrap_or(block);
    let at = cx.span(owner).start;
    Some(Fix::replace(range, text).and(Span::new(at, at), String::new()))
}
