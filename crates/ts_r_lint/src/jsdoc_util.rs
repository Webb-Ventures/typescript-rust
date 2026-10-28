//! More shared helpers for the `jsdoc/*` rules, on top of [`crate::jsdoc`]: the `contexts`
//! option every iterating rule takes, preferred-tag lookup, the "replaces docs" exemptions,
//! and small AST questions (generators, `yield`, `throw`) several rules ask.

use serde_json::{Map, Value};
use ts_r_ast::{Ast, FnFlags, KeywordTypeKind, MethodKind, Node, NodeId, NodeList};

use crate::jsdoc::{self, Block, ContextEntry, Contexts, Doc, Finder, Settings, Target, Tokens};
use crate::rule::Ctx;

/// Recursion limit for the AST walks below; deeper input counts as "not found".
const MAX_DEPTH: u32 = 400;

/// The options object of a rule that takes `contexts`, checked up front.
#[derive(Debug, Clone, Default)]
pub struct ContextOpts {
    pub opts: Map<String, Value>,
    /// The rule's own default contexts, when they differ from [`jsdoc::DEFAULT_CONTEXTS`].
    pub defaults: Option<&'static [&'static str]>,
}

impl ContextOpts {
    pub fn parse(opts: &[Value]) -> Result<ContextOpts, String> {
        let o = opts
            .first()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if o.contains_key("contexts") {
            Contexts::from_options(&o, &Settings::default_for_validation())?;
        }
        Ok(ContextOpts {
            opts: o,
            defaults: None,
        })
    }

    /// [`ContextOpts::parse`] for a rule whose default contexts are `defaults`.
    pub fn parse_with(
        opts: &[Value],
        defaults: &'static [&'static str],
    ) -> Result<ContextOpts, String> {
        let mut c = ContextOpts::parse(opts)?;
        c.defaults = Some(defaults);
        Ok(c)
    }

    /// For a rule without `contextDefaults`: the `contexts` option and setting are ignored
    /// and only functions are checked.
    pub fn functions_only(opts: &[Value]) -> ContextOpts {
        let mut o = opts
            .first()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        o.insert(
            "contexts".to_owned(),
            Value::Array(
                [
                    "ArrowFunctionExpression",
                    "FunctionDeclaration",
                    "FunctionExpression",
                    "TSDeclareFunction",
                ]
                .iter()
                .map(|s| Value::from(*s))
                .collect(),
            ),
        );
        ContextOpts {
            opts: o,
            defaults: None,
        }
    }

    /// The contexts in force: the option, the setting, then the rule's defaults.
    pub fn contexts(&self, settings: &Settings) -> Result<Contexts, String> {
        match self.defaults {
            Some(d) if !self.opts.contains_key("contexts") && settings.contexts.is_none() => {
                let mut o = self.opts.clone();
                o.insert(
                    "contexts".to_owned(),
                    Value::Array(d.iter().map(|s| Value::from(*s)).collect()),
                );
                Contexts::from_options(&o, settings)
            }
            _ => Contexts::from_options(&self.opts, settings),
        }
    }

    /// Runs `f` on every block the contexts (or the defaults) select.
    pub fn each_doc(
        &self,
        cx: &mut Ctx<'_, '_>,
        f: impl FnMut(&mut Ctx<'_, '_>, &Finder<'_>, &mut Doc),
    ) {
        self.each_doc_checking(cx, Checks::default(), f);
    }

    /// [`ContextOpts::each_doc`], keeping the blocks `checks` asks for.
    pub fn each_doc_checking(
        &self,
        cx: &mut Ctx<'_, '_>,
        checks: Checks,
        f: impl FnMut(&mut Ctx<'_, '_>, &Finder<'_>, &mut Doc),
    ) {
        let settings = Settings::from_cx(cx);
        let Ok(contexts) = self.contexts(&settings) else {
            return;
        };
        run(cx, &settings, &contexts, checks, f);
    }
}

/// Blocks a rule checks even when `ignorePrivate` / `ignoreInternal` would skip them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Checks {
    pub private: bool,
    pub internal: bool,
}

