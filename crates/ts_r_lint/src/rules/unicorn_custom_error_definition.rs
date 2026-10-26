//! `unicorn/custom-error-definition`: classes extending an `…Error` class (by name: a
//! PascalCase name ending in `Error`) must be named `…Error`, set `name` to their own name
//! (a class field, or `this.name = …` in the constructor), call `super()`, pass the message
//! to `super()` instead of assigning `this.message` (unless a `message` accessor exists),
//! and, when correctly named and extending a native error that takes `ErrorOptions`,
//! accept an `options` parameter and forward it. `exports.X = class YError …` must export
//! under the class name. Checked on class declarations and classes assigned anywhere.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_conditions::{comment_after, join_fixes};
use super::unicorn_no_error_property_assignment::BUILTIN_ERRORS;
use super::unicorn_text::{is_parenthesized, needs_semicolon, paren_text, static_string_value};
use super::unicorn_util::{indent_unit, line_indent};
use ts_r_ast::visit::{Visitor, Walk, walk};
use ts_r_ast::{Ast, HeritageKind, MethodKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/custom-error-definition",
        "Enforce correct `Error` subclassing.",
        NO_PRESET,
        Category::Correctness,
    )
    .fixable(),
    create: create::<CustomErrorDefinition>,
};

#[derive(Debug, Default)]
struct CustomErrorDefinition;

impl FromOptions for CustomErrorDefinition {
    fn from_options(_opts: &[Value]) -> Result<Self, String> {
        Ok(CustomErrorDefinition)
    }
}

fn unwrap(ast: &Ast, id: NodeId) -> NodeId {
    super::unicorn_conditions::unwrap_ts_and_parens(ast, id)
}

/// `upperFirst(name)` with a trailing `error` (any case) replaced by `Error`, or `Error`
/// appended.
fn class_name_for(name: &str) -> String {
    let mut chars = name.chars();
    let upper: String = match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    let cut = upper.len().saturating_sub(5);
    if upper.len() >= 5 && upper.is_char_boundary(cut) && upper[cut..].eq_ignore_ascii_case("error")
    {
        format!("{}Error", &upper[..cut])
    } else {
        format!("{upper}Error")
    }
}

/// `^(?:[A-Z][\da-z]*)*Error$`.
fn is_error_class_name(name: &str) -> bool {
    let Some(prefix) = name.strip_suffix("Error") else {
        return false;
    };
    prefix.is_empty()
        || (prefix.starts_with(|c: char| c.is_ascii_uppercase())
            && prefix.chars().all(|c| c.is_ascii_alphanumeric()))
}

struct Class {
    node: NodeId,
    name: NodeId,
    members: ts_r_ast::NodeList,
    super_class: Option<NodeId>,
    abstract_: bool,
}

fn class_of(ast: &Ast, id: NodeId) -> Option<Class> {
    let (Node::ClassDecl {
        modifiers,
        name,
        heritage,
        members,
        ..
    }
    | Node::ClassExpr {
        modifiers,
        name,
        heritage,
        members,
        ..
    }) = *ast.node(id)
    else {
        return None;
    };
    let super_class = ast.list(heritage).iter().find_map(|&h| match *ast.node(h) {
        Node::HeritageClause {
            kind: HeritageKind::Extends,
            types,
        } => ast.list(types).first().map(|&t| match *ast.node(t) {
            Node::ExprWithTypeArgs { expr, .. } => expr,
            _ => t,
        }),
        _ => None,
    });
    Some(Class {
        node: id,
        name: name?,
        members,
        super_class,
        abstract_: modifiers.contains(Modifiers::ABSTRACT),
    })
}

fn super_class_name(ast: &Ast, super_class: NodeId) -> Option<&str> {
    let s = unwrap(ast, super_class);
    match *ast.node(s) {
        Node::Ident { .. } => ident_name(ast, s),
        Node::Member { prop, .. } => ident_name(ast, prop),
        _ => None,
    }
}

fn has_valid_super_class(ast: &Ast, class: &Class) -> bool {
    class
        .super_class
        .and_then(|s| super_class_name(ast, s))
        .is_some_and(is_error_class_name)
}

