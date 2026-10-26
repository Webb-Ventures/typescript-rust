//! `unicorn/prefer-switch`: an `if`/`else if` chain whose tests are all `===` comparisons
//! (or `||` of them) against one shared expression, at least `minimumCases` (3) long, is
//! reported at the head of the first `if`.
//!
//! The shared expression ("discriminant") is a non-constant side of the first comparison
//! that every later comparison also has (same reference); the chain ends at the first `if`
//! that does not fit, which becomes the `default:`. Chains are found outermost-first in
//! source order, and an `if` already in a reported chain does not start another.
//!
//! The fix rewrites the chain in place: `switch (d) {` before it, each `if (...)` head
//! replaced by its `case` labels, `else` text dropped, a `break;` added (with braces for a
//! non-block body) unless the body ends in `return`/`throw` (through blocks and complete
//! `if`/`else`), and a closing `// No default` (or `default: // Do nothing`) when there is
//! no `else`. There is no fix when the discriminant has side effects or an unlabeled
//! `break` in a branch targets a loop or `switch` outside it (or nothing), and no fix when
//! the dropped text holds a comment or two labels have the same static value. It is a
//! suggestion when the discriminant may run a getter or a test has side effects.

use super::prelude::*;
use super::unicorn_ast::linebreak;
use super::unicorn_static::{Static, static_value};
use super::unicorn_text::{is_same_reference, static_string_value, would_remove_comments};
use super::unicorn_util::{fix_of, has_side_effect_with, indent_before};
use ts_r_ast::{Ast, BinaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-switch",
        "Prefer `switch` over multiple `else-if`.",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<PreferSwitch>,
};

const MESSAGE: &str = "Use `switch` instead of multiple `else-if`.";

#[derive(Debug, Clone, Copy, PartialEq)]
enum EmptyDefault {
    NoDefaultComment,
    DoNothingComment,
    NoDefaultCase,
}

#[derive(Debug)]
struct PreferSwitch {
    minimum_cases: usize,
    empty_default: EmptyDefault,
}

impl FromOptions for PreferSwitch {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut rule = PreferSwitch {
            minimum_cases: 3,
            empty_default: EmptyDefault::NoDefaultComment,
        };
        if let Some(o) = opts.first().and_then(Value::as_object) {
            if let Some(v) = o.get("minimumCases") {
                let n = v
                    .as_u64()
                    .filter(|&n| n >= 2)
                    .ok_or("`minimumCases` must be an integer of at least 2")?;
                rule.minimum_cases = usize::try_from(n).unwrap_or(usize::MAX);
            }
            if let Some(v) = o.get("emptyDefaultCase") {
                rule.empty_default = match v.as_str() {
                    Some("no-default-comment") => EmptyDefault::NoDefaultComment,
                    Some("do-nothing-comment") => EmptyDefault::DoNothingComment,
                    Some("no-default-case") => EmptyDefault::NoDefaultCase,
                    _ => return Err("invalid `emptyDefaultCase`".into()),
                };
            }
        }
        Ok(rule)
    }
}

struct Branch {
    stmt: NodeId,
    /// `(left, right)` of each `===`, parentheses skipped.
    compares: Vec<(NodeId, NodeId)>,
}

fn is_constant(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::StringLit { .. }
        | Node::RegexLit { .. } => true,
        Node::Ident { name } => ast.atom(name) == "undefined",
        _ => static_string_value(ast, id).is_some(),
    }
}

fn or_operands(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>, depth: u32) {
    let id = skip_parens(ast, id);
    match *ast.node(id) {
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr,
            right,
        } if depth < 256 => {
            or_operands(ast, left, out, depth + 1);
            or_operands(ast, right, out, depth + 1);
        }
        _ => out.push(id),
    }
}

/// The `===` comparisons of an `||` chain, or nothing when one operand is something else.
fn equality_comparisons(ast: &Ast, test: NodeId) -> Vec<(NodeId, NodeId)> {
    let mut ops = Vec::new();
    or_operands(ast, test, &mut ops, 0);
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        match *ast.node(op) {
            Node::Binary {
                left,
                op: BinaryOp::StrictEq,
                right,
            } => out.push((skip_parens(ast, left), skip_parens(ast, right))),
            _ => return Vec::new(),
        }
    }
    out
}

fn is_same(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    a == b || is_same_reference(cx, a, b) || same_computed_member(cx, a, b)
}

/// `a.b` and `a["b" + ""]`: `isSameReference` folds a computed key made only of literals
/// (`getStaticValue` without a scope), which `is_same_reference` leaves out.
fn same_computed_member(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    let ast = cx.ast();
    let key = |id: NodeId| -> Option<(NodeId, String)> {
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::Member { obj, prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => Some((obj, ast.atom(name).to_owned())),
                _ => None,
            },
            Node::Index { obj, index, .. } if literals_only(ast, index, 0) => {
                let v = static_value(cx, index)?;
                Some((obj, label_key(&v)?.splitn(3, ':').nth(2)?.to_owned()))
            }
            _ => None,
        }
    };
    let (Some((oa, ka)), Some((ob, kb))) = (key(a), key(b)) else {
        return false;
    };
    ka == kb && is_same_reference(cx, oa, ob)
}

