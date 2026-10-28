//! Call shapes the eslint-plugin-unicorn rules test for (`foo.bar(x)` with a plain property
//! name), on our AST: parentheses around the callee are transparent, as in ESTree.

use super::prelude::*;
use super::unicorn_text::paren_span;
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

/// The `extends` expression of a class (ESTree's `superClass`).
pub(crate) fn superclass(ast: &Ast, class: NodeId) -> Option<NodeId> {
    let (Node::ClassDecl { heritage, .. } | Node::ClassExpr { heritage, .. }) = *ast.node(class)
    else {
        return None;
    };
    ast.list(heritage).iter().find_map(|&h| match *ast.node(h) {
        Node::HeritageClause {
            kind: ts_r_ast::HeritageKind::Extends,
            types,
        } => ast.list(types).first().map(|&t| match *ast.node(t) {
            Node::ExprWithTypeArgs { expr, .. } => expr,
            _ => t,
        }),
        _ => None,
    })
}

/// The identifier that declares the variable `ident` refers to (its first declaration), or
/// `None` for an undeclared or global name.
pub(crate) fn declared_name(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<NodeId> {
    let sem = cx.semantic();
    let Resolution::Symbol(s) = sem.reference(sem.reference_at(ident)?).resolution else {
        return None;
    };
    sem.symbol(s).decls.first().map(|d| d.name)
}

/// A statement (expression, declaration, `return`, `throw`, `export default`) that ends
/// without `;`, so code starting with `(`, `[`, `+`, ... on the next line would continue it.
pub(crate) fn ends_without_semicolon(cx: &Ctx<'_, '_>, stmt: NodeId) -> bool {
    matches!(
        cx.ast().node(stmt),
        Node::ExprStmt { .. }
            | Node::VarDecl { .. }
            | Node::Return { .. }
            | Node::Throw { .. }
            | Node::ExportDefaultExpr { .. }
    ) && !cx.span(stmt).text(cx.src()).trim_end().ends_with(';')
}

/// Code starting with `first` that replaces the start of statement `stmt` needs a `;` in
/// front: it could continue the previous statement.
pub(crate) fn needs_semicolon_before(cx: &Ctx<'_, '_>, stmt: NodeId, first: Option<char>) -> bool {
    first.is_some_and(|c| "[(/`+-*,.<".contains(c))
        && prev_statement(cx.ast(), cx.file.parents, stmt)
            .is_some_and(|p| ends_without_semicolon(cx, p))
}

/// Parenthesize a `return`/`throw` argument: ` (` after the keyword and `)` before the
/// statement's `;` (or after it).
pub(crate) fn paren_return_or_throw(cx: &Ctx<'_, '_>, stmt: NodeId) -> Vec<(Span, String)> {
    let span = cx.span(stmt);
    let kw = match cx.node(stmt) {
        Node::Return { .. } => "return",
        Node::Throw { .. } => "throw",
        _ => return Vec::new(),
    };
    let after_kw = span.start + kw.len() as u32;
    let close = if cx.span(stmt).text(cx.src()).ends_with(';') {
        span.end - 1
    } else {
        span.end
    };
    vec![
        (Span::at(after_kw), " (".to_owned()),
        (Span::at(close), ")".to_owned()),
    ]
}

/// JavaScript's `\s`: whitespace and line terminators, including U+FEFF but not U+0085.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// The syntax requires the string or template literal `node` to stay a literal: module
/// sources and specifier names, import attributes, non-computed keys (and any key of
/// abstract members and signatures), JSX attribute values, enum members, module names,
/// `require` references, and types.
pub(crate) fn is_string_literal_required(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(parent) = cx.parent(node) else {
        return false;
    };
    let abstract_member = |m: NodeId| match *ast.node(m) {
        Node::MethodDef { modifiers, .. } | Node::PropertyDef { modifiers, .. } => {
            modifiers.contains(Modifiers::ABSTRACT)
        }
        Node::PropertySignature { .. } | Node::MethodSignature { .. } => true,
        _ => false,
    };
    match *ast.node(parent) {
        Node::ComputedKey { .. } => cx.parent(parent).is_some_and(abstract_member),
        Node::Property { key, .. }
        | Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. } => key == node,
        Node::ImportDecl { source, .. } | Node::ExportAll { source, .. } if source == node => true,
        Node::ExportNamed {
            source: Some(source),
            ..
        } => source == node,
        Node::ExportAll { exported, .. } => exported == Some(node),
        Node::ImportAttribute { .. }
        | Node::ImportSpecifier { .. }
        | Node::ExportSpecifier { .. }
        | Node::JsxAttr { .. }
        | Node::EnumMember { .. }
        | Node::ExternalModuleRef { .. } => true,
        Node::Module { name, .. } => name == node,
        Node::VarDeclarator { type_ann, .. }
        | Node::Param { type_ann, .. }
        | Node::IndexSignature { type_ann, .. } => type_ann == Some(node),
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type == Some(node),
        Node::TypeAlias { ty, .. }
        | Node::As { ty, .. }
        | Node::Satisfies { ty, .. }
        | Node::TypeAssertion { ty, .. } => ty == node,
        _ => matches!(
            ast.kind(parent),
            NodeKind::TypeRef
                | NodeKind::TemplateLitType
                | NodeKind::ArrayType
                | NodeKind::TupleType
                | NodeKind::NamedTupleMember
                | NodeKind::OptionalType
                | NodeKind::RestType
                | NodeKind::UnionType
                | NodeKind::IntersectionType
                | NodeKind::FunctionType
                | NodeKind::ConstructorType
                | NodeKind::PropertySignature
                | NodeKind::MethodSignature
                | NodeKind::CallSignature
                | NodeKind::ConstructSignature
                | NodeKind::TypeOperator
                | NodeKind::IndexedAccessType
                | NodeKind::ConditionalType
                | NodeKind::MappedType
                | NodeKind::ParenType
                | NodeKind::TypePredicate
                | NodeKind::ImportType
                | NodeKind::TypeParam
                | NodeKind::TypeArgs
        ),
    }
}