fn is_native_error_base_with_options(ast: &Ast, class: &Class) -> bool {
    let Some(s) = class.super_class else {
        return false;
    };
    let Some(name) = super_class_name(ast, s) else {
        return false;
    };
    let s = unwrap(ast, s);
    BUILTIN_ERRORS.contains(&name)
        && !matches!(name, "AggregateError" | "SuppressedError")
        && match *ast.node(s) {
            Node::Ident { .. } => true,
            Node::Member { obj, .. } => {
                let obj = skip_parens(ast, obj);
                matches!(ast.node(obj), Node::Ident { .. })
                    && ident_name(ast, obj) == Some("globalThis")
            }
            _ => false,
        }
}

/// `this.<name>` (non-computed).
fn is_this_member(ast: &Ast, id: NodeId, name: &str) -> bool {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { obj, prop, .. } => {
            matches!(ast.node(skip_parens(ast, obj)), Node::This {})
                && ident_name(ast, prop) == Some(name)
        }
        _ => false,
    }
}

/// `super(…)` as a statement: the call.
fn super_call(ast: &Ast, stmt: NodeId) -> Option<NodeId> {
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    let Node::Call { callee, .. } = *ast.node(expr) else {
        return None;
    };
    matches!(ast.node(skip_parens(ast, callee)), Node::Super {}).then_some(expr)
}

/// `this.<name> = …` as a statement: the assignment's right side.
fn this_assignment(ast: &Ast, stmt: NodeId, name: &str) -> Option<NodeId> {
    let Node::ExprStmt { expr } = *ast.node(stmt) else {
        return None;
    };
    match *ast.node(expr) {
        Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } if is_this_member(ast, left, name) => Some(right),
        _ => None,
    }
}

fn is_named_key(ast: &Ast, key: NodeId, name: &str) -> bool {
    matches!(ast.node(key), Node::Ident { .. }) && ident_name(ast, key) == Some(name)
}

fn name_property(ast: &Ast, members: &[NodeId]) -> Option<(NodeId, Option<NodeId>)> {
    members.iter().find_map(|&m| match *ast.node(m) {
        Node::PropertyDef {
            modifiers,
            key,
            value,
            ..
        } if !modifiers.intersects(Modifiers::STATIC | Modifiers::ACCESSOR)
            && is_named_key(ast, key, "name") =>
        {
            Some((m, value))
        }
        _ => None,
    })
}

fn is_message_accessor(ast: &Ast, member: NodeId, want: MethodKind) -> bool {
    matches!(*ast.node(member), Node::MethodDef { modifiers, kind, key, .. }
        if kind == want && !modifiers.contains(Modifiers::STATIC) && is_named_key(ast, key, "message"))
}

fn is_valid_name_value(ast: &Ast, value: Option<NodeId>, name: &str) -> bool {
    value.is_some_and(|v| static_string_value(ast, unwrap(ast, v)).as_deref() == Some(name))
}

fn is_missing_or_undefined(ast: &Ast, arg: Option<NodeId>) -> bool {
    arg.is_none_or(|a| {
        let a = unwrap(ast, a);
        matches!(ast.node(a), Node::Ident { .. }) && ident_name(ast, a) == Some("undefined")
    })
}

/// `getParameterIdentifier`: a plain (or defaulted, or parameter-property) identifier
/// parameter's name node.
fn param_identifier(ast: &Ast, param: Option<NodeId>) -> Option<NodeId> {
    let Node::Param { rest, pattern, .. } = *ast.node(param?) else {
        return None;
    };
    (!rest && matches!(ast.node(pattern), Node::Ident { .. })).then_some(pattern)
}

fn is_options_param(ast: &Ast, param: Option<NodeId>) -> bool {
    param_identifier(ast, param).and_then(|i| ident_name(ast, i)) == Some("options")
}

fn is_options_arg(ast: &Ast, arg: Option<NodeId>) -> bool {
    arg.is_some_and(|a| {
        let a = unwrap(ast, a);
        matches!(ast.node(a), Node::Ident { .. }) && ident_name(ast, a) == Some("options")
    })
}

