//! `unicorn/no-unnecessary-default-arguments`: trailing call arguments equal to their
//! parameter's primitive default, and object-argument properties equal to the default of a
//! destructured parameter, are unnecessary.
//!
//! Only calls of a name bound once, never reassigned, to a function in this file (a
//! function declaration not in the global scope of a script, or a `const` initialized with
//! a function or arrow) with a body, no type parameters and no `arguments` use. Values are
//! primitive literals (signed numbers, `-` bigints, expression-free templates) compared
//! like `Object.is`. Nothing is reported in a file with `with`, a direct `eval` call or a
//! sloppy-mode function declaration in a block. The fix (removal) becomes a suggestion in
//! TypeScript files and files with JSDoc types or `@ts-check`, and is withheld when it
//! would remove comments. Imported functions need type information, which the rule doesn't
//! use here.

use super::prelude::*;
use super::this_binding::is_strict;
use super::unicorn_conditions::unwrap_ts_and_parens;
use super::unicorn_text::{paren_span, would_remove_comments};
use super::unicorn_util::is_direct_eval_call;
use crate::semantic::DeclKind;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-default-arguments",
        "Disallow arguments that match their parameter defaults.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<NoUnnecessaryDefaultArguments>,
};

#[derive(Debug, Default)]
struct NoUnnecessaryDefaultArguments;

