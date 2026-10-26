//! `unicorn/no-immediate-mutation`: a variable declared (as the last declarator) or assigned
//! with an array literal, object literal, or `new Set/WeakSet/Map/WeakMap` (no iterable or
//! an array literal) and changed by the very next statement: `x.push(..)`/`x.unshift(..)`,
//! `x.key = v` / `x[key] = v`, `Object.assign(x, source, ..)`, `x.add(v)`, `x.set(k, v)`.
//! The mutation's inputs must not mention the variable. The fix moves the mutation into the
//! literal and deletes the statement; it is only a suggestion when the moved inputs may have
//! side effects, and there is none when a comment would be lost, an accessor or `__proto__`
//! is involved, or an `unshift` would move side-effecting elements after its arguments.
//!
//! With `checkConditionals`, a next statement `if (c) <one mutation> [else <one mutation>]`,
//! `c && <mutation>` or `c ? <mutation> : <mutation>` is reported as a whole and merged as a
//! conditional spread (`...((c) ? [..] : [])`); never fixed in TypeScript or when the
//! statement has comments in, before or after it on its line.
//!
//! Separately, `let`/`var` declarations with only plain identifiers and no initializers,
//! followed by `x = value` or `[a, b] = value` assigning only declared names, are reported
//! at the assignment ("initialization") and merged into a declaration of those names.

use super::prelude::*;
use super::unicorn_ast::{call_tokens, method_call, no_spread_in, remove_statements};
use super::unicorn_prefer_spread::is_ts_file;
use super::unicorn_static::{has_side_effect, static_value};
use super::unicorn_text::{is_parenthesized, paren_span, paren_text, would_remove_comments};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-immediate-mutation",
        "Disallow immediate mutation after assignment and initialization immediately after declaration.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<NoImmediateMutation>,
};

#[derive(Debug, Default)]
struct NoImmediateMutation {
    check_conditionals: bool,
}

