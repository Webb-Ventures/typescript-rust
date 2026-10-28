//! `curly`: braces around the bodies of `if`, `else`, `for`, `while` and `do`
//! (`all`, `multi`, `multi-line`, `multi-or-nest`, plus `consistent` for `if` chains).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "curly",
        "Enforce consistent brace style for all control statements",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<Curly>,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    #[default]
    All,
    Multi,
    MultiLine,
    MultiOrNest,
}

#[derive(Debug, Default)]
struct Curly {
    mode: Mode,
    consistent: bool,
}

impl FromOptions for Curly {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mode = match opts.first().and_then(Value::as_str) {
            Some("multi") => Mode::Multi,
            Some("multi-line") => Mode::MultiLine,
            Some("multi-or-nest") => Mode::MultiOrNest,
            _ => Mode::All,
        };
        Ok(Curly {
            mode,
            consistent: opts.get(1).and_then(Value::as_str) == Some("consistent"),
        })
    }
}

/// One body to check: the statement it belongs to, the body, and what is expected.
struct Check {
    body: NodeId,
    name: &'static str,
    condition: bool,
    actual: bool,
    expected: Option<bool>,
}

/// The end of the last token of `id`, not counting a trailing `;`.
fn last_token_end(cx: &Ctx<'_, '_>, id: NodeId) -> u32 {
    let src = cx.src();
    let end = cx.span(id).end;
    if src[..end as usize].ends_with(';') {
        skip_trivia_backward(cx.ast(), src, end as usize - 1) as u32
    } else {
        end
    }
}

fn is_lexical_declaration(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::VarDecl { kind, .. } => kind != VarKind::Var,
        Node::FunctionDecl { .. } | Node::ClassDecl { .. } => true,
        _ => false,
    }
}

/// An `if` without `else` at the end of `id` (through loop and label bodies).
fn has_unsafe_if(ast: &ts_r_ast::Ast, mut id: NodeId) -> bool {
    for _ in 0..10_000 {
        id = match *ast.node(id) {
            Node::If { alt: None, .. } => return true,
            Node::If { alt: Some(a), .. } => a,
            Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::Labeled { body, .. }
            | Node::With { body, .. }
            | Node::While { body, .. } => body,
            _ => return false,
        };
    }
    false
}

fn starts_with_word(s: &str, word: &str) -> bool {
    s.strip_prefix(word)
        .is_some_and(|r| !r.starts_with(|c: char| c == '$' || ts_r_lexer::is_id_part(c)))
}

/// ESLint's `areBracesNecessary` for a one-statement block.
fn braces_necessary(cx: &Ctx<'_, '_>, block: NodeId, stmt: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    is_lexical_declaration(ast, stmt)
        || (has_unsafe_if(ast, stmt)
            && starts_with_word(
                &src[skip_trivia_forward(src, cx.span(block).end as usize)..],
                "else",
            ))
}

/// The innermost node of `id` that ends at `end`.
fn deepest_ending_at(ast: &ts_r_ast::Ast, mut id: NodeId, end: u32) -> NodeId {
    for _ in 0..10_000 {
        let mut next = None;
        ast.for_each_child(id, |c| {
            if ast.span(c).end == end {
                next = Some(c);
            }
        });
        match next {
            Some(n) => id = n,
            None => break,
        }
    }
    id
}

impl Curly {
    fn prepare(
        &self,
        cx: &Ctx<'_, '_>,
        body: NodeId,
        name: &'static str,
        condition: bool,
    ) -> Check {
        let ast = cx.ast();
        let src = cx.src();
        let block = match *ast.node(body) {
            Node::Block { stmts } => Some(ast.list(stmts)),
            _ => None,
        };
        let one_liner = |s: NodeId| {
            matches!(ast.node(s), Node::Empty {})
                || !has_newline(src, cx.span(s).start, last_token_end(cx, s))
        };
        let expected = match block {
            Some(stmts) if stmts.len() != 1 || braces_necessary(cx, body, stmts[0]) => Some(true),
            _ => match self.mode {
                Mode::Multi => Some(false),
                Mode::MultiLine => {
                    let before = skip_trivia_backward(ast, src, cx.span(body).start as usize);
                    has_newline(src, before as u32, last_token_end(cx, body)).then_some(true)
                }
                Mode::MultiOrNest => Some(match block {
                    Some(stmts) => {
                        let s = stmts[0];
                        let open = cx.span(body).start + 1;
                        !one_liner(s)
                            || !ast
                                .comments_in(Span::new(open, cx.span(s).start))
                                .is_empty()
                    }
                    None => !one_liner(body),
                }),
                Mode::All => Some(true),
            },
        };
        Check {
            body,
            name,
            condition,
            actual: block.is_some(),
            expected,
        }
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, c: &Check) {
        let Some(expected) = c.expected else { return };
        if expected == c.actual {
            return;
        }
        let src = cx.src();
        let span = cx.span(c.body);
        let name = c.name;
        if expected {
            let (msg, id) = if c.condition {
                (
                    format!("Expected {{ after '{name}' condition."),
                    "missingCurlyAfterCondition",
                )
            } else {
                (format!("Expected {{ after '{name}'."), "missingCurlyAfter")
            };
            let fix = Fix::replace(span, format!("{{{}}}", span.text(src)));
            cx.report(span, msg).id(id).fix(fix);
            return;
        }
        let (msg, id) = if c.condition {
            (
                format!("Unnecessary {{ after '{name}' condition."),
                "unexpectedCurlyAfterCondition",
            )
        } else {
            (
                format!("Unnecessary {{ after '{name}'."),
                "unexpectedCurlyAfter",
            )
        };
        let fix = self
            .removal(cx, c.body)
            .map(|text| Fix::replace(span, text));
        cx.report(span, msg).id(id).fix_opt(fix);
    }