impl FromOptions for NoUnnecessaryDefaultArguments {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(NoUnnecessaryDefaultArguments)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Prim {
    Str(String),
    Num(f64),
    BigInt(i128),
    BigIntText(String),
    Bool(bool),
    Null,
}

fn same_value(a: &Prim, b: &Prim) -> bool {
    match (a, b) {
        (Prim::Num(x), Prim::Num(y)) => x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

fn bigint(text: &str, negative: bool) -> Prim {
    let digits = text.replace('_', "");
    let parsed = if let Some(h) = digits.strip_prefix("0x").or(digits.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()
    } else if let Some(o) = digits.strip_prefix("0o").or(digits.strip_prefix("0O")) {
        i128::from_str_radix(o, 8).ok()
    } else if let Some(b) = digits.strip_prefix("0b").or(digits.strip_prefix("0B")) {
        i128::from_str_radix(b, 2).ok()
    } else {
        digits.parse::<i128>().ok()
    };
    match parsed {
        Some(v) => Prim::BigInt(if negative { -v } else { v }),
        None => Prim::BigIntText(format!("{}{digits}", if negative { "-" } else { "" })),
    }
}

/// `getPrimitiveLiteralValue`.
fn primitive(ast: &Ast, id: Option<NodeId>) -> Option<Prim> {
    let id = unwrap_ts_and_parens(ast, id?);
    match *ast.node(id) {
        Node::StringLit { value } => Some(Prim::Str(ast.atom(value).to_owned())),
        Node::NumberLit { value } => Some(Prim::Num(value.get())),
        Node::BigIntLit { value } => Some(bigint(ast.atom(value), false)),
        Node::BoolLit { value } => Some(Prim::Bool(value)),
        Node::NullLit {} => Some(Prim::Null),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [] => Some(Prim::Str(String::new())),
            [only] => match *ast.node(*only) {
                Node::TemplateElement { cooked, .. } => {
                    Some(Prim::Str(ast.atom(cooked).to_owned()))
                }
                _ => None,
            },
            _ => None,
        },
        Node::Unary { op, arg } => {
            let arg = unwrap_ts_and_parens(ast, arg);
            match (op, ast.node(arg)) {
                (UnaryOp::Minus, Node::NumberLit { value }) => Some(Prim::Num(-value.get())),
                (UnaryOp::Plus, Node::NumberLit { value }) => Some(Prim::Num(value.get())),
                (UnaryOp::Minus, Node::BigIntLit { value }) => Some(bigint(ast.atom(*value), true)),
                _ => None,
            }
        }
        _ => None,
    }
}

const OBJECT_PROTOTYPE: &[&str] = &[
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

fn js_number_key(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e21 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn key_name(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(js_number_key(value.get())),
        _ => None,
    }
}

struct Parameter {
    value: Option<Prim>,
    /// Destructured property defaults (`None`: not an object pattern).
    properties: Option<Vec<(String, Option<Prim>)>>,
}

/// The defaults of an object pattern's properties (empty when it has a computed key,
/// a duplicate or `__proto__`).
fn pattern_defaults(ast: &Ast, pattern: NodeId) -> Vec<(String, Option<Prim>)> {
    let Node::ObjectPattern { props } = *ast.node(pattern) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Option<Prim>)> = Vec::new();
    for &p in ast.list(props) {
        match *ast.node(p) {
            Node::Rest { .. } => {}
            Node::BindingProperty { key, value, init } => {
                let Some(name) = key_name(ast, key) else {
                    return Vec::new();
                };
                if name == "__proto__" || out.iter().any(|(n, _)| *n == name) {
                    return Vec::new();
                }
                let binding = value.unwrap_or(key);
                let default = (init.is_some()
                    && matches!(ast.node(binding), Node::Ident { .. })
                    && !OBJECT_PROTOTYPE.contains(&name.as_str()))
                .then(|| primitive(ast, init))
                .flatten();
                out.push((name, default));
            }
            _ => return Vec::new(),
        }
    }
    out
}

/// An `arguments` reference that belongs to `function` (not a nested non-arrow function).
fn uses_arguments(cx: &Ctx<'_, '_>, function: NodeId) -> bool {
    struct Find<'c, 'a, 'b> {
        cx: &'c Ctx<'a, 'b>,
        root: NodeId,
        found: bool,
    }
    impl Visitor for Find<'_, '_, '_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            let node = ast.node(id);
            if id != self.root
                && matches!(node, Node::FunctionDecl { .. } | Node::FunctionExpr { .. })
            {
                return Walk::SkipChildren;
            }
            if matches!(node, Node::Ident { .. })
                && ident_name(ast, id) == Some("arguments")
                && self.cx.semantic().reference_at(id).is_some()
            {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find {
        cx,
        root: function,
        found: false,
    };
    walk(cx.ast(), function, &mut f);
    f.found
}

fn local_parameters(cx: &Ctx<'_, '_>, function: NodeId) -> Option<Vec<Parameter>> {
    let ast = cx.ast();
    let (type_params, params, body) = match *ast.node(function) {
        Node::FunctionDecl {
            type_params,
            params,
            body,
            ..
        }
        | Node::FunctionExpr {
            type_params,
            params,
            body,
            ..
        } => (type_params, params, body?),
        Node::Arrow {
            type_params,
            params,
            body,
            ..
        } => (type_params, params, body),
        _ => return None,
    };
    let _ = body;
    if type_params.is_some_and(|t| match *ast.node(t) {
        Node::TypeParams { params } => !ast.list(params).is_empty(),
        _ => true,
    }) || uses_arguments(cx, function)
    {
        return None;
    }
    let mut out = Vec::new();
    for &p in ast.list(params) {
        let Node::Param { pattern, init, .. } = *ast.node(p) else {
            continue;
        };
        if matches!(ast.node(pattern), Node::Ident { .. })
            && ident_name(ast, pattern) == Some("this")
        {
            continue;
        }
        if matches!(ast.node(pattern), Node::Ident { .. }) {
            out.push(Parameter {
                value: primitive(ast, init),
                properties: None,
            });
        } else {
            out.push(Parameter {
                value: None,
                properties: Some(pattern_defaults(ast, pattern)),
            });
        }
    }
    Some(out)
}

/// `getFunctionFromIdentifier` plus upstream's binding checks.
fn callee_function(cx: &Ctx<'_, '_>, callee: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let name = ident_name(ast, callee)?;
    let sym = sem.lookup(sem.enclosing_scope(callee), name)?;
    let symbol = sem.symbol(sym);
    if symbol.decls.len() != 1
        || symbol.refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(crate::semantic::RefFlags::INIT)
        })
    {
        return None;
    }
    let decl = &symbol.decls[0];
    match decl.kind {
        DeclKind::Function => {
            if sem.scope(symbol.scope).kind == crate::semantic::ScopeKind::Global {
                return None;
            }
            let f = cx
                .parent(decl.node)
                .filter(|&f| matches!(ast.node(f), Node::FunctionDecl { .. }));
            let f = f.or_else(|| {
                matches!(ast.node(decl.node), Node::FunctionDecl { .. }).then_some(decl.node)
            })?;
            matches!(*ast.node(f), Node::FunctionDecl { body: Some(_), .. }).then_some(f)
        }
        DeclKind::Const => {
            let declarator = std::iter::successors(Some(decl.node), |&n| cx.parent(n))
                .take(3)
                .find(|&n| matches!(ast.node(n), Node::VarDeclarator { .. }))?;
            let Node::VarDeclarator {
                name: id,
                init: Some(init),
                ..
            } = *ast.node(declarator)
            else {
                return None;
            };
            if !matches!(ast.node(id), Node::Ident { .. }) {
                return None;
            }
            let init = unwrap_ts_and_parens(ast, init);
            matches!(
                ast.node(init),
                Node::FunctionExpr { body: Some(_), .. } | Node::Arrow { .. }
            )
            .then_some(init)
        }
        _ => None,
    }
}

