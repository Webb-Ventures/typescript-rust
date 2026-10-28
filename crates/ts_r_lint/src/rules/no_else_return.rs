//! `no-else-return`: disallow an `else` block after an `if` whose block always returns.
//!
//! Only `if` statements directly in a statement list are checked. By default
//! (`allowElseIf: true`) an `if`/`else if` chain reports its final `else` when every branch
//! before it always returns; with `allowElseIf: false` any `else` (an `else if` too) after a
//! returning consequent reports. A branch "always returns" when it is a `return`, or a block
//! with a `return` (or an `if`/`else` whose two branches each end in one) among its
//! statements.
//!
//! The fix unwraps the `else` into the enclosing statement list, unless that could change
//! meaning: a function declaration as the `else`, names the `else` block declares that would
//! collide with (or capture references in) the surrounding scope, or a statement boundary
//! that automatic semicolon insertion could merge. The fix covers the enclosing function
//! (or the whole file), so only one applies per function per pass.

use super::prelude::*;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-else-return",
        "Disallow `else` blocks after `return` statements in `if` statements",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoElseReturn>,
};

const MESSAGE: &str = "Unnecessary 'else' after 'return'.";

#[derive(Debug)]
struct NoElseReturn {
    allow_else_if: bool,
}

impl Default for NoElseReturn {
    fn default() -> Self {
        NoElseReturn {
            allow_else_if: true,
        }
    }
}

impl FromOptions for NoElseReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let empty = serde_json::Map::new();
        let o = opts.first().and_then(Value::as_object).unwrap_or(&empty);
        Ok(NoElseReturn {
            allow_else_if: bool_field(o, "allowElseIf", true),
        })
    }
}

fn is_return(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Return { .. })
}

/// The branch is a `return`, or a block whose last statement is one.
fn ends_in_return(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Block { stmts } => ast.list(stmts).last().is_some_and(|&s| is_return(ast, s)),
        _ => is_return(ast, id),
    }
}

fn return_or_if(ast: &Ast, id: NodeId) -> bool {
    if is_return(ast, id) {
        return true;
    }
    matches!(*ast.node(id), Node::If { cons, alt: Some(alt), .. }
        if ends_in_return(ast, alt) && ends_in_return(ast, cons))
}

fn always_returns(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Block { stmts } => ast.list(stmts).iter().any(|&s| return_or_if(ast, s)),
        _ => return_or_if(ast, id),
    }
}

/// Starts with a character that could continue the previous statement.
fn continues(c: Option<char>) -> bool {
    matches!(c, Some('(' | '[' | '/' | '+' | '`' | '-'))
}

impl NoElseReturn {
    fn report(&self, cx: &mut Ctx<'_, '_>, if_node: NodeId, alt: NodeId) {
        let fix = self.fix(cx, if_node, alt);
        cx.report(cx.span(alt), MESSAGE)
            .id("unexpected")
            .fix_opt(fix);
    }

