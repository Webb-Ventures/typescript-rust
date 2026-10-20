//! Expression and member rewrites, as tsserver 6.0.3 offers them (observed on the
//! `refactor-template-*`, `refactor-optional-chain-*`, `refactor-inline-*` and
//! `refactor-accessors-*` fixtures and probes; not taken from its source). Syntax only, but
//! for one checker query (operand types, when inlining a sum to the right of `+`).
//!
//! | Refactor | Offered at | Result |
//! |---|---|---|
//! | `Convert to template string` | a `+` chain holding a string or template literal (cursor anywhere in it), or a lone string literal in an expression position (also a directive, a computed key, an element access, `import()`) | `` `a${b}c` `` |
//! | `Convert to optional chain expression` | an `&&` chain whose operands are prefixes of its last access chain, or `p ? p.x : y` | `a?.b?.c`, `p?.x ?? y` |
//! | `Inline variable` | a `const`/`let`/`var` declaration name (or, when invoked, a reference) with one declaration, an initializer, no writes, not exported | every reference replaced, the declaration removed |
//! | `Generate 'get' and 'set' accessors` | a class field, a parameter property or an object literal property (when invoked, a cursor anywhere in it; on a cursor move only a selection) | the field renamed `_x` and made private, `set x` then `get x` after it |
//!
//! Observed rules:
//! - Template strings: the chain is the outermost run of binary operators above the cursor; it
//!   must be a `+` holding a string or template literal (`a - 'x'`, `'x' + a - 1`, `a + b`, a
//!   parenthesised string alone are not offered). Operands left of the first string that hold
//!   none stay one expression (`1 + n + 's'` → `` `${1 + n}s` ``); one level of parentheses
//!   is dropped (`(a + b)` → `${a + b}`); template operands are merged. String text is copied
//!   raw (escapes kept) except `\uXXXX`, which is decoded; every `` ` `` and `$` is escaped.
//!   Comments between operands move into the substitutions: before an expression (or attached
//!   to a string before the first expression) → `${ /* c */a}` (a line comment becomes
//!   `/* c*/`); after it (or attached to a string after it) → `${a /* c */}`. Not offered by
//!   us when a comment follows the chain or no expression takes it.
//! - Optional chains: with a cursor, the node is the outermost run of binary, conditional and
//!   access expressions above it (stopping at parentheses and arguments); with a selection,
//!   the innermost `&&` or conditional around it. Every operand but the last must be a call-free
//!   access chain rooted at an identifier (not `this`) and a prefix of the last, in order; the
//!   last must have no `?.`, `!` or parentheses. A conditional's condition must end with the
//!   whole chain but its last link and be more than an identifier (`a ? a.b : x` is not
//!   offered); the result is `<chain> ?? <whenFalse>`. Links are printed compactly.
//! - Inline variable: the initializer replaces each reference, parenthesised by precedence
//!   (`(1 + 2) * 3`, `-(1 + 2)`, `(o).x` only when needed; a right `+` operand keeps no parens
//!   when both sides are numeric); `{ x }` becomes `{ x: init }`; the statement goes with its
//!   line and trailing comment (comments above stay), or only its declarator among others.
//!   On a cursor move only the declaration's name offers it. Not offered by us when the result
//!   could change meaning: a name in the initializer is shadowed at a use, `this`/`arguments`/
//!   `await`/`yield` would move into another function, a use in a type (`typeof x`), a
//!   function expression initializer (tsserver reprints those), an initializer with effects
//!   (a call, `await`, ...) used more than once or anywhere but unconditionally in the next
//!   statement with nothing effectful before it (tsserver duplicates or moves it), an
//!   initializer that may create an object (`{}`, `[]`, `new C()`, a regex, a function) used
//!   more than once or inside a loop or nested function (tsserver creates one per use), a
//!   member access inlined as a callee (`obj.method` would become the call's receiver), and a
//!   value starting with `(`/`[` at the start of a statement after a line without `;`.
//! - Accessors: the field becomes `private _x` (`_x_1`, ... when taken by a member) keeping
//!   `static`/`readonly`/`!`/`?`; a field already named `_x` keeps its name and the accessors
//!   are `x`. Accessors take the field's accessibility (default `public`), its type (an
//!   optional field's type gains `| undefined`, function types parenthesised), and `static`;
//!   `readonly` fields get only the getter. They go after the field's line (before the
//!   constructor for a parameter property); object literal properties get `set x(value) {...},
//!   get x() {...},` after the property. Statics use `ClassName._x`. Not offered for
//!   `#private`, `abstract`, `declare`, `override`, `accessor` fields, parameters without an
//!   accessibility modifier, methods; nor by us for decorated or string-named fields, a member
//!   sharing its line, or static fields of anonymous classes.

use super::edit::Edit;
use super::{Action, Ctx, Request, style};
use ts_r_ast::{Ast, BinaryOp, Modifiers, Node, NodeId, ParentMap, Span, UnaryOp, UpdateOp};
use ts_r_checker::{Ck, Loc};
use ts_r_types::TypeFlags;

const TEMPLATE: &str = "Convert to template string";
const OPTIONAL: &str = "Convert to optional chain expression";
const INLINE: &str = "Inline variable";
const ACCESSORS: &str = "Generate 'get' and 'set' accessors";

/// The rewrites applicable at the request's selection.
pub fn rewrites(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>, out: &mut Vec<Action>) {
    if req.wants("refactor.rewrite.string")
        && let Some(e) = template_string(cx, req)
    {
        out.push(action(cx, TEMPLATE, "refactor.rewrite.string", e));
    }
    if req.wants("refactor.rewrite.expression.optionalChain")
        && let Some(e) = optional_chain(cx, req)
    {
        out.push(action(
            cx,
            OPTIONAL,
            "refactor.rewrite.expression.optionalChain",
            e,
        ));
    }
    if req.wants("refactor.inline.variable")
        && let Some(e) = inline_variable(cx, ck, req)
    {
        out.push(action(cx, INLINE, "refactor.inline.variable", e));
    }
    if req.wants("refactor.rewrite.property.generateAccessors")
        && let Some(e) = accessors(cx, req)
    {
        out.push(action(
            cx,
            ACCESSORS,
            "refactor.rewrite.property.generateAccessors",
            e,
        ));
    }
}

fn action(cx: &Ctx<'_, '_>, name: &str, kind: &str, edits: Vec<Edit>) -> Action {
    let mut a = Action::new(name, kind);
    a.refactor = Some(format!("{name}/{name}"));
    a.edit(&cx.checker.file(cx.file).path, edits);
    a
}

/// The deepest node whose span strictly contains `offset` (`start <= offset < end`).
fn node_inside(ast: &Ast, offset: u32) -> Option<NodeId> {
    let sp = ast.span(ast.root);
    if offset < sp.start || offset >= sp.end {
        return None;
    }
    let mut cur = ast.root;
    for _ in 0..ast.node_count() {
        let mut inside = None;
        ast.for_each_child(cur, |c| {
            let s = ast.span(c);
            if inside.is_none() && s.start <= offset && offset < s.end {
                inside = Some(c);
            }
        });
        match inside {
            Some(c) if c != cur => cur = c,
            _ => break,
        }
    }
    Some(cur)
}

fn is_binary(ast: &Ast, n: NodeId, want: BinaryOp) -> bool {
    matches!(*ast.node(n), Node::Binary { op, .. } if op == want)
}

/// Skips one level of parentheses.
fn unparen(ast: &Ast, n: NodeId) -> NodeId {
    match *ast.node(n) {
        Node::Paren { expr } => expr,
        _ => n,
    }
}

// ---- Convert to template string ----

