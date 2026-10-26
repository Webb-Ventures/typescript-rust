//! `unicorn/prefer-set-has`: an array only used for `.includes()` lookups should be a `Set`.
//!
//! A plain declarator `name = init` (not exported) whose `init` builds an array: `[...]`,
//! `Array(..)` / `new Array(..)`, `Array.from/of(..)`, a call of an array-returning method
//! (`map`, `filter`, `split`, `toArray`, ...), or `slice`/`concat` on a receiver proven to
//! be one (directly or through `const` initializers). With `minimumItems`, the array's
//! length must be statically known and at least that.
//!
//! Every other use of the binding must be `name.includes(x)` (one argument, no optional
//! chaining), and, only when the init is a literal of unique, side-effect-free primitive
//! values (no holes, spreads or `-0`), `name.length` reads, `for (.. of name)`, `...name`
//! in an array or argument list, or `name.forEach(x => ..)` with a one-parameter arrow.
//! At least one `includes` is needed; a single one only counts when a loop or function
//! lies between it and the declaration's statement list.
//!
//! The fix wraps the init in `new Set(..)`, renames `includes` to `has` and `length` to
//! `size`, and turns a `T[]` / `readonly T[]` / `Array<T>` / `ReadonlyArray<T>` annotation
//! into `Set<T>` / `ReadonlySet<T>`; any other annotation (or one holding comments) makes
//! it a suggestion.

use super::prelude::*;
use super::unicorn_ast::is_left_hand_side;
use super::unicorn_ast::method_call;
use super::unicorn_no_array_reduce::const_initializer as const_init;
use super::unicorn_static::{Static, static_string_literal, static_value_no_side_effects};
use super::unicorn_text::would_remove_comments;
use super::unicorn_util::estree_ident_span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-set-has",
        "Prefer `Set#has()` over `Array#includes()` when checking for existence or non-existence.",
        NO_PRESET,
        Category::Pedantic,
    )
    .fixable()
    .suggestions(),
    create: create::<PreferSetHas>,
};

#[derive(Debug)]
struct PreferSetHas {
    minimum_items: f64,
}

impl FromOptions for PreferSetHas {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(PreferSetHas {
            minimum_items: o.get("minimumItems").and_then(Value::as_f64).unwrap_or(0.0),
        })
    }
}

const ARRAY_METHODS: &[&str] = &[
    "copyWithin",
    "fill",
    "filter",
    "flat",
    "flatMap",
    "map",
    "reverse",
    "sort",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "with",
    "split",
    "toArray",
];

const MAX_ARRAY_LENGTH: f64 = 4_294_967_295.0;

fn is_array_length(v: f64) -> bool {
    v.fract() == 0.0 && (0.0..=MAX_ARRAY_LENGTH).contains(&v)
}

/// `Array(..)` or `new Array(..)` (callee the identifier `Array`, not an optional call).
fn is_array_ctor_call(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&[NodeId]> {
    match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            args,
            ..
        }
        | Node::New { callee, args, .. }
            if ident_name(ast, callee) == Some("Array") =>
        {
            Some(ast.list(args))
        }
        _ => None,
    }
}

fn plain_method<'a>(
    ast: &'a ts_r_ast::Ast,
    id: NodeId,
    names: &[&str],
) -> Option<super::unicorn_ast::MethodCall<'a>> {
    let call = method_call(ast, id)?;
    (call.plain() && names.contains(&call.name)).then_some(call)
}

fn is_array_static_call(ast: &ts_r_ast::Ast, id: NodeId, names: &[&str]) -> Option<&'static str> {
    let call = plain_method(ast, id, names)?;
    (ident_name(ast, skip_parens(ast, call.obj)) == Some("Array"))
        .then(|| if call.name == "from" { "from" } else { "of" })
}

