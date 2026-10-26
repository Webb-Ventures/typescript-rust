//! Expressions, patterns, literals and JSX, with precedence-driven parenthesization.

use ts_r_ast::{BinaryOp, FnFlags, ImportPhase, MetaPropKind, Node, NodeId, NodeKind, UpdateOp};

use crate::SpecifierKind;
use crate::literals::{best_quote, js_number, quote_string, template_raw};
use crate::printer::Printer;

/// Operator precedence, loosest first. An expression printed where at least `min` is required
/// gets parentheses if its own precedence is lower.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prec {
    Lowest,
    /// Assignment, arrow functions, `yield`, spread arguments.
    Assign,
    Conditional,
    Coalesce,
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    Equality,
    Relational,
    Shift,
    Additive,
    Multiplicative,
    Exponent,
    /// Prefix operators, `await`, `<T>x`.
    Unary,
    /// Postfix `++`/`--`.
    Update,
    /// `new X` without arguments.
    LeftHandSide,
    /// Calls, member access, `new X()`.
    Call,
    Primary,
}

pub(crate) fn binary_prec(op: BinaryOp) -> Prec {
    use BinaryOp::*;
    match op {
        Coalesce => Prec::Coalesce,
        LogicalOr => Prec::LogicalOr,
        LogicalAnd => Prec::LogicalAnd,
        BitOr => Prec::BitOr,
        BitXor => Prec::BitXor,
        BitAnd => Prec::BitAnd,
        Eq | NotEq | StrictEq | StrictNotEq => Prec::Equality,
        Lt | LtEq | Gt | GtEq | In | InstanceOf => Prec::Relational,
        Shl | Shr | UShr => Prec::Shift,
        Add | Sub => Prec::Additive,
        Mul | Div | Mod => Prec::Multiplicative,
        Exp => Prec::Exponent,
    }
}

/// The leftmost token of an expression, for statement-start ambiguities.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Leftmost {
    Brace,
    Function,
    Class,
    Let,
    Other,
}

