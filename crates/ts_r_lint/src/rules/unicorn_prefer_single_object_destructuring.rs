//! `unicorn/prefer-single-object-destructuring`: in a statement list (the program, blocks,
//! static blocks and `case` bodies), two adjacent statements that should be one.
//!
//! - Merge: two single-declarator `const` (or two `let`) declarations destructuring a
//!   simple object pattern (plain, non-computed keys bound to plain names) from the same
//!   `const` variable, with no key in both. Reported on the second; the fix writes
//!   `kind {first props, second props} = source;`.
//! - Inline: `const x = init;` (a plain name, no type) followed by such a declaration from
//!   `x`, when `x` is read nowhere else and `init` (inside TypeScript wrappers) is a name,
//!   member access, call, `new`, tagged template or `await`. Reported on `x`; the fix
//!   writes `kind pattern = init;`.
//!
//! Neither fix is offered when it would remove a comment.

use super::prelude::*;
use super::unicorn_static::skip_ts_only;
use super::unicorn_text::{paren_span, would_remove_comments};
use ts_r_ast::{Ast, VarKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-single-object-destructuring",
        "Prefer a single object destructuring declaration per local const source.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferSingleObjectDestructuring>,
};

#[derive(Debug, Default)]
struct PreferSingleObjectDestructuring;

/// A single-declarator `const`/`let` declaration (not `declare`): its kind and declarator.
fn single_declarator(ast: &Ast, stmt: NodeId) -> Option<(VarKind, NodeId)> {
    let Node::VarDecl {
        modifiers,
        kind,
        decls,
    } = *ast.node(stmt)
    else {
        return None;
    };
    if modifiers.intersects(Modifiers::DECLARE | Modifiers::EXPORT)
        || !matches!(kind, VarKind::Const | VarKind::Let)
    {
        return None;
    }
    match ast.list(decls) {
        &[d] => Some((kind, d)),
        _ => None,
    }
}

/// The key names of a simple object pattern (every property `key` or `key: name`), or
/// `None` when the pattern is not simple.
fn simple_pattern_keys(cx: &Ctx<'_, '_>, pattern: NodeId) -> Option<Vec<String>> {
    let ast = cx.ast();
    let Node::ObjectPattern { props } = *ast.node(pattern) else {
        return None;
    };
    let props = ast.list(props);
    if props.is_empty() {
        return None;
    }
    let mut keys = Vec::with_capacity(props.len());
    for &p in props {
        let Node::BindingProperty {
            key,
            value,
            init: None,
        } = *ast.node(p)
        else {
            return None;
        };
        if value.is_some_and(|v| !matches!(ast.node(v), Node::Ident { .. })) {
            return None;
        }
        keys.push(match *ast.node(key) {
            Node::Ident { name } => ast.atom(name).to_owned(),
            Node::StringLit { value } => ast.atom(value).to_owned(),
            Node::ComputedKey { .. } => return None,
            _ => cx.text(key).to_owned(),
        });
    }
    Some(keys)
}

/// `init` is a name bound once by a `const` declarator (not `declare`).
fn is_const_reference(cx: &Ctx<'_, '_>, init: NodeId) -> Option<SymbolId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(init)?;
    let [decl] = sem.symbol(s).decls.as_slice() else {
        return None;
    };
    if decl.kind != DeclKind::Const || ast.kind(decl.node) != NodeKind::VarDeclarator {
        return None;
    }
    let stmt = cx.parent(decl.node)?;
    match *ast.node(stmt) {
        Node::VarDecl { modifiers, .. } if !modifiers.contains(Modifiers::DECLARE) => Some(s),
        _ => None,
    }
}

struct Supported {
    kind: VarKind,
    pattern: NodeId,
    init: NodeId,
    symbol: SymbolId,
    keys: Vec<String>,
}

fn supported_declaration(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<Supported> {
    let ast = cx.ast();
    let (kind, declarator) = single_declarator(ast, stmt)?;
    let Node::VarDeclarator {
        name,
        type_ann: None,
        init: Some(init),
        ..
    } = *ast.node(declarator)
    else {
        return None;
    };
    let keys = simple_pattern_keys(cx, name)?;
    let init = skip_parens(ast, init);
    if !matches!(ast.node(init), Node::Ident { .. }) {
        return None;
    }
    let symbol = is_const_reference(cx, init)?;
    Some(Supported {
        kind,
        pattern: name,
        init,
        symbol,
        keys,
    })
}

fn kind_word(kind: VarKind) -> &'static str {
    if kind == VarKind::Const {
        "const"
    } else {
        "let"
    }
}

