//! `unicorn/prefer-iterator-zip`: `for (let i = 0; <bound>; i++) { .. }` (block body, `i`
//! starting at literal `0`, stepped by `i++`, `++i`, `i += 1` or `i = i + 1`) where the
//! bound stops at the shortest of two or more distinct identifier inputs: `i <
//! Math.min(a.length, b.length, ..)`, `a.length > i && i < b.length && ..` (either
//! comparison direction), or `i < n` with `n = Math.min(..)` cached as the second header
//! declarator or as a one-declarator `const`/`let` statement right before the loop, read
//! nowhere else. No input may be known to be a non-indexed collection.
//!
//! Every reference in the body to `i` or to an input (the same bindings as in the header)
//! must be part of an `input[i]` read (computed, not optional, not written, called, tagged
//! or a `for` head) outside any nested function or class, and every input must be read.
//! Reported over the loop header. The suggestion becomes `for (const [a1, b1] of
//! Iterator.zip([a, b]))`, each name the input's singular (or `element`) made unique in
//! the body's scopes and among themselves (never `Iterator`), replaces each read, and
//! removes a cached bound statement; none when a comment is in a replaced range.

use super::prelude::*;
use super::unicorn_ast::{is_left_hand_side, method_call};
use super::unicorn_text::has_comment_in;
use super::unicorn_types::is_known_non_indexed_collection;
use super::unicorn_util_h4::{available_name, scope_within, singular};
use crate::semantic::{RefFlags, Resolution, ScopeId, SymbolId};
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-iterator-zip",
        "Prefer `Iterator.zip()` over parallel-array indexing.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferIteratorZip>,
};

#[derive(Debug, Default)]
struct PreferIteratorZip;

fn is_named(ast: &Ast, id: NodeId, name: &str) -> bool {
    let id = skip_parens(ast, id);
    matches!(ast.node(id), Node::Ident { .. }) && ident_name(ast, id) == Some(name)
}

fn is_number(ast: &Ast, id: NodeId, n: f64) -> bool {
    matches!(*ast.node(skip_parens(ast, id)), Node::NumberLit { value } if value.get() == n)
}

fn is_increment(ast: &Ast, update: Option<NodeId>, name: &str) -> bool {
    let Some(update) = update.map(|u| skip_parens(ast, u)) else {
        return false;
    };
    match *ast.node(update) {
        Node::Update {
            op: UpdateOp::Increment,
            arg,
            ..
        } => is_named(ast, arg, name),
        Node::Assign { left, op, right } if is_named(ast, left, name) => match op {
            AssignOp::AddAssign => is_number(ast, right, 1.0),
            AssignOp::Assign => matches!(
                *ast.node(skip_parens(ast, right)),
                Node::Binary { op: BinaryOp::Add, left, right }
                    if is_named(ast, left, name) && is_number(ast, right, 1.0)
            ),
            _ => false,
        },
        _ => false,
    }
}

/// `i < bound` / `bound > i`: the bound.
fn upper_bound(ast: &Ast, test: Option<NodeId>, index: &str) -> Option<NodeId> {
    let Node::Binary { left, op, right } = *ast.node(skip_parens(ast, test?)) else {
        return None;
    };
    match op {
        BinaryOp::Lt if is_named(ast, left, index) => Some(skip_parens(ast, right)),
        BinaryOp::Gt if is_named(ast, right, index) => Some(skip_parens(ast, left)),
        _ => None,
    }
}

/// `x.length` with `x` an identifier: `x`.
fn length_input(ast: &Ast, id: Option<NodeId>) -> Option<NodeId> {
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(skip_parens(ast, id?))
    else {
        return None;
    };
    let obj = skip_parens(ast, obj);
    (ident_name(ast, prop) == Some("length") && matches!(ast.node(obj), Node::Ident { .. }))
        .then_some(obj)
}

fn distinct(ast: &Ast, inputs: Vec<Option<NodeId>>) -> Option<Vec<NodeId>> {
    let inputs: Vec<NodeId> = inputs.into_iter().collect::<Option<_>>()?;
    let mut names: Vec<&str> = inputs.iter().filter_map(|&i| ident_name(ast, i)).collect();
    names.sort_unstable();
    names.dedup();
    (names.len() == inputs.len()).then_some(inputs)
}

fn math_min_inputs(ast: &Ast, id: Option<NodeId>) -> Option<Vec<NodeId>> {
    let id = skip_parens(ast, id?);
    let call = method_call(ast, id)?;
    if call.name != "min"
        || call.args.len() < 2
        || !call.plain()
        || !is_named(ast, call.obj, "Math")
    {
        return None;
    }
    distinct(
        ast,
        call.args
            .iter()
            .map(|&a| length_input(ast, Some(a)))
            .collect(),
    )
}

fn and_operands(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    let inner = skip_parens(ast, id);
    match *ast.node(inner) {
        Node::Binary {
            op: BinaryOp::LogicalAnd,
            left,
            right,
        } if depth < 1000 => {
            and_operands(ast, left, out, depth + 1);
            and_operands(ast, right, out, depth + 1);
        }
        _ => out.push(inner),
    }
}