    fn fix(&self, cx: &Ctx<'_, '_>, if_node: NodeId, alt: NodeId) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let Node::If { cons, .. } = *ast.node(if_node) else {
            return None;
        };
        if !self.safe_names(cx, if_node, alt) {
            return None;
        }
        let alt_span = cx.span(alt);
        // The `else` keyword before the alternate.
        let else_end = skip_trivia_backward(ast, src, alt_span.start as usize);
        let else_start = else_end.checked_sub(4)?;
        if src.get(else_start..else_end) != Some("else") {
            return None;
        }
        let before_else = skip_trivia_backward(ast, src, else_start);
        let last_if_char = src[..before_else].chars().next_back();
        let is_block = matches!(ast.node(alt), Node::Block { .. });
        let body_start = if is_block {
            skip_trivia_forward(src, alt_span.start as usize + 1)
        } else {
            alt_span.start as usize
        };
        let cons_unsafe =
            !matches!(ast.node(cons), Node::Block { .. }) && last_if_char != Some(';');
        if cons_unsafe && continues(src[body_start..].chars().next()) {
            return None;
        }
        // The token before the alternate's last token.
        let end = alt_span.end as usize;
        let last_char = src[..end].chars().next_back();
        let last_len = last_char.map_or(0, char::len_utf8);
        let before_last = if is_block || last_char == Some(';') {
            skip_trivia_backward(ast, src, end - last_len)
        } else {
            // A statement not ending in `;` never has one as its second-to-last token.
            end
        };
        let semi = (is_block || last_char == Some(';')) && src[..before_last].ends_with(';');
        if !semi {
            let next = skip_trivia_forward(src, end);
            if let Some(c) = src[next..].chars().next() {
                let same_line =
                    !has_newline(src, before_last.saturating_sub(1) as u32, next as u32);
                if continues(Some(c)) || (same_line && c != '}') {
                    return None;
                }
            }
        }
        let text = alt_span.text(src);
        let fixed = if is_block {
            &text[1..text.len().saturating_sub(1).max(1)]
        } else {
            text
        };
        // Keep the enclosing function (or the whole file) as the fix's range.
        let parents = cx.file.parents;
        let outer = parents
            .ancestors(if_node)
            .find(|&a| is_function(ast.node(a).kind()))
            .map_or(Span::new(0, src.len() as u32), |f| cx.span(f));
        Some(
            Fix::replace(Span::new(else_start as u32, alt_span.end), fixed)
                .and(Span::at(outer.start), "")
                .and(Span::at(outer.end), ""),
        )
    }

    /// Whether the names the `else` block declares can move into the enclosing scope.
    fn safe_names(&self, cx: &Ctx<'_, '_>, if_node: NodeId, alt: NodeId) -> bool {
        let ast = cx.ast();
        match ast.node(alt) {
            Node::FunctionDecl { .. } => return false,
            Node::Block { .. } => {}
            _ => return true,
        }
        let sem = cx.semantic();
        let Some(block_scope) = sem.scope_of_node(alt) else {
            return true;
        };
        let names: Vec<&str> = sem
            .scope(block_scope)
            .symbols
            .iter()
            .map(|&s| sem.name(s))
            .collect();
        if names.is_empty() {
            return true;
        }
        let scope = sem.enclosing_scope(if_node);
        let var_scope = {
            let mut s = scope;
            for _ in 0..10_000 {
                let sc = sem.scope(s);
                if sc.kind.is_var_scope() {
                    break;
                }
                match sc.parent {
                    Some(p) => s = p,
                    None => break,
                }
            }
            s
        };
        let has = |s: ScopeId, n: &str| sem.scope(s).symbols.iter().any(|&x| sem.name(x) == n);
        // Declared in the scope (or in a catch clause right around it).
        if names.iter().any(|n| has(scope, n)) {
            return false;
        }
        if scope != var_scope
            && let Some(p) = sem.scope(scope).parent
            && sem.scope(p).kind == ScopeKind::Catch
            && names.iter().any(|n| has(p, n))
        {
            return false;
        }
        // A reference inside the scope that resolves outside it (or to a global).
        let inside = |mut s: ScopeId| {
            for _ in 0..10_000 {
                if s == scope {
                    return true;
                }
                match sem.scope(s).parent {
                    Some(p) => s = p,
                    None => return false,
                }
            }
            false
        };
        // An implicit variable of the scope itself (a function's `arguments`) counts as
        // used from outside, like ESLint's referenced variables without definitions.
        let resolves_inside = |r: &crate::semantic::Reference| match r.resolution {
            Resolution::Symbol(sym) => {
                let s = sem.symbol(sym);
                inside(s.scope) && !(s.scope == scope && s.decls.is_empty())
            }
            _ => false,
        };
        let refs = sem.references();
        if refs
            .iter()
            .any(|r| names.contains(&ast.atom(r.name)) && inside(r.scope) && !resolves_inside(r))
        {
            return false;
        }
        // A `var` of the function scope declared inside this (block) scope.
        if scope != var_scope {
            let range = cx.span(sem.scope(scope).node);
            let hoisted = sem.scope(var_scope).symbols.iter().any(|&s| {
                names.contains(&sem.name(s))
                    && sem.symbol(s).decls.iter().any(|d| {
                        let r = cx.span(d.node);
                        range.start <= r.start && r.end <= range.end
                    })
            });
            if hoisted {
                return false;
            }
        }
        true
    }
}

/// `if` statements directly in a statement list.
fn in_statement_list(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    cx.parent(id).is_some_and(|p| {
        matches!(
            cx.node(p),
            Node::Program { .. }
                | Node::Block { .. }
                | Node::StaticBlock { .. }
                | Node::SwitchCase { .. }
        )
    })
}

impl Rule for NoElseReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn exit_kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::If]
    }
    fn exit(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !in_statement_list(cx, node) {
            return;
        }
        let ast = cx.ast();
        if self.allow_else_if {
            let mut cur = node;
            let mut last_alt = None;
            for _ in 0..100_000 {
                let Node::If { cons, alt, .. } = *ast.node(cur) else {
                    break;
                };
                let Some(alt) = alt else { return };
                if !always_returns(ast, cons) {
                    return;
                }
                last_alt = Some((cur, alt));
                cur = alt;
            }
            if let Some((owner, alt)) = last_alt {
                self.report(cx, owner, alt);
            }
        } else if let Node::If {
            cons,
            alt: Some(alt),
            ..
        } = *ast.node(node)
            && always_returns(ast, cons)
        {
            self.report(cx, node, alt);
        }
    }
}
