//! `unicorn/no-typeof-undefined`: compare with `undefined` instead of
//! `typeof x === "undefined"`.
//!
//! Applies to `===`, `!==`, `==`, `!=` with `typeof x` on the left and a string (or
//! substitution-free template) equal to `undefined` on the right; reported at `typeof`.
//! When `x` (through parentheses and TS wrappers) is an identifier:
//! - a variable whose declarations are all `declare` variables or types is skipped;
//! - a global (built in, configured, `/* global */` or unresolved) is skipped unless
//!   `checkGlobalVariables`, and then the change is only a suggestion (it would throw for an
//!   undeclared name).
//!
//! The fix drops `typeof` and the spaces after it, writes `undefined`, and turns `==`/`!=`
//! into `===`/`!==`. It parenthesizes an operand that would then start a statement, arrow
//! body or `export default` with `{`, `function`, `class` or `let [`; wraps a `return`/`throw`
//! argument in parentheses when `typeof` and the next token are on different lines (else
//! the statement would end at the line break); is withheld (report only) for a `yield` in the
//! same situation; and adds a leading `;` when the new start would continue the previous
//! statement.

use super::prelude::*;
use super::ts_no_require_imports::plain_string;
use super::unicorn_no_instanceof_builtins::starts_continued_statement;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-typeof-undefined",
        "Disallow comparing `undefined` using `typeof`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<NoTypeofUndefined>,
};

#[derive(Debug, Default)]
struct NoTypeofUndefined {
    check_globals: bool,
}

impl FromOptions for NoTypeofUndefined {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoTypeofUndefined {
            check_globals: bool_field(obj_opt(opts, 0), "checkGlobalVariables", false),
        })
    }
}

const TYPEOF_LEN: u32 = "typeof".len() as u32;

impl Rule for NoTypeofUndefined {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Binary]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::Binary { left, op, right } = *cx.node(node) else {
            return;
        };
        if !matches!(
            op,
            BinaryOp::StrictEq | BinaryOp::StrictNotEq | BinaryOp::Eq | BinaryOp::NotEq
        ) {
            return;
        }
        let typeof_node = skip_parens(ast, left);
        let Node::Unary {
            op: UnaryOp::TypeOf,
            arg,
        } = *ast.node(typeof_node)
        else {
            return;
        };
        let right_inner = skip_parens(ast, right);
        if plain_string(cx, right_inner) != Some("undefined") {
            return;
        }
        let value = unwrap_ts(ast, arg);
        let mut global = false;
        if matches!(ast.node(value), Node::Ident { .. }) {
            let sem = cx.semantic();
            match sem.reference_at(value).map(|r| sem.reference(r).resolution) {
                Some(Resolution::Symbol(s)) => {
                    let sym = sem.symbol(s);
                    if sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                        global = true;
                    } else if is_ambient(ast, &sym.decls) {
                        return;
                    }
                }
                _ => global = true,
            }
        }
        if global && !self.check_globals {
            return;
        }

        let src = cx.src();
        let typeof_start = cx.span(typeof_node).start;
        let typeof_span = Span::new(typeof_start, typeof_start + TYPEOF_LEN);
        let second = skip_trivia_forward(src, typeof_span.end as usize) as u32;
        let same_line = !has_newline(src, typeof_span.end, second);
        let keyword_parent = keyword_argument_parent(cx, typeof_node);

        let fix =
            (same_line || !matches!(keyword_parent, Some(k) if is_yield(ast, k))).then(|| {
                self.fix(
                    cx,
                    node,
                    typeof_node,
                    arg,
                    right_inner,
                    op,
                    second,
                    same_line,
                    keyword_parent,
                )
            });

        let report = cx
            .report(
                typeof_span,
                "Compare with `undefined` directly instead of using `typeof`.",
            )
            .id("no-typeof-undefined/error");
        match fix {
            Some(fix) if global => {
                let operator = if matches!(op, BinaryOp::StrictNotEq | BinaryOp::NotEq) {
                    "!=="
                } else {
                    "==="
                };
                report.suggest(
                    "no-typeof-undefined/suggestion",
                    format!("Switch to `… {operator} undefined`."),
                    fix,
                );
            }
            Some(fix) => {
                report.fix(fix);
            }
            None => {}
        }
    }
}

