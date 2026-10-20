//! `no-unused-private-class-members`: disallow unused private class members.

use rustc_hash::FxHashMap;

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unused-private-class-members",
        "Disallow unused private class members",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create_default::<NoUnusedPrivateClassMembers>,
};

#[derive(Debug, Default)]
struct NoUnusedPrivateClassMembers;

#[derive(Default)]
struct Member {
    /// The last declaration of the name (getter/setter pairs share a name).
    decl: Option<NodeId>,
    accessor: bool,
    used: bool,
    written: bool,
}

/// How a `x.#name` expression is used.
enum Use {
    Read,
    WriteOnly,
}

fn classify(cx: &Ctx<'_, '_>, member: NodeId) -> Use {
    let ast = cx.ast();
    let mut cur = member;
    let Some(mut p) = cx.parent(cur) else {
        return Use::Read;
    };
    while matches!(ast.node(p), Node::Paren { .. }) {
        cur = p;
        match cx.parent(p) {
            Some(pp) => p = pp,
            None => return Use::Read,
        }
    }
    let stmt_parent = |n: NodeId| {
        let mut n = n;
        while let Some(pp) = cx.parent(n) {
            if matches!(ast.node(pp), Node::Paren { .. }) {
                n = pp;
            } else {
                return matches!(ast.node(pp), Node::ExprStmt { .. });
            }
        }
        false
    };
    match *ast.node(p) {
        Node::Assign { left, op, .. } if left == cur => {
            if op == AssignOp::Assign || stmt_parent(p) {
                Use::WriteOnly
            } else {
                Use::Read
            }
        }
        Node::Update { .. } => {
            if stmt_parent(p) {
                Use::WriteOnly
            } else {
                Use::Read
            }
        }
        Node::ForIn { left, .. } | Node::ForOf { left, .. } if left == cur => Use::WriteOnly,
        _ => {
            if write_root(ast, cx.file.parents, member).is_some() {
                Use::WriteOnly
            } else {
                Use::Read
            }
        }
    }
}

impl NoUnusedPrivateClassMembers {
    /// The suggestion that deletes `decl` (with its attached comments when it owns its line).
    fn removal(cx: &Ctx<'_, '_>, decl: NodeId, members: &[NodeId]) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let span = cx.span(decl);
        let line_start = |pos: u32| src[..pos as usize].rfind('\n').map_or(0, |i| i + 1) as u32;
        let line_end = |pos: u32| {
            src[pos as usize..]
                .find('\n')
                .map_or(src.len(), |i| pos as usize + i) as u32
        };
        let only_ws_or_comments =
            |a: u32, b: u32| skip_trivia_forward(src, a as usize) >= b as usize;
        let idx = members.iter().position(|&m| m == decl);
        let prev = idx.and_then(|i| i.checked_sub(1)).map(|i| members[i]);
        let next = idx.and_then(|i| members.get(i + 1).copied());
        // Does the member own its line(s)? Nothing but trivia before it on its first line and
        // after it on its last line.
        let ls = line_start(span.start);
        let before_ok = src[ls as usize..span.start as usize].trim().is_empty()
            || (only_ws_or_comments(ls, span.start)
                && ast
                    .comments_in(Span::new(ls, span.start))
                    .iter()
                    .all(|c| c.span.start >= ls));
        let le = line_end(span.end);
        let next_on_line = next.is_some_and(|n| cx.span(n).start < le)
            || src[span.end as usize..le as usize]
                .trim_start()
                .starts_with('}');
        let owns = before_ok && !next_on_line && prev.is_none_or(|p| cx.span(p).end <= ls);
        let mut start = span.start;
        let mut end = span.end;
        if owns {
            // Leading comments on the same line and attached comment lines above.
            let comments = &ast.comments;
            let mut i = comments.partition_point(|c| c.span.end <= span.start);
            while i > 0 {
                let c = comments[i - 1];
                let between = &src[c.span.end as usize..start as usize];
                if !between.trim().is_empty() {
                    break;
                }
                let newlines = between.matches('\n').count();
                let cl = line_start(c.span.start);
                let own_line = src[cl as usize..c.span.start as usize].trim().is_empty();
                if newlines > 1 || !own_line {
                    break;
                }
                start = c.span.start;
                i -= 1;
            }
            // Trailing comments starting on the member's line.
            for c in ast.comments.iter().filter(|c| c.span.start >= span.end) {
                if !src[end as usize..c.span.start as usize].trim().is_empty()
                    || has_newline(src, end, c.span.start)
                {
                    break;
                }
                end = c.span.end;
            }
            let le = line_end(end);
            if src[end as usize..le as usize].trim().is_empty() {
                // Remove the whole lines with their line break.
                let ls = line_start(start);
                let mut stop = if le as usize >= src.len() { le } else { le + 1 };
                // Don't leave a blank line right after `{` or another blank line.
                let prev_line = src[..ls as usize].trim_end_matches(['\n', '\r']);
                let prev_line = &prev_line[line_start(prev_line.len() as u32) as usize..];
                let next_end = line_end(stop);
                if (stop as usize) < src.len()
                    && src[stop as usize..next_end as usize].trim().is_empty()
                    && (prev_line.trim().is_empty() || prev_line.trim_end().ends_with('{'))
                    && (next_end as usize) < src.len()
                {
                    stop = next_end + 1;
                }
                return Self::with_asi(cx, decl, prev, next, Span::new(ls, stop));
            }
        }
        // Partial removal: the member and the spaces after it.
        let rest = &src[end as usize..];
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        end += ws as u32;
        Self::with_asi(cx, decl, prev, next, Span::new(start, end))
    }

    /// Adds a `;` after the previous field when removing `decl` would join it with the next
    /// member.
    fn with_asi(
        cx: &Ctx<'_, '_>,
        _decl: NodeId,
        prev: Option<NodeId>,
        next: Option<NodeId>,
        remove: Span,
    ) -> Fix {
        let ast = cx.ast();
        let src = cx.src();
        let fix = Fix::delete(remove);
        let (Some(p), Some(n)) = (prev, next) else {
            return fix;
        };
        let Node::PropertyDef { value: Some(v), .. } = *ast.node(p) else {
            return fix;
        };
        let pspan = cx.span(p);
        if src[..pspan.end as usize].trim_end().ends_with(';') {
            return fix;
        }
        if let Node::Arrow { body, .. } = *ast.node(skip_parens(ast, v))
            && matches!(ast.node(body), Node::Block { .. })
        {
            return fix;
        }
        let nt = cx.text(n);
        let hazard = nt.starts_with(['[', '(', '*', '+', '-', '/', '`'])
            || nt.starts_with("in ")
            || nt.starts_with("in=")
            || nt.starts_with("instanceof");
        if !hazard {
            return fix;
        }
        Fix::insert(pspan.end, ";").and(remove, "")
    }
}

