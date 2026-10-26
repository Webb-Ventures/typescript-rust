//! `unicorn/prefer-single-call`: merge consecutive `x.push(..)` / `x.unshift(..)` /
//! `el.classList.add(..)` / `el.classList.remove(..)` / `importScripts(..)` statements.
//!
//! The second call must be an expression statement whose previous sibling statement is
//! the same kind of call on the same callee (`isSameReference`). `push`/`unshift` (not an
//! optional call) skip ignored callees (`stream.push`, `this.push`, `process.stdout.push`,
//! ... and the `ignore` option) and receivers known not to be arrays; `classList` calls
//! allow no optional chaining at all. Reported at the method name (or `importScripts`).
//!
//! The fix appends the second call's arguments to the first and deletes the second
//! statement (keeping a `;` the first lacked). For `unshift` the first call's arguments go
//! after the second's and the first statement is deleted, adding a `;` if the second would
//! then continue the previous line; when only the second call has `?.`, the second's
//! arguments are prepended to the first instead. No fix when the removed range holds a
//! comment. For `push`/`unshift` it is only a suggestion unless the receiver is known to
//! be an array and every argument of both calls is a side-effect-free static value; for the
//! others, unless no argument of the second call has side effects.

use super::prelude::*;
use super::unicorn_static::{has_side_effect, static_value_no_side_effects};
use super::unicorn_text::{has_comment_in, is_same_reference};
use super::unicorn_types::{is_array, should_skip_known_non_array_receiver};
use super::unicorn_util::has_optional_chain;
use super::unicorn_util_h5::{needs_semicolon, node_matches_path};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-single-call",
        "Enforce combining multiple `Array#{push,unshift}()`, `Element#classList.{add,remove}()`, and `importScripts()` into one call.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferSingleCall>,
};

#[derive(Debug, Default)]
struct PreferSingleCall {
    ignore: Vec<String>,
}