fn has_string(ast: &Ast, n: NodeId) -> bool {
    // Down the left operands in a loop: a `+` chain nests to the left.
    let mut n = n;
    loop {
        match *ast.node(n) {
            Node::StringLit { .. } | Node::TemplateLit { .. } => return true,
            Node::Binary {
                left,
                op: BinaryOp::Add,
                right,
            } => {
                if has_string(ast, right) {
                    return true;
                }
                n = left;
            }
            _ => return false,
        }
    }
}

/// Operands of a string concatenation, left to right. The `+` chain is walked once (not
/// once per prefix), so a long chain costs linear time.
fn concat_operands(ast: &Ast, n: NodeId, out: &mut Vec<NodeId>) {
    // `n`, its left operand, that one's left operand, ... while they are `+`.
    let mut spine = vec![n];
    while let Node::Binary {
        left,
        op: BinaryOp::Add,
        ..
    } = *ast.node(spine[spine.len() - 1])
    {
        spine.push(left);
    }
    // Whether each one has a string, from the innermost out.
    let mut stringy = vec![false; spine.len()];
    let mut any = false;
    for (i, &s) in spine.iter().enumerate().rev() {
        any |= match *ast.node(s) {
            Node::Binary { right, .. } if i + 1 < spine.len() => has_string(ast, right),
            _ => has_string(ast, s),
        };
        stringy[i] = any;
    }
    // A `+` with a string splits into its operands; the first one without stays whole.
    let mut rights = Vec::new();
    let mut i = 0;
    while i + 1 < spine.len() && stringy[i] {
        if let Node::Binary { right, .. } = *ast.node(spine[i]) {
            rights.push(right);
        }
        i += 1;
    }
    out.push(spine[i]);
    out.extend(rights.into_iter().rev());
}

/// A string literal's raw text for a template: escapes kept but `\uXXXX` decoded, `` ` `` and
/// `$` escaped. `None` when a decoded character would need an escape of its own.
fn template_text(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next = chars.next()?;
                if next == 'u' && chars.peek() != Some(&'{') {
                    let hex: String = chars.by_ref().take(4).collect();
                    let v = u32::from_str_radix(&hex, 16)
                        .ok()
                        .filter(|_| hex.len() == 4)?;
                    let ch = char::from_u32(v)?;
                    if (ch as u32) < 0x20 || matches!(ch, '`' | '$' | '\\' | '\u{7f}') {
                        return None;
                    }
                    out.push(ch);
                } else {
                    out.push('\\');
                    out.push(next);
                }
            }
            '`' => out.push_str("\\`"),
            '$' => out.push_str("\\$"),
            _ => out.push(c),
        }
    }
    Some(out)
}

/// The text between a string literal's quotes.
fn string_raw(text: &str, sp: Span) -> Option<&str> {
    let s = sp.text(text);
    if s.len() < 2 {
        return None;
    }
    s.get(1..s.len() - 1)
}

fn template_string(cx: &Ctx<'_, '_>, req: &Request<'_>) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    let n = node_inside(ast, req.start)?;
    if let Node::Directive { .. } = ast.node(n) {
        let sp = ast.span(n);
        let src = sp.text(text);
        let q = src.chars().next()?;
        let close = src[1..].find(q)? + 1;
        let body = template_text(&src[1..close])?;
        return Some(vec![Edit::new(
            sp.start,
            sp.start + close as u32 + 1,
            format!("`{body}`"),
        )]);
    }
    let mut top = n;
    while let Some(p) = parents.parent(top)
        && matches!(ast.node(p), Node::Binary { .. })
    {
        top = p;
    }
    if top == n {
        if !matches!(ast.node(n), Node::StringLit { .. }) || !string_in_expression(cx, n) {
            return None;
        }
        let sp = ast.span(n);
        let body = template_text(string_raw(text, sp)?)?;
        return Some(vec![Edit::new(sp.start, sp.end, format!("`{body}`"))]);
    }
    if !is_binary(ast, top, BinaryOp::Add) || !has_string(ast, top) {
        return None;
    }
    let tsp = ast.span(top);
    // A comment right after the chain: tsserver moves it oddly; leave those alone.
    let rest = text[tsp.end as usize..].trim_start_matches([' ', '\t']);
    if rest.starts_with("/*") || rest.starts_with("//") {
        return None;
    }
    let mut ops = Vec::new();
    concat_operands(ast, top, &mut ops);
    // Literal text before the first substitution, then (expression, comments, literal) per span.
    struct Sub {
        expr: String,
        start: u32,
        end: u32,
        leading: Vec<String>,
        trailing: Vec<String>,
        lit: String,
    }
    let mut head = String::new();
    let mut subs: Vec<Sub> = Vec::new();
    let mut has_template = false;
    for &op in &ops {
        let lit = |subs: &mut Vec<Sub>, head: &mut String, s: &str| match subs.last_mut() {
            Some(x) => x.lit.push_str(s),
            None => head.push_str(s),
        };
        match *ast.node(op) {
            Node::StringLit { .. } => {
                let t = template_text(string_raw(text, ast.span(op))?)?;
                lit(&mut subs, &mut head, &t);
            }
            Node::TemplateLit { parts } => {
                has_template = true;
                for (i, &p) in ast.list(parts).iter().enumerate() {
                    if i % 2 == 0 {
                        let raw = ast.span(p).text(text);
                        lit(&mut subs, &mut head, raw);
                    } else {
                        let sp = ast.span(p);
                        subs.push(Sub {
                            expr: sp.text(text).to_string(),
                            start: sp.start,
                            end: sp.end,
                            leading: Vec::new(),
                            trailing: Vec::new(),
                            lit: String::new(),
                        });
                    }
                }
            }
            _ => {
                let sp = ast.span(op);
                let inner = ast.span(unparen(ast, op));
                subs.push(Sub {
                    expr: inner.text(text).to_string(),
                    start: sp.start,
                    end: sp.end,
                    leading: Vec::new(),
                    trailing: Vec::new(),
                    lit: String::new(),
                });
            }
        }
    }
    // Comments between operands.
    let comments: Vec<_> = ast
        .comments_in(tsp)
        .iter()
        .filter(|c| {
            !ops.iter().any(|&o| {
                let os = ast.span(o);
                os.start <= c.span.start && c.span.end <= os.end
            })
        })
        .copied()
        .collect();
    if !comments.is_empty() {
        if has_template || subs.is_empty() {
            return None;
        }
        for c in comments {
            // The operands around the comment and the `+` between them.
            let k = ops.iter().rposition(|&o| ast.span(o).end <= c.span.start)?;
            let next = *ops.get(k + 1)?;
            let gap_start = ast.span(ops[k]).end;
            let gap_end = ast.span(next).start;
            let plus = plus_in_gap(ast, text, gap_start, gap_end)?;
            let owner = if c.span.start < plus { ops[k] } else { next };
            let ctext = c.span.text(text);
            let as_leading = |t: &str| -> String {
                match t.strip_prefix("//") {
                    Some(rest) => format!("/*{rest}*/"),
                    None => t.to_string(),
                }
            };
            let as_trailing = |t: &str| -> String {
                if t.starts_with("//") {
                    format!("{t}\n")
                } else {
                    t.to_string()
                }
            };
            let osp = ast.span(owner);
            if let Some(s) = subs.iter_mut().find(|s| s.start == osp.start) {
                if c.span.start < osp.start {
                    s.leading.push(as_leading(ctext));
                } else {
                    s.trailing.push(as_trailing(ctext));
                }
            } else {
                // A string's comment: to the expression before it, or the first one.
                match subs.iter_mut().rev().find(|s| s.end <= osp.start) {
                    Some(s) => s.trailing.push(as_trailing(ctext)),
                    None => subs[0].leading.push(as_leading(ctext)),
                }
            }
        }
    }
    let mut out = format!("`{head}");
    for s in &subs {
        out.push_str("${");
        for c in &s.leading {
            out.push(' ');
            out.push_str(c);
        }
        out.push_str(&s.expr);
        for c in &s.trailing {
            out.push(' ');
            out.push_str(c);
        }
        out.push('}');
        out.push_str(&s.lit);
    }
    out.push('`');
    Some(vec![Edit::new(tsp.start, tsp.end, out)])
}

