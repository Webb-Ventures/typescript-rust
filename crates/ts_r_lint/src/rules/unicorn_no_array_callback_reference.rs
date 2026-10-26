//! `unicorn/no-array-callback-reference`: no function reference passed straight to an
//! iterator method (`array.map(callback)`), since the method hands it extra arguments.
//!
//! - `x.every|filter|find|findLast|findIndex|findLastIndex|flatMap|forEach|map|reduce|
//!   reduceRight|some(cb[, t])`: one or two arguments, no spread among them, no `?.(`, a
//!   plain property name. Skipped when the call is awaited (not for `reduce*`), when `x` (or
//!   the callee of a call `x`) is an ignored callee (`Promise`, `lodash`, `_`, `$`, `this`,
//!   ... and the `ignore` option, matched as dotted paths), for `Vue.filter` and
//!   `types.map`, and when `x` is known not to be an array or typed array (class syntax
//!   and heritage resolved, a union with any non-array member is not one, nullish members
//!   dropped).
//! - `cb` is split through `?:` into its branches; each branch is reported unless it is a
//!   function, a call, a per-method allowed global (`Boolean`, and `String`/`Number`/...
//!   for `map`), a value that can't be a function (literals, operators, classes, `this`,
//!   `undefined`, ... directly or through `const` initializers), or, for `every`/`filter`/
//!   `find`/`findLast`, a type predicate: a local function or variable whose own
//!   annotation returns `x is T`, or any import.
//! - Suggestions (none for `yield`/`await` branches) wrap the branch in an arrow taking
//!   `element, index, array` (`accumulator, ...` for `reduce*`) from the shallowest useful
//!   depth: the callback's declared parameter count when its declaration is in reach (a
//!   rest parameter takes them all), at least one (two for `reduce*`). With an identifier
//!   callback on an identifier receiver, `element`/`array` become the receiver's singular
//!   and its name when those are free valid names. Any name the callback refers to gets
//!   `_` appended until free. `forEach` gets a block body.

use super::prelude::*;
use super::unicorn_ast::method_call;
use super::unicorn_no_array_reduce::const_initializer as const_init;
use super::unicorn_types::{Checker, INDEXED_COLLECTION};
use super::unicorn_util::{parenthesized_span, skip_wrappers};
use super::unicorn_util_h4::singular;
use super::unicorn_util_h5::{
    is_parenthesized, is_value_not_function, node_matches_path, parenthesized_text,
    starts_like_declaration,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-array-callback-reference",
        "Prevent passing a function reference directly to iterator methods.",
        NO_PRESET,
        Category::Suspicious,
    )
    .suggestions(),
    create: create::<NoArrayCallbackReference>,
};

#[derive(Debug, Default)]
struct NoArrayCallbackReference {
    ignore: Vec<String>,
}

impl FromOptions for NoArrayCallbackReference {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoArrayCallbackReference {
            ignore: str_list(o, "ignore"),
        })
    }
}

const DEFAULT_IGNORED: &[&str] = &[
    "Promise",
    "React.Children",
    "Children",
    "lodash",
    "underscore",
    "_",
    "Async",
    "async",
    "this",
    "$",
    "jQuery",
];

const ELEMENT_PARAMS: &[&str] = &["element", "index", "array"];
const REDUCE_PARAMS: &[&str] = &["accumulator", "element", "index", "array"];

struct MethodInfo {
    params: &'static [&'static str],
    min_params: usize,
    returns_undefined: bool,
    allowed: &'static [&'static str],
}

fn method_info(name: &str) -> Option<MethodInfo> {
    let plain = |allowed| MethodInfo {
        params: ELEMENT_PARAMS,
        min_params: 1,
        returns_undefined: false,
        allowed,
    };
    Some(match name {
        "every" | "filter" | "find" | "findLast" | "findIndex" | "findLastIndex" | "some" => {
            plain(&["Boolean"])
        }
        "flatMap" => plain(&[]),
        "forEach" => MethodInfo {
            returns_undefined: true,
            ..plain(&[])
        },
        "map" => plain(&["String", "Number", "BigInt", "Boolean", "Symbol"]),
        "reduce" | "reduceRight" => MethodInfo {
            params: REDUCE_PARAMS,
            min_params: 2,
            returns_undefined: false,
            allowed: &[],
        },
        _ => return None,
    })
}