/// The source with every token of `span` removed: whitespace between tokens stays (as
/// ESLint's merged token removals leave it), whitespace inside literals goes.
fn token_removal(cx: &Ctx<'_, '_>, span: Span, literals: &[Span]) -> String {
    let src = cx.src();
    let text = &src[span.start as usize..span.end as usize];
    let mut out = String::new();
    for (i, c) in text.char_indices() {
        let pos = span.start + i as u32;
        if c.is_whitespace() && !literals.iter().any(|l| pos >= l.start && pos < l.end) {
            out.push(c);
        }
    }
    out
}

fn literal_spans(ast: &Ast, root: NodeId) -> Vec<Span> {
    struct Collect {
        out: Vec<Span>,
    }
    impl Visitor for Collect {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(
                ast.node(id),
                Node::StringLit { .. } | Node::TemplateLit { .. }
            ) {
                self.out.push(ast.span(id));
                return Walk::SkipChildren;
            }
            Walk::Continue
        }
    }
    let mut c = Collect { out: Vec::new() };
    walk(ast, root, &mut c);
    c.out
}

fn remove_property(cx: &Ctx<'_, '_>, object_props: &[NodeId], property: NodeId) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(property);
    let mut fix = Fix::replace(span, token_removal(cx, span, &literal_spans(ast, property)));
    let after = skip_trivia_forward(src, span.end as usize);
    if src[after..].starts_with(',') {
        fix = fix.and(Span::new(after as u32, after as u32 + 1), "");
    } else if object_props.len() > 1 && object_props.last() == Some(&property) {
        let before = skip_trivia_backward(ast, src, span.start as usize);
        if before > 0 && src[..before].ends_with(',') {
            fix = Fix::replace(Span::new(before as u32 - 1, before as u32), "").and(span, {
                token_removal(cx, span, &literal_spans(ast, property))
            });
        }
    }
    fix
}

/// The properties of an object literal by name; `None` for spreads, methods, accessors,
/// computed or duplicate keys and `__proto__`.
fn object_properties(ast: &Ast, object: NodeId) -> Option<Vec<(String, NodeId, Option<NodeId>)>> {
    let Node::ObjectLit { props } = *ast.node(object) else {
        return None;
    };
    let mut out: Vec<(String, NodeId, Option<NodeId>)> = Vec::new();
    for &p in ast.list(props) {
        let (name, value) = match *ast.node(p) {
            Node::Property { key, value, .. } => (key_name(ast, key)?, Some(value)),
            Node::ShorthandProp {
                name, init: None, ..
            } => (key_name(ast, name)?, Some(name)),
            _ => return None,
        };
        if name == "__proto__" || out.iter().any(|(n, _, _)| *n == name) {
            return None;
        }
        out.push((name, p, value));
    }
    Some(out)
}