fn options_param_text(ast: &Ast, first: NodeId) -> &'static str {
    match *ast.node(first) {
        Node::Param {
            type_ann: Some(_),
            optional,
            ..
        } => {
            if optional {
                "options?: ErrorOptions"
            } else {
                "options: ErrorOptions"
            }
        }
        _ => "options",
    }
}

fn is_same_identifier(ast: &Ast, arg: Option<NodeId>, ident: NodeId) -> bool {
    arg.is_some_and(|a| {
        let a = unwrap(ast, a);
        matches!(ast.node(a), Node::Ident { .. }) && ident_name(ast, a) == ident_name(ast, ident)
    })
}

fn has_this_or_super(ast: &Ast, root: NodeId) -> bool {
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            if matches!(ast.node(id), Node::This {} | Node::Super {}) {
                self.found = true;
                return Walk::Stop;
            }
            Walk::Continue
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

struct SuperCall {
    call: NodeId,
    callee: NodeId,
    args: Vec<NodeId>,
}

fn super_call_parts(ast: &Ast, call: NodeId) -> SuperCall {
    let (callee, args) = match *ast.node(call) {
        Node::Call { callee, args, .. } => (callee, ast.list(args).to_vec()),
        _ => (call, Vec::new()),
    };
    SuperCall { call, callee, args }
}

/// Position right after `super(`.
fn after_open_paren(cx: &Ctx<'_, '_>, s: &SuperCall) -> u32 {
    let src = cx.src();
    let pos = skip_trivia_forward(src, cx.span(s.callee).end as usize);
    (pos + usize::from(src[pos..].starts_with('('))) as u32
}

fn insert(pos: u32, text: impl Into<String>) -> Fix {
    Fix::replace(Span::new(pos, pos), text)
}

fn comments_inside(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    !cx.ast().comments_in(cx.span(id)).is_empty()
}

fn fix_super_options(cx: &Ctx<'_, '_>, s: &SuperCall, message: &str) -> Option<Fix> {
    let ast = cx.ast();
    if comments_inside(cx, s.call) {
        return None;
    }
    match s.args[..] {
        [] => Some(insert(
            after_open_paren(cx, s),
            format!("{message}, options"),
        )),
        [arg] if !is_parenthesized(cx, arg) => {
            if message != "undefined" && is_missing_or_undefined(ast, Some(arg)) {
                Some(Fix::replace(cx.span(arg), format!("{message}, options")))
            } else {
                Some(insert(cx.span(arg).end, ", options"))
            }
        }
        _ => None,
    }
}

fn fix_super_message(cx: &Ctx<'_, '_>, s: &SuperCall, message: &str) -> Option<Fix> {
    let ast = cx.ast();
    if comments_inside(cx, s.call) {
        return None;
    }
    let Some(&first) = s.args.first() else {
        return Some(insert(
            after_open_paren(cx, s),
            format!("{message}, options"),
        ));
    };
    if !is_missing_or_undefined(ast, Some(first)) || is_parenthesized(cx, first) {
        return None;
    }
    let text = if s.args.len() == 1 {
        format!("{message}, options")
    } else {
        message.to_owned()
    };
    Some(Fix::replace(cx.span(first), text))
}

fn property_name_is_cause(ast: &Ast, prop: NodeId) -> Option<NodeId> {
    match *ast.node(prop) {
        Node::Property { key, value, .. } => {
            let named = match ast.node(key) {
                Node::Ident { .. } => ident_name(ast, key) == Some("cause"),
                Node::StringLit { value } => ast.atom(*value) == "cause",
                _ => false,
            };
            named.then_some(value)
        }
        Node::ShorthandProp {
            name, init: None, ..
        } => (ident_name(ast, name) == Some("cause")).then_some(name),
        _ => None,
    }
}

/// `super(…, {cause})` already forwards the error options.
fn has_inline_options(cx: &Ctx<'_, '_>, s: &SuperCall, options_param: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let [message, options] = s.args[..] else {
        return false;
    };
    let Node::ObjectLit { props } = *ast.node(unwrap(ast, options)) else {
        return false;
    };
    let [prop] = ast.list(props) else {
        return false;
    };
    let Some(cause) = property_name_is_cause(ast, *prop) else {
        return false;
    };
    if let Some(param) = options_param {
        let Some(param_name) = param_identifier(ast, Some(param)).and_then(|i| ident_name(ast, i))
        else {
            return false;
        };
        return match *ast.node(unwrap(ast, cause)) {
            Node::Member { obj, prop, .. } => {
                let obj = unwrap(ast, obj);
                ident_name(ast, prop) == Some("cause")
                    && matches!(ast.node(obj), Node::Ident { .. })
                    && ident_name(ast, obj) == Some(param_name)
            }
            _ => false,
        };
    }
    static_string_value(ast, unwrap(ast, message)).is_some()
}

struct Constructor {
    key: NodeId,
    params: Vec<NodeId>,
    body: Option<NodeId>,
}

fn constructor_of(ast: &Ast, members: &[NodeId]) -> Option<Constructor> {
    let ctor = |m: NodeId| -> Option<Constructor> {
        let Node::MethodDef {
            kind: MethodKind::Constructor,
            key,
            value,
            ..
        } = *ast.node(m)
        else {
            return None;
        };
        let Node::FunctionExpr { params, body, .. } = *ast.node(value) else {
            return None;
        };
        Some(Constructor {
            key,
            params: ast.list(params).to_vec(),
            body,
        })
    };
    let all: Vec<Constructor> = members.iter().filter_map(|&m| ctor(m)).collect();
    let index = all.iter().position(|c| c.body.is_some()).unwrap_or(0);
    all.into_iter().nth(index)
}

const PASS_MESSAGE: &str = "Pass the error message to `super()` as the first argument.";
const PASS_OPTIONS: &str = "Pass `options` to `super()` as the second argument.";

fn error_options_problem(
    cx: &mut Ctx<'_, '_>,
    ctor: &Constructor,
    s: &SuperCall,
    has_message_accessor: bool,
) {
    let ast = cx.ast();
    let params = &ctor.params;
    let first = params.first().copied();
    let Some(first_ident) = param_identifier(ast, first) else {
        return;
    };
    let Some(first) = first else {
        return;
    };
    let options_param = params
        .iter()
        .copied()
        .find(|&p| is_options_param(ast, Some(p)));
    let call_span = cx.span(s.call);
    if options_param == Some(first) {
        if !is_options_arg(ast, s.args.get(1).copied()) {
            let fix = (!is_same_identifier(ast, s.args.first().copied(), first_ident))
                .then(|| fix_super_options(cx, s, "undefined"))
                .flatten();
            cx.report(call_span, PASS_OPTIONS)
                .id("passOptionsToSuper")
                .fix_opt(fix);
        }
        return;
    }
    let inline = has_inline_options(cx, s, options_param);
    if inline && options_param.is_none() {
        return;
    }
    let pass_message = !has_message_accessor && ident_name(ast, first_ident) == Some("message");
    let message = if pass_message { "message" } else { "undefined" };
    if params.len() < 2 {
        let fix = if comment_after(cx, cx.span(first).end) {
            None
        } else {
            fix_super_options(cx, s, message).map(|f| {
                join_fixes(
                    insert(
                        cx.span(first).end,
                        format!(", {}", options_param_text(ast, first)),
                    ),
                    f,
                )
            })
        };
        cx.report(
            cx.span(first),
            "Error constructors should accept an `options` parameter.",
        )
        .id("missingOptionsParameter")
        .fix_opt(fix);
        return;
    }
    let Some(options_param) = options_param else {
        cx.report(
            cx.span(ctor.key),
            "Error constructors should use a non-rest parameter named `options`.",
        )
        .id("invalidOptionsParameter");
        return;
    };
    if pass_message && !is_same_identifier(ast, s.args.first().copied(), first_ident) {
        let fix = fix_super_message(cx, s, message);
        cx.report(call_span, PASS_MESSAGE)
            .id("passMessageToSuper")
            .fix_opt(fix);
        return;
    }
    if inline || is_options_arg(ast, s.args.get(1).copied()) {
        return;
    }
    let options_ident = param_identifier(ast, Some(options_param));
    let fix = options_ident
        .is_none_or(|i| !is_same_identifier(ast, s.args.first().copied(), i))
        .then(|| fix_super_options(cx, s, message))
        .flatten();
    cx.report(call_span, PASS_OPTIONS)
        .id("passOptionsToSuper")
        .fix_opt(fix);
}

fn invalid_name(cx: &mut Ctx<'_, '_>, at: NodeId, name: &str) {
    let span = cx.span(skip_parens(cx.ast(), at));
    cx.report(
        span,
        format!("The `name` property should be set to `{name}`."),
    );
}

/// `fix` of the "pass the message to `super()`" problem.
fn message_fix(
    cx: &Ctx<'_, '_>,
    ctor: &Constructor,
    stmts: &[NodeId],
    super_index: usize,
    message_index: usize,
    s: &SuperCall,
) -> Option<Fix> {
    let ast = cx.ast();
    if message_index < super_index {
        return None;
    }
    let statement = stmts[message_index];
    let rhs = this_assignment(ast, statement, "message")?;
    let first = ctor.params.first().copied();
    let second = ctor.params.get(1).copied();
    let add_param = ctor.params.len() == 1
        && param_identifier(ast, first).is_some()
        && !is_options_param(ast, first);
    let start = cx.span(stmts[message_index - 1]).end;
    let end = cx.span(statement).end;
    if add_param && first.is_some_and(|f| comment_after(cx, cx.span(f).end)) {
        return None;
    }
    if !ast.comments_in(Span::new(start, end)).is_empty() || comment_after(cx, end) {
        return None;
    }
    if comments_inside(cx, s.call) {
        return None;
    }
    let add_arg = add_param || is_options_param(ast, second);
    let mut fix: Option<Fix> = None;
    let mut push = |f: Fix| {
        fix = Some(match fix.take() {
            Some(prev) => join_fixes(prev, f),
            None => f,
        });
    };
    match s.args.first() {
        None => {
            if message_index != super_index + 1 || has_this_or_super(ast, rhs) {
                return None;
            }
            let text = paren_text(cx, rhs);
            let text = if add_arg {
                format!("{text}, options")
            } else {
                text.to_owned()
            };
            push(insert(after_open_paren(cx, s), text));
        }
        Some(&arg) => {
            if cx.text(unwrap(ast, arg)) != cx.text(unwrap(ast, rhs)) {
                return None;
            }
            if add_arg && s.args.len() == 1 {
                if is_parenthesized(cx, arg) {
                    return None;
                }
                push(insert(cx.span(arg).end, ", options"));
            }
        }
    }
    if add_param && let Some(f) = first {
        push(insert(
            cx.span(f).end,
            format!(", {}", options_param_text(ast, f)),
        ));
    }
    let src = cx.src();
    let next = &src[skip_trivia_forward(src, end as usize)..];
    let semicolon = if needs_semicolon(cx, start, next) {
        ";"
    } else {
        ""
    };
    push(Fix::replace(Span::new(start, end), semicolon));
    fix
}

fn constructor_problems(cx: &mut Ctx<'_, '_>, ctor: &Constructor, info: &Info) {
    let ast = cx.ast();
    let Some(body) = ctor.body else {
        return;
    };
    let Node::Block { stmts } = *ast.node(body) else {
        return;
    };
    let stmts = ast.list(stmts);
    let super_index = stmts.iter().position(|&s| super_call(ast, s).is_some());
    let message_index = stmts
        .iter()
        .position(|&s| this_assignment(ast, s, "message").is_some());
    let has_accessor = info.getter || info.setter;
    let mut body_problem = false;
    let super_parts = super_index
        .and_then(|i| super_call(ast, stmts[i]))
        .map(|c| super_call_parts(ast, c));
    match (super_index, &super_parts) {
        (Some(si), Some(s)) => {
            if has_accessor && !is_missing_or_undefined(ast, s.args.first().copied()) {
                body_problem = true;
                cx.report(
                    cx.span(stmts[si]),
                    "Do not pass the error message to `super()` when the class defines a `message` accessor.",
                )
                .id("doNotPassMessageToSuper");
            } else if let Some(mi) = message_index
                && info.getter
                && !info.setter
            {
                body_problem = true;
                cx.report(
                    cx.span(stmts[mi]),
                    "Do not assign to `this.message` when the class defines a `message` getter without a setter.",
                )
                .id("doNotAssignMessageWithoutSetter");
            } else if let Some(mi) = message_index
                && !info.setter
            {
                body_problem = true;
                let fix = message_fix(cx, ctor, stmts, si, mi, s);
                cx.report(
                    cx.span(stmts[si]),
                    "Pass the error message to `super()` instead of setting `this.message`.",
                )
                .fix_opt(fix);
            }
        }
        _ => {
            body_problem = true;
            cx.report(cx.span(body), "Missing call to `super()` in constructor.");
        }
    }
    let name = info.name.as_str();
    let name_assignment = stmts.iter().find_map(|&s| this_assignment(ast, s, "name"));
    let valid_name = match name_assignment {
        None => {
            if is_valid_name_value(ast, info.name_value, name) {
                true
            } else {
                invalid_name(cx, info.name_value.unwrap_or(body), name);
                false
            }
        }
        Some(right) => {
            if static_string_value(ast, unwrap(ast, right)).as_deref() == Some(name) {
                true
            } else {
                invalid_name(cx, right, name);
                false
            }
        }
    };
    if !valid_name || !info.check_options || body_problem {
        return;
    }
    if let Some(s) = &super_parts {
        error_options_problem(cx, ctor, s, has_accessor);
    }
}

struct Info {
    name: String,
    name_value: Option<NodeId>,
    getter: bool,
    setter: bool,
    check_options: bool,
}

/// The text of a `name = 'X';` field inserted right after the class body's `{`.
fn name_property_text(cx: &Ctx<'_, '_>, class: &Class, open: u32, name: &str) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let lb = linebreak(src);
    let class_indent = line_indent(src, cx.span(class.node).start as usize);
    let indent = match ast.list(class.members).first() {
        Some(&m) => line_indent(src, cx.span(m).start as usize).to_owned(),
        None => format!("{class_indent}{}", indent_unit(cx)),
    };
    let own_break = src[open as usize + 1..].starts_with(lb);
    format!(
        "{lb}{indent}name = '{name}';{}",
        if own_break {
            String::new()
        } else {
            format!("{lb}{class_indent}")
        }
    )
}