impl FromOptions for NoImmediateMutation {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoImmediateMutation {
            check_conditionals: bool_field(obj_opt(opts, 0), "checkConditionals", false),
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Case {
    Array,
    ObjectProperty,
    ObjectAssign,
    Set,
    Map,
}

/// The assigned variable and where its value and next statement are.
struct Target<'a> {
    name: &'a str,
    sym: SymbolId,
    value: NodeId,
    next: NodeId,
    assign_type: &'static str,
}

/// One branch of a conditional mutation.
struct Branch {
    text: String,
    inputs: Vec<NodeId>,
    can_fix: bool,
    method: Option<String>,
}

fn next_statement(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<NodeId> {
    let list = statement_list(cx.ast(), cx.parent(stmt)?)?;
    let i = list.iter().position(|&s| s == stmt)?;
    list.get(i + 1).copied()
}

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

/// `new Set()`, `new Set`, `new Set([..])` (at most one argument, an array literal).
fn is_new_with_optional_array(ast: &Ast, id: NodeId, names: &[&str]) -> bool {
    let Node::New { callee, args, .. } = *ast.node(id) else {
        return false;
    };
    let args = ast.list(args);
    let callee = skip_parens(ast, callee);
    matches!(ast.node(callee), Node::Ident { .. })
        && ident_name(ast, callee).is_some_and(|n| names.contains(&n))
        && args.len() <= 1
        && args
            .first()
            .is_none_or(|&a| matches!(ast.node(skip_parens(ast, a)), Node::ArrayLit { .. }))
}

fn cases_for(ast: &Ast, value: NodeId) -> &'static [Case] {
    match ast.node(value) {
        Node::ArrayLit { .. } => &[Case::Array],
        Node::ObjectLit { .. } => &[Case::ObjectProperty, Case::ObjectAssign],
        _ if is_new_with_optional_array(ast, value, &["Set", "WeakSet"]) => &[Case::Set],
        _ if is_new_with_optional_array(ast, value, &["Map", "WeakMap"]) => &[Case::Map],
        _ => &[],
    }
}

/// An identifier of `sym` (a declaration or a reference) lies inside one of `nodes`.
fn mentions(cx: &Ctx<'_, '_>, sym: SymbolId, nodes: &[NodeId]) -> bool {
    let sem = cx.semantic();
    let symbol = sem.symbol(sym);
    let spans: Vec<Span> = nodes.iter().map(|&n| cx.span(n)).collect();
    symbol
        .decls
        .iter()
        .map(|d| d.name)
        .chain(symbol.refs.iter().map(|&r| sem.reference(r).node))
        .any(|i| {
            let s = cx.span(i);
            spans.iter().any(|r| s.start >= r.start && s.end <= r.end)
        })
}

fn list_of(ast: &Ast, id: NodeId) -> &[NodeId] {
    match *ast.node(id) {
        Node::ArrayLit { elements } => ast.list(elements),
        Node::ObjectLit { props } => ast.list(props),
        Node::Call { args, .. } | Node::New { args, .. } => ast.list(args),
        _ => &[],
    }
}

/// The start of the token before the closing bracket of `id` is a comma.
fn ends_with_comma(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let close = cx.span(id).end.saturating_sub(1) as usize;
    let p = skip_trivia_backward(cx.ast(), cx.src(), close);
    p > 0 && cx.src().as_bytes()[p - 1] == b','
}

/// Appends `text` as the last element/property of the array or object literal `id`.
fn append_to_list(cx: &Ctx<'_, '_>, id: NodeId, text: &str) -> (Span, String) {
    let close = cx.span(id).end.saturating_sub(1);
    let comma = !list_of(cx.ast(), id).is_empty() && !ends_with_comma(cx, id);
    (
        Span::at(close),
        format!("{} {text}", if comma { "," } else { "" }),
    )
}

/// The arguments' text range of a call: after `(` up to the trailing comma or `)`.
fn arguments_range(cx: &Ctx<'_, '_>, call: NodeId) -> Option<Span> {
    let (open, close, comma) = call_tokens(cx, call)?;
    Some(Span::new(open + 1, comma.unwrap_or(close)))
}

fn arguments_text(cx: &Ctx<'_, '_>, call: NodeId) -> Option<String> {
    Some(arguments_range(cx, call)?.text(cx.src()).to_owned())
}

/// Deletes the mutation statement, keeping its `;` when the token before isn't one.
fn remove_after_assign(cx: &Ctx<'_, '_>, stmt: NodeId) -> Fix {
    let p = skip_trivia_backward(cx.ast(), cx.src(), cx.span(stmt).start as usize);
    let preserve = p == 0 || cx.src().as_bytes()[p - 1] != b';';
    remove_statements(cx, stmt, stmt, preserve)
}

fn with(mut fix: Fix, edits: impl IntoIterator<Item = (Span, String)>) -> Fix {
    for (span, text) in edits {
        fix = fix.and(span, text);
    }
    fix
}

/// Adds `elements` to the constructor's iterable (creating `[..]` when there is none) and
/// deletes the mutation statement.
fn append_to_constructor(
    cx: &Ctx<'_, '_>,
    new: NodeId,
    elements: &str,
    stmt: NodeId,
) -> Option<Fix> {
    let ast = cx.ast();
    let Node::New { has_args, args, .. } = *ast.node(new) else {
        return None;
    };
    let edit = if has_args {
        match ast.list(args).first() {
            Some(&iterable) => append_to_list(cx, skip_parens(ast, iterable), elements),
            None => {
                let (open, _, _) = call_tokens(cx, new)?;
                (Span::at(open + 1), format!("[{elements}]"))
            }
        }
    } else {
        (Span::at(cx.span(new).end), format!("([{elements}])"))
    };
    Some(with(remove_after_assign(cx, stmt), [edit]))
}

/// `getPropertyName` of a member target or an object literal member.
fn property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    let key = match *ast.node(id) {
        Node::Member { prop, .. } => return ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => {
            return static_value(cx, index).and_then(|v| v.to_js_string());
        }
        Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
        Node::ShorthandProp { name, .. } => return ident_name(ast, name).map(str::to_owned),
        _ => return None,
    };
    match *ast.node(key) {
        Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::ComputedKey { expr } => static_value(cx, expr).and_then(|v| v.to_js_string()),
        _ => static_value(cx, key).and_then(|v| v.to_js_string()),
    }
}

fn is_proto_key(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    property_name(cx, id).as_deref() == Some("__proto__")
}