fn is_array_method_call(cx: &Ctx<'_, '_>, id: NodeId, seen: &mut Vec<NodeId>) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    if matches!(ast.node(id), Node::ArrayLit { .. })
        || is_array_ctor_call(ast, id).is_some()
        || is_array_static_call(ast, id, &["from", "of"]).is_some()
        || plain_method(ast, id, ARRAY_METHODS).is_some()
    {
        return true;
    }
    let Some(call) = plain_method(ast, id, &["slice", "concat"]) else {
        return false;
    };
    // The receiver must be proven to be an array (a string has these methods too).
    let recv = super::unicorn_util::skip_wrappers(ast, call.obj);
    if matches!(ast.node(recv), Node::Ident { .. }) {
        let Some(init) = const_init(cx, recv) else {
            return false;
        };
        if seen.contains(&init) || seen.len() > 100 {
            return false;
        }
        seen.push(init);
        is_array_method_call(cx, init, seen)
    } else {
        is_array_method_call(cx, recv, seen)
    }
}

fn static_length(cx: &Ctx<'_, '_>, id: NodeId) -> Option<f64> {
    match static_value_no_side_effects(cx, id)? {
        Static::Num(v) if is_array_length(v) => Some(v),
        _ => None,
    }
}

fn has_spread(ast: &ts_r_ast::Ast, list: &[NodeId]) -> bool {
    list.iter()
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

fn known_array_size(cx: &Ctx<'_, '_>, init: NodeId) -> Option<f64> {
    let ast = cx.ast();
    let init = skip_parens(ast, init);
    if let Node::ArrayLit { elements } = *ast.node(init) {
        let list = ast.list(elements);
        return (!has_spread(ast, list)).then_some(list.len() as f64);
    }
    if let Some(args) = is_array_ctor_call(ast, init) {
        if has_spread(ast, args) {
            return None;
        }
        if args.len() != 1 {
            return Some(args.len() as f64);
        }
        return match static_value_no_side_effects(cx, args[0])? {
            Static::Num(v) => is_array_length(v).then_some(v),
            _ => Some(1.0),
        };
    }
    match is_array_static_call(ast, init, &["from", "of"]) {
        Some("of") => {
            let args = method_call(ast, init)?.args;
            (!has_spread(ast, args)).then_some(args.len() as f64)
        }
        Some(_) => {
            let args = method_call(ast, init)?.args;
            let &source = args.first()?;
            let source = skip_parens(ast, source);
            match *ast.node(source) {
                Node::Spread { .. } => None,
                Node::ArrayLit { elements } => {
                    let list = ast.list(elements);
                    (!has_spread(ast, list)).then_some(list.len() as f64)
                }
                Node::ObjectLit { props } => {
                    let &[p] = ast.list(props) else {
                        return None;
                    };
                    let Node::Property { key, value, .. } = *ast.node(p) else {
                        return None;
                    };
                    let is_length = match ast.node(key) {
                        Node::Ident { .. } => ident_name(ast, key) == Some("length"),
                        Node::StringLit { value } => ast.atom(*value) == "length",
                        _ => false,
                    };
                    if is_length {
                        static_length(cx, value)
                    } else {
                        None
                    }
                }
                _ => static_string_literal(ast, source).map(|s| s.chars().count() as f64),
            }
        }
        None => None,
    }
}

fn is_comparable(v: &Static) -> bool {
    matches!(
        v,
        Static::Undefined
            | Static::Null
            | Static::Bool(_)
            | Static::Num(_)
            | Static::Str(_)
            | Static::BigInt(_)
    )
}

fn same_value_zero(a: &Static, b: &Static) -> bool {
    match (a, b) {
        (Static::Num(x), Static::Num(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

fn is_known_unique_array(cx: &Ctx<'_, '_>, init: NodeId) -> bool {
    let ast = cx.ast();
    let Node::ArrayLit { elements } = *ast.node(skip_parens(ast, init)) else {
        return false;
    };
    let mut values: Vec<Static> = Vec::new();
    for &e in ast.list(elements) {
        if matches!(ast.node(e), Node::Spread { .. } | Node::Hole {}) {
            return false;
        }
        let Some(v) = static_value_no_side_effects(cx, e) else {
            return false;
        };
        if !is_comparable(&v) || matches!(v, Static::Num(n) if n == 0.0 && n.is_sign_negative()) {
            return false;
        }
        if values.iter().any(|p| same_value_zero(p, &v)) {
            return false;
        }
        values.push(v);
    }
    true
}

enum Use {
    Includes(NodeId),
    Length(NodeId),
    Extra,
}

fn classify(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Use> {
    let ast = cx.ast();
    let parent = parent_skip_parens(ast, cx.file.parents, ident)?;
    match *ast.node(parent) {
        Node::Member {
            obj,
            prop,
            optional,
        } if skip_parens(ast, obj) == ident && matches!(ast.node(prop), Node::Ident { .. }) => {
            let name = ident_name(ast, prop)?;
            let grand = parent_skip_parens(ast, cx.file.parents, parent);
            let call = grand.and_then(|g| {
                let Node::Call { callee, .. } = *ast.node(g) else {
                    return None;
                };
                (skip_parens(ast, callee) == parent).then_some(g)
            });
            if name == "includes"
                && !optional
                && let Some(c) = call
                && let Some(m) = method_call(ast, c)
                && m.plain()
                && m.args.len() == 1
                && !has_spread(ast, m.args)
            {
                return Some(Use::Includes(prop));
            }
            if name == "length" && !optional && !is_left_hand_side(cx, parent) && call.is_none() {
                return Some(Use::Length(prop));
            }
            if name == "forEach"
                && !optional
                && let Some(c) = call
                && let Some(m) = method_call(ast, c)
                && m.plain()
                && m.args.len() == 1
                && !has_spread(ast, m.args)
                && let Node::Arrow { params, .. } = *ast.node(skip_parens(ast, m.args[0]))
                && let &[p] = ast.list(params)
                && !matches!(*ast.node(p), Node::Param { rest: true, .. })
            {
                return Some(Use::Extra);
            }
            None
        }
        Node::ForOf { right, .. } if skip_parens(ast, right) == ident => Some(Use::Extra),
        Node::Spread { .. } => {
            let g = cx.parent(parent)?;
            matches!(
                ast.node(g),
                Node::ArrayLit { .. } | Node::Call { .. } | Node::New { .. }
            )
            .then_some(Use::Extra)
        }
        _ => None,
    }
}

/// A loop or function lies between the `includes` call and `root`.
fn is_multiple_call(cx: &Ctx<'_, '_>, call_part: NodeId, root: Option<NodeId>) -> bool {
    let mut cur = cx.parent(call_part);
    for _ in 0..100_000 {
        let Some(n) = cur else {
            return false;
        };
        if Some(n) == root {
            return false;
        }
        if matches!(
            cx.node(n),
            Node::ForOf { .. }
                | Node::For { .. }
                | Node::ForIn { .. }
                | Node::While { .. }
                | Node::DoWhile { .. }
                | Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
        ) {
            return true;
        }
        cur = cx.parent(n);
    }
    false
}

fn skip_paren_types(ast: &ts_r_ast::Ast, mut id: NodeId) -> NodeId {
    while let Node::ParenType { ty } = *ast.node(id) {
        id = ty;
    }
    id
}

fn set_annotation_text(cx: &Ctx<'_, '_>, ty: NodeId) -> Option<String> {
    let ast = cx.ast();
    let ty_span = cx.span(ty);
    match *ast.node(ty) {
        Node::ArrayType { elem } => {
            let elem = skip_paren_types(ast, elem);
            (!would_remove_comments(cx, ty_span, &[cx.span(elem)]))
                .then(|| format!("Set<{}>", cx.text(elem)))
        }
        Node::TypeOperator {
            op: ts_r_ast::TypeOperatorKind::Readonly,
            ty: inner,
        } => {
            let Node::ArrayType { elem } = *ast.node(inner) else {
                return None;
            };
            let elem = skip_paren_types(ast, elem);
            (!would_remove_comments(cx, ty_span, &[cx.span(elem)]))
                .then(|| format!("ReadonlySet<{}>", cx.text(elem)))
        }
        Node::TypeRef {
            name,
            type_args: Some(args),
        } if matches!(ast.node(name), Node::Ident { .. }) => {
            let Node::TypeArgs { args: list } = *ast.node(args) else {
                return None;
            };
            if ast.list(list).len() != 1 || would_remove_comments(cx, ty_span, &[cx.span(args)]) {
                return None;
            }
            match ident_name(ast, name)? {
                "Array" => Some(format!("Set{}", cx.text(args))),
                "ReadonlyArray" => Some(format!("ReadonlySet{}", cx.text(args))),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Rule for PreferSetHas {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            type_ann,
            init: Some(init),
            ..
        } = *cx.node(node)
        else {
            return;
        };
        if !matches!(ast.node(name), Node::Ident { .. }) {
            return;
        }
        let Some(decl) = cx.parent(node) else {
            return;
        };
        let Node::VarDecl { modifiers, .. } = *ast.node(decl) else {
            return;
        };
        if modifiers.contains(Modifiers::EXPORT)
            || cx
                .parent(decl)
                .is_some_and(|p| matches!(ast.node(p), Node::ExportNamed { .. }))
        {
            return;
        }
        if !is_array_method_call(cx, init, &mut Vec::new()) {
            return;
        }
        if self.minimum_items > 0.0 {
            match known_array_size(cx, init) {
                Some(n) if n >= self.minimum_items => {}
                _ => return,
            }
        }
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_decl(name) else {
            return;
        };
        let symbol = sem.symbol(sym);
        let mut idents: Vec<NodeId> = symbol.decls.iter().map(|d| d.name).collect();
        for &r in &symbol.refs {
            let n = sem.reference(r).node;
            if !idents.contains(&n) {
                idents.push(n);
            }
        }
        idents.retain(|&i| i != name);
        let mut includes = Vec::new();
        let mut lengths = Vec::new();
        let mut extra = 0usize;
        for &i in &idents {
            match classify(cx, i) {
                Some(Use::Includes(p)) => includes.push((i, p)),
                Some(Use::Length(p)) => {
                    lengths.push(p);
                    extra += 1;
                }
                Some(Use::Extra) => extra += 1,
                None => return,
            }
        }
        if includes.is_empty() {
            return;
        }
        if extra > 0 && !is_known_unique_array(cx, init) {
            return;
        }
        if includes.len() == 1 {
            let (ident, _) = includes[0];
            // The member, then the call.
            let member = parent_skip_parens(ast, cx.file.parents, ident);
            let root = cx.parent(decl);
            if !member.is_some_and(|m| is_multiple_call(cx, m, root)) {
                return;
            }
        }
        let id_text = cx.text(name);
        let set_text = type_ann.and_then(|t| set_annotation_text(cx, t));
        let init_span = cx.span(skip_parens(ast, init));
        let mut fix = Fix::insert(init_span.start, "new Set(").and(Span::at(init_span.end), ")");
        if let (Some(t), Some(text)) = (type_ann, &set_text) {
            fix = fix.and(cx.span(t), text.clone());
        }
        for (_, p) in &includes {
            fix = fix.and(cx.span(*p), "has");
        }
        for p in &lengths {
            fix = fix.and(cx.span(*p), "size");
        }
        let span = estree_ident_span(cx, name);
        let r = cx
            .report(
                span,
                format!(
                    "`{id_text}` should be a `Set`, and use `{id_text}.has()` to check existence or non-existence."
                ),
            )
            .id("error");
        if type_ann.is_some() && set_text.is_none() {
            r.suggest("suggestion", format!("Switch `{id_text}` to `Set`."), fix);
        } else {
            r.fix(fix);
        }
    }
}
