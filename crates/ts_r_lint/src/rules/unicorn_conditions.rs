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
    super::unicorn_util::line_indent(cx.src(), pos as usize)
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
        super::unicorn_util::line_indent(src, first)
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
            if super::unicorn_util::is_direct_eval_call(ast, id) {
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
    let unit = super::unicorn_util::indent_unit(cx);
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
        let body_indent = format!("{indent}{}", super::unicorn_util::indent_unit(cx));
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

/// A `boolean` keyword or a `true`/`false` literal type.
fn is_boolean_type(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Boolean
        } | Node::BoolLit { .. }
    )
}

/// `isBoolean` with a few more of the plugin's syntax cases than
/// [`super::unicorn_types::is_boolean`]: an optional chain may give `undefined`;
/// `as`/`<T>`/`satisfies` with a boolean type; `satisfies` (or `as any`/`as unknown`) of a
/// boolean; and calls of a stable local function (a declaration in module code or a
/// `const` function value without other writes) whose only result is boolean.
pub(crate) fn is_boolean_expression(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    boolean_expression(cx, id, 0)
}

fn boolean_expression(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if depth > 50 {
        return false;
    }
    match *ast.node(id) {
        Node::Member { .. } | Node::Index { .. } | Node::Call { .. }
            if has_optional_chain_element(ast, id) =>
        {
            false
        }
        Node::Satisfies { ty, expr } => {
            is_boolean_type(ast, ty)
                || boolean_expression(cx, expr, depth + 1)
                || super::unicorn_types::is_boolean(cx, id)
        }
        Node::As { ty, expr } | Node::TypeAssertion { ty, expr } => {
            is_boolean_type(ast, ty)
                || (matches!(
                    ast.node(ty),
                    Node::KeywordType {
                        kind: KeywordTypeKind::Any | KeywordTypeKind::Unknown
                    }
                ) && boolean_expression(cx, expr, depth + 1))
                || super::unicorn_types::is_boolean(cx, id)
        }
        Node::Call { callee, .. } => {
            super::unicorn_types::is_boolean(cx, id)
                || boolean_function_reference(cx, skip_parens(ast, callee), depth + 1)
        }
        _ => super::unicorn_types::is_boolean(cx, id),
    }
}

fn boolean_function_reference(cx: &Ctx<'_, '_>, callee: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    if !matches!(ast.node(callee), Node::Ident { .. }) {
        return false;
    }
    let Some(sym) = sem.symbol_of_ident(callee) else {
        return false;
    };
    let symbol = sem.symbol(sym);
    let [decl] = symbol.decls[..] else {
        return false;
    };
    // A JSDoc type decides on its own (the base `isBoolean` reads it).
    if super::unicorn_types::jsdoc_type_annotation(cx, decl.name).is_some() {
        return false;
    }
    let writes = |allowed: Option<NodeId>| {
        symbol.refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && Some(r.node) != allowed
        })
    };
    let function = match decl.kind {
        DeclKind::Function | DeclKind::FunctionExprName => {
            let stable = decl.kind == DeclKind::FunctionExprName
                || cx.env().source_type == crate::env::SourceType::Module;
            if !stable || writes(None) {
                return false;
            }
            decl.node
        }
        _ => {
            let Node::VarDeclarator {
                name,
                init: Some(mut init),
                type_ann: None,
                ..
            } = *ast.node(decl.node)
            else {
                return false;
            };
            if name != decl.name || writes(Some(name)) {
                return false;
            }
            for _ in 0..1000 {
                match *ast.node(init) {
                    Node::Satisfies { expr, .. }
                    | Node::NonNull { expr }
                    | Node::Paren { expr } => {
                        init = expr;
                    }
                    _ => break,
                }
            }
            init
        }
    };
    let (flags, return_type, body) = match *ast.node(function) {
        Node::FunctionDecl {
            flags,
            return_type,
            body: Some(body),
            ..
        }
        | Node::FunctionExpr {
            flags,
            return_type,
            body: Some(body),
            ..
        } => (flags, return_type, body),
        Node::Arrow {
            flags,
            return_type,
            body,
            ..
        } => (flags, return_type, body),
        _ => return false,
    };
    if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR)
        || super::unicorn_types::jsdoc_return_type(cx, function).is_some()
    {
        return false;
    }
    if let Some(t) = return_type {
        return is_boolean_type(ast, t);
    }
    let result = match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts) {
            [only] => match *ast.node(*only) {
                Node::Return { arg: Some(arg) } => arg,
                _ => return false,
            },
            _ => return false,
        },
        _ => body,
    };
    boolean_expression(cx, result, depth + 1)
}