/// `expect(x).toMatchInlineSnapshot(node)`.
pub(crate) fn is_jest_inline_snapshot(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let Some(call) = cx.parent(node).and_then(|p| method_call(ast, p)) else {
        return false;
    };
    call.name == "toMatchInlineSnapshot"
        && call.plain()
        && call.args == [node]
        && matches!(*ast.node(skip_parens(ast, call.obj)), Node::Call { callee, optional: false, args, .. }
            if ident_name(ast, skip_parens(ast, callee)) == Some("expect") && ast.list(args).len() == 1)
}

/// The file's first line break (ESLint's `getLinebreak`), `\n` without one.
pub(crate) fn linebreak(src: &str) -> &'static str {
    match src.find(['\n', '\r', '\u{2028}', '\u{2029}']) {
        Some(i) if src[i..].starts_with("\r\n") => "\r\n",
        Some(i) if src[i..].starts_with('\r') => "\r",
        Some(i) if src[i..].starts_with('\u{2028}') => "\u{2028}",
        Some(i) if src[i..].starts_with('\u{2029}') => "\u{2029}",
        _ => "\n",
    }
}

/// The fix removing argument `index` of the call or `new` `call`, with its parentheses and
/// one comma: the only argument takes a trailing comma along, the first of several the
/// comma after it and the spaces up to the next token or comment, any other the comma
/// before it. Comments in the removed range are kept (moved in front of the rest when the
/// first of several goes); a line comment gets a line break after it.
pub(crate) fn remove_argument(cx: &Ctx<'_, '_>, call: NodeId, index: usize) -> Option<Fix> {
    let ast = cx.ast();
    let src = cx.src();
    let args = match *ast.node(call) {
        Node::Call { args, .. } | Node::New { args, .. } => ast.list(args),
        _ => return None,
    };
    let arg = paren_span(cx, *args.get(index)?);
    let (mut start, mut end) = (arg.start, arg.end);
    let comma_after = || {
        let at = skip_trivia_forward(src, arg.end as usize);
        (src.as_bytes().get(at) == Some(&b',')).then_some(at as u32)
    };
    let mut first_of_several = None;
    if args.len() == 1 {
        if let Some(c) = comma_after() {
            end = c + 1;
        }
    } else if index == 0 {
        let c = comma_after()?;
        end = skip_spaces(src, c as usize + 1) as u32;
        first_of_several = Some(c);
    } else {
        let at = skip_trivia_backward(ast, src, arg.start as usize);
        if at == 0 || src.as_bytes()[at - 1] != b',' {
            return None;
        }
        start = at as u32 - 1;
    }
    let range = Span::new(start, end);
    let comments: Vec<_> = ast
        .comments_in(range)
        .iter()
        .filter(|c| c.span.end <= end)
        .collect();
    if comments.is_empty() {
        return Some(Fix::replace(range, ""));
    }
    let text = |c: &ts_r_ast::Comment| {
        let t = c.span.text(src);
        if matches!(c.kind, ts_r_ast::CommentKind::Line) {
            format!("{t}{}", linebreak(src))
        } else {
            t.to_owned()
        }
    };
    let replacement = match first_of_several {
        Some(comma) => {
            let inside: Vec<String> = comments
                .iter()
                .filter(|c| c.span.end <= arg.end)
                .map(|c| text(c))
                .collect();
            format!(
                "{}{}{}",
                inside.join(" "),
                &src[arg.end as usize..comma as usize],
                &src[comma as usize + 1..end as usize]
            )
        }
        None => comments.iter().map(|c| format!(" {}", text(c))).collect(),
    };
    Some(Fix::replace(range, replacement))
}