fn literals_only(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 64 {
        return false;
    }
    match *ast.node(id) {
        Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {} => true,
        Node::Paren { expr } | Node::Unary { arg: expr, .. } => literals_only(ast, expr, depth + 1),
        Node::Binary { left, right, .. } => {
            literals_only(ast, left, depth + 1) && literals_only(ast, right, depth + 1)
        }
        _ => static_string_value(ast, id).is_some(),
    }
}

fn chain(cx: &Ctx<'_, '_>, first: NodeId) -> (Vec<Branch>, Option<NodeId>) {
    let ast = cx.ast();
    let mut candidates: Option<Vec<NodeId>> = None;
    let mut branches = Vec::new();
    let mut cur = Some(first);
    while let Some(stmt) = cur {
        let Node::If { test, alt, .. } = *ast.node(stmt) else {
            break;
        };
        let compares = equality_comparisons(ast, test);
        let Some(&(l, r)) = compares.first() else {
            break;
        };
        let start = candidates.get_or_insert_with(|| {
            [l, r]
                .into_iter()
                .filter(|&n| !is_constant(ast, n))
                .collect()
        });
        let mut cands = start.clone();
        for &(left, right) in &compares {
            cands.retain(|&n| is_same(cx, n, left) || is_same(cx, n, right));
            if cands.is_empty() {
                break;
            }
        }
        if cands.is_empty() {
            break;
        }
        candidates = Some(cands);
        branches.push(Branch { stmt, compares });
        cur = alt;
    }
    let disc = candidates.and_then(|c| c.first().copied());
    (branches, disc)
}

fn is_breakable(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::While { .. }
            | Node::DoWhile { .. }
            | Node::For { .. }
            | Node::ForOf { .. }
            | Node::ForIn { .. }
            | Node::Switch { .. }
    )
}