/// The receiver checker: arrays and typed arrays, with class syntax and heritage, a union
/// with any non-array member counting as non-array, nullish members dropped.
static RECEIVER: Checker = Checker {
    class_heritage: true,
    class_syntax: true,
    mixed_union_non_target: true,
    nullish_in_mixed_union: true,
    ..INDEXED_COLLECTION
};

impl Rule for NoArrayCallbackReference {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Some(call) = method_call(ast, node) else {
            return;
        };
        if call.optional_call
            || call.args.is_empty()
            || call.args.len() > 2
            || call
                .args
                .iter()
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return;
        }
        let Some(info) = method_info(call.name) else {
            return;
        };
        if !call.name.starts_with("reduce")
            && parent_skip_parens(ast, cx.file.parents, node)
                .is_some_and(|p| matches!(ast.node(p), Node::Await { .. }))
        {
            return;
        }
        let obj = skip_parens(ast, call.obj);
        let ignored = |id: NodeId| {
            DEFAULT_IGNORED
                .iter()
                .copied()
                .chain(self.ignore.iter().map(String::as_str))
                .any(|p| node_matches_path(ast, id, p))
        };
        if ignored(obj) {
            return;
        }
        if let Node::Call { callee, .. } = *ast.node(obj)
            && ignored(skip_parens(ast, callee))
        {
            return;
        }
        if (call.name == "filter" && ident_name(ast, obj) == Some("Vue"))
            || (call.name == "map" && ident_name(ast, obj) == Some("types"))
        {
            return;
        }
        let mut callbacks = Vec::new();
        let mut stack = vec![skip_parens(ast, call.args[0])];
        while let Some(n) = stack.pop() {
            if callbacks.len() > 10_000 {
                return;
            }
            match *ast.node(n) {
                Node::Conditional { cons, alt, .. } => {
                    stack.push(skip_parens(ast, alt));
                    stack.push(skip_parens(ast, cons));
                }
                _ => callbacks.push(n),
            }
        }
        callbacks.retain(|&cb| !ignore_callback(cx, cb, call.name, &info));
        if callbacks.is_empty() || RECEIVER.is_known_non_target(cx, call.obj) {
            return;
        }
        for cb in callbacks {
            report(cx, cb, obj, call.name, &info);
        }
    }
}

fn ignore_callback(cx: &Ctx<'_, '_>, cb: NodeId, method: &str, info: &MethodInfo) -> bool {
    let ast = cx.ast();
    matches!(
        ast.node(cb),
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::Call { .. }
    ) || ident_name(ast, cb).is_some_and(|n| info.allowed.contains(&n))
        || definitely_not_function(cx, cb)
        || is_value_not_function(ast, cb)
        || (matches!(method, "every" | "filter" | "find" | "findLast")
            && is_type_predicate_callback(cx, cb))
}