/// `@ts-check` or JSDoc type tags in a comment.
fn has_js_type_annotations(cx: &Ctx<'_, '_>) -> bool {
    const TAGS: &[&str] = &[
        "param",
        "arg",
        "argument",
        "type",
        "returns",
        "return",
        "template",
        "this",
        "overload",
        "satisfies",
        "constructor",
        "class",
        "implements",
        "extends",
        "augments",
    ];
    let src = cx.src();
    cx.ast().comments.iter().any(|c| {
        let text = &src[c.span.start as usize..c.span.end as usize];
        let (block, value) = if let Some(v) = text.strip_prefix("/*") {
            (true, v.strip_suffix("*/").unwrap_or(v))
        } else {
            (false, text.strip_prefix("//").unwrap_or(text))
        };
        let lead = value.trim_start();
        let lead = lead.strip_prefix(['*', '/']).map_or(lead, str::trim_start);
        if lead
            .get(..9)
            .is_some_and(|s| s.eq_ignore_ascii_case("@ts-check"))
            && !lead[9..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return true;
        }
        block
            && value.trim_start().starts_with('*')
            && value.match_indices('@').any(|(i, _)| {
                let rest = &value[i + 1..];
                TAGS.iter().any(|t| {
                    rest.starts_with(t)
                        && !rest[t.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
            })
    })
}

struct Scan {
    calls: Vec<NodeId>,
    dynamic: bool,
}

impl NoUnnecessaryDefaultArguments {
    fn scan(&self, cx: &Ctx<'_, '_>) -> Scan {
        struct V<'c, 'a, 'b> {
            cx: &'c Ctx<'a, 'b>,
            s: Scan,
        }
        impl Visitor for V<'_, '_, '_> {
            fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
                let cx = self.cx;
                match *ast.node(id) {
                    Node::With { .. } => self.s.dynamic = true,
                    Node::FunctionDecl { .. } => {
                        let in_block = cx.parent(id).is_some_and(|p| {
                            !matches!(cx.node(p), Node::Program { .. })
                                && !cx.parent(p).is_some_and(|g| {
                                    matches!(
                                        cx.node(g),
                                        Node::FunctionDecl { .. }
                                            | Node::FunctionExpr { .. }
                                            | Node::Arrow { .. }
                                    )
                                })
                                && !matches!(
                                    cx.node(p),
                                    Node::ExportNamed { .. } | Node::Module { .. }
                                )
                        });
                        if in_block && !is_strict(cx, id) {
                            self.s.dynamic = true;
                        }
                    }
                    Node::Call { callee, args, .. } => {
                        if is_direct_eval_call(ast, id) {
                            self.s.dynamic = true;
                        }
                        let callee = unwrap_ts_and_parens(ast, callee);
                        let args = ast.list(args);
                        if matches!(ast.node(callee), Node::Ident { .. })
                            && (primitive(ast, args.last().copied()).is_some()
                                || args.iter().any(|&a| {
                                    matches!(
                                        ast.node(unwrap_ts_and_parens(ast, a)),
                                        Node::ObjectLit { .. }
                                    )
                                }))
                        {
                            self.s.calls.push(id);
                        }
                    }
                    _ => {}
                }
                Walk::Continue
            }
        }
        let mut v = V {
            cx,
            s: Scan {
                calls: Vec::new(),
                dynamic: false,
            },
        };
        walk(cx.ast(), cx.ast().root, &mut v);
        v.s
    }
}