/// `process.exit(..)` on the global `process`, not optional or computed.
pub(crate) fn is_process_exit_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        ..
    } = *ast.node(unwrap_ts_and_parens(ast, id))
    else {
        return false;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(unwrap_ts_and_parens(ast, callee))
    else {
        return false;
    };
    let obj = skip_parens(ast, obj);
    ident_name(ast, obj) == Some("process")
        && ident_name(ast, prop) == Some("exit")
        && global_value_available(cx, obj, "process")
}

/// A literal whose truthiness is known: `Some(truthy)`.
fn literal_truthiness(ast: &Ast, id: NodeId) -> Option<bool> {
    match ast.node(skip_parens(ast, id)) {
        Node::BoolLit { value } => Some(*value),
        Node::NullLit {} => Some(false),
        Node::NumberLit { value } => {
            let v = value.get();
            Some(v != 0.0 && !v.is_nan())
        }
        Node::StringLit { value } => Some(!ast.atom(*value).is_empty()),
        Node::BigIntLit { value } => Some(
            !ast.atom(*value)
                .trim_start_matches(['0', 'x', 'X', 'o', 'O', 'b', 'B'])
                .is_empty(),
        ),
        Node::RegexLit { .. } => Some(true),
        _ => None,
    }
}

/// What a jump search looks for: `break`s (and, with `continue`, `continue`s) that land on
/// one statement: unlabeled ones outside any nested loop (or `switch`, for `break`), or
/// ones naming `label`.
#[derive(Clone, Copy)]
struct JumpTarget<'l> {
    label: Option<&'l str>,
    unlabeled: bool,
    with_continue: bool,
}

/// A jump to `t` under `id`, ignoring dead code (branches of a literal `if` test that
/// never run, statements after one that always exits) and nested functions.
fn jumps_to(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    t: JumpTarget<'_>,
    in_breakable: bool,
    in_loop: bool,
    depth: u32,
) -> bool {
    if depth > 500 {
        return true;
    }
    let ast = cx.ast();
    let next = depth + 1;
    let list = |stmts: &[NodeId], in_breakable: bool, in_loop: bool| {
        for &s in stmts {
            if jumps_to(cx, s, t, in_breakable, in_loop, next) {
                return true;
            }
            if exits(cx, s, next) {
                return false;
            }
        }
        false
    };
    let named = |label: Option<NodeId>| {
        t.label.is_some() && label.and_then(|l| ident_name(ast, l)) == t.label
    };
    match *ast.node(id) {
        Node::Break { label: None } => t.unlabeled && !in_breakable,
        Node::Continue { label: None } => t.unlabeled && t.with_continue && !in_loop,
        Node::Break { label } => named(label),
        Node::Continue { label } => t.with_continue && named(label),
        Node::Block { stmts } => list(ast.list(stmts), in_breakable, in_loop),
        Node::If { test, cons, alt } => match literal_truthiness(ast, test) {
            Some(true) => jumps_to(cx, cons, t, in_breakable, in_loop, next),
            Some(false) => alt.is_some_and(|a| jumps_to(cx, a, t, in_breakable, in_loop, next)),
            None => {
                jumps_to(cx, cons, t, in_breakable, in_loop, next)
                    || alt.is_some_and(|a| jumps_to(cx, a, t, in_breakable, in_loop, next))
            }
        },
        Node::For { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::While { body, .. }
        | Node::DoWhile { body, .. } => jumps_to(cx, body, t, true, true, next),
        Node::Switch { cases, .. } => ast.list(cases).iter().any(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => list(ast.list(body), true, in_loop),
            _ => false,
        }),
        Node::Labeled { body, .. } | Node::With { body, .. } => {
            jumps_to(cx, body, t, in_breakable, in_loop, next)
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            jumps_to(cx, block, t, in_breakable, in_loop, next)
                || handler.is_some_and(|h| match *ast.node(h) {
                    Node::CatchClause { body, .. } => {
                        jumps_to(cx, body, t, in_breakable, in_loop, next)
                    }
                    _ => false,
                })
                || finalizer.is_some_and(|f| jumps_to(cx, f, t, in_breakable, in_loop, next))
        }
        _ => false,
    }
}

