//! `@typescript-eslint/require-await`: async functions should await something (or return a
//! thenable).

use super::prelude::*;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/require-await",
        "Disallow async functions which do not return promises and have no await expression",
        TS_REC_TC,
        Category::Suspicious,
    )
    .typed()
    .suggestions(),
    create: create_default::<RequireAwait>,
};

#[derive(Default, Debug)]
struct RequireAwait;

impl Rule for RequireAwait {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let (flags, body) = match *cx.node(node) {
            Node::FunctionDecl {
                flags,
                body: Some(body),
                ..
            }
            | Node::FunctionExpr {
                flags,
                body: Some(body),
                ..
            }
            | Node::Arrow { flags, body, .. } => (flags, body),
            _ => return,
        };
        if !flags.contains(FnFlags::ASYNC) {
            return;
        }
        let ast = cx.ast();
        let expression_body = flags.contains(FnFlags::EXPRESSION_BODY);
        if !expression_body
            && matches!(*cx.node(body), Node::Block { stmts } if ast.list(stmts).is_empty())
        {
            return;
        }
        let ti = cx.types();
        if expression_body
            && ti
                .type_at(body)
                .is_some_and(|t| thenable(ti, t) == Some(true))
        {
            return;
        }
        let mut finder = AwaitFinder {
            ti,
            root: body,
            generator: flags.contains(FnFlags::GENERATOR),
            async_iterator: None,
            found: false,
        };
        if flags.contains(FnFlags::GENERATOR) {
            finder.async_iterator = ti.well_known_symbol("asyncIterator");
        }
        walk(ast, body, &mut finder);
        if finder.found {
            return;
        }
        let src = cx.src();
        let parents = cx.file.parents;
        let kind = function_name_with_kind(ast, src, parents, node);
        // An empty name (`async ''() {}`) is left out.
        let kind = kind
            .strip_suffix(" ''")
            .map_or(kind.clone(), str::to_string);
        let mut chars = kind.chars();
        let kind = match chars.next() {
            Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
            None => kind,
        };
        let head = function_head_span(ast, src, parents, node);
        let fix = remove_async_fix(cx, node, flags.contains(FnFlags::GENERATOR));
        let r = cx
            .report(head, format!("{kind} has no 'await' expression."))
            .id("missingAwait");
        if let Some(fix) = fix {
            r.suggest("removeAsync", "Remove 'async'.", fix);
        }
    }
}

struct AwaitFinder<'a> {
    ti: &'a dyn TypeInfo,
    root: NodeId,
    generator: bool,
    async_iterator: Option<String>,
    found: bool,
}

impl AwaitFinder<'_> {
    fn has_async_iterator(&self, t: Ty) -> bool {
        let Some(name) = self.async_iterator.as_deref() else {
            return false;
        };
        let ti = self.ti;
        let t = constrained(ti, t);
        parts(ti, t).into_iter().any(|p| {
            let p = ti.apparent_type(p);
            let mut members = vec![p];
            if ti.flags(p).contains(TypeFlags::INTERSECTION) {
                members = ti.intersection_members(p);
            }
            members
                .into_iter()
                .any(|m| ti.property_of_type(m, name).is_some())
        })
    }
}

impl Visitor for AwaitFinder<'_> {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        if self.found {
            return Walk::Stop;
        }
        let ti = self.ti;
        match *ast.node(id) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                if id != self.root =>
            {
                return Walk::SkipChildren;
            }
            Node::Await { .. }
            | Node::ForOf { is_await: true, .. }
            | Node::VarDecl {
                kind: VarKind::AwaitUsing,
                ..
            } => self.found = true,
            Node::Return { arg: Some(a) } => {
                if ti.type_at(a).is_some_and(|t| thenable(ti, t) == Some(true)) {
                    self.found = true;
                }
            }
            Node::Yield {
                delegate,
                arg: Some(a),
            } if self.generator => {
                if let Some(t) = ti.type_at(a) {
                    if delegate {
                        if self.has_async_iterator(t) {
                            self.found = true;
                        }
                    } else if thenable(ti, t) == Some(true) {
                        self.found = true;
                    }
                }
            }
            _ => {}
        }
        if self.found {
            Walk::Stop
        } else {
            Walk::Continue
        }
    }
}