/// Offset of the `+` between two operands (outside comments).
fn plus_in_gap(ast: &Ast, text: &str, from: u32, to: u32) -> Option<u32> {
    let comments = ast.comments_in(Span::new(from, to));
    let mut i = from;
    while i < to {
        if let Some(c) = comments.iter().find(|c| c.span.start == i) {
            i = c.span.end;
            continue;
        }
        if text.as_bytes()[i as usize] == b'+' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether a string literal sits where tsserver converts it alone: an expression, not a
/// module specifier, property name, type, enum member name or JSX attribute value.
fn string_in_expression(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    if let Some(p) = parents.parent(n) {
        if matches!(ast.node(p), Node::ComputedKey { .. }) {
            return true;
        }
        if matches!(
            ast.node(p),
            Node::ImportCall { .. } | Node::Index { .. } | Node::SwitchCase { .. }
        ) {
            return true;
        }
    }
    super::refactor::in_expression_context(ast, parents, n)
}

// ---- Convert to optional chain expression ----

/// `root.a[b].c()`: the nodes from the root identifier to `n` (each an access or call on the
/// one before); `None` for anything else (`this`, `?.`, `!`, parentheses, type arguments).
fn access_chain(ast: &Ast, n: NodeId) -> Option<Vec<NodeId>> {
    let mut v = vec![n];
    let mut cur = n;
    for _ in 0..ast.node_count() {
        match *ast.node(cur) {
            Node::Ident { .. } => {
                v.reverse();
                return Some(v);
            }
            Node::Member {
                obj,
                optional: false,
                prop,
            } if matches!(
                ast.node(prop),
                Node::Ident { .. } | Node::PrivateName { .. }
            ) =>
            {
                cur = obj
            }
            Node::Index {
                obj,
                optional: false,
                ..
            } => cur = obj,
            Node::Call {
                callee,
                optional: false,
                type_args: None,
                ..
            } => cur = callee,
            _ => return None,
        }
        v.push(cur);
    }
    None
}

/// The object of an access link.
fn link_object(ast: &Ast, n: NodeId) -> Option<NodeId> {
    match *ast.node(n) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => Some(obj),
        Node::Call { callee, .. } => Some(callee),
        _ => None,
    }
}

/// A compact, whitespace-free text of an access chain (for comparing prefixes and printing),
/// with `?.` before the links whose index is in `optional`.
fn chain_text(ast: &Ast, text: &str, chain: &[NodeId], optional: &[usize]) -> Option<String> {
    let mut out = ast.span(chain[0]).text(text).to_string();
    for (i, &link) in chain.iter().enumerate().skip(1) {
        let opt = optional.contains(&(i - 1));
        let obj_end = ast.span(link_object(ast, link)?).end;
        match *ast.node(link) {
            Node::Member { prop, .. } => {
                out.push_str(if opt { "?." } else { "." });
                out.push_str(ast.span(prop).text(text));
            }
            Node::Index { .. } | Node::Call { .. } => {
                if opt {
                    out.push_str("?.");
                }
                let rest = text[obj_end as usize..ast.span(link).end as usize].trim_start();
                // Comments between the object and the bracket: leave those alone.
                if rest.starts_with("/") {
                    return None;
                }
                out.push_str(rest);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn and_operands(ast: &Ast, n: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(n) {
        Node::Binary {
            left,
            op: BinaryOp::LogicalAnd,
            right,
        } => {
            and_operands(ast, left, out);
            out.push(right);
        }
        _ => out.push(n),
    }
}

/// For each of `prefixes`, the index in `chain` it equals (strictly increasing, before
/// `limit`).
fn match_prefixes(
    ast: &Ast,
    text: &str,
    prefixes: &[NodeId],
    chain: &[NodeId],
    limit: usize,
) -> Option<Vec<usize>> {
    let texts: Vec<String> = (0..limit)
        .map(|i| chain_text(ast, text, &chain[..=i], &[]))
        .collect::<Option<_>>()?;
    let mut at = 0usize;
    let mut out = Vec::new();
    for &p in prefixes {
        let pc = access_chain(ast, p)?;
        if pc.iter().any(|&x| matches!(ast.node(x), Node::Call { .. })) {
            return None;
        }
        let pt = chain_text(ast, text, &pc, &[])?;
        let j = (at..limit).find(|&j| texts[j] == pt)?;
        out.push(j);
        at = j + 1;
    }
    Some(out)
}

fn optional_chain(cx: &Ctx<'_, '_>, req: &Request<'_>) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    let is_target = |n: NodeId| {
        is_binary(ast, n, BinaryOp::LogicalAnd) || matches!(ast.node(n), Node::Conditional { .. })
    };
    let (s, e) = trim(text, req.start, req.end);
    if s == e && !req.invoked {
        return None;
    }
    let target = if s < e {
        let n = super::fixes::node_covering(cx, s, e)?;
        std::iter::once(n)
            .chain(parents.ancestors(n))
            .take_while(|&a| !ast.node(a).is_statement() && !is_function(ast, a))
            .find(|&a| is_target(a))?
    } else {
        let mut top = node_inside(ast, req.start)?;
        while let Some(p) = parents.parent(top) {
            let up = match *ast.node(p) {
                Node::Binary { .. } | Node::Conditional { .. } => true,
                Node::Member { .. } => true,
                Node::Index { obj, .. } => obj == top,
                Node::Call { callee, .. } => callee == top,
                _ => false,
            };
            if !up {
                break;
            }
            top = p;
        }
        if !is_target(top) {
            return None;
        }
        top
    };
    let sp = ast.span(target);
    let new_text = match *ast.node(target) {
        Node::Conditional { test, cons, alt } => {
            let chain = access_chain(ast, cons)?;
            if chain.len() < 3 {
                return None;
            }
            let mut ops = Vec::new();
            and_operands(ast, test, &mut ops);
            let last = *ops.last()?;
            if access_chain(ast, last)?.len() < 2 {
                return None;
            }
            let idx = match_prefixes(ast, text, &ops, &chain, chain.len() - 1)?;
            if *idx.last()? != chain.len() - 2 {
                return None;
            }
            // `??` can't take `||`/`&&` unparenthesised, nor anything weaker than itself.
            if precedence(ast, alt) <= PREC_COALESCE {
                return None;
            }
            format!(
                "{} ?? {}",
                chain_text(ast, text, &chain, &idx)?,
                ast.span(alt).text(text)
            )
        }
        _ => {
            let mut ops = Vec::new();
            and_operands(ast, target, &mut ops);
            let (&last, prefixes) = ops.split_last()?;
            let chain = access_chain(ast, last)?;
            if chain.len() < 2 {
                return None;
            }
            let idx = match_prefixes(ast, text, prefixes, &chain, chain.len() - 1)?;
            chain_text(ast, text, &chain, &idx)?
        }
    };
    Some(vec![Edit::new(sp.start, sp.end, new_text)])
}

fn is_function(ast: &Ast, n: NodeId) -> bool {
    matches!(
        ast.node(n),
        Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
    )
}

/// Whether `e` is a property access (through parentheses and type assertions), which as a
/// callee passes its object as `this`.
fn is_member_ref(ast: &Ast, e: NodeId) -> bool {
    match *ast.node(e) {
        Node::Member { .. } | Node::Index { .. } => true,
        Node::Paren { expr }
        | Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::TypeAssertion { expr, .. }
        | Node::NonNull { expr } => is_member_ref(ast, expr),
        _ => false,
    }
}

fn is_loop(ast: &Ast, n: NodeId) -> bool {
    matches!(
        ast.node(n),
        Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
    )
}

/// Whether the value of `e` may be an object it creates.
fn fresh_object(ast: &Ast, e: NodeId) -> bool {
    match *ast.node(e) {
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::RegexLit { .. }
        | Node::New { .. }
        | Node::Arrow { .. }
        | Node::FunctionExpr { .. }
        | Node::ClassExpr { .. } => true,
        Node::Paren { expr }
        | Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::TypeAssertion { expr, .. }
        | Node::NonNull { expr } => fresh_object(ast, expr),
        Node::Conditional { cons, alt, .. } => fresh_object(ast, cons) || fresh_object(ast, alt),
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            right,
        } => fresh_object(ast, left) || fresh_object(ast, right),
        Node::Seq { exprs } => ast
            .list(exprs)
            .last()
            .is_some_and(|&x| fresh_object(ast, x)),
        _ => false,
    }
}

/// The selection without surrounding whitespace.
fn trim(text: &str, start: u32, end: u32) -> (u32, u32) {
    let s = &text[start as usize..end as usize];
    let lead = s.len() - s.trim_start().len();
    let trail = s.len() - s.trim_end().len();
    if lead == s.len() {
        return (start, start);
    }
    (start + lead as u32, end - trail as u32)
}

// ---- Precedence ----

const PREC_COMMA: u8 = 0;
const PREC_ASSIGN: u8 = 2;
const PREC_COALESCE: u8 = 4;
const PREC_UNARY: u8 = 16;
const PREC_POSTFIX: u8 = 17;
const PREC_NEW_NO_ARGS: u8 = 18;
const PREC_MEMBER: u8 = 19;
const PREC_PRIMARY: u8 = 20;

fn binary_prec(op: BinaryOp) -> u8 {
    use BinaryOp::*;
    match op {
        Coalesce => PREC_COALESCE,
        LogicalOr => 5,
        LogicalAnd => 6,
        BitOr => 7,
        BitXor => 8,
        BitAnd => 9,
        Eq | NotEq | StrictEq | StrictNotEq => 10,
        Lt | LtEq | Gt | GtEq | In | InstanceOf => 11,
        Shl | Shr | UShr => 12,
        Add | Sub => 13,
        Mul | Div | Mod => 14,
        Exp => 15,
    }
}

/// How tightly an expression binds (higher is tighter).
fn precedence(ast: &Ast, n: NodeId) -> u8 {
    match *ast.node(n) {
        Node::Seq { .. } => PREC_COMMA,
        Node::Arrow { .. }
        | Node::Yield { .. }
        | Node::Assign { .. }
        | Node::Conditional { .. } => PREC_ASSIGN,
        Node::Binary { op, .. } => binary_prec(op),
        Node::As { .. } | Node::Satisfies { .. } => 11,
        Node::Unary { .. } | Node::Await { .. } | Node::TypeAssertion { .. } => PREC_UNARY,
        Node::Update { prefix: true, .. } => PREC_UNARY,
        Node::Update { .. } => PREC_POSTFIX,
        Node::New {
            has_args: false, ..
        } => PREC_NEW_NO_ARGS,
        Node::Call { .. }
        | Node::New { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::NonNull { .. }
        | Node::TaggedTemplate { .. }
        | Node::Instantiation { .. }
        | Node::MetaProperty { .. }
        | Node::ImportCall { .. } => PREC_MEMBER,
        _ => PREC_PRIMARY,
    }
}

/// Whether `left + (a + b)` equals `left + a + b`: all numbers, or `left` and `a` strings.
fn same_sums(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, left: NodeId, init: NodeId) -> bool {
    let ast = &cx.checker.file(cx.file).ast;
    let Node::Binary { left: a, .. } = *ast.node(init) else {
        return false;
    };
    let mut kind = |n: NodeId| {
        let t = ck.check_expr(Loc::new(cx.file, n));
        let fl = ck.store().flags(t);
        if fl.intersects(TypeFlags::UNION) {
            0
        } else if fl.intersects(TypeFlags::NUMBER_LIKE) {
            1
        } else if fl.intersects(TypeFlags::STRING_LIKE) {
            2
        } else {
            0
        }
    };
    let (l, i, a) = (kind(left), kind(init), kind(a));
    (l == 1 && i == 1) || (l == 2 && a == 2)
}

// ---- Inline variable ----

fn inline_variable(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, req: &Request<'_>) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let b = &f.bound;
    let parents = f.parents();
    let n = super::super::locate::node_at(ast, req.start)?;
    if !matches!(ast.node(n), Node::Ident { .. }) {
        return None;
    }
    // At the declaration's name, or (invoked) a reference.
    let decl = match parents.parent(n).map(|p| (p, ast.node(p))) {
        Some((p, Node::VarDeclarator { name, .. })) if *name == n => p,
        _ if req.invoked => {
            let sym = b.resolved(n)?;
            let &[d] = b.decls(sym) else {
                return None;
            };
            match ast.node(d) {
                Node::VarDeclarator { .. } => d,
                _ => {
                    let p = parents.parent(d)?;
                    match ast.node(p) {
                        Node::VarDeclarator { name, .. } if *name == d => p,
                        _ => return None,
                    }
                }
            }
        }
        _ => return None,
    };
    let Node::VarDeclarator {
        name,
        init: Some(init),
        type_ann,
        ..
    } = *ast.node(decl)
    else {
        return None;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return None;
    }
    // The annotation types literals contextually (`const t: Record<string, number> = {}`);
    // inlined, they would get their own type.
    if type_ann.is_some() && contextually_typed(ast, init) {
        return None;
    }
    let vd = parents.parent(decl)?;
    let Node::VarDecl {
        modifiers, decls, ..
    } = *ast.node(vd)
    else {
        return None;
    };
    if modifiers.contains(Modifiers::EXPORT) || modifiers.contains(Modifiers::DECLARE) {
        return None;
    }
    // A statement of a block or the file (not a `for` head).
    let owner = parents.parent(vd)?;
    if !matches!(
        ast.node(owner),
        Node::Block { .. }
            | Node::Program { .. }
            | Node::ModuleBlock { .. }
            | Node::SwitchCase { .. }
    ) {
        return None;
    }
    let sym = b.symbol_of_decl(decl).or_else(|| b.symbol_of_decl(name))?;
    if b.decls(sym).len() != 1 {
        return None;
    }
    if matches!(
        unparen(ast, init),
        x if matches!(ast.node(x), Node::FunctionExpr { .. } | Node::ClassExpr { .. })
    ) {
        return None;
    }
    let uses: Vec<NodeId> = b
        .refs
        .resolved
        .iter()
        .filter(|&&(r, s)| s == sym && r != name)
        .map(|&(r, _)| r)
        .collect();
    if uses.is_empty() {
        return None;
    }
    let isp = ast.span(init);
    let init_text = isp.text(text);
    let init_fn = enclosing_fn(ast, parents, decl);
    let names = init_names(cx, init);
    // An initializer with effects (`await load()`) runs once, where it is: only a single use
    // in the next statement, reached unconditionally with nothing effectful before it.
    if names.effects {
        let &[u] = uses.as_slice() else {
            return None;
        };
        let stmts = match *ast.node(owner) {
            Node::Block { stmts } | Node::Program { stmts } | Node::ModuleBlock { stmts } => stmts,
            Node::SwitchCase { body, .. } => body,
            _ => return None,
        };
        let list = ast.list(stmts);
        let next = list.get(list.iter().position(|&x| x == vd)? + 1).copied()?;
        if !parents.ancestors(u).any(|a| a == next)
            || parents
                .ancestors(u)
                .take_while(|&a| a != next)
                .any(|a| is_function(ast, a))
            || extract_hazard(cx, &[u], next, true)
            || effects_before(ast, next, ast.span(u).start)
        {
            return None;
        }
    }
    // A fresh object (`{}`, `[]`, `new C()`, `() => 1`) has an identity: inlined, it must
    // still be created once, so only into a single use outside loops and functions.
    if fresh_object(ast, init) {
        let &[u] = uses.as_slice() else {
            return None;
        };
        if parents
            .ancestors(u)
            .take_while(|&a| a != owner)
            .any(|a| is_function(ast, a) || is_loop(ast, a))
        {
            return None;
        }
    }
    let mut edits = Vec::new();
    for &u in &uses {
        let p = parents.parent(u)?;
        if super::refactor::is_write(ast, parents, u)
            || matches!(
                ast.node(p),
                Node::ExportSpecifier { .. } | Node::TypeQuery { .. } | Node::QualifiedName { .. }
            )
        {
            return None;
        }
        // Inside the initializer itself (`const x = () => x`).
        if isp.start <= ast.span(u).start && ast.span(u).end <= isp.end {
            return None;
        }
        if enclosing_fn(ast, parents, u) != init_fn && names.moves_badly {
            return None;
        }
        if !names.same_at(cx, u) {
            return None;
        }
        let usp = ast.span(u);
        if let Node::ShorthandProp {
            name: sn,
            init: None,
            ..
        } = *ast.node(p)
            && sn == u
        {
            let t = if precedence(ast, init) == PREC_COMMA {
                format!("({init_text})")
            } else {
                init_text.to_string()
            };
            edits.push(Edit::new(
                usp.start,
                usp.end,
                format!("{}: {t}", ast.span(u).text(text)),
            ));
            continue;
        }
        if matches!(ast.node(p), Node::ShorthandProp { .. }) {
            return None;
        }
        let paren = needs_parens(cx, ck, init, u)?;
        let t = if paren {
            format!("({init_text})")
        } else {
            init_text.to_string()
        };
        if continues_previous(ast, parents, text, u, &t) {
            return None;
        }
        edits.push(Edit::new(usp.start, usp.end, t));
    }
    // The declaration.
    let list = ast.list(decls);
    if list.len() > 1 {
        let i = list.iter().position(|&d| d == decl)?;
        edits.extend(super::unused::delete_element(ast, list, i));
    } else {
        let sp = ast.span(vd);
        let ls = style::line_start(text, sp.start);
        let after = style::next_line(text, sp.end);
        let rest = &text[sp.end as usize..after as usize];
        let rest_ok = rest.trim().is_empty()
            || (rest.trim_start().starts_with("//")
                && ast
                    .comments_in(Span::new(sp.end, after))
                    .first()
                    .is_some_and(|c| {
                        c.span.end as usize >= after as usize - rest.len() + rest.trim_end().len()
                    }));
        if style::blank(text, ls, sp.start) && rest_ok {
            edits.push(Edit::delete(ls, after));
        } else {
            edits.push(Edit::delete(sp.start, sp.end));
        }
    }
    Some(edits)
}

/// Whether `t` put at `u` would join the statement before it (no semicolons: `f()` then
/// `(await g()).x = 1` reads as `f()(await g())`).
fn continues_previous(ast: &Ast, parents: &ParentMap, text: &str, u: NodeId, t: &str) -> bool {
    if !t.starts_with(['(', '[', '`', '+', '-', '/', '<']) {
        return false;
    }
    let start = ast.span(u).start;
    parents
        .ancestors(u)
        .take_while(|&a| ast.span(a).start == start)
        .any(|a| matches!(ast.node(a), Node::ExprStmt { .. }))
        && text[..start as usize]
            .trim_end()
            .chars()
            .last()
            .is_some_and(|c| !matches!(c, ';' | '{' | '}'))
}

/// Whether something with effects in `stmt` (outside nested functions) ends before `pos`.
fn effects_before(ast: &Ast, stmt: NodeId, pos: u32) -> bool {
    let mut stack = vec![stmt];
    while let Some(x) = stack.pop() {
        let sp = ast.span(x);
        if sp.start >= pos || is_function(ast, x) {
            continue;
        }
        if sp.end <= pos && has_effect(ast, x) {
            return true;
        }
        ast.for_each_child(x, |c| stack.push(c));
    }
    false
}

/// A call, `new`, `await`, `yield`, assignment, `++`/`--`, `delete` or tagged template.
fn has_effect(ast: &Ast, n: NodeId) -> bool {
    matches!(
        ast.node(n),
        Node::Call { .. }
            | Node::New { .. }
            | Node::Await { .. }
            | Node::Yield { .. }
            | Node::Assign { .. }
            | Node::Update { .. }
            | Node::TaggedTemplate { .. }
            | Node::Unary {
                op: UnaryOp::Delete,
                ..
            }
    )
}

/// The innermost non-arrow function around `n` (where `this`, `arguments`, `await`, `yield`
/// mean the same).
fn enclosing_fn(ast: &Ast, parents: &ParentMap, n: NodeId) -> Option<NodeId> {
    parents.ancestors(n).find(|&a| {
        matches!(
            ast.node(a),
            Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
        )
    })
}

/// Names an initializer uses.
struct InitNames {
    /// (identifier text, the local symbol it means or `None` for a free name).
    names: Vec<(String, Option<ts_r_binder::SymbolId>)>,
    /// Uses `this`, `arguments`, `super`, `await`, `yield` or `new.target` outside nested
    /// functions.
    moves_badly: bool,
    /// Evaluating it has effects (a call, `await`, an assignment, ...).
    effects: bool,
}

fn init_names(cx: &Ctx<'_, '_>, init: NodeId) -> InitNames {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let b = &f.bound;
    let mut out = InitNames {
        names: Vec::new(),
        moves_badly: false,
        effects: false,
    };
    let mut stack = vec![init];
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::Ident { name } => {
                let resolved = b.resolved(n);
                let free = b.refs.free.iter().any(|r| r.node == n);
                if resolved.is_some() || free {
                    out.names.push((ast.atom(name).to_string(), resolved));
                }
                if ast.atom(name) == "arguments" && resolved.is_none() {
                    out.moves_badly = true;
                }
            }
            Node::This {}
            | Node::Super {}
            | Node::Await { .. }
            | Node::Yield { .. }
            | Node::MetaProperty { .. } => out.moves_badly = true,
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    // Not in nested functions: they run later, if at all.
    out.effects = effects_before(ast, init, u32::MAX);
    out
}

impl InitNames {
    /// Whether every name of the initializer means the same at `at`.
    fn same_at(&self, cx: &Ctx<'_, '_>, at: NodeId) -> bool {
        let f = cx.checker.file(cx.file);
        let ast = &f.ast;
        let b = &f.bound;
        let parents = f.parents();
        for (name, sym) in &self.names {
            let mut found = None;
            for a in parents.ancestors(at) {
                if let Some(sc) = b.scope_of(a)
                    && let Some(s) = b.lookup_str(ast, b.scopes[sc.0 as usize].locals, name)
                {
                    found = Some(s);
                    break;
                }
            }
            if found.is_none()
                && let Some(s) = b.lookup_str(ast, b.scopes[0].locals, name)
            {
                found = Some(s);
            }
            if found != *sym {
                return false;
            }
        }
        true
    }
}

/// Whether `init` needs parentheses in place of the reference `u`; `None` when the place is
/// one we don't handle.
fn needs_parens(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, init: NodeId, u: NodeId) -> Option<bool> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    let p = parents.parent(u)?;
    let prec = precedence(ast, init);
    let starts_badly = matches!(
        ast.node(init),
        Node::ObjectLit { .. } | Node::FunctionExpr { .. } | Node::ClassExpr { .. }
    );
    Some(match *ast.node(p) {
        Node::Member { obj, .. } | Node::Index { obj, .. } if obj == u => {
            prec < PREC_MEMBER || matches!(ast.node(init), Node::NumberLit { .. }) || starts_badly
        }
        Node::Call { callee, .. } | Node::TaggedTemplate { tag: callee, .. } if callee == u => {
            // `const fn = obj.method; fn()` calls it with no `this`; `obj.method()` would not.
            if is_member_ref(ast, init) {
                return None;
            }
            prec < PREC_MEMBER || starts_badly
        }
        Node::New { callee, .. } if callee == u => prec < PREC_MEMBER || contains_call(ast, init),
        Node::NonNull { .. } => prec < PREC_MEMBER,
        Node::Unary { op, .. } => {
            // `-(-b)`, `+(+b)`, `-(--b)`: not `--b`.
            prec < PREC_UNARY
                || matches!(
                    (op, ast.node(init)),
                    (
                        UnaryOp::Minus,
                        Node::Unary {
                            op: UnaryOp::Minus,
                            ..
                        }
                    ) | (
                        UnaryOp::Plus,
                        Node::Unary {
                            op: UnaryOp::Plus,
                            ..
                        }
                    ) | (
                        UnaryOp::Minus,
                        Node::Update {
                            op: UpdateOp::Decrement,
                            prefix: true,
                            ..
                        }
                    ) | (
                        UnaryOp::Plus,
                        Node::Update {
                            op: UpdateOp::Increment,
                            prefix: true,
                            ..
                        }
                    )
                )
        }
        Node::Await { .. } | Node::TypeAssertion { .. } => prec < PREC_UNARY,
        Node::Update { .. } => return None,
        Node::Binary { left, op, right } => {
            let op_prec = binary_prec(op);
            // `??` doesn't mix with `||`/`&&` unparenthesised.
            let mixes = |o: BinaryOp| {
                matches!(
                    (op, o),
                    (
                        BinaryOp::Coalesce,
                        BinaryOp::LogicalOr | BinaryOp::LogicalAnd
                    ) | (
                        BinaryOp::LogicalOr | BinaryOp::LogicalAnd,
                        BinaryOp::Coalesce
                    )
                )
            };
            let init_op = match *ast.node(init) {
                Node::Binary { op, .. } => Some(op),
                _ => None,
            };
            if init_op.is_some_and(mixes) {
                true
            } else if left == u {
                // `**` is right-associative and takes no unary on its left.
                if op == BinaryOp::Exp {
                    prec <= op_prec
                } else {
                    prec < op_prec
                }
            } else if right == u {
                if prec != op_prec {
                    prec < op_prec
                } else {
                    // Same precedence on the right: only where regrouping keeps the value.
                    let assoc = match op {
                        BinaryOp::Mul
                        | BinaryOp::BitOr
                        | BinaryOp::BitAnd
                        | BinaryOp::BitXor
                        | BinaryOp::LogicalAnd
                        | BinaryOp::LogicalOr
                        | BinaryOp::Coalesce => init_op == Some(op),
                        BinaryOp::Add => {
                            init_op == Some(BinaryOp::Add) && same_sums(cx, ck, left, init)
                        }
                        BinaryOp::Exp => true,
                        _ => false,
                    };
                    !assoc
                }
            } else {
                return None;
            }
        }
        Node::As { expr, .. } | Node::Satisfies { expr, .. } if expr == u => prec < 11,
        Node::Conditional { test, .. } if test == u => prec <= PREC_ASSIGN,
        Node::Conditional { .. }
        | Node::Assign { .. }
        | Node::Spread { .. }
        | Node::Property { .. }
        | Node::ArrayLit { .. }
        | Node::Call { .. }
        | Node::New { .. }
        | Node::VarDeclarator { .. }
        | Node::Return { .. }
        | Node::Throw { .. }
        | Node::TemplateLit { .. }
        | Node::JsxExprContainer { .. }
        | Node::JsxSpreadAttr { .. }
        | Node::Paren { .. }
        | Node::Seq { .. }
        | Node::ComputedKey { .. }
        | Node::Index { .. }
        | Node::If { .. }
        | Node::While { .. }
        | Node::DoWhile { .. }
        | Node::Switch { .. }
        | Node::SwitchCase { .. }
        | Node::ForOf { .. }
        | Node::ForIn { .. }
        | Node::ExportDefaultExpr { .. } => {
            prec == PREC_COMMA && !matches!(ast.node(p), Node::Paren { .. } | Node::Seq { .. })
        }
        Node::ExprStmt { .. } => prec == PREC_COMMA || starts_badly,
        Node::Arrow { body, .. } if body == u => {
            prec == PREC_COMMA || matches!(ast.node(init), Node::ObjectLit { .. })
        }
        _ => return None,
    })
}

/// Whether `n` holds an object or array literal or a function (typed by context).
fn contextually_typed(ast: &Ast, n: NodeId) -> bool {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if matches!(
            ast.node(x),
            Node::ObjectLit { .. }
                | Node::ArrayLit { .. }
                | Node::Arrow { .. }
                | Node::FunctionExpr { .. }
                | Node::ClassExpr { .. }
        ) {
            return true;
        }
        ast.for_each_child(x, |c| stack.push(c));
    }
    false
}

fn contains_call(ast: &Ast, n: NodeId) -> bool {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if matches!(ast.node(x), Node::Call { .. }) {
            return true;
        }
        if is_function(ast, x) {
            continue;
        }
        ast.for_each_child(x, |c| stack.push(c));
    }
    false
}

// ---- Generate 'get' and 'set' accessors ----

fn accessors(cx: &Ctx<'_, '_>, req: &Request<'_>) -> Option<Vec<Edit>> {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let parents = f.parents();
    if !req.invoked && req.start == req.end {
        return None;
    }
    let n = node_inside(ast, req.start)?;
    let member = std::iter::once(n).chain(parents.ancestors(n)).find(|&a| {
        matches!(
            ast.node(a),
            Node::PropertyDef { .. } | Node::Param { .. } | Node::Property { .. }
        ) || ast.node(a).is_statement()
            || is_function(ast, a)
            || matches!(ast.node(a), Node::MethodDef { .. } | Node::ObjectLit { .. })
    })?;
    let unit = style::indent_unit(text);
    let nl = style::newline(text);
    match *ast.node(member) {
        Node::PropertyDef {
            decorators,
            modifiers,
            key,
            optional,
            type_ann,
            ..
        } => {
            if !ast.list(decorators).is_empty()
                || modifiers.intersects(
                    Modifiers::ABSTRACT
                        | Modifiers::DECLARE
                        | Modifiers::OVERRIDE
                        | Modifiers::ACCESSOR,
                )
            {
                return None;
            }
            let class = parents.parent(member)?;
            let (class_name, members) = match *ast.node(class) {
                Node::ClassDecl { name, members, .. } | Node::ClassExpr { name, members, .. } => {
                    (name, members)
                }
                _ => return None,
            };
            let names = Names::new(ast, key)?;
            let (field, accessor) = names.pick(ast, ast.list(members))?;
            let is_static = modifiers.contains(Modifiers::STATIC);
            let receiver = if is_static {
                ast.name_of(class_name?)?.to_string()
            } else {
                "this".into()
            };
            let msp = ast.span(member);
            // The member has its line(s) to itself.
            let ls = style::line_start(text, msp.start);
            if !style::blank(text, ls, msp.start) {
                return None;
            }
            let after = style::next_line(text, msp.end);
            let rest = text[msp.end as usize..after as usize].trim();
            if !(rest.is_empty() || rest.starts_with("//") || rest.starts_with("/*")) {
                return None;
            }
            let ends_semi = text[msp.start as usize..msp.end as usize].ends_with(';');
            let semi = if ends_semi { ";" } else { "" };
            let indent = style::line_indent(text, msp.start);
            let mut edits = head_edits(ast, member, key, &field)?;
            let ty = accessor_type(ast, text, type_ann, optional)?;
            if optional && let Some(t) = type_ann {
                let tsp = ast.span(t);
                if ty != tsp.text(text) {
                    edits.push(Edit::new(tsp.start, tsp.end, ty.clone()));
                }
            }
            let access = access_of(modifiers);
            let stat = if is_static { "static " } else { "" };
            let body = accessor_text(&Accessors {
                indent,
                unit: &unit,
                nl,
                prefix: &format!("{access} {stat}"),
                name: &accessor,
                field: &field,
                receiver: &receiver,
                ty: &ty,
                semi,
                setter: !modifiers.contains(Modifiers::READONLY),
                sep: "",
            });
            edits.push(Edit::insert(after, body));
            Some(edits)
        }
        Node::Param {
            decorators,
            modifiers,
            pattern,
            type_ann,
            optional,
            ..
        } => {
            if !ast.list(decorators).is_empty()
                || !modifiers
                    .intersects(Modifiers::PUBLIC | Modifiers::PRIVATE | Modifiers::PROTECTED)
                || modifiers.contains(Modifiers::OVERRIDE)
                || !matches!(ast.node(pattern), Node::Ident { .. })
            {
                return None;
            }
            // constructor(...) is the `FunctionExpr` value of a `MethodDef`.
            let ctor = parents.parent(parents.parent(member)?)?;
            if !matches!(ast.node(ctor), Node::MethodDef { .. }) {
                return None;
            }
            let class = parents.parent(ctor)?;
            let members = match *ast.node(class) {
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
                _ => return None,
            };
            let names = Names::new(ast, pattern)?;
            let (field, accessor) = names.pick(ast, ast.list(members))?;
            // Before the class's first member.
            let first = *ast.list(members).first()?;
            let fsp = ast.span(first);
            let ls = style::line_start(text, fsp.start);
            if !style::blank(text, ls, fsp.start) {
                return None;
            }
            let indent = style::line_indent(text, fsp.start);
            let semi = style::semi(ast, text);
            let mut edits = head_edits(ast, member, pattern, &field)?;
            let ty = accessor_type(ast, text, type_ann, optional)?;
            let body = accessor_text(&Accessors {
                indent,
                unit: &unit,
                nl,
                prefix: &format!("{} ", access_of(modifiers)),
                name: &accessor,
                field: &field,
                receiver: "this",
                ty: &ty,
                semi,
                setter: !modifiers.contains(Modifiers::READONLY),
                sep: "",
            });
            edits.push(Edit::insert(ls, body));
            Some(edits)
        }
        Node::Property { modifiers, key, .. } => {
            if !modifiers.is_empty() || !matches!(ast.node(key), Node::Ident { .. }) {
                return None;
            }
            let obj = parents.parent(member)?;
            let Node::ObjectLit { props } = *ast.node(obj) else {
                return None;
            };
            // Only an object without a contextual type (`const o = {...}`): a type would not
            // know `_x`.
            match parents.parent(obj).map(|p| ast.node(p)) {
                Some(Node::VarDeclarator {
                    type_ann: None,
                    init: Some(i),
                    ..
                }) if *i == obj => {}
                _ => return None,
            }
            let names = Names::new(ast, key)?;
            let (field, accessor) = names.pick(ast, ast.list(props))?;
            let msp = ast.span(member);
            let ls = style::line_start(text, msp.start);
            if !style::blank(text, ls, msp.start) {
                return None;
            }
            let indent = style::line_indent(text, msp.start);
            // After the property and its comma, at the end of its line.
            let after_prop = text[msp.end as usize..].trim_start_matches([' ', '\t']);
            let has_comma = after_prop.starts_with(',');
            let line_end = style::next_line(text, msp.end);
            let rest = text[msp.end as usize..line_end as usize].trim();
            let rest = rest.strip_prefix(',').unwrap_or(rest).trim();
            if !rest.is_empty() {
                return None;
            }
            let semi = style::semi(ast, text);
            let ksp = ast.span(key);
            let mut edits = vec![Edit::new(ksp.start, ksp.end, field.clone())];
            if !has_comma {
                edits.push(Edit::insert(msp.end, ","));
            }
            let body = accessor_text(&Accessors {
                indent,
                unit: &unit,
                nl,
                prefix: "",
                name: &accessor,
                field: &field,
                receiver: "this",
                ty: "",
                semi,
                setter: true,
                sep: ",",
            });
            edits.push(Edit::insert(line_end, body));
            Some(edits)
        }
        _ => None,
    }
}

/// The field's and the accessors' names.
struct Names {
    base: String,
    underscored: bool,
}

impl Names {
    fn new(ast: &Ast, key: NodeId) -> Option<Names> {
        let Node::Ident { name } = *ast.node(key) else {
            return None;
        };
        let s = ast.atom(name);
        match s.strip_prefix('_') {
            Some(rest) if !rest.is_empty() => Some(Names {
                base: rest.to_string(),
                underscored: true,
            }),
            _ => Some(Names {
                base: s.to_string(),
                underscored: false,
            }),
        }
    }

    /// (field name, accessor name), unique among `members`' names.
    fn pick(&self, ast: &Ast, members: &[NodeId]) -> Option<(String, String)> {
        let taken: Vec<&str> = members
            .iter()
            .filter_map(|&m| match *ast.node(m) {
                Node::PropertyDef { key, .. }
                | Node::MethodDef { key, .. }
                | Node::Property { key, .. } => ast.name_of(key),
                Node::ShorthandProp { name, .. } => ast.name_of(name),
                _ => None,
            })
            .collect();
        let unique = |base: String| -> String {
            if !taken.contains(&base.as_str()) {
                return base;
            }
            (1..)
                .map(|i| format!("{base}_{i}"))
                .find(|n| !taken.contains(&n.as_str()))
                .unwrap_or(base)
        };
        if self.underscored {
            Some((format!("_{}", self.base), unique(self.base.clone())))
        } else {
            Some((unique(format!("_{}", self.base)), self.base.clone()))
        }
    }
}

/// The accessors' accessibility: `protected` stays, anything else is `public`.
fn access_of(m: Modifiers) -> &'static str {
    if m.contains(Modifiers::PROTECTED) {
        "protected"
    } else {
        "public"
    }
}

