//! Statements and declarations.

use ts_r_ast::{
    FnFlags, HeritageKind, ImportPhase, MethodKind, Modifiers, ModuleKind, Node, NodeId, NodeKind,
    NodeList, VarKind,
};

use crate::SpecifierKind;
use crate::expr::Prec;
use crate::printer::Printer;

/// Modifiers tsc 7 never prints in JavaScript output.
const TS_ONLY_MODIFIERS: Modifiers = Modifiers::DECLARE
    .union(Modifiers::PUBLIC)
    .union(Modifiers::PRIVATE)
    .union(Modifiers::PROTECTED)
    .union(Modifiers::ABSTRACT)
    .union(Modifiers::OVERRIDE)
    .union(Modifiers::READONLY)
    .union(Modifiers::CONST)
    .union(Modifiers::IN)
    .union(Modifiers::OUT);

impl Printer<'_> {
    /// A statement inside a statement list: leading comments, the statement, trailing comments.
    pub(crate) fn statement_in_list(&mut self, id: NodeId) {
        self.statement_in_list_before(id, u32::MAX, false);
    }

    /// [`Printer::statement_in_list`] where `next` is the start of the next original statement
    /// of the list (`u32::MAX` if none). `last` marks the last statement a transform wrote in
    /// place of an original one: the original's same-line comments trail it
    /// (`const a = require("a"); // c`, `})(E || (E = {})); // c`).
    fn statement_in_list_before(&mut self, id: NodeId, next: u32, last: bool) {
        if let Some(orig) = self.moved_of(id) {
            self.moved_statement(id, orig);
            return;
        }
        let span = self.span(id);
        let saved = self.stmt_end;
        let mut stmt_end = span.end;
        let mut trail = None;
        if !self.is_synthetic(id) {
            self.leading_comments(span.start);
        } else if self.tsc
            && (span.start > 0 || (self.code_at_zero() && !self.in_erased(0)))
            && span.start <= next
            && !self.unmapped_insert(id)
        {
            // A statement a transform wrote in place of an original one starts where it did
            // (its source-map hint): the comments in front of the original lead it. Statements
            // moved ahead of original ones (field initializers in a constructor) take none.
            self.leading_comments(span.start);
            // It ends where the original's code does: before the next original statement, or
            // the end of the enclosing one.
            let bound = if next != u32::MAX {
                next
            } else if saved != 0 {
                saved - 1
            } else {
                self.src.len() as u32
            };
            stmt_end = self.trivia_start(bound).max(span.start);
            // Not for a stand-in at the end of a nested list (moved there). Erased code
            // between it and the next statement is skipped: the original ends before it
            // (`exports.x = 1; // c` above `export interface I {}`).
            let mut end = stmt_end;
            while end > span.start
                && let Some(start) = self.erased_start(end - 1)
            {
                end = self.trivia_start(start);
            }
            if last && end > span.start && (next != u32::MAX || (saved == 0 && self.indent == 0)) {
                trail = Some(end);
            }
        } else {
            stmt_end = 0;
        }
        if !(self.tsc && self.after_inline_comment()) {
            self.newline();
        }
        self.stmt_end = stmt_end;
        self.statement(id);
        self.stmt_end = saved;
        if !self.is_synthetic(id) {
            self.trailing_comments(span.end);
        } else if let Some(at) = self.trail_from(id) {
            self.trailing_comments(at);
            // A comment after the element's `,` on its line goes (observed).
            let after = self.next_token_start(at);
            if self.src.as_bytes().get(after as usize) == Some(&b',') {
                self.drop_same_line_comments(after + 1);
            }
        } else if let Some(end) = trail {
            self.trailing_comments(end);
        }
        self.newline();
    }

    pub(crate) fn statement_list(&mut self, list: NodeList) {
        let items = self.ast.list(list).to_vec();
        self.statements(&items);
    }

    /// Prints `items` as a statement list.
    pub(crate) fn statements(&mut self, items: &[NodeId]) {
        // Index of the next original statement after `i` (found lazily, so linear).
        let mut next_orig = 0;
        for (i, &s) in items.iter().enumerate() {
            let (next, last) = if self.tsc && self.is_synthetic(s) {
                if next_orig <= i {
                    next_orig = i + 1;
                    while next_orig < items.len() && self.is_synthetic(items[next_orig]) {
                        next_orig += 1;
                    }
                }
                // The last stand-in for its original: the next statement is an original one,
                // or a stand-in for a later original.
                let start = self.span(s).start;
                let later = items
                    .get(i + 1)
                    .map(|&n| self.span(n).start)
                    .filter(|&n| n > start);
                let last = items
                    .get(i + 1)
                    .is_none_or(|&n| !self.is_synthetic(n) || later.is_some());
                let next = items
                    .get(next_orig)
                    .map_or(u32::MAX, |&n| self.span(n).start);
                // A stand-in for a later original bounds this one's original too.
                (later.map_or(next, |l| l.min(next)), last)
            } else {
                (u32::MAX, false)
            };
            self.statement_in_list_before(s, next, last);
        }
    }

    /// `{ stmts }` for a `Block`, `ModuleBlock` or static block body.
    pub(crate) fn block_of(&mut self, id: NodeId, list: NodeList) {
        let span = self.span(id);
        self.map_node(id);
        let is_body = std::mem::take(&mut self.body_block);
        // tsc maps a statement block's `{` (not a function body's) and every block's `}`.
        let orig = !self.is_synthetic(id);
        let open =
            (orig && !is_body && self.src.as_bytes().get(span.start as usize) == Some(&b'{'))
                .then_some(span.start);
        // A missing `}` (unterminated block) is mapped where the block ends, as tsc does. A
        // synthesized block maps the `}` of the block its range names.
        let span = if orig {
            span
        } else {
            self.map_range_of(id).unwrap_or(span)
        };
        // A block standing for no source block (`static { this.x = 1; }` for a field) maps
        // no `}`.
        let close = (!self.unmapped_insert(id) && (orig || span.end > span.start)).then(|| {
            if span.end > span.start + 1
                && self.src.as_bytes().get(span.end as usize - 1) == Some(&b'}')
            {
                span.end - 1
            } else {
                span.end
            }
        });
        self.map_token("{", open);
        if !self.is_synthetic(id) {
            // Comments before the brace belong to whatever precedes it.
            self.skip_comments_to(span.start + 1);
        }
        let items = self.ast.list(list);
        if self.tsc {
            // Function bodies written on one line stay on one line; statement blocks only
            // when empty (`{ }`).
            if (self.single_line_source(id) && (is_body || items.is_empty()))
                || self.hint_single_line(id)
            {
                // tsc keeps a block written on one line on one line: `{ return x; }`, `{ }`.
                if !is_body && !self.is_synthetic(id) {
                    // ... with the comments of a statement block (`{ /* c */ }`); a function
                    // body's go.
                    self.trailing_comments(span.start + 1);
                }
                for &s in items {
                    self.space();
                    self.statement(s);
                    if !self.is_synthetic(s) {
                        self.trailing_comments(self.span(s).end);
                    }
                }
                self.space();
                self.map_token("}", close);
                if !self.is_synthetic(id) {
                    self.skip_comments_to(span.end);
                }
                return;
            }
            if items.is_empty() {
                // `{` and `}` on their own lines, as written. An empty function body loses
                // the comments on the `{` line (`function* g() { // c`).
                if is_body && !self.is_synthetic(id) {
                    self.drop_same_line_comments(span.start + 1);
                } else if !self.is_synthetic(id) {
                    // A statement block keeps them there (`if (x) { // c\n}`).
                    self.trailing_comments(span.start + 1);
                }
                if !self.is_synthetic(id) && self.has_comments_in(span) {
                    self.indent();
                    self.leading_comments(span.end.saturating_sub(1));
                    self.dedent();
                }
                // A block comment the `}` follows on its line keeps it there (`/* c */ }`).
                if !self.after_inline_comment() {
                    self.newline();
                }
                self.map_token("}", close);
                if !self.is_synthetic(id) {
                    self.skip_comments_to(span.end);
                }
                return;
            }
        }
        if items.is_empty() {
            // Dangling comments inside an empty block.
            if !self.is_synthetic(id) && self.has_comments_in(span) {
                self.indent();
                self.leading_comments(span.end.saturating_sub(1));
                self.dedent();
                self.newline();
            }
            self.map_token("}", close);
            return;
        }
        if self.tsc && !self.is_synthetic(id) {
            // A comment after the `{` on its line stays there in a statement block and goes
            // with a function body's.
            if is_body {
                self.drop_same_line_comments(span.start + 1);
            } else {
                self.trailing_comments(span.start + 1);
            }
        }
        self.indent();
        self.newline();
        self.statement_list(list);
        if !self.is_synthetic(id) {
            self.leading_comments(span.end.saturating_sub(1));
        }
        self.dedent();
        if !(self.tsc && self.after_inline_comment()) {
            self.newline();
        }
        self.map_token("}", close);
        if !self.is_synthetic(id) {
            self.skip_comments_to(span.end);
        }
    }

    /// An original node whose source text has no line break (tsc keeps it on one line).
    pub(crate) fn single_line_source(&self, id: NodeId) -> bool {
        if self.is_synthetic(id) || self.hint_multi_line(id) {
            return false;
        }
        // A transform that added statements (parameter properties, moved fields) makes the
        // body multi-line, as in tsc.
        if let Node::Block { stmts } | Node::ModuleBlock { stmts } = self.node(id)
            && self.ast.list(stmts).iter().any(|&s| self.is_synthetic(s))
        {
            return false;
        }
        let span = self.span(id);
        !self.has_newline_between(span.start, span.end)
    }

    fn has_comments_in(&self, span: ts_r_span::Span) -> bool {
        !self.ast.comments_in(span).is_empty()
    }

    /// The body of a function, method, accessor or arrow (tsc keeps a one-line body on one
    /// line).
    pub(crate) fn function_body(&mut self, id: NodeId) {
        self.body_block = self.kind(id) == NodeKind::Block;
        self.block(id);
        self.body_block = false;
    }

    pub(crate) fn block(&mut self, id: NodeId) {
        match self.node(id) {
            Node::Block { stmts } | Node::ModuleBlock { stmts } => self.block_of(id, stmts),
            _ => self.statement(id),
        }
    }

    /// Body of `if`/`for`/`while`/...: a block on the same line, anything else indented.
    fn embedded(&mut self, id: NodeId) {
        if self.kind(id) == NodeKind::Block {
            self.space();
            self.block(id);
        } else if self.hint_single_line(id) {
            self.space();
            self.statement(id);
        } else if self.kind(id) == NodeKind::Empty && !self.tsc {
            self.tok(";");
        } else {
            self.indent();
            self.newline();
            if self.compact {
                self.hard_space_if_word();
            }
            // Comments between the head and the statement lead it (`if (x)\n  // c\n  return;`).
            if self.tsc && !self.is_synthetic(id) {
                self.expr_leading(self.span(id).start);
            }
            self.statement(id);
            self.dedent();
        }
    }

    /// [`Layout::Tsc`]: the comments after the source token `tok` (the first one between
    /// `from` and `to`) on its line trail it, as tsc writes a statement's keywords and
    /// parentheses (`if /* c */ ( /* d */x) /* e */ {`).
    fn token_trailing(&mut self, tok: &str, from: u32, to: u32) {
        if !self.tsc || from >= to {
            return;
        }
        if let Some(p) = self.token_between(from, to, tok) {
            self.expr_trailing(p + tok.len() as u32);
        }
    }

    /// [`Layout::Tsc`]: `kw (` of statement `id` whose head starts at `head` (the first node
    /// inside the parentheses, or what follows them), with the comments after each token.
    fn keyword_paren(&mut self, id: NodeId, kw: &str, head: Option<NodeId>) {
        self.tok(kw);
        let orig = self.tsc && !self.is_synthetic(id) && head.is_none_or(|h| !self.is_synthetic(h));
        let start = self.span(id).start;
        let to = head.map_or(start, |h| self.span(h).start);
        if orig {
            self.token_trailing(kw, start, to);
        }
        self.space();
        self.tok("(");
        if orig {
            self.token_trailing("(", start + kw.len() as u32, to);
        }
    }

    /// [`Layout::Tsc`]: `)` after the original node ending at `from`, before `to`, with the
    /// comments after it on its line (`) /* c */ {`).
    fn close_paren(&mut self, id: NodeId, from: Option<NodeId>, to: NodeId) {
        self.tok(")");
        if self.tsc
            && !self.is_synthetic(id)
            && !self.is_synthetic(to)
            && from.is_none_or(|f| !self.is_synthetic(f))
        {
            let from = from.map_or(self.span(id).start, |f| self.span(f).end);
            self.token_trailing(")", from, self.span(to).start);
        }
    }

    /// Before `catch` / `finally`: tsc puts them on a new line.
    fn space_or_newline(&mut self) {
        if self.tsc {
            self.newline();
        } else {
            self.space();
        }
    }

    fn hard_space_if_word(&mut self) {
        // In compact layout `if(a)return` is fine; spacing is handled by `tok`.
    }

    pub(crate) fn statement(&mut self, id: NodeId) {
        self.map_start(id);
        self.mod_src = None;
        self.statement_inner(id);
        self.map_end(id);
    }

    fn statement_inner(&mut self, id: NodeId) {
        let n = self.node(id);
        self.map_node(id);
        match n {
            Node::Directive { .. } => {
                let raw = self.raw(id).unwrap_or("").trim_end();
                let raw = raw.strip_suffix(';').unwrap_or(raw).trim_end();
                if raw.is_empty() {
                    if let Node::Directive { value } = n {
                        let v = self.ast.atom(value).to_owned();
                        self.string_value(&v);
                    }
                } else {
                    self.tok(raw);
                }
                self.semi();
            }
            Node::Block { stmts } => self.block_of(id, stmts),
            // Never dropped: `a: ;` and `if (a) ;` need it.
            Node::Empty {} => self.tok(";"),
            Node::ExprStmt { expr } => {
                if self.starts_ambiguously(expr, true) {
                    self.tok("(");
                    self.expr(expr, Prec::Lowest);
                    self.tok(")");
                } else {
                    self.expr(expr, Prec::Lowest);
                }
                self.semi();
            }
            Node::If { test, cons, alt } => {
                self.keyword_paren(id, "if", Some(test));
                self.expr(test, Prec::Lowest);
                // Comments before the `)` stay there (`a // ||\n    // b\n)`).
                if self.tsc
                    && !self.is_synthetic(test)
                    && !self.is_synthetic(cons)
                    && let Some(p) =
                        self.token_between(self.span(test).end, self.span(cons).start, ")")
                {
                    self.expr_leading(p);
                }
                self.close_paren(id, Some(test), cons);
                // `if (a) if (b) x; else y` must keep the else on the inner if.
                let wrap = alt.is_some() && self.ends_with_dangling_if(cons);
                if wrap {
                    self.space();
                    self.tok("{");
                    self.indent();
                    self.newline();
                    self.statement(cons);
                    self.dedent();
                    self.newline();
                    self.tok("}");
                } else {
                    self.embedded(cons);
                }
                if let Some(alt) = alt {
                    let mut else_at = None;
                    if self.tsc {
                        // tsc: `}` and `else` on separate lines, the comments before `else`
                        // in between (those on the `}` line trail it).
                        if !self.is_synthetic(id) && !self.is_synthetic(cons) {
                            self.expr_trailing(self.span(cons).end);
                        }
                        self.newline();
                        if !self.is_synthetic(id)
                            && !self.is_synthetic(cons)
                            && !self.is_synthetic(alt)
                            && let Some(p) = self.token_between(
                                self.span(cons).end,
                                self.span(alt).start,
                                "else",
                            )
                        {
                            self.expr_leading(p);
                            else_at = Some(p);
                        }
                    } else if self.kind(cons) == NodeKind::Block || wrap {
                        self.space();
                    } else {
                        self.newline();
                    }
                    self.tok("else");
                    if let Some(p) = else_at {
                        self.expr_trailing(p + 4);
                    }
                    if self.kind(alt) == NodeKind::If {
                        self.hard_space();
                        self.statement(alt);
                    } else {
                        self.embedded(alt);
                    }
                }
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                let head = init.or(test).or(update).unwrap_or(body);
                self.keyword_paren(id, "for", Some(head));
                if let Some(init) = init {
                    let saved = self.no_in;
                    self.no_in = true;
                    if self.kind(init) == NodeKind::VarDecl {
                        self.var_decl(init, false);
                    } else {
                        let paren = self.starts_with_let(init);
                        if paren {
                            self.tok("(");
                        }
                        self.expr(init, Prec::Lowest);
                        if paren {
                            self.tok(")");
                        }
                    }
                    self.no_in = saved;
                }
                self.tok(";");
                if let Some(test) = test {
                    self.space();
                    self.expr(test, Prec::Lowest);
                }
                self.tok(";");
                if let Some(update) = update {
                    self.space();
                    self.expr(update, Prec::Lowest);
                }
                self.close_paren(id, update.or(test).or(init), body);
                self.embedded(body);
            }
            Node::ForIn { left, right, body } => {
                self.keyword_paren(id, "for", Some(left));
                self.for_head_left(left, false, false);
                // An empty list (`for (let in x)`, error recovery) keeps the space after its
                // keyword: tsc writes `let  in x`.
                if matches!(self.node(left), Node::VarDecl { decls, .. } if decls.is_empty()) {
                    self.space();
                }
                self.hard_space();
                self.tok("in");
                self.hard_space();
                self.expr(right, Prec::Lowest);
                self.close_paren(id, Some(right), body);
                self.embedded(body);
            }
            Node::ForOf {
                is_await,
                left,
                right,
                body,
            } => {
                if is_await {
                    self.tok("for");
                    self.hard_space();
                    self.tok("await");
                    self.space();
                    self.tok("(");
                } else {
                    self.keyword_paren(id, "for", Some(left));
                }
                self.for_head_left(left, true, is_await);
                self.hard_space();
                self.tok("of");
                self.hard_space();
                self.expr(right, Prec::Assign);
                self.close_paren(id, Some(right), body);
                self.embedded(body);
            }
            Node::While { test, body } => {
                self.keyword_paren(id, "while", Some(test));
                self.expr(test, Prec::Lowest);
                self.close_paren(id, Some(test), body);
                self.embedded(body);
            }
            Node::DoWhile { body, test } => {
                self.tok("do");
                let orig = self.tsc && !self.is_synthetic(id) && !self.is_synthetic(body);
                if orig && self.kind(body) == NodeKind::Block {
                    self.token_trailing("do", self.span(id).start, self.span(body).start);
                }
                if self.tsc && self.kind(body) != NodeKind::Block {
                    // tsc: `do\n    x();\nwhile (y);`
                    self.indent();
                    self.newline();
                    self.statement(body);
                    self.dedent();
                    self.newline();
                } else if self.kind(body) == NodeKind::Block {
                    self.space();
                    self.block(body);
                    if orig {
                        self.expr_trailing(self.span(body).end);
                    }
                    self.space();
                } else {
                    self.hard_space();
                    self.indent();
                    self.newline();
                    self.statement(body);
                    self.dedent();
                    self.newline();
                }
                self.tok("while");
                if orig && !self.is_synthetic(test) {
                    let (from, to) = (self.span(body).end, self.span(test).start);
                    self.token_trailing("while", from, to);
                    self.space();
                    self.tok("(");
                    if let Some(p) = self.token_between(from, to, "while") {
                        self.token_trailing("(", p + 5, to);
                    }
                } else {
                    self.space();
                    self.tok("(");
                }
                self.expr(test, Prec::Lowest);
                self.tok(")");
                self.semi();
            }
            Node::Return { arg } => {
                self.tok("return");
                if let Some(arg) = arg {
                    self.hard_space();
                    self.restricted_expr(arg);
                }
                self.semi();
            }
            Node::Throw { arg } => {
                self.tok("throw");
                self.hard_space();
                self.restricted_expr(arg);
                self.semi();
            }
            Node::Break { label } | Node::Continue { label } => {
                self.tok(if matches!(n, Node::Break { .. }) {
                    "break"
                } else {
                    "continue"
                });
                if let Some(label) = label {
                    let orig = self.tsc && !self.is_synthetic(id) && !self.is_synthetic(label);
                    if orig {
                        let kw = if matches!(n, Node::Break { .. }) {
                            "break"
                        } else {
                            "continue"
                        };
                        self.token_trailing(kw, self.span(id).start, self.span(label).start);
                    }
                    self.hard_space();
                    self.ident(label);
                    if orig {
                        self.expr_trailing(self.span(label).end);
                    }
                }
                self.semi();
            }
            Node::Labeled { label, body } => {
                self.ident(label);
                let orig = self.tsc && !self.is_synthetic(label) && !self.is_synthetic(body);
                if orig {
                    self.expr_trailing(self.span(label).end);
                }
                self.tok(":");
                if orig {
                    self.token_trailing(":", self.span(label).end, self.span(body).start);
                }
                if self.kind(body) == NodeKind::Block {
                    self.space();
                    self.block(body);
                } else {
                    self.space();
                    self.statement(body);
                }
            }
            Node::Switch { disc, cases } => {
                self.keyword_paren(id, "switch", Some(disc));
                self.expr(disc, Prec::Lowest);
                self.tok(")");
                if self.tsc && !self.is_synthetic(id) && !self.is_synthetic(disc) {
                    let from = self.span(disc).end;
                    if let Some(p) = self.token_between(from, self.span(id).end, ")") {
                        self.expr_trailing(p + 1);
                    }
                }
                self.space();
                let case_list = self.ast.list(cases).to_vec();
                // tsc maps the case block's braces.
                let (open, close) =
                    if self.tsc_maps && !self.is_synthetic(id) && !self.is_synthetic(disc) {
                        let span = self.span(id);
                        let to = case_list
                            .first()
                            .filter(|&&c| !self.is_synthetic(c))
                            .map_or(span.end, |&c| self.span(c).start);
                        let close = (self.src.as_bytes().get(span.end as usize - 1) == Some(&b'}'))
                            .then(|| span.end - 1);
                        (self.token_between(self.span(disc).end, to, "{"), close)
                    } else {
                        (None, None)
                    };
                self.map_token("{", open);
                if self.tsc && !self.is_synthetic(id) && !self.is_synthetic(disc) {
                    // A comment after the `{` on its line stays there.
                    let to = case_list
                        .first()
                        .map_or(self.span(id).end, |&c| self.span(c).start);
                    if let Some(p) = self.token_between(self.span(disc).end, to, "{") {
                        self.trailing_comments(p + 1);
                    }
                }
                self.indent();
                for c in case_list {
                    let cspan = self.span(c);
                    if !self.is_synthetic(c) {
                        self.leading_comments(cspan.start);
                    }
                    // A block comment the clause follows on its line stays there.
                    if !(self.tsc && self.after_inline_comment()) {
                        self.newline();
                    }
                    self.map_node(c);
                    self.map_start(c);
                    if let Node::SwitchCase { test, body } = self.node(c) {
                        match test {
                            Some(t) => {
                                self.tok("case");
                                if self.tsc && !self.is_synthetic(c) && !self.is_synthetic(t) {
                                    self.token_trailing("case", cspan.start, self.span(t).start);
                                }
                                self.hard_space();
                                self.expr(t, Prec::Lowest);
                                self.tok(":");
                            }
                            None => {
                                self.tok("default");
                                if self.tsc && !self.is_synthetic(c) {
                                    self.token_trailing("default", cspan.start, cspan.end);
                                }
                                self.tok(":");
                            }
                        }
                        let stmts = self.ast.list(body).to_vec();
                        if self.tsc && !self.is_synthetic(c) {
                            // ... and so does one after the case's `:`.
                            let from = test.map_or(cspan.start, |t| self.span(t).end);
                            let to = stmts.first().map_or(cspan.end, |&b| self.span(b).start);
                            if let Some(p) = self.token_between(from, to.max(from), ":") {
                                self.trailing_comments(p + 1);
                            }
                        }
                        // tsc keeps a lone statement written on the `case` line there.
                        let same_line = self.tsc
                            && stmts.len() == 1
                            && !self.is_synthetic(stmts[0])
                            && !self.is_synthetic(c)
                            && !self.has_newline_between(cspan.start, self.span(stmts[0]).start);
                        // ... and a lone block on the next line stays on its own line.
                        let lone_block = stmts.len() == 1
                            && self.kind(stmts[0]) == NodeKind::Block
                            && !(self.tsc && !self.is_synthetic(c) && !self.is_synthetic(stmts[0]));
                        if lone_block || same_line {
                            self.space();
                            self.statement_in_list_inline(stmts[0]);
                        } else {
                            self.indent();
                            self.statements(&stmts);
                            self.dedent();
                        }
                    }
                    self.map_end(c);
                }
                if !self.is_synthetic(id) {
                    self.leading_comments(self.span(id).end.saturating_sub(1));
                }
                self.dedent();
                self.newline();
                self.map_token("}", close);
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.tok("try");
                let orig = self.tsc && !self.is_synthetic(id) && !self.is_synthetic(block);
                if orig {
                    self.token_trailing("try", self.span(id).start, self.span(block).start);
                }
                self.space();
                self.block(block);
                // Comments after a block's `}` on its line trail it (`} /* c */\ncatch`).
                let mut prev = orig.then(|| self.span(block).end);
                if let Some(p) = prev {
                    self.expr_trailing(p);
                }
                if let Some(h) = handler
                    && let Node::CatchClause {
                        param,
                        type_ann,
                        body,
                    } = self.node(h)
                {
                    self.space_or_newline();
                    self.map_node(h);
                    self.map_start(h);
                    self.tok("catch");
                    let orig = orig && !self.is_synthetic(h) && !self.is_synthetic(body);
                    let hs = self.span(h).start;
                    if orig {
                        let to = param.map_or(self.span(body).start, |p| self.span(p).start);
                        self.token_trailing("catch", hs, to);
                    }
                    if let Some(p) = param {
                        self.space();
                        self.tok("(");
                        let orig = orig && !self.is_synthetic(p);
                        if orig {
                            self.token_trailing("(", hs + 5, self.span(p).start);
                        }
                        self.pattern(p);
                        if let Some(t) = type_ann {
                            self.type_annotation(t);
                        }
                        self.tok(")");
                        if orig {
                            let from = type_ann.map_or(self.span(p).end, |t| self.span(t).end);
                            self.token_trailing(")", from, self.span(body).start);
                        }
                    }
                    self.space();
                    self.block(body);
                    prev = orig.then(|| self.span(body).end);
                    if let Some(p) = prev {
                        self.expr_trailing(p);
                    }
                    self.map_end(h);
                }
                if let Some(f) = finalizer {
                    self.space_or_newline();
                    self.tok("finally");
                    if let Some(p) = prev
                        && !self.is_synthetic(f)
                    {
                        self.token_trailing("finally", p, self.span(f).start);
                    }
                    self.space();
                    self.block(f);
                }
            }
            Node::Debugger {} => {
                self.tok("debugger");
                self.semi();
            }
            Node::With { obj, body } => {
                self.keyword_paren(id, "with", Some(obj));
                self.expr(obj, Prec::Lowest);
                self.close_paren(id, Some(obj), body);
                self.embedded(body);
            }
            Node::VarDecl { .. } => self.var_decl(id, true),
            Node::FunctionDecl {
                modifiers,
                flags,
                name,
                type_params,
                params,
                return_type,
                body,
            } => {
                let to = name
                    .or(body)
                    .map_or(self.span(id).end, |n| self.span(n).start);
                self.set_mod_src(id, self.span(id).start, to);
                self.decl_modifiers(modifiers);
                self.function(flags, name, type_params, params, return_type, body, false);
            }
            Node::ClassDecl { .. } => self.class(id),
            Node::Interface {
                modifiers,
                name,
                type_params,
                extends,
                body,
            } => {
                self.ts_modifiers(modifiers);
                self.tok("interface");
                self.hard_space();
                self.ident(name);
                if let Some(tp) = type_params {
                    self.ty(tp);
                }
                let ext = self.ast.list(extends).to_vec();
                if !ext.is_empty() {
                    self.hard_space();
                    self.tok("extends");
                    self.hard_space();
                    self.comma_list(&ext, |p, t| p.ty(t));
                }
                self.space();
                self.type_members(body, id);
            }
            Node::TypeAlias {
                modifiers,
                name,
                type_params,
                ty,
            } => {
                self.ts_modifiers(modifiers);
                self.tok("type");
                self.hard_space();
                self.ident(name);
                if let Some(tp) = type_params {
                    self.ty(tp);
                }
                self.space();
                self.tok("=");
                self.space();
                self.ty(ty);
                self.semi();
            }
            Node::Enum {
                modifiers,
                name,
                members,
            } => {
                let mut other = modifiers;
                other.remove(Modifiers::CONST);
                self.ts_modifiers(other);
                if modifiers.contains(Modifiers::CONST) {
                    self.tok("const");
                    self.hard_space();
                }
                self.tok("enum");
                self.hard_space();
                self.ident(name);
                self.space();
                self.tok("{");
                let ms = self.ast.list(members).to_vec();
                if !ms.is_empty() {
                    self.indent();
                    for (i, m) in ms.iter().enumerate() {
                        if !self.is_synthetic(*m) {
                            self.leading_comments(self.span(*m).start);
                        }
                        self.newline();
                        if let Node::EnumMember { name, init } = self.node(*m) {
                            self.map_node(*m);
                            self.property_key(name);
                            if let Some(init) = init {
                                self.space();
                                self.tok("=");
                                self.space();
                                self.expr(init, Prec::Assign);
                            }
                        }
                        if i + 1 < ms.len() {
                            self.tok(",");
                        }
                    }
                    self.dedent();
                    self.newline();
                }
                self.tok("}");
            }
            Node::Module {
                modifiers,
                kind,
                name,
                body,
            } => {
                self.ts_modifiers(modifiers);
                match kind {
                    ModuleKind::Namespace => {
                        self.tok("namespace");
                        self.hard_space();
                    }
                    ModuleKind::Module => {
                        self.tok("module");
                        self.hard_space();
                    }
                    ModuleKind::Global => {}
                }
                if kind == ModuleKind::Global {
                    self.tok("global");
                } else {
                    self.entity_name(name);
                }
                match body {
                    Some(b) => {
                        self.space();
                        self.block(b);
                    }
                    None => self.semi(),
                }
            }
            Node::ModuleBlock { stmts } => self.block_of(id, stmts),
            Node::ImportDecl {
                type_only,
                phase,
                has_clause,
                specifiers,
                source,
                attributes,
            } => {
                self.tok("import");
                if type_only {
                    self.hard_space();
                    self.tok("type");
                }
                match phase {
                    ImportPhase::Defer => {
                        self.hard_space();
                        self.tok("defer");
                    }
                    ImportPhase::Source => {
                        self.hard_space();
                        self.tok("source");
                    }
                    ImportPhase::Evaluation => {}
                }
                if has_clause {
                    self.space();
                    self.import_clause(id, specifiers);
                    self.space();
                    self.tok("from");
                }
                self.space();
                self.module_specifier(source, SpecifierKind::Import);
                if let Some(a) = attributes {
                    self.import_attributes(a);
                }
                self.semi();
            }
            Node::ImportEquals {
                modifiers,
                type_only,
                name,
                module_ref,
            } => {
                self.decl_modifiers(modifiers);
                self.tok("import");
                self.hard_space();
                if type_only {
                    self.tok("type");
                    self.hard_space();
                }
                self.ident(name);
                self.space();
                self.tok("=");
                self.space();
                match self.node(module_ref) {
                    Node::ExternalModuleRef { expr } => {
                        self.map_node(module_ref);
                        self.tok("require(");
                        self.module_specifier(expr, SpecifierKind::Import);
                        self.tok(")");
                    }
                    _ => self.entity_name(module_ref),
                }
                self.semi();
            }
            Node::ExportNamed {
                type_only,
                specifiers,
                source,
                attributes,
            } => {
                self.tok("export");
                if type_only {
                    self.hard_space();
                    self.tok("type");
                }
                self.space();
                let specs = self.ast.list(specifiers).to_vec();
                let braces = self.named_braces(id, &specs);
                let span = self.span(id);
                let open = self.clause_token(id, span.start, span.end, "{");
                if let Some(o) = open {
                    self.expr_leading(o);
                }
                if let Some((open, _)) = braces {
                    self.map_pos(open);
                }
                self.tok("{");
                if !specs.is_empty() {
                    self.space();
                    self.comma_list(&specs, |p, s| p.export_specifier(s));
                    // tsc keeps a trailing comma written in the source, also when the
                    // specifiers after the last printed one were erased (observed).
                    if self.named_trailing_comma(id) {
                        self.tok(",");
                    }
                    self.space();
                }
                self.tok("}");
                if let Some((_, end)) = braces {
                    self.map_pos(end);
                }
                if let Some(c) = open.and_then(|o| self.clause_token(id, o, span.end, "}")) {
                    self.expr_trailing(c + 1);
                }
                if let Some(src) = source {
                    self.space();
                    self.tok("from");
                    self.space();
                    self.module_specifier(src, SpecifierKind::Export);
                }
                if let Some(a) = attributes {
                    self.import_attributes(a);
                }
                self.semi();
            }
            Node::ExportAll {
                type_only,
                exported,
                source,
                attributes,
            } => {
                self.tok("export");
                if type_only {
                    self.hard_space();
                    self.tok("type");
                }
                self.space();
                // tsc maps `* as ns` (a namespace export node).
                let star = exported
                    .filter(|&e| self.tsc_maps && !self.is_synthetic(id) && !self.is_synthetic(e))
                    .and_then(|e| self.token_between(self.span(id).start, self.span(e).start, "*"));
                if let Some(p) = star {
                    self.map_pos(p);
                }
                let span = self.span(id);
                let star_at = self.clause_token(id, span.start, span.end, "*");
                if let Some(p) = star_at {
                    self.expr_leading(p);
                }
                self.tok("*");
                if let Some(p) = star_at {
                    self.expr_trailing(p + 1);
                }
                if let Some(e) = exported {
                    self.hard_space();
                    self.tok("as");
                    self.hard_space();
                    self.module_export_name(e);
                    if star.is_some() {
                        self.map_end(e);
                    }
                }
                self.space();
                self.tok("from");
                self.space();
                self.module_specifier(source, SpecifierKind::Export);
                if let Some(a) = attributes {
                    self.import_attributes(a);
                }
                self.semi();
            }
            Node::ExportDefaultExpr { expr } => {
                self.tok("export");
                self.hard_space();
                let span = self.span(id);
                if let Some(p) = self.clause_token(id, span.start, self.span(expr).start, "default")
                {
                    self.expr_leading(p);
                }
                self.tok("default");
                self.hard_space();
                let paren = self.starts_ambiguously(expr, false)
                    || matches!(self.kind(expr), NodeKind::Seq);
                if paren {
                    self.tok("(");
                }
                self.expr(expr, Prec::Assign);
                if paren {
                    self.tok(")");
                }
                self.semi();
            }
            Node::ExportAssignment { expr } => {
                self.tok("export");
                self.space();
                self.tok("=");
                self.space();
                self.expr(expr, Prec::Assign);
                self.semi();
            }
            Node::NamespaceExport { name } => {
                self.tok("export as namespace");
                self.hard_space();
                self.ident(name);
                self.semi();
            }
            Node::Missing {} => {}
            _ => {
                // An expression where a statement was expected (should not happen).
                if !n.is_statement() {
                    self.expr(id, Prec::Lowest);
                    self.semi();
                }
            }
        }
    }

    fn statement_in_list_inline(&mut self, id: NodeId) {
        self.statement(id);
        if !self.is_synthetic(id) {
            self.trailing_comments(self.span(id).end);
        }
    }

    /// `return`/`throw` argument: no line break may follow the keyword, so a leading
    /// annotation comment is kept on the same line.
    fn restricted_expr(&mut self, id: NodeId) {
        self.expr(id, Prec::Lowest);
    }

    /// True if `cons` ends with an `if` that has no `else` (dangling-else hazard).
    fn ends_with_dangling_if(&self, mut id: NodeId) -> bool {
        loop {
            match self.node(id) {
                Node::If { alt: None, .. } => return true,
                Node::If { alt: Some(a), .. } => id = a,
                Node::For { body, .. }
                | Node::ForIn { body, .. }
                | Node::ForOf { body, .. }
                | Node::While { body, .. }
                | Node::With { body, .. }
                | Node::Labeled { body, .. } => id = body,
                _ => return false,
            }
        }
    }

    /// The left side of `for (left in/of right)`. The parser reads it as any expression
    /// without `in` (an invalid target like `v++` or `a = 1` is a grammar error), so it is
    /// printed at the lowest precedence with `in` parenthesized.
    fn for_head_left(&mut self, left: NodeId, is_of: bool, is_await: bool) {
        match self.kind(left) {
            NodeKind::VarDecl => self.var_decl(left, false),
            _ => {
                // `for (let of x)` / `for (async of x)` are ambiguous; parenthesize
                // (`for await (async of x)` is not). tsc prints a source `for (async of x)`
                // as written (a grammar error, TS1106; parserForOfStatement22).
                let paren = self.starts_with_let(left)
                    || (is_of
                        && !is_await
                        && (!self.tsc || self.is_synthetic(left))
                        && matches!(self.node(left), Node::Ident { name } if self.ast.atom(name) == "async"));
                if paren {
                    self.tok("(");
                }
                let saved = self.no_in;
                self.no_in = !paren;
                self.expr(left, Prec::Lowest);
                self.no_in = saved;
                if paren {
                    self.tok(")");
                }
            }
        }
    }

    pub(crate) fn var_decl(&mut self, id: NodeId, semi: bool) {
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = self.node(id)
        else {
            return;
        };
        self.map_node(id);
        let ds = self.ast.list(decls).to_vec();
        let span = self.span(id);
        let first = ds.first().map_or(span.end, |&d| self.span(d).start);
        self.set_mod_src(id, span.start, first);
        self.decl_modifiers(modifiers);
        let kw = match kind {
            VarKind::Var => "var",
            VarKind::Let => "let",
            VarKind::Const => "const",
            VarKind::Using => "using",
            VarKind::AwaitUsing => "await",
        };
        // tsc maps the declaration list: from its keyword to the end of the last declarator.
        // A synthesized list maps through its first declarator's range (see `PrintHints`).
        let synth_list = self.is_synthetic(id)
            && ds
                .first()
                .is_some_and(|&d| self.is_synthetic(d) && self.map_range_of(d).is_some());
        let src_range = self.mod_src.take();
        let list_start = if synth_list {
            ds.first()
                .and_then(|&d| self.map_range_of(d))
                .map(|r| r.start)
        } else {
            src_range.and_then(|(from, to)| self.keyword_between(from, to, kw))
        };
        if let Some(p) = list_start {
            self.map_pos(p);
        }
        self.tok(if kind == VarKind::AwaitUsing {
            "await using"
        } else {
            kw
        });
        self.hard_space();
        for (i, d) in ds.iter().enumerate() {
            if i > 0 {
                self.tok(",");
                self.comma_comments(ds[i - 1], *d);
                self.space();
            }
            if let Node::VarDeclarator {
                name,
                definite,
                type_ann,
                init,
            } = self.node(*d)
            {
                self.map_node(*d);
                if !synth_list {
                    self.map_start(*d);
                }
                self.pattern(name);
                if definite {
                    self.tok("!");
                }
                if let Some(t) = type_ann {
                    self.type_annotation(t);
                }
                if let Some(init) = init {
                    self.space();
                    self.tok("=");
                    self.space();
                    self.expr(init, Prec::Assign);
                }
                if !synth_list {
                    self.map_end(*d);
                }
            }
        }
        if list_start.is_some()
            && let Some(&last) = ds.last()
        {
            self.map_end(last);
        }
        if semi {
            self.semi();
        }
    }

    /// The modifiers of a declaration that can reach JavaScript output, in canonical order.
    /// With [`crate::Syntax::JavaScript`], TypeScript-only modifiers are left out (tsc 7 drops
    /// them, misplaced or not); misplaced JavaScript ones (`async class`) are kept.
    pub(crate) fn decl_modifiers(&mut self, mut m: Modifiers) {
        if self.js {
            m.remove(TS_ONLY_MODIFIERS);
        }
        self.ts_modifiers(m);
    }

    /// Every modifier in `m`, in canonical order (`const`, `in`, `out` last: `const enum`,
    /// `const in T`). For nodes that only exist in TypeScript.
    pub(crate) fn ts_modifiers(&mut self, m: Modifiers) {
        const ORDER: [(Modifiers, &str); 15] = [
            (Modifiers::EXPORT, "export"),
            (Modifiers::DEFAULT, "default"),
            (Modifiers::DECLARE, "declare"),
            (Modifiers::PUBLIC, "public"),
            (Modifiers::PRIVATE, "private"),
            (Modifiers::PROTECTED, "protected"),
            (Modifiers::STATIC, "static"),
            (Modifiers::ABSTRACT, "abstract"),
            (Modifiers::OVERRIDE, "override"),
            (Modifiers::READONLY, "readonly"),
            (Modifiers::ACCESSOR, "accessor"),
            (Modifiers::ASYNC, "async"),
            (Modifiers::CONST, "const"),
            (Modifiers::IN, "in"),
            (Modifiers::OUT, "out"),
        ];
        if m.is_empty() {
            return;
        }
        for (flag, text) in ORDER {
            if m.contains(flag) {
                self.modifier_kw(text);
                self.hard_space();
            }
        }
    }

    /// A modifier keyword; with [`Printer::mod_src`] (the source range of the modifiers still
    /// to print) tsc maps its start and end, and the range moves past it.
    pub(crate) fn modifier_kw(&mut self, text: &str) {
        let range = self.mod_src.filter(|_| self.tsc_maps);
        let at = range.and_then(|(from, to)| self.keyword_between(from, to, text));
        if let (Some(p), Some((_, to))) = (at, range) {
            self.mod_src = Some((p + text.len() as u32, to));
        }
        self.map_token(text, at);
    }

    /// Sets [`Printer::mod_src`] to the source between `from` and `to` when both are real
    /// positions of the original node `id`.
    pub(crate) fn set_mod_src(&mut self, id: NodeId, from: u32, to: u32) {
        self.mod_src =
            (self.tsc_maps && !self.is_synthetic(id) && from <= to).then_some((from, to));
    }

    /// Position of the keyword `kw` in the source between `from` and `to` (a whole word,
    /// outside comments).
    pub(crate) fn keyword_between(&self, mut from: u32, to: u32, kw: &str) -> Option<u32> {
        let b = self.src.as_bytes();
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
        while from < to {
            let p = self.token_between(from, to, kw)?;
            let end = p as usize + kw.len();
            let before_ok = p == 0 || !word(b[p as usize - 1]);
            let after_ok = b.get(end).is_none_or(|&c| !word(c));
            if before_ok && after_ok {
                return Some(p);
            }
            from = p + 1;
        }
        None
    }

    /// `function name<T>(params): R { body }` (also for function expressions).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn function(
        &mut self,
        flags: FnFlags,
        name: Option<NodeId>,
        type_params: Option<NodeId>,
        params: NodeList,
        return_type: Option<NodeId>,
        body: Option<NodeId>,
        _is_expr: bool,
    ) {
        if flags.contains(FnFlags::ASYNC) {
            self.modifier_kw("async");
            self.hard_space();
        }
        self.mod_src = None;
        self.tok("function");
        if flags.contains(FnFlags::GENERATOR) {
            // tsc maps the `*` (before a name, the `*` is the token right before it).
            let star = name.filter(|_| self.tsc_maps).and_then(|n| {
                let b = self.src.as_bytes();
                let mut p = self.ast.span(n).start as usize;
                while p > 0 && b[p - 1].is_ascii_whitespace() {
                    p -= 1;
                }
                (p > 0 && b[p - 1] == b'*').then(|| p as u32 - 1)
            });
            self.map_token("*", star);
        }
        if let Some(n) = name {
            self.space_if_needed_after_star(flags);
            self.ident(n);
        } else {
            self.space();
        }
        // Parameters and body are outside any `for` head: `in` needs no parentheses there.
        let saved = self.no_in;
        self.no_in = false;
        if self.tsc
            && let (Some(n), Some(b)) = (name, body)
            && !self.is_synthetic(n)
            && !self.is_synthetic(b)
        {
            self.params_paren = Some((self.span(n).end, self.span(b).start));
        }
        self.signature(type_params, params, return_type);
        match body {
            Some(b) => {
                self.space();
                self.function_body(b);
            }
            None => self.semi(),
        }
        self.no_in = saved;
    }

    fn space_if_needed_after_star(&mut self, flags: FnFlags) {
        if flags.contains(FnFlags::GENERATOR) {
            self.space();
        } else {
            self.hard_space();
        }
    }

    /// `<T>(params): R`
    pub(crate) fn signature(
        &mut self,
        type_params: Option<NodeId>,
        params: NodeList,
        return_type: Option<NodeId>,
    ) {
        if let Some(tp) = type_params {
            self.ty(tp);
        }
        self.tok("(");
        let ps = self.ast.list(params).to_vec();
        if let Some((from, to)) = self.params_paren.take()
            && ps.is_empty()
            && let Some(p) = self.token_between(from, to, "(")
        {
            // Comments in empty parameters trail the `(`: `m( /* c */)`.
            self.expr_trailing(p + 1);
        }
        self.comma_list(&ps, |p, x| p.param(x));
        self.tok(")");
        if let Some(r) = return_type {
            self.type_annotation(r);
        }
    }

    pub(crate) fn param(&mut self, id: NodeId) {
        let orig = self.tsc && !self.is_synthetic(id);
        if orig {
            self.expr_leading(self.span(id).start);
        }
        self.map_start(id);
        self.param_inner(id);
        self.map_end(id);
        if orig {
            self.expr_trailing(self.span(id).end);
        }
    }

    fn param_inner(&mut self, id: NodeId) {
        match self.node(id) {
            Node::Param {
                decorators,
                modifiers,
                rest,
                pattern,
                optional,
                type_ann,
                init,
            } => {
                self.map_node(id);
                self.decorators(decorators, true);
                let mut modifiers = modifiers;
                if self.js {
                    // tsc 7 drops `export` on a parameter too.
                    modifiers.remove(Modifiers::EXPORT.union(Modifiers::DEFAULT));
                }
                self.decl_modifiers(modifiers);
                if rest {
                    self.dots(pattern);
                }
                self.pattern(pattern);
                if optional {
                    self.tok("?");
                }
                if let Some(t) = type_ann {
                    self.type_annotation(t);
                }
                if let Some(init) = init {
                    self.space();
                    self.tok("=");
                    self.space();
                    self.expr(init, Prec::Assign);
                }
            }
            _ => self.pattern(id),
        }
    }

    pub(crate) fn decorators(&mut self, list: NodeList, inline: bool) {
        let items = self.ast.list(list).to_vec();
        self.decorator_items(&items, inline);
    }

    fn decorator_items(&mut self, items: &[NodeId], inline: bool) {
        // tsc keeps the comments between decorators on their own lines, and those between
        // the last one and the declaration.
        let own_lines = self.tsc && !inline && !items.iter().any(|&d| self.is_synthetic(d));
        for &d in items {
            if own_lines {
                self.leading_comments(self.span(d).start);
            }
            if let Node::Decorator { expr } = self.node(d) {
                self.map_node(d);
                self.tok("@");
                // tsc prints a decorator as written, even one that needs parentheses (but
                // keeps those around an erased `as`).
                let as_written = self.tsc
                    && !self.is_synthetic(d)
                    && self.kind(expr) != NodeKind::Paren
                    && self
                        .src
                        .get(self.span(d).start as usize + 1..)
                        .is_some_and(|t| !t.trim_start().starts_with('('));
                if !as_written && self.decorator_needs_parens(expr) {
                    self.tok("(");
                    self.expr(expr, Prec::Lowest);
                    self.tok(")");
                } else if as_written {
                    self.expr(expr, Prec::Lowest);
                } else {
                    self.expr(expr, Prec::Call);
                }
                if inline {
                    self.hard_space();
                } else {
                    self.newline();
                    if self.compact {
                        self.hard_space();
                    }
                }
            }
        }
        if own_lines && let Some(&last) = items.last() {
            let next = self.next_token_start(self.span(last).end);
            self.leading_comments(next);
        }
    }

    /// Decorators allow `@a.b.c`, `@a.b(args)` and `@(expr)` only.
    fn decorator_needs_parens(&self, mut id: NodeId) -> bool {
        if let Node::Call { callee, .. } = self.node(id) {
            id = callee;
        }
        loop {
            match self.node(id) {
                Node::Ident { .. } => return false,
                // `@a.#b` is allowed too (private names in decorator member chains).
                Node::Member {
                    obj,
                    optional: false,
                    ..
                } => id = obj,
                Node::Paren { .. } => return false,
                // `@x!`, `@x.y!`, `@x!.y`.
                Node::NonNull { expr } => id = expr,
                _ => return true,
            }
        }
    }

    pub(crate) fn class(&mut self, id: NodeId) {
        let (decorators, modifiers, name, type_params, heritage, members) = match self.node(id) {
            Node::ClassDecl {
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            }
            | Node::ClassExpr {
                decorators,
                modifiers,
                name,
                type_params,
                heritage,
                members,
            } => (decorators, modifiers, name, type_params, heritage, members),
            _ => return,
        };
        let is_decl = self.kind(id) == NodeKind::ClassDecl;
        // Decorators go before `export` unless they were written after it (`export @dec
        // class`, or both: `@a export @b class`).
        let decos = self.ast.list(decorators).to_vec();
        let split = if modifiers.contains(Modifiers::EXPORT) {
            let mut from = self.span(id).start;
            decos
                .iter()
                .position(|&d| {
                    if self.is_synthetic(d) {
                        return false;
                    }
                    let start = self.span(d).start;
                    let after = start > self.span(id).start
                        && (from == self.span(id).start
                            || self.token_between(from, start, "export").is_some());
                    from = self.span(d).end;
                    after
                })
                .unwrap_or(decos.len())
        } else {
            decos.len()
        };
        let decos_after_export = split < decos.len();
        self.decorator_items(&decos[..split], !is_decl);
        let head_end = name
            .or(heritage_first(self, heritage))
            .map_or(self.span(id).end, |n| self.span(n).start);
        let mods_from = decos[..split]
            .last()
            .filter(|&&d| !self.is_synthetic(d))
            .map_or(self.span(id).start, |&d| self.span(d).end);
        self.set_mod_src(id, mods_from, head_end);
        let mut mods = modifiers;
        if decos_after_export {
            let mut export_mods = Modifiers::NONE;
            for f in [Modifiers::EXPORT, Modifiers::DEFAULT] {
                if mods.contains(f) {
                    export_mods.insert(f);
                    mods.remove(f);
                }
            }
            self.decl_modifiers(export_mods);
            let saved = self.mod_src.take();
            if self.tsc && is_decl {
                // tsc: `export ` and each decorator on a line of its own.
                self.newline();
                self.decorator_items(&decos[split..], false);
            } else {
                self.decorator_items(&decos[split..], true);
            }
            if let (Some((_, to)), Some(&d)) = (saved, decos.last()) {
                self.mod_src = Some((self.span(d).end, to));
            }
        }
        self.decl_modifiers(mods);
        self.mod_src = None;
        self.tok("class");
        if let Some(n) = name {
            self.hard_space();
            self.ident(n);
        }
        if let Some(tp) = type_params {
            self.ty(tp);
        }
        for h in self.ast.list(heritage).to_vec() {
            if let Node::HeritageClause { kind, types } = self.node(h) {
                // tsc maps the clause before the space in front of it.
                self.map_start(h);
                self.hard_space();
                self.tok(match kind {
                    HeritageKind::Extends => "extends",
                    HeritageKind::Implements => "implements",
                });
                self.hard_space();
                let ts = self.ast.list(types).to_vec();
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.tok(",");
                        self.space();
                    }
                    if let Node::ExprWithTypeArgs { expr, type_args } = self.node(*t) {
                        self.map_start(*t);
                        if kind == HeritageKind::Extends {
                            self.expr(expr, Prec::LeftHandSide);
                        } else {
                            self.entity_or_expr(expr);
                        }
                        if let Some(ta) = type_args {
                            self.ty(ta);
                        }
                        self.map_end(*t);
                    } else {
                        self.expr(*t, Prec::LeftHandSide);
                    }
                }
                self.map_end(h);
            }
        }
        self.space();
        self.tok("{");
        let ms = self.ast.list(members).to_vec();
        let span = self.span(id);
        if self.tsc && !self.is_synthetic(id) {
            // tsc drops a comment after a class body's `{` on its line.
            let header_end = name
                .map(|n| self.span(n).end)
                .into_iter()
                .chain(type_params.map(|t| self.span(t).end))
                .chain(self.ast.list(heritage).iter().map(|&h| self.span(h).end))
                .chain(self.ast.list(decorators).iter().map(|&d| self.span(d).end))
                .filter(|&e| e <= span.end)
                .max()
                .unwrap_or(span.start);
            let body_start = ms
                .iter()
                .find(|&&m| !self.is_synthetic(m))
                .map_or(span.end, |&m| self.span(m).start);
            if let Some(p) = self.token_between(header_end, body_start, "{") {
                self.drop_same_line_comments(p + 1);
            }
        }
        // The class's empty `;` members, printed in source order among its original members
        // (tsc keeps them, also after members a transform removed).
        let semis: Vec<u32> = if self.tsc && !self.is_synthetic(id) {
            let all = &self.ast.class_semicolons;
            let from = all.partition_point(|&(_, p)| p < span.start);
            all[from..]
                .iter()
                .take_while(|&&(_, p)| p < span.end)
                .filter(|&&(end, _)| end == span.end)
                .map(|&(_, p)| p)
                .collect()
        } else {
            Vec::new()
        };
        if ms.is_empty() && semis.is_empty() {
            if self.tsc {
                // tsc always prints a class body on its own lines: `class C {\n}`.
                self.newline();
            }
            self.tok("}");
            if !self.is_synthetic(id) {
                self.skip_comments_to(span.end);
            }
            return;
        }
        self.indent();
        // Members are outside any `for` head: `in` needs no parentheses there.
        let saved = self.no_in;
        self.no_in = false;
        let mut semis = semis.into_iter().peekable();
        for &m in &ms {
            if !self.is_synthetic(m) {
                let start = self.span(m).start;
                while let Some(p) = semis.next_if(|&p| p < start) {
                    self.leading_comments(p);
                    self.newline();
                    self.map_token(";", Some(p));
                    self.trailing_comments(p + 1);
                }
                self.leading_comments(start);
            }
            if !(self.tsc && self.after_inline_comment()) {
                self.newline();
            }
            // A member ends like a statement: its trailing comments follow its `;`.
            let saved = self.stmt_end;
            if !self.is_synthetic(m) && self.stmt_end != 0 {
                self.stmt_end = self.span(m).end;
            }
            self.class_member(m);
            self.stmt_end = saved;
            if !self.is_synthetic(m) {
                self.trailing_comments(self.span(m).end);
            }
        }
        for p in semis {
            self.leading_comments(p);
            self.newline();
            self.map_token(";", Some(p));
            self.trailing_comments(p + 1);
        }
        self.no_in = saved;
        if !self.is_synthetic(id) {
            self.leading_comments(span.end.saturating_sub(1));
        }
        self.dedent();
        self.newline();
        self.tok("}");
        if !self.is_synthetic(id) {
            self.skip_comments_to(span.end);
        }
    }

    fn entity_or_expr(&mut self, id: NodeId) {
        self.expr(id, Prec::LeftHandSide);
    }

    pub(crate) fn class_member(&mut self, id: NodeId) {
        self.map_start(id);
        self.mod_src = None;
        self.class_member_inner(id);
        self.map_end(id);
    }

    /// Sets [`Printer::mod_src`] for a member: after its decorators, up to its key.
    fn member_mod_src(&mut self, id: NodeId, decorators: NodeList, key: NodeId) {
        let from = self
            .ast
            .list(decorators)
            .last()
            .filter(|&&d| !self.is_synthetic(d))
            .map_or(self.span(id).start, |&d| self.span(d).end);
        let to = if self.is_synthetic(key) {
            from
        } else {
            self.span(key).start
        };
        self.set_mod_src(id, from, to);
    }

    fn class_member_inner(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                optional,
                definite,
                type_ann,
                value,
            } => {
                self.decorators(decorators, false);
                self.member_mod_src(id, decorators, key);
                self.decl_modifiers(modifiers);
                self.mod_src = None;
                self.property_key(key);
                if optional {
                    self.tok("?");
                }
                if definite {
                    self.tok("!");
                }
                if let Some(t) = type_ann {
                    self.type_annotation(t);
                }
                if let Some(v) = value {
                    self.space();
                    self.tok("=");
                    self.space();
                    self.expr(v, Prec::Assign);
                }
                self.semi();
            }
            Node::MethodDef { .. } => {
                self.method(id);
                if let Node::MethodDef { value, .. } = self.node(id)
                    && let Node::FunctionExpr { body: None, .. } = self.node(value)
                {
                    self.semi();
                }
            }
            Node::StaticBlock { body } => {
                self.tok("static");
                self.space();
                self.body_block = true;
                self.block_of(id, body);
            }
            Node::IndexSignature { .. } => {
                self.index_signature(id);
                self.semi();
            }
            Node::Empty {} => self.semi(),
            _ => self.any(id),
        }
    }

    /// Method, getter, setter or constructor (class or object literal).
    pub(crate) fn method(&mut self, id: NodeId) {
        let Node::MethodDef {
            decorators,
            modifiers,
            kind,
            key,
            optional,
            value,
        } = self.node(id)
        else {
            return;
        };
        self.decorators(decorators, false);
        self.member_mod_src(id, decorators, key);
        self.decl_modifiers(modifiers);
        let (flags, type_params, params, return_type, body) = match self.node(value) {
            Node::FunctionExpr {
                flags,
                type_params,
                params,
                return_type,
                body,
                ..
            } => (flags, type_params, params, return_type, body),
            _ => return,
        };
        if flags.contains(FnFlags::ASYNC) {
            self.modifier_kw("async");
            self.hard_space();
        }
        self.mod_src = None;
        match kind {
            MethodKind::Get => {
                self.tok("get");
                self.hard_space();
            }
            MethodKind::Set => {
                self.tok("set");
                self.hard_space();
            }
            _ => {}
        }
        if flags.contains(FnFlags::GENERATOR) {
            // tsc maps the `*` (the token right before the key).
            let star = (self.tsc_maps && !self.is_synthetic(key))
                .then_some(())
                .and_then(|()| {
                    let b = self.src.as_bytes();
                    let mut p = self.ast.span(key).start as usize;
                    while p > 0 && b[p - 1].is_ascii_whitespace() {
                        p -= 1;
                    }
                    (p > 0 && b[p - 1] == b'*').then(|| p as u32 - 1)
                });
            self.map_token("*", star);
        }
        if kind == MethodKind::Constructor {
            // tsc has no name node for `constructor`: nothing of it is mapped.
            let saved = std::mem::replace(&mut self.map_muted, true);
            self.property_key(key);
            self.map_muted = saved;
        } else {
            self.property_key(key);
        }
        if optional {
            self.tok("?");
        }
        self.map_node(value);
        if self.tsc
            && let Some(b) = body
            && !self.is_synthetic(key)
            && !self.is_synthetic(b)
        {
            self.params_paren = Some((self.span(key).end, self.span(b).start));
        }
        self.signature(type_params, params, return_type);
        if let Some(b) = body {
            self.space();
            self.function_body(b);
        }
    }

    pub(crate) fn comma_list<F: FnMut(&mut Self, NodeId)>(&mut self, items: &[NodeId], mut f: F) {
        for (i, &it) in items.iter().enumerate() {
            if i > 0 {
                self.tok(",");
                self.comma_comments(items[i - 1], it);
                self.space();
            }
            f(self, it);
        }
    }

    /// [`Printer::tsc_maps`]: the source positions of the `{` and `}` around the named
    /// imports or exports `named` of the original declaration `decl` (tsc maps the start and
    /// end of that list).
    fn named_braces(&self, decl: NodeId, named: &[NodeId]) -> Option<(u32, u32)> {
        if !self.tsc_maps || self.is_synthetic(decl) || named.iter().any(|&n| self.is_synthetic(n))
        {
            return None;
        }
        let span = self.span(decl);
        let (from, to) = match (named.first(), named.last()) {
            (Some(&f), Some(&l)) => (self.span(f).start, self.span(l).end),
            _ => (span.end, span.start),
        };
        let open = self.token_between(span.start, from.min(span.end), "{")?;
        let close = self.token_between(to.max(open + 1), span.end, "}")?;
        Some((open, close + 1))
    }

    /// Layout::Tsc: the comments before original node `n` (`import /*2*/ D`).
    fn node_leading(&mut self, n: NodeId) {
        if self.tsc && !self.is_synthetic(n) {
            self.expr_leading(self.span(n).start);
        }
    }

    /// Layout::Tsc: the comments after original node `n` on its line (`D /*3*/,`).
    fn node_trailing(&mut self, n: NodeId) {
        if self.tsc && !self.is_synthetic(n) {
            self.expr_trailing(self.span(n).end);
        }
    }

    /// Layout::Tsc: where token `tok` of the original `decl` is, between `from` and `to`.
    fn clause_token(&self, decl: NodeId, from: u32, to: u32, tok: &str) -> Option<u32> {
        if !self.tsc || self.is_synthetic(decl) {
            return None;
        }
        self.token_between(from, to, tok)
    }

    /// The original `import`/`export` `id` ends its `{ ... }` list with a `,` (tsc layout).
    fn named_trailing_comma(&self, id: NodeId) -> bool {
        if !self.tsc || self.is_synthetic(id) {
            return false;
        }
        let span = self.span(id);
        let Some(open) = self.token_between(span.start, span.end, "{") else {
            return false;
        };
        let Some(close) = self.token_between(open + 1, span.end, "}") else {
            return false;
        };
        let p = self.prev_token_end(close);
        p > open + 1 && self.src.as_bytes().get(p as usize - 1) == Some(&b',')
    }

    fn import_clause(&mut self, decl: NodeId, specifiers: NodeList) {
        let specs = self.ast.list(specifiers).to_vec();
        let named: Vec<NodeId> = specs
            .iter()
            .copied()
            .filter(|&s| self.kind(s) == NodeKind::ImportSpecifier)
            .collect();
        let braces = self.named_braces(decl, &named);
        // tsc maps the import clause: from its first binding to the end of its last.
        let clause = specs
            .first()
            .filter(|_| self.tsc_maps && !self.is_synthetic(decl))
            .map(|&s| match self.kind(s) {
                NodeKind::ImportSpecifier => braces.map_or(self.span(s).start, |b| b.0),
                _ => self.span(s).start,
            });
        if let Some(p) = clause {
            self.map_pos(p);
        }
        let mut first = true;
        let mut clause_end = None;
        for s in specs {
            match self.node(s) {
                Node::ImportDefaultSpecifier { local } => {
                    if !first {
                        self.tok(",");
                        self.space();
                    }
                    self.node_leading(s);
                    self.ident(local);
                    self.node_trailing(s);
                    clause_end = Some(s);
                    first = false;
                }
                Node::ImportNamespaceSpecifier { local } => {
                    if !first {
                        self.tok(",");
                        self.space();
                    }
                    self.node_leading(s);
                    self.map_start(s);
                    self.tok("*");
                    if self.tsc && !self.is_synthetic(s) {
                        self.expr_trailing(self.span(s).start + 1);
                    }
                    self.space();
                    self.tok("as");
                    self.hard_space();
                    self.node_leading(local);
                    self.ident(local);
                    self.map_end(s);
                    self.node_trailing(local);
                    clause_end = Some(s);
                    first = false;
                }
                _ => {}
            }
        }
        let has_named_braces = !named.is_empty() || first;
        if has_named_braces {
            if !first {
                self.tok(",");
                self.space();
            }
            let span = self.span(decl);
            let open = self.clause_token(decl, span.start, span.end, "{");
            if let Some(o) = open {
                self.expr_leading(o);
            }
            if let Some((open, _)) = braces {
                self.map_pos(open);
            }
            self.tok("{");
            if !named.is_empty() {
                self.space();
                self.comma_list(&named, |p, s| p.import_specifier(s));
                // tsc keeps a trailing comma written in the source, also when the
                // specifiers after the last printed one were erased (observed).
                if self.named_trailing_comma(decl) {
                    self.tok(",");
                }
                self.space();
            }
            self.tok("}");
            if let Some((_, end)) = braces {
                self.map_pos(end);
            }
            if let Some(c) = open.and_then(|o| self.clause_token(decl, o, span.end, "}")) {
                self.expr_trailing(c + 1);
            }
        } else if let (Some(_), Some(e)) = (clause, clause_end) {
            self.map_end(e);
        }
    }

    fn import_specifier(&mut self, id: NodeId) {
        if let Node::ImportSpecifier {
            type_only,
            imported,
            local,
        } = self.node(id)
        {
            self.specifier_leading(id);
            self.map_node(id);
            if type_only {
                self.tok("type");
                self.hard_space();
            }
            if let Some(imp) = imported {
                self.module_export_name(imp);
                self.node_trailing(imp);
                self.hard_space();
                self.tok("as");
                self.hard_space();
                self.node_leading(local);
            }
            self.ident(local);
            self.specifier_trailing(id);
        }
    }

    fn export_specifier(&mut self, id: NodeId) {
        if let Node::ExportSpecifier {
            type_only,
            local,
            exported,
        } = self.node(id)
        {
            self.specifier_leading(id);
            self.map_node(id);
            if type_only {
                self.tok("type");
                self.hard_space();
            }
            self.module_export_name(local);
            if let Some(e) = exported {
                self.node_trailing(local);
                self.hard_space();
                self.tok("as");
                self.hard_space();
                self.node_leading(e);
                self.module_export_name(e);
            }
            self.specifier_trailing(id);
        }
    }

    /// Layout::Tsc: the comments before an original import or export specifier. A comment
    /// on its own source line goes on its own line after the `, ` (or `{ `):
    /// `export { a, \n// c\nb, }` (observed in tsc 7).
    fn specifier_leading(&mut self, id: NodeId) {
        if self.tsc && !self.is_synthetic(id) {
            self.expr_leading(self.span(id).start);
        }
    }

    /// Layout::Tsc: the comments after an original specifier, before its `,`
    /// (`b /* c */, c`).
    fn specifier_trailing(&mut self, id: NodeId) {
        if self.tsc && !self.is_synthetic(id) {
            self.expr_trailing(self.span(id).end);
        }
    }

    /// An identifier or string module export name.
    fn module_export_name(&mut self, id: NodeId) {
        match self.node(id) {
            Node::StringLit { .. } => self.expr(id, Prec::Primary),
            _ => self.ident(id),
        }
    }

    fn module_specifier(&mut self, id: NodeId, kind: SpecifierKind) {
        self.map_node(id);
        let start = self.pos();
        self.expr(id, Prec::Primary);
        let end = self.pos();
        if end >= start + 2 {
            self.record_specifier(kind, start + 1, end - 1);
        }
    }

    pub(crate) fn import_attributes(&mut self, id: NodeId) {
        if let Node::ImportAttributes { assert, entries } = self.node(id) {
            self.space();
            self.tok(if assert { "assert" } else { "with" });
            self.space();
            self.tok("{");
            let es = self.ast.list(entries).to_vec();
            if !es.is_empty() {
                self.space();
                self.comma_list(&es, |p, e| {
                    if let Node::ImportAttribute { key, value } = p.node(e) {
                        p.property_key(key);
                        p.tok(":");
                        p.space();
                        p.expr(value, Prec::Assign);
                    }
                });
                self.space();
            }
            self.tok("}");
        }
    }

    /// `A`, `A.B.C` (identifiers and qualified names), or a string module name.
    pub(crate) fn entity_name(&mut self, id: NodeId) {
        match self.node(id) {
            Node::QualifiedName { left, right } => {
                self.entity_name(left);
                self.tok(".");
                self.ident(right);
            }
            Node::Member { obj, prop, .. } => {
                self.entity_name(obj);
                self.tok(".");
                self.ident(prop);
            }
            Node::StringLit { .. } => self.expr(id, Prec::Primary),
            _ => self.ident(id),
        }
    }
}

/// The first heritage clause of a class, if any.
fn heritage_first(p: &Printer<'_>, heritage: NodeList) -> Option<NodeId> {
    p.ast.list(heritage).first().copied()
}
