//! JSDoc in JavaScript files, read as tsc 7 reads it (observed behaviour, probed with
//! `tsc --checkJs`; no tsc source was consulted).
//!
//! [`attach_jsdoc`] parses the `/** ... */` comments of a parsed JavaScript file and writes
//! what their tags say into the tree as the TypeScript syntax they stand for, so the binder
//! and checker treat them like annotations:
//!
//! - A comment documents the nodes that start at the first token after it (the statement,
//!   and the expressions or members starting there). Of several comments before one node,
//!   a later `@type` wins.
//! - `@type {T}`: a parenthesized expression becomes `<T>(expr)` (a cast); else the first
//!   declarator of a variable statement, a class property or a parameter gets the
//!   annotation `T`. On a function, an assignment target (`this.x = v`, `o.x = v`) or an
//!   object literal property it is only listed in [`Ast::jsdoc_tags`]. The braces are
//!   optional (`@type number`).
//! - `@param {T} name` / `@arg` / `@argument`: the parameter named `name` of the documented
//!   function (a binding pattern by position) gets `T`; `[name]`, `[name=default]` and `T=`
//!   make it optional, `...T` is `T[]`. `@param {Object} o` with `@param {string} o.a` is
//!   `{ a: string }` (`Object[]` with `o[].a`: an array of it).
//! - `@returns {T}` / `@return`: the return type. `@this {T}`: a `this` parameter.
//! - `@template [{C}] T, U` (`[T=D]` for a default): type parameters of the function or
//!   class, or of the comment's `@typedef`/`@callback`.
//! - `@typedef {T} Name` (with `@property`/`@prop` tags when `T` is `Object` or missing)
//!   and `@callback Name` (with `@param`/`@returns`; no `@returns` returns `any`): a type
//!   alias statement before the documented statement (the end of the enclosing statement
//!   list for an unattached comment), so it is scoped to that block or function.
//! - `@import { A } from "m"` (any import clause): a type-only import at the top level.
//! - `@satisfies {T}`: `(expr satisfies T)` for a parenthesized expression, a variable's
//!   first initializer or an assignment's value; the braces are required (TS1005).
//! - `@extends {B<T>}` / `@augments`: type arguments of the class's `extends` clause;
//!   `@implements {I}`: an `implements` clause.
//! - `@private`, `@protected`, `@public`, `@readonly`, `@override` on class members:
//!   the modifiers.
//! - A comment with `@overload` documents an overload, not the implementation: its
//!   `@param`/`@returns` are not applied.
//!
//! JSDoc type syntax beyond TypeScript's: `*` (any), `?T`/`T?` (`T | null`), `!T`/`T!`
//! (`T`), `Array.<T>`, a top-level `...T` (rest) and `T=` (optional). The checker maps
//! the JSDoc names `String`, `Object`, `Object<K, V>`, ... (see `ts_r_checker`).
//!
//! Syntax errors inside JSDoc types are returned, not added to the tree's diagnostics:
//! tsc reports them only for checked files, and they do not stop semantic checking.

use std::collections::HashMap;

use ts_r_ast::{
    Ast, CommentKind, HeritageKind, JsDocTag, JsDocTagKind, KeywordTypeKind, MethodKind, Modifiers,
    Node, NodeId, NodeList, ParentMap, Span,
};
use ts_r_diagnostics::{Diagnostic, messages};
use ts_r_lexer::{TokenKind as T, is_id_part, is_id_start};

use crate::Parser;

/// Parses the JSDoc comments of a JavaScript file and writes their types into `ast` (see
/// the module docs). `src` is the text `ast` was parsed from. Returns the syntax errors
/// found in JSDoc types. Does nothing for TypeScript files.
pub fn attach_jsdoc(ast: &mut Ast, src: &str) -> Vec<Diagnostic> {
    attach_jsdoc_with_syntax(ast, src).0
}

/// [`attach_jsdoc`], also returning the JSDoc errors tsc 7 reports as syntax errors of
/// the file (checked or not): `Identifier expected.` one character before where a
/// `@typedef {T}`'s missing name should be.
pub fn attach_jsdoc_with_syntax(ast: &mut Ast, src: &str) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
    if ast.source_kind.is_typescript() || src.len() >= u32::MAX as usize {
        return (Vec::new(), Vec::new());
    }
    let docs: Vec<Span> = ast
        .comments
        .iter()
        .filter(|c| c.kind == CommentKind::Block && is_jsdoc_text(src, c.span))
        .map(|c| c.span)
        .collect();
    let own_line: Vec<bool> = ast
        .comments
        .iter()
        .filter(|c| c.kind == CommentKind::Block && is_jsdoc_text(src, c.span))
        .map(|c| c.newline_before)
        .collect();
    if docs.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let masked = mask(src, &docs);
    let kind = ast.source_kind;
    let taken = std::mem::replace(ast, Ast::new(kind, 0));
    let mut d = Doc::new(src, &masked, taken);
    for (&span, &own_line) in docs.iter().zip(own_line.iter()) {
        if d.p.halted {
            break;
        }
        d.comment(span, own_line);
    }
    d.finish();
    let Doc {
        p,
        mut diags,
        tags,
        mut syntax,
        ..
    } = d;
    let mut out = p.ast;
    let mut tags = tags;
    tags.sort_by_key(|t| t.host);
    out.jsdoc_tags = tags;
    *ast = out;
    diags.sort_by_key(|d| (d.span.start, d.span.end));
    // One syntax error per position (checker errors like TS8023 are kept).
    // (`@import x y` gets both `'from' expected.` and `String literal expected.` at `y`.)
    diags.dedup_by(|a, b| {
        a.span.start == b.span.start
            && a.code < 2000
            && b.code < 2000
            && !(a.code == 1141 && b.code == 1005)
    });
    syntax.sort_by_key(|d| (d.span.start, d.span.end));
    (diags, syntax)
}

/// A `\\uXXXX` or `\\u{X...}` escape at the start of `s`: the character and the escape's
/// length.
fn unicode_escape(s: &str) -> Option<(char, usize)> {
    let rest = s.strip_prefix("\\u")?;
    let (hex, len) = if let Some(r) = rest.strip_prefix('{') {
        let close = r.find('}')?;
        if close == 0 || close > 6 {
            return None;
        }
        (&r[..close], 2 + 1 + close + 1)
    } else {
        (rest.get(..4)?, 6)
    };
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some((char::from_u32(v)?, len))
}

/// `/** ... */`, not `/**/`.
fn is_jsdoc_text(src: &str, span: Span) -> bool {
    let b = src.as_bytes();
    let (s, e) = (span.start as usize, span.end as usize);
    e >= s + 5 && b.get(s..s + 3) == Some(b"/**") && b.get(s + 3) != Some(&b'/')
}