fn object_props(ast: &Ast, obj: NodeId) -> &[NodeId] {
    match *ast.node(obj) {
        Node::ObjectLit { props } => ast.list(props),
        _ => &[],
    }
}

fn has_proto_property(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
    object_props(cx.ast(), obj).iter().any(|&p| {
        matches!(
            cx.node(p),
            Node::Property { .. } | Node::MethodDef { .. } | Node::ShorthandProp { .. }
        ) && is_proto_key(cx, p)
    })
}

fn has_accessor(ast: &Ast, obj: NodeId) -> bool {
    object_props(ast, obj).iter().any(|&p| {
        matches!(
            ast.node(p),
            Node::MethodDef {
                kind: MethodKind::Get | MethodKind::Set,
                ..
            }
        )
    })
}

/// The text between an object literal's braces, without a trailing comma.
fn object_properties_range(cx: &Ctx<'_, '_>, obj: NodeId) -> Span {
    let span = cx.span(obj);
    let close = span.end.saturating_sub(1);
    let p = skip_trivia_backward(cx.ast(), cx.src(), close as usize) as u32;
    let end = if p > span.start + 1 && cx.src().as_bytes()[p as usize - 1] == b',' {
        p - 1
    } else {
        close
    };
    Span::new(span.start + 1, end.max(span.start + 1))
}

impl NoImmediateMutation {
    /// The mutation `expr` (parentheses skipped) of `case` on the target, if it is one.
    fn mutation(&self, cx: &Ctx<'_, '_>, t: &Target<'_>, case: Case, expr: NodeId) -> bool {
        let ast = cx.ast();
        match case {
            Case::Array | Case::Set | Case::Map => {
                let Some(call) = method_call(ast, expr) else {
                    return false;
                };
                let ok = match case {
                    Case::Array => matches!(call.name, "push" | "unshift") && !call.args.is_empty(),
                    Case::Set => {
                        call.name == "add"
                            && call.args.len() == 1
                            && no_spread_in(ast, call.args, 1)
                    }
                    _ => {
                        call.name == "set"
                            && call.args.len() == 2
                            && no_spread_in(ast, call.args, 2)
                    }
                };
                ok && call.plain()
                    && is_named(ast, call.obj, t.name)
                    && !mentions(cx, t.sym, call.args)
            }
            Case::ObjectProperty => {
                let Node::Assign {
                    left,
                    op: AssignOp::Assign,
                    right,
                } = *ast.node(expr)
                else {
                    return false;
                };
                match *ast.node(skip_parens(ast, left)) {
                    Node::Member {
                        obj,
                        optional: false,
                        prop,
                    } => {
                        matches!(ast.node(prop), Node::Ident { .. })
                            && is_named(ast, obj, t.name)
                            && !mentions(cx, t.sym, &[right])
                    }
                    Node::Index {
                        obj,
                        optional: false,
                        index,
                    } => is_named(ast, obj, t.name) && !mentions(cx, t.sym, &[index, right]),
                    _ => false,
                }
            }
            Case::ObjectAssign => {
                let Some(call) = method_call(ast, expr) else {
                    return false;
                };
                call.name == "assign"
                    && call.plain()
                    && is_named(ast, call.obj, "Object")
                    && call.args.len() >= 2
                    && matches!(ast.node(skip_parens(ast, call.args[0])), Node::Ident { .. })
                    && is_named(ast, call.args[0], t.name)
                    && !matches!(ast.node(call.args[1]), Node::Spread { .. })
                    && !mentions(cx, t.sym, &call.args[1..2])
            }
        }
    }

