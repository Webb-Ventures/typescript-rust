//! `@typescript-eslint/strict-void-return`: a function that returns a value, passed where a
//! function returning `void` is expected.

use super::prelude::*;
use super::ts_strict_boolean_expressions::wrapping_fix;
use super::unicorn_static::has_side_effect;
use crate::diagnostic::Edit;
use ts_r_ast::HeritageKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/strict-void-return",
        "Disallow passing a value-returning function in a position accepting a void function",
        NO_PRESET,
        Category::Correctness,
    )
    .typed()
    .suggestions(),
    create: create::<StrictVoidReturn>,
};

#[derive(Debug)]
struct StrictVoidReturn {
    allow_return_any: bool,
}

impl FromOptions for StrictVoidReturn {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(StrictVoidReturn {
            allow_return_any: bool_field(obj_opt(opts, 0), "allowReturnAny", false),
        })
    }
}

const NULLISH_OR_ANY: TypeFlags = TypeFlags::VOID
    .union(TypeFlags::UNDEFINED)
    .union(TypeFlags::NULL)
    .union(TypeFlags::ANY)
    .union(TypeFlags::NEVER);

/// Call signatures of a type: every union member's, and an intersection's only when just one
/// of its members has any.
fn call_signatures(ti: &dyn TypeInfo, t: Ty, depth: u32) -> Vec<Sig> {
    if depth > 16 {
        return Vec::new();
    }
    let f = ti.flags(t);
    if f.contains(TypeFlags::UNION) {
        return ti
            .union_members(t)
            .into_iter()
            .flat_map(|m| call_signatures(ti, m, depth + 1))
            .collect();
    }
    if f.contains(TypeFlags::INTERSECTION) {
        let mut found: Option<Vec<Sig>> = None;
        for m in ti.intersection_members(t) {
            let s = call_signatures(ti, m, depth + 1);
            if !s.is_empty() {
                if found.is_some() {
                    return Vec::new();
                }
                found = Some(s);
            }
        }
        return found.unwrap_or_default();
    }
    ti.signatures(t, false)
}

/// Every call signature returns only `void` (and there is one).
fn returns_void(ti: &dyn TypeInfo, t: Ty) -> bool {
    let returns: Vec<Ty> = call_signatures(ti, t, 0)
        .into_iter()
        .flat_map(|s| ti.union_members(ti.return_type(s)))
        .collect();
    !returns.is_empty()
        && returns
            .iter()
            .all(|&r| ti.flags(r).contains(TypeFlags::VOID))
}

impl StrictVoidReturn {
    fn allowed(&self) -> TypeFlags {
        let f = TypeFlags::VOID | TypeFlags::NEVER | TypeFlags::UNDEFINED;
        if self.allow_return_any {
            f | TypeFlags::ANY
        } else {
            f
        }
    }

    /// Checks `node` against its contextual type; true when that is a void function type.
    fn check_expr(&self, cx: &mut Ctx<'_, '_>, node: NodeId) -> bool {
        let node = skip_parens(cx.ast(), node);
        let ti = cx.types();
        match ti.contextual_type(node) {
            Some(t) if returns_void(ti, t) => {
                self.report_non_void(cx, node);
                true
            }
            _ => false,
        }
    }