/// A copy of `src` (same byte offsets) in which the `/**` and the leading `*` of every
/// line of each JSDoc comment are spaces, so the type parser can read types that span
/// lines. The closing `*/` is kept.
fn mask(src: &str, docs: &[Span]) -> String {
    let mut b = src.as_bytes().to_vec();
    for s in docs {
        let (start, end) = (s.start as usize, s.end as usize);
        if end < start + 5 || end > b.len() {
            continue;
        }
        b[start..start + 3].fill(b' ');
        let body_end = end - 2;
        let mut i = start + 3;
        while i < body_end {
            if b[i] == b'\n' || b[i] == b'\r' {
                i += 1;
                while i < body_end && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
                    i += 1;
                }
                if i < body_end && b[i] == b'*' {
                    b[i] = b' ';
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
    }
    // Only ASCII bytes were replaced by ASCII bytes.
    String::from_utf8(b).unwrap_or_else(|_| src.to_owned())
}

/// A tag as written: `@name` at `at`, its text up to `end`.
#[derive(Clone, Copy)]
struct RawTag {
    at: u32,
    name_end: u32,
    end: u32,
}

/// A `@param`/`@property` name: dotted segments (`o.a`, `o[].a`) and whether it was
/// bracketed (`[o.a]`, optional).
struct TagName {
    segments: Vec<Span>,
    optional: bool,
}

/// A parsed `@param`/`@property` tag.
struct ParamTag {
    name: TagName,
    ty: Option<NodeId>,
    span: Span,
}

/// What a comment documents, from the nodes starting after it (outermost first).
#[derive(Default)]
struct Targets {
    /// The function its `@param`/`@returns`/`@template`/`@this` describe.
    func: Option<NodeId>,
    class: Option<NodeId>,
    /// The first declarator of a variable statement.
    var: Option<NodeId>,
    /// The innermost parenthesized expression (a cast).
    paren: Option<NodeId>,
    param: Option<NodeId>,
    /// A class property or method.
    member: Option<NodeId>,
    /// An assignment expression statement.
    assign: Option<NodeId>,
    /// An object literal property.
    prop: Option<NodeId>,
    /// `export default <expr>`.
    export_default: Option<NodeId>,
}

struct Doc<'a> {
    p: Parser<'a>,
    /// The original text (comments intact).
    orig: &'a str,
    /// The masked text the parser reads.
    src: &'a str,
    parents: ParentMap,
    /// `(start, node)` for every node of the original tree, by start, outermost first.
    starts: Vec<(u32, NodeId)>,
    tags: Vec<JsDocTag>,
    diags: Vec<Diagnostic>,
    /// Errors tsc 7 reports as the file's syntax errors (see [`attach_jsdoc_with_syntax`]).
    syntax: Vec<Diagnostic>,
    /// Statements to insert: `(list owner, insert before this position, statement)`.
    inserts: Vec<(NodeId, u32, NodeId)>,
    desugar_work: usize,
}

impl<'a> Doc<'a> {
    fn new(orig: &'a str, src: &'a str, ast: Ast) -> Doc<'a> {
        let parents = ParentMap::build(&ast);
        let mut starts = Vec::with_capacity(ast.node_count());
        let mut stack = vec![ast.root];
        let mut kids = Vec::new();
        while let Some(id) = stack.pop() {
            starts.push((ast.span(id).start, id));
            kids.clear();
            ast.for_each_child(id, |c| kids.push(c));
            stack.extend(kids.iter().rev());
        }
        starts.sort_by_key(|s| s.0);
        let mut p = Parser::with_ast(src, ast);
        p.ts = true;
        p.jsdoc = true;
        p.ast.jsdoc_start = p.ast.nodes.len() as u32;
        Doc {
            p,
            orig,
            src,
            parents,
            starts,
            tags: Vec::new(),
            diags: Vec::new(),
            syntax: Vec::new(),
            inserts: Vec::new(),
            desugar_work: src.len().saturating_mul(4).saturating_add(1024),
        }
    }

    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    // ---- Comments and tags ----

    /// A comment on the line of the previous token (`f(); /** @type {T} */ let x`) belongs
    /// only to a parameter, type parameter, function expression, arrow, parenthesized
    /// expression or variable declarator right after it; otherwise it is ignored, tags
    /// such as `@typedef` included (tsc 7).
    fn comment(&mut self, doc: Span, own_line: bool) {
        // (Sorted by start: the first node that isn't the program starts the first token.)
        let leading = own_line
            || self
                .starts
                .iter()
                .find(|s| !matches!(self.p.ast.node(s.1), Node::Program { .. }))
                .is_none_or(|s| s.0 >= doc.start);
        let next = next_token_pos(self.orig.as_bytes(), doc.end as usize) as u32;
        let lo = self.starts.partition_point(|s| s.0 < next);
        let hosts: Vec<NodeId> = self.starts[lo..]
            .iter()
            .take_while(|s| s.0 == next)
            .map(|s| s.1)
            .filter(|&n| {
                !matches!(self.p.ast.node(n), Node::Missing {} | Node::Program { .. })
                    && !self.p.ast.span(n).is_empty()
                    && (leading
                        || matches!(
                            self.p.ast.node(n),
                            Node::Param { .. }
                                | Node::TypeParam { .. }
                                | Node::FunctionExpr { .. }
                                | Node::Arrow { .. }
                                | Node::Paren { .. }
                                | Node::VarDeclarator { .. }
                        ))
            })
            .collect();
        if !leading && hosts.is_empty() {
            return;
        }
        let raw = self.split_tags(doc);
        if raw.is_empty() {
            return;
        }
        let t = self.targets(&hosts);
        self.apply(doc, &hosts, &t, &raw);
    }

    /// The tags of a comment: `@name` at the start of the text or after whitespace.
    fn split_tags(&self, doc: Span) -> Vec<RawTag> {
        let b = self.bytes();
        let (s, e) = (doc.start as usize + 3, doc.end as usize - 2);
        let mut tags: Vec<RawTag> = Vec::new();
        let mut i = s;
        while i < e {
            if b[i] == b'@' && (i == s || b[i - 1].is_ascii_whitespace()) {
                let mut j = i + 1;
                while j < e && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j > i + 1 {
                    if let Some(last) = tags.last_mut() {
                        last.end = i as u32;
                    }
                    tags.push(RawTag {
                        at: i as u32,
                        name_end: j as u32,
                        end: e as u32,
                    });
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
        tags
    }

    fn tag_name(&self, t: RawTag) -> &'a str {
        let src: &'a str = self.src;
        src.get(t.at as usize + 1..t.name_end as usize)
            .unwrap_or("")
    }

    fn targets(&self, hosts: &[NodeId]) -> Targets {
        let ast = &self.p.ast;
        let mut t = Targets::default();
        let fn_or_class = |t: &mut Targets, e: NodeId| match ast.node(skip_parens(ast, e)) {
            Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                t.func = t.func.or(Some(skip_parens(ast, e)));
            }
            Node::ClassExpr { .. } => t.class = t.class.or(Some(skip_parens(ast, e))),
            _ => {}
        };
        for &h in hosts {
            match *ast.node(h) {
                Node::VarDecl { decls, .. } if t.var.is_none() => {
                    if let Some(&d) = ast.list(decls).first() {
                        t.var = Some(d);
                        if let Node::VarDeclarator { init: Some(i), .. } = *ast.node(d) {
                            fn_or_class(&mut t, i);
                        }
                    }
                }
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    t.func = t.func.or(Some(h));
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => {
                    t.class = t.class.or(Some(h));
                }
                Node::MethodDef { value, .. } if t.member.is_none() => {
                    t.member = Some(h);
                    t.func = t.func.or(Some(value));
                }
                Node::PropertyDef { value, .. } if t.member.is_none() => {
                    t.member = Some(h);
                    if let Some(v) = value {
                        fn_or_class(&mut t, v);
                    }
                }
                Node::Param { .. } => t.param = t.param.or(Some(h)),
                Node::ExprStmt { expr } if t.assign.is_none() => {
                    if let Node::Assign { mut right, .. } = *ast.node(expr) {
                        t.assign = Some(expr);
                        // `a = b = function () {}`: the innermost value. Bounded: each
                        // round goes one assignment deeper.
                        while let Node::Assign { right: r, .. } = *ast.node(right) {
                            right = r;
                        }
                        fn_or_class(&mut t, right);
                    }
                }
                Node::Property { value, .. } if t.prop.is_none() => {
                    t.prop = Some(h);
                    fn_or_class(&mut t, value);
                }
                Node::ShorthandProp { .. } if t.prop.is_none() => t.prop = Some(h),
                Node::ExportDefaultExpr { expr } => {
                    t.export_default = Some(h);
                    fn_or_class(&mut t, expr);
                }
                // `/** @param {number} a */ return (a) => a;`
                Node::Return { arg: Some(a) } => fn_or_class(&mut t, a),
                Node::Paren { .. } => t.paren = Some(h),
                _ => {}
            }
        }
        t
    }

    // ---- Applying tags ----

    fn apply(&mut self, doc: Span, hosts: &[NodeId], t: &Targets, raw: &[RawTag]) {
        let names: Vec<&str> = raw.iter().map(|&r| self.tag_name(r)).collect();
        let has_typedef = names.iter().any(|n| matches!(*n, "typedef" | "callback"));
        let is_overload = names.contains(&"overload");
        let record_host = t
            .func
            .or(t.class)
            .or(t.member)
            .or(t.var)
            .or(t.param)
            .or(t.paren)
            .or(hosts.first().copied());
        let mut templates: Vec<NodeId> = Vec::new();
        let mut params: Vec<ParamTag> = Vec::new();
        let mut returns: Option<NodeId> = None;
        let mut this_ty: Option<NodeId> = None;
        let mut type_tag: Option<NodeId> = None;
        let mut satisfies: Option<NodeId> = None;
        let mut modifiers = Modifiers::NONE;
        let mut access_tags: Vec<Span> = Vec::new();
        let mut readonly_at: Option<Span> = None;
        let mut template_at: Option<Span> = None;
        let mut this_at: Option<u32> = None;
        let mut typedefs: Vec<NodeId> = Vec::new();
        let mut imports: Vec<NodeId> = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let r = raw[i];
            let span = Span::new(r.at, r.end);
            match names[i] {
                "typedef" | "callback" => {
                    let mut j = i + 1;
                    while j < raw.len() && !matches!(names[j], "typedef" | "callback") {
                        j += 1;
                    }
                    let tpl = if typedefs.is_empty() {
                        self.templates_in(raw, &names, i, j)
                    } else {
                        Vec::new()
                    };
                    // A `@typedef`'s children are the `@property`/`@prop` tags after it; a
                    // later `@param` or `@returns` documents the host (tsc 7). A typedef of
                    // another type than `Object` has none: a `@type` after it is the host's.
                    if names[i] == "typedef" && !self.typedef_takes_children(r) {
                        j = i + 1;
                    } else if names[i] == "typedef" {
                        let mut k = i + 1;
                        while k < j && matches!(names[k], "property" | "prop" | "template" | "type")
                        {
                            k += 1;
                        }
                        j = k;
                    }
                    let alias = if names[i] == "typedef" {
                        self.typedef(r, &raw[i + 1..j], &names[i + 1..j], tpl)
                    } else {
                        self.callback(r, &raw[i + 1..j], &names[i + 1..j], tpl)
                    };
                    if let Some(a) = alias {
                        typedefs.push(a);
                    }
                    if let Some(h) = record_host {
                        self.record(h, JsDocTagKind::Typedef, None, span);
                    }
                    i = j;
                    continue;
                }
                "template" if !has_typedef => {
                    template_at = template_at.or(Some(span));
                    templates.extend(self.template(r));
                }
                "param" | "arg" | "argument" => {
                    if let Some(pt) = self.param_tag(r) {
                        params.push(pt);
                    }
                }
                "returns" | "return" => {
                    returns = self.tag_type(r, false).or(returns);
                }
                "type" => {
                    if let Some(ty) = self.tag_type(r, true) {
                        type_tag = Some(ty);
                    }
                }
                "this" => {
                    this_ty = self.tag_type(r, true).or(this_ty);
                    this_at = this_at.or(Some(r.at + 1));
                }
                "satisfies" => satisfies = self.tag_type_required(r).or(satisfies),
                "extends" | "augments" => {
                    let tag = names[i];
                    if let Some(ty) = self.tag_type(r, true) {
                        self.extends_tag(t, ty, span, tag);
                    } else if self.skip_ws(r.name_end, r.end) >= r.end {
                        // `@augments` without a type: TS1003, and no match for `extends`.
                        let at = Span::new(r.name_end, r.name_end);
                        self.diags
                            .push(Diagnostic::new(messages::IDENTIFIER_EXPECTED, at, &[]));
                        if let Some(c) = t.class
                            && let Some(base) = self.class_extends_text(c)
                        {
                            self.diags.push(Diagnostic::new(
                                messages::JSDOC_EXTENDS_MISMATCH,
                                at,
                                &[tag, "", &base],
                            ));
                        }
                    }
                }
                "implements" => {
                    if let Some(ty) = self.tag_type(r, true) {
                        self.implements_tag(t, ty, span);
                    } else if self.skip_ws(r.name_end, r.end) >= r.end {
                        // `@implements` without a type: TS1003 after the tag name.
                        let at = Span::new(r.name_end, r.name_end);
                        self.diags
                            .push(Diagnostic::new(messages::IDENTIFIER_EXPECTED, at, &[]));
                    }
                }
                // tsc 7 does not read `@enum` types: no syntax or name errors in them.
                "enum" => {
                    if let Some(h) = record_host {
                        self.record(h, JsDocTagKind::Enum, None, span);
                    }
                }
                "import" => {
                    if let Some(imp) = self.import_tag(r, doc) {
                        imports.push(imp);
                    }
                }
                "private" | "protected" | "public" => {
                    modifiers |= match names[i] {
                        "private" => Modifiers::PRIVATE,
                        "protected" => Modifiers::PROTECTED,
                        _ => Modifiers::PUBLIC,
                    };
                    access_tags.push(span);
                    // `/** @private */ this.x = v`: recorded for the checker.
                    if t.member.is_none()
                        && t.assign.is_some()
                        && let Some(h) = record_host
                    {
                        match names[i] {
                            "private" => self.record(h, JsDocTagKind::Private, None, span),
                            "protected" => self.record(h, JsDocTagKind::Protected, None, span),
                            _ => {}
                        }
                    }
                }
                "readonly" => {
                    modifiers |= Modifiers::READONLY;
                    readonly_at = readonly_at.or(Some(span));
                }
                "override" => modifiers |= Modifiers::OVERRIDE,
                "constructor" | "class" => {
                    if let Some(h) = record_host {
                        self.record(h, JsDocTagKind::Constructor, None, span);
                    }
                }
                "deprecated" => {
                    if let Some(h) = record_host {
                        self.record(h, JsDocTagKind::Deprecated, None, span);
                    }
                }
                "overload" => {
                    if let Some(h) = record_host {
                        let ty = self.tag_type(r, false);
                        self.record(h, JsDocTagKind::Overload, ty, span);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if !typedefs.is_empty() {
            self.insert_statements(doc, hosts, &typedefs);
        }
        if !imports.is_empty() {
            let root = self.p.ast.root;
            for imp in imports {
                self.inserts.push((root, doc.start, imp));
            }
        }
        // `@this {T}` on an arrow function: TS2730 at the tag name (tsc 7).
        if this_ty.is_some()
            && let (Some(f), Some(at)) = (t.func, this_at)
            && matches!(self.p.ast.node(f), Node::Arrow { .. })
        {
            self.diags.push(Diagnostic::new(
                messages::ARROW_THIS_PARAMETER,
                Span::new(at, at + 4),
                &[],
            ));
        }
        if !is_overload {
            if let Some(f) = t.func {
                self.check_param_names(f, &params);
                self.apply_function(f, &params, returns, this_ty, &templates, raw, &names);
            } else if let Some(c) = t.class {
                self.set_type_params(c, &templates);
            }
        }
        if let Some(ty) = type_tag {
            self.apply_type(t, ty, record_host);
        }
        if let Some(ty) = satisfies {
            self.apply_satisfies(t, ty, record_host);
        }
        // On a method, accessor or constructor (tsc 7): `@readonly` is TS1024 at the tag;
        // on a constructor `@template` is TS1092 at the tag, `@returns {T}` TS1093 at `T`.
        if let Some(m) = t.member
            && let Node::MethodDef { kind, .. } = *self.p.ast.node(m)
        {
            if let Some(at) = readonly_at {
                self.diags.push(Diagnostic::new(
                    messages::READONLY_ONLY_ON_PROPERTY,
                    at,
                    &[],
                ));
            }
            if kind == MethodKind::Constructor {
                if let Some(at) = template_at {
                    self.diags.push(Diagnostic::new(
                        messages::CONSTRUCTOR_TYPE_PARAMETERS,
                        at,
                        &[],
                    ));
                }
                if let Some(r) = returns {
                    let at = self.p.ast.span(r);
                    self.diags.push(Diagnostic::new(
                        messages::CONSTRUCTOR_TYPE_ANNOTATION,
                        at,
                        &[],
                    ));
                }
            }
        }
        if !modifiers.is_empty()
            && let Some(m) = t.member
        {
            let mut private_key = None;
            match self.p.ast.jsdoc_node_mut(m) {
                Node::MethodDef {
                    modifiers: ms, key, ..
                }
                | Node::PropertyDef {
                    modifiers: ms, key, ..
                } => {
                    *ms |= modifiers;
                    private_key = Some(*key);
                }
                _ => {}
            }
            // `/** @private */ #x`: TS18010 at each accessibility tag (tsc 7).
            if private_key.is_some_and(|k| matches!(self.p.ast.node(k), Node::PrivateName { .. })) {
                for &at in &access_tags {
                    self.diags.push(Diagnostic::new(
                        messages::ACCESSIBILITY_ON_PRIVATE_NAME,
                        at,
                        &[],
                    ));
                }
            }
        }
    }

    fn record(&mut self, host: NodeId, kind: JsDocTagKind, ty: Option<NodeId>, span: Span) {
        self.tags.push(JsDocTag {
            host,
            kind,
            ty,
            span,
        });
    }

    // ---- Reading tag text ----

    fn skip_ws(&self, mut i: u32, end: u32) -> u32 {
        let b = self.bytes();
        while i < end && b[i as usize].is_ascii_whitespace() {
            i += 1;
        }
        i
    }

    /// An identifier starting at `i`: its end.
    /// The end of the identifier at `i` (`\u0061` and `\u{61}` escapes included).
    fn ident_end(&self, i: u32, end: u32) -> Option<u32> {
        let s = self.src.get(i as usize..end as usize)?;
        let mut k = 0;
        let mut first = true;
        while k < s.len() {
            let (c, len) = match unicode_escape(&s[k..]) {
                Some(x) => x,
                None => {
                    let c = s[k..].chars().next()?;
                    (c, c.len_utf8())
                }
            };
            let ok = if first { is_id_start(c) } else { is_id_part(c) };
            if !ok {
                break;
            }
            first = false;
            k += len;
        }
        if first {
            return None;
        }
        Some(i + k as u32)
    }

    fn ident(&mut self, span: Span) -> NodeId {
        let text = self.seg_text(span);
        let name = self.p.ast.intern(&text);
        self.p.ast.add(Node::Ident { name }, span)
    }

    /// The diagnostics the parser produced since `d0`/`l0`, kept when inside `doc`.
    fn take_diags(&mut self, d0: usize, l0: usize, doc: Span) -> bool {
        let mut any = false;
        let mut new: Vec<Diagnostic> = self.p.diags.drain(d0..).collect();
        new.extend(self.p.lx.diagnostics.drain(l0..));
        for d in new {
            any = true;
            if d.span.start >= doc.start && d.span.start <= doc.end {
                self.diags.push(d);
            }
        }
        self.p.grammar_diags = self.p.grammar_diags.min(self.p.diags.len() as u32);
        self.p.deferred.clear();
        any
    }

    /// The type expression `{T}` at `i` (the `{`), or a bare type with `!braced`. Returns
    /// the type and the end of the expression.
    /// The braced type expression `{T}` at `i`: the type (`None` when missing) and the end
    /// of the expression, where parsing stopped.
    fn type_expr_end(&mut self, i: u32, doc: Span) -> (Option<NodeId>, u32) {
        let d0 = self.p.diags.len();
        let l0 = self.p.lx.diagnostics.len();
        self.p.lx.seek(i);
        self.p.lx.next_token();
        self.p.prev_end = i;
        self.p.lists = 0;
        self.p.bump();
        let ty = self.p.jsdoc_type_expr();
        self.p.expect(T::RBrace);
        let end = self.p.prev_end;
        self.take_diags(d0, l0, doc);
        let ty = (!matches!(self.p.ast.node(ty), Node::Missing {})).then_some(ty);
        (ty, end)
    }

    fn type_expr(&mut self, i: u32, doc: Span, braced: bool) -> Option<(NodeId, u32)> {
        let d0 = self.p.diags.len();
        let l0 = self.p.lx.diagnostics.len();
        self.p.lx.seek(i);
        self.p.lx.next_token();
        self.p.prev_end = i;
        self.p.lists = 0;
        if braced {
            self.p.bump();
        }
        let ty = self.p.jsdoc_type_expr();
        if braced {
            self.p.expect(T::RBrace);
        }
        let end = self.p.prev_end;
        self.take_diags(d0, l0, doc);
        if matches!(self.p.ast.node(ty), Node::Missing {}) {
            return None;
        }
        Some((ty, end))
    }

    /// Just past the `}` matching the `{` at `i` (or `i` when there is none before `end`).
    fn brace_end(&self, i: u32, end: u32) -> u32 {
        let b = self.bytes();
        let mut depth = 0u32;
        let mut k = i as usize;
        while k < (end as usize).min(b.len()) {
            match b[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return k as u32 + 1;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        i
    }

    /// The comment span containing tag `r`.
    fn doc_of(&self, r: RawTag) -> Span {
        // Tags never cross comments; the comment ends at `end` + `*/`.
        Span::new(r.at, r.end + 2)
    }

    /// The type of a tag (`@type {T}`, `@returns {T}`): `{T}` after the tag name, or with
    /// `bare_ok` a type without braces.
    fn tag_type(&mut self, r: RawTag, bare_ok: bool) -> Option<NodeId> {
        let i = self.skip_ws(r.name_end, r.end);
        let doc = self.doc_of(r);
        if self.bytes().get(i as usize) == Some(&b'{') {
            return self.type_expr(i, doc, true).map(|t| t.0);
        }
        if bare_ok && i < r.end {
            return self.type_expr(i, doc, false).map(|t| t.0);
        }
        None
    }

    /// `@satisfies {T}`: the braces are required (TS1005 `'{' expected`, then the type and
    /// `'}' expected`).
    fn tag_type_required(&mut self, r: RawTag) -> Option<NodeId> {
        let i = self.skip_ws(r.name_end, r.end);
        let doc = self.doc_of(r);
        if self.bytes().get(i as usize) == Some(&b'{') {
            return self.type_expr(i, doc, true).map(|t| t.0);
        }
        let at = if i < r.end { i } else { r.end };
        let span = Span::new(at, self.ident_end(at, r.end).unwrap_or(at + 1).max(at + 1));
        self.diags
            .push(Diagnostic::new(messages::EXPECTED, span, &["{"]));
        if i < r.end {
            let d0 = self.p.diags.len();
            let l0 = self.p.lx.diagnostics.len();
            self.p.lx.seek(i);
            self.p.lx.next_token();
            self.p.prev_end = i;
            let _ = self.p.jsdoc_type_expr();
            self.p.expect(T::RBrace);
            self.take_diags(d0, l0, doc);
        }
        None
    }

    /// A `@param`/`@property` name: `a`, `a.b`, `a[].b`, `[a]`, `[a=default]`, `` `a` ``.
    fn tag_name_at(&self, i: u32, end: u32) -> Option<(TagName, u32)> {
        let b = self.bytes();
        let mut i = i;
        if b.get(i as usize) == Some(&b'`') {
            let e = self.ident_end(i + 1, end)?;
            if b.get(e as usize) != Some(&b'`') {
                return None;
            }
            let segments = vec![Span::new(i + 1, e)];
            return Some((
                TagName {
                    segments,
                    optional: false,
                },
                e + 1,
            ));
        }
        let bracketed = b.get(i as usize) == Some(&b'[');
        if bracketed {
            i = self.skip_ws(i + 1, end);
        }
        let mut segments = Vec::new();
        loop {
            let e = self.ident_end(i, end)?;
            segments.push(Span::new(i, e));
            i = e;
            if b.get(i as usize) == Some(&b'[') && b.get(i as usize + 1) == Some(&b']') {
                i += 2;
            }
            if b.get(i as usize) == Some(&b'.') {
                i += 1;
                continue;
            }
            break;
        }
        if bracketed {
            // Skip `= default` up to the closing bracket.
            let mut depth = 1;
            while i < end && depth > 0 {
                match b[i as usize] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
        }
        Some((
            TagName {
                segments,
                optional: bracketed,
            },
            i,
        ))
    }

    /// `@param {T} name` (or `@param name {T}`).
    fn param_tag(&mut self, r: RawTag) -> Option<ParamTag> {
        self.param_or_property_tag(r, false)
    }

    /// `@property {T} name` (`@prop`).
    fn property_tag(&mut self, r: RawTag) -> Option<ParamTag> {
        self.param_or_property_tag(r, true)
    }

    fn param_or_property_tag(&mut self, r: RawTag, property: bool) -> Option<ParamTag> {
        let doc = self.doc_of(r);
        let mut i = self.skip_ws(r.name_end, r.end);
        let mut ty = None;
        if self.bytes().get(i as usize) == Some(&b'{') {
            let close = self.brace_end(i, r.end);
            let (t, e) = self.type_expr(i, doc, true)?;
            ty = Some(t);
            i = e.max(close);
        }
        // The name may follow stray `*`s (a wrapped line's prefix, or one more).
        let b = self.bytes();
        while i < r.end && (b[i as usize].is_ascii_whitespace() || b[i as usize] == b'*') {
            i += 1;
        }
        // `@property {T} #x`: a private name is TS1003 at the `#` (tsc 7; not for
        // `@param`).
        if property && b.get(i as usize) == Some(&b'#') {
            self.diags.push(Diagnostic::new(
                messages::IDENTIFIER_EXPECTED,
                Span::new(i, i),
                &[],
            ));
            return None;
        }
        let (name, e) = self.tag_name_at(i, r.end)?;
        if ty.is_none() {
            let j = self.skip_ws(e, r.end);
            if self.bytes().get(j as usize) == Some(&b'{') {
                ty = self.type_expr(j, doc, true).map(|t| t.0);
            }
        }
        Some(ParamTag {
            name,
            ty,
            span: Span::new(r.at, r.end),
        })
    }

    /// `@template [{C}] T, U` / `[T=D]`.
    fn template(&mut self, r: RawTag) -> Vec<NodeId> {
        let doc = self.doc_of(r);
        let mut i = self.skip_ws(r.name_end, r.end);
        let mut constraint = None;
        let mut te_end = None;
        if self.bytes().get(i as usize) == Some(&b'{') {
            if let Some((t, e)) = self.type_expr(i, doc, true) {
                constraint = Some(t);
                te_end = Some(e);
                i = e;
            } else {
                return Vec::new();
            }
        }
        let mut out = Vec::new();
        loop {
            i = self.skip_ws(i, r.end);
            let mut modifiers = Modifiers::NONE;
            let first = i;
            // `@template const T`, `@template in out T`; other modifier keywords are taken
            // too (the checker reports TS1273 at them). Bounded: each modifier is taken
            // once, in this order.
            for (word, flag) in [
                ("public", Modifiers::PUBLIC),
                ("private", Modifiers::PRIVATE),
                ("protected", Modifiers::PROTECTED),
                ("readonly", Modifiers::READONLY),
                ("static", Modifiers::STATIC),
                ("const", Modifiers::CONST),
                ("in", Modifiers::IN),
                ("out", Modifiers::OUT),
            ] {
                let n = word.len() as u32;
                if self
                    .src
                    .get(i as usize..)
                    .is_some_and(|s| s.starts_with(word))
                    && self
                        .bytes()
                        .get((i + n) as usize)
                        .is_some_and(|c| c.is_ascii_whitespace())
                {
                    let j = self.skip_ws(i + n, r.end);
                    if self.ident_end(j, r.end).is_some() {
                        modifiers |= flag;
                        i = j;
                    }
                }
            }
            let bracketed = self.bytes().get(i as usize) == Some(&b'[');
            if bracketed {
                i = self.skip_ws(i + 1, r.end);
            }
            let Some(e) = self.ident_end(i, r.end) else {
                if out.is_empty()
                    && let Some(te) = te_end
                {
                    self.template_name_missing(te);
                }
                break;
            };
            let name = self.ident(Span::new(i, e));
            i = self.skip_ws(e, r.end);
            let mut default = None;
            if bracketed {
                if self.bytes().get(i as usize) == Some(&b'=') {
                    let d0 = self.p.diags.len();
                    let l0 = self.p.lx.diagnostics.len();
                    self.p.lx.seek(i + 1);
                    self.p.lx.next_token();
                    self.p.prev_end = i + 1;
                    let ty = self.p.parse_type();
                    let end = self.p.prev_end;
                    self.take_diags(d0, l0, doc);
                    if !matches!(self.p.ast.node(ty), Node::Missing {}) {
                        default = Some(ty);
                    }
                    i = self.skip_ws(end, r.end);
                }
                if self.bytes().get(i as usize) == Some(&b']') {
                    i += 1;
                }
            }
            let tp = self.p.ast.add(
                Node::TypeParam {
                    modifiers,
                    name,
                    // `@template {C} T, U`: only `T` is constrained (observed tsc 7).
                    constraint: if out.is_empty() { constraint } else { None },
                    default,
                },
                // From the first modifier, so the checker finds the keywords before the name.
                Span::new(
                    if modifiers.is_empty() {
                        self.p.ast.span(name).start
                    } else {
                        first
                    },
                    self.p.ast.span(name).end,
                ),
            );
            out.push(tp);
            i = self.skip_ws(i, r.end);
            if self.bytes().get(i as usize) == Some(&b',') {
                i += 1;
                continue;
            }
            break;
        }
        out
    }

    /// `@template {T}` with no name after the braces: TS1069 (a JSDoc error). Its position
    /// (observed tsc 7) is the end of the braces, or the `}` itself when `*/` follows with no
    /// space, or the next line's first character (the margin `*`) when the comment goes on.
    fn template_name_missing(&mut self, te: u32) {
        let b = self.orig.as_bytes();
        let mut j = te as usize;
        while matches!(b.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let pos = if b.get(j..j + 2) == Some(b"*/") {
            if j == te as usize {
                te.saturating_sub(1)
            } else {
                te
            }
        } else if matches!(b.get(j), Some(b'\n' | b'\r')) {
            let mut k = j;
            while b.get(k).is_some_and(|c| c.is_ascii_whitespace()) {
                k += 1;
            }
            if b.get(k..k + 2) == Some(b"*/") {
                te
            } else {
                k as u32
            }
        } else {
            te
        };
        if !self.diags.iter().any(|d| d.span.start == pos) {
            self.diags.push(Diagnostic::new(
                messages::UNEXPECTED_TOKEN_TYPE_PARAMETER_NAME_WITHOUT_BRACES,
                Span::new(pos, pos + 1),
                &[],
            ));
        }
    }

    fn templates_in(
        &mut self,
        raw: &[RawTag],
        names: &[&str],
        from: usize,
        to: usize,
    ) -> Vec<NodeId> {
        // `@template` tags anywhere in a comment with a typedef belong to its first typedef.
        let _ = (from, to);
        let mut out = Vec::new();
        for (k, &r) in raw.iter().enumerate() {
            if names[k] == "template" {
                out.extend(self.template(r));
            }
        }
        out
    }

    fn type_params_node(&mut self, tps: &[NodeId]) -> Option<NodeId> {
        if tps.is_empty() {
            return None;
        }
        let first = self.p.ast.span(tps[0]);
        let last = self.p.ast.span(tps[tps.len() - 1]);
        let params = self.p.ast.add_list(tps);
        Some(self.p.ast.add(
            Node::TypeParams { params },
            Span::new(first.start, last.end),
        ))
    }

    // ---- @typedef / @callback ----

    /// The name after `@typedef {T}` / `@callback`.
    fn alias_name(&mut self, from: u32, end: u32) -> Option<NodeId> {
        let i = self.skip_ws(from, end);
        let e = self.ident_end(i, end)?;
        // `NS.Name` is not modelled; references through `NS` stay unresolved.
        if self.bytes().get(e as usize) == Some(&b'.') {
            let root = self.src.get(i as usize..e as usize).unwrap_or("");
            let a = self.p.ast.intern(root);
            if !self.p.ast.jsdoc_namespaces.contains(&a) {
                self.p.ast.jsdoc_namespaces.push(a);
            }
            return None;
        }
        Some(self.ident(Span::new(i, e)))
    }

    /// The name of a `@typedef {T}` that has none, `e` the end of its type expression
    /// (observed tsc 7): the alias is still declared, named `(Missing)`, at `p`, the next
    /// token after the type in the comment (or `e` at the comment's end). tsc 7 reports
    /// `Identifier expected.` as a syntax error of the file at `p - 1`, and for checked
    /// files again at `p` unless another JSDoc error is there.
    fn missing_typedef_name(&mut self, e: u32, r: RawTag) -> NodeId {
        let q = self.skip_ws(e, r.end);
        let at_comment_end =
            q >= r.end && self.src.get(r.end as usize..r.end as usize + 2) == Some("*/");
        let p = if at_comment_end { e } else { q };
        let msg = messages::IDENTIFIER_EXPECTED;
        self.syntax
            .push(Diagnostic::new(msg, Span::new(p.saturating_sub(1), p), &[]));
        if !self.diags.iter().any(|d| d.span.start == p) {
            self.diags.push(Diagnostic::new(msg, Span::new(p, p), &[]));
        }
        let name = self.p.ast.intern("(Missing)");
        self.p.ast.add(Node::Ident { name }, Span::new(p, p))
    }

    /// Whether a `@typedef` takes child tags: its type is missing or `Object`/`object`
    /// (or an array of it), by the text between its braces (observed tsc 7).
    fn typedef_takes_children(&self, r: RawTag) -> bool {
        let i = self.skip_ws(r.name_end, r.end);
        let b = self.bytes();
        if b.get(i as usize) != Some(&b'{') {
            return true;
        }
        let mut depth = 0u32;
        let mut k = i as usize;
        let end = (r.end as usize).min(b.len());
        while k < end {
            match b[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        let text: String = self
            .src
            .get(i as usize + 1..k)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        matches!(
            text.as_str(),
            "Object" | "object" | "Object[]" | "object[]" | "Array<Object>" | "Array<object>"
        )
    }

    fn typedef(
        &mut self,
        r: RawTag,
        rest: &[RawTag],
        names: &[&str],
        templates: Vec<NodeId>,
    ) -> Option<NodeId> {
        let doc = self.doc_of(r);
        let mut i = self.skip_ws(r.name_end, r.end);
        let mut ty = None;
        let mut braced = false;
        if self.bytes().get(i as usize) == Some(&b'{') {
            let (t, e) = self.type_expr_end(i, doc);
            ty = t;
            i = e;
            braced = true;
        }
        let name = if braced && self.ident_end(self.skip_ws(i, r.end), r.end).is_none() {
            self.missing_typedef_name(i, r)
        } else if braced && ty.is_none() {
            return None;
        } else {
            self.alias_name(i, r.end)?
        };
        // Each `@type` child after the first: TS8033 just after its `}` (tsc 7).
        for (&t, _) in rest
            .iter()
            .zip(names)
            .filter(|(_, n)| **n == "type")
            .skip(1)
        {
            let open = self.skip_ws(t.name_end, t.end);
            if let Some(end) = self.closing_brace_end(open, t.end) {
                self.diags.push(Diagnostic::new(
                    messages::TYPEDEF_MULTIPLE_TYPE_TAGS,
                    Span::new(end, end),
                    &[],
                ));
            }
        }
        let props: Vec<ParamTag> = rest
            .iter()
            .zip(names)
            .filter(|(_, n)| matches!(**n, "property" | "prop"))
            .filter_map(|(&t, _)| self.property_tag(t))
            .collect();
        // `@typedef Name` + `@type {T}`: the first `@type` is the alias's type.
        if ty.is_none()
            && !braced
            && props.is_empty()
            && let Some((&t, _)) = rest.iter().zip(names).find(|(_, n)| **n == "type")
        {
            ty = self.tag_type(t, true);
        }
        let object_like = match ty {
            None => true,
            Some(t) => self.is_object_name(t),
        };
        let ty = if object_like && !props.is_empty() {
            let span = Span::new(r.at, r.end);
            let (types, tops) = self.nested_types(&props, false);
            let lit = self.object_literal_type(&props, &types, &tops, span);
            match ty {
                Some(t) if self.is_array_of_object(t) => {
                    self.p.ast.add(Node::ArrayType { elem: lit }, span)
                }
                _ => lit,
            }
        } else {
            match ty {
                Some(t) => t,
                None => self.p.ast.add(
                    Node::KeywordType {
                        kind: KeywordTypeKind::Any,
                    },
                    self.p.ast.span(name),
                ),
            }
        };
        let type_params = self.type_params_node(&templates);
        Some(self.p.ast.add(
            Node::TypeAlias {
                modifiers: Modifiers::NONE,
                name,
                type_params,
                ty,
            },
            Span::new(r.at + 1, r.end),
        ))
    }

    /// The position after the `}` matching the `{` at `open` (before `end`), if any.
    fn closing_brace_end(&self, open: u32, end: u32) -> Option<u32> {
        let b = self.bytes();
        if b.get(open as usize) != Some(&b'{') {
            return None;
        }
        let mut depth = 0u32;
        let mut i = open;
        while i < end {
            match b.get(i as usize) {
                Some(b'{') => depth += 1,
                Some(b'}') => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                None => return None,
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn callback(
        &mut self,
        r: RawTag,
        rest: &[RawTag],
        names: &[&str],
        templates: Vec<NodeId>,
    ) -> Option<NodeId> {
        let name = self.alias_name(r.name_end, r.end)?;
        let mut params = Vec::new();
        let mut ret = None;
        let mut this_ty = None;
        for (&t, &n) in rest.iter().zip(names) {
            match n {
                "param" | "arg" | "argument" => {
                    if let Some(pt) = self.param_tag(t) {
                        params.push(pt);
                    }
                }
                "returns" | "return" => ret = self.tag_type(t, false).or(ret),
                "this" => this_ty = self.tag_type(t, true).or(this_ty),
                _ => {}
            }
        }
        let span = Span::new(r.at + 1, r.end);
        let mut nodes = Vec::new();
        // `@this {T}`: a leading `this: T` parameter.
        if let Some(t) = this_ty {
            let tspan = self.p.ast.span(t);
            let this_name = self.p.ast.intern("this");
            let pattern = self.p.ast.add(Node::Ident { name: this_name }, tspan);
            let param = self.p.ast.add(
                Node::Param {
                    decorators: NodeList::EMPTY,
                    modifiers: Modifiers::NONE,
                    rest: false,
                    pattern,
                    optional: false,
                    type_ann: Some(t),
                    init: None,
                },
                tspan,
            );
            nodes.push(param);
        }
        let tops: Vec<usize> = (0..params.len())
            .filter(|&k| params[k].name.segments.len() == 1)
            .collect();
        let (types, _) = self.nested_types(&params, true);
        for &k in &tops {
            let seg = params[k].name.segments[0];
            let pattern = self.ident(seg);
            let ty = types[k];
            let (ty, rest, optional) = self.unwrap_param_type(ty);
            let optional = optional || params[k].name.optional;
            let param = self.p.ast.add(
                Node::Param {
                    decorators: NodeList::EMPTY,
                    modifiers: Modifiers::NONE,
                    rest,
                    pattern,
                    optional: optional && !rest,
                    type_ann: ty,
                    init: None,
                },
                params[k].span,
            );
            nodes.push(param);
        }
        let params_list = self.p.ast.add_list(&nodes);
        let return_type = match ret {
            Some(t) => t,
            None => self.p.ast.add(
                Node::KeywordType {
                    kind: KeywordTypeKind::Any,
                },
                Span::at(self.p.ast.span(name).end),
            ),
        };
        let fty = self.p.ast.add(
            Node::FunctionType {
                type_params: None,
                params: params_list,
                return_type,
            },
            span,
        );
        let type_params = self.type_params_node(&templates);
        Some(self.p.ast.add(
            Node::TypeAlias {
                modifiers: Modifiers::NONE,
                name,
                type_params,
                ty: fty,
            },
            span,
        ))
    }

    /// `Object` / `object` (a JSDoc object whose properties come from tags).
    fn is_object_name(&self, t: NodeId) -> bool {
        let ast = &self.p.ast;
        match *ast.node(t) {
            Node::KeywordType {
                kind: KeywordTypeKind::Object,
            } => true,
            Node::TypeRef {
                name,
                type_args: None,
            } => ast.name_of(name) == Some("Object"),
            _ => false,
        }
    }

    /// `Object[]` / `Array<Object>`.
    fn is_array_of_object(&self, t: NodeId) -> bool {
        let ast = &self.p.ast;
        match *ast.node(t) {
            Node::ArrayType { elem } => self.is_object_name(elem),
            Node::TypeRef {
                name,
                type_args: Some(ta),
            } if ast.name_of(name) == Some("Array") => match *ast.node(ta) {
                Node::TypeArgs { args } => {
                    ast.list(args).len() == 1 && self.is_object_name(ast.list(args)[0])
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// The name in `s`, escapes decoded.
    fn seg_text(&self, s: Span) -> std::borrow::Cow<'a, str> {
        let src: &'a str = self.src;
        let raw = src.get(s.start as usize..s.end as usize).unwrap_or("");
        if !raw.contains('\\') {
            return std::borrow::Cow::Borrowed(raw);
        }
        let mut out = String::new();
        let mut k = 0;
        while k < raw.len() {
            if let Some((c, len)) = unicode_escape(&raw[k..]) {
                out.push(c);
                k += len;
            } else if let Some(c) = raw[k..].chars().next() {
                out.push(c);
                k += c.len_utf8();
            } else {
                break;
            }
        }
        std::borrow::Cow::Owned(out)
    }

    fn spend_desugar_work(&mut self, work: usize, span: Span) -> bool {
        if self.p.halted {
            return false;
        }
        if work > self.desugar_work || self.p.ast.node_count() > self.p.node_budget {
            self.p.halted = true;
            self.diags
                .push(Diagnostic::new(messages::PARSER_NO_PROGRESS, span, &[]));
            return false;
        }
        self.desugar_work -= work;
        true
    }

    /// Group equal paths, then build each prefix once, from the leaves up. Only explicitly
    /// tagged parents get members; a missing intermediate tag is still TS8032.
    fn nested_types(
        &mut self,
        tags: &[ParamTag],
        param_roots: bool,
    ) -> (Vec<Option<NodeId>>, Vec<usize>) {
        let mut types: Vec<_> = tags.iter().map(|t| t.ty).collect();
        let mut paths = HashMap::new();
        let mut unique = Vec::new();
        let mut canonical = Vec::new();
        for (k, tag) in tags.iter().enumerate() {
            let work = tag.name.segments.iter().fold(1usize, |n, s| {
                n.saturating_add((s.end - s.start) as usize + 1)
            });
            if !self.spend_desugar_work(work, tag.span) {
                return (types, Vec::new());
            }
            let path: Vec<_> = tag
                .name
                .segments
                .iter()
                .map(|&s| self.seg_text(s))
                .collect();
            let first = *paths.entry(path).or_insert_with(|| {
                unique.push(k);
                k
            });
            canonical.push(first);
        }
        let mut children = vec![Vec::new(); tags.len()];
        let mut tops = Vec::new();
        for (path, &k) in &paths {
            if path.len() == 1 {
                tops.push(k);
            } else if let Some(&parent) = paths.get(&path[..path.len() - 1]) {
                children[parent].push(k);
            }
        }
        // Hash maps only find parents; source order determines member order.
        tops.sort_unstable();
        for members in &mut children {
            members.sort_unstable();
        }
        unique.sort_by_key(|&k| std::cmp::Reverse(tags[k].name.segments.len()));
        for k in unique {
            let mut ty = types[k];
            let nested_optional = (!param_roots || tags[k].name.segments.len() > 1)
                && ty.is_some_and(|t| matches!(self.p.ast.node(t), Node::OptionalType { .. }));
            if nested_optional
                && let Some(t) = ty
                && let Node::OptionalType { ty: inner } = *self.p.ast.node(t)
            {
                ty = Some(inner);
            }
            if !children[k].is_empty()
                && ty.is_none_or(|t| self.is_object_name(t) || self.is_array_of_object(t))
            {
                let lit = self.object_literal_type(tags, &types, &children[k], tags[k].span);
                ty = Some(match ty {
                    Some(t) if self.is_array_of_object(t) => {
                        self.p.ast.add(Node::ArrayType { elem: lit }, tags[k].span)
                    }
                    _ => lit,
                });
                if nested_optional {
                    ty = ty.map(|ty| self.p.ast.add(Node::OptionalType { ty }, tags[k].span));
                }
                types[k] = ty;
            }
            if self.p.halted {
                break;
            }
        }
        for (k, first) in canonical.into_iter().enumerate() {
            types[k] = types[first];
        }
        (types, tops)
    }

    /// `{ a: A; b?: B }` from the distinct immediate child tags, in source order.
    fn object_literal_type(
        &mut self,
        tags: &[ParamTag],
        types: &[Option<NodeId>],
        children: &[usize],
        span: Span,
    ) -> NodeId {
        let mut members = Vec::new();
        for &k in children {
            if !self.spend_desugar_work(3, tags[k].span) {
                break;
            }
            let segs = &tags[k].name.segments;
            let key = self.ident(segs[segs.len() - 1]);
            let mut ty = types[k];
            let mut optional = tags[k].name.optional;
            if let Some(t) = ty
                && let Node::OptionalType { ty: inner } = *self.p.ast.node(t)
            {
                ty = Some(inner);
                optional = true;
            }
            let m = self.p.ast.add(
                Node::PropertySignature {
                    modifiers: Modifiers::NONE,
                    key,
                    optional,
                    type_ann: ty,
                },
                tags[k].span,
            );
            members.push(m);
        }
        let members = self.p.ast.add_list(&members);
        self.p.ast.add(Node::TypeLiteral { members }, span)
    }

    /// TS1016 at the first required parameter after one that JSDoc made optional
    /// (`@param {T=} a`, `@param [a]`); the parse-time check saw neither as optional.
    fn required_after_jsdoc_optional(&mut self, plist: &[NodeId]) {
        let mut seen_optional = false;
        for &p in plist {
            let Node::Param {
                rest,
                pattern,
                optional,
                init,
                ..
            } = *self.p.ast.node(p)
            else {
                continue;
            };
            if rest {
                return;
            }
            if seen_optional && !optional && init.is_none() {
                let span = self.p.ast.span(pattern);
                self.diags.push(Diagnostic::new(
                    messages::REQUIRED_AFTER_OPTIONAL,
                    span,
                    &[],
                ));
                return;
            }
            seen_optional |= optional;
        }
    }

    /// `...T` is a rest of `T[]`, `T=` optional `T`: `(type, rest, optional)`.
    fn unwrap_param_type(&mut self, ty: Option<NodeId>) -> (Option<NodeId>, bool, bool) {
        let Some(t) = ty else {
            return (None, false, false);
        };
        match *self.p.ast.node(t) {
            Node::RestType { ty: inner } => {
                let span = self.p.ast.span(t);
                let arr = self.p.ast.add(Node::ArrayType { elem: inner }, span);
                (Some(arr), true, false)
            }
            Node::OptionalType { ty: inner } => (Some(inner), false, true),
            _ => (Some(t), false, false),
        }
    }

    // ---- Functions ----

    /// TS8024, TS8029 and TS8032 (observed tsc 7, which reports them for checked files):
    /// the `@param` tags of a comment on function `f` that name none of its parameters.
    /// A top-level tag is fine when it names a parameter, sits at the position of a binding
    /// pattern, or has nested `name.x` tags; when `f` uses `arguments`, only its last tag
    /// with a non-array type is reported (TS8029). A nested `a.b.c` needs an earlier `a.b`.
    fn check_param_names(&mut self, f: NodeId, params: &[ParamTag]) {
        let (fparams, body) = match *self.p.ast.node(f) {
            Node::FunctionDecl { params, body, .. } | Node::FunctionExpr { params, body, .. } => {
                (params, body)
            }
            Node::Arrow { params, body, .. } => (params, Some(body)),
            _ => return,
        };
        if params.is_empty() {
            return;
        }
        let plist: Vec<NodeId> = self.p.ast.list(fparams).to_vec();
        let full = |d: &Self, t: &ParamTag, n: usize| -> String {
            t.name.segments[..n]
                .iter()
                .map(|&s| d.seg_text(s).into_owned())
                .collect::<Vec<_>>()
                .join(".")
        };
        let names: Vec<String> = params
            .iter()
            .map(|t| full(self, t, t.name.segments.len()))
            .collect();
        let uses_arguments = body.is_some_and(|b| self.uses_arguments(b));
        let last = params.len() - 1;
        let mut pos = 0usize;
        for (k, t) in params.iter().enumerate() {
            let Some(&first) = t.name.segments.first() else {
                continue;
            };
            let n = t.name.segments.len();
            if n > 1 {
                let parent = full(self, t, n - 1);
                if names[..k].contains(&parent) {
                    continue;
                }
                pos += 1;
                if !uses_arguments {
                    let name = names[k].clone();
                    self.diags.push(Diagnostic::new(
                        messages::JSDOC_QUALIFIED_PARAM,
                        Span::new(first.start, t.name.segments[n - 1].end),
                        &[&name, &parent],
                    ));
                }
                continue;
            }
            let at = pos;
            pos += 1;
            let name = &names[k];
            let matched = plist
                .iter()
                .enumerate()
                .any(|(i, &p)| match *self.p.ast.node(p) {
                    Node::Param { pattern, .. } => match self.p.ast.node(pattern) {
                        Node::Ident { .. } => self.p.ast.name_of(pattern) == Some(name.as_str()),
                        Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => i == at,
                        _ => true,
                    },
                    _ => true,
                });
            let prefix = format!("{name}.");
            if matched || names[k + 1..].iter().any(|m| m.starts_with(&prefix)) {
                continue;
            }
            let span = Span::new(first.start, first.end);
            if !uses_arguments {
                let name = name.clone();
                self.diags.push(Diagnostic::new(
                    messages::JSDOC_PARAM_NO_MATCH,
                    span,
                    &[&name],
                ));
            } else if k == last && t.ty.is_some_and(|ty| self.is_non_array_type(ty)) {
                let name = name.clone();
                self.diags.push(Diagnostic::new(
                    messages::JSDOC_PARAM_NOT_ARRAY,
                    span,
                    &[&name],
                ));
            }
        }
    }

    /// A JSDoc parameter type that is surely not an array (a keyword, literal or object
    /// type); a reference other than `Array` is not known here.
    fn is_non_array_type(&self, t: NodeId) -> bool {
        matches!(
            self.p.ast.node(t),
            Node::KeywordType { .. } | Node::TypeLiteral { .. } | Node::FunctionType { .. }
        )
    }

    /// Whether a function body mentions `arguments` (outside nested non-arrow functions
    /// and classes; property names excluded).
    fn uses_arguments(&self, body: NodeId) -> bool {
        let ast = &self.p.ast;
        let mut stack = vec![body];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 1_000_000 {
                return true;
            }
            match *ast.node(n) {
                Node::Ident { .. } if ast.name_of(n) == Some("arguments") => return true,
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. } => continue,
                Node::Member { obj, .. } => {
                    stack.push(obj);
                    continue;
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_function(
        &mut self,
        f: NodeId,
        params: &[ParamTag],
        returns: Option<NodeId>,
        this_ty: Option<NodeId>,
        templates: &[NodeId],
        raw: &[RawTag],
        names: &[&str],
    ) {
        let _ = (raw, names);
        let (fparams, is_arrow) = match *self.p.ast.node(f) {
            Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => {
                (params, false)
            }
            Node::Arrow { params, .. } => (params, true),
            _ => return,
        };
        let plist: Vec<NodeId> = self.p.ast.list(fparams).to_vec();
        let tops: Vec<usize> = (0..params.len())
            .filter(|&k| params[k].name.segments.len() == 1)
            .collect();
        let mut made_optional = false;
        let is_setter = self.parents.parent(f).is_some_and(|m| {
            matches!(
                self.p.ast.node(m),
                Node::MethodDef {
                    kind: MethodKind::Set,
                    ..
                }
            )
        });
        let (types, _) = self.nested_types(params, true);
        for (pos, &k) in tops.iter().enumerate() {
            let name = self.seg_text(params[k].name.segments[0]);
            // By name; a binding pattern by position.
            let target = plist
                .iter()
                .copied()
                .find(|&p| match *self.p.ast.node(p) {
                    Node::Param { pattern, .. } => self.p.ast.name_of(pattern) == Some(&*name),
                    _ => false,
                })
                .or_else(|| {
                    plist.get(pos).copied().filter(|&p| {
                        matches!(*self.p.ast.node(p), Node::Param { pattern, .. }
                            if matches!(self.p.ast.node(pattern), Node::ObjectPattern { .. } | Node::ArrayPattern { .. }))
                    })
                });
            let ty = types[k];
            if let Some(t) = ty {
                self.record(f, JsDocTagKind::Param, Some(t), params[k].span);
            } else {
                self.record(f, JsDocTagKind::Param, None, params[k].span);
            }
            let Some(target) = target else { continue };
            let (ty, _rest, opt) = self.unwrap_param_type(ty);
            let optional = opt || params[k].name.optional;
            if ty.is_none() && !optional {
                continue;
            }
            if let Node::Param {
                type_ann,
                optional: o,
                init,
                ..
            } = self.p.ast.jsdoc_node_mut(target)
                && type_ann.is_none()
            {
                // `@param [a]`: optional, still untyped.
                *type_ann = ty;
                if optional && init.is_none() && !*o {
                    *o = true;
                    made_optional = true;
                    if is_setter {
                        self.diags.push(Diagnostic::new(
                            messages::SET_ACCESSOR_OPTIONAL_PARAMETER,
                            params[k].span,
                            &[],
                        ));
                    }
                }
            }
        }
        if made_optional {
            self.required_after_jsdoc_optional(&plist);
        }
        if let Some(r) = returns {
            self.record(f, JsDocTagKind::Returns, Some(r), self.p.ast.span(r));
            let is_ctor = self.parents.parent(f).is_some_and(|m| {
                matches!(
                    self.p.ast.node(m),
                    Node::MethodDef {
                        kind: MethodKind::Constructor | MethodKind::Set,
                        ..
                    }
                )
            });
            if !is_ctor {
                match self.p.ast.jsdoc_node_mut(f) {
                    Node::FunctionDecl { return_type, .. }
                    | Node::FunctionExpr { return_type, .. }
                    | Node::Arrow { return_type, .. } => {
                        if return_type.is_none() {
                            *return_type = Some(r);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(t) = this_ty
            && !is_arrow
        {
            self.record(f, JsDocTagKind::This, Some(t), self.p.ast.span(t));
            let has_this = plist.first().is_some_and(|&p| {
                matches!(*self.p.ast.node(p), Node::Param { pattern, .. } if self.p.ast.name_of(pattern) == Some("this"))
            });
            if !has_this {
                let span = self.p.ast.span(t);
                let this_name = self.p.ast.intern("this");
                let pattern = self.p.ast.add(Node::Ident { name: this_name }, span);
                let param = self.p.ast.add(
                    Node::Param {
                        decorators: NodeList::EMPTY,
                        modifiers: Modifiers::NONE,
                        rest: false,
                        pattern,
                        optional: false,
                        type_ann: Some(t),
                        init: None,
                    },
                    span,
                );
                let mut all = vec![param];
                all.extend(plist.iter().copied());
                let list = self.p.ast.add_list(&all);
                match self.p.ast.jsdoc_node_mut(f) {
                    Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => {
                        *params = list
                    }
                    _ => {}
                }
            }
        }
        self.set_type_params(f, templates);
    }

    fn set_type_params(&mut self, n: NodeId, templates: &[NodeId]) {
        if templates.is_empty() {
            return;
        }
        let has = match *self.p.ast.node(n) {
            Node::FunctionDecl { type_params, .. }
            | Node::FunctionExpr { type_params, .. }
            | Node::Arrow { type_params, .. }
            | Node::ClassDecl { type_params, .. }
            | Node::ClassExpr { type_params, .. } => type_params.is_some(),
            _ => return,
        };
        if has {
            return;
        }
        let tp = self.type_params_node(templates);
        self.record(
            n,
            JsDocTagKind::Template,
            None,
            self.p.ast.span(templates[0]),
        );
        match self.p.ast.jsdoc_node_mut(n) {
            Node::FunctionDecl { type_params, .. }
            | Node::FunctionExpr { type_params, .. }
            | Node::Arrow { type_params, .. }
            | Node::ClassDecl { type_params, .. }
            | Node::ClassExpr { type_params, .. } => *type_params = tp,
            _ => {}
        }
    }

    // ---- @type and @satisfies ----

    fn apply_type(&mut self, t: &Targets, ty: NodeId, record_host: Option<NodeId>) {
        let span = self.p.ast.span(ty);
        let js = self.p.ast.jsdoc_start;
        if let Some(p) = t.paren {
            // `/** @type {T} */ (expr)`: a type assertion inside the parentheses (kept for
            // emit, which prints this tree).
            if let Node::Paren { expr } = *self.p.ast.node(p) {
                let a = self
                    .p
                    .ast
                    .add(Node::TypeAssertion { ty, expr }, self.p.ast.span(p));
                *self.p.ast.jsdoc_node_mut(p) = Node::Paren { expr: a };
                self.record(p, JsDocTagKind::Type, Some(ty), span);
            }
            return;
        }
        if let Some(v) = t.var {
            if let Node::VarDeclarator { type_ann, .. } = self.p.ast.jsdoc_node_mut(v)
                && type_ann.is_none_or(|a| a.raw() >= js)
            {
                *type_ann = Some(ty);
            }
            self.record(v, JsDocTagKind::Type, Some(ty), span);
            return;
        }
        if let Some(m) = t.member
            && matches!(self.p.ast.node(m), Node::PropertyDef { .. })
        {
            if let Node::PropertyDef { type_ann, .. } = self.p.ast.jsdoc_node_mut(m)
                && type_ann.is_none_or(|a| a.raw() >= js)
            {
                *type_ann = Some(ty);
            }
            self.record(m, JsDocTagKind::Type, Some(ty), span);
            return;
        }
        if let Some(p) = t.param {
            if let Node::Param { type_ann, .. } = self.p.ast.jsdoc_node_mut(p)
                && type_ann.is_none_or(|a| a.raw() >= js)
            {
                *type_ann = Some(ty);
            }
            self.record(p, JsDocTagKind::Type, Some(ty), span);
            return;
        }
        if let Some(f) = t.func {
            self.record(f, JsDocTagKind::Type, Some(ty), span);
            return;
        }
        if let Some(a) = t.assign
            && let Node::Assign { left, .. } = *self.p.ast.node(a)
        {
            self.record(left, JsDocTagKind::Type, Some(ty), span);
            return;
        }
        if let Some(p) = t.prop {
            self.record(p, JsDocTagKind::Type, Some(ty), span);
            return;
        }
        if let Some(h) = record_host {
            self.record(h, JsDocTagKind::Type, Some(ty), span);
        }
    }

    fn apply_satisfies(&mut self, t: &Targets, ty: NodeId, record_host: Option<NodeId>) {
        let span = self.p.ast.span(ty);
        if let Some(p) = t.paren {
            if let Node::Paren { expr } = *self.p.ast.node(p) {
                let a = self
                    .p
                    .ast
                    .add(Node::Satisfies { expr, ty }, self.p.ast.span(p));
                *self.p.ast.jsdoc_node_mut(p) = Node::Paren { expr: a };
                self.record(p, JsDocTagKind::Satisfies, Some(ty), span);
            }
            return;
        }
        let wrap = |d: &mut Self, e: NodeId| -> NodeId {
            let s = d.p.ast.span(e);
            d.p.ast.add(Node::Satisfies { expr: e, ty }, s)
        };
        if let Some(v) = t.var {
            if let Node::VarDeclarator { init: Some(i), .. } = *self.p.ast.node(v) {
                let w = wrap(self, i);
                if let Node::VarDeclarator { init, .. } = self.p.ast.jsdoc_node_mut(v) {
                    *init = Some(w);
                }
            }
            self.record(v, JsDocTagKind::Satisfies, Some(ty), span);
            return;
        }
        if let Some(a) = t.assign
            && let Node::Assign { right, .. } = *self.p.ast.node(a)
        {
            let w = wrap(self, right);
            if let Node::Assign { right, .. } = self.p.ast.jsdoc_node_mut(a) {
                *right = w;
            }
            self.record(a, JsDocTagKind::Satisfies, Some(ty), span);
            return;
        }
        if let Some(e) = t.export_default
            && let Node::ExportDefaultExpr { expr } = *self.p.ast.node(e)
        {
            let w = wrap(self, expr);
            if let Node::ExportDefaultExpr { expr } = self.p.ast.jsdoc_node_mut(e) {
                *expr = w;
            }
            self.record(e, JsDocTagKind::Satisfies, Some(ty), span);
            return;
        }
        if let Some(p) = t.prop
            && let Node::Property { value, .. } = *self.p.ast.node(p)
        {
            let w = wrap(self, value);
            if let Node::Property { value, .. } = self.p.ast.jsdoc_node_mut(p) {
                *value = w;
            }
            self.record(p, JsDocTagKind::Satisfies, Some(ty), span);
            return;
        }
        if let Some(h) = record_host {
            self.record(h, JsDocTagKind::Satisfies, Some(ty), span);
        }
    }

    // ---- Classes ----

    /// The text of a class's `extends` expression when it is an entity name (`B`, `ns.B`).
    fn class_extends_text(&self, c: NodeId) -> Option<String> {
        let ast = &self.p.ast;
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(c)
        else {
            return None;
        };
        let e = ast
            .list(heritage)
            .iter()
            .find_map(|&h| match *ast.node(h) {
                Node::HeritageClause {
                    kind: HeritageKind::Extends,
                    types,
                } => ast.list(types).first().copied(),
                _ => None,
            })?;
        let Node::ExprWithTypeArgs { expr, .. } = *ast.node(e) else {
            return None;
        };
        self.entity_text(expr).map(|(t, _)| t)
    }

    /// The rightmost identifier of an entity name (`c` of `a.b.c`, a qualified name or
    /// member access chain of identifiers), with its span; `None` for another node.
    fn entity_text(&self, n: NodeId) -> Option<(String, Span)> {
        let ast = &self.p.ast;
        let last = match *ast.node(n) {
            Node::Ident { .. } => n,
            Node::Member { prop, .. } => prop,
            Node::QualifiedName { right, .. } => right,
            _ => return None,
        };
        let mut cur = n;
        for _ in 0..64 {
            match *ast.node(cur) {
                Node::Ident { .. } => {
                    let text = ast.name_of(last)?.to_owned();
                    return Some((text, ast.span(last)));
                }
                Node::Member { obj, .. } => cur = obj,
                Node::QualifiedName { left, .. } => cur = left,
                _ => return None,
            }
        }
        None
    }

    fn extends_tag(&mut self, t: &Targets, ty: NodeId, span: Span, tag: &str) {
        let Some(c) = t.class else { return };
        self.record(c, JsDocTagKind::Extends, Some(ty), span);
        // TS8023: `@augments A` on `class C extends B`.
        if let Node::TypeRef { name, .. } = *self.p.ast.node(ty)
            && let Some(base) = self.class_extends_text(c)
            && let Some((text, at)) = self.entity_text(name)
            && text != base
        {
            self.diags.push(Diagnostic::new(
                messages::JSDOC_EXTENDS_MISMATCH,
                at,
                &[tag, &text, &base],
            ));
        }
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *self.p.ast.node(c)
        else {
            return;
        };
        let Node::TypeRef {
            type_args: Some(ta),
            ..
        } = *self.p.ast.node(ty)
        else {
            return;
        };
        for &h in self.p.ast.list(heritage).to_vec().iter() {
            if let Node::HeritageClause {
                kind: HeritageKind::Extends,
                types,
            } = *self.p.ast.node(h)
                && let Some(&e) = self.p.ast.list(types).first()
                && let Node::ExprWithTypeArgs { type_args, .. } = self.p.ast.jsdoc_node_mut(e)
                && type_args.is_none()
            {
                *type_args = Some(ta);
            }
        }
    }

    fn implements_tag(&mut self, t: &Targets, ty: NodeId, span: Span) {
        let Some(c) = t.class else { return };
        self.record(c, JsDocTagKind::Implements, Some(ty), span);
        let Node::TypeRef { name, type_args } = *self.p.ast.node(ty) else {
            return;
        };
        let Some(expr) = self.entity_to_expr(name) else {
            return;
        };
        let ety = self
            .p
            .ast
            .add(Node::ExprWithTypeArgs { expr, type_args }, span);
        let types = self.p.ast.add_list(&[ety]);
        let clause = self.p.ast.add(
            Node::HeritageClause {
                kind: HeritageKind::Implements,
                types,
            },
            span,
        );
        let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) =
            *self.p.ast.node(c)
        else {
            return;
        };
        let mut all = self.p.ast.list(heritage).to_vec();
        all.push(clause);
        let list = self.p.ast.add_list(&all);
        match self.p.ast.jsdoc_node_mut(c) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => *heritage = list,
            _ => {}
        }
    }

    /// `A.B` as a member expression.
    fn entity_to_expr(&mut self, n: NodeId) -> Option<NodeId> {
        let mut chain = Vec::new();
        let mut cur = n;
        let mut expr = loop {
            match *self.p.ast.node(cur) {
                Node::Ident { .. } => break cur,
                Node::QualifiedName { left, right } => {
                    chain.push((right, self.p.ast.span(cur)));
                    cur = left;
                }
                _ => return None,
            }
        };
        for (prop, span) in chain.into_iter().rev() {
            expr = self.p.add_node(
                span,
                Node::Member {
                    obj: expr,
                    optional: false,
                    prop,
                },
            );
        }
        Some(expr)
    }

    // ---- @import ----

    fn import_tag(&mut self, r: RawTag, doc: Span) -> Option<NodeId> {
        // `@import ...` is `import type ...` starting at the tag name.
        let start = r.at + 1;
        let d0 = self.p.diags.len();
        // The last tag of a comment is parsed to the comment's end, where tsc 7's tokens
        // end (observed): the source is cut at the `*/`.
        let src: &'a str = self.src;
        let last = src.get(r.end as usize..r.end as usize + 2) == Some("*/");
        let short = if last { &src[..r.end as usize] } else { src };
        let saved_lx = std::mem::replace(&mut self.p.lx, ts_r_lexer::Lexer::new(short));
        let saved_src = std::mem::replace(&mut self.p.src, short);
        self.p.lx.seek(start);
        self.p.lx.next_token();
        self.p.prev_end = start;
        let imp = self.p.parse_import(start, Modifiers::NONE);
        let lexed: Vec<Diagnostic> = self.p.lx.diagnostics.drain(..).collect();
        self.p.lx = saved_lx;
        self.p.src = saved_src;
        // The missing `;` after the specifier (the comment goes on) is not an error.
        let source_end = match *self.p.ast.node(imp) {
            Node::ImportDecl { source, .. } => self.p.ast.span(source).end,
            _ => start,
        };
        let mut new: Vec<Diagnostic> = self.p.diags.drain(d0..).collect();
        new.extend(lexed);
        let source_missing = match *self.p.ast.node(imp) {
            Node::ImportDecl { source, .. } => {
                matches!(self.p.ast.node(source), Node::Missing {})
            }
            _ => true,
        };
        if last && source_missing {
            self.import_tag_specifier_errors(&mut new, start, r.end, source_end);
        }
        // `@import x = require("m")`: no import-equals form; the token after `x` is where
        // `from` and the specifier are missing (observed tsc 7).
        if let Node::ImportEquals { name, .. } = *self.p.ast.node(imp) {
            let at = self.skip_ws(self.p.ast.span(name).end, r.end);
            let span = Span::new(at, at + 1);
            new.push(Diagnostic::new(messages::EXPECTED, span, &["from"]));
            new.push(Diagnostic::new(
                messages::STRING_LITERAL_EXPECTED,
                span,
                &[],
            ));
        }
        let mut bad = false;
        for d in new {
            if d.span.start >= source_end && !source_missing {
                // After the specifier: only errors before the comment's end count, and
                // they keep the import.
                if last && d.span.start >= doc.start && d.span.start <= doc.end {
                    self.diags.push(d);
                }
                continue;
            }
            bad = true;
            if d.span.start >= doc.start && d.span.start <= doc.end {
                self.diags.push(d);
            }
        }
        self.p.deferred.clear();
        if bad {
            return None;
        }
        match self.p.ast.jsdoc_node_mut(imp) {
            Node::ImportDecl {
                type_only,
                has_clause: true,
                ..
            } => {
                *type_only = true;
            }
            _ => return None,
        }
        let span = Span::new(r.at + 1, source_end);
        self.p.ast.set_span(imp, span);
        Some(imp)
    }

    /// tsc 7's errors for an `@import` tag without a module specifier, the last tag of its
    /// comment (`ce` the comment's `*/`), from the parser's (observed): at the comment's end
    /// the specifier is `Expression expected.` at the end of the last token (`at`, where
    /// the missing specifier is), after `'from' expected.` at the end unless nothing follows
    /// `import`; before it, a token that is not `from` gets both `'from' expected.` and
    /// `String literal expected.`.
    fn import_tag_specifier_errors(&self, new: &mut Vec<Diagnostic>, start: u32, ce: u32, at: u32) {
        let from_expected = |d: &Diagnostic| d.code == 1005 && d.message == "'from' expected.";
        let empty = self.skip_ws(start + "import".len() as u32, ce) >= ce;
        let mut out = Vec::with_capacity(new.len() + 1);
        for d in new.drain(..) {
            if d.span.start >= ce && (from_expected(&d) || d.code == 1141) {
                if from_expected(&d) && !empty {
                    out.push(d);
                }
                out.push(Diagnostic::new(
                    messages::EXPRESSION_EXPECTED,
                    Span::new(at, at),
                    &[],
                ));
            } else if from_expected(&d) {
                let span = d.span;
                out.push(d);
                out.push(Diagnostic::new(
                    messages::STRING_LITERAL_EXPECTED,
                    span,
                    &[],
                ));
            } else {
                out.push(d);
            }
        }
        *new = out;
    }

    // ---- Statement insertion ----

    /// Inserts `stmts` (typedef aliases) before the statement the comment documents, or
    /// into the innermost statement list around an unattached comment.
    fn insert_statements(&mut self, doc: Span, hosts: &[NodeId], stmts: &[NodeId]) {
        let (owner, before) = match hosts.first() {
            Some(&h) => {
                let mut cur = h;
                let mut found = None;
                let mut guard = 0;
                while let Some(p) = self.parents.parent(cur) {
                    guard += 1;
                    if guard > 10_000 {
                        break;
                    }
                    if is_list_owner(self.p.ast.node(p)) {
                        found = Some((p, self.p.ast.span(cur).start));
                        break;
                    }
                    cur = p;
                }
                match found {
                    Some(f) => f,
                    None => (self.p.ast.root, doc.start),
                }
            }
            None => (self.owner_at(doc.start), doc.start),
        };
        for &s in stmts {
            self.inserts.push((owner, before, s));
        }
    }

    /// The innermost statement list owner whose span contains `pos`.
    fn owner_at(&self, pos: u32) -> NodeId {
        let ast = &self.p.ast;
        let mut cur = ast.root;
        let mut owner = ast.root;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 10_000 {
                return owner;
            }
            let mut next = None;
            ast.for_each_child(cur, |c| {
                let s = ast.span(c);
                if next.is_none() && s.start < pos && pos < s.end {
                    next = Some(c);
                }
            });
            let Some(n) = next else { return owner };
            if is_list_owner(ast.node(n)) {
                owner = n;
            }
            cur = n;
        }
    }

    fn finish(&mut self) {
        if self.inserts.is_empty() {
            return;
        }
        let mut inserts = std::mem::take(&mut self.inserts);
        inserts.sort_by_key(|&(o, b, _)| (o, b));
        let mut k = 0;
        while k < inserts.len() {
            let owner = inserts[k].0;
            let mut j = k;
            while j < inserts.len() && inserts[j].0 == owner {
                j += 1;
            }
            let group = &inserts[k..j];
            let list = match *self.p.ast.node(owner) {
                Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => {
                    stmts
                }
                Node::StaticBlock { body } => body,
                Node::SwitchCase { body, .. } => body,
                _ => {
                    k = j;
                    continue;
                }
            };
            let old = self.p.ast.list(list).to_vec();
            let mut out = Vec::with_capacity(old.len() + group.len());
            let mut g = 0;
            for &s in &old {
                let start = self.p.ast.span(s).start;
                while g < group.len() && group[g].1 <= start {
                    out.push(group[g].2);
                    g += 1;
                }
                out.push(s);
            }
            while g < group.len() {
                out.push(group[g].2);
                g += 1;
            }
            let new = self.p.ast.add_list(&out);
            match self.p.ast.jsdoc_node_mut(owner) {
                Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => {
                    *stmts = new
                }
                Node::StaticBlock { body } | Node::SwitchCase { body, .. } => *body = new,
                _ => {}
            }
            k = j;
        }
    }
}

fn is_list_owner(n: &Node) -> bool {
    matches!(
        n,
        Node::Program { .. }
            | Node::Block { .. }
            | Node::ModuleBlock { .. }
            | Node::StaticBlock { .. }
            | Node::SwitchCase { .. }
    )
}

fn skip_parens(ast: &Ast, mut e: NodeId) -> NodeId {
    let mut guard = 0;
    while let Node::Paren { expr } = *ast.node(e) {
        e = expr;
        guard += 1;
        if guard > 10_000 {
            break;
        }
    }
    e
}

/// The start of the first token at or after `i`, skipping whitespace and comments.
fn next_token_pos(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == 0x0b || b[i] == 0x0c) {
            i += 1;
        }
        if b.get(i..i + 2) == Some(b"//") {
            while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        if b.get(i..i + 2) == Some(b"/*") {
            match b[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(k) => i = i + 2 + k + 2,
                None => return b.len(),
            }
            continue;
        }
        // U+00A0 and U+FEFF are whitespace too.
        if b.get(i..i + 2) == Some(&[0xC2, 0xA0]) {
            i += 2;
            continue;
        }
        if b.get(i..i + 3) == Some(&[0xEF, 0xBB, 0xBF]) {
            i += 3;
            continue;
        }
        return i;
    }
}

impl Parser<'_> {
    /// A JSDoc type expression: a type or type predicate (`x is T`, valid only as a return
    /// type: the checker reports TS1228 elsewhere), `...T` (rest) or `T=` (optional) at the
    /// top level, `const` (a const assertion).
    pub(crate) fn jsdoc_type_expr(&mut self) -> NodeId {
        let start = self.start();
        if self.at(T::Const) && self.peek_kind() == T::RBrace {
            self.bump();
            return self.finish_node(
                start,
                Node::KeywordType {
                    kind: KeywordTypeKind::Const,
                },
            );
        }
        if self.eat(T::DotDotDot) {
            let ty = self.parse_type();
            return self.finish_node(start, Node::RestType { ty });
        }
        let ty = self.parse_type_or_predicate();
        if self.at(T::Eq) {
            self.bump();
            return self.finish_node(start, Node::OptionalType { ty });
        }
        ty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn run(src: &str) -> (Ast, Vec<Diagnostic>) {
        let mut r = crate::parse(src, SourceKind::Js);
        let d = attach_jsdoc(&mut r.ast, src);
        (r.ast, d)
    }

    #[test]
    fn escaped_names_and_returned_functions() {
        let src = "/** @param {number} \\u0061 @param {string} a\\u{61} */\nfunction f(a, aa) {}\nfunction g() {\n  /** @param {number} x */\n  return (x) => x;\n}\n/** @param {function(number)} cb */\nfunction h(cb) {}\n";
        let (ast, _) = run(src);
        let annotated = (1..=ast.node_count() as u32)
            .filter_map(ts_r_ast::NodeId::from_raw)
            .filter(|&n| {
                matches!(
                    ast.node(n),
                    Node::Param {
                        type_ann: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(annotated, 4, "{}", ast.dump());
    }

    #[test]
    fn wrapped_backquoted_and_optional_params() {
        // Stray `*` before a name, a backquoted name, `??T`, and TS1016 after `{T=}`.
        let src = "/**\n * @param {number}\n * * y\n * @param {string=} `a`\n * @param {???!?number?=} p\n * @param {number} b\n */\nfunction f(y, a, p, b) {}\n";
        let (ast, d) = run(src);
        let annotated = (1..=ast.node_count() as u32)
            .filter_map(ts_r_ast::NodeId::from_raw)
            .filter(|&n| {
                matches!(
                    ast.node(n),
                    Node::Param {
                        type_ann: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(annotated, 4, "{}", ast.dump());
        let codes: Vec<u32> = d.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![1016], "{d:?}");
    }

    #[test]
    fn enum_types_are_not_read_and_callbacks_read_this() {
        let src = "/** @enum {function(number): number} */\nconst E = {};\n/**\n * @callback C\n * @this {{ a: string }}\n * @param {string} a\n */\n/** @template in out T @param {T} x */\nfunction g(x) {}\n";
        let (ast, d) = run(src);
        assert!(d.is_empty(), "{d:?}");
        let dump = ast.dump();
        assert!(dump.contains("name=\"this\""), "{dump}");
    }

    #[test]
    fn strip_restores_parsed_tree() {
        let src = "/** @typedef {{a: number}} P */\n/** @param {P} p @returns {number} */\nfunction f(p) { return /** @type {number} */ (p.a); }\nclass C {\n  /** @private @type {string} */\n  x = '';\n}\n";
        let parsed = crate::parse(src, SourceKind::Js).ast;
        let (mut ast, _) = run(src);
        assert_ne!(ast.dump(), parsed.dump());
        ast.strip_jsdoc();
        assert_eq!(ast.dump(), parsed.dump());
        assert_eq!(ast.node_count(), parsed.node_count());
    }

    #[test]
    fn same_line_comments_only_document_inner_nodes() {
        // After a token on the same line: ignored by a statement or member, used by a
        // parameter, a parenthesized expression or a declarator.
        let (ast, _) = run("let a = 1; /** @type {string} */ let b = 2;\n\
                            class C { /** @type {string} */ x = 1 }\n\
                            f(/** @type {string} */ (c));\n\
                            function g(/** @type {string} */ d) {}\n");
        let dump = ast.dump();
        assert_eq!(dump.matches("type_ann: KeywordType").count(), 1, "{dump}");
        assert_eq!(dump.matches("TypeAssertion").count(), 1, "{dump}");
        let (_, d) = run("let a = 1; /** @typedef {string} T @param {} */\n");
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn optional_setter_parameter() {
        let (_, d) = run("class C {\n  /** @param {string=} v */\n  set x(v) {}\n}\n");
        let codes: Vec<u32> = d.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![1051], "{d:?}");
    }

    #[test]
    fn var_type_annotation() {
        let (ast, d) = run("/** @type {?number} */\nvar x;\n");
        assert!(d.is_empty(), "{d:?}");
        let dump = ast.dump();
        assert!(dump.contains("type_ann: UnionType"), "{dump}");
    }

    #[test]
    fn param_and_returns() {
        let (ast, d) = run(
            "/**\n * @param {string} a\n * @param {number=} b\n * @returns {boolean}\n */\nfunction f(a, b) { return true }\n",
        );
        assert!(d.is_empty(), "{d:?}");
        let dump = ast.dump();
        assert!(dump.contains("KeywordType"), "{dump}");
        assert!(dump.contains("optional"), "{dump}");
        assert!(dump.contains("return_type"), "{dump}");
    }

    #[test]
    fn typedef_becomes_alias() {
        let (ast, d) = run(
            "/**\n * @typedef {Object} P\n * @property {string} a\n * @property {number} [b]\n */\nvar x;\n",
        );
        assert!(d.is_empty(), "{d:?}");
        let dump = ast.dump();
        assert!(dump.contains("TypeAlias"), "{dump}");
        assert!(dump.contains("PropertySignature"), "{dump}");
    }

    #[test]
    fn jsdoc_errors_reported() {
        let (_, d) =
            run("/** @type {function(): string} */\nvar x;\n/** @satisfies T */\nvar y = 1;\n");
        let codes: Vec<u32> = d.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![1005, 1005, 1005], "{d:?}");
    }

    #[test]
    fn param_tags_without_parameter() {
        let src = "/** @param {string} colour */\nfunction f(color) {}\n\
                   /** @param {object} o\n * @param {string} o.a\n * @param {number} b.c */\nfunction g({a}) {}\n\
                   /** @param {string} first */\nfunction h() { arguments }\n\
                   /** @param {...string} rest */\nfunction k() { arguments }\n";
        let (_, d) = run(src);
        let codes: Vec<u32> = d.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![8024, 8032, 8029], "{d:?}");
    }

    #[test]
    fn augments_must_match_extends() {
        let src = "class A {}\nclass B {}\n/** @augments */\nclass C extends A {}\n\
                   /** @augments A */\nclass D extends B {}\n/** @extends {A} */\nclass E extends ns.A {}\n";
        let (_, d) = run(src);
        let codes: Vec<u32> = d.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![1003, 8023, 8023], "{d:?}");
    }

    #[test]
    fn pathological_comments_terminate() {
        for src in [
            "/** @type {",
            "/** @param {{{{{{{{ */ function f(a) {}",
            "/** @typedef */",
            "/** @template */ function f() {}",
            "/** @import */",
            "/** @import { */",
            "/** @param [a */ function f(a) {}",
            "/**/ /** */ /*** @type {x} */ var x;",
        ] {
            let _ = run(src);
        }
        let deep = format!("/** @type {{{}}} */ var x;", "Array<".repeat(20_000));
        let _ = run(&deep);
    }

    #[test]
    fn duplicate_nested_paths_share_prefix_types() {
        for (tag, head, tail) in [
            ("param", "@param {Object} o", "function f(o) {}"),
            (
                "property",
                "@typedef {Object} T @property {Object} o",
                "var x;",
            ),
            ("param", "@callback F @param {Object} o", "var x;"),
        ] {
            let mut src = format!("/** {head}\n");
            let mut path = String::from("o");
            for _ in 0..40 {
                path.push_str(".a");
                src.push_str(&format!(" * @{tag} {{Object}} {path}\n"));
                src.push_str(&format!(" * @{tag} {{Object}} {path}\n"));
            }
            src.push_str(&format!(" */\n{tail}"));
            let (ast, d) = run(&src);
            assert!(d.is_empty(), "{d:?}");
            assert!(ast.node_count() < 1000, "{}", ast.node_count());
            let properties = ast
                .nodes
                .iter()
                .filter(|n| matches!(n, Node::PropertySignature { .. }))
                .count();
            assert_eq!(properties, if tag == "property" { 41 } else { 40 });
        }
    }

    #[test]
    fn desugaring_work_is_bounded() {
        let src = "/** @param {Object} o @param {number} o.a */\nfunction f(o) {}";
        let ast = crate::parse(src, SourceKind::Js).ast;
        let docs: Vec<_> = ast.comments.iter().map(|c| c.span).collect();
        let masked = mask(src, &docs);
        let mut d = Doc::new(src, &masked, ast);
        d.desugar_work = 0;
        d.comment(docs[0], true);
        assert!(d.p.halted);
        assert!(d.diags.iter().any(|d| d.code == 99003));
    }

    #[test]
    fn long_implements_name_is_converted_iteratively() {
        let src = format!(
            "/** @implements {{A{}}} */\nclass C {{}}",
            ".A".repeat(99_999)
        );
        let (ast, d) = run(&src);
        assert!(d.is_empty(), "{d:?}");
        let members = ast
            .nodes
            .iter()
            .filter(|n| matches!(n, Node::Member { .. }))
            .count();
        assert_eq!(members, 99_999);
        assert!(ast.node_count() < src.len() * 4 + 1024);
    }
}
