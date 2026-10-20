//! Type nodes, parameters and names in declaration layout (tsc 7.0.2, observed): type
//! literals and mapped types always multi-line with one member per line, everything else on
//! one line; literal types from the source keep their text, synthesized ones are printed
//! from their values.

use ts_r_ast::{
    KeywordTypeKind as K, MappedModifier, MethodKind, Modifiers, Node, NodeId, NodeList,
    TypeOperatorKind,
};
use ts_r_binder::meaning;
use ts_r_types::TypeFlags;

use super::{Emitter, number_text};

/// Modifiers that make a constructor parameter a parameter property.
const PARAM_PROPERTY: Modifiers = Modifiers(
    Modifiers::PUBLIC.0
        | Modifiers::PRIVATE.0
        | Modifiers::PROTECTED.0
        | Modifiers::READONLY.0
        | Modifiers::OVERRIDE.0,
);

impl Emitter<'_, '_> {
    /// Prints a type node (from the source or synthesized).
    /// The attributes of an `import()` type: `{ with: { "resolution-mode": "import" } }`.
    fn import_type_attributes(&mut self, a: NodeId) {
        let Node::ImportAttributes { assert, entries } = *self.ast.node(a) else {
            self.w(self.raw(a));
            return;
        };
        self.w(if assert { "{ assert: {" } else { "{ with: {" });
        let entries = self.ast.list(entries).to_vec();
        for (i, &e) in entries.iter().enumerate() {
            self.w(if i == 0 { " " } else { ", " });
            if let Node::ImportAttribute { key, value } = *self.ast.node(e) {
                self.w(self.raw(key));
                self.w(": ");
                self.w(self.raw(value));
            } else {
                self.w(self.raw(e));
            }
        }
        self.w(if entries.is_empty() { "} }" } else { " } }" });
    }

    /// Prints a type node with its declaration-map marks.
    pub(super) fn type_node(&mut self, n: NodeId) {
        self.start(n);
        self.type_node_text(n);
        self.end(n);
    }

    fn type_node_text(&mut self, n: NodeId) {
        if self.type_depth >= 64 {
            self.fail(n, ts_r_checker::typenode::NotNameable::TooDeep);
            return;
        }
        self.type_depth += 1;
        self.type_node_inner(n);
        self.type_depth -= 1;
    }