fn test_inputs(ast: &Ast, test: Option<NodeId>, index: &str) -> Option<Vec<NodeId>> {
    if let Some(inputs) = math_min_inputs(ast, upper_bound(ast, test, index)) {
        return Some(inputs);
    }
    let mut operands = Vec::new();
    and_operands(ast, test?, &mut operands, 0);
    if operands.len() < 2 {
        return None;
    }
    distinct(
        ast,
        operands
            .iter()
            .map(|&o| length_input(ast, upper_bound(ast, Some(o), index)))
            .collect(),
    )
}

/// The inputs of a cached `n = Math.min(..)` bound read only by the loop test.
fn cached_inputs(
    cx: &Ctx<'_, '_>,
    test: Option<NodeId>,
    index: &str,
    declarator: NodeId,
) -> Option<Vec<NodeId>> {
    let ast = cx.ast();
    let bound = upper_bound(ast, test, index)?;
    let Node::VarDeclarator { name, init, .. } = *ast.node(declarator) else {
        return None;
    };
    if !matches!(ast.node(bound), Node::Ident { .. })
        || !matches!(ast.node(name), Node::Ident { .. })
        || ident_name(ast, name) != ident_name(ast, bound)
    {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.symbol_of_decl(name)?;
    let refs: Vec<NodeId> = sem
        .symbol(sym)
        .refs
        .iter()
        .map(|&r| sem.reference(r))
        .filter(|r| !r.flags.contains(RefFlags::INIT))
        .map(|r| r.node)
        .collect();
    if refs != [bound] {
        return None;
    }
    math_min_inputs(ast, init)
}

struct LoopInfo {
    index: NodeId,
    inputs: Vec<NodeId>,
    remove: Option<NodeId>,
}

fn loop_info(cx: &Ctx<'_, '_>, id: NodeId) -> Option<LoopInfo> {
    let ast = cx.ast();
    let Node::For {
        init: Some(init),
        test,
        update,
        body,
    } = *ast.node(id)
    else {
        return None;
    };
    if !matches!(ast.node(body), Node::Block { .. }) {
        return None;
    }
    let Node::VarDecl {
        kind: VarKind::Let,
        decls,
        ..
    } = *ast.node(init)
    else {
        return None;
    };
    let decls = ast.list(decls);
    if decls.len() > 2 {
        return None;
    }
    let Node::VarDeclarator {
        name: index,
        init: Some(start),
        ..
    } = *ast.node(decls[0])
    else {
        return None;
    };
    if !matches!(ast.node(index), Node::Ident { .. }) || !is_number(ast, start, 0.0) {
        return None;
    }
    let name = ident_name(ast, index)?;
    if !is_increment(ast, update, name) {
        return None;
    }
    if let [_, cache] = *decls {
        let inputs = cached_inputs(cx, test, name, cache)?;
        return Some(LoopInfo {
            index,
            inputs,
            remove: None,
        });
    }
    if let Some(inputs) = test_inputs(ast, test, name) {
        return Some(LoopInfo {
            index,
            inputs,
            remove: None,
        });
    }
    let prev = prev_statement(ast, cx.file.parents, id)?;
    let Node::VarDecl { kind, decls, .. } = *ast.node(prev) else {
        return None;
    };
    let [declarator] = *ast.list(decls) else {
        return None;
    };
    if !matches!(kind, VarKind::Const | VarKind::Let) {
        return None;
    }
    let inputs = cached_inputs(cx, test, name, declarator)?;
    Some(LoopInfo {
        index,
        inputs,
        remove: Some(prev),
    })
}

fn resolved(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    match sem.reference(sem.reference_at(ident)?).resolution {
        Resolution::Symbol(s) => Some(s),
        _ => None,
    }
}

fn is_captured(cx: &Ctx<'_, '_>, ident: NodeId, body: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = cx.parent(ident);
    while let Some(n) = cur {
        if n == body {
            return false;
        }
        if matches!(
            ast.node(n),
            Node::Arrow { .. }
                | Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::MethodDef { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
        ) {
            return true;
        }
        cur = cx.parent(n);
    }
    false
}

/// The element access is only read: not written, called, tagged or a `for` head.
fn is_element_read(cx: &Ctx<'_, '_>, member: NodeId) -> bool {
    if is_left_hand_side(cx, member) {
        return false;
    }
    let ast = cx.ast();
    let mut cur = member;
    let parent = loop {
        let Some(p) = cx.parent(cur) else {
            return true;
        };
        match ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            _ => break p,
        }
    };
    !match *ast.node(parent) {
        Node::ForIn { left, .. } | Node::ForOf { left, .. } => left == cur,
        Node::Call { callee, .. } => callee == cur,
        Node::TaggedTemplate { tag, .. } => tag == cur,
        _ => false,
    }
}

/// `singular`, with pluralize's `-ta` → `-tum` for `data` (`metadata` → `metadatum`).
fn singular_name(name: &str) -> Option<String> {
    if name.ends_with("data") || name.ends_with("Data") {
        return Some(format!("{}um", &name[..name.len() - 1]));
    }
    singular(name)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

impl Rule for PreferIteratorZip {
    fn exit_kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::For]
    }
    fn exit(&self, id: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(info) = loop_info(cx, id) else {
            return;
        };
        if info
            .inputs
            .iter()
            .any(|&i| is_known_non_indexed_collection(cx, i))
        {
            return;
        }
        let ast = cx.ast();
        let Node::For { body, .. } = *ast.node(id) else {
            return;
        };
        let sem = cx.semantic();
        let Some(index_sym) = sem.symbol_of_decl(info.index) else {
            return;
        };
        let input_names: Vec<&str> = info
            .inputs
            .iter()
            .filter_map(|&i| ident_name(ast, i))
            .collect();
        let input_syms: Vec<Option<SymbolId>> =
            info.inputs.iter().map(|&i| resolved(cx, i)).collect();
        let input_of = |ident: NodeId, sym: Option<SymbolId>| {
            let name = ident_name(ast, ident)?;
            let k = input_names.iter().position(|&n| n == name)?;
            (input_syms[k] == sym).then_some(k)
        };
        let body_span = cx.span(body);
        let mut reads: Vec<Vec<NodeId>> = vec![Vec::new(); info.inputs.len()];
        for r in sem.references() {
            let s = cx.span(r.node);
            if s.start < body_span.start || s.end > body_span.end {
                continue;
            }
            let sym = match r.resolution {
                Resolution::Symbol(s) => Some(s),
                _ => None,
            };
            let is_index = sym == Some(index_sym);
            if !is_index && input_of(r.node, sym).is_none() {
                continue;
            }
            if is_captured(cx, r.node, body) {
                return;
            }
            let Some(member) = parent_skip_parens(ast, cx.file.parents, r.node) else {
                return;
            };
            let Node::Index {
                obj,
                optional: false,
                index,
            } = *ast.node(member)
            else {
                return;
            };
            let (obj, index) = (skip_parens(ast, obj), skip_parens(ast, index));
            if !matches!(ast.node(obj), Node::Ident { .. })
                || !matches!(ast.node(index), Node::Ident { .. })
                || resolved(cx, index) != Some(index_sym)
                || !is_element_read(cx, member)
            {
                return;
            }
            let Some(k) = input_of(obj, resolved(cx, obj)) else {
                return;
            };
            if !reads[k].contains(&member) {
                reads[k].push(member);
            }
        }
        if reads.iter().any(Vec::is_empty) {
            return;
        }
        let src = cx.src();
        let span = cx.span(id);
        let header_end = skip_trivia_backward(ast, src, body_span.start as usize) as u32;
        let header = Span::new(span.start, header_end);
        let removal = info.remove.map(|d| Span::new(cx.span(d).start, span.start));
        let commented = has_comment_in(cx, header)
            || removal.is_some_and(|r| has_comment_in(cx, r))
            || reads
                .iter()
                .flatten()
                .any(|&m| has_comment_in(cx, cx.span(m)));
        let fix = (!commented)
            .then(|| {
                let body_scope = sem
                    .scope_of_node(body)
                    .unwrap_or_else(|| sem.enclosing_scope(body));
                let scopes: Vec<ScopeId> = (0..sem.scopes().len())
                    .map(|i| ScopeId(i as u32))
                    .filter(|&s| scope_within(sem, s, body_scope))
                    .collect();
                let mut generated = vec!["Iterator".to_owned()];
                let mut names = Vec::new();
                for name in &input_names {
                    let base = singular_name(name).unwrap_or_else(|| "element".to_owned());
                    let n =
                        available_name(cx, &base, &scopes, &|n| !generated.iter().any(|g| g == n))?;
                    generated.push(n.clone());
                    names.push(n);
                }
                let mut fix = Fix::replace(
                    header,
                    format!(
                        "for (const [{}] of Iterator.zip([{}]))",
                        names.join(", "),
                        input_names.join(", ")
                    ),
                );
                if let Some(r) = removal {
                    fix = fix.and(r, "");
                }
                for (k, members) in reads.iter().enumerate() {
                    for &m in members {
                        let s = cx.span(m);
                        let bytes = src.as_bytes();
                        let before = s.start > 0 && is_word_byte(bytes[s.start as usize - 1]);
                        let after = bytes.get(s.end as usize).is_some_and(|&b| is_word_byte(b));
                        fix = fix.and(
                            s,
                            format!(
                                "{}{}{}",
                                if before { " " } else { "" },
                                names[k],
                                if after { " " } else { "" }
                            ),
                        );
                    }
                }
                Some(fix)
            })
            .flatten();
        let r = cx
            .report(
                header,
                "Use `Iterator.zip(…)` to iterate over these arrays together.",
            )
            .id("prefer-iterator-zip");
        if let Some(f) = fix {
            r.suggest(
                "prefer-iterator-zip/suggestion",
                "Use `Iterator.zip(…)`.",
                f,
            );
        }
    }
}