/// Removes `async` and the whitespace after it, unwraps a `Promise<T>` return type (or turns
/// `AsyncGenerator` into `Generator`), and adds a `;` when the function would otherwise join
/// the previous line.
fn remove_async_fix(cx: &Ctx<'_, '_>, f: NodeId, generator: bool) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let parents = cx.file.parents;
    let parent = parents.parent(f);
    // Methods carry `async` before their key.
    let holder = match parent.map(|p| (p, cx.node(p))) {
        Some((p, Node::MethodDef { value, .. })) if *value == f => p,
        _ => f,
    };
    let mut from = cx.span(holder).start;
    if let Node::MethodDef { decorators, .. } = *cx.node(holder)
        && let Some(&last) = ast.list(decorators).last()
    {
        from = cx.span(last).end;
    }
    let at = find_word(src, from, cx.span(f).end, "async")?;
    let mut end = at as usize + 5;
    let bytes = src.as_bytes();
    while end < bytes.len() && matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r') {
        end += 1;
    }
    let next = bytes.get(end).copied();
    let continues = matches!(next, Some(b'(' | b'[' | b'`'));
    let semicolon = continues
        && (needs_leading_semicolon(ast, src, parents, f) || after_unterminated_field(cx, holder));
    let mut fix = Fix::replace(Span::new(at, end as u32), if semicolon { ";" } else { "" });
    let return_type = match *cx.node(f) {
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type,
        _ => None,
    };
    if let Some(rt) = return_type
        && let Node::TypeRef { name, type_args } = *cx.node(rt)
        && let Some(n) = ident_name(ast, name)
    {
        if generator {
            let sync = match n {
                "AsyncGenerator" => Some("Generator"),
                "AsyncIterator" => Some("Iterator"),
                "AsyncIterable" => Some("Iterable"),
                "AsyncIterableIterator" => Some("IterableIterator"),
                _ => None,
            };
            if let Some(sync) = sync {
                fix = fix.and(cx.span(name), sync);
            }
        } else if n == "Promise"
            && let Some(ta) = type_args
            && let Node::TypeArgs { args } = *cx.node(ta)
            && let [arg] = ast.list(args)
        {
            fix = fix.and(cx.span(rt), cx.span(*arg).text(src).to_string());
        }
    }
    Some(fix)
}

/// `holder` is a class member after a field declaration that does not end with `;`.
fn after_unterminated_field(cx: &Ctx<'_, '_>, holder: NodeId) -> bool {
    let Some(class) = cx.parent(holder) else {
        return false;
    };
    let members = match *cx.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
        _ => return false,
    };
    let list = cx.ast().list(members);
    let Some(i) = list.iter().position(|&m| m == holder) else {
        return false;
    };
    let Some(&prev) = i.checked_sub(1).and_then(|j| list.get(j)) else {
        return false;
    };
    matches!(cx.node(prev), Node::PropertyDef { .. })
        && !cx.span(prev).text(cx.src()).trim_end().ends_with(';')
}

/// The first whole-word `word` in `src[from..to]`.
fn find_word(src: &str, from: u32, to: u32, word: &str) -> Option<u32> {
    let hay = src.get(from as usize..to as usize)?;
    let mut start = 0;
    while let Some(i) = hay[start..].find(word) {
        let pos = start + i;
        let before = hay[..pos].chars().next_back();
        let after = hay[pos + word.len()..].chars().next();
        if !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char) {
            return Some(from + pos as u32);
        }
        start = pos + word.len();
    }
    None
}