fn definitely_not_function(cx: &Ctx<'_, '_>, mut id: NodeId) -> bool {
    let ast = cx.ast();
    let mut seen = Vec::new();
    for _ in 0..100 {
        id = skip_wrappers(ast, id);
        let direct = match *ast.node(id) {
            Node::ArrayLit { .. }
            | Node::ClassExpr { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::ObjectLit { .. }
            | Node::TemplateLit { .. }
            | Node::Unary { .. }
            | Node::Update { .. } => true,
            Node::Binary { op, .. } => !matches!(
                op,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
            ),
            _ => is_undefined(ast, id),
        };
        if direct {
            return true;
        }
        let Some(init) = const_init(cx, id) else {
            return false;
        };
        if seen.contains(&init) {
            return false;
        }
        seen.push(init);
        id = init;
    }
    false
}

/// `type` is a function type (no parentheses) whose return type is a type predicate.
fn is_predicate_function_type(ast: &ts_r_ast::Ast, ty: Option<NodeId>) -> bool {
    ty.is_some_and(|t| {
        matches!(*ast.node(t), Node::FunctionType { return_type, .. }
            if matches!(ast.node(return_type), Node::TypePredicate { .. }))
    })
}

fn returns_predicate(ast: &ts_r_ast::Ast, f: NodeId) -> bool {
    let rt = match *ast.node(f) {
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type,
        _ => None,
    };
    rt.is_some_and(|t| matches!(ast.node(t), Node::TypePredicate { .. }))
}

fn is_type_predicate_callback(cx: &Ctx<'_, '_>, cb: NodeId) -> bool {
    let ast = cx.ast();
    if !matches!(ast.node(cb), Node::Ident { .. }) {
        return false;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.reference_at(cb).and_then(|r| sem.reference(r).symbol()) else {
        return false;
    };
    let Some(d) = sem.symbol(sym).decls.first().copied() else {
        return false;
    };
    match d.kind {
        DeclKind::Function => returns_predicate(ast, d.node),
        DeclKind::ImportDefault | DeclKind::ImportNamed | DeclKind::ImportNamespace => true,
        DeclKind::Param => match *ast.node(d.node) {
            Node::Param {
                pattern, type_ann, ..
            } if pattern == d.name => is_predicate_function_type(ast, type_ann),
            _ => false,
        },
        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
            let Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } = *ast.node(d.node)
            else {
                return false;
            };
            if name != d.name {
                return false;
            }
            if is_predicate_function_type(ast, type_ann) {
                return true;
            }
            init.is_some_and(|i| {
                let i = skip_parens(ast, i);
                matches!(ast.node(i), Node::Arrow { .. } | Node::FunctionExpr { .. })
                    && returns_predicate(ast, i)
            })
        }
        _ => false,
    }
}

/// The function a callback names: a function declaration or a variable's initializer; or,
/// for `object.method` with `object` a `const` object literal, the first plain property
/// of that name.
fn declaration(cx: &Ctx<'_, '_>, cb: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    match *ast.node(cb) {
        Node::Ident { .. } => {
            let sym = sem.reference(sem.reference_at(cb)?).symbol()?;
            let d = *sem.symbol(sym).decls.first()?;
            match d.kind {
                DeclKind::Function => Some(d.node),
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                    match *ast.node(d.node) {
                        Node::VarDeclarator { init, .. } => init.map(|i| skip_parens(ast, i)),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        Node::Member {
            obj,
            prop,
            optional: _,
        } => {
            let name = match ast.node(prop) {
                Node::Ident { .. } => ident_name(ast, prop)?,
                _ => return None,
            };
            let init = const_init(cx, skip_wrappers(ast, obj))?;
            let Node::ObjectLit { props } = *ast.node(skip_wrappers(ast, init)) else {
                return None;
            };
            ast.list(props).iter().find_map(|&p| match *ast.node(p) {
                Node::Property { key, value, .. }
                    if matches!(ast.node(key), Node::Ident { .. })
                        && ident_name(ast, key) == Some(name) =>
                {
                    Some(Some(skip_parens(ast, value)))
                }
                Node::MethodDef { key, value, .. }
                    if matches!(ast.node(key), Node::Ident { .. })
                        && ident_name(ast, key) == Some(name) =>
                {
                    Some(Some(value))
                }
                Node::ShorthandProp { name: n, .. } if ident_name(ast, n) == Some(name) => {
                    Some(None)
                }
                _ => None,
            })?
        }
        _ => None,
    }
}

fn declared_param_count(cx: &Ctx<'_, '_>, cb: NodeId) -> Option<usize> {
    let ast = cx.ast();
    let f = declaration(cx, cb)?;
    let params = match *ast.node(f) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. } => ast.list(params),
        _ => return None,
    };
    if params
        .last()
        .is_some_and(|&p| matches!(*ast.node(p), Node::Param { rest: true, .. }))
    {
        return Some(usize::MAX);
    }
    Some(params.len())
}

const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "arguments",
    "eval",
    "globalThis",
    "Infinity",
    "NaN",
    "undefined",
];