impl FromOptions for PreferSingleCall {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(PreferSingleCall {
            ignore: str_list(obj_opt(opts, 0), "ignore"),
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Case {
    Push,
    Unshift,
    ClassListAdd,
    ClassListRemove,
    ImportScripts,
}

impl Case {
    const ALL: [Case; 5] = [
        Case::Push,
        Case::Unshift,
        Case::ClassListAdd,
        Case::ClassListRemove,
        Case::ImportScripts,
    ];
    fn description(self) -> &'static str {
        match self {
            Case::Push => "Array#push()",
            Case::Unshift => "Array#unshift()",
            Case::ClassListAdd => "Element#classList.add()",
            Case::ClassListRemove => "Element#classList.remove()",
            Case::ImportScripts => "importScripts()",
        }
    }
    fn check_array(self) -> bool {
        matches!(self, Case::Push | Case::Unshift)
    }
    fn ignored(self) -> &'static [&'static str] {
        match self {
            Case::Push => &[
                "stream.push",
                "this.push",
                "this.stream.push",
                "process.stdin.push",
                "process.stdout.push",
                "process.stderr.push",
            ],
            Case::Unshift => &[
                "stream.unshift",
                "this.unshift",
                "this.stream.unshift",
                "process.stdin.unshift",
                "process.stdout.unshift",
                "process.stderr.unshift",
            ],
            _ => &[],
        }
    }
    fn test(self, ast: &ts_r_ast::Ast, call: NodeId) -> bool {
        let Node::Call {
            callee, optional, ..
        } = *ast.node(call)
        else {
            return false;
        };
        if self == Case::ImportScripts {
            return ident_name(ast, skip_parens(ast, callee)) == Some("importScripts");
        }
        let Node::Member {
            obj,
            prop,
            optional: optional_member,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return false;
        };
        if optional || !matches!(ast.node(prop), Node::Ident { .. }) {
            return false;
        }
        let name = ident_name(ast, prop);
        match self {
            Case::Push => name == Some("push"),
            Case::Unshift => name == Some("unshift"),
            Case::ClassListAdd | Case::ClassListRemove => {
                name == Some(if self == Case::ClassListAdd {
                    "add"
                } else {
                    "remove"
                }) && !optional_member
                    && matches!(*ast.node(skip_parens(ast, obj)), Node::Member { prop, .. }
                        if ident_name(ast, prop) == Some("classList"))
            }
            Case::ImportScripts => false,
        }
    }
}

fn args_of(ast: &ts_r_ast::Ast, call: NodeId) -> &[NodeId] {
    match *ast.node(call) {
        Node::Call { args, .. } => ast.list(args),
        _ => &[],
    }
}

fn callee_of(ast: &ts_r_ast::Ast, call: NodeId) -> NodeId {
    match *ast.node(call) {
        Node::Call { callee, .. } => skip_parens(ast, callee),
        _ => call,
    }
}

/// The `(`, `)` and trailing `,` of a call's argument list (an optional call's `?.` is
/// skipped).
fn call_tokens(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(u32, u32, Option<u32>)> {
    let ast = cx.ast();
    let src = cx.src();
    let Node::Call {
        callee,
        type_args,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let after = type_args.map_or(cx.span(callee).end, |t| cx.span(t).end);
    let mut open = skip_trivia_forward(src, after as usize);
    if src[open..].starts_with("?.") {
        open = skip_trivia_forward(src, open + 2);
    }
    if src.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let close = cx.span(call).end.checked_sub(1)?;
    if src.as_bytes().get(close as usize) != Some(&b')') {
        return None;
    }
    let comma = ast.list(args).last().and_then(|_| {
        let p = skip_trivia_backward(ast, src, close as usize);
        (p > 0 && src.as_bytes()[p - 1] == b',').then(|| p as u32 - 1)
    });
    Some((open as u32, close, comma))
}

fn last_char_is_semicolon(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    cx.text(stmt).ends_with(';')
}

impl Rule for PreferSingleCall {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call]
    }
    fn enter(&self, second: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        for case in Case::ALL {
            if !case.test(ast, second) {
                continue;
            }
            let second_callee = callee_of(ast, second);
            if case
                .ignored()
                .iter()
                .copied()
                .chain(self.ignore.iter().map(String::as_str))
                .any(|p| node_matches_path(ast, second_callee, p))
            {
                continue;
            }
            let Some(second_stmt) = parent_skip_parens(ast, cx.file.parents, second) else {
                continue;
            };
            if !matches!(ast.node(second_stmt), Node::ExprStmt { .. }) {
                continue;
            }
            let Some(first_stmt) = prev_statement(ast, cx.file.parents, second_stmt) else {
                continue;
            };
            let Node::ExprStmt { expr } = *ast.node(first_stmt) else {
                continue;
            };
            let first = skip_parens(ast, expr);
            let first_callee = callee_of(ast, first);
            let second_obj = match *ast.node(second_callee) {
                Node::Member { obj, .. } => Some(obj),
                _ => None,
            };
            if !case.test(ast, first)
                || !is_same_reference(cx, first_callee, second_callee)
                || (case.check_array()
                    && second_obj.is_some_and(|o| should_skip_known_non_array_receiver(cx, o)))
            {
                continue;
            }
            self.report(cx, case, first, second, first_stmt, second_stmt);
        }
    }
}

impl PreferSingleCall {
    fn report(
        &self,
        cx: &mut Ctx<'_, '_>,
        case: Case,
        first: NodeId,
        second: NodeId,
        first_stmt: NodeId,
        second_stmt: NodeId,
    ) {
        let ast = cx.ast();
        let first_callee = callee_of(ast, first);
        let second_callee = callee_of(ast, second);
        let keep_second = case == Case::Unshift
            && (!has_optional_chain(ast, second_callee) || has_optional_chain(ast, first_callee));
        let prepend = case == Case::Unshift && !keep_second;
        let (target, source) = if keep_second {
            (second, first)
        } else {
            (first, second)
        };
        let fs = cx.span(first_stmt);
        let ss = cx.span(second_stmt);
        let removal = if keep_second {
            Span::new(fs.start, ss.start)
        } else {
            Span::new(fs.end, ss.end)
        };
        let keep_semicolon = !keep_second
            && !last_char_is_semicolon(cx, first_stmt)
            && last_char_is_semicolon(cx, second_stmt);
        let add_semicolon = keep_second
            && needs_semicolon(
                cx,
                fs.start,
                cx.src()
                    .get(ss.start as usize..ss.start as usize + 1)
                    .unwrap_or(""),
            );
        let report_node = match *ast.node(second_callee) {
            Node::Member { prop, .. } => prop,
            _ => second_callee,
        };
        let message = format!("Do not call `{}` multiple times.", case.description());
        let fix = (|| {
            if has_comment_in(cx, removal) {
                return None;
            }
            let mut fix = Fix::replace(
                removal,
                if keep_semicolon || add_semicolon {
                    ";"
                } else {
                    ""
                },
            );
            let source_args = args_of(ast, source);
            if !source_args.is_empty() {
                let (s_open, s_close, s_comma) = call_tokens(cx, source)?;
                let end = if prepend {
                    s_comma.unwrap_or(s_close)
                } else {
                    s_close
                };
                let text = cx.src().get(s_open as usize + 1..end as usize)?.to_owned();
                let (t_open, t_close, t_comma) = call_tokens(cx, target)?;
                let target_has_args = !args_of(ast, target).is_empty();
                fix = if prepend {
                    fix.and(
                        Span::at(t_open + 1),
                        if target_has_args {
                            format!("{text}, ")
                        } else {
                            text
                        },
                    )
                } else if let Some(c) = t_comma {
                    fix.and(Span::at(c + 1), format!(" {text}"))
                } else {
                    fix.and(
                        Span::at(t_close),
                        if target_has_args {
                            format!(", {text}")
                        } else {
                            text
                        },
                    )
                };
            }
            Some(fix)
        })();
        let suggestion = if case.check_array() {
            let obj = match *ast.node(second_callee) {
                Node::Member { obj, .. } => obj,
                _ => second_callee,
            };
            !is_array(cx, obj)
                || args_of(ast, first)
                    .iter()
                    .chain(args_of(ast, second))
                    .any(|&a| static_value_no_side_effects(cx, a).is_none())
        } else {
            args_of(ast, second)
                .iter()
                .any(|&a| has_side_effect(ast, a))
        };
        let span = cx.span(report_node);
        let r = cx.report(span, message).id("error/array-push");
        if let Some(f) = fix {
            if suggestion {
                r.suggest("suggestion", "Merge with previous one.", f);
            } else {
                r.fix(f);
            }
        }
    }
}