impl Rule for NoUnusedPrivateClassMembers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let members = match *cx.node(node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
            _ => return,
        };
        let mut privates: FxHashMap<&str, Member> = FxHashMap::default();
        let mut order: Vec<&str> = Vec::new();
        for &m in members {
            let (key, accessor) = match *ast.node(m) {
                Node::PropertyDef { key, modifiers, .. } => {
                    (key, modifiers.contains(Modifiers::ACCESSOR))
                }
                Node::MethodDef { key, kind, .. } => {
                    (key, matches!(kind, MethodKind::Get | MethodKind::Set))
                }
                _ => continue,
            };
            let Node::PrivateName { name } = *ast.node(key) else {
                continue;
            };
            let name = ast.atom(name);
            if !privates.contains_key(name) {
                order.push(name);
            }
            let e = privates.entry(name).or_default();
            e.decl = Some(m);
            e.accessor |= accessor;
        }
        if privates.is_empty() {
            return;
        }
        // Walk the class body; nested classes that redeclare a name shadow it.
        let mut stack: Vec<(NodeId, Vec<&str>)> =
            members.iter().map(|&m| (m, Vec::new())).collect();
        while let Some((n, shadowed)) = stack.pop() {
            let mut shadow = shadowed.clone();
            if let Node::ClassDecl { members: ms, .. } | Node::ClassExpr { members: ms, .. } =
                *ast.node(n)
            {
                for &m in ast.list(ms) {
                    let key = match *ast.node(m) {
                        Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                        _ => continue,
                    };
                    if let Node::PrivateName { name } = *ast.node(key) {
                        shadow.push(ast.atom(name));
                    }
                }
            }
            match *ast.node(n) {
                Node::Member { prop, .. } => {
                    if let Node::PrivateName { name } = *ast.node(prop) {
                        let name = ast.atom(name);
                        if !shadow.contains(&name)
                            && let Some(e) = privates.get_mut(name)
                        {
                            if e.accessor {
                                e.used = true;
                            } else {
                                match classify(cx, n) {
                                    Use::Read => e.used = true,
                                    Use::WriteOnly => e.written = true,
                                }
                            }
                        }
                    }
                }
                Node::Binary {
                    left,
                    op: BinaryOp::In,
                    ..
                } => {
                    // `#x in obj`
                    if let Node::PrivateName { name } = *ast.node(left) {
                        let name = ast.atom(name);
                        if !shadow.contains(&name)
                            && let Some(e) = privates.get_mut(name)
                        {
                            e.used = true;
                        }
                    }
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push((c, shadow.clone())));
        }
        for name in order {
            let e = &privates[name];
            if e.used {
                continue;
            }
            let Some(decl) = e.decl else { continue };
            let key = match *ast.node(decl) {
                Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                _ => continue,
            };
            let msg = format!("'#{name}' is defined but never used.");
            if e.written {
                cx.report_node(key, msg).id("unusedPrivateClassMember");
            } else {
                let fix = Self::removal(cx, decl, members);
                cx.report_node(key, msg)
                    .id("unusedPrivateClassMember")
                    .suggest(
                        "removeUnusedPrivateClassMember",
                        format!("Remove unused private class member '#{name}'."),
                        fix,
                    );
            }
        }
    }
}