fn emit(
    cx: &mut Ctx<'_, '_>,
    span: Span,
    message: String,
    id: &'static str,
    fix: Option<Fix>,
    review: bool,
    suggestion: (&'static str, &'static str),
) {
    let r = cx.report(span, message).id(id);
    match fix {
        Some(f) if review => {
            r.suggest(suggestion.0, suggestion.1, f);
        }
        Some(f) => {
            r.fix(f);
        }
        None => {}
    }
}

impl Rule for NoUnnecessaryDefaultArguments {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let scan = self.scan(cx);
        if scan.dynamic {
            return;
        }
        let ast = cx.ast();
        let src = cx.src();
        let review = super::unicorn_prefer_spread::is_ts_file(cx)
            || cx.env().typescript
            || has_js_type_annotations(cx);
        for call in scan.calls {
            let Node::Call { callee, args, .. } = *ast.node(call) else {
                continue;
            };
            let callee = unwrap_ts_and_parens(ast, callee);
            let Some(function) = callee_function(cx, callee) else {
                continue;
            };
            let Some(params) = local_parameters(cx, function) else {
                continue;
            };
            let args = ast.list(args);
            let spread = args
                .iter()
                .position(|&a| matches!(ast.node(a), Node::Spread { .. }));
            if spread.is_none() {
                let mut first = args.len();
                while first > 0 {
                    let i = first - 1;
                    let Some(value) = params.get(i).and_then(|p| p.value.as_ref()) else {
                        break;
                    };
                    if !primitive(ast, Some(args[i])).is_some_and(|a| same_value(value, &a)) {
                        break;
                    }
                    first -= 1;
                }
                if first < args.len() {
                    let first_arg = args[first];
                    let last_arg = args[args.len() - 1];
                    let start = if first == 0 {
                        paren_span(cx, first_arg).start
                    } else {
                        let before = skip_trivia_backward(
                            ast,
                            src,
                            paren_span(cx, first_arg).start as usize,
                        );
                        (before as u32).saturating_sub(1)
                    };
                    let mut end = paren_span(cx, last_arg).end;
                    let after = skip_trivia_forward(src, end as usize);
                    if src[after..].starts_with(',') {
                        end = after as u32 + 1;
                    }
                    let range = Span::new(start, end);
                    let fix =
                        (!would_remove_comments(cx, range, &[])).then(|| Fix::replace(range, ""));
                    emit(
                        cx,
                        cx.span(skip_parens(ast, first_arg)),
                        "Omit trailing arguments that match their parameter defaults.".to_owned(),
                        "arguments",
                        fix,
                        review,
                        (
                            "remove-arguments",
                            "Remove these arguments (requires type review).",
                        ),
                    );
                }
            }
            let before_spread = &args[..spread.unwrap_or(args.len())];
            for (i, &arg) in before_spread.iter().enumerate() {
                let Some(defaults) = params.get(i).and_then(|p| p.properties.as_ref()) else {
                    continue;
                };
                let object = unwrap_ts_and_parens(ast, arg);
                let Some(props) = object_properties(ast, object) else {
                    continue;
                };
                let all: Vec<NodeId> = props.iter().map(|(_, p, _)| *p).collect();
                for (name, property, value) in &props {
                    let Some(default) = defaults
                        .iter()
                        .find(|(n, _)| n == name)
                        .and_then(|(_, d)| d.as_ref())
                    else {
                        continue;
                    };
                    if !primitive(ast, *value).is_some_and(|v| same_value(default, &v)) {
                        continue;
                    }
                    let span = cx.span(*property);
                    let fix = (!would_remove_comments(cx, span, &[]))
                        .then(|| remove_property(cx, &all, *property));
                    emit(
                        cx,
                        span,
                        format!("Property `{name}` matches its destructuring default."),
                        "property",
                        fix,
                        review,
                        (
                            "remove-property",
                            "Remove this property (requires type review).",
                        ),
                    );
                }
            }
        }
    }
}