    fn check_call(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let (callee, args, construct) = match *ast.node(node) {
            Node::Call { callee, args, .. } => (callee, args, false),
            Node::New { callee, args, .. } => (callee, args, true),
            _ => return,
        };
        let args = ast.list(args);
        if args.is_empty() {
            return;
        }
        let ti = cx.types();
        let Some(ft) = ti.type_at(callee) else {
            return;
        };
        let sigs: Vec<Sig> = ti
            .union_members(ft)
            .into_iter()
            .flat_map(|t| ti.signatures(t, construct))
            .collect();
        for (i, &arg) in args.iter().enumerate() {
            if matches!(ast.node(arg), Node::Spread { .. }) {
                continue;
            }
            let ti = cx.types();
            let expected: Vec<Ty> = sigs
                .iter()
                .filter_map(|&s| {
                    let (count, rest) = ti.param_count(s);
                    if i < count {
                        ti.param_type(s, i)
                    } else if rest && i == count {
                        ti.rest_param_type(s)
                    } else {
                        None
                    }
                })
                .flat_map(|t| ti.union_members(t))
                .flat_map(|t| ti.signatures(t, false))
                .map(|s| ti.return_type(s))
                .collect();
            let all_void = expected.iter().all(|&r| {
                let f = ti.flags(r);
                f.intersects(NULLISH_OR_ANY) || f.contains(TypeFlags::TYPE_PARAMETER)
            });
            if (sigs.len() == 1 || all_void) && self.check_expr(cx, arg) {
                continue;
            }
            let ti = cx.types();
            if expected
                .iter()
                .any(|&r| ti.flags(r).contains(TypeFlags::VOID))
                && expected
                    .iter()
                    .all(|&r| ti.flags(r).intersects(NULLISH_OR_ANY))
            {
                self.report_non_void(cx, skip_parens(cx.ast(), arg));
            }
        }
    }

