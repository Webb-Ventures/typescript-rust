//! Call shapes the eslint-plugin-unicorn rules test for (`foo.bar(x)` with a plain property
//! name), on our AST: parentheses around the callee are transparent, as in ESTree.

use super::prelude::*;
use ts_r_ast::Ast;

/// `obj.name(args)`: a call whose callee (through parentheses) is a non-computed member
/// with an identifier property.
#[derive(Clone, Copy)]
pub(crate) struct MethodCall<'a> {
    pub obj: NodeId,
    pub prop: NodeId,
    pub name: &'a str,
    pub args: &'a [NodeId],
    pub optional_call: bool,
    pub optional_member: bool,
}

impl MethodCall<'_> {
    /// Neither `?.(` nor `?.name`.
    pub fn plain(&self) -> bool {
        !self.optional_call && !self.optional_member
    }
}

pub(crate) fn method_call(ast: &Ast, id: NodeId) -> Option<MethodCall<'_>> {
    let Node::Call {
        callee,
        optional,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: optional_member,
        prop,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    let Node::Ident { .. } = ast.node(prop) else {
        return None;
    };
    Some(MethodCall {
        obj,
        prop,
        name: ident_name(ast, prop)?,
        args: ast.list(args),
        optional_call: optional,
        optional_member,
    })
}

/// No spread among the first `n` arguments (the plugin's argument-count checks reject a
/// spread that could stand for the counted ones).
pub(crate) fn no_spread_in(ast: &Ast, args: &[NodeId], n: usize) -> bool {
    !args
        .iter()
        .take(n)
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
}

/// `Promise.<method>(<one non-spread argument>)`, no optional chaining, with `method` one
/// of `methods`: the method name.
pub(crate) fn promise_method_call<'a>(
    ast: &'a Ast,
    id: NodeId,
    methods: &[&str],
) -> Option<MethodCall<'a>> {
    let call = method_call(ast, id)?;
    (call.plain()
        && methods.contains(&call.name)
        && ident_name(ast, skip_parens(ast, call.obj)) == Some("Promise")
        && call.args.len() == 1
        && no_spread_in(ast, call.args, 1))
    .then_some(call)
}

/// The end of the whitespace (JavaScript's `\s`) starting at `pos`.
pub(crate) fn skip_spaces(src: &str, pos: usize) -> usize {
    let rest = &src[pos..];
    pos + rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
            .len()
}

/// A comment lies inside `replaced` but outside every `kept` span (a fix replacing
/// `replaced` while copying the kept parts would drop it).
pub(crate) fn would_remove_comments(ast: &Ast, replaced: Span, kept: &[Span]) -> bool {
    ast.comments_in(replaced).iter().any(|c| {
        c.span.end <= replaced.end
            && !kept
                .iter()
                .any(|k| k.start <= c.span.start && c.span.end <= k.end)
    })
}

/// `callee` is the name or dotted path `path` (`_.cloneDeep`, `new.target.x`, `this.y`):
/// non-optional, non-computed members down to a name, `this`, `super` or a meta property.
pub(crate) fn matches_path(ast: &Ast, mut node: NodeId, path: &str) -> bool {
    let names: Vec<&str> = path.trim().split('.').collect();
    let mut i = names.len() - 1;
    while i > 0 {
        let name = names[i];
        if name.is_empty() {
            return false;
        }
        node = skip_parens(ast, node);
        if i == 1
            && let Node::MetaProperty { kind } = *ast.node(node)
        {
            let (meta, prop) = match kind {
                ts_r_ast::MetaPropKind::NewTarget => ("new", "target"),
                ts_r_ast::MetaPropKind::ImportMeta => ("import", "meta"),
            };
            return prop == name && meta == names[0];
        }
        match *ast.node(node) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } if ident_name(ast, prop) == Some(name) => node = obj,
            _ => return false,
        }
        i -= 1;
    }
    node = skip_parens(ast, node);
    match ast.node(node) {
        Node::Ident { .. } => ident_name(ast, node) == Some(names[0]),
        Node::This {} => names[0] == "this",
        Node::Super {} => names[0] == "super",
        _ => false,
    }
}

/// The `(`, `)` and trailing `,` tokens of a call's argument list (`None` when the source
/// doesn't have them where expected).
pub(crate) fn call_tokens(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(u32, u32, Option<u32>)> {
    let ast = cx.ast();
    let src = cx.src();
    let (callee, type_args, args) = match *ast.node(call) {
        Node::Call {
            callee,
            type_args,
            args,
            ..
        }
        | Node::New {
            callee,
            type_args,
            args,
            ..
        } => (callee, type_args, args),
        _ => return None,
    };
    let after = type_args.map_or(cx.span(callee).end, |t| cx.span(t).end);
    let open = skip_trivia_forward(src, after as usize);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The callee of the first expression statement's call in `src`.
    fn callee(src: &str) -> (ts_r_parser::ParseResult, NodeId) {
        let parsed = ts_r_parser::parse(src, ts_r_ast::SourceKind::Js);
        let ast = &parsed.ast;
        let Node::Program { stmts } = *ast.node(ast.root) else {
            panic!("program");
        };
        let Node::ExprStmt { expr } = *ast.node(ast.list(stmts)[0]) else {
            panic!("expression statement");
        };
        let Node::Call { callee, .. } = *ast.node(skip_parens(ast, expr)) else {
            panic!("call");
        };
        (parsed, callee)
    }

    #[test]
    fn paths() {
        for (src, path, want) in [
            ("_.cloneDeep(x)", "_.cloneDeep", true),
            ("(_).cloneDeep(x)", "_.cloneDeep", true),
            ("_?.cloneDeep(x)", "_.cloneDeep", false),
            ("_['cloneDeep'](x)", "_.cloneDeep", false),
            ("a.b.c(x)", " a.b.c ", true),
            ("a.b.c(x)", "a..c", false),
            ("clone(x)", "clone", true),
            ("this.clone(x)", "this.clone", true),
        ] {
            let (p, c) = callee(src);
            assert_eq!(matches_path(&p.ast, c, path), want, "{src} {path}");
        }
    }

    #[test]
    fn spaces_and_comments() {
        assert_eq!(skip_spaces("await \n\t x", 5), 9);
        assert_eq!(skip_spaces("await", 5), 5);
        let p = ts_r_parser::parse("f(/* a */ x /* b */)", ts_r_ast::SourceKind::Js);
        let all = Span::new(0, 20);
        assert!(would_remove_comments(&p.ast, all, &[Span::new(2, 11)]));
        assert!(!would_remove_comments(&p.ast, all, &[Span::new(2, 19)]));
    }
}