    fn object_type(cx: &Ctx<'_, '_>, value: NodeId) -> String {
        let ast = cx.ast();
        match *ast.node(value) {
            Node::ArrayLit { .. } => "array".to_owned(),
            Node::ObjectLit { .. } => "object".to_owned(),
            Node::New { callee, .. } => {
                format!(
                    "`{}`",
                    ident_name(ast, skip_parens(ast, callee)).unwrap_or("")
                )
            }
            _ => String::new(),
        }
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, t: &Target<'_>) {
        let ast = cx.ast();
        for &case in cases_for(ast, t.value) {
            match *ast.node(t.next) {
                Node::If { test, cons, alt } => {
                    let Some(cons) = only_expression(ast, Some(cons)) else {
                        continue;
                    };
                    let alt_expr = only_expression(ast, alt);
                    if alt.is_some() && alt_expr.is_none() {
                        continue;
                    }
                    if self.check_conditionals {
                        self.conditional(cx, t, case, test, cons, alt_expr);
                    }
                }
                Node::ExprStmt { expr } => {
                    let expr = skip_parens(ast, expr);
                    match *ast.node(expr) {
                        Node::Conditional { test, cons, alt } => {
                            if self.check_conditionals {
                                self.conditional(cx, t, case, test, cons, Some(alt));
                            }
                        }
                        Node::Binary {
                            left,
                            op: BinaryOp::LogicalAnd,
                            right,
                        } => {
                            if self.check_conditionals {
                                self.conditional(cx, t, case, left, right, None);
                            }
                        }
                        _ => {
                            if self.mutation(cx, t, case, expr) {
                                self.report(cx, t, case, expr);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn report(&self, cx: &mut Ctx<'_, '_>, t: &Target<'_>, case: Case, expr: NodeId) {
        let ast = cx.ast();
        let message = format!(
            "Immediate mutation on {} is not allowed.",
            Self::object_type(cx, t.value)
        );
        let stmt = t.next;
        let at = t.assign_type;
        let (span, outcome) = match case {
            Case::Array => {
                let Some(call) = method_call(ast, expr) else {
                    return;
                };
                let callee = cx.parent(call.prop).unwrap_or(expr);
                let prepend = call.name == "unshift";
                let outcome = (|| {
                    let elements = list_of(ast, t.value);
                    if (prepend
                        && elements.iter().any(|&e| {
                            !matches!(ast.node(e), Node::Hole {}) && has_side_effect(ast, e)
                        }))
                        || would_remove_comments(cx, cx.span(stmt), &[arguments_range(cx, expr)?])
                    {
                        return None;
                    }
                    let text = arguments_text(cx, expr)?;
                    let edit = if prepend {
                        (Span::at(cx.span(t.value).start + 1), format!("{text}, "))
                    } else {
                        append_to_list(cx, t.value, &text)
                    };
                    let fix = with(remove_after_assign(cx, stmt), [edit]);
                    let suggest = call.args.iter().any(|&a| has_side_effect(ast, a));
                    let desc = format!(
                        "{} the elements to the {at}.",
                        if prepend { "Prepend" } else { "Append" }
                    );
                    Some((suggest, "suggestion/array", desc, fix))
                })();
                (cx.span(callee), outcome)
            }
            Case::ObjectProperty => {
                let Node::Assign { left, right, .. } = *ast.node(expr) else {
                    return;
                };
                let member = skip_parens(ast, left);
                let (property, computed) = match *ast.node(member) {
                    Node::Member { prop, .. } => (prop, false),
                    Node::Index { index, .. } => (index, true),
                    _ => return,
                };
                let eq = skip_trivia_forward(cx.src(), paren_span(cx, left).end as usize) as u32;
                let span = Span::new(cx.span(expr).start, eq + 1);
                let outcome = (|| {
                    if would_remove_comments(
                        cx,
                        cx.span(stmt),
                        &[paren_span(cx, property), paren_span(cx, right)],
                    ) || is_proto_key(cx, member)
                        || has_accessor(ast, t.value)
                    {
                        return None;
                    }
                    let mut key = paren_text(cx, property).to_owned();
                    if computed {
                        key = format!("[{key}]");
                    }
                    let text = format!("{key}: {},", paren_text(cx, right));
                    let fix = with(
                        remove_after_assign(cx, stmt),
                        [append_to_list(cx, t.value, &text)],
                    );
                    let suggest =
                        (computed && has_side_effect(ast, property)) || has_side_effect(ast, right);
                    Some((
                        suggest,
                        "suggestion/object",
                        format!("Move this property to the {at}."),
                        fix,
                    ))
                })();
                (span, outcome)
            }
            Case::ObjectAssign => {
                let Some(call) = method_call(ast, expr) else {
                    return;
                };
                let callee = cx.parent(call.prop).unwrap_or(expr);
                let source = call.args[1];
                let source_obj = skip_parens(ast, source);
                let is_object = matches!(ast.node(source_obj), Node::ObjectLit { .. });
                let outcome = (|| {
                    if has_accessor(ast, t.value)
                        || (is_object
                            && (has_accessor(ast, source_obj)
                                || has_proto_property(cx, source_obj)))
                    {
                        return None;
                    }
                    let copied = if is_object {
                        object_properties_range(cx, source_obj)
                    } else {
                        paren_span(cx, source)
                    };
                    if would_remove_comments(cx, cx.span(stmt), &[copied]) {
                        return None;
                    }
                    let text = if is_object {
                        copied.text(cx.src()).to_owned()
                    } else {
                        format!("...{}", paren_text(cx, source))
                    };
                    let mut edits = Vec::new();
                    if !text.trim().is_empty() {
                        edits.push(append_to_list(cx, t.value, &text));
                    }
                    let fix = if call.args.len() != 2 {
                        let arg = paren_span(cx, source);
                        let p = skip_trivia_backward(ast, cx.src(), arg.start as usize);
                        if p == 0 || cx.src().as_bytes()[p - 1] != b',' {
                            return None;
                        }
                        with(Fix::replace(Span::new(p as u32 - 1, arg.end), ""), edits)
                    } else {
                        with(remove_after_assign(cx, stmt), edits)
                    };
                    let desc = if is_object {
                        "Move properties to"
                    } else {
                        "Spread properties in"
                    };
                    Some((
                        has_side_effect(ast, source),
                        "suggestion/object-assign",
                        format!("{desc} the {at}."),
                        fix,
                    ))
                })();
                (cx.span(callee), outcome)
            }
            Case::Set | Case::Map => {
                let Some(call) = method_call(ast, expr) else {
                    return;
                };
                let callee = cx.parent(call.prop).unwrap_or(expr);
                let outcome = (|| {
                    if would_remove_comments(cx, cx.span(stmt), &[arguments_range(cx, expr)?]) {
                        return None;
                    }
                    let text = arguments_text(cx, expr)?;
                    let (elements, id, desc) = if case == Case::Map {
                        (format!("[{text}]"), "suggestion/map", "Move the entry to")
                    } else {
                        (text, "suggestion/set", "Move the element to")
                    };
                    let fix = append_to_constructor(cx, t.value, &elements, stmt)?;
                    let suggest = call.args.iter().any(|&a| has_side_effect(ast, a));
                    Some((suggest, id, format!("{desc} the {at}."), fix))
                })();
                (cx.span(callee), outcome)
            }
        };
        let r = cx.report(span, message).id("error");
        if let Some((suggest, id, desc, fix)) = outcome {
            if suggest {
                r.suggest(id, desc, fix);
            } else {
                r.fix(fix);
            }
        }
    }

    fn branch(&self, cx: &Ctx<'_, '_>, t: &Target<'_>, case: Case, node: NodeId) -> Option<Branch> {
        let ast = cx.ast();
        let node = skip_parens(ast, node);
        if !self.mutation(cx, t, case, node) {
            return None;
        }
        match case {
            Case::ObjectProperty => {
                let Node::Assign { left, right, .. } = *ast.node(node) else {
                    return None;
                };
                let member = skip_parens(ast, left);
                let (property, computed) = match *ast.node(member) {
                    Node::Member { prop, .. } => (prop, false),
                    Node::Index { index, .. } => (index, true),
                    _ => return None,
                };
                let key = paren_text(cx, property);
                let key = if computed {
                    format!("[{key}]")
                } else {
                    key.to_owned()
                };
                Some(Branch {
                    text: format!("{{{key}: {}}}", paren_text(cx, right)),
                    inputs: if computed {
                        vec![property, right]
                    } else {
                        vec![right]
                    },
                    can_fix: !is_proto_key(cx, member) && !has_accessor(ast, t.value),
                    method: None,
                })
            }
            Case::ObjectAssign => {
                let call = method_call(ast, node)?;
                if call.args.len() != 2 {
                    return None;
                }
                let source = call.args[1];
                let source_obj = skip_parens(ast, source);
                let can_fix = !has_accessor(ast, t.value)
                    && (!matches!(ast.node(source_obj), Node::ObjectLit { .. })
                        || !has_proto_property(cx, source_obj));
                Some(Branch {
                    text: paren_text(cx, source).to_owned(),
                    inputs: vec![source],
                    can_fix,
                    method: None,
                })
            }
            _ => {
                let call = method_call(ast, node)?;
                let text = arguments_text(cx, node)?;
                Some(Branch {
                    text: if case == Case::Map {
                        format!("[[{text}]]")
                    } else {
                        format!("[{text}]")
                    },
                    inputs: call.args.to_vec(),
                    can_fix: true,
                    method: Some(call.name.to_owned()),
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn conditional(
        &self,
        cx: &mut Ctx<'_, '_>,
        t: &Target<'_>,
        case: Case,
        test: NodeId,
        cons: NodeId,
        alt: Option<NodeId>,
    ) {
        let ast = cx.ast();
        if mentions(cx, t.sym, &[test]) {
            return;
        }
        let Some(consequent) = self.branch(cx, t, case, cons) else {
            return;
        };
        let alternate = match alt {
            Some(a) => match self.branch(cx, t, case, a) {
                Some(b) => Some(b),
                None => return,
            },
            None => None,
        };
        if alternate
            .as_ref()
            .is_some_and(|a| a.method != consequent.method)
        {
            return;
        }
        let is_object = matches!(ast.node(t.value), Node::ObjectLit { .. });
        let stmt = t.next;
        let message = format!(
            "Immediate mutation on {} is not allowed.",
            Self::object_type(cx, t.value)
        );
        let unfixable = !consequent.can_fix || alternate.as_ref().is_some_and(|a| !a.can_fix);
        let fix = if is_typescript(cx) || comments_would_move(cx, stmt) || unfixable {
            None
        } else {
            let prepend = consequent.method.as_deref() == Some("unshift");
            let text = format!(
                "...(({}) ? {} : {})",
                paren_text(cx, test),
                consequent.text,
                alternate
                    .as_ref()
                    .map_or(if is_object { "{}" } else { "[]" }, |a| a.text.as_str())
            );
            let fix = if matches!(ast.node(t.value), Node::New { .. }) {
                append_to_constructor(cx, t.value, &text, stmt)
            } else {
                let edit = if prepend {
                    (Span::at(cx.span(t.value).start + 1), format!("{text}, "))
                } else {
                    append_to_list(cx, t.value, &text)
                };
                Some(with(remove_after_assign(cx, stmt), [edit]))
            };
            fix.map(|fix| {
                let side_effect = std::iter::once(test)
                    .chain(consequent.inputs.iter().copied())
                    .chain(alternate.iter().flat_map(|a| a.inputs.iter().copied()))
                    .any(|n| has_side_effect(ast, n));
                let suggest = side_effect || (prepend && !list_of(ast, t.value).is_empty());
                (suggest, fix)
            })
        };
        let span = cx.span(stmt);
        let desc = format!("Move the conditional mutation to the {}.", t.assign_type);
        let r = cx.report(span, message).id("error");
        if let Some((suggest, fix)) = fix {
            if suggest {
                r.suggest("suggestion/conditional", desc, fix);
            } else {
                r.fix(fix);
            }
        }
    }

    /// `x = value;` or the last declarator of a declaration statement.
    fn mutation_target(&self, cx: &mut Ctx<'_, '_>, id: NodeId) {
        let ast = cx.ast();
        let (name_node, value, stmt, assign_type) = match *ast.node(id) {
            Node::VarDecl {
                modifiers, decls, ..
            } => {
                if modifiers.contains(Modifiers::EXPORT) {
                    return;
                }
                let Some(&last) = ast.list(decls).last() else {
                    return;
                };
                let Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } = *ast.node(last)
                else {
                    return;
                };
                (name, init, id, "declaration")
            }
            Node::ExprStmt { expr } => {
                let Node::Assign {
                    left,
                    op: AssignOp::Assign,
                    right,
                } = *ast.node(skip_parens(ast, expr))
                else {
                    return;
                };
                (skip_parens(ast, left), right, id, "assignment")
            }
            _ => return,
        };
        if !matches!(ast.node(name_node), Node::Ident { .. }) {
            return;
        }
        let value = skip_parens(ast, value);
        if cases_for(ast, value).is_empty() {
            return;
        }
        let Some(next) = next_statement(cx, stmt) else {
            return;
        };
        if !matches!(ast.node(next), Node::ExprStmt { .. } | Node::If { .. }) {
            return;
        }
        let sem = cx.semantic();
        let sym = if assign_type == "declaration" {
            match sem.symbol_of_decl(name_node) {
                Some(s) if sem.symbol(s).decls.len() == 1 => s,
                _ => return,
            }
        } else {
            match sem
                .reference_at(name_node)
                .and_then(|r| sem.reference(r).symbol())
            {
                Some(s) => s,
                None => return,
            }
        };
        let Some(name) = ident_name(ast, name_node) else {
            return;
        };
        let t = Target {
            name,
            sym,
            value,
            next,
            assign_type,
        };
        self.check(cx, &t);
    }
}

/// The expression of the only statement of an `if` branch (`x;` or `{ x; }`).
fn only_expression(ast: &Ast, stmt: Option<NodeId>) -> Option<NodeId> {
    let mut stmt = stmt?;
    if let Node::Block { stmts } = *ast.node(stmt) {
        let [only] = ast.list(stmts) else {
            return None;
        };
        stmt = *only;
    }
    match *ast.node(stmt) {
        Node::ExprStmt { expr } => Some(expr),
        _ => None,
    }
}

/// The statement has a comment inside it, right before it, or after it on its last line.
fn comments_would_move(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(stmt);
    if !ast.comments_in(span).is_empty() {
        return true;
    }
    let before = skip_trivia_backward(ast, src, span.start as usize) as u32;
    if ast
        .comments_in(Span::new(before, span.start))
        .iter()
        .any(|c| c.span.start >= before && c.span.end <= span.start)
    {
        return true;
    }
    let after = skip_trivia_forward(src, span.end as usize) as u32;
    ast.comments_in(Span::new(span.end, after)).iter().any(|c| {
        c.span.start >= span.end
            && !src[span.end as usize..c.span.start as usize]
                .contains(['\n', '\r', '\u{2028}', '\u{2029}'])
    })
}

/// `isSimpleInitializationValue`: identifiers, literals, functions, plain templates,
/// negative numbers, and arrays/objects of those.
fn is_simple_value(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 100 {
        return false;
    }
    let id = skip_ts_wrappers(ast, id);
    match *ast.node(id) {
        Node::Ident { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::RegexLit { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. } => true,
        Node::TemplateLit { parts } => ast.list(parts).len() <= 1,
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => matches!(
            ast.node(skip_ts_wrappers(ast, arg)),
            Node::NumberLit { .. } | Node::BigIntLit { .. }
        ),
        Node::ArrayLit { elements } => ast
            .list(elements)
            .iter()
            .all(|&e| matches!(ast.node(e), Node::Hole {}) || is_simple_value(ast, e, depth + 1)),
        Node::ObjectLit { props } => ast.list(props).iter().all(|&p| match *ast.node(p) {
            Node::Property { key, value, .. } => {
                !matches!(ast.node(key), Node::ComputedKey { .. })
                    && is_simple_value(ast, value, depth + 1)
            }
            Node::ShorthandProp { init: None, .. } => true,
            Node::MethodDef { key, .. } => !matches!(ast.node(key), Node::ComputedKey { .. }),
            _ => false,
        }),
        _ => false,
    }
}

fn skip_ts_wrappers(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

impl NoImmediateMutation {
    fn initialization(&self, cx: &mut Ctx<'_, '_>, decl: NodeId) {
        let ast = cx.ast();
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = *ast.node(decl)
        else {
            return;
        };
        let decls = ast.list(decls);
        if !matches!(kind, VarKind::Let | VarKind::Var)
            || modifiers.contains(Modifiers::DECLARE)
            || modifiers.contains(Modifiers::EXPORT)
            || decls.iter().any(|&d| {
                !matches!(*ast.node(d), Node::VarDeclarator { init: None, name, .. }
                    if matches!(ast.node(name), Node::Ident { .. }))
            })
        {
            return;
        }
        let Some(next) = next_statement(cx, decl) else {
            return;
        };
        let Node::ExprStmt { expr } = *ast.node(next) else {
            return;
        };
        let assign = skip_parens(ast, expr);
        let Node::Assign {
            left,
            op: AssignOp::Assign,
            right,
        } = *ast.node(assign)
        else {
            return;
        };
        let pattern = matches!(ast.node(skip_parens(ast, left)), Node::ArrayLit { .. })
            && !is_parenthesized(cx, left);
        let left_inner = skip_parens(ast, left);
        let targets: Vec<NodeId> = match *ast.node(left_inner) {
            Node::ArrayLit { elements } if pattern => ast
                .list(elements)
                .iter()
                .copied()
                .filter(|&e| !matches!(ast.node(e), Node::Hole {}))
                .collect(),
            Node::Ident { .. } => vec![left_inner],
            _ => return,
        };
        if targets.is_empty()
            || targets
                .iter()
                .any(|&e| !matches!(ast.node(e), Node::Ident { .. }))
        {
            return;
        }
        let mut names: Vec<&str> = Vec::new();
        for &e in &targets {
            let Some(n) = ident_name(ast, e) else {
                return;
            };
            if names.contains(&n) {
                return;
            }
            names.push(n);
        }
        let decl_name = |d: NodeId| match *ast.node(d) {
            Node::VarDeclarator { name, .. } => ident_name(ast, name),
            _ => None,
        };
        let declarators: Vec<NodeId> = decls
            .iter()
            .copied()
            .filter(|&d| decl_name(d).is_some_and(|n| names.contains(&n)))
            .collect();
        if declarators.len() != names.len() {
            return;
        }
        let sem = cx.semantic();
        for &d in &declarators {
            let Node::VarDeclarator { name, .. } = *ast.node(d) else {
                return;
            };
            match sem.symbol_of_decl(name) {
                Some(s) if sem.symbol(s).decls.len() == 1 && !mentions(cx, s, &[right]) => {}
                _ => return,
            }
        }
        let message = "Combine this initialization with the preceding declaration.";
        let span = cx.span(assign);
        let declarator_spans: Vec<Span> = decls.iter().map(|&d| cx.span(d)).collect();
        let annotated = |d: NodeId| {
            matches!(
                *ast.node(d),
                Node::VarDeclarator {
                    type_ann: Some(_),
                    ..
                }
            )
        };
        if would_remove_comments(cx, cx.span(decl), &declarator_spans)
            || would_remove_comments(
                cx,
                cx.span(next),
                &[cx.span(left_inner), paren_span(cx, right)],
            )
            || declarators.iter().any(|&d| {
                matches!(*ast.node(d), Node::VarDeclarator { definite: true, .. })
                    || (pattern && annotated(d))
            })
        {
            cx.report(span, message).id("initialization");
            return;
        }
        let keyword = match kind {
            VarKind::Let => "let",
            _ => "var",
        };
        let remaining: Vec<&str> = decls
            .iter()
            .filter(|d| !declarators.contains(d))
            .map(|&d| cx.text(d))
            .collect();
        let first = if remaining.is_empty() {
            remove_statements(cx, decl, decl, false)
        } else {
            Fix::replace(
                cx.span(decl),
                format!("{keyword} {};", remaining.join(", ")),
            )
        };
        let target = if pattern {
            cx.text(left_inner).to_owned()
        } else {
            cx.text(declarators[0]).to_owned()
        };
        let fix = first.and(
            cx.span(next),
            format!("{keyword} {target} = {};", paren_text(cx, right)),
        );
        let suggest = pattern
            || is_parenthesized(cx, left_inner)
            || !is_simple_value(ast, right, 0)
            || (is_typescript(cx) && declarators.iter().any(|&d| !annotated(d)));
        let r = cx.report(span, message).id("initialization");
        if suggest {
            r.suggest(
                "suggestion/initialization",
                "Combine the initialization and declaration.",
                fix,
            );
        } else {
            r.fix(fix);
        }
    }
}

impl Rule for NoImmediateMutation {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDecl, NodeKind::ExprStmt]
    }
    fn enter(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        if matches!(cx.node(id), Node::VarDecl { .. }) {
            self.initialization(cx, id);
        }
        self.mutation_target(cx, id);
    }
}

/// A TypeScript file name, or the TypeScript parser.
fn is_typescript(cx: &Ctx<'_, '_>) -> bool {
    is_ts_file(cx) || cx.env().typescript
}