/// The class body's `{`.
fn body_open(cx: &Ctx<'_, '_>, class: &Class) -> Option<u32> {
    let ast = cx.ast();
    let src = cx.src();
    let header_end = match *ast.node(class.node) {
        Node::ClassDecl {
            name,
            type_params,
            heritage,
            ..
        }
        | Node::ClassExpr {
            name,
            type_params,
            heritage,
            ..
        } => ast
            .list(heritage)
            .iter()
            .map(|&h| cx.span(h).end)
            .chain(type_params.map(|t| cx.span(t).end))
            .chain(name.map(|n| cx.span(n).end))
            .max()?,
        _ => return None,
    };
    let pos = skip_trivia_forward(src, header_end as usize);
    src[pos..].starts_with('{').then_some(pos as u32)
}

/// The class node's range as ESTree has it: from `class` (or `abstract`), after `export`
/// and `default`.
fn class_span(cx: &Ctx<'_, '_>, class: &Class) -> Span {
    let src = cx.src();
    let span = cx.span(class.node);
    let mut pos = span.start as usize;
    for _ in 0..8 {
        let p = skip_trivia_forward(src, pos);
        let rest = &src[p..];
        let word = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
            .map_or(rest, |i| &rest[..i]);
        if matches!(word, "export" | "default" | "declare") {
            pos = p + word.len();
        } else {
            pos = p;
            break;
        }
    }
    Span::new((pos as u32).min(span.end), span.end)
}