fn inline_problem(cx: &mut Ctx<'_, '_>, first: NodeId, second: NodeId) -> bool {
    let ast = cx.ast();
    let Some((VarKind::Const, declarator)) = single_declarator(ast, first) else {
        return false;
    };
    let Node::VarDeclarator {
        name,
        type_ann: None,
        init: Some(init),
        ..
    } = *ast.node(declarator)
    else {
        return false;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return false;
    }
    let Some(sup) = supported_declaration(cx, second) else {
        return false;
    };
    let sem = cx.semantic();
    let sym = sem.symbol(sup.symbol);
    if sym.decls.first().map(|d| d.node) != Some(declarator)
        || sym.refs.iter().any(|&r| {
            let r = sem.reference(r);
            !r.flags.contains(RefFlags::INIT) && r.node != sup.init
        })
    {
        return false;
    }
    let mut inner = init;
    for _ in 0..64 {
        let next = skip_ts_only(ast, skip_parens(ast, inner));
        if next == inner {
            break;
        }
        inner = next;
    }
    if !matches!(
        ast.kind(inner),
        NodeKind::Ident
            | NodeKind::Member
            | NodeKind::Index
            | NodeKind::Call
            | NodeKind::New
            | NodeKind::TaggedTemplate
            | NodeKind::Await
    ) {
        return false;
    }
    let range = Span::new(cx.span(first).start, cx.span(second).end);
    let init_span = paren_span(cx, init);
    let fix = (!would_remove_comments(cx, range, &[])).then(|| {
        Fix::replace(
            range,
            format!(
                "{} {} = {};",
                kind_word(sup.kind),
                cx.text(sup.pattern),
                &cx.src()[init_span.start as usize..init_span.end as usize]
            ),
        )
    });
    let msg = format!(
        "Prefer destructuring directly from the initializer of `{}`.",
        cx.text(name)
    );
    cx.report_node(name, msg)
        .id("prefer-direct-object-destructuring")
        .fix_opt(fix);
    true
}

fn merge_problem(cx: &mut Ctx<'_, '_>, first: NodeId, second: NodeId) {
    let ast = cx.ast();
    let (Some(a), Some(b)) = (
        supported_declaration(cx, first),
        supported_declaration(cx, second),
    ) else {
        return;
    };
    let source = cx.text(a.init);
    if a.kind != b.kind || source != cx.text(b.init) {
        return;
    }
    if b.keys.iter().any(|k| a.keys.contains(k)) {
        return;
    }
    let props_text = |s: &Supported| {
        let Node::ObjectPattern { props } = *ast.node(s.pattern) else {
            return String::new();
        };
        ast.list(props)
            .iter()
            .map(|&p| cx.text(p))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let range = Span::new(cx.span(first).start, cx.span(second).end);
    let fix = (!would_remove_comments(cx, range, &[])).then(|| {
        Fix::replace(
            range,
            format!(
                "{} {{{}, {}}} = {source};",
                kind_word(a.kind),
                props_text(&a),
                props_text(&b)
            ),
        )
    });
    let msg = format!("Prefer a single object destructuring declaration from `{source}`.");
    cx.report_node(second, msg)
        .id("prefer-single-object-destructuring")
        .fix_opt(fix);
}

impl Rule for PreferSingleObjectDestructuring {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Program,
            NodeKind::Block,
            NodeKind::StaticBlock,
            NodeKind::SwitchCase,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let stmts = match *cx.node(node) {
            Node::Program { stmts } | Node::Block { stmts } => stmts,
            Node::StaticBlock { body } | Node::SwitchCase { body, .. } => body,
            _ => return,
        };
        let stmts = ast.list(stmts);
        for pair in stmts.windows(2) {
            if !inline_problem(cx, pair[0], pair[1]) {
                merge_problem(cx, pair[0], pair[1]);
            }
        }
    }
}