/// The labels directly on `stmt` (`a: b: while (..)`).
fn own_labels<'a>(cx: &Ctx<'a, '_>, stmt: NodeId) -> Vec<&'a str> {
    let ast = cx.ast();
    let mut out = Vec::new();
    let mut cur = stmt;
    while let Some(p) = cx.parent(cur) {
        let Node::Labeled { label, .. } = *ast.node(p) else {
            break;
        };
        out.extend(ident_name(ast, label));
        cur = p;
    }
    out
}

/// A `break` (or, with `with_continue`, a `continue`) under `body` targets `stmt`.
fn targets(cx: &Ctx<'_, '_>, stmt: NodeId, body: NodeId, with_continue: bool, depth: u32) -> bool {
    let unlabeled = JumpTarget {
        label: None,
        unlabeled: true,
        with_continue,
    };
    jumps_to(cx, body, unlabeled, false, false, depth)
        || own_labels(cx, stmt).into_iter().any(|l| {
            let t = JumpTarget {
                label: Some(l),
                unlabeled: false,
                with_continue,
            };
            jumps_to(cx, body, t, false, false, depth)
        })
}

/// A global `process.exit(..)` is always evaluated in `id` (not behind a short circuit,
/// an optional chain or one side of `?:`, unless a literal decides it).
pub(crate) fn expression_exits(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    expr_exits(cx, id, 0)
}

fn expr_exits(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return false;
    }
    let ast = cx.ast();
    let next = depth + 1;
    let id = unwrap_ts_and_parens(ast, id);
    let any = |list: ts_r_ast::NodeList| ast.list(list).iter().any(|&e| expr_exits(cx, e, next));
    match *ast.node(id) {
        Node::Call { callee, args, .. } => {
            is_process_exit_call(cx, id)
                || expr_exits(cx, callee, next)
                || (!has_optional_chain_element(ast, id) && any(args))
        }
        Node::New { callee, args, .. } => expr_exits(cx, callee, next) || any(args),
        Node::Member { obj, .. } => expr_exits(cx, obj, next),
        Node::Index { obj, index, .. } => {
            expr_exits(cx, obj, next)
                || (!has_optional_chain_element(ast, id) && expr_exits(cx, index, next))
        }
        Node::TaggedTemplate { tag, quasi, .. } => {
            expr_exits(cx, tag, next) || expr_exits(cx, quasi, next)
        }
        Node::TemplateLit { parts } | Node::Seq { exprs: parts } => any(parts),
        Node::ArrayLit { elements } => any(elements),
        Node::ObjectLit { props } => ast.list(props).iter().any(|&p| match *ast.node(p) {
            Node::Property { key, value, .. } => {
                expr_exits(cx, key, next) || expr_exits(cx, value, next)
            }
            Node::Spread { arg } => expr_exits(cx, arg, next),
            _ => false,
        }),
        Node::Spread { arg }
        | Node::Unary { arg, .. }
        | Node::Await { arg }
        | Node::Update { arg, .. } => expr_exits(cx, arg, next),
        Node::Binary { left, op, right } => {
            if expr_exits(cx, left, next) {
                return true;
            }
            let right_runs = match op {
                BinaryOp::LogicalAnd => literal_truthiness(ast, left) == Some(true),
                BinaryOp::LogicalOr => literal_truthiness(ast, left) == Some(false),
                BinaryOp::Coalesce => matches!(ast.node(skip_parens(ast, left)), Node::NullLit {}),
                _ => true,
            };
            right_runs && expr_exits(cx, right, next)
        }
        Node::Assign { left, op, right } => {
            let target = match *ast.node(left) {
                Node::Member { obj, .. } => expr_exits(cx, obj, next),
                Node::Index { obj, index, .. } => {
                    expr_exits(cx, obj, next) || expr_exits(cx, index, next)
                }
                _ => false,
            };
            target
                || (!matches!(
                    op,
                    AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign
                ) && expr_exits(cx, right, next))
        }
        Node::Conditional { test, cons, alt } => {
            expr_exits(cx, test, next)
                || match literal_truthiness(ast, test) {
                    Some(true) => expr_exits(cx, cons, next),
                    Some(false) => expr_exits(cx, alt, next),
                    None => expr_exits(cx, cons, next) && expr_exits(cx, alt, next),
                }
        }
        _ => false,
    }
}