/// Edits of the field's head: `private` in place of its accessibility, the new name.
fn head_edits(ast: &Ast, member: NodeId, key: NodeId, field: &str) -> Option<Vec<Edit>> {
    let mut edits = Vec::new();
    let toks = ast.modifier_tokens_of(member);
    let access = toks.iter().find(|t| {
        t.flag
            .intersects(Modifiers::PUBLIC | Modifiers::PRIVATE | Modifiers::PROTECTED)
    });
    let ksp = ast.span(key);
    match access {
        Some(t) if t.flag.contains(Modifiers::PRIVATE) => {}
        Some(t) => edits.push(Edit::new(t.span.start, t.span.end, "private")),
        None => {
            let at = toks.first().map_or(ksp.start, |t| t.span.start);
            edits.push(Edit::insert(at, "private "));
        }
    }
    if ast.name_of(key)? != field {
        edits.push(Edit::new(ksp.start, ksp.end, field.to_string()));
    }
    Some(edits)
}

/// The accessors' type: the annotation, with `| undefined` for an optional member.
fn accessor_type(ast: &Ast, text: &str, ann: Option<NodeId>, optional: bool) -> Option<String> {
    let Some(t) = ann else {
        return Some(String::new());
    };
    let src = ast.span(t).text(text);
    if !optional {
        return Some(src.to_string());
    }
    let has_undefined = |n: NodeId| {
        matches!(
            ast.node(n),
            Node::KeywordType {
                kind: ts_r_ast::KeywordTypeKind::Undefined
            }
        )
    };
    match *ast.node(t) {
        Node::UnionType { types } if ast.list(types).iter().any(|&x| has_undefined(x)) => {
            Some(src.to_string())
        }
        _ if has_undefined(t) => Some(src.to_string()),
        Node::FunctionType { .. } | Node::ConstructorType { .. } | Node::ConditionalType { .. } => {
            Some(format!("({src}) | undefined"))
        }
        _ => Some(format!("{src} | undefined")),
    }
}