fn is_valid_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '$' || first == '_' || ts_r_lexer::is_id_start(first))
        && chars.all(|c| {
            c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}' || ts_r_lexer::is_id_part(c)
        })
        && !RESERVED.contains(&name)
}

fn referenced_names(cx: &Ctx<'_, '_>, cb: NodeId) -> Vec<String> {
    let ast = cx.ast();
    let span = cx.span(cb);
    let mut out: Vec<String> = cx
        .semantic()
        .references()
        .iter()
        .filter(|r| {
            !r.flags.contains(RefFlags::IMPLICIT) && {
                let s = cx.span(r.node);
                s.start >= span.start && s.end <= span.end
            }
        })
        .map(|r| ast.atom(r.name).to_owned())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn suggestion_params(
    cx: &Ctx<'_, '_>,
    obj: NodeId,
    cb: NodeId,
    params: &[&'static str],
) -> Vec<String> {
    let ast = cx.ast();
    let referenced = referenced_names(cx, cb);
    let is_referenced = |n: &str| referenced.iter().any(|r| r == n);
    let mut out: Vec<String> = params.iter().map(|p| (*p).to_owned()).collect();
    if let (Some(_), Some(obj_name)) = (
        matches!(ast.node(cb), Node::Ident { .. }).then_some(()),
        ident_name(ast, obj),
    ) && let Some(element) = singular(obj_name)
    {
        for p in &mut out {
            let replacement = match p.as_str() {
                "element" => Some(element.as_str()),
                "array" => Some(obj_name),
                _ => None,
            };
            if let Some(r) = replacement
                && !params.contains(&r)
                && !is_referenced(r)
                && is_valid_param_name(r)
            {
                *p = r.to_owned();
            }
        }
    }
    for p in &mut out {
        for _ in 0..1000 {
            if !is_referenced(p) {
                break;
            }
            p.push('_');
        }
    }
    out
}

fn report(cx: &mut Ctx<'_, '_>, cb: NodeId, obj: NodeId, method: &str, info: &MethodInfo) {
    let ast = cx.ast();
    let name = match ast.node(cb) {
        Node::Ident { .. } => ident_name(ast, cb).unwrap_or(""),
        _ => "",
    };
    let message = if name.is_empty() {
        format!("Do not pass function directly to `.{method}(…)`.")
    } else {
        format!("Do not pass function `{name}` directly to `.{method}(…)`.")
    };
    let span = cx.span(cb);
    let mut suggestions = Vec::new();
    if !matches!(ast.node(cb), Node::Yield { .. } | Node::Await { .. }) {
        let params = suggestion_params(cx, obj, cb, info.params);
        let declared = declared_param_count(cx, cb).unwrap_or(info.min_params);
        let shallowest = declared.max(info.min_params).min(params.len());
        let mut text = parenthesized_text(cx, cb).to_owned();
        if !is_parenthesized(cx, cb) {
            let callee_parens =
                precedence(ast, cb) < 18 || matches!(ast.node(cb), Node::New { .. });
            let start_parens = if info.returns_undefined {
                starts_like_declaration(cx.src(), span.start)
            } else {
                cx.src()[span.start as usize..].starts_with('{')
            };
            if callee_parens || start_parens {
                text = format!("({text})");
            }
        }
        let range = parenthesized_span(cx, cb);
        for len in shallowest..=params.len() {
            let list = params[..len].join(", ");
            let replacement = if info.returns_undefined {
                format!("({list}) => {{ {text}({list}); }}")
            } else {
                format!("({list}) => {text}({list})")
            };
            let desc = if name.is_empty() {
                format!("Replace function with `… => …({list})`.")
            } else {
                format!("Replace function `{name}` with `… => {name}({list})`.")
            };
            suggestions.push((desc, Fix::replace(range, replacement)));
        }
    }
    let id = if name.is_empty() {
        ("error-without-name", "replace-without-name")
    } else {
        ("error-with-name", "replace-with-name")
    };
    let mut r = cx.report(span, message).id(id.0);
    for (desc, fix) in suggestions {
        r = r.suggest(id.1, desc, fix);
    }
}