/// `id` can't throw: a literal, `this`, a declared local name, or `!`/`typeof`/`void`/`-`
/// of one, and `?:` of those.
fn expr_cannot_throw(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 100 {
        return false;
    }
    let ast = cx.ast();
    let id = unwrap_ts_and_parens(ast, id);
    match *ast.node(id) {
        Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::StringLit { .. }
        | Node::BigIntLit { .. }
        | Node::RegexLit { .. }
        | Node::This {} => true,
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        Node::Ident { .. } => {
            let sem = cx.semantic();
            sem.reference_at(id)
                .and_then(|r| sem.reference(r).symbol())
                .is_some_and(|s| {
                    sem.symbol(s).decls.first().is_some_and(|d| {
                        matches!(
                            d.kind,
                            crate::semantic::DeclKind::Param
                                | crate::semantic::DeclKind::Var
                                | crate::semantic::DeclKind::Function
                        )
                    })
                })
        }
        Node::Unary {
            op: UnaryOp::Not | UnaryOp::TypeOf | UnaryOp::Void,
            arg,
        } => expr_cannot_throw(cx, arg, depth + 1),
        Node::Conditional { test, cons, alt } => {
            expr_cannot_throw(cx, test, depth + 1)
                && expr_cannot_throw(cx, cons, depth + 1)
                && expr_cannot_throw(cx, alt, depth + 1)
        }
        _ => false,
    }
}

/// `id` reaches a global `process.exit(..)` without anything before it that could throw,
/// so a `catch` around it never runs.
fn exits_without_throwing(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 100 {
        return false;
    }
    let ast = cx.ast();
    let next = depth + 1;
    let expr = |e: NodeId| -> bool {
        let e = unwrap_ts_and_parens(ast, e);
        match *ast.node(e) {
            Node::Call { args, .. } if is_process_exit_call(cx, e) => ast
                .list(args)
                .iter()
                .all(|&a| expr_cannot_throw(cx, a, next)),
            Node::Conditional { test, cons, alt } => {
                expr_cannot_throw(cx, test, next)
                    && exits_without_throwing_expr(cx, cons, next)
                    && exits_without_throwing_expr(cx, alt, next)
            }
            _ => false,
        }
    };
    match *ast.node(id) {
        Node::ExprStmt { expr: e } => expr(e),
        Node::Block { stmts } => {
            for &s in ast.list(stmts) {
                if exits_without_throwing(cx, s, next) {
                    return true;
                }
                if !matches!(ast.node(s), Node::Empty {}) {
                    return false;
                }
            }
            false
        }
        Node::If {
            test,
            cons,
            alt: Some(alt),
        } => {
            expr_cannot_throw(cx, test, next)
                && exits_without_throwing(cx, cons, next)
                && exits_without_throwing(cx, alt, next)
        }
        _ => false,
    }
}

fn exits_without_throwing_expr(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    let ast = cx.ast();
    let id = unwrap_ts_and_parens(ast, id);
    match *ast.node(id) {
        Node::Call { args, .. } if is_process_exit_call(cx, id) => ast
            .list(args)
            .iter()
            .all(|&a| expr_cannot_throw(cx, a, depth + 1)),
        Node::Conditional { test, cons, alt } if depth < 100 => {
            expr_cannot_throw(cx, test, depth + 1)
                && exits_without_throwing_expr(cx, cons, depth + 1)
                && exits_without_throwing_expr(cx, alt, depth + 1)
        }
        _ => false,
    }
}

/// `branchAlwaysExits` (code-path analysis, structurally): control never falls out of
/// `stmt` to what follows it. `return`, `throw`, `break`, `continue` and an always-evaluated
/// global `process.exit(..)` exit; a block exits when one of its statements does; an `if`
/// when both branches do (a literal test picks one); `try` when its `finally` does, or its
/// block does and the handler does too (or can't run: the block reaches `process.exit`
/// without anything that could throw); a `switch` with a `default` when every case falls
/// into an exit and none breaks out; a loop with a literal truthy test (or a `do` whose
/// body exits, or whose test exits) with no `break` to it; a labeled statement whose body
/// exits without a `break` to the label. Dead code (after an exit, or under a literal test)
/// is ignored.
pub(crate) fn always_exits(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    exits(cx, stmt, 0)
}