struct Accessors<'a> {
    indent: &'a str,
    unit: &'a str,
    nl: &'a str,
    /// Modifiers with a trailing space (`public static `), or nothing.
    prefix: &'a str,
    name: &'a str,
    field: &'a str,
    receiver: &'a str,
    /// The type (empty: none).
    ty: &'a str,
    semi: &'a str,
    setter: bool,
    /// After each accessor (`,` in object literals).
    sep: &'a str,
}

/// `set x(value) {...}` (unless read-only) and `get x() {...}`, each on its own lines.
fn accessor_text(a: &Accessors<'_>) -> String {
    let Accessors {
        indent,
        unit,
        nl,
        prefix,
        name,
        field,
        receiver,
        ty,
        semi,
        setter,
        sep,
    } = *a;
    let ann = if ty.is_empty() {
        String::new()
    } else {
        format!(": {ty}")
    };
    let mut out = String::new();
    if setter {
        out.push_str(&format!(
            "{indent}{prefix}set {name}(value{ann}) {{{nl}{indent}{unit}{receiver}.{field} = value{semi}{nl}{indent}}}{sep}{nl}"
        ));
    }
    out.push_str(&format!(
        "{indent}{prefix}get {name}(){ann} {{{nl}{indent}{unit}return {receiver}.{field}{semi}{nl}{indent}}}{sep}{nl}"
    ));
    out
}