impl NoTypeofUndefined {
    #[allow(clippy::too_many_arguments)]
    fn fix(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        typeof_node: NodeId,
        arg: NodeId,
        right: NodeId,
        op: BinaryOp,
        second: u32,
        same_line: bool,
        keyword_parent: Option<NodeId>,
    ) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let typeof_start = cx.span(typeof_node).start;
        let mut fix = Fix::replace(cx.span(right), "undefined");
        if matches!(op, BinaryOp::Eq | BinaryOp::NotEq) {
            let Node::Binary { left, .. } = *ast.node(node) else {
                return fix;
            };
            let at = skip_trivia_forward(src, cx.span(left).end as usize) as u32;
            fix = fix.and(Span::at(at + 2), "=");
        }
        // `typeof` and the spaces after it (not comments).
        let after = typeof_start + TYPEOF_LEN;
        let rest = &src[after as usize..];
        let spaces = (rest.len() - rest.trim_start().len()) as u32;
        fix = fix.and(Span::new(typeof_start, after + spaces), "");

        let parens = !matches!(ast.node(arg), Node::Paren { .. })
            && start_needs_parens(cx, arg, typeof_node);
        if parens {
            let a = cx.span(arg);
            fix = fix.and(Span::at(a.start), "(").and(Span::at(a.end), ")");
        }
        if let Some(k) = keyword_parent
            && !same_line
            && matches!(ast.node(k), Node::Return { .. } | Node::Throw { .. })
        {
            let s = cx.span(k);
            let keyword = if matches!(ast.node(k), Node::Return { .. }) {
                "return"
            } else {
                "throw"
            };
            fix = fix.and(Span::at(s.start + keyword.len() as u32), " (");
            let close = if s.text(src).ends_with(';') {
                s.end - 1
            } else {
                s.end
            };
            return fix.and(Span::at(close), ")");
        }
        let first = if parens {
            b'('
        } else {
            src.as_bytes().get(second as usize).copied().unwrap_or(b' ')
        };
        if b"[(/`+-*,.<".contains(&first) && starts_continued_statement(cx, node) {
            fix = fix.and(Span::at(cx.span(node).start), ";");
        }
        fix
    }
}

fn is_yield(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Yield { .. })
}

/// Strips parentheses and TS-only wrappers, including `f<T>` instantiations.
fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        id = skip_ts_wrappers(ast, id);
        match *ast.node(id) {
            Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// Every declaration is a type or a variable of a `declare` statement, and at least one is
/// the latter.
fn is_ambient(ast: &Ast, decls: &[crate::semantic::Decl]) -> bool {
    let declared = |d: &crate::semantic::Decl| {
        matches!(d.kind, DeclKind::Var | DeclKind::Let | DeclKind::Const)
            && matches!(*ast.node(d.parent), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::DECLARE))
    };
    decls.iter().any(declared)
        && decls
            .iter()
            .all(|d| declared(d) || matches!(d.kind, DeclKind::TypeAlias | DeclKind::Interface))
}

/// The `return`, `throw` or non-delegating `yield` whose unparenthesized argument starts
/// with `node`.
fn keyword_argument_parent(cx: &Ctx<'_, '_>, mut node: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    for _ in 0..10_000 {
        let parent = cx.parent(node)?;
        match *ast.node(parent) {
            Node::Paren { .. } => return None,
            Node::Return { arg: Some(a) } | Node::Throw { arg: a } if a == node => {
                return Some(parent);
            }
            Node::Yield {
                delegate: false,
                arg: Some(a),
            } if a == node => return Some(parent),
            _ => {}
        }
        if cx.span(parent).start != cx.span(node).start {
            return None;
        }
        node = parent;
    }
    None
}

/// `expr`, moved to where `typeof expr` was, would open a statement, arrow body or
/// `export default` with a token that reads as a block or declaration there.
fn start_needs_parens(cx: &Ctx<'_, '_>, expr: NodeId, replaced: NodeId) -> bool {
    let ast = cx.ast();
    let first = leftmost_expr(ast, expr);
    let regex = matches!(ast.node(first), Node::RegexLit { .. });
    let brace = matches!(ast.node(first), Node::ObjectLit { .. });
    let let_bracket = matches!(*ast.node(first), Node::Index { obj, .. } if ident_name(ast, obj) == Some("let") && !matches!(ast.node(obj), Node::Paren { .. }));
    let declaration_like = brace
        || let_bracket
        || matches!(
            ast.node(first),
            Node::FunctionExpr { .. } | Node::ClassExpr { .. }
        );
    if !regex && !declaration_like {
        return false;
    }
    let mut node = replaced;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *ast.node(parent) {
            Node::Paren { .. } => return false,
            Node::Arrow { body, .. } if body == node => return brace,
            Node::ExportDefaultExpr { expr } if expr == node => {
                return !regex && !brace && !let_bracket;
            }
            Node::For { init: Some(i), .. } if i == node => return let_bracket,
            Node::ExprStmt { .. } => return !regex,
            _ => {}
        }
        if cx.span(parent).start != cx.span(node).start {
            return false;
        }
        node = parent;
    }
    false
}