    /// The block's body without its braces, or `None` when that would join statements.
    fn removal(&self, cx: &Ctx<'_, '_>, body: NodeId) -> Option<String> {
        let ast = cx.ast();
        let src = cx.src();
        let span = cx.span(body);
        let Node::Block { stmts } = *ast.node(body) else {
            return None;
        };
        let close = span.end as usize - 1;
        let last = skip_trivia_backward(ast, src, close);
        // Would the statement run into what follows (ASI)?
        if !src[..last].ends_with(';') {
            let after = skip_trivia_forward(src, span.end as usize);
            if after < src.len() {
                let stmt = ast.list(stmts).last().copied();
                let node = stmt.map(|s| deepest_ending_at(ast, s, last as u32));
                let nested_block = node.is_some_and(|n| {
                    matches!(ast.node(n), Node::Block { .. })
                        && !cx.parent(n).is_some_and(|p| {
                            matches!(ast.node(p), Node::FunctionExpr { .. } | Node::Arrow { .. })
                        })
                });
                if !nested_block
                    && (!has_newline(src, last as u32, after as u32)
                        || src[after..].starts_with(['(', '[', '/', '`', '+', '-'])
                        || src[..last].ends_with("++")
                        || src[..last].ends_with("--"))
                {
                    return None;
                }
            }
        }
        let inner = &src[span.start as usize + 1..close];
        let before = skip_trivia_backward(ast, src, span.start as usize);
        let first = skip_trivia_forward(src, span.start as usize + 1);
        let prev = src[..before].chars().next_back();
        let next = src[first..].chars().next();
        let join = before == span.start as usize
            && match (prev, next) {
                (Some(a), Some(b)) => {
                    (is_ident_char(a) && is_ident_char(b))
                        || (a == '+' && b == '+')
                        || (a == '-' && b == '-')
                }
                _ => false,
            };
        Some(if join {
            format!(" {inner}")
        } else {
            inner.to_string()
        })
    }
}

impl Rule for Curly {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::If,
            NodeKind::While,
            NodeKind::DoWhile,
            NodeKind::For,
            NodeKind::ForIn,
            NodeKind::ForOf,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let check = match *ast.node(node) {
            Node::If { .. } => {
                if let Some(p) = cx.parent(node)
                    && let Node::If { alt: Some(a), .. } = *ast.node(p)
                    && a == node
                {
                    return;
                }
                let mut checks = Vec::new();
                let mut cur = node;
                for _ in 0..100_000 {
                    let Node::If { cons, alt, .. } = *ast.node(cur) else {
                        break;
                    };
                    checks.push(self.prepare(cx, cons, "if", true));
                    match alt {
                        Some(a) if matches!(ast.node(a), Node::If { .. }) => cur = a,
                        Some(a) => {
                            checks.push(self.prepare(cx, a, "else", false));
                            break;
                        }
                        None => break,
                    }
                }
                if self.consistent {
                    let expected = checks.iter().any(|c| c.expected.unwrap_or(c.actual));
                    for c in &mut checks {
                        c.expected = Some(expected);
                    }
                }
                for c in &checks {
                    self.report(cx, c);
                }
                return;
            }
            Node::While { body, .. } => self.prepare(cx, body, "while", true),
            Node::DoWhile { body, .. } => self.prepare(cx, body, "do", false),
            Node::For { body, .. } => self.prepare(cx, body, "for", true),
            Node::ForIn { body, .. } => self.prepare(cx, body, "for-in", false),
            Node::ForOf { body, .. } => self.prepare(cx, body, "for-of", false),
            _ => return,
        };
        self.report(cx, &check);
    }
}