// ---- Extract: what moving code out of place breaks ----

/// Whether extracting `roots` (an expression, or statements of one block) out of its place
/// could change what the code does or how it type-checks: with `eager` (a constant evaluated
/// before the statement `stop`), an operand that only runs after a short-circuit or in a
/// branch; and a condition whose narrowing the code after it relies on (`a && a.b`:
/// extracting `a`). tsserver extracts these anyway; its results then fail to type-check or
/// evaluate too early.
pub(super) fn extract_hazard(
    cx: &Ctx<'_, '_>,
    roots: &[NodeId],
    stop: NodeId,
    eager: bool,
) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    let Some(&first) = roots.first() else {
        return false;
    };
    let mut names: Vec<&str> = Vec::new();
    for &r in roots {
        let mut stack = vec![r];
        while let Some(n) = stack.pop() {
            if let Node::Ident { name } = *ast.node(n) {
                names.push(ast.atom(name));
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
    }
    let shares = |other: NodeId| {
        let mut stack = vec![other];
        while let Some(n) = stack.pop() {
            if let Node::Ident { name } = *ast.node(n)
                && names.contains(&ast.atom(name))
            {
                return true;
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        false
    };
    let mut child = first;
    for p in parents.ancestors(first) {
        if p == stop {
            break;
        }
        match *ast.node(p) {
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right,
            } => {
                if (eager && right == child) || (left == child && shares(right)) {
                    return true;
                }
            }
            Node::Conditional { test, cons, alt } => {
                if (eager && child != test) || (child == test && (shares(cons) || shares(alt))) {
                    return true;
                }
            }
            Node::If { test, cons, alt } => {
                if (eager && child != test)
                    || (child == test && (shares(cons) || alt.is_some_and(&shares)))
                {
                    return true;
                }
            }
            Node::While { test, body } | Node::DoWhile { body, test } => {
                if eager || (child == test && shares(body)) {
                    return true;
                }
            }
            Node::For { .. } | Node::ForIn { .. } | Node::ForOf { .. } | Node::Switch { .. }
                if eager =>
            {
                return true;
            }
            _ => {}
        }
        child = p;
    }
    false
}

/// Whether `roots` read a property access narrowed where they are (`u.p && u.p.x`): moved
/// into a new function, `u.p` would have its declared type again.
pub(super) fn narrowed_access(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, roots: &[NodeId]) -> bool {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let parents = f.parents();
    let mut accesses = Vec::new();
    for &r in roots {
        let mut stack = vec![r];
        while let Some(n) = stack.pop() {
            if is_function(ast, n) {
                continue;
            }
            if matches!(ast.node(n), Node::Member { .. } | Node::Index { .. })
                && !super::refactor::is_write(ast, parents, n)
            {
                accesses.push(n);
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
    }
    // The accesses on the extracted expression (`a` of a narrowed `a.b.c`).
    if let &[r] = roots {
        let mut child = r;
        for p in parents.ancestors(r) {
            match *ast.node(p) {
                Node::Member { obj, .. } | Node::Index { obj, .. } if obj == child => {
                    accesses.push(p)
                }
                _ => break,
            }
            child = p;
        }
    }
    for n in accesses {
        let loc = Loc::new(cx.file, n);
        let Some(sym) = ck.property_symbol_of_access(loc) else {
            continue;
        };
        let declared = ck.type_of_symbol(sym);
        let here = ck.check_expr(loc);
        if declared != here {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outermost `+` of `src`.
    fn top_add(ast: &Ast) -> NodeId {
        (1..=ast.node_count() as u32)
            .filter_map(NodeId::from_raw)
            .filter(|&n| is_binary(ast, n, BinaryOp::Add))
            .max_by_key(|&n| ast.span(n).end - ast.span(n).start)
            .unwrap()
    }

    #[test]
    fn concat_operands_split_from_the_first_string() {
        let src = "const s = a + b + 'x' + c + (d + e);\n";
        let ast = ts_r_parser::parse(src, ts_r_ast::SourceKind::Ts).ast;
        let mut ops = Vec::new();
        concat_operands(&ast, top_add(&ast), &mut ops);
        let texts: Vec<&str> = ops.iter().map(|&o| ast.span(o).text(src)).collect();
        assert_eq!(texts, ["a + b", "'x'", "c", "(d + e)"]);
    }

    /// 20,000 operands: once per prefix this was 200 million node visits.
    #[test]
    fn long_concatenations_flatten_in_linear_time() {
        let src = format!("const s = 'x'{};\n", " + a".repeat(20_000));
        let ast = ts_r_parser::parse(&src, ts_r_ast::SourceKind::Ts).ast;
        let top = top_add(&ast);
        let t = std::time::Instant::now();
        assert!(has_string(&ast, top));
        let mut ops = Vec::new();
        concat_operands(&ast, top, &mut ops);
        assert_eq!(ops.len(), 20_001);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            t.elapsed()
        );
    }
}