impl Printer<'_> {
    pub(crate) fn prec(&self, id: NodeId) -> Prec {
        match self.node(id) {
            Node::Seq { .. } => Prec::Lowest,
            Node::Arrow { .. } | Node::Yield { .. } | Node::Assign { .. } | Node::Spread { .. } => {
                Prec::Assign
            }
            Node::Conditional { .. } => Prec::Conditional,
            Node::Binary { op, .. } => binary_prec(op),
            Node::As { .. } | Node::Satisfies { .. } => Prec::Relational,
            Node::Unary { .. } | Node::Await { .. } | Node::TypeAssertion { .. } => Prec::Unary,
            Node::Update { .. } => Prec::Update,
            Node::New {
                has_args: false, ..
            } => Prec::LeftHandSide,
            Node::New { .. }
            | Node::Call { .. }
            | Node::Member { .. }
            | Node::Index { .. }
            | Node::TaggedTemplate { .. }
            | Node::MetaProperty { .. }
            | Node::ImportCall { .. }
            | Node::NonNull { .. }
            | Node::Instantiation { .. } => Prec::Call,
            Node::NumberLit { value }
                if self.is_synthetic(id) && value.get().is_sign_negative() =>
            {
                Prec::Unary
            }
            Node::NumberLit { value } if self.is_synthetic(id) && !value.get().is_finite() => {
                Prec::Multiplicative
            }
            _ => Prec::Primary,
        }
    }

    /// Prints `id`, parenthesized if its precedence is below `min`.
    pub(crate) fn expr(&mut self, id: NodeId, min: Prec) {
        let orig = self.tsc && !self.is_synthetic(id);
        if orig {
            self.expr_leading(self.span(id).start);
        }
        // Trailing comments go after the outermost expression ending where they start,
        // after its source-map end (tsc).
        let end = self.ast.span(id).end;
        let parent_end = self.expr_ends.last().copied();
        self.expr_ends.push(if orig { end } else { u32::MAX });
        self.expr_parenthesized(id, min);
        self.expr_ends.pop();
        if orig && parent_end != Some(end) {
            self.expr_trailing(end);
        }
    }

    fn expr_parenthesized(&mut self, id: NodeId, min: Prec) {
        let p = self.prec(id);
        if p < min {
            let saved = self.no_in;
            self.no_in = false;
            self.tok("(");
            self.expr_mapped(id);
            self.tok(")");
            self.no_in = saved;
        } else {
            self.expr_mapped(id);
        }
    }

    /// [`Printer::expr_commented`] between the node's start and end segments.
    fn expr_mapped(&mut self, id: NodeId) {
        self.map_start(id);
        self.expr_commented(id);
        self.map_end(id);
    }

    /// `expr_inner` with the node's synthesized comments (`0 /* E.A */`), inside any
    /// parentheses, as tsc prints them.
    fn expr_commented(&mut self, id: NodeId) {
        let comments = self.ast.synthetic_comments_of(id);
        if comments.is_empty() {
            return self.expr_inner(id);
        }
        for c in comments.iter().filter(|c| !c.trailing) {
            self.synthetic_comment(self.ast.atom(c.text), false);
        }
        self.expr_inner(id);
        for c in comments.iter().filter(|c| c.trailing) {
            self.synthetic_comment(self.ast.atom(c.text), true);
        }
    }

    /// An argument-like position (call arguments, array elements, property values).
    fn arg(&mut self, id: NodeId) {
        let saved = self.no_in;
        self.no_in = false;
        self.expr(id, Prec::Assign);
        self.no_in = saved;
    }

    fn expr_inner(&mut self, id: NodeId) {
        let n = self.node(id);
        match n {
            Node::Ident { .. } => self.ident(id),
            Node::PrivateName { name } => {
                self.map_node(id);
                let s = format!("#{}", self.ast.atom(name));
                match self.escaped_raw(id, &s) {
                    Some(raw) => self.tok(raw),
                    None => self.tok(&s),
                }
            }
            Node::This {} => {
                self.map_node(id);
                self.tok("this");
            }
            Node::Super {} => {
                self.map_node(id);
                self.tok("super");
            }
            Node::NullLit {} => {
                self.map_node(id);
                self.tok("null");
            }
            Node::BoolLit { value } => {
                self.map_node(id);
                self.tok(if value { "true" } else { "false" });
            }
            Node::NumberLit { value } => {
                self.map_node(id);
                match self.raw(id) {
                    // Numeric separators below ES2021, and (tsc) a legacy octal (`055`) or
                    // leading-zero decimal (`09.5`) literal, print as their value (`45`, `9.5`).
                    Some(raw)
                        if self.strip_separators(raw)
                            || (self.tsc
                                && raw.len() > 1
                                && raw.starts_with('0')
                                && raw.as_bytes()[1].is_ascii_digit()
                                && value.get().is_finite()) =>
                    {
                        let s = js_number(value.get());
                        self.tok(&s);
                    }
                    Some(raw) => self.tok(raw),
                    None => {
                        let v = value.get();
                        if v.is_nan() {
                            self.tok("NaN");
                        } else if v.is_infinite() {
                            self.tok(if v > 0.0 { "1 / 0" } else { "-1 / 0" });
                        } else {
                            let s = js_number(v);
                            self.tok(&s);
                        }
                    }
                }
            }
            Node::BigIntLit { value } => {
                self.map_node(id);
                match self.raw(id) {
                    Some(raw) if self.tsc => {
                        let s = tsc_bigint(raw);
                        self.tok(&s);
                    }
                    Some(raw) => self.tok(raw),
                    None => {
                        let s = format!("{}n", self.ast.atom(value));
                        self.tok(&s);
                    }
                }
            }
            Node::StringLit { value } => {
                self.map_node(id);
                match self.raw(id) {
                    Some(raw) if raw.starts_with('"') || raw.starts_with('\'') => self.tok(raw),
                    // tsc prints its synthesized strings double-quoted, non-ASCII escaped.
                    None if self.tsc => {
                        let v = self.ast.atom(value);
                        let mut s = String::with_capacity(v.len() + 2);
                        crate::literals::quote_string_ascii(v, &mut s);
                        self.tok(&s);
                    }
                    _ => {
                        let v = self.ast.atom(value).to_owned();
                        self.string_value(&v);
                    }
                }
            }
            Node::RegexLit { pattern, flags } => {
                self.map_node(id);
                match self.raw(id) {
                    Some(raw) => self.tok(raw),
                    None => {
                        let s = format!("/{}/{}", self.ast.atom(pattern), self.ast.atom(flags));
                        self.tok(&s);
                    }
                }
            }
            Node::TemplateLit { parts } => {
                self.map_node(id);
                self.template(parts);
            }
            Node::TaggedTemplate {
                tag,
                type_args,
                quasi,
            } => {
                self.callee(tag, false);
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
                if self.tsc {
                    // tsc separates every tag from its template: `tag `x``.
                    self.space();
                }
                self.map_node(quasi);
                // tsc maps the template's start and end.
                self.map_start(quasi);
                if let Node::TemplateLit { parts } = self.node(quasi) {
                    self.template(parts);
                }
                self.map_end(quasi);
            }
            Node::ArrayLit { elements } => {
                self.map_node(id);
                self.array_like(id, elements, false);
            }
            Node::Hole {} => {}
            Node::ObjectLit { props } => {
                self.map_node(id);
                self.object_like(id, props, false);
            }
            Node::Property { .. }
            | Node::ShorthandProp { .. }
            | Node::MethodDef { .. }
            | Node::Spread { .. } => self.object_member(id),
            Node::FunctionExpr {
                flags,
                name,
                type_params,
                params,
                return_type,
                body,
                ..
            } => {
                self.map_node(id);
                let start = self.span(id).start;
                self.set_mod_src(id, start, start + 5);
                self.function(flags, name, type_params, params, return_type, body, true);
            }
            Node::ClassExpr { .. } => {
                self.map_node(id);
                self.class(id);
            }
            Node::Arrow {
                flags,
                type_params,
                params,
                return_type,
                body,
            } => {
                self.map_node(id);
                self.arrow(id, flags, type_params, params, return_type, body);
            }
            Node::Paren { expr } => {
                self.map_node(id);
                let saved = self.no_in;
                self.no_in = false;
                self.tok("(");
                if self.tsc && !self.is_synthetic(id) {
                    self.expr_trailing(self.span(id).start + 1);
                }
                self.expr(expr, Prec::Lowest);
                self.tok(")");
                self.no_in = saved;
            }
            Node::Seq { exprs } => {
                let mut es = self.ast.list(exprs).to_vec();
                // Adjacent JSX elements (`<a/><b/>`, TS2657) parse as a sequence led by a
                // zero-width `Missing` standing for tsc's synthesized comma: it prints nothing.
                if es.len() > 1 && self.is_jsx_sibling_marker(es[0], es[1]) {
                    es.remove(0);
                }
                // A sequence the transform breaks after every comma is indented from its
                // first element on (`(_a = class {\n    },\n    _a)`).
                let hinted = self.tsc && self.hint_multi_line(id);
                let mut indented = hinted;
                if hinted {
                    self.indent();
                }
                for (i, e) in es.iter().enumerate() {
                    if i > 0 {
                        self.tok(",");
                        self.comma_comments(es[i - 1], *e);
                        // tsc keeps a line break after a comma, indented once (every
                        // comma of a sequence the transform asked to break).
                        if hinted || (self.tsc && self.source_breaks(es[i - 1], *e)) {
                            if !indented {
                                self.indent();
                                indented = true;
                            }
                            self.newline();
                        } else {
                            self.space();
                        }
                    }
                    self.expr(*e, Prec::Assign);
                }
                if indented {
                    self.dedent();
                }
            }
            Node::Unary { op, arg } => {
                self.map_node(id);
                let s = op.as_str();
                self.tok(s);
                if self.tsc && !self.is_synthetic(id) && !self.is_synthetic(arg) {
                    // Comments after the operator trail it: `! /* c */x`.
                    let at = self.span(id).start + s.len() as u32;
                    if at <= self.span(arg).start {
                        self.expr_trailing(at);
                    }
                }
                if s.as_bytes()[0].is_ascii_alphabetic() {
                    self.hard_space();
                }
                self.expr(arg, Prec::Unary);
            }
            Node::Update { op, prefix, arg } => {
                self.map_node(id);
                let s = match op {
                    UpdateOp::Increment => "++",
                    UpdateOp::Decrement => "--",
                };
                if prefix {
                    self.tok(s);
                    self.expr(arg, Prec::LeftHandSide);
                } else {
                    self.expr(arg, Prec::LeftHandSide);
                    self.tok(s);
                }
            }
            Node::Await { arg } => {
                self.map_node(id);
                self.tok("await");
                self.hard_space();
                self.expr(arg, Prec::Unary);
            }
            Node::Yield { delegate, arg } => {
                self.map_node(id);
                self.tok("yield");
                if delegate {
                    self.tok("*");
                }
                if let Some(a) = arg {
                    if delegate {
                        self.space();
                    } else {
                        self.hard_space();
                    }
                    self.expr(a, Prec::Assign);
                }
            }
            Node::Binary { left, op, right } => self.binary(id, left, op, right),
            Node::Assign { left, op, right } => {
                self.assign_target(left);
                let (before, after) = self.op_breaks(left, right, op.as_str());
                let mut n = self.break_or_space(before);
                self.map_node(id);
                self.tok(op.as_str());
                n += self.break_or_space(after);
                self.expr(right, Prec::Assign);
                self.dedent_n(n);
            }
            Node::Conditional { test, cons, alt } => {
                self.expr(test, Prec::Coalesce);
                let (before, after) = self.op_breaks(test, cons, "?");
                let mut n = self.break_or_space(before);
                // tsc maps `?` and `:`.
                let q = self.token_after(test, cons, "?");
                self.map_token("?", q);
                n += self.break_or_space(after);
                let saved = self.no_in;
                self.no_in = false;
                self.expr(cons, Prec::Assign);
                self.no_in = saved;
                self.dedent_n(n);
                let (before, after) = self.op_breaks(cons, alt, ":");
                let mut n = self.break_or_space(before);
                let c = self.token_after(cons, alt, ":");
                self.map_token(":", c);
                n += self.break_or_space(after);
                self.expr(alt, Prec::Assign);
                self.dedent_n(n);
            }
            Node::Call {
                callee,
                optional,
                type_args,
                args,
            } => {
                self.annotation_comments(self.span(id).start);
                self.callee(callee, false);
                self.map_node(id);
                if optional {
                    self.question_dot(callee, self.span(id).end);
                }
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
                self.args_after(args, id, callee);
            }
            Node::New {
                callee,
                type_args,
                has_args,
                args,
            } => {
                self.annotation_comments(self.span(id).start);
                self.map_node(id);
                self.tok("new");
                if self.tsc && !self.is_synthetic(id) {
                    self.expr_trailing(self.span(id).start + 3);
                }
                self.hard_space();
                self.callee(callee, true);
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
                if has_args || !args.is_empty() {
                    self.args_after(args, id, callee);
                }
            }
            Node::Member {
                obj,
                optional,
                prop,
            } => {
                self.member_object(obj);
                let dot = if optional { "?." } else { "." };
                // tsc keeps a line break before the `.`, indented once.
                let (before, _) = self.op_breaks(obj, prop, dot);
                if before {
                    self.indent();
                    self.newline();
                    // Comments on the lines before the `.` stay there (tsc).
                    if self.tsc && !self.is_synthetic(prop) {
                        let p = self
                            .prev_token_end(self.span(prop).start)
                            .saturating_sub(dot.len() as u32);
                        if self
                            .src
                            .get(p as usize..)
                            .is_some_and(|t| t.starts_with(dot))
                        {
                            self.expr_leading(p);
                        }
                    }
                }
                self.map_node(prop);
                if optional {
                    self.question_dot(obj, self.span(prop).start);
                } else {
                    self.tok(dot);
                }
                if self.tsc && !self.is_synthetic(obj) && !self.is_synthetic(prop) {
                    // Comments after the `.` trail it: `x. /* c */y`.
                    let (from, to) = (self.span(obj).end, self.span(prop).start);
                    if let Some(p) = self.token_between(from, to.max(from), dot) {
                        self.expr_trailing(p + dot.len() as u32);
                    }
                }
                match self.node(prop) {
                    Node::PrivateName { .. } => {
                        self.map_start(prop);
                        self.expr_inner(prop);
                        self.map_end(prop);
                    }
                    _ => self.ident_name(prop),
                }
                if before {
                    self.dedent();
                }
            }
            Node::Index {
                obj,
                optional,
                index,
            } => {
                self.member_object(obj);
                if optional {
                    self.question_dot(obj, self.span(index).start);
                }
                self.tok("[");
                if self.tsc && !self.is_synthetic(obj) && !self.is_synthetic(index) {
                    // Comments after the `[` trail it: `x[ /* c */0]`.
                    let (from, to) = (self.span(obj).end, self.span(index).start);
                    if let Some(p) = self.token_between(from, to.max(from), "[") {
                        self.expr_trailing(p + 1);
                    }
                }
                let saved = self.no_in;
                self.no_in = false;
                self.expr(index, Prec::Lowest);
                self.no_in = saved;
                self.tok("]");
            }
            Node::MetaProperty { kind } => {
                self.map_node(id);
                let text = match kind {
                    MetaPropKind::NewTarget => "new.target",
                    MetaPropKind::ImportMeta => "import.meta",
                };
                // tsc prints a misspelt meta property as written (`import.metal`).
                let raw = self.raw(id).filter(|r| {
                    self.tsc
                        && r.len() <= 64
                        && r.bytes()
                            .all(|b| b == b'.' || b == b'_' || b.is_ascii_alphanumeric())
                });
                let text = raw.unwrap_or(text);
                // tsc maps the name after the `.` (`target` of `new.target`).
                match text.find('.') {
                    Some(dot) if self.tsc_maps && !self.is_synthetic(id) => {
                        self.tok(&text[..=dot]);
                        let at = self.span(id).start + dot as u32 + 1;
                        if self.src.as_bytes().get(at as usize - 1) == Some(&b'.') {
                            self.map_pos(at);
                        }
                        self.raw_append(&text[dot + 1..]);
                    }
                    _ => self.tok(text),
                }
            }
            Node::ImportCall {
                phase,
                arg,
                options,
            } => {
                self.map_node(id);
                self.tok(match phase {
                    ImportPhase::Evaluation => "import",
                    ImportPhase::Defer => "import.defer",
                    ImportPhase::Source => "import.source",
                });
                self.tok("(");
                let saved = self.no_in;
                self.no_in = false;
                if self.kind(arg) == NodeKind::StringLit {
                    let start = self.pos();
                    self.expr(arg, Prec::Assign);
                    let end = self.pos();
                    if end >= start + 2 {
                        self.record_specifier(SpecifierKind::Dynamic, start + 1, end - 1);
                    }
                } else {
                    self.expr(arg, Prec::Assign);
                }
                if let Some(o) = options {
                    self.tok(",");
                    self.space();
                    self.expr(o, Prec::Assign);
                }
                self.no_in = saved;
                self.tok(")");
            }
            Node::As { expr, ty } | Node::Satisfies { expr, ty } => {
                self.expr(expr, Prec::Relational);
                self.hard_space();
                self.tok(if matches!(n, Node::As { .. }) {
                    "as"
                } else {
                    "satisfies"
                });
                self.hard_space();
                self.ty(ty);
            }
            Node::TypeAssertion { ty, expr } => {
                self.tok("<");
                self.ty(ty);
                self.tok(">");
                self.expr(expr, Prec::Unary);
            }
            Node::NonNull { expr } => {
                self.member_object(expr);
                self.tok("!");
            }
            Node::Instantiation { expr, type_args } => {
                self.member_object(expr);
                self.ty(type_args);
            }
            // Patterns can appear where expressions are printed (assignment targets).
            Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. }
            | Node::AssignPattern { .. }
            | Node::Rest { .. }
            | Node::BindingProperty { .. } => self.pattern(id),
            Node::JsxElement { .. }
            | Node::JsxFragment { .. }
            | Node::JsxText { .. }
            | Node::JsxExprContainer { .. }
            | Node::JsxSpreadChild { .. } => self.jsx(id),
            Node::ExprWithTypeArgs { expr, type_args } => {
                self.expr(expr, Prec::Call);
                if let Some(ta) = type_args {
                    self.ty(ta);
                }
            }
            Node::Missing {} => {}
            _ => {
                if crate::types::is_type_node(n.kind()) {
                    self.ty(id);
                } else if n.is_statement() {
                    // Statement in expression position: should not happen.
                    self.statement(id);
                }
            }
        }
    }

    pub(crate) fn string_value(&mut self, v: &str) {
        let mut s = String::with_capacity(v.len() + 2);
        quote_string(v, best_quote(v), &mut s);
        self.tok(&s);
    }

    pub(crate) fn ident(&mut self, id: NodeId) {
        self.map_start(id);
        self.ident_inner(id);
        self.map_end(id);
    }

    fn ident_inner(&mut self, id: NodeId) {
        match self.node(id) {
            Node::Ident { name } => {
                let text = self.ast.atom(name);
                // The source text outlives the printer: no copy of the original name.
                let renamed = self
                    .raw(id)
                    .filter(|raw| *raw != text && !raw.contains('\\'));
                if let Some(raw) = renamed {
                    self.map_span_named(self.span(id), Some(raw));
                } else {
                    self.map_node(id);
                }
                match self.escaped_raw(id, text) {
                    Some(raw) => self.tok(raw),
                    None => self.tok(text),
                }
            }
            Node::Missing {} => {}
            _ => self.expr(id, Prec::Primary),
        }
    }

    /// An IdentifierName after `.` (keywords allowed).
    fn ident_name(&mut self, id: NodeId) {
        self.ident(id);
    }

    fn binary(&mut self, id: NodeId, left: NodeId, op: BinaryOp, right: NodeId) {
        let p = binary_prec(op);
        let wrap_in = self.no_in && op == BinaryOp::In;
        if wrap_in {
            self.no_in = false;
            self.tok("(");
        }
        // `a ?? b || c` mixes are syntax errors without parentheses.
        let mixes = |s: &Self, child: NodeId| -> bool {
            match s.node(child) {
                Node::Binary { op: cop, .. } => match op {
                    BinaryOp::Coalesce => {
                        matches!(cop, BinaryOp::LogicalOr | BinaryOp::LogicalAnd)
                    }
                    BinaryOp::LogicalOr | BinaryOp::LogicalAnd => cop == BinaryOp::Coalesce,
                    _ => false,
                },
                _ => false,
            }
        };
        if op == BinaryOp::Exp {
            // Right-associative; a unary operand on the left is a syntax error.
            let lp = self.prec(left);
            if lp <= Prec::Unary {
                self.paren(left);
            } else {
                self.expr(left, Prec::Update);
            }
        } else if mixes(self, left) {
            self.paren(left);
        } else {
            self.expr(left, p);
        }
        let (before, after) = self.op_breaks(left, right, op.as_str());
        let mut breaks = self.break_or_space(before);
        // The operator's own comments: `/* c */ in /* d */`.
        let op_pos = (self.tsc && !self.is_synthetic(left) && !self.is_synthetic(right))
            .then(|| self.token_between(self.span(left).end, self.span(right).start, op.as_str()))
            .flatten();
        if let Some(p) = op_pos {
            self.expr_leading(p);
        }
        self.map_node(id);
        self.tok(op.as_str());
        if let Some(p) = op_pos {
            self.expr_trailing(p + op.as_str().len() as u32);
        }
        breaks += self.break_or_space(after);
        let rmin = if op == BinaryOp::Exp {
            Prec::Exponent
        } else {
            next_prec(p)
        };
        if mixes(self, right) {
            self.paren(right);
        } else {
            self.expr(right, rmin);
        }
        self.dedent_n(breaks);
        if wrap_in {
            self.tok(")");
            self.no_in = true;
        }
    }

    fn paren(&mut self, id: NodeId) {
        let saved = self.no_in;
        self.no_in = false;
        self.tok("(");
        self.expr(id, Prec::Lowest);
        self.tok(")");
        self.no_in = saved;
    }

    /// Callee of a call (`new_callee`: of `new`, which may not contain a call).
    fn callee(&mut self, id: NodeId, new_callee: bool) {
        if new_callee {
            if self.prec(id) < Prec::Call || self.contains_call_in_chain(id) {
                self.paren(id);
            } else {
                self.expr(id, Prec::Call);
            }
        } else {
            self.expr(id, Prec::Call);
        }
    }

    /// `new a.b().c` would bind the call to `new`: parenthesize callees with calls or
    /// optional chains on their spine.
    fn contains_call_in_chain(&self, mut id: NodeId) -> bool {
        loop {
            match self.node(id) {
                Node::Call { .. } | Node::ImportCall { .. } => return true,
                Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                    if optional {
                        return true;
                    }
                    id = obj;
                }
                Node::TaggedTemplate { tag, .. } => id = tag,
                Node::NonNull { expr } | Node::Instantiation { expr, .. } => id = expr,
                _ => return false,
            }
        }
    }

    /// Object of `.`/`[]`: needs member precedence; integer literals need a dot or parens.
    fn member_object(&mut self, id: NodeId) {
        if let Node::NumberLit { value } = self.node(id) {
            let by_value = self.raw(id).is_some_and(|r| self.strip_separators(r));
            match self.raw(id).filter(|_| !by_value) {
                Some(raw) => {
                    let plain = raw.bytes().all(|b| b.is_ascii_digit() || b == b'_')
                        && !(raw.len() > 1 && raw.starts_with('0'));
                    self.map_node(id);
                    self.tok(raw);
                    if plain {
                        self.tok(".");
                    } else if raw.len() > 1
                        && raw.starts_with('0')
                        && raw.bytes().all(|b| b.is_ascii_digit())
                    {
                        // Legacy octal-like literal: a space keeps `.` separate.
                        self.push_space();
                    }
                }
                None => {
                    let v = value.get();
                    if v.is_finite() && v >= 0.0 {
                        let s = js_number(v);
                        let comments = self.ast.synthetic_comments_of(id);
                        for c in comments.iter().filter(|c| !c.trailing) {
                            self.synthetic_comment(self.ast.atom(c.text), false);
                        }
                        self.map_node(id);
                        self.tok(&s);
                        // `0 /* E.A */.toString()`: a trailing comment already separates
                        // the `.` from the number (tsc).
                        let mut separated = false;
                        for c in comments.iter().filter(|c| c.trailing) {
                            separated |= self.synthetic_comment(self.ast.atom(c.text), true);
                        }
                        if !separated && !s.contains(['.', 'e']) {
                            self.tok(".");
                        }
                    } else {
                        self.paren(id);
                    }
                }
            }
            return;
        }
        if matches!(
            self.node(id),
            Node::New {
                has_args: false,
                ..
            }
        ) {
            self.paren(id);
            return;
        }
        self.expr(id, Prec::Call);
    }

    fn push_space(&mut self) {
        self.hard_space();
    }

    /// The arguments of the call `call` to `callee`.
    /// `?.` after the original `obj`: tsc maps the token's start and end.
    fn question_dot(&mut self, obj: NodeId, to: u32) {
        let at = if self.tsc_maps && !self.is_synthetic(obj) {
            let from = self.span(obj).end;
            self.token_between(from, to.max(from), "?.")
        } else {
            None
        };
        self.map_token("?.", at);
    }

    fn args_after(&mut self, args: ts_r_ast::NodeList, call: NodeId, callee: NodeId) {
        if self.tsc && args.is_empty() && !self.is_synthetic(call) && !self.is_synthetic(callee) {
            // Comments in empty arguments trail the `(`: `f( /* c */)`.
            self.tok("(");
            let (from, to) = (self.span(callee).end, self.span(call).end);
            if let Some(p) = self.token_between(from, to.max(from), "(") {
                self.expr_trailing(p + 1);
            }
            self.tok(")");
            return;
        }
        self.args(args);
    }

    fn args(&mut self, args: ts_r_ast::NodeList) {
        self.tok("(");
        let saved = self.no_in;
        self.no_in = false;
        let items = self.ast.list(args).to_vec();
        for (i, a) in items.iter().enumerate() {
            if i > 0 {
                self.tok(",");
                self.comma_comments(items[i - 1], *a);
                self.space();
            }
            self.arg(*a);
        }
        self.no_in = saved;
        self.tok(")");
    }

    fn template(&mut self, parts: ts_r_ast::NodeList) {
        self.tok("`");
        let saved = self.no_in;
        self.no_in = false;
        let items = self.ast.list(parts).to_vec();
        for (i, &p) in items.iter().enumerate() {
            match self.node(p) {
                Node::TemplateElement { cooked, .. } => match self.raw(p) {
                    Some(raw) => self.push_raw(raw),
                    None => {
                        let mut s = String::new();
                        template_raw(self.ast.atom(cooked), &mut s);
                        self.push_raw(&s);
                    }
                },
                _ => {
                    self.push_raw("${");
                    self.expr(p, Prec::Lowest);
                    // tsc maps the start of the middle or tail (its `}`).
                    if self.tsc_maps
                        && let Some(&next) = items.get(i + 1)
                    {
                        let at = self.ast.span(next).start;
                        if at > 0 && self.src.as_bytes().get(at as usize - 1) == Some(&b'}') {
                            self.map_pos(at - 1);
                        }
                    }
                    self.push_raw("}");
                }
            }
        }
        self.no_in = saved;
        self.push_raw("`");
    }

    /// Writes text with no spacing logic (template content, JSX).
    fn push_raw(&mut self, s: &str) {
        self.raw_append(s);
    }

    /// Whether the single simple parameter `param` of the arrow `id` prints without
    /// parentheses: as written (`x => x`), or (tsc layout) when both are synthesized. tsc
    /// always parenthesizes an `async` arrow's parameter.
    fn bare_arrow_param(&self, id: NodeId, param: NodeId, flags: FnFlags) -> bool {
        if self.tsc && flags.contains(FnFlags::ASYNC) {
            return false;
        }
        match (self.is_synthetic(param), self.is_synthetic(id)) {
            (true, true) => self.tsc,
            (false, false) => {
                let (start, p) = (
                    self.span(id).start as usize,
                    self.span(param).start as usize,
                );
                self.src.as_bytes().get(p).is_some_and(|&b| b != b'(')
                    && self.src.get(start..p).is_some_and(|s| !s.contains('('))
            }
            _ => false,
        }
    }

    fn arrow(
        &mut self,
        id: NodeId,
        flags: FnFlags,
        type_params: Option<NodeId>,
        params: ts_r_ast::NodeList,
        return_type: Option<NodeId>,
        body: NodeId,
    ) {
        if flags.contains(FnFlags::ASYNC) {
            let start = self.span(id).start;
            self.set_mod_src(id, start, start + 5);
            self.modifier_kw("async");
            self.mod_src = None;
            self.hard_space();
        }
        let ps = self.ast.list(params).to_vec();
        // Keep `x => x` without parentheses when written that way (always in compact layout).
        let bare = type_params.is_none()
            && return_type.is_none()
            && ps.len() == 1
            && matches!(
                self.node(ps[0]),
                Node::Param { decorators, modifiers, rest: false, optional: false, type_ann: None, init: None, pattern }
                    if decorators.is_empty() && modifiers.is_empty() && self.kind(pattern) == NodeKind::Ident
            )
            && (self.compact || self.bare_arrow_param(id, ps[0], flags));
        if bare {
            self.param(ps[0]);
        } else if let Some(tp) = type_params
            && self.ast.source_kind == ts_r_ast::SourceKind::Tsx
            && let Node::TypeParams { params: tps } = self.node(tp)
            && let [only] = *self.ast.list(tps)
            && matches!(
                self.node(only),
                Node::TypeParam {
                    constraint: None,
                    ..
                }
            )
        {
            // `<T>(x) => x` would read as a JSX tag in a .tsx file.
            self.tok("<");
            self.ty(only);
            self.tok(",>");
            self.signature(None, params, return_type);
        } else {
            self.signature(type_params, params, return_type);
        }
        self.space();
        // tsc maps `=>`.
        // A body a transform rebuilt maps through its range.
        let body_start = self
            .map_range_of(body)
            .map(|r| r.start)
            .filter(|&s| s != crate::NO_MAP);
        let arrow_at = if self.tsc_maps
            && !self.is_synthetic(id)
            && let Some(body_start) = body_start
        {
            let from = ps
                .last()
                .copied()
                .into_iter()
                .chain(return_type)
                .chain(type_params)
                .filter(|&n| !self.is_synthetic(n))
                .map(|n| self.span(n).end)
                .max()
                .unwrap_or(self.span(id).start);
            self.token_between(from, body_start, "=>")
        } else {
            None
        };
        self.map_token("=>", arrow_at);
        self.space();
        if flags.contains(FnFlags::EXPRESSION_BODY) || self.kind(body) != NodeKind::Block {
            let saved = self.no_in;
            self.no_in = false;
            if self.leftmost(body) == Leftmost::Brace {
                self.paren(body);
            } else {
                self.expr(body, Prec::Assign);
            }
            self.no_in = saved;
        } else {
            let saved = self.no_in;
            self.no_in = false;
            self.function_body(body);
            self.no_in = saved;
        }
    }

    /// `[a, , ...b]` for array literals and patterns.
    fn array_like(&mut self, id: NodeId, elements: ts_r_ast::NodeList, pattern: bool) {
        if self.tsc {
            return self.tsc_list(id, elements, pattern, false);
        }
        let items = self.ast.list(elements).to_vec();
        let saved = self.no_in;
        self.no_in = false;
        let multiline = !self.compact && self.source_multiline(id, items.first().copied());
        self.tok("[");
        if multiline {
            self.indent();
            self.newline();
        }
        for (i, e) in items.iter().enumerate() {
            if i > 0 {
                self.tok(",");
                if multiline {
                    self.newline();
                } else if self.kind(*e) != NodeKind::Hole || self.tsc {
                    // tsc: `[, b, , a]`.
                    self.space();
                }
            }
            if pattern {
                self.pattern(*e);
            } else {
                match self.node(*e) {
                    Node::Spread { arg } => {
                        self.map_node(*e);
                        self.dots(arg);
                        self.expr(arg, Prec::Assign);
                    }
                    Node::Hole {} => {}
                    _ => self.arg(*e),
                }
            }
        }
        // A trailing hole needs an extra comma: `[a, ,]`.
        if items
            .last()
            .is_some_and(|&e| self.kind(e) == NodeKind::Hole)
        {
            self.tok(",");
        }
        if multiline {
            self.dedent();
            self.newline();
        }
        self.tok("]");
        self.no_in = saved;
    }

    /// An object or array literal (or pattern) in [`crate::Layout::Tsc`]: line breaks after
    /// the open token, between elements and before the close token are kept from the source
    /// (a break after the open token also breaks before the close one); patterns stay on one
    /// line. Elements are indented one level whether or not the list breaks, and a trailing
    /// comma in the source is kept. Observed in tsc 7.
    fn tsc_list(&mut self, id: NodeId, list: ts_r_ast::NodeList, pattern: bool, object: bool) {
        let items = self.ast.list(list).to_vec();
        let saved = self.no_in;
        self.no_in = false;
        let (open, close) = if object { ("{", "}") } else { ("[", "]") };
        self.tok(open);
        if items.is_empty() {
            self.tok(close);
            self.no_in = saved;
            return;
        }
        let orig = !self.is_synthetic(id);
        let span = self.span(id);
        let close_pos = span.end.saturating_sub(1);
        let first = items[0];
        let last = items[items.len() - 1];
        let breaks = orig && !pattern;
        let every = !orig && self.hint_multi_line(id);
        let nl_open = every
            || breaks
                && !self.is_synthetic(first)
                && self.has_newline_between(span.start + 1, self.span(first).start);
        let last_hole = self.kind(last) == NodeKind::Hole;
        let nl_close = nl_open
            || (breaks
                && !last_hole
                && !self.is_synthetic(last)
                && self.has_newline_between(self.span(last).end, close_pos));
        let trailing_comma = orig
            && !last_hole
            && !self.is_synthetic(last)
            && self.has_comma_between(self.span(last).end, close_pos);
        // Binding patterns stay on one line and add no indentation (a class in a computed
        // key keeps the outer level); literals indent their elements.
        let indented = !pattern || nl_open;
        if indented {
            self.indent();
        }
        if object && nl_open {
            // tsc drops a comment after a multi-line object's `{` on its line.
            self.drop_same_line_comments(span.start + 1);
        }
        if nl_open {
            self.newline();
        } else if object {
            self.space();
        }
        for (i, e) in items.iter().enumerate() {
            if i > 0 {
                self.tok(",");
                let prev = items[i - 1];
                if self.kind(prev) != NodeKind::Hole && self.kind(*e) != NodeKind::Hole {
                    self.comma_comments(prev, *e);
                }
                let nl = every
                    || breaks
                        && !self.is_synthetic(prev)
                        && !self.is_synthetic(*e)
                        && self.kind(*e) != NodeKind::Hole
                        && self.has_newline_between(self.span(prev).end, self.span(*e).start);
                if nl {
                    self.newline();
                } else {
                    self.space();
                }
            }
            if pattern {
                self.pattern(*e);
            } else if object {
                self.object_member(*e);
            } else {
                match self.node(*e) {
                    Node::Spread { arg } => {
                        if self.tsc && !self.is_synthetic(*e) {
                            self.expr_leading(self.span(*e).start);
                        }
                        self.map_node(*e);
                        self.map_start(*e);
                        self.dots(arg);
                        self.expr(arg, Prec::Assign);
                        self.map_end(*e);
                    }
                    Node::Hole {} => self.hole(*e),
                    _ => self.arg(*e),
                }
            }
        }
        // A trailing hole needs an extra comma: `[a, ,]`.
        if last_hole || trailing_comma {
            self.tok(",");
        }
        if trailing_comma && let Some(p) = self.token_between(self.span(last).end, close_pos, ",") {
            // `x * 2, // c` keeps its comment on the line, after a space.
            self.expr_trailing(p + 1);
        }
        if nl_close {
            // Comments before the close token, one level in.
            self.expr_leading(close_pos);
        }
        if indented {
            self.dedent();
        }
        if nl_close {
            self.newline();
        } else if object {
            self.space();
        }
        self.tok(close);
        self.no_in = saved;
    }

    /// [`Printer::tsc_maps`]: the position of `tok` written between the original nodes `a`
    /// and `b`.
    fn token_after(&self, a: NodeId, b: NodeId, tok: &str) -> Option<u32> {
        if !self.tsc_maps || self.is_synthetic(a) || self.is_synthetic(b) {
            return None;
        }
        let (from, to) = (self.span(a).end, self.span(b).start);
        self.token_between(from, to.max(from), tok)
    }

    /// An elision (`[a, , b]`). tsc maps its start (after the preceding `,` and trivia) and
    /// then its end, which is where its trivia starts: one segment back.
    fn hole(&mut self, id: NodeId) {
        // The parser puts an elision at the `,` (or `]`) that ends it.
        let at = self.span(id).start;
        let b = self.src.as_bytes();
        if !self.tsc_maps || at == 0 || !matches!(b.get(at as usize), Some(b',' | b']')) {
            return;
        }
        let mut end = at as usize;
        while end > 0 && b[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        self.map_pos(at);
        self.map_pos(end as u32);
    }

    /// The source between `from` and `to` has a `,` outside comments.
    pub(crate) fn has_comma_between(&self, from: u32, to: u32) -> bool {
        self.token_between(from, to, ",").is_some()
    }

    /// Position of the first `tok` in the source between `from` and `to`, skipping
    /// comments (the range should hold only trivia and the token).
    pub(crate) fn token_between(&self, from: u32, to: u32, tok: &str) -> Option<u32> {
        let text = self.src.get(from as usize..to as usize)?;
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            if text.is_char_boundary(i) && text[i..].starts_with(tok) {
                return Some(from + i as u32);
            }
            i += 1;
        }
        None
    }

    /// True if the original `[`/`{` was followed by a line break before the first item.
    fn source_multiline(&self, id: NodeId, first: Option<NodeId>) -> bool {
        let (Some(first), false) = (first, self.is_synthetic(id)) else {
            return false;
        };
        if self.is_synthetic(first) {
            return false;
        }
        self.has_newline_between(self.span(id).start, self.span(first).start)
    }

    fn object_like(&mut self, id: NodeId, props: ts_r_ast::NodeList, pattern: bool) {
        if self.tsc {
            return self.tsc_list(id, props, pattern, true);
        }
        let items = self.ast.list(props).to_vec();
        let saved = self.no_in;
        self.no_in = false;
        self.tok("{");
        if items.is_empty() {
            self.tok("}");
            self.no_in = saved;
            return;
        }
        let multiline =
            !self.compact && !pattern && self.source_multiline(id, items.first().copied());
        if multiline {
            self.indent();
        } else {
            self.space();
        }
        for (i, p) in items.iter().enumerate() {
            if i > 0 {
                self.tok(",");
                if !multiline {
                    self.space();
                }
            }
            if multiline {
                if !self.is_synthetic(*p) {
                    self.leading_comments(self.span(*p).start);
                }
                self.newline();
            }
            if pattern {
                self.pattern(*p);
            } else {
                self.object_member(*p);
            }
        }
        if multiline {
            self.dedent();
            self.newline();
        } else {
            self.space();
        }
        self.tok("}");
        self.no_in = saved;
    }

    fn object_member(&mut self, id: NodeId) {
        let orig = self.tsc && !self.is_synthetic(id);
        if orig {
            self.expr_leading(self.span(id).start);
        }
        self.map_start(id);
        self.object_member_inner(id);
        self.map_end(id);
        if orig {
            self.expr_trailing(self.span(id).end);
        }
    }

    fn object_member_inner(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::Property {
                modifiers,
                key,
                value,
            } => {
                self.object_property_modifiers(modifiers);
                self.property_key(key);
                self.tok(":");
                self.space();
                self.arg(value);
            }
            Node::ShorthandProp {
                modifiers,
                name,
                init,
            } => {
                self.object_property_modifiers(modifiers);
                self.ident(name);
                if let Some(init) = init {
                    self.space();
                    self.tok("=");
                    self.space();
                    self.arg(init);
                }
            }
            Node::Spread { arg } => {
                self.dots(arg);
                self.arg(arg);
            }
            Node::MethodDef { .. } => self.method(id),
            _ => self.arg(id),
        }
    }

    /// Misplaced modifiers on an object literal property (`{ public a: 1 }`): printed in
    /// TypeScript output; tsc 7's JavaScript output has none.
    fn object_property_modifiers(&mut self, m: ts_r_ast::Modifiers) {
        if !self.js {
            self.ts_modifiers(m);
        }
    }

    /// A property name: identifier, string, number, computed key or private name.
    pub(crate) fn property_key(&mut self, id: NodeId) {
        self.map_start(id);
        self.property_key_inner(id);
        self.map_end(id);
        if self.tsc && !self.is_synthetic(id) {
            self.expr_trailing(self.span(id).end);
        }
    }

    fn property_key_inner(&mut self, id: NodeId) {
        match self.node(id) {
            Node::Ident { .. } => self.ident(id),
            Node::ComputedKey { expr } => {
                self.map_node(id);
                self.tok("[");
                let saved = self.no_in;
                self.no_in = false;
                self.expr(expr, Prec::Assign);
                self.no_in = saved;
                self.tok("]");
            }
            Node::NumberLit { value } if self.is_synthetic(id) && value.get() < 0.0 => {
                self.tok("[");
                self.expr(id, Prec::Assign);
                self.tok("]");
            }
            _ => self.expr(id, Prec::Primary),
        }
    }

    /// The left side of `=`: expression targets or patterns.
    fn assign_target(&mut self, id: NodeId) {
        match self.node(id) {
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => {
                self.map_start(id);
                self.expr_inner(id);
                self.map_end(id);
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => self.pattern(id),
            _ => self.expr(id, Prec::LeftHandSide),
        }
    }

    /// Binding patterns.
    pub(crate) fn pattern(&mut self, id: NodeId) {
        let orig = self.tsc && !self.is_synthetic(id);
        if orig {
            self.expr_leading(self.span(id).start);
        }
        self.map_start(id);
        self.pattern_inner(id);
        self.map_end(id);
        if orig {
            self.expr_trailing(self.span(id).end);
        }
    }

    fn pattern_inner(&mut self, id: NodeId) {
        match self.node(id) {
            Node::ObjectPattern { props } => {
                self.map_node(id);
                self.object_like(id, props, true);
            }
            Node::ArrayPattern { elements } => {
                self.map_node(id);
                self.array_like(id, elements, true);
            }
            Node::BindingProperty { key, value, init } => {
                self.map_node(id);
                match value {
                    Some(v) => {
                        self.property_key(key);
                        self.tok(":");
                        self.space();
                        self.pattern(v);
                    }
                    None => self.property_key(key),
                }
                if let Some(init) = init {
                    self.space();
                    self.tok("=");
                    self.space();
                    self.arg(init);
                }
            }
            Node::AssignPattern { left, right } => {
                self.pattern(left);
                self.space();
                self.tok("=");
                self.space();
                self.arg(right);
            }
            Node::Rest { arg } => {
                self.map_node(id);
                self.dots(arg);
                self.pattern(arg);
            }
            Node::Hole {} => self.hole(id),
            Node::Ident { .. } => self.ident(id),
            Node::Param { .. } => self.param(id),
            _ => self.expr(id, Prec::LeftHandSide),
        }
    }

    // ---- statement-start ambiguities ----

    /// The leftmost token kind of `id` as it will be printed.
    fn leftmost(&self, mut id: NodeId) -> Leftmost {
        loop {
            let (child, min) = match self.node(id) {
                Node::ObjectLit { .. } | Node::ObjectPattern { .. } => return Leftmost::Brace,
                Node::FunctionExpr { .. } => return Leftmost::Function,
                Node::ClassExpr { decorators, .. } => {
                    return if decorators.is_empty() {
                        Leftmost::Class
                    } else {
                        Leftmost::Other
                    };
                }
                Node::Ident { name } => {
                    return if self.ast.atom(name) == "let" {
                        Leftmost::Let
                    } else {
                        Leftmost::Other
                    };
                }
                Node::Call { callee, .. } => (callee, Prec::Call),
                Node::Member { obj, .. } | Node::Index { obj, .. } => {
                    if matches!(self.node(obj), Node::NumberLit { .. }) {
                        return Leftmost::Other;
                    }
                    (obj, Prec::Call)
                }
                Node::TaggedTemplate { tag, .. } => (tag, Prec::Call),
                Node::NonNull { expr } | Node::Instantiation { expr, .. } => (expr, Prec::Call),
                Node::Binary { left, op, .. } => (left, binary_prec(op)),
                Node::As { expr, .. } | Node::Satisfies { expr, .. } => (expr, Prec::Relational),
                Node::Assign { left, .. } => match self.node(left) {
                    Node::ObjectLit { .. } | Node::ObjectPattern { .. } => return Leftmost::Brace,
                    _ => (left, Prec::LeftHandSide),
                },
                Node::Conditional { test, .. } => (test, Prec::Coalesce),
                Node::Seq { exprs } => match self.ast.list(exprs).first() {
                    Some(&f) => (f, Prec::Assign),
                    None => return Leftmost::Other,
                },
                Node::Update {
                    prefix: false, arg, ..
                } => (arg, Prec::LeftHandSide),
                _ => return Leftmost::Other,
            };
            if self.prec(child) < min {
                return Leftmost::Other;
            }
            id = child;
        }
    }

    /// Whether an expression statement (or `export default` expression) needs parentheses.
    pub(crate) fn starts_ambiguously(&self, id: NodeId, statement: bool) -> bool {
        match self.leftmost(id) {
            Leftmost::Brace => statement,
            Leftmost::Function | Leftmost::Class => {
                // `export default function () {}` is a declaration; only wrap when the
                // function/class is not the whole expression.
                statement
                    || !matches!(
                        self.node(id),
                        Node::FunctionExpr { .. } | Node::ClassExpr { .. }
                    )
            }
            Leftmost::Let => {
                statement && matches!(self.node(id), Node::Index { .. }) || self.let_bracket(id)
            }
            Leftmost::Other => false,
        }
    }

    fn let_bracket(&self, id: NodeId) -> bool {
        // `let [` at statement start: any chain whose leftmost `let` is followed by `[`.
        let mut cur = id;
        loop {
            match self.node(cur) {
                Node::Index { obj, .. } => {
                    if matches!(self.node(obj), Node::Ident { name } if self.ast.atom(name) == "let")
                    {
                        return true;
                    }
                    cur = obj;
                }
                Node::Member { obj, .. } | Node::Call { callee: obj, .. } => cur = obj,
                Node::Binary { left, .. } | Node::Assign { left, .. } => cur = left,
                _ => return false,
            }
        }
    }

    pub(crate) fn starts_with_let(&self, id: NodeId) -> bool {
        self.leftmost(id) == Leftmost::Let
    }

    // ---- JSX ----

    pub(crate) fn jsx(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::JsxElement {
                opening,
                children,
                closing,
            } => {
                if let Node::JsxOpening {
                    name,
                    type_args,
                    attrs,
                    self_closing,
                } = self.node(opening)
                {
                    self.raw_append("<");
                    self.jsx_name(name);
                    if let Some(ta) = type_args {
                        self.ty(ta);
                    }
                    let attrs = self.ast.list(attrs).to_vec();
                    for &a in &attrs {
                        self.raw_append(" ");
                        self.jsx_attr(a);
                    }
                    if self_closing {
                        // tsc: `<div />`, but `<div a={1}/>`.
                        self.raw_append(if self.tsc && !attrs.is_empty() {
                            "/>"
                        } else {
                            " />"
                        });
                    } else {
                        self.raw_append(">");
                    }
                }
                for c in self.ast.list(children).to_vec() {
                    self.jsx_child(c);
                }
                if let Some(cl) = closing
                    && let Node::JsxClosing { name } = self.node(cl)
                {
                    self.raw_append("</");
                    self.jsx_name(name);
                    self.raw_append(">");
                }
            }
            Node::JsxFragment { children } => {
                self.raw_append("<>");
                for c in self.ast.list(children).to_vec() {
                    self.jsx_child(c);
                }
                self.raw_append("</>");
            }
            _ => self.jsx_child(id),
        }
    }

    fn jsx_child(&mut self, id: NodeId) {
        match self.node(id) {
            Node::JsxText {} => {
                let raw = self.raw(id).unwrap_or("");
                self.raw_append(raw);
            }
            Node::JsxExprContainer { expr } => {
                self.raw_append("{");
                if let Some(e) = expr {
                    // tsc indents what an expression container breaks onto later lines one
                    // level deeper (attribute values and children, not spread attributes).
                    if self.tsc {
                        self.indent();
                    }
                    self.expr(e, Prec::Assign);
                    if self.tsc {
                        self.dedent();
                    }
                } else if !self.is_synthetic(id) {
                    // Keep `{/* comment */}` children.
                    let span = self.span(id);
                    let texts: Vec<(u32, String)> = self
                        .ast
                        .comments_in(span)
                        .iter()
                        .filter(|c| c.kind == ts_r_ast::CommentKind::Block)
                        .map(|c| {
                            let t = &self.src[c.span.start as usize..c.span.end as usize];
                            (c.span.start, t.to_owned())
                        })
                        .collect();
                    if self.opts.comments {
                        for (start, t) in texts {
                            if self.tsc && t.contains('\n') {
                                // Indented as an expression container's contents are.
                                self.indent();
                                self.writer_indented_comment(start, &t);
                                self.dedent();
                            } else {
                                self.raw_append(&t);
                            }
                        }
                    }
                }
                self.raw_append("}");
            }
            Node::JsxSpreadChild { expr } => {
                self.raw_append("{...");
                self.expr(expr, Prec::Assign);
                self.raw_append("}");
            }
            Node::JsxElement { .. } | Node::JsxFragment { .. } => self.jsx(id),
            _ => self.expr(id, Prec::Assign),
        }
    }

    fn jsx_name(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::Ident { name } => {
                let s = self.ast.atom(name).to_owned();
                self.raw_append(&s);
            }
            Node::This {} => self.raw_append("this"),
            Node::JsxNamespacedName { ns, name } => {
                self.jsx_name(ns);
                self.raw_append(":");
                self.jsx_name(name);
            }
            Node::JsxMemberExpr { obj, prop } => {
                self.jsx_name(obj);
                self.raw_append(".");
                self.jsx_name(prop);
            }
            Node::Member { obj, prop, .. } => {
                self.jsx_name(obj);
                self.raw_append(".");
                self.jsx_name(prop);
            }
            _ => self.expr(id, Prec::Primary),
        }
    }

    /// Whether `first` is the zero-width `Missing` the parser puts before adjacent JSX
    /// elements it groups into a sequence (`<a/><b/>`).
    pub(crate) fn is_jsx_sibling_marker(&self, first: NodeId, second: NodeId) -> bool {
        let at = self.ast.span(first);
        matches!(self.node(first), Node::Missing {})
            && at.is_empty()
            && (matches!(
                self.node(second),
                Node::JsxElement { .. } | Node::JsxFragment { .. }
            ) || self.ast.span(second).start == at.start)
    }

    fn jsx_attr(&mut self, id: NodeId) {
        self.map_node(id);
        match self.node(id) {
            Node::JsxAttr { name, value } => {
                self.jsx_name(name);
                if let Some(v) = value {
                    self.raw_append("=");
                    match self.node(v) {
                        Node::StringLit { value } => match self.raw(v) {
                            Some(raw) => self.raw_append(raw),
                            None => {
                                // JSX attribute strings have no escapes.
                                let s = self.ast.atom(value);
                                let q = if s.contains('"') { '\'' } else { '"' };
                                let text = format!("{q}{s}{q}");
                                self.raw_append(&text);
                            }
                        },
                        Node::JsxExprContainer { .. } => self.jsx_child(v),
                        // `a=<b/><c/>` (TS2657): tsc prints the sequence bare, `a=<b />, <c />`.
                        Node::Seq { .. } => self.expr(v, Prec::Lowest),
                        _ => self.jsx(v),
                    }
                }
            }
            Node::JsxSpreadAttr { arg } => {
                self.raw_append("{...");
                self.expr(arg, Prec::Assign);
                self.raw_append("}");
            }
            _ => {}
        }
    }
}