    /// The type of the member of the class's base types (`extends` / `implements`) named like
    /// `member`.
    fn base_member_types(cx: &Ctx<'_, '_>, member: NodeId, key: NodeId) -> Vec<Ty> {
        let ast = cx.ast();
        let ti = cx.types();
        // A computed key goes by its member symbol's name (`[k]` with `k = 'cb' as const`).
        let name = match key_name(ast, cx.src(), key) {
            Some(n) => n.into_owned(),
            None => {
                let literal = match *ast.node(key) {
                    Node::ComputedKey { expr } => ti
                        .type_at(expr)
                        .filter(|&t| {
                            ti.flags(t)
                                .intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
                        })
                        .and_then(|t| ti.literal_text(t)),
                    _ => None,
                };
                match literal {
                    Some(n) => n,
                    None => return Vec::new(),
                }
            }
        };
        let Some(class) = cx.parent(member) else {
            return Vec::new();
        };
        let heritage = match *ast.node(class) {
            Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. } => heritage,
            _ => return Vec::new(),
        };
        // The base class's instance type, as `getTypeAtLocation` gives for an `extends`
        // entry.
        let class_name = match *ast.node(class) {
            Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
            _ => None,
        };
        let class_sym = class_name
            .and_then(|n| ti.symbol_at(n))
            .or_else(|| ti.symbol_at(class));
        let base_class = class_sym
            .and_then(|s| ti.declared_type_of_symbol(s))
            .and_then(|t| ti.base_types(t).first().copied());
        let mut out = Vec::new();
        for &h in ast.list(heritage) {
            let Node::HeritageClause { kind, types } = *ast.node(h) else {
                continue;
            };
            for &tn in ast.list(types) {
                let bt = match kind {
                    HeritageKind::Extends => base_class,
                    HeritageKind::Implements => ti.type_of_type_node(tn),
                };
                if let Some(bt) = bt
                    && let Some(p) = ti.property_of_type(bt, &name)
                {
                    out.push(ti.type_of_symbol(p));
                }
            }
        }
        out
    }

    fn check_class_member(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let (key, value) = match *ast.node(node) {
            Node::MethodDef { key, value, .. } => {
                if matches!(*ast.node(value), Node::FunctionExpr { body: None, .. }) {
                    return;
                }
                (key, Some(value))
            }
            Node::PropertyDef { key, value, .. } => (key, value),
            _ => return,
        };
        let Some(value) = value else {
            return;
        };
        let ti = cx.types();
        if Self::base_member_types(cx, node, key)
            .into_iter()
            .any(|t| returns_void(ti, t))
        {
            self.report_non_void(cx, skip_parens(ast, value));
            return;
        }
        if matches!(ast.node(node), Node::PropertyDef { .. }) {
            self.check_expr(cx, value);
        }
    }

    fn check_object(&self, cx: &mut Ctx<'_, '_>, node: NodeId) {
        let ast = cx.ast();
        let Node::ObjectLit { props } = *ast.node(node) else {
            return;
        };
        for &p in ast.list(props) {
            match *ast.node(p) {
                Node::Property { value, .. } => {
                    self.check_expr(cx, value);
                }
                Node::ShorthandProp { name, .. } => {
                    self.check_expr(cx, name);
                }
                Node::MethodDef {
                    kind: MethodKind::Method,
                    key,
                    value,
                    ..
                } => {
                    if matches!(ast.node(key), Node::ComputedKey { .. }) {
                        continue;
                    }
                    let ti = cx.types();
                    let Some(name) = key_name(ast, cx.src(), key) else {
                        continue;
                    };
                    let expected = ti
                        .contextual_type(node)
                        .and_then(|t| ti.property_of_type(t, &name))
                        .map(|s| ti.type_of_symbol(s));
                    if expected.is_some_and(|t| returns_void(ti, t)) {
                        self.report_non_void(cx, value);
                    }
                }
                _ => {}
            }
        }
    }

    /// Reports `f` unless it is a function whose returns are all allowed.
    fn report_non_void(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let ast = cx.ast();
        let ti = cx.types();
        let allowed = self.allowed();
        // An accessor's type is its property's.
        let accessor = cx.parent(f).and_then(|p| match *ast.node(p) {
            Node::MethodDef {
                kind: MethodKind::Get | MethodKind::Set,
                value,
                ..
            } if value == f => Some(p),
            _ => None,
        });
        let t = match accessor {
            Some(m) => ti.symbol_at(m).map(|s| ti.type_of_symbol(s)),
            None => ti.type_at(f),
        };
        let Some(t) = t else {
            return;
        };
        let actual = ti.apparent_type(t);
        let all_allowed = call_signatures(ti, actual, 0)
            .into_iter()
            .flat_map(|s| ti.union_members(ti.return_type(s)))
            .all(|r| ti.flags(r).intersects(allowed));
        if all_allowed {
            return;
        }
        let (flags, body, return_type) = match *ast.node(f) {
            Node::Arrow {
                flags,
                body,
                return_type,
                ..
            } => (flags, Some(body), return_type),
            Node::FunctionExpr {
                flags,
                body,
                return_type,
                ..
            } => (flags, body, return_type),
            _ => {
                cx.report_node(
                    f,
                    "Value-returning function used in a context where a void function is expected.",
                )
                .id("nonVoidFunc");
                return;
            }
        };
        let Some(body) = body else {
            return;
        };
        let head = member_function_head_span(ast, cx.src(), cx.file.parents, f);
        let block = matches!(ast.node(body), Node::Block { .. });
        if flags.contains(FnFlags::GENERATOR) {
            cx.report(
                head,
                "Value-returning function used in a context where a void function is expected.",
            )
            .id("nonVoidFunc");
            return;
        }
        if flags.contains(FnFlags::ASYNC) {
            let msg = "Async function used in a context where a void function is expected.";
            if !block {
                let inner = skip_parens(ast, body);
                let suggest = has_side_effect(ast, inner).then(|| self.add_void_fix(cx, f, inner));
                add_void_suggestion(cx.report(head, msg).id("asyncFunc"), suggest);
                return;
            }
            let fix = self.wrap_iife_fix(cx, f, body);
            cx.report(head, msg).id("asyncFunc").suggest(
                "suggestWrapInAsyncIIFE",
                "Wrap the function body in an async IIFE.",
                fix,
            );
            return;
        }
        if !block {
            let inner = skip_parens(ast, body);
            let suggest = has_side_effect(ast, inner).then(|| self.add_void_fix(cx, f, inner));
            let r = cx
                .report_node(
                    inner,
                    "Value returned in a context where a void return is expected.",
                )
                .id("nonVoidReturn");
            add_void_suggestion(r, suggest);
            return;
        }
        if let Some(rt) = return_type
            && !matches!(
                ast.node(rt),
                Node::KeywordType {
                    kind: KeywordTypeKind::Void
                }
            )
        {
            cx.report_node(
                rt,
                "Value-returning function used in a context where a void function is expected.",
            )
            .id("nonVoidFunc");
            return;
        }
        let mut returns = Vec::new();
        if let Node::Block { stmts } = *ast.node(body) {
            collect_returns(ast, ast.list(stmts), &mut returns, 0);
        }
        for (stmt, arg) in returns {
            let ti = cx.types();
            if ti
                .type_at(arg)
                .is_some_and(|t| ti.flags(t).intersects(allowed))
            {
                continue;
            }
            let start = cx.span(stmt).start;
            cx.report(
                Span::new(start, start + "return".len() as u32),
                "Value returned in a context where a void return is expected.",
            )
            .id("nonVoidReturn");
        }
    }

    /// Edits that drop `async` and turn a written return type into `void`.
    fn sync_edits(cx: &Ctx<'_, '_>, f: NodeId) -> Vec<Edit> {
        let ast = cx.ast();
        let mut edits = Vec::new();
        let (flags, return_type) = match *ast.node(f) {
            Node::Arrow {
                flags, return_type, ..
            }
            | Node::FunctionExpr {
                flags, return_type, ..
            } => (flags, return_type),
            _ => return edits,
        };
        if flags.contains(FnFlags::ASYNC) {
            let head = cx
                .parent(f)
                .filter(|&p| matches!(ast.node(p), Node::Property { .. } | Node::MethodDef { .. }))
                .unwrap_or(f);
            if let Some(at) = find_word(cx.src(), cx.span(head), "async") {
                edits.push(Edit {
                    span: Span::new(at, at + "async".len() as u32),
                    text: String::new(),
                });
            }
        }
        if let Some(rt) = return_type {
            edits.push(Edit {
                span: cx.span(rt),
                text: "void".to_owned(),
            });
        }
        edits
    }

    fn add_void_fix(&self, cx: &Ctx<'_, '_>, f: NodeId, inner: NodeId) -> Fix {
        let mut edits = Self::sync_edits(cx, f);
        edits.extend(wrapping_fix(cx, inner, inner, "void {}").edits);
        Fix { edits }
    }

    fn wrap_iife_fix(&self, cx: &Ctx<'_, '_>, f: NodeId, body: NodeId) -> Fix {
        let src = cx.src();
        let mut edits = Self::sync_edits(cx, f);
        let b = cx.span(body);
        let insert = |at: u32, text: &str| Edit {
            span: Span::new(at, at),
            text: text.to_owned(),
        };
        if matches!(cx.node(f), Node::Arrow { .. }) {
            if let Some(arrow) = src[..b.start as usize].rfind("=>") {
                edits.push(insert(arrow as u32 + 2, " void (async () =>"));
            }
            edits.push(insert(cx.span(f).end, ")()"));
        } else {
            edits.push(insert(b.start, "{ (async () => "));
            edits.push(insert(b.end, ")(); }"));
        }
        Fix { edits }
    }
}