fn check_class(cx: &mut Ctx<'_, '_>, id: NodeId) {
    let ast = cx.ast();
    let Some(class) = class_of(ast, id) else {
        return;
    };
    if class.abstract_ || !has_valid_super_class(ast, &class) {
        return;
    }
    let Some(name) = ident_name(ast, class.name) else {
        return;
    };
    let name = name.to_owned();
    let expected = class_name_for(&name);
    if name != expected {
        let span = cx.span(class.name);
        cx.report(span, format!("Invalid class name, use `{expected}`."));
    }
    let members = ast.list(class.members);
    let name_prop = name_property(ast, members);
    let name_value = name_prop.and_then(|(_, v)| v).map(|v| skip_parens(ast, v));
    let Some(ctor) = constructor_of(ast, members) else {
        if is_valid_name_value(ast, name_value, &name) {
            return;
        }
        let at = name_value.unwrap_or(class.node);
        let span = if at == class.node {
            class_span(cx, &class)
        } else {
            cx.span(at)
        };
        let quoted = format!("'{name}'");
        let fix = match name_prop {
            Some((_, Some(value))) => {
                let value = skip_parens(ast, value);
                let inner = unwrap(ast, value);
                if static_string_value(ast, inner).is_some() {
                    Some(Fix::replace(cx.span(inner), quoted))
                } else if comments_inside(cx, value) {
                    None
                } else {
                    Some(Fix::replace(cx.span(value), quoted))
                }
            }
            Some((prop, None)) => body_open(cx, &class).map(|open| {
                Fix::replace(
                    cx.span(prop),
                    name_property_text(cx, &class, open, &name)
                        .trim()
                        .to_owned(),
                )
            }),
            None => body_open(cx, &class)
                .map(|open| insert(open + 1, name_property_text(cx, &class, open, &name))),
        };
        cx.report(
            span,
            format!("The `name` property should be set to `{name}`."),
        )
        .fix_opt(fix);
        return;
    };
    let info = Info {
        getter: members
            .iter()
            .any(|&m| is_message_accessor(ast, m, MethodKind::Get)),
        setter: members
            .iter()
            .any(|&m| is_message_accessor(ast, m, MethodKind::Set)),
        check_options: name == expected && is_native_error_base_with_options(ast, &class),
        name,
        name_value,
    };
    constructor_problems(cx, &ctor, &info);
}