/// The next-tighter precedence (for the right operand of left-associative operators).
fn next_prec(p: Prec) -> Prec {
    use Prec::*;
    match p {
        Lowest => Assign,
        Assign => Conditional,
        Conditional => Coalesce,
        Coalesce => LogicalOr,
        LogicalOr => LogicalAnd,
        LogicalAnd => BitOr,
        BitOr => BitXor,
        BitXor => BitAnd,
        BitAnd => Equality,
        Equality => Relational,
        Relational => Shift,
        Shift => Additive,
        Additive => Multiplicative,
        Multiplicative => Exponent,
        Exponent => Unary,
        Unary => Update,
        Update => LeftHandSide,
        LeftHandSide => Call,
        Call => Primary,
        Primary => Primary,
    }
}

/// A bigint literal as tsc writes it: separators dropped, binary and octal in decimal, hex
/// in lower case (a missing digit becomes `0`). Text that is not a plain bigint (an
/// exponent or fraction after a parse error) is kept.
fn tsc_bigint(raw: &str) -> String {
    let Some(body) = raw.strip_suffix('n') else {
        return raw.to_owned();
    };
    let digits: String = body.chars().filter(|&c| c != '_').collect();
    let lower = digits.to_ascii_lowercase();
    let radix = match lower.get(..2) {
        Some("0x") => 16,
        Some("0o") => 8,
        Some("0b") => 2,
        _ => 10,
    };
    if radix == 10 {
        if !lower.bytes().all(|b| b.is_ascii_digit()) {
            return raw.to_owned();
        }
        return format!("{digits}n");
    }
    let rest = &lower[2..];
    if !rest.bytes().all(|b| (b as char).is_digit(radix)) {
        return raw.to_owned();
    }
    if radix == 16 {
        return format!("0x{}n", if rest.is_empty() { "0" } else { rest });
    }
    // Base conversion in limbs of 10^9, least significant first.
    let mut limbs: Vec<u64> = vec![0];
    for b in rest.bytes() {
        let mut carry = u64::from(b - b'0');
        for limb in &mut limbs {
            let v = *limb * u64::from(radix) + carry;
            *limb = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        if carry > 0 {
            limbs.push(carry);
        }
    }
    let mut out = String::new();
    for (i, limb) in limbs.iter().rev().enumerate() {
        if i == 0 {
            out.push_str(&limb.to_string());
        } else {
            out.push_str(&format!("{limb:09}"));
        }
    }
    out.push('n');
    out
}

#[cfg(test)]
mod bigint_tests {
    use super::tsc_bigint;

    #[test]
    fn bigints_as_tsc_writes_them() {
        assert_eq!(tsc_bigint("0b101n"), "5n");
        assert_eq!(tsc_bigint("0o567n"), "375n");
        assert_eq!(tsc_bigint("0xC0Bn"), "0xc0bn");
        assert_eq!(tsc_bigint("0x000000FFn"), "0x000000ffn");
        assert_eq!(
            tsc_bigint("0o123456712345671234567n"),
            "1505852261029722487n"
        );
        assert_eq!(tsc_bigint("123_456__789n"), "123456789n");
        assert_eq!(tsc_bigint("0bn"), "0n");
        assert_eq!(tsc_bigint("0xn"), "0x0n");
        assert_eq!(tsc_bigint("1e2n"), "1e2n");
        assert_eq!(tsc_bigint("0b0n"), "0n");
    }
}