fn add_void_suggestion(r: crate::rule::Report<'_>, fix: Option<Fix>) {
    if let Some(fix) = fix {
        r.suggest(
            "suggestAddVoidOp",
            "Add a void operator to discard the return value.",
            fix,
        );
    }
}

/// The first `word` token in `span` (outside comments and strings).
fn find_word(src: &str, span: Span, word: &str) -> Option<u32> {
    let b = src.as_bytes();
    let end = (span.end as usize).min(b.len());
    let mut i = span.start as usize;
    while i < end {
        let c = b[i];
        if src[i..].starts_with("//") {
            i = src[i..end].find('\n').map_or(end, |p| i + p);
        } else if src[i..].starts_with("/*") {
            i = src[i + 2..end].find("*/").map_or(end, |p| i + 2 + p + 2);
        } else if c == b'"' || c == b'\'' || c == b'`' {
            let mut j = i + 1;
            while j < end && b[j] != c {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            i = j + 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
            let mut j = i;
            while j < end && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$') {
                j += 1;
            }
            if &src[i..j] == word {
                return Some(i as u32);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// `return x` statements of a body, through blocks, `if`, loops, `switch`, `try`, labels and
/// `with` (not nested functions).
fn collect_returns(
    ast: &ts_r_ast::Ast,
    stmts: &[NodeId],
    out: &mut Vec<(NodeId, NodeId)>,
    depth: u32,
) {
    if depth > 512 {
        return;
    }
    for &s in stmts {
        match *ast.node(s) {
            Node::Block { stmts } => collect_returns(ast, ast.list(stmts), out, depth + 1),
            Node::Switch { cases, .. } => {
                for &c in ast.list(cases) {
                    if let Node::SwitchCase { body, .. } = *ast.node(c) {
                        collect_returns(ast, ast.list(body), out, depth + 1);
                    }
                }
            }
            Node::If { cons, alt, .. } => {
                collect_returns(ast, &[cons], out, depth + 1);
                if let Some(a) = alt {
                    collect_returns(ast, &[a], out, depth + 1);
                }
            }
            Node::While { body, .. }
            | Node::DoWhile { body, .. }
            | Node::For { body, .. }
            | Node::ForIn { body, .. }
            | Node::ForOf { body, .. }
            | Node::With { body, .. }
            | Node::Labeled { body, .. } => collect_returns(ast, &[body], out, depth + 1),
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                collect_returns(ast, &[block], out, depth + 1);
                if let Some(h) = handler
                    && let Node::CatchClause { body, .. } = *ast.node(h)
                {
                    collect_returns(ast, &[body], out, depth + 1);
                }
                if let Some(f) = finalizer {
                    collect_returns(ast, &[f], out, depth + 1);
                }
            }
            Node::Return { arg: Some(a) } => out.push((s, a)),
            _ => {}
        }
    }
}

impl Rule for StrictVoidReturn {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ArrayLit,
            NodeKind::Arrow,
            NodeKind::Assign,
            NodeKind::Call,
            NodeKind::New,
            NodeKind::JsxAttr,
            NodeKind::MethodDef,
            NodeKind::ObjectLit,
            NodeKind::PropertyDef,
            NodeKind::Return,
            NodeKind::VarDeclarator,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        match *ast.node(node) {
            Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    if !matches!(ast.node(e), Node::Spread { .. } | Node::Hole {}) {
                        self.check_expr(cx, e);
                    }
                }
            }
            Node::Arrow { body, .. } => {
                if !matches!(ast.node(body), Node::Block { .. }) {
                    self.check_expr(cx, body);
                }
            }
            Node::Assign { right, .. } => {
                self.check_expr(cx, right);
            }
            Node::Call { .. } | Node::New { .. } => self.check_call(cx, node),
            Node::JsxAttr { value: Some(v), .. } => {
                if let Node::JsxExprContainer { expr: Some(e) } = *ast.node(v) {
                    self.check_expr(cx, e);
                }
            }
            Node::MethodDef { .. } | Node::PropertyDef { .. } => {
                let in_class = cx.parent(node).is_some_and(|p| {
                    matches!(ast.node(p), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                });
                if in_class {
                    self.check_class_member(cx, node);
                }
            }
            Node::ObjectLit { .. } => self.check_object(cx, node),
            Node::Return { arg: Some(a) } => {
                self.check_expr(cx, a);
            }
            Node::VarDeclarator { init: Some(i), .. } => {
                self.check_expr(cx, i);
            }
            _ => {}
        }
    }
}