/// `node` (through parentheses and TS wrappers) is written to: an assignment or default
/// target, an update or `delete` operand, a destructuring element or property value, or a
/// `for-in`/`for-of` head.
pub(crate) fn is_left_hand_side(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    let parent = loop {
        let Some(p) = cx.parent(cur) else {
            return false;
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
    match *ast.node(parent) {
        Node::Update { arg, .. } => arg == cur,
        Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } => arg == cur,
        Node::Assign { left, .. } if left == cur => true,
        _ => is_assignment_target(ast, cx.file.parents, cur),
    }
}

/// Some node in the subtree under `root` (itself included) satisfies `pred`.
pub(crate) fn contains_node(ast: &Ast, root: NodeId, pred: impl Fn(&Node) -> bool) -> bool {
    struct Find<F> {
        pred: F,
        found: bool,
    }
    impl<F: Fn(&Node) -> bool> ts_r_ast::visit::Visitor for Find<F> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            if (self.pred)(ast.node(id)) {
                self.found = true;
                return ts_r_ast::visit::Walk::Stop;
            }
            ts_r_ast::visit::Walk::Continue
        }
    }
    let mut f = Find { pred, found: false };
    ts_r_ast::visit::walk(ast, root, &mut f);
    f.found
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The fix removing the statements `first..=last` (`removeStatement`): when the token
/// before would then continue into what follows, they become `;`; otherwise, when nothing
/// but spaces shares their lines, the whole lines go (with the line break before them, or
/// after them on the first line). `preserve_semicolon` keeps the last statement's `;`.
pub(crate) fn remove_statements(
    cx: &Ctx<'_, '_>,
    first: NodeId,
    last: NodeId,
    preserve_semicolon: bool,
) -> Fix {
    let src = cx.src();
    let start0 = cx.span(first).start;
    let last_span = cx.span(last);
    if super::unicorn_text::needs_semicolon(cx, start0, "(") {
        return Fix::replace(Span::new(start0, last_span.end), ";");
    }
    let mut end = last_span.end;
    if preserve_semicolon && src[..end as usize].ends_with(';') {
        end = skip_trivia_backward(cx.ast(), src, end as usize - 1) as u32;
    }
    let mut start = start0;
    let line_start = src[..start as usize]
        .rfind(is_line_break)
        .map_or(0, |i| i + src[i..].chars().next().map_or(1, char::len_utf8));
    let line_end = src[end as usize..]
        .find(is_line_break)
        .map_or(src.len(), |i| end as usize + i);
    let blank = |t: &str| t.chars().all(is_js_space);
    if blank(&src[line_start..start as usize]) && blank(&src[end as usize..line_end]) {
        end = line_end as u32;
        start = line_start as u32;
        if start == 0 {
            let rest = &src[end as usize..];
            end += if rest.starts_with("\r\n") {
                2
            } else {
                rest.chars().next().map_or(0, |c| c.len_utf8() as u32)
            };
        } else {
            let before = &src[..start as usize];
            start -= if before.ends_with("\r\n") {
                2
            } else {
                before
                    .chars()
                    .next_back()
                    .map_or(0, |c| c.len_utf8() as u32)
            };
        }
    }
    Fix::replace(Span::new(start, end), "")
}