    fn type_node_inner(&mut self, n: NodeId) {
        let node = *self.ast.node(n);
        match node {
            Node::KeywordType { kind } => self.w(keyword(kind)),
            Node::TypeRef { name, type_args } => {
                if !self.is_synth(n) {
                    self.reference_entity(name, meaning::TYPE);
                }
                self.entity_name(name);
                self.type_args(type_args);
            }
            Node::ThisType {} => self.w("this"),
            Node::TemplateLitType { parts } => {
                self.w("`");
                let parts = self.ast.list(parts).to_vec();
                for (i, &p) in parts.iter().enumerate() {
                    if i % 2 == 0 {
                        self.template_text(p);
                    } else {
                        self.w("${");
                        self.type_node(p);
                        self.w("}");
                    }
                }
                self.w("`");
            }
            Node::ArrayType { elem } => {
                self.type_node(elem);
                self.w("[]");
            }
            Node::TupleType { elems } => {
                let elems = self.ast.list(elems).to_vec();
                // A tuple written across lines keeps one element per line (observed).
                let multi_line = !self.is_synth(n)
                    && elems.first().is_some_and(|&e| {
                        let (a, b) = (self.ast.span(n).start, self.ast.span(e).start);
                        self.src
                            .get(a as usize..b as usize)
                            .is_some_and(|t| t.contains('\n'))
                    });
                self.w("[");
                if multi_line {
                    self.indent += 1;
                    for (i, &e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.w(",");
                        }
                        self.nl();
                        self.type_node(e);
                    }
                    self.indent -= 1;
                    self.nl();
                } else {
                    for (i, &e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.w(", ");
                        }
                        self.type_node(e);
                    }
                }
                self.w("]");
            }
            Node::NamedTupleMember {
                rest,
                name,
                optional,
                ty,
            } => {
                if rest {
                    self.w("...");
                }
                self.ident_text(name);
                if optional {
                    self.question(name);
                }
                self.w(": ");
                self.type_node(ty);
            }
            Node::OptionalType { ty } => {
                self.type_node(ty);
                self.w("?");
            }
            Node::RestType { ty } => {
                self.w("...");
                self.type_node(ty);
            }
            Node::UnionType { types } => self.type_list(types, " | "),
            Node::IntersectionType { types } => self.type_list(types, " & "),
            Node::FunctionType {
                type_params,
                params,
                return_type,
            } => {
                self.type_params(type_params);
                self.params(params, n);
                self.w(" => ");
                self.type_node(return_type);
            }
            Node::ConstructorType {
                is_abstract,
                type_params,
                params,
                return_type,
            } => {
                if is_abstract {
                    self.w("abstract ");
                }
                self.w("new ");
                self.type_params(type_params);
                self.params(params, n);
                self.w(" => ");
                self.type_node(return_type);
            }
            Node::TypeLiteral { members } => {
                let members = self.ast.list(members).to_vec();
                if members.is_empty() {
                    self.w("{}");
                    return;
                }
                let before = self.out.len();
                let marks = self.marks.len();
                self.w("{");
                self.nl();
                let opened = self.out.len();
                self.indent += 1;
                let start = if self.is_synth(n) {
                    0
                } else {
                    self.ast.span(n).start
                };
                self.type_members(&members, start);
                self.indent -= 1;
                if self.out.len() == opened {
                    // Every member was dropped.
                    self.out.truncate(before);
                    self.marks.truncate(marks);
                    self.line_start = false;
                    self.w("{}");
                    return;
                }
                self.w("}");
            }
            Node::TypeQuery {
                expr_name,
                type_args,
            } => {
                self.w("typeof ");
                if !self.is_synth(n) {
                    self.reference_entity(expr_name, meaning::VALUE);
                }
                match *self.ast.node(expr_name) {
                    Node::ImportType { .. } => self.type_node(expr_name),
                    _ => self.entity_name(expr_name),
                }
                self.type_args(type_args);
            }
            Node::TypeOperator { op, ty } => {
                self.w(match op {
                    TypeOperatorKind::KeyOf => "keyof ",
                    TypeOperatorKind::Unique => "unique ",
                    TypeOperatorKind::Readonly => "readonly ",
                });
                // `readonly (readonly T[])`, `keyof (infer U extends C)`.
                let paren = match *self.ast.node(ty) {
                    Node::TypeOperator { .. } => op == TypeOperatorKind::Readonly,
                    Node::InferType { type_param } => matches!(
                        *self.ast.node(type_param),
                        Node::TypeParam {
                            constraint: Some(_),
                            ..
                        }
                    ),
                    _ => false,
                };
                if paren {
                    self.w("(");
                    self.type_node(ty);
                    self.w(")");
                } else {
                    self.type_node(ty);
                }
            }
            Node::IndexedAccessType { obj, index } => {
                self.type_node(obj);
                self.w("[");
                self.type_node(index);
                self.w("]");
            }
            Node::ConditionalType {
                check,
                extends,
                true_ty,
                false_ty,
            } => {
                self.type_node(check);
                self.w(" extends ");
                // A function type's conditional return type is parenthesized there:
                // `<T>() => (T extends Y ? 1 : 2)`.
                match *self.ast.node(extends) {
                    Node::FunctionType {
                        type_params,
                        params,
                        return_type,
                    } if matches!(self.ast.node(return_type), Node::ConditionalType { .. }) => {
                        self.type_params(type_params);
                        self.params(params, extends);
                        self.w(" => (");
                        self.type_node(return_type);
                        self.w(")");
                    }
                    _ => self.type_node(extends),
                }
                self.w(" ? ");
                self.type_node(true_ty);
                self.w(" : ");
                self.type_node(false_ty);
            }
            Node::InferType { type_param } => {
                self.w("infer ");
                if let Node::TypeParam {
                    name, constraint, ..
                } = *self.ast.node(type_param)
                {
                    self.start(type_param);
                    self.ident_text(name);
                    if let Some(c) = constraint {
                        self.w(" extends ");
                        self.type_node(c);
                    }
                    self.end(type_param);
                }
            }
            Node::MappedType {
                readonly,
                type_param,
                name_type,
                optional,
                ty,
            } => {
                self.w("{");
                self.nl();
                self.indent += 1;
                // The `readonly` / `+` / `-` and `?` / `+` / `-` tokens map (the keyword
                // after a `+` or `-` does not).
                let src = !self.is_synth(n);
                let open = self.ast.span(n).start + 1;
                let ro_at = src.then(|| self.next_token(open));
                match readonly {
                    MappedModifier::None => {}
                    MappedModifier::Present => {
                        self.token("readonly", ro_at);
                        self.w(" ");
                    }
                    MappedModifier::Add => {
                        self.token("+", ro_at);
                        self.w("readonly ");
                    }
                    MappedModifier::Remove => {
                        self.token("-", ro_at);
                        self.w("readonly ");
                    }
                }
                self.w("[");
                if let Node::TypeParam {
                    name, constraint, ..
                } = *self.ast.node(type_param)
                {
                    self.start(type_param);
                    self.ident_text(name);
                    self.w(" in ");
                    if let Some(c) = constraint {
                        self.type_node(c);
                    }
                    self.end(type_param);
                }
                if let Some(nt) = name_type {
                    self.w(" as ");
                    self.type_node(nt);
                }
                self.w("]");
                // The token after the `]` that closes the type parameter (or name type).
                let q_at = if src {
                    let after = name_type.unwrap_or(type_param);
                    self.char_token(self.ast.span(after).end, b']')
                        .map(|b| self.next_token(b + 1))
                } else {
                    None
                };
                match optional {
                    MappedModifier::None => {}
                    MappedModifier::Present => self.token("?", q_at),
                    MappedModifier::Add => {
                        self.token("+", q_at);
                        self.w("?");
                    }
                    MappedModifier::Remove => {
                        self.token("-", q_at);
                        self.w("?");
                    }
                }
                self.w(": ");
                match ty {
                    Some(t) => self.type_node(t),
                    None => self.w("any"),
                }
                self.w(";");
                self.nl();
                self.indent -= 1;
                self.w("}");
            }
            Node::ParenType { ty } => {
                self.w("(");
                self.type_node(ty);
                self.w(")");
            }
            Node::TypePredicate { asserts, param, ty } => {
                if asserts {
                    let at = (!self.is_synth(n)).then(|| self.ast.span(n).start);
                    self.token("asserts", at);
                    self.w(" ");
                }
                match *self.ast.node(param) {
                    Node::ThisType {} => {
                        self.start(param);
                        self.w("this");
                        self.end(param);
                    }
                    _ => self.ident_text(param),
                }
                if let Some(t) = ty {
                    self.w(" is ");
                    self.type_node(t);
                }
            }
            Node::ImportType {
                is_typeof,
                arg,
                attributes,
                qualifier,
                type_args,
            } => {
                if is_typeof {
                    self.w("typeof ");
                }
                self.w("import(");
                self.literal(arg);
                if let Some(a) = attributes {
                    self.w(", ");
                    self.import_type_attributes(a);
                }
                self.w(")");
                if let Some(q) = qualifier {
                    self.w(".");
                    self.entity_name(q);
                }
                self.type_args(type_args);
            }
            Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::Unary { .. }
            | Node::TemplateLit { .. } => self.literal(n),
            Node::ExprWithTypeArgs { .. } => self.heritage_type(n),
            _ => {
                // Anything else is printed as written.
                let raw = self.raw(n).to_owned();
                self.w(&raw);
            }
        }
    }

    fn type_list(&mut self, types: NodeList, sep: &str) {
        let types = self.ast.list(types).to_vec();
        for (i, &t) in types.iter().enumerate() {
            if i > 0 {
                self.w(sep);
            }
            self.type_node(t);
        }
    }

    fn template_text(&mut self, p: NodeId) {
        if self.is_synth(p) {
            if let Node::TemplateElement { cooked, .. } = *self.ast.node(p) {
                let text = ts_r_checker::print::escape_template_text(self.ast.atom(cooked));
                self.w(&text);
            }
        } else {
            let raw = self.raw(p).to_owned();
            self.w(&raw);
        }
    }

    /// A literal: source text for source nodes, values for synthesized ones.
    pub(super) fn literal(&mut self, n: NodeId) {
        if !self.is_synth(n) {
            self.start(n);
            if let Node::Unary { op, arg } = *self.ast.node(n) {
                // `-1`: the operand maps too.
                self.w(op.as_str());
                self.literal(arg);
            } else {
                let raw = self.raw(n).to_owned();
                self.w(&raw);
            }
            self.end(n);
            return;
        }
        match *self.ast.node(n) {
            Node::StringLit { value } => {
                let q = ts_r_checker::print::quote_string(self.ast.atom(value));
                self.w(&q);
            }
            Node::NumberLit { value } => self.w(&number_text(value.get())),
            Node::BigIntLit { value } => {
                let v = format!("{}n", self.ast.atom(value));
                self.w(&v);
            }
            Node::BoolLit { value } => self.w(if value { "true" } else { "false" }),
            Node::NullLit {} => self.w("null"),
            Node::Unary { op, arg } => {
                self.w(op.as_str());
                self.literal(arg);
            }
            _ => {}
        }
    }

    fn ident_text(&mut self, n: NodeId) {
        if self.is_synth(n) {
            if let Node::Ident { name } = *self.ast.node(n) {
                let t = self.ast.atom(name).to_owned();
                self.w(&t);
            }
        } else {
            self.name_text(n);
        }
    }

    /// `A`, `A.B.C` (qualified names and property accesses).
    pub(super) fn entity_name(&mut self, n: NodeId) {
        match *self.ast.node(n) {
            Node::Ident { .. } => self.ident_text(n),
            Node::QualifiedName { left, right }
            | Node::Member {
                obj: left,
                prop: right,
                ..
            } => {
                self.start(n);
                self.entity_name(left);
                self.w(".");
                self.ident_text(right);
                self.end(n);
            }
            Node::This {} => {
                self.start(n);
                self.w("this");
                self.end(n);
            }
            Node::ImportType { .. } => self.type_node(n),
            _ => {
                self.start(n);
                let raw = self.raw(n).to_owned();
                self.w(&raw);
                self.end(n);
            }
        }
    }

    pub(super) fn type_args(&mut self, args: Option<NodeId>) {
        let Some(a) = args else { return };
        let Node::TypeArgs { args } = *self.ast.node(a) else {
            return;
        };
        let args = self.ast.list(args).to_vec();
        self.w("<");
        for (i, &t) in args.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            self.type_node(t);
        }
        self.w(">");
    }

    pub(super) fn type_params(&mut self, tps: Option<NodeId>) {
        let Some(tps) = tps else { return };
        let Node::TypeParams { params } = *self.ast.node(tps) else {
            return;
        };
        let params = self.ast.list(params).to_vec();
        if params.is_empty() {
            return;
        }
        self.w("<");
        for (i, &p) in params.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            let Node::TypeParam {
                modifiers,
                name,
                constraint,
                default,
            } = *self.ast.node(p)
            else {
                continue;
            };
            if !self.is_synth(p) && !self.opts.remove_comments {
                self.inline_comments_before(self.ast.span(p).start, &['<', ',']);
            }
            self.start(p);
            let mut words: Vec<(Modifiers, &str)> = Vec::new();
            for (f, w) in [
                (Modifiers::CONST, "const"),
                (Modifiers::IN, "in"),
                (Modifiers::OUT, "out"),
            ] {
                if modifiers.contains(f) {
                    words.push((f, w));
                }
            }
            self.modifiers(p, &words);
            self.ident_text(name);
            if let Some(c) = constraint {
                self.w(" extends ");
                self.type_node(c);
            }
            if let Some(d) = default {
                self.w(" = ");
                self.type_node(d);
            }
            self.end(p);
        }
        self.w(">");
    }

    /// `(a: T, b?: U, ...c: V[])` for a declaration's parameters. Missing types are
    /// inferred; initializers make a parameter optional.
    pub(super) fn params(&mut self, params: NodeList, owner: NodeId) {
        let _ = owner;
        let rebuilt = u32::from(std::mem::take(&mut self.rebuilt_params));
        let ps = self.ast.list(params).to_vec();
        // A parameter with an initializer is optional unless a required one follows.
        let last_required = ps.iter().rposition(|&p| {
            matches!(
                *self.ast.node(p),
                Node::Param {
                    optional: false,
                    init: None,
                    rest: false,
                    ..
                }
            )
        });
        self.w("(");
        for (i, &p) in ps.iter().enumerate() {
            if i > 0 {
                self.w(", ");
            }
            let Node::Param {
                modifiers,
                rest,
                pattern,
                optional,
                type_ann,
                init,
                ..
            } = *self.ast.node(p)
            else {
                continue;
            };
            if !self.is_synth(p) {
                self.param_comments(p, pattern);
            }
            self.quiet += rebuilt;
            self.start(p);
            if rest {
                self.w("...");
            }
            self.binding_name(pattern);
            let is_this =
                matches!(self.ast.node(pattern), Node::Ident { .. }) && self.raw(pattern) == "this";
            let opt_by_init = init.is_some() && last_required.is_none_or(|l| l < i);
            if optional {
                self.question(pattern);
            } else if opt_by_init {
                self.w("?");
            }
            self.quiet -= rebuilt;
            // A defaulted parameter before a required one: `T | undefined`.
            let add_undefined =
                init.is_some() && !opt_by_init && !optional && self.c.opts.strict_null_checks;
            match type_ann {
                Some(t) => {
                    self.w(": ");
                    let start = self.out.len();
                    self.type_node(t);
                    // An optional parameter property is `T | undefined` as its property.
                    let add_undefined =
                        add_undefined || (optional && modifiers.intersects(PARAM_PROPERTY));
                    if add_undefined && !self.is_synth(p) && !self.annotation_includes_undefined(t)
                    {
                        self.append_undefined(start);
                    }
                }
                None if self.is_synth(p) => {}
                None if is_this => {}
                None => {
                    self.iso_param(p, add_undefined);
                    self.w(": ");
                    if let Some(i) = init
                        && super::reuse::is_reusable(&self.ast, self.src, i)
                    {
                        let start = self.out.len();
                        self.reuse_expr_type(i);
                        if add_undefined {
                            self.append_undefined(start);
                        }
                        self.quiet += rebuilt;
                        self.end(p);
                        self.quiet -= rebuilt;
                        continue;
                    }
                    let mut t = self.ck.param_type(self.loc(p));
                    if add_undefined {
                        t = self.ck.add_undefined(t);
                    }
                    self.inferred_type(t, p);
                }
            }
            self.quiet += rebuilt;
            self.end(p);
            self.quiet -= rebuilt;
        }
        self.w(")");
    }

    /// Whether the type of annotation `ann` includes `undefined` (or is `any`/`unknown`).
    pub(super) fn annotation_includes_undefined(&mut self, ann: NodeId) -> bool {
        let t = self.ck.type_from_node(self.loc(ann));
        let s = self.ck.store();
        let any = TypeFlags::UNDEFINED | TypeFlags::ANY | TypeFlags::UNKNOWN;
        s.flags(t).intersects(any)
            || (s.flags(t).contains(TypeFlags::UNION)
                && s.members_of_union(t)
                    .iter()
                    .any(|&m| s.flags(m).intersects(TypeFlags::UNDEFINED)))
    }

    /// Appends ` | undefined` to the type written since `start` (parenthesized when it is
    /// a function, conditional or intersection type), unless it already ends with it.
    pub(super) fn append_undefined(&mut self, start: usize) {
        let text = self.out[start..].to_owned();
        if text.ends_with("undefined") {
            return;
        }
        if needs_parens_in_union(&text) {
            self.out.insert(start, '(');
            for m in self.marks.iter_mut().rev() {
                if (m.0 as usize) < start {
                    break;
                }
                m.0 += 1;
            }
            self.w(")");
        }
        self.w(" | undefined");
    }

    /// Comments written inside the parameter list before a parameter (`/* c */ x`).
    pub(super) fn param_comments(&mut self, p: NodeId, _pattern: NodeId) {
        if self.opts.remove_comments {
            return;
        }
        let start = self.ast.span(p).start;
        self.inline_comments_before(start, &['(', ',', '<', '[']);
    }

    /// Block comments between the previous list token (one of `after`) and `start`, as
    /// tsc keeps them inside lists: on the same line `/* c */ x`; a comment on its own line
    /// starts a new line, and so does the item after it.
    pub(super) fn inline_comments_before(&mut self, start: u32, after: &[char]) {
        let from = self.list_token_end(start, after);
        let comments: Vec<_> = self.ast.comments_between(from, start).to_vec();
        let mut prev = from;
        for c in comments {
            let own_line = self
                .src
                .get(prev as usize..c.span.start as usize)
                .is_some_and(|t| t.contains('\n'));
            if c.kind == ts_r_ast::CommentKind::Line {
                // A line comment after the list token, on its line, stays there and the
                // item starts a new line (observed: `path: string[], // note`).
                if !own_line {
                    let text = self.src[c.span.start as usize..c.span.end as usize].to_owned();
                    self.write_comment(&text, c.span.start);
                    self.nl();
                    prev = c.span.end;
                }
                continue;
            }
            if c.kind != ts_r_ast::CommentKind::Block {
                continue;
            }
            if own_line {
                self.nl();
            }
            let text = self.src[c.span.start as usize..c.span.end as usize].to_owned();
            // A multi-line comment is re-indented like other comments (observed:
            // parameter JSDoc of a top-level function starts its lines at ` *`).
            self.write_comment(&text, c.span.start);
            prev = c.span.end;
            let item_on_next_line = self
                .src
                .get(prev as usize..start as usize)
                .is_some_and(|t| t.contains('\n'));
            if item_on_next_line && own_line {
                self.nl();
            } else {
                self.w(" ");
            }
        }
    }

    /// The end of the list token (one of `after`) before `start`, skipping whitespace and
    /// comments (whose text may itself contain `,` or `(`); `0` when none is found.
    pub(super) fn list_token_end(&self, start: u32, after: &[char]) -> u32 {
        let comments = self.ast.comments_between(0, start);
        let mut ci = comments.len();
        let mut pos = start as usize;
        loop {
            let trimmed = self.src[..pos].trim_end();
            pos = trimmed.len();
            while ci > 0 && comments[ci - 1].span.end as usize > pos {
                ci -= 1;
            }
            if ci > 0 && comments[ci - 1].span.end as usize == pos {
                ci -= 1;
                pos = comments[ci].span.start as usize;
                continue;
            }
            break;
        }
        match self.src[..pos].chars().next_back() {
            Some(ch) if after.contains(&ch) => pos as u32,
            _ => self.src[..pos].rfind(after).map_or(0, |i| i + 1) as u32,
        }
    }

    /// A parameter's binding name: an identifier, or a pattern without initializers.
    pub(super) fn binding_name(&mut self, n: NodeId) {
        match *self.ast.node(n) {
            Node::Ident { .. } => self.ident_text(n),
            Node::ObjectPattern { props } => {
                let props = self.ast.list(props).to_vec();
                self.start(n);
                if props.is_empty() {
                    self.w("{}");
                    self.end(n);
                    return;
                }
                self.w("{ ");
                for (i, &p) in props.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    self.start(p);
                    match *self.ast.node(p) {
                        Node::BindingProperty { key, value, .. } => {
                            self.property_key(key);
                            if let Some(v) = value {
                                self.w(": ");
                                self.binding_name(v);
                            }
                        }
                        Node::Rest { arg } => {
                            self.w("...");
                            self.binding_name(arg);
                        }
                        _ => {}
                    }
                    self.end(p);
                }
                // A trailing comma in the source is kept (`{ a, b, }`).
                if !self.is_synth(n)
                    && let Some(&last) = props.last()
                {
                    let (from, to) = (self.ast.span(last).end, self.ast.span(n).end);
                    if self
                        .src
                        .get(from as usize..to as usize)
                        .is_some_and(|t| t.contains(','))
                    {
                        self.w(",");
                    }
                }
                self.w(" }");
                self.end(n);
            }
            Node::ArrayPattern { elements } => {
                let els = self.ast.list(elements).to_vec();
                self.start(n);
                self.w("[");
                for (i, &e) in els.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    match *self.ast.node(e) {
                        Node::Hole {} => {}
                        Node::Rest { arg } => {
                            self.start(e);
                            self.w("...");
                            self.binding_name(arg);
                            self.end(e);
                        }
                        _ => self.binding_name(e),
                    }
                }
                if matches!(els.last().map(|&e| *self.ast.node(e)), Some(Node::Hole {})) {
                    self.w(",");
                }
                self.w("]");
                self.end(n);
            }
            Node::AssignPattern { left, .. } => {
                // A binding element with a default: its end is after the default.
                self.start(n);
                self.binding_name(left);
                self.end(n);
            }
            _ => {
                let raw = self.raw(n).to_owned();
                self.w(&raw);
            }
        }
    }

    /// A property name: identifiers, literals as written, `[expr]`.
    pub(super) fn property_key(&mut self, key: NodeId) {
        if self.is_synth(key) {
            self.start(key);
            self.ident_text(key);
            self.end(key);
            return;
        }
        match *self.ast.node(key) {
            Node::ComputedKey { expr } => {
                self.reference_entity(super::skip_parens(&self.ast, expr), meaning::VALUE);
                self.start(key);
                self.w("[");
                match *self.ast.node(expr) {
                    Node::Ident { .. } | Node::Member { .. } => self.entity_name(expr),
                    _ => {
                        self.start(expr);
                        let raw = self.raw(expr).to_owned();
                        self.w(&raw);
                        self.end(expr);
                    }
                }
                self.w("]");
                self.end(key);
            }
            _ => self.name_text(key),
        }
    }

    /// `A<T>` in `extends` / `implements` (an entity expression with type arguments).
    pub(super) fn heritage_type(&mut self, n: NodeId) {
        match *self.ast.node(n) {
            Node::ExprWithTypeArgs { expr, .. }
                if matches!(self.ast.node(expr), Node::NullLit {}) =>
            {
                self.w("null");
            }
            Node::ExprWithTypeArgs { expr, type_args } => {
                self.reference_entity(expr, meaning::TYPE | meaning::VALUE | meaning::NAMESPACE);
                self.start(n);
                self.entity_name(expr);
                self.type_args(type_args);
                self.end(n);
            }
            _ => self.type_node(n),
        }
    }

    /// Interface and type literal members, one per line.
    pub(super) fn type_members(&mut self, members: &[NodeId], start: u32) {
        let mut prev_end = start;
        for &m in members {
            if !self.is_synth(m) {
                let key = match *self.ast.node(m) {
                    Node::PropertySignature { key, .. } | Node::MethodSignature { key, .. } => {
                        Some(key)
                    }
                    _ => None,
                };
                // `@internal` members, computed names that are not an entity or a
                // literal (`["" + ""]()`) and ones that are not late-bindable (`[k]` of a
                // `string`, observed) are not written.
                if self.is_internal(m, prev_end)
                    || key.is_some_and(|k| self.skip_key(k) || self.unbindable_key(k))
                {
                    prev_end = self.ast.span(m).end;
                    continue;
                }
                self.leading_jsdoc(m, prev_end);
                prev_end = self.ast.span(m).end;
            } else if let Some(&d) = self.member_docs.get(&m) {
                self.object_member_jsdoc(d);
            }
            self.type_member(m);
        }
    }

    pub(super) fn type_member(&mut self, m: NodeId) {
        match *self.ast.node(m) {
            Node::PropertySignature {
                modifiers,
                key,
                optional,
                type_ann,
            } => {
                let words: &[(Modifiers, &str)] = if modifiers.contains(Modifiers::READONLY) {
                    &[(Modifiers::READONLY, "readonly")]
                } else {
                    &[]
                };
                self.modifiers(m, words);
                self.property_key(key);
                if optional {
                    self.question(key);
                }
                self.w(": ");
                match type_ann {
                    Some(t) => self.type_node(t),
                    None => self.w("any"),
                }
            }
            Node::MethodSignature {
                kind,
                key,
                optional,
                type_params,
                params,
                return_type,
                ..
            } => {
                match kind {
                    MethodKind::Get => self.w("get "),
                    MethodKind::Set => self.w("set "),
                    _ => {}
                }
                self.property_key(key);
                if optional {
                    self.question(key);
                }
                self.type_params(type_params);
                self.params(params, m);
                if kind != MethodKind::Set {
                    self.w(": ");
                    match return_type {
                        Some(r) => self.type_node(r),
                        None => {
                            self.iso_implicit_any(m);
                            self.w("any");
                        }
                    }
                }
            }
            Node::CallSignature {
                type_params,
                params,
                return_type,
            } => {
                self.type_params(type_params);
                self.params(params, m);
                self.w(": ");
                match return_type {
                    Some(r) => self.type_node(r),
                    None => self.w("any"),
                }
            }
            Node::ConstructSignature {
                type_params,
                params,
                return_type,
            } => {
                self.w("new ");
                self.type_params(type_params);
                self.params(params, m);
                self.w(": ");
                match return_type {
                    Some(r) => self.type_node(r),
                    None => self.w("any"),
                }
            }
            Node::IndexSignature {
                modifiers,
                params,
                type_ann,
            } => {
                let mut words: Vec<(Modifiers, &str)> = Vec::new();
                if modifiers.contains(Modifiers::STATIC) {
                    words.push((Modifiers::STATIC, "static"));
                }
                if modifiers.contains(Modifiers::READONLY) {
                    words.push((Modifiers::READONLY, "readonly"));
                }
                self.modifiers(m, &words);
                self.w("[");
                let ps = self.ast.list(params).to_vec();
                for (i, &p) in ps.iter().enumerate() {
                    if i > 0 {
                        self.w(", ");
                    }
                    if let Node::Param {
                        pattern, type_ann, ..
                    } = *self.ast.node(p)
                    {
                        if !self.is_synth(p) {
                            self.param_comments(p, pattern);
                        }
                        self.start(p);
                        self.binding_name(pattern);
                        if let Some(t) = type_ann {
                            self.w(": ");
                            self.type_node(t);
                        }
                        self.end(p);
                    }
                }
                self.w("]: ");
                match type_ann {
                    Some(t) => self.type_node(t),
                    None => self.w("any"),
                }
            }
            _ => return,
        }
        self.w(";");
        self.end(m);
        self.nl();
    }
}

/// Whether a printed type needs parentheses as a union member (function, constructor and
/// conditional types).
fn needs_parens_in_union(text: &str) -> bool {
    if text.starts_with("new ") || text.starts_with('<') {
        return true;
    }
    // An operator outside brackets and strings: `=>`, `&`, `? :`.
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'"' | b'\'' | b'`' => quote = c,
                b'(' | b'{' | b'[' | b'<' => depth += 1,
                b')' | b'}' | b']' => depth -= 1,
                b'>' if i > 0 && b[i - 1] != b'=' => depth -= 1,
                b'=' if depth == 0 && b.get(i + 1) == Some(&b'>') => return true,
                b'&' | b'?' if depth == 0 => return true,
                _ => {}
            }
        }
        i += 1;
    }
    false
}

fn keyword(kind: K) -> &'static str {
    match kind {
        K::Any => "any",
        K::Unknown => "unknown",
        K::Number => "number",
        K::BigInt => "bigint",
        K::Boolean => "boolean",
        K::String => "string",
        K::Symbol => "symbol",
        K::Object => "object",
        K::Never => "never",
        K::Void => "void",
        K::Undefined => "undefined",
        K::Null => "null",
        K::Intrinsic => "intrinsic",
        K::Const => "const",
    }
}