fn inside(inner: Span, outer: Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

/// An unlabeled `break` in `stmt` that leaves it (its target is outside, or there is none).
fn has_break_inside(cx: &Ctx<'_, '_>, breaks: &[NodeId], stmt: NodeId) -> bool {
    let ast = cx.ast();
    let span = cx.span(stmt);
    breaks.iter().any(|&b| {
        if !inside(cx.span(b), span) {
            return false;
        }
        match ancestors_inclusive(cx.file.parents, b)
            .skip(1)
            .find(|&a| is_breakable(ast, a))
        {
            None => true,
            Some(target) => inside(span, cx.span(target)),
        }
    })
}

fn block_last_node(ast: &Ast, block: NodeId, depth: u32) -> Option<NodeId> {
    let Node::Block { stmts } = *ast.node(block) else {
        return None;
    };
    for &s in ast.list(stmts).iter().rev() {
        match ast.node(s) {
            Node::FunctionDecl { .. } | Node::Empty {} => {}
            Node::Block { .. } => {
                if depth < 256
                    && let Some(last) = block_last_node(ast, s, depth + 1)
                {
                    return Some(last);
                }
            }
            _ => return Some(s),
        }
    }
    None
}

/// The branch can fall through to the next `case` (a guess without code path analysis).
fn needs_break(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 256 {
        return true;
    }
    match *ast.node(id) {
        Node::Return { .. } | Node::Throw { .. } => false,
        Node::If { alt: None, .. } => true,
        Node::If {
            cons,
            alt: Some(alt),
            ..
        } => needs_break(ast, cons, depth + 1) || needs_break(ast, alt, depth + 1),
        Node::Block { .. } => {
            block_last_node(ast, id, 0).is_none_or(|last| needs_break(ast, last, depth + 1))
        }
        _ => true,
    }
}

/// `getStaticValue`'s value as a label key: its `typeof` and `String()` form.
fn label_key(v: &Static) -> Option<String> {
    fn string_of(v: &Static) -> Option<String> {
        Some(match v {
            Static::Undefined => "undefined".into(),
            Static::Null => "null".into(),
            Static::Bool(b) => b.to_string(),
            Static::Num(n) => number_to_string(*n),
            Static::Str(s) => s.clone(),
            Static::BigInt(n) => n.to_string(),
            Static::RegExp { source, flags } => format!("/{source}/{flags}"),
            Static::Array(items) => {
                let mut parts = Vec::with_capacity(items.len());
                for item in items {
                    parts.push(match item {
                        Static::Undefined | Static::Null => String::new(),
                        _ => string_of(item)?,
                    });
                }
                parts.join(",")
            }
            Static::Object(_) => "[object Object]".into(),
            Static::Builtin(_) => return None,
        })
    }
    let ty = match v {
        Static::Undefined => "undefined",
        Static::Bool(_) => "boolean",
        Static::Num(_) => "number",
        Static::Str(_) => "string",
        Static::BigInt(_) => "bigint",
        Static::Builtin(path) => return Some(format!("builtin:{path}")),
        _ => "object",
    };
    Some(format!("value:{ty}:{}", string_of(v)?))
}

impl PreferSwitch {
    fn fix(&self, cx: &Ctx<'_, '_>, disc: NodeId, branches: &[Branch]) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let first = branches[0].stmt;
        let first_span = cx.span(first);
        let indent = indent_before(cx, first_span.start);
        let nl = linebreak(src);
        for b in branches {
            let Node::If { cons, alt, .. } = *ast.node(b.stmt) else {
                return None;
            };
            let s = cx.span(b.stmt);
            let c = cx.span(cons);
            if would_remove_comments(cx, Span::new(s.start, c.start), &[])
                || alt.is_some_and(|a| {
                    would_remove_comments(cx, Span::new(c.end, cx.span(a).start), &[])
                })
            {
                return None;
            }
        }
        let mut labels: Vec<String> = Vec::new();
        for b in branches {
            for &(left, right) in &b.compares {
                let node = if is_same(cx, left, disc) { right } else { left };
                let key = static_value(cx, node)
                    .as_ref()
                    .and_then(label_key)
                    .unwrap_or_else(|| format!("text:{}", cx.text(node)));
                if labels.contains(&key) {
                    return None;
                }
                labels.push(key);
            }
        }
        let at = |pos: u32| Span::new(pos, pos);
        let mut edits = vec![(
            at(first_span.start),
            format!("switch ({}) {{", cx.text(disc)),
        )];
        let last = branches[branches.len() - 1].stmt;
        let last_alt = match *ast.node(last) {
            Node::If { alt, .. } => alt,
            _ => None,
        };
        if let Some(alt) = last_alt {
            edits.push((at(cx.span(alt).start), format!("{nl}{indent}default: ")));
        }
        for b in branches {
            let Node::If { cons, alt, .. } = *ast.node(b.stmt) else {
                return None;
            };
            let c = cx.span(cons);
            if let Some(alt) = alt {
                edits.push((Span::new(c.end, cx.span(alt).start), String::new()));
            }
            edits.push((Span::new(cx.span(b.stmt).start, c.start), String::new()));
            for &(left, right) in &b.compares {
                let node = if is_same(cx, left, disc) { right } else { left };
                edits.push((at(c.start), format!("{nl}{indent}case {}: ", cx.text(node))));
            }
            if needs_break(ast, cons, 0) {
                if matches!(ast.node(cons), Node::Block { .. }) {
                    let close = c.end.saturating_sub(1);
                    edits.push((at(close), format!("{nl}{indent}break;{nl}{indent}")));
                } else {
                    edits.push((at(c.end), format!("{nl}{indent}break;")));
                    edits.push((at(c.start), format!("{{{nl}{indent}")));
                    edits.push((at(c.end), format!("{nl}{indent}}}")));
                }
            }
        }
        if last_alt.is_none() {
            match self.empty_default {
                EmptyDefault::NoDefaultComment => {
                    edits.push((at(first_span.end), format!("{nl}{indent}// No default")));
                }
                EmptyDefault::DoNothingComment => edits.push((
                    at(first_span.end),
                    format!("{nl}{indent}default:{nl}{indent}// Do nothing"),
                )),
                EmptyDefault::NoDefaultCase => {}
            }
        }
        edits.push((at(first_span.end), format!("{nl}{indent}}}")));
        Some(fix_of(edits))
    }
}

impl Rule for PreferSwitch {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // Every `if` in source order, and every unlabeled `break`.
        let mut ifs = Vec::new();
        let mut breaks = Vec::new();
        let mut stack = vec![ast.root];
        let mut kids = Vec::new();
        while let Some(id) = stack.pop() {
            match *ast.node(id) {
                Node::If { .. } => ifs.push(id),
                Node::Break { label: None } => breaks.push(id),
                _ => {}
            }
            kids.clear();
            ast.for_each_child(id, |c| kids.push(c));
            stack.extend(kids.iter().rev());
        }
        let mut checked = rustc_hash::FxHashSet::default();
        for node in ifs {
            if checked.contains(&node) {
                continue;
            }
            let (branches, disc) = chain(cx, node);
            let Some(disc) = disc else {
                continue;
            };
            if branches.len() < self.minimum_cases {
                continue;
            }
            checked.extend(branches.iter().map(|b| b.stmt));
            let Node::If { cons, .. } = *ast.node(node) else {
                continue;
            };
            let span = Span::new(cx.span(node).start, cx.span(cons).start);
            let fixable = !has_side_effect_with(ast, disc, false, false)
                && branches
                    .iter()
                    .all(|b| !has_break_inside(cx, &breaks, b.stmt));
            let fix = if fixable {
                self.fix(cx, disc, &branches)
            } else {
                None
            };
            let suggest = fixable
                && (has_side_effect_with(ast, disc, true, false)
                    || branches.iter().any(|b| match *ast.node(b.stmt) {
                        Node::If { test, .. } => has_side_effect_with(ast, test, false, false),
                        _ => false,
                    }));
            let report = cx.report(span, MESSAGE).id("prefer-switch");
            match fix {
                Some(fix) if suggest => {
                    report.suggest("prefer-switch", MESSAGE, fix);
                }
                fix => {
                    report.fix_opt(fix);
                }
            }
        }
    }
}