fn check_export(cx: &mut Ctx<'_, '_>, left: NodeId, right: NodeId) {
    let ast = cx.ast();
    let Some(class) = class_of(ast, skip_parens(ast, right)) else {
        return;
    };
    if !matches!(ast.node(skip_parens(ast, right)), Node::ClassExpr { .. })
        || !has_valid_super_class(ast, &class)
    {
        return;
    }
    let Some(error_name) = ident_name(ast, class.name) else {
        return;
    };
    let (property, replace, text, exported) = match *ast.node(left) {
        Node::Member { prop, .. } => (
            prop,
            prop,
            error_name.to_owned(),
            ident_name(ast, prop).map(str::to_owned),
        ),
        Node::Index { index, .. } => {
            let inner = unwrap(ast, index);
            (
                skip_parens(ast, index),
                inner,
                format!("'{error_name}'"),
                static_string_value(ast, inner),
            )
        }
        _ => return,
    };
    let Some(exported) = exported else {
        return;
    };
    if exported == error_name {
        return;
    }
    let fix = Fix::replace(cx.span(replace), text);
    let span = cx.span(property);
    cx.report(span, "Exported error name should match error class")
        .id("invalidExport")
        .fix(fix);
}

impl Rule for CustomErrorDefinition {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::ClassDecl { .. } => check_class(cx, node),
            Node::Assign { left, right, .. } => {
                let right_inner = skip_parens(ast, right);
                if matches!(ast.node(right_inner), Node::ClassExpr { .. }) {
                    check_class(cx, right_inner);
                }
                let exports_object = match *ast.node(left) {
                    Node::Member { obj, .. } | Node::Index { obj, .. } => {
                        matches!(ast.node(obj), Node::Ident { .. })
                            && ident_name(ast, obj) == Some("exports")
                    }
                    _ => false,
                };
                if exports_object {
                    check_export(cx, left, right);
                }
            }
            _ => {}
        }
    }
}