fn run(
    cx: &mut Ctx<'_, '_>,
    settings: &Settings,
    contexts: &Contexts,
    checks: Checks,
    mut f: impl FnMut(&mut Ctx<'_, '_>, &Finder<'_>, &mut Doc),
) {
    if !jsdoc::settings_load(cx) {
        return;
    }
    let file = cx.file;
    let finder = Finder {
        ast: file.ast,
        src: file.src,
        parents: file.parents,
        lines: file.lines,
        settings,
    };
    let view = Settings {
        ignore_private: settings.ignore_private && !checks.private,
        ignore_internal: settings.ignore_internal && !checks.internal,
        ..settings.clone()
    };
    for item in jsdoc::collect(&finder, contexts) {
        let mut doc = Doc::new(&finder, &item);
        if jsdoc::skipped(&view, &doc.block) {
            continue;
        }
        f(cx, &finder, &mut doc);
    }
}

/// The `any` context: every JSDoc block in the file, once.
pub fn all_blocks() -> Contexts {
    Contexts {
        entries: vec![ContextEntry {
            ty: None,
            is_object: false,
            force_require_return: false,
            ..Default::default()
        }],
        all: true,
        plain_any: true,
        additive: false,
    }
}

/// Runs `f` on every JSDoc block of the file (the `any` context).
pub fn each_block(cx: &mut Ctx<'_, '_>, f: impl FnMut(&mut Ctx<'_, '_>, &Finder<'_>, &mut Doc)) {
    each_block_checking(cx, Checks::default(), f);
}

/// [`each_block`], keeping the blocks `checks` asks for.
pub fn each_block_checking(
    cx: &mut Ctx<'_, '_>,
    checks: Checks,
    f: impl FnMut(&mut Ctx<'_, '_>, &Finder<'_>, &mut Doc),
) {
    let settings = Settings::from_cx(cx);
    run(cx, &settings, &all_blocks(), checks, f);
}

/// Adds a line holding just `@tag`: after the first line of the last tag, or (with no tags)
/// before the last line.
pub fn add_tag(doc: &mut Doc, tag: &str) {
    let b = &mut doc.block;
    let number = match b.tags.last() {
        Some(t) => t.source.first().map_or(0, |&id| b.lines[id].number),
        None => {
            let first = b
                .source
                .iter()
                .position(|&id| !b.lines[id].tokens.tag.is_empty())
                .map_or(-1, |i| i as i64);
            first - 1
        }
    } + 1;
    let len = b.source.len() as i64;
    let at = if number < 0 {
        (len + number).max(0)
    } else {
        number.min(len)
    } as usize;
    let tokens = Tokens {
        start: format!("{} ", doc.indent),
        delimiter: "*".to_owned(),
        post_delimiter: " ".to_owned(),
        tag: format!("@{tag}"),
        ..Tokens::default()
    };
    let id = b.new_line(at as i64, tokens);
    b.source.insert(at, id);
    for &later in &b.source[at + 1..] {
        b.lines[later].number += 1;
    }
}

/// The preferred name of `name` and the indexes of the block's tags with that name; `None`
/// (after reporting a use of it) when the tag is blocked.
pub fn preferred_tags(
    cx: &mut Ctx<'_, '_>,
    finder: &Finder<'_>,
    doc: &Doc,
    name: &str,
) -> Option<(String, Vec<usize>)> {
    let tag = doc.preferred(cx, finder, name)?;
    let ids = doc.block.tags_named(&tag).map(|(i, _)| i).collect();
    Some((tag, ids))
}

/// Like [`preferred_tags`] but without reporting a blocked tag.
pub fn preferred_name(settings: &Settings, name: &str) -> Option<String> {
    match jsdoc::preferred_tag(settings, name) {
        jsdoc::Preferred::Name(n) => Some(n),
        jsdoc::Preferred::Blocked(_) => None,
    }
}

/// The `exemptedBy` option: the given list, or `inheritDoc`/`inheritdoc`.
pub fn exempted_by(o: &Map<String, Value>, settings_mode_closure: bool) -> Vec<String> {
    match o.get("exemptedBy").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        None if settings_mode_closure => vec!["inheritDoc".to_owned()],
        None => vec!["inheritDoc".to_owned(), "inheritdoc".to_owned()],
    }
}

/// The settings that make a tag on the block (or its class) stand in for the docs.
pub fn replaced_docs(finder: &Finder<'_>, doc: &Doc) -> bool {
    let b = &doc.block;
    let class = finder.class_block(doc.target);
    let either = |t: &str| b.has_tag(t) || class.as_ref().is_some_and(|c| c.has_tag(t));
    let s = finder.settings;
    (s.ignore_replaces_docs && either("ignore"))
        || (s.override_replaces_docs && either("override"))
        || (s.implements_replaces_docs && either("implements"))
        || (s.augments_extends_replaces_docs && (either("augments") || either("extends")))
}

/// True when the block has one of the `exemptedBy` tags (exact names).
pub fn has_exempting_tag(block: &Block, exempted: &[String]) -> bool {
    exempted
        .iter()
        .any(|t| block.tags.iter().any(|x| &x.tag == t))
}

/// The function a target documents: the node itself, a method's value, or the function
/// initialising a variable or property.
pub fn function_of(f: &Finder<'_>, t: Option<Target>) -> Option<NodeId> {
    let id = t?.id();
    let ast = f.ast;
    let is_fn = |n: NodeId| {
        matches!(
            ast.node(n),
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
        )
    };
    if is_fn(id) {
        return Some(id);
    }
    match *ast.node(id) {
        Node::MethodDef { value, .. } | Node::Property { value, .. } => {
            let v = crate::ast_util::skip_parens(ast, value);
            is_fn(v).then_some(v)
        }
        Node::PropertyDef { value, .. } => {
            let v = crate::ast_util::skip_parens(ast, value?);
            is_fn(v).then_some(v)
        }
        _ => None,
    }
}

/// `isGenerator`: the target is a generator function, a generator method, or an export
/// of a generator function.
pub fn is_generator(f: &Finder<'_>, t: Option<Target>) -> bool {
    let Some(t) = t else {
        return false;
    };
    let id = match *f.ast.node(t.id()) {
        Node::MethodDef { value, .. } if matches!(t, Target::Node(_)) => value,
        _ => t.id(),
    };
    fn_flags(f.ast, id).contains(FnFlags::GENERATOR)
}

/// The function's flags (async, generator, ...).
pub fn fn_flags(ast: &Ast, id: NodeId) -> FnFlags {
    match *ast.node(id) {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => flags,
        _ => FnFlags::default(),
    }
}

const FUNCTION_TYPES: &[&str] = &[
    "ArrowFunctionExpression",
    "FunctionDeclaration",
    "FunctionExpression",
    "MethodDefinition",
];

/// `isIteratingFunctionOrVariable`: not iterating every block, or the node is a function (or
/// a variable declaration initialised with one).
pub fn iterating_function_or_variable(f: &Finder<'_>, all: bool, t: Option<Target>) -> bool {
    if !all {
        return true;
    }
    let Some(t) = t else {
        return false;
    };
    if FUNCTION_TYPES.contains(&f.es_type(t)) {
        return true;
    }
    let Node::VarDecl { decls, .. } = *f.ast.node(t.id()) else {
        return false;
    };
    f.ast.list(decls).iter().any(|&d| match *f.ast.node(d) {
        Node::VarDeclarator { init: Some(i), .. } => matches!(
            f.ast.node(crate::ast_util::skip_parens(f.ast, i)),
            Node::Arrow { .. } | Node::FunctionExpr { .. } | Node::FunctionDecl { .. }
        ),
        _ => false,
    })
}

/// `isVirtualFunction`: iterating every block, and the block names a function itself.
pub fn is_virtual_function(all: bool, block: &Block) -> bool {
    all && block.has_any_tag(&["callback", "function", "func", "method"])
}

/// The kind of the method a target is (or whose function it is).
pub fn method_kind(f: &Finder<'_>, t: Option<Target>) -> Option<MethodKind> {
    let id = t?.id();
    let kind = |n: NodeId| match *f.ast.node(n) {
        Node::MethodDef { kind, .. } => Some(kind),
        _ => None,
    };
    kind(id).or_else(|| f.parents.parent(id).and_then(kind))
}

/// `hasYieldValue`: whether generator function `id` yields. With `next`, only a `yield`
/// that is a variable's initializer counts (its result is used); otherwise any `yield`
/// with an argument. The walk only follows the statement and expression forms below; it
/// skips nested functions, call callees, `new`, `await`, class bodies and TS wrappers.
pub fn has_yield_value(ast: &Ast, id: NodeId, next: bool) -> bool {
    match *ast.node(id) {
        Node::FunctionDecl { flags, body, .. } | Node::FunctionExpr { flags, body, .. } => {
            flags.contains(FnFlags::GENERATOR) && body.is_some_and(|b| yields(ast, b, next, 0))
        }
        _ => false,
    }
}

fn yields(ast: &Ast, id: NodeId, next: bool, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let d = depth + 1;
    let any = |l: NodeList| ast.list(l).iter().any(|&n| yields(ast, n, next, d));
    let one = |n: NodeId| yields(ast, n, next, d);
    let opt = |n: Option<NodeId>| n.is_some_and(|n| yields(ast, n, next, d));
    let key = |k: NodeId| match *ast.node(k) {
        Node::ComputedKey { expr } => one(expr),
        _ => false,
    };
    match *ast.node(id) {
        Node::ArrayLit { elements } | Node::ArrayPattern { elements } => any(elements),
        Node::Assign { left, right, .. } | Node::Binary { left, right, .. } => {
            one(left) || one(right)
        }
        Node::AssignPattern { right, .. } => one(right),
        Node::Block { stmts } => ast.list(stmts).iter().any(|&s| {
            !matches!(
                ast.node(s),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            ) && one(s)
        }),
        Node::Call { args, .. } => any(args),
        Node::ExprStmt { expr } | Node::Paren { expr } => one(expr),
        Node::Property { key: k, value, .. } => key(k) || one(value),
        Node::BindingProperty {
            key: k,
            value,
            init,
        } => key(k) || opt(value) || opt(init),
        Node::ShorthandProp { init, .. } => opt(init),
        Node::Conditional { test, cons, alt } => one(test) || one(cons) || one(alt),
        Node::If { test, cons, alt } => one(test) || one(cons) || opt(alt),
        Node::DoWhile { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::For { body, .. }
        | Node::Labeled { body, .. }
        | Node::While { body, .. }
        | Node::With { body, .. } => one(body),
        Node::ImportCall { arg, .. } => one(arg),
        Node::Member { obj, .. } => one(obj),
        Node::Index { obj, index, .. } => one(obj) || one(index),
        Node::ObjectLit { props } | Node::ObjectPattern { props } => any(props),
        Node::Return { arg } => opt(arg),
        Node::Seq { exprs } => any(exprs),
        Node::TemplateLit { parts } => any(parts),
        Node::Spread { arg } | Node::Unary { arg, .. } => one(arg),
        Node::Switch { cases, .. } => ast.list(cases).iter().any(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => any(body),
            _ => false,
        }),
        Node::TaggedTemplate { quasi, .. } => one(quasi),
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            one(block)
                || handler.is_some_and(|h| match *ast.node(h) {
                    Node::CatchClause { body, .. } => one(body),
                    _ => false,
                })
                || opt(finalizer)
        }
        Node::VarDecl { decls, .. } => any(decls),
        Node::VarDeclarator { name, init, .. } => {
            let direct = init.is_some_and(|i| {
                matches!(
                    ast.node(crate::ast_util::skip_parens(ast, i)),
                    Node::Yield { .. }
                )
            });
            one(name) || (next && direct) || opt(init)
        }
        Node::Yield { arg, .. } => !next && arg.is_some(),
        _ => false,
    }
}

/// The block description (`getDescription`): its lines, joined, and the index of its last
/// line within the block.
#[derive(Debug, Clone, Default)]
pub struct Description {
    pub lines: Vec<String>,
    pub last_line: usize,
}

impl Description {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// The description lines before the first tag (or the closing line). An empty first
/// description line counts as `"\n"`.
pub fn description(b: &Block) -> Description {
    let mut d = Description::default();
    let mut tags_begun = false;
    for (idx, &id) in b.source.iter().enumerate() {
        let t = &b.lines[id].tokens;
        if !t.tag.is_empty() {
            tags_begun = true;
        }
        if idx > 0 && (!t.tag.is_empty() || !t.end.is_empty()) {
            d.last_line = idx - 1;
            if !tags_begun && !t.description.is_empty() {
                d.lines.push(t.description.clone());
            }
            break;
        }
        if !tags_begun && (idx > 0 || !t.description.is_empty()) {
            let line = if !t.description.is_empty() {
                t.description.clone()
            } else if d.lines.is_empty() {
                "\n".to_owned()
            } else {
                String::new()
            };
            d.lines.push(line);
        }
    }
    d
}

/// Replaces the description lines (after the `/**` line, up to the first tag or closing line)
/// with the lines `f` builds from the first one's `start`, `delimiter` and `post_delimiter`
/// (`setBlockDescription`). Does nothing without description lines.
pub fn set_block_description(b: &mut Block, f: impl FnOnce(&Tokens) -> Vec<Tokens>) {
    let mut start = None;
    let mut end = None;
    let mut info = Tokens::default();
    let mut count = 0;
    for (idx, &id) in b.source.iter().enumerate() {
        let t = &b.lines[id].tokens;
        if t.delimiter == "/**" {
            continue;
        }
        if start.is_none() {
            start = Some(idx);
            info = Tokens {
                start: t.start.clone(),
                delimiter: t.delimiter.clone(),
                post_delimiter: t.post_delimiter.clone(),
                ..Tokens::default()
            };
        }
        if !t.tag.is_empty() || !t.end.is_empty() {
            end = Some(idx);
            break;
        }
        count += 1;
    }
    let Some(start) = start else {
        return;
    };
    if count == 0 {
        return;
    }
    let ids: Vec<usize> = f(&info).into_iter().map(|t| b.new_line(0, t)).collect();
    let end = end.unwrap_or(start).max(start);
    b.source.splice(start..end, ids);
}

/// The fix for a block whose text became empty: drop it along with the line break and
/// indentation before it, or (first line) with one following whitespace character.
pub fn removal_fix(src: &str, comment: ts_r_span::Span) -> crate::diagnostic::Fix {
    let before = &src[..comment.start as usize];
    let indent = before.trim_end_matches([' ', '\t']);
    if let Some(head) = indent.strip_suffix('\n') {
        let span = ts_r_span::Span::new(head.len() as u32, comment.end);
        return crate::diagnostic::Fix::delete(span);
    }
    let end = match src[comment.end as usize..].chars().next() {
        Some(c) if c.is_whitespace() => comment.end + c.len_utf8() as u32,
        _ => comment.end,
    };
    crate::diagnostic::Fix::delete(ts_r_span::Span::new(comment.start, end))
}

/// Like [`Doc::line_span`], but at the comment's own column on that line (a report with
/// `column: 0`), clamped to the line's end.
pub fn column_span(finder: &Finder<'_>, doc: &Doc, rel: i64) -> ts_r_span::Span {
    let line = doc.line_span(finder, rel).start;
    let col = doc.comment.start
        - finder
            .lines
            .line_start(doc.line)
            .unwrap_or(doc.comment.start);
    let rest = &finder.src[line as usize..];
    let line_len = rest.find('\n').unwrap_or(rest.len()) as u32;
    let at = line + col.min(line_len);
    ts_r_span::Span::new(at, at)
}

/// A tag's description (`getTagDescription`): the text after the tag, type and name on its
/// first line, then its other lines, joined with newlines.
pub fn tag_description(b: &Block, tag: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for &id in &b.tags[tag].source {
        let t = &b.lines[id].tokens;
        let lead = if !t.tag.is_empty() && !t.post_tag.is_empty() {
            t.post_tag.as_str()
        } else if t.tag.is_empty() && t.name.is_empty() && t.typ.is_empty() {
            t.post_delimiter.as_str()
        } else {
            ""
        };
        let mut lead = lead.chars();
        lead.next();
        let d = format!("{}{}{}", lead.as_str(), t.description, t.line_end);
        if !t.end.is_empty() {
            if !d.is_empty() {
                out.push(d);
            }
            break;
        }
        out.push(d);
    }
    out.join("\n")
}

/// `hasThrowValue`: whether function `id` (not async) throws, looking only through
/// statements: blocks, loops, labels, `if`, `switch` cases and the `catch`/`finally` of a
/// `try` (a `throw` inside the `try` block is caught). Nested functions don't count.
pub fn has_throw_value(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::FunctionDecl { flags, body, .. } | Node::FunctionExpr { flags, body, .. } => {
            !flags.contains(FnFlags::ASYNC) && body.is_some_and(|b| throws(ast, b, 0))
        }
        Node::Arrow { flags, body, .. } => !flags.contains(FnFlags::ASYNC) && throws(ast, body, 0),
        _ => false,
    }
}

fn throws(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let d = depth + 1;
    match *ast.node(id) {
        Node::Block { stmts } => ast.list(stmts).iter().any(|&s| throws(ast, s, d)),
        Node::DoWhile { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::For { body, .. }
        | Node::Labeled { body, .. }
        | Node::While { body, .. }
        | Node::With { body, .. } => throws(ast, body, d),
        Node::If { cons, alt, .. } => {
            throws(ast, cons, d) || alt.is_some_and(|a| throws(ast, a, d))
        }
        Node::Switch { cases, .. } => ast.list(cases).iter().any(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => ast.list(body).iter().any(|&s| throws(ast, s, d)),
            _ => false,
        }),
        Node::Throw { .. } => true,
        Node::Try {
            handler, finalizer, ..
        } => {
            let caught = handler.is_some_and(|h| match *ast.node(h) {
                Node::CatchClause { body, .. } => throws(ast, body, d),
                _ => false,
            });
            caught || finalizer.is_some_and(|f| throws(ast, f, d))
        }
        _ => false,
    }
}

/// `mayBeUndefinedTypeTag`: the tag's type allows `undefined`: it is `undefined`, `void`,
/// `*` or `any`, or a top-level union with an `undefined` or `void` member. A missing tag
/// counts too.
pub fn may_be_undefined(tag: Option<&jsdoc::Tag>) -> bool {
    let Some(tag) = tag else {
        return true;
    };
    let ty = tag.typ.trim();
    if matches!(ty, "undefined" | "void" | "*" | "any") {
        return true;
    }
    top_union(ty)
        .is_some_and(|m| m.len() > 1 && m.iter().any(|m| matches!(*m, "undefined" | "void")))
}

/// The tags that define a name or namepath in `mode`, other than parameters and
/// properties (the typedef-like tags `require-returns-check` looks a return type up in).
pub fn defines_name(tag: &str, mode: &str) -> bool {
    match tag {
        "alias" | "callback" | "class" | "const" | "constant" | "constructor" | "constructs"
        | "enum" | "event" | "exports" | "external" | "func" | "function" | "host" | "member"
        | "method" | "mixin" | "name" | "namespace" | "typedef" | "var" => true,
        "interface" => mode != "closure",
        "module" => mode == "jsdoc",
        _ => false,
    }
}

fn void_like(ast: &Ast, ty: NodeId) -> bool {
    matches!(
        ast.node(ty),
        Node::KeywordType {
            kind: KeywordTypeKind::Void | KeywordTypeKind::Never | KeywordTypeKind::Undefined
        }
    )
}

/// An arrow whose expression body isn't a `new Promise<void>(...)`.
fn arrow_value(ast: &Ast, body: NodeId) -> bool {
    let Node::New {
        callee, type_args, ..
    } = *ast.node(crate::ast_util::skip_parens(ast, body))
    else {
        return true;
    };
    let promise = crate::ast_util::ident_name(ast, crate::ast_util::skip_parens(ast, callee))
        == Some("Promise");
    let void = type_args.is_some_and(|ta| match *ast.node(ta) {
        Node::TypeArgs { args } => ast.list(args).first().is_some_and(|&a| {
            matches!(
                ast.node(a),
                Node::KeywordType {
                    kind: KeywordTypeKind::Void
                }
            )
        }),
        _ => false,
    });
    !(promise && void)
}

/// `hasValueOrExecutorHasNonEmptyResolveValue` with `allBranches`: function `id` returns a
/// value somewhere (no bare `return;` met on the way) and every branch ends in a value
/// return or a `throw`.
pub fn returns_on_all_branches(ast: &Ast, id: NodeId, any_promise: bool) -> bool {
    matches!(some_return(ast, id, true, any_promise, 0), Ok(true))
        && all_return(ast, id, any_promise, 0)
}

/// `hasReturnValue`; with `strict`, meeting a bare `return;` before a value return aborts
/// the walk (`Err`).
fn some_return(
    ast: &Ast,
    id: NodeId,
    strict: bool,
    any_promise: bool,
    depth: u32,
) -> Result<bool, ()> {
    if depth > MAX_DEPTH {
        return Ok(false);
    }
    let rec = |n: NodeId| some_return(ast, n, strict, any_promise, depth + 1);
    let opt = |n: Option<NodeId>| n.map_or(Ok(false), rec);
    let any = |l: NodeList| {
        for &s in ast.list(l) {
            if rec(s)? {
                return Ok(true);
            }
        }
        Ok(false)
    };
    Ok(match *ast.node(id) {
        Node::FunctionDecl {
            body: None,
            return_type,
            ..
        } => return_type.is_some_and(|t| !void_like(ast, t)),
        Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => opt(body)?,
        Node::Arrow { flags, body, .. } => {
            if flags.contains(FnFlags::EXPRESSION_BODY) {
                arrow_value(ast, body)
            } else {
                rec(body)?
            }
        }
        Node::FunctionType { return_type, .. } => !void_like(ast, return_type),
        Node::MethodSignature { return_type, .. } => {
            return_type.is_some_and(|t| !void_like(ast, t))
        }
        Node::MethodDef { value, .. } => rec(value)?,
        Node::Block { stmts } => {
            for &s in ast.list(stmts) {
                if !matches!(ast.node(s), Node::FunctionDecl { .. }) && rec(s)? {
                    return Ok(true);
                }
            }
            false
        }
        Node::DoWhile { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::For { body, .. }
        | Node::Labeled { body, .. }
        | Node::While { body, .. }
        | Node::With { body, .. } => rec(body)?,
        Node::If { cons, alt, .. } => rec(cons)? || opt(alt)?,
        Node::Return { arg: None } if strict => return Err(()),
        Node::Return { arg: None } => false,
        // A value return, or a promise that resolves with one.
        Node::Return { .. } => jsdoc::has_return_value(ast, id, any_promise),
        Node::Switch { cases, .. } => {
            for &c in ast.list(cases) {
                if let Node::SwitchCase { body, .. } = *ast.node(c)
                    && any(body)?
                {
                    return Ok(true);
                }
            }
            false
        }
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            rec(block)?
                || match handler.map(|h| ast.node(h)) {
                    Some(&Node::CatchClause { body, .. }) => rec(body)?,
                    _ => false,
                }
                || opt(finalizer)?
        }
        _ => false,
    })
}

/// `allBrancheshaveReturnValues`: the last statement of each block decides; `if` needs
/// both arms, a `switch` needs no case with a `break` or bare `return;`, an infinite loop
/// needs one value return in its body, and a `finally` that returns covers everything.
fn all_return(ast: &Ast, id: NodeId, any_promise: bool, depth: u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let rec = |n: NodeId| all_return(ast, n, any_promise, depth + 1);
    let loose = |n: NodeId| matches!(some_return(ast, n, false, any_promise, depth + 1), Ok(true));
    let infinite = |test: NodeId| {
        matches!(
            ast.node(crate::ast_util::skip_parens(ast, test)),
            Node::BoolLit { value: true }
        )
    };
    match *ast.node(id) {
        Node::FunctionDecl {
            body: None,
            return_type,
            ..
        } => return_type.is_some_and(|t| !void_like(ast, t)),
        Node::FunctionDecl { body: Some(b), .. }
        | Node::FunctionExpr { body: Some(b), .. }
        | Node::Arrow { body: b, .. } => {
            if matches!(ast.node(id), Node::Arrow { flags, .. } if flags.contains(FnFlags::EXPRESSION_BODY))
            {
                return arrow_value(ast, b);
            }
            rec(b)
                || match *ast.node(b) {
                    Node::Block { stmts } => ast
                        .list(stmts)
                        .iter()
                        .any(|&s| matches!(ast.node(s), Node::Return { .. })),
                    _ => false,
                }
        }
        Node::FunctionType { return_type, .. } => !void_like(ast, return_type),
        Node::MethodSignature { return_type, .. } => {
            return_type.is_some_and(|t| !void_like(ast, t))
        }
        Node::Block { stmts } => ast.list(stmts).last().is_some_and(|&s| rec(s)),
        Node::DoWhile { test, body } | Node::While { test, body } => {
            if infinite(test) {
                loose(body)
            } else {
                rec(body)
            }
        }
        Node::For {
            test: None, body, ..
        } => loose(body),
        Node::For { body, .. }
        | Node::ForIn { body, .. }
        | Node::ForOf { body, .. }
        | Node::Labeled { body, .. }
        | Node::With { body, .. } => rec(body),
        Node::If { cons, alt, .. } => rec(cons) && alt.is_some_and(rec),
        Node::Return { arg: None } => false,
        Node::Return { .. } => jsdoc::has_return_value(ast, id, any_promise),
        Node::Switch { cases, .. } => ast.list(cases).iter().all(|&c| match *ast.node(c) {
            Node::SwitchCase { body, .. } => !ast
                .list(body)
                .iter()
                .any(|&s| matches!(ast.node(s), Node::Break { .. } | Node::Return { arg: None })),
            _ => true,
        }),
        Node::Throw { .. } => true,
        Node::Try {
            block,
            handler,
            finalizer,
        } => {
            finalizer.is_some_and(rec)
                || (rec(block)
                    && handler.is_none_or(|h| match *ast.node(h) {
                        Node::CatchClause { body, .. } => rec(body),
                        _ => false,
                    })
                    && finalizer
                        .is_none_or(|f| some_return(ast, f, true, any_promise, depth + 1).is_ok()))
        }
        _ => false,
    }
}

/// `mayReturnUndefined` beyond [`may_be_undefined`]: a top-level member of the return type
/// (inside one pair of parentheses) is `undefined`, `void`, or a name `typedef_undefined`
/// says allows `undefined`.
pub fn return_may_be_undefined(ty: &str, typedef_undefined: impl Fn(&str) -> bool) -> bool {
    let mut ty = ty.trim();
    if let Some(inner) = ty.strip_prefix('(').and_then(|s| s.strip_suffix(')'))
        && balanced(inner)
    {
        ty = inner.trim();
    }
    let Some(members) = top_union(ty) else {
        return false;
    };
    members.iter().any(|&m| {
        m == "undefined"
            || m == "void"
            || (m
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                && typedef_undefined(m))
    })
}

/// The members of a type's top-level union (one member when it isn't a union); `None`
/// when the brackets don't balance or a member is empty.
fn top_union(ty: &str) -> Option<Vec<&str>> {
    let mut members = Vec::new();
    let (mut depth, mut start, mut prev) = (0i32, 0, ' ');
    for (i, c) in ty.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if prev == '=' => {}
            ')' | ']' | '}' | '>' => depth -= 1,
            '|' if depth == 0 => {
                members.push(ty[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
        prev = c;
    }
    members.push(ty[start..].trim());
    (depth == 0 && !members.iter().any(|m| m.is_empty())).then_some(members)
}

fn balanced(s: &str) -> bool {
    top_union(s).is_some()
}

/// The names a JSDoc type uses as types, in order: identifiers that are not a namepath's
/// later segment (`a.b`), not an object key (`{a: T}`) and not inside a string, plus `*`.
/// `None` when the brackets don't balance (the type wouldn't parse).
pub fn type_names(ty: &str) -> Option<Vec<&str>> {
    top_union(ty)?;
    let bytes = ty.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80;
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            // Skip the string (an unterminated one runs to the end).
            i += 1;
            while i < bytes.len() && bytes[i] != b {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        if b == b'*' {
            out.push("*");
            i += 1;
            continue;
        }
        if !is_ident(b) || b.is_ascii_digit() {
            i += 1;
            while i < bytes.len() && b.is_ascii_digit() && is_ident(bytes[i]) {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident(bytes[i]) {
            i += 1;
        }
        let before = ty[..start].trim_end();
        let after = ty[i..].trim_start();
        let after = after.strip_prefix('?').unwrap_or(after).trim_start();
        if !before.ends_with('.') && !after.starts_with(':') {
            out.push(&ty[start..i]);
        }
    }
    Some(out)
}

/// The `reject-*-type` rules: every tag type that uses one of `rejected` (name, message)
/// gets the message on the tag's line.
pub fn reject_type_names(cx: &mut Ctx<'_, '_>, rejected: &[(&str, &str)]) {
    each_block(cx, |cx, finder, doc| {
        for (i, tag) in doc.block.tags.iter().enumerate() {
            if tag.typ.is_empty() {
                continue;
            }
            let Some(names) = type_names(&tag.typ) else {
                continue;
            };
            for n in names {
                if let Some(&(_, msg)) = rejected.iter().find(|(r, _)| *r == n) {
                    let span = doc.line_span(finder, doc.block.tag_line(i));
                    cx.report(span, msg);
                }
            }
        }
    });
}

/// `removeTag`: from `offset` lines into tag `idx`, drops the rest of its untagged lines
/// (the closing line stays), then renumbers the lines after the tag. A line that opens or
/// closes the comment, or the block's first line when it has other tags, is blanked
/// instead (its delimiters stay). Removing from the block's first line drops everything.
pub fn remove_tag(b: &mut Block, idx: usize, offset: usize) {
    let Some(tag) = b.tags.get(idx) else {
        return;
    };
    let Some(&first) = tag.source.first() else {
        return;
    };
    let Some(pos) = b.source.iter().position(|&l| l == first) else {
        return;
    };
    let mut count: usize = 1;
    for &l in &tag.source[1..] {
        let t = &b.lines[l].tokens;
        if !t.tag.is_empty() || !t.end.is_empty() {
            break;
        }
        count += 1;
    }
    let at = pos + offset;
    let Some(&line) = b.source.get(at) else {
        return;
    };
    let t = &b.lines[line].tokens;
    if (at == 0 && b.tags.len() >= 2) || !t.end.is_empty() || t.delimiter == "/**" {
        let t = &mut b.lines[line].tokens;
        for s in [
            &mut t.post_delimiter,
            &mut t.tag,
            &mut t.post_tag,
            &mut t.typ,
            &mut t.post_type,
            &mut t.name,
            &mut t.post_name,
            &mut t.description,
        ] {
            s.clear();
        }
    } else if at == 0 {
        b.source.clear();
        b.tags[idx].source.clear();
    } else {
        let n = count.saturating_sub(offset);
        b.source.drain(at..(at + n).min(b.source.len()));
        let src = &mut b.tags[idx].source;
        let from = offset.min(src.len());
        src.drain(from..(from + n).min(src.len()));
    }
    renumber(b, pos);
}

/// `addLines`: inserts `n` empty `*` lines `offset` lines into tag `idx` (they join no tag),
/// then renumbers the lines after the tag.
pub fn add_lines(b: &mut Block, indent: &str, idx: usize, offset: usize, n: usize) {
    let Some(&first) = b.tags.get(idx).and_then(|t| t.source.first()) else {
        return;
    };
    let number = b.lines[first].number;
    let Some(pos) = b
        .source
        .iter()
        .position(|&l| b.lines[l].number == number && b.lines[l].tokens.end.is_empty())
    else {
        return;
    };
    let at = (pos + offset).min(b.source.len());
    let ids: Vec<usize> = (0..n)
        .map(|_| {
            let tokens = Tokens {
                start: format!("{indent} "),
                delimiter: "*".to_owned(),
                ..Tokens::default()
            };
            b.new_line(number, tokens)
        })
        .collect();
    b.source.splice(at..at, ids);
    renumber(b, pos);
}

/// Numbers the block's lines from source index `from` on by their position.
fn renumber(b: &mut Block, from: usize) {
    let Some(&first) = b.source.first() else {
        return;
    };
    let base = b.lines[first].number;
    for k in from..b.source.len() {
        let id = b.source[k];
        b.lines[id].number = base + k as i64;
    }
}

/// How one function parameter (or a destructured property inside one) is documented by
/// name: what `@param` paths it expects.
#[derive(Debug, Clone)]
pub enum ParamName {
    /// A plain name (`a`, `this`, a property key).
    Name(String),
    /// Nothing nameable (a computed key, an index signature).
    Missing,
    /// A rest element or an array slot: `restElement` / `isRestProperty` flags with it.
    Item {
        name: ItemName,
        rest_element: bool,
        rest_property: bool,
    },
    /// A destructured (or type-literal-annotated) root and what it holds.
    Root(Option<String>, Box<Inner>),
}

#[derive(Debug, Clone)]
pub enum ItemName {
    Str(String),
    /// `...[a, b]`: the names in the array pattern.
    List(Vec<Option<String>>),
    Undefined,
}

impl ItemName {
    /// The name as JavaScript's `String()` shows it.
    pub fn display(&self) -> String {
        match self {
            ItemName::Str(s) => s.clone(),
            ItemName::List(l) => l
                .iter()
                .map(|n| n.clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(","),
            ItemName::Undefined => "undefined".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Inner {
    Items(Vec<ParamName>),
    Flat(Flat),
}

/// A destructured root flattened to dotted paths (`a`, `a.b`), with each path's
/// "is a rest property" flag (`rests` can run longer than `names`: a nameless root
/// still takes a slot).
#[derive(Debug, Clone, Default)]
pub struct Flat {
    pub names: Vec<String>,
    pub rests: Vec<bool>,
    pub has_rest_element: bool,
    pub has_property_rest: bool,
    /// The parameter's own name when its type annotation is a type literal.
    pub annotation: Option<String>,
}

/// Flattens nested roots into dotted paths under `root` (empty for none).
pub fn flatten_roots(items: &[ParamName], root: &str) -> Flat {
    let mut out = Flat::default();
    let join = |n: &str| {
        if root.is_empty() {
            n.to_owned()
        } else {
            format!("{root}.{n}")
        }
    };
    for cur in items {
        match cur {
            ParamName::Root(name, inner) => {
                let nested;
                let items: &[ParamName] = match &**inner {
                    Inner::Items(v) => v,
                    Inner::Flat(f) => {
                        out.has_rest_element |= f.has_rest_element;
                        out.has_property_rest |= f.has_property_rest;
                        nested = f
                            .names
                            .iter()
                            .cloned()
                            .map(ParamName::Name)
                            .collect::<Vec<_>>();
                        &nested
                    }
                };
                let prefix = match name {
                    Some(n) => join(n),
                    None if root.is_empty() => String::new(),
                    None => join("undefined"),
                };
                let f = flatten_roots(items, &prefix);
                out.has_rest_element |= f.has_rest_element;
                out.has_property_rest |= f.has_property_rest;
                if !prefix.is_empty() {
                    out.names.push(prefix);
                }
                out.names
                    .extend(f.names.into_iter().filter(|n| !n.is_empty()));
                out.rests.push(false);
                out.rests.extend(f.rests);
            }
            ParamName::Item {
                name,
                rest_element,
                rest_property,
            } => {
                out.rests.push(*rest_property);
                out.has_property_rest |= *rest_property;
                out.has_rest_element |= *rest_element;
                out.names.push(join(&name.display()));
            }
            ParamName::Name(n) => {
                out.rests.push(false);
                out.names.push(join(n));
            }
            ParamName::Missing => {}
        }
    }
    out
}

/// What each of a function's parameters expects to be documented as
/// (`getFunctionParameterNames`). `default_objects`: a property defaulting to an object
/// literal documents that literal's keys.
pub fn function_param_names(
    ast: &Ast,
    src: &str,
    params: NodeList,
    default_objects: bool,
) -> Vec<ParamName> {
    let p = ParamNames {
        ast,
        src,
        default_objects,
    };
    ast.list(params).iter().map(|&id| p.param(id)).collect()
}

/// The parameters of a function-like target: a function, a method signature, or a
/// property signature typed as a function.
pub fn target_params(ast: &Ast, id: NodeId) -> Option<NodeList> {
    match *ast.node(id) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. }
        | Node::MethodSignature { params, .. } => Some(params),
        Node::MethodDef { value, .. } => target_params(ast, value),
        Node::PropertySignature {
            type_ann: Some(t), ..
        } => match *ast.node(skip_paren_type(ast, t)) {
            Node::FunctionType { params, .. } => Some(params),
            _ => None,
        },
        _ => None,
    }
}

fn skip_paren_type(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..MAX_DEPTH {
        match *ast.node(id) {
            Node::ParenType { ty } => id = ty,
            _ => break,
        }
    }
    id
}

struct ParamNames<'a> {
    ast: &'a Ast,
    src: &'a str,
    default_objects: bool,
}

impl ParamNames<'_> {
    fn ident(&self, id: NodeId) -> Option<String> {
        match *self.ast.node(id) {
            Node::Ident { name } => Some(self.ast.atom(name).to_owned()),
            Node::This {} => Some("this".to_owned()),
            _ => None,
        }
    }

    fn param(&self, id: NodeId) -> ParamName {
        let ast = self.ast;
        let Node::Param {
            rest,
            pattern,
            type_ann,
            init,
            ..
        } = *ast.node(id)
        else {
            return ParamName::Missing;
        };
        if let Some(t) = type_ann
            && let Node::TypeLiteral { members } = *ast.node(skip_paren_type(ast, t))
        {
            let props: Vec<ParamName> = ast
                .list(members)
                .iter()
                .map(|&m| self.signature(m, 0))
                .collect();
            let plain = !rest && init.is_none();
            let name = self.ident(pattern);
            let mut flat = flatten_roots(&props, "");
            flat.annotation = if plain { name.clone() } else { None };
            let root = if rest { None } else { name };
            return ParamName::Root(root, Box::new(Inner::Flat(flat)));
        }
        if rest {
            return self.rest(pattern, false);
        }
        if let Some(n) = self.ident(pattern) {
            return ParamName::Name(n);
        }
        self.pattern_root(pattern, 0)
    }

    /// A top-level object or array pattern.
    fn pattern_root(&self, id: NodeId, depth: u32) -> ParamName {
        let ast = self.ast;
        let items: Vec<ParamName> = match *ast.node(id) {
            Node::ObjectPattern { props } => ast
                .list(props)
                .iter()
                .map(|&p| self.prop(p, true, depth + 1))
                .collect(),
            Node::ArrayPattern { elements } => ast
                .list(elements)
                .iter()
                .enumerate()
                .map(|(i, &e)| ParamName::Item {
                    name: ItemName::Str(format!("\"{i}\"")),
                    rest_element: matches!(ast.node(e), Node::Rest { .. }),
                    rest_property: false,
                })
                .collect(),
            _ => return ParamName::Missing,
        };
        ParamName::Root(None, Box::new(Inner::Flat(flatten_roots(&items, ""))))
    }

    fn rest(&self, arg: NodeId, property: bool) -> ParamName {
        let name = match *self.ast.node(arg) {
            Node::Ident { name } => ItemName::Str(self.ast.atom(name).to_owned()),
            Node::ArrayPattern { elements } => ItemName::List(
                self.ast
                    .list(elements)
                    .iter()
                    .map(|&e| self.ident(e))
                    .collect(),
            ),
            _ => ItemName::Undefined,
        };
        ParamName::Item {
            name,
            rest_element: true,
            rest_property: property,
        }
    }

    fn array_items(&self, elements: NodeList) -> Vec<ParamName> {
        self.ast
            .list(elements)
            .iter()
            .enumerate()
            .map(|(i, &e)| ParamName::Item {
                name: ItemName::Str(i.to_string()),
                rest_element: matches!(self.ast.node(e), Node::Rest { .. }),
                rest_property: false,
            })
            .collect()
    }

    fn key(&self, key: NodeId) -> ParamName {
        let ast = self.ast;
        let key = match *ast.node(key) {
            Node::ComputedKey { expr } => expr,
            _ => key,
        };
        match *ast.node(key) {
            Node::Ident { name } => ParamName::Name(ast.atom(name).to_owned()),
            Node::StringLit { .. } | Node::NumberLit { .. } | Node::BigIntLit { .. } => {
                ParamName::Name(ast.span(key).text(self.src).to_owned())
            }
            _ => ParamName::Missing,
        }
    }

    /// One property of an object pattern.
    fn prop(&self, id: NodeId, property: bool, depth: u32) -> ParamName {
        let ast = self.ast;
        if depth > MAX_DEPTH {
            return ParamName::Missing;
        }
        let (key, value, init) = match *ast.node(id) {
            Node::Rest { arg } => return self.rest(arg, property),
            Node::BindingProperty { key, value, init } => (key, value, init),
            _ => return ParamName::Missing,
        };
        let key_name = self
            .ident(key)
            .filter(|_| matches!(ast.node(key), Node::Ident { .. }));
        let root = |items| ParamName::Root(key_name.clone(), Box::new(Inner::Items(items)));
        let value_node = value.map(|v| *ast.node(v));
        match (value_node, init) {
            (Some(Node::ArrayPattern { elements }), _) => return root(self.array_items(elements)),
            (Some(Node::ObjectPattern { props }), _) => {
                let items = ast
                    .list(props)
                    .iter()
                    .map(|&p| self.prop(p, property, depth + 1))
                    .collect();
                return root(items);
            }
            (None | Some(Node::Ident { .. }), Some(init)) if self.default_objects => {
                if let Node::ObjectLit { props } = *ast.node(init) {
                    let items = ast
                        .list(props)
                        .iter()
                        .map(|&p| self.literal_prop(p))
                        .collect();
                    return root(items);
                }
            }
            _ => {}
        }
        self.key(key)
    }

    /// A property of a default object literal.
    fn literal_prop(&self, id: NodeId) -> ParamName {
        match *self.ast.node(id) {
            Node::Property { key, .. } | Node::MethodDef { key, .. } => self.key(key),
            Node::ShorthandProp { name, .. } => self.key(name),
            _ => ParamName::Missing,
        }
    }

    /// A member of a type literal annotating a parameter.
    fn signature(&self, id: NodeId, depth: u32) -> ParamName {
        let ast = self.ast;
        if depth > MAX_DEPTH {
            return ParamName::Missing;
        }
        let key_name = |key: NodeId| match *ast.node(key) {
            Node::Ident { name } => ParamName::Name(ast.atom(name).to_owned()),
            _ => ParamName::Missing,
        };
        match *ast.node(id) {
            Node::PropertySignature { key, type_ann, .. } => {
                if let Some(t) = type_ann
                    && let Node::TypeLiteral { members } = *ast.node(skip_paren_type(ast, t))
                {
                    let name = match key_name(key) {
                        ParamName::Name(n) => Some(n),
                        _ => None,
                    };
                    let items = ast
                        .list(members)
                        .iter()
                        .map(|&m| self.signature(m, depth + 1))
                        .collect();
                    return ParamName::Root(name, Box::new(Inner::Items(items)));
                }
                key_name(key)
            }
            Node::MethodSignature { key, .. } => key_name(key),
            _ => ParamName::Missing,
        }
    }
}

/// Drops the quotes of quoted path segments: `a."b"` is `a.b`.
pub fn drop_path_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.')
            && let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'')
        {
            let body = &after[1..];
            let line_end = body
                .find(['\n', '\r', '\u{2028}', '\u{2029}'])
                .unwrap_or(body.len());
            if let Some(close) = body[..line_end].rfind(q) {
                out.push('.');
                out.push_str(&body[..close]);
                rest = &body[close + 1..];
                continue 'outer;
            }
        }
        let mut it = rest.chars();
        if let Some(c) = it.next() {
            out.push(c);
        }
        rest = it.as_str();
    }
    out
}

/// The two paths name the same thing (ignoring quoted segments).
pub fn same_path(a: &str, b: &str) -> bool {
    a == b || drop_path_quotes(a) == drop_path_quotes(b)
}

/// `name` does not start with `other` (quoted or not).
pub fn path_does_not_begin_with(name: &str, other: &str) -> bool {
    !name.starts_with(other) && !drop_path_quotes(name).starts_with(&drop_path_quotes(other))
}

/// `getRegexFromString` then `search`: whether `text` matches `re`, given as `/pattern/flags`
/// or a bare pattern (the `v` flag when none is given). A pattern that doesn't compile
/// matches nothing.
pub fn regex_string_matches(re: &str, text: &str) -> bool {
    let (pattern, flags) = match re
        .strip_prefix('/')
        .and_then(|r| r.rfind('/').map(|i| (&r[..i], &r[i + 1..])))
    {
        Some((p, f)) if f.chars().all(|c| "gimyvus".contains(c)) => {
            (p, if f.is_empty() { "v" } else { f })
        }
        _ => (re, "v"),
    };
    crate::regex::parse(pattern, crate::regex::Flags::new(flags))
        .is_ok_and(|r| r.is_match(text, flags.contains('i')))
}