/// The subtree under `root` uses the enclosing function's `this`, `super` or `new.target`
/// (not counting non-arrow functions, which rebind them, nor class bodies apart from
/// `extends` clauses and computed keys).
pub(crate) fn contains_lexical_this(ast: &Ast, root: NodeId) -> bool {
    use ts_r_ast::visit::{Visitor, Walk, walk};
    struct Find {
        found: bool,
    }
    impl Visitor for Find {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
            match *ast.node(id) {
                Node::This {}
                | Node::Super {}
                | Node::MetaProperty {
                    kind: ts_r_ast::MetaPropKind::NewTarget,
                } => {
                    self.found = true;
                    Walk::Stop
                }
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => Walk::SkipChildren,
                Node::ClassDecl {
                    heritage, members, ..
                }
                | Node::ClassExpr {
                    heritage, members, ..
                } => {
                    let mut parts: Vec<NodeId> = Vec::new();
                    for &h in ast.list(heritage) {
                        if matches!(
                            *ast.node(h),
                            Node::HeritageClause {
                                kind: ts_r_ast::HeritageKind::Extends,
                                ..
                            }
                        ) {
                            parts.push(h);
                        }
                    }
                    for &m in ast.list(members) {
                        let key = match *ast.node(m) {
                            Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
                            _ => continue,
                        };
                        if matches!(ast.node(key), Node::ComputedKey { .. }) {
                            parts.push(key);
                        }
                    }
                    for part in parts {
                        if !walk(ast, part, self) {
                            return Walk::Stop;
                        }
                    }
                    Walk::SkipChildren
                }
                _ => Walk::Continue,
            }
        }
    }
    let mut f = Find { found: false };
    walk(ast, root, &mut f);
    f.found
}

/// `isValueNotUsable`: the value of `node` is thrown away (an expression statement, or a
/// sequence element whose value is not passed on).
pub(crate) fn is_value_not_usable(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = node;
    for _ in 0..100_000 {
        let mut p = match cx.parent(cur) {
            Some(p) => p,
            None => return false,
        };
        while matches!(ast.node(p), Node::Paren { .. }) {
            cur = p;
            p = match cx.parent(p) {
                Some(q) => q,
                None => return false,
            };
        }
        match *ast.node(p) {
            Node::ExprStmt { .. } => return true,
            Node::Seq { exprs } => {
                if ast.list(exprs).last() != Some(&cur) {
                    return true;
                }
                cur = p;
            }
            _ => return false,
        }
    }
    false
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
    fn js_space_matches_regexp_backslash_s() {
        for c in ['\t', '\u{b}', '\u{a0}', '\u{2007}', '\u{3000}', '\u{feff}'] {
            assert!(is_js_space(c), "{c:?}");
        }
        for c in ['a', '\u{85}', '\u{200b}', '0'] {
            assert!(!is_js_space(c), "{c:?}");
        }
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