fn exits(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> bool {
    if depth > 500 {
        return false;
    }
    let ast = cx.ast();
    let next = depth + 1;
    match *ast.node(id) {
        Node::Return { .. } | Node::Throw { .. } | Node::Break { .. } | Node::Continue { .. } => {
            true
        }
        Node::ExprStmt { expr } => expr_exits(cx, expr, next),
        Node::VarDecl { decls, .. } => ast.list(decls).iter().any(|&d| match *ast.node(d) {
            Node::VarDeclarator { init: Some(i), .. } => expr_exits(cx, i, next),
            _ => false,
        }),
        Node::Block { stmts } => ast.list(stmts).iter().any(|&s| exits(cx, s, next)),
        Node::If { test, cons, alt } => {
            expr_exits(cx, test, next)
                || match literal_truthiness(ast, test) {
                    Some(true) => exits(cx, cons, next),
                    Some(false) => alt.is_some_and(|a| exits(cx, a, next)),
                    None => alt.is_some_and(|a| exits(cx, cons, next) && exits(cx, a, next)),
                }
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            finalizer.is_some_and(|f| exits(cx, f, next))
                || (exits(cx, block, next)
                    && handler.is_none_or(|h| match *ast.node(h) {
                        Node::CatchClause { body, .. } => {
                            exits(cx, body, next) || exits_without_throwing(cx, block, next)
                        }
                        _ => false,
                    }))
        }
        Node::Switch { disc, cases } => {
            if expr_exits(cx, disc, next) {
                return true;
            }
            let cases = ast.list(cases);
            let mut has_default = false;
            let mut exit_test = None;
            let mut parts = Vec::new();
            for (i, &c) in cases.iter().enumerate() {
                if let Node::SwitchCase { test, body } = *ast.node(c) {
                    has_default |= test.is_none();
                    if exit_test.is_none() && test.is_some_and(|t| expr_exits(cx, t, next)) {
                        exit_test = Some(i);
                    }
                    parts.push(body);
                }
            }
            let mut case_exits = vec![false; parts.len()];
            let mut falls_into_exit = false;
            let breaks = JumpTarget {
                label: None,
                unlabeled: true,
                with_continue: false,
            };
            for (i, &body) in parts.iter().enumerate().rev() {
                let stmts = ast.list(body);
                let mut broke = false;
                for &s in stmts {
                    if jumps_to(cx, s, breaks, false, false, next) {
                        broke = true;
                        break;
                    }
                    if exits(cx, s, next) {
                        break;
                    }
                }
                if broke {
                    falls_into_exit = false;
                } else {
                    falls_into_exit = falls_into_exit || stmts.iter().any(|&s| exits(cx, s, next));
                }
                case_exits[i] = falls_into_exit;
            }
            match exit_test {
                Some(first) => case_exits[..first].iter().all(|&e| e),
                None => has_default && case_exits.iter().all(|&e| e),
            }
        }
        Node::While { test, body } => {
            expr_exits(cx, test, next)
                || (literal_truthiness(ast, test) == Some(true)
                    && !targets(cx, id, body, false, next))
        }
        Node::For {
            init, test, body, ..
        } => {
            init.is_some_and(|i| {
                if ast.node(i).is_statement() {
                    exits(cx, i, next)
                } else {
                    expr_exits(cx, i, next)
                }
            }) || test.is_some_and(|t| expr_exits(cx, t, next))
                || (test.is_none_or(|t| literal_truthiness(ast, t) == Some(true))
                    && !targets(cx, id, body, false, next))
        }
        Node::DoWhile { body, test } => {
            if targets(cx, id, body, false, next) {
                return false;
            }
            if literal_truthiness(ast, test) == Some(true) || expr_exits(cx, test, next) {
                return true;
            }
            exits(cx, body, next) && !targets(cx, id, body, true, next)
        }
        Node::ForIn { right, .. } | Node::ForOf { right, .. } => expr_exits(cx, right, next),
        Node::Labeled { label, body } => {
            let t = JumpTarget {
                label: ident_name(ast, label),
                unlabeled: false,
                with_continue: false,
            };
            exits(cx, body, next) && !jumps_to(cx, body, t, false, false, next)
        }
        Node::With { obj, body } => expr_exits(cx, obj, next) || exits(cx, body, next),
        _ => false,
    }
}
