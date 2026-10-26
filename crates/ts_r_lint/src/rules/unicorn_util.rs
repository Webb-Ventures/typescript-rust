//! Helpers shared by the eslint-plugin-unicorn rules: following references to globals
//! (`Intl.Collator`, `globalThis.Array`, `const {Array} = globalThis`) and the fixes that
//! turn a call into a `new` expression and back.

use super::prelude::*;
use super::unicorn_static::static_value;
use crate::semantic::Semantic;
use ts_r_ast::Ast;

/// Names that stand for the global object itself.
const GLOBAL_OBJECTS: &[&str] = &["globalThis", "global", "self", "window"];

/// The dotted global path `expr` refers to (`Array`, `Intl.Collator`), when it reads a
/// global that the file neither declares nor writes, possibly through `globalThis`-like
/// objects, property accesses, and variables initialised from such a path (`const A =
/// Array`, `const {Array: A} = globalThis`). Like ESLint's reference tracker, values pass
/// through `?:` branches, `||`/`&&`/`??`, the last expression of a sequence and TS
/// wrappers. `None` for the global object itself and for anything else.
pub(crate) fn global_path(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<String> {
    let mut out = None;
    path_of(cx, expr, 0, &mut |p| {
        if out.is_none() && !p.is_empty() {
            out = Some(p.join("."));
        }
    });
    out
}

/// True when `expr` is the global object (`globalThis`, `window`, ... or a variable
/// initialised from one), as ESLint's reference tracker follows it.
pub(crate) fn is_global_object(cx: &Ctx<'_, '_>, expr: NodeId) -> bool {
    let mut out = false;
    path_of(cx, expr, 0, &mut |p| out |= p.is_empty());
    out
}

/// Calls `found` with each path `expr` can carry (empty for the global object).
fn path_of(cx: &Ctx<'_, '_>, expr: NodeId, depth: u32, found: &mut dyn FnMut(&[String])) {
    if depth > 16 {
        return;
    }
    let ast = cx.ast();
    let expr = skip_wrappers(ast, expr);
    match *ast.node(expr) {
        Node::Conditional { cons, alt, .. } => {
            path_of(cx, cons, depth + 1, found);
            path_of(cx, alt, depth + 1, found);
        }
        Node::Binary {
            left,
            op: BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce,
            right,
        } => {
            path_of(cx, left, depth + 1, found);
            path_of(cx, right, depth + 1, found);
        }
        Node::Seq { exprs } => {
            if let Some(&last) = ast.list(exprs).last() {
                path_of(cx, last, depth + 1, found);
            }
        }
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            let Some(name) = member_name(ast, cx.src(), expr) else {
                return;
            };
            if matches!(ast.node(expr), Node::Index { index, .. }
                if !matches!(ast.node(skip_parens(ast, *index)), Node::StringLit { .. } | Node::NumberLit { .. } | Node::TemplateLit { .. }))
            {
                return;
            }
            path_of(cx, obj, depth + 1, &mut |p| {
                let mut p = p.to_vec();
                p.push(name.to_string());
                found(&p);
            });
        }
        Node::Ident { .. } => ident_path(cx, expr, depth, found),
        _ => {}
    }
}

fn ident_path(cx: &Ctx<'_, '_>, ident: NodeId, depth: u32, found: &mut dyn FnMut(&[String])) {
    let ast = cx.ast();
    let Some(name) = ident_name(ast, ident) else {
        return;
    };
    let sem = cx.semantic();
    let Some(r) = sem.reference_at(ident) else {
        return;
    };
    let reference = sem.reference(r);
    if !reference.is_read() {
        return;
    }
    let local = match reference.resolution {
        Resolution::Global => None,
        Resolution::Symbol(s) if sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL) => None,
        Resolution::Symbol(s) => Some(s),
        Resolution::Unresolved => return,
    };
    let Some(sym) = local else {
        if is_modified_global(cx, name) {
            return;
        }
        if GLOBAL_OBJECTS.contains(&name) {
            found(&[]);
        } else {
            found(&[name.to_owned()]);
        }
        return;
    };
    // A local: follow the values written to it.
    let s = sem.symbol(sym);
    for decl in &s.decls {
        let Node::VarDeclarator {
            init: Some(init), ..
        } = *ast.node(decl.node)
        else {
            continue;
        };
        let Some(keys) = pattern_keys(cx, decl.name, decl.node) else {
            continue;
        };
        path_of(cx, init, depth + 1, &mut |p| {
            let mut p = p.to_vec();
            p.extend(keys.iter().cloned());
            found(&p);
        });
    }
    for &rid in &s.refs {
        let rf = sem.reference(rid);
        let Some(parent) = cx.parent(rf.node) else {
            continue;
        };
        if let Node::Assign { left, right, .. } = *ast.node(parent)
            && left == rf.node
        {
            path_of(cx, right, depth + 1, found);
        }
    }
}

/// The property keys from a declarator's pattern down to the binding `name`
/// (`{Array: A}` gives `["Array"]`), or `None` through array patterns or computed keys.
fn pattern_keys(cx: &Ctx<'_, '_>, name: NodeId, declarator: NodeId) -> Option<Vec<String>> {
    let ast = cx.ast();
    let mut keys = Vec::new();
    let mut cur = name;
    for _ in 0..64 {
        let parent = cx.parent(cur)?;
        if parent == declarator {
            keys.reverse();
            return Some(keys);
        }
        match *ast.node(parent) {
            Node::BindingProperty { key, .. } => {
                if matches!(ast.node(key), Node::ComputedKey { .. }) {
                    let Node::ComputedKey { expr } = *ast.node(key) else {
                        return None;
                    };
                    if !matches!(
                        ast.node(skip_parens(ast, expr)),
                        Node::StringLit { .. } | Node::NumberLit { .. }
                    ) {
                        return None;
                    }
                }
                keys.push(key_name(ast, cx.src(), key)?.into_owned());
            }
            Node::ObjectPattern { .. } | Node::AssignPattern { .. } => {}
            _ => return None,
        }
        cur = parent;
    }
    None
}

/// A global the file writes to (`Array = x`) doesn't count as the built-in.
pub(crate) fn is_modified_global(cx: &Ctx<'_, '_>, name: &str) -> bool {
    let sem = cx.semantic();
    let ast = cx.ast();
    sem.references().iter().any(|r| {
        r.is_write()
            && !matches!(r.resolution, Resolution::Symbol(s) if !sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL))
            && ast.atom(r.name) == name
    })
}

/// Strips parentheses and the TS wrappers values pass through.
pub(crate) fn skip_wrappers(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => return id,
        }
    }
}

/// An optional link (`?.`) anywhere in the chain `id` ends (parentheses and TS wrappers
/// are looked through).
pub(crate) fn has_optional_chain(ast: &Ast, id: NodeId) -> bool {
    let mut id = id;
    for _ in 0..100_000 {
        id = skip_wrappers(ast, id);
        match *ast.node(id) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            _ => return false,
        }
    }
    false
}

/// The span of `id` with all the parentheses around it.
pub(crate) fn parenthesized_span(cx: &Ctx<'_, '_>, id: NodeId) -> Span {
    let mut id = id;
    while let Some(p) = cx.parent(id) {
        if !matches!(cx.node(p), Node::Paren { .. }) {
            break;
        }
        id = p;
    }
    cx.span(id)
}

/// Words a token can't be glued to: lowercase keywords plus `of`, `await`, `as`,
/// `satisfies`.
fn is_keyword_like(word: &str) -> bool {
    matches!(
        word,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "let"
            | "static"
            | "of"
            | "await"
            | "as"
            | "satisfies"
    )
}

/// Edits adding a space between `node` (with its parentheses) and a keyword glued to it
/// (`return(a)` / `(a)in b`), so replacing the node can't merge them.
pub(crate) fn space_around_keyword(cx: &Ctx<'_, '_>, node: NodeId) -> Vec<(Span, String)> {
    let src = cx.src();
    let range = parenthesized_span(cx, node);
    let mut edits = Vec::new();
    let before = &src[..range.start as usize];
    if before.ends_with(is_ident_char) {
        let word_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident_char(c))
            .last()
            .map_or(0, |(i, _)| i);
        if is_keyword_like(&before[word_start..]) {
            edits.push((Span::at(range.start), " ".to_owned()));
        }
    }
    let after = &src[range.end as usize..];
    if after.starts_with(is_ident_char) {
        let len = after
            .char_indices()
            .find(|&(_, c)| !is_ident_char(c))
            .map_or(after.len(), |(i, _)| i);
        if is_keyword_like(&after[..len]) {
            edits.push((Span::at(range.end), " ".to_owned()));
        }
    }
    edits
}

/// Adds `edits` to `fix`.
pub(crate) fn with_edits(mut fix: Fix, edits: Vec<(Span, String)>) -> Fix {
    for (span, text) in edits {
        fix = fix.and(span, text);
    }
    fix
}

/// `Foo()` -> `new Foo()`: inserts `new `, spaces it from a glued keyword, and wraps a
/// member callee whose object chain contains a call (`a().b()` -> `new (a().b)()`).
pub(crate) fn call_to_new(cx: &Ctx<'_, '_>, call: NodeId) -> Fix {
    let ast = cx.ast();
    let start = cx.span(call).start;
    let mut fix = Fix::insert(start, "new ");
    let edits = space_around_keyword(cx, call);
    // A space glued before the node goes before `new `.
    let mut fix_edits = Vec::new();
    for (span, text) in edits {
        if span.start == start {
            fix = Fix::insert(start, format!("{text}new "));
        } else {
            fix_edits.push((span, text));
        }
    }
    fix = with_edits(fix, fix_edits);
    if let Node::Call { callee, .. } = *ast.node(call)
        && !matches!(ast.node(callee), Node::Paren { .. })
        && member_contains_call(ast, callee)
    {
        let s = cx.span(callee);
        fix = fix.and(Span::at(s.start), "(").and(Span::at(s.end), ")");
    }
    fix
}

/// A member expression whose object chain (through members, tags and TS wrappers, not
/// parentheses) reaches a call.
fn member_contains_call(ast: &Ast, callee: NodeId) -> bool {
    let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(callee) else {
        return false;
    };
    let mut cur = obj;
    for _ in 0..100_000 {
        match *ast.node(cur) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => cur = expr,
            Node::Member { obj, .. } | Node::Index { obj, .. } => cur = obj,
            Node::TaggedTemplate { tag, .. } => cur = tag,
            Node::Call { .. } => return true,
            _ => return false,
        }
    }
    false
}

/// `new Foo()` -> `Foo()`: removes `new` and the spaces after it, adds `()` when the `new`
/// had no argument list, and wraps the result when removing `new` would change how it
/// parses (inside an unparenthesized `new` callee, or a `return`/`throw`/`yield` argument
/// whose callee starts on a later line). Adds a `;` when the statement would otherwise join
/// the previous one.
pub(crate) fn new_to_call(cx: &Ctx<'_, '_>, new: NodeId) -> Fix {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(new);
    let Node::New {
        callee, has_args, ..
    } = *ast.node(new)
    else {
        return Fix::default();
    };
    let new_kw = Span::new(span.start, span.start + 3);
    let callee_start = cx.span(skip_parens(ast, callee)).start;
    let same_line = !has_newline(src, new_kw.end, callee_start);
    let parenthesize = in_unparenthesized_new_callee(cx, new)
        || (!same_line && keyword_argument_parent(cx, new).is_some());
    let after = &src[new_kw.end as usize..];
    let spaces = (after.len() - after.trim_start().len()) as u32;
    let mut fix = Fix::replace(
        Span::new(new_kw.start, new_kw.end + spaces),
        if parenthesize { "(" } else { "" },
    );
    if !has_args {
        fix = fix.and(Span::at(span.end), "()");
    }
    if parenthesize {
        fix = fix.and(Span::at(span.end), ")");
    }
    let next = src[(new_kw.end + spaces) as usize..].chars().next();
    if matches!(
        next,
        Some('[' | '(' | '/' | '`' | '+' | '-' | '*' | ',' | '.' | '<')
    ) && joins_previous_statement(cx, new)
    {
        fix = fix.and(Span::at(span.start), ";");
    }
    fix
}

/// `node` (not parenthesized) is, through members, tags and TS wrappers, the callee of a
/// `new`.
pub(crate) fn in_unparenthesized_new_callee(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..100_000 {
        let Some(parent) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(parent) {
            Node::New { callee, .. } if callee == cur => return true,
            Node::Member { obj, .. } | Node::Index { obj, .. } if obj == cur => {}
            Node::TaggedTemplate { tag, .. } if tag == cur => {}
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. }
                if expr == cur => {}
            _ => return false,
        }
        cur = parent;
    }
    false
}

/// `node` starts an expression statement that follows a statement ending without `;` in
/// an expression a `(`/`[`/... could continue.
pub(crate) fn joins_previous_statement(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let parents = cx.file.parents;
    if expr_start(ast, parents, node) != Some(ExprStart::Statement) {
        return false;
    }
    let mut stmt = node;
    while let Some(p) = parents.parent(stmt) {
        stmt = p;
        if matches!(ast.node(p), Node::ExprStmt { .. }) {
            break;
        }
    }
    prev_statement(ast, parents, stmt).is_some_and(|p| stmt_continuable(ast, cx.src(), p))
}

/// An error class name: capitalised words then `Error` (`^(?:[A-Z][\da-z]*)*Error$`).
pub(crate) fn is_error_name(name: &str) -> bool {
    let Some(prefix) = name.strip_suffix("Error") else {
        return false;
    };
    prefix.bytes().all(|c| c.is_ascii_alphanumeric())
        && prefix.bytes().next().is_none_or(|c| c.is_ascii_uppercase())
}

// ---- Reference equality, `&&` / `||` chains, token runs and what an expression evaluates ----

/// The static name of a member access: `a.b`, `a["b"]`, `a[0]`, `a[`b`]`.
pub(crate) fn static_property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    match *cx.node(id) {
        Node::Member { prop, .. } => match *cx.node(prop) {
            Node::Ident { name } => Some(cx.ast().atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => static_value(cx, index).and_then(|v| v.to_js_string()),
        _ => None,
    }
}

/// The operands of a chain of `op` (left to right, through parentheses, not through TS
/// wrappers): `a && (b && c)` gives `[a, b, c]`.
pub(crate) fn logical_operands(ast: &Ast, id: NodeId, op: BinaryOp) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        match *ast.node(skip_parens(ast, n)) {
            Node::Binary { left, op: o, right } if o == op => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(skip_parens(ast, n)),
        }
    }
    out
}

/// A logical expression that is not an operand of the same operator around it.
pub(crate) fn is_outermost_logical(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Node::Binary { op, .. } = *cx.node(id) else {
        return true;
    };
    let ast = cx.ast();
    let parent = parent_skip_parens(ast, cx.file.parents, id);
    !parent.is_some_and(|p| matches!(*ast.node(p), Node::Binary { op: o, .. } if o == op))
}

/// The runs of non-space text in `span` outside comments: removing each one removes the
/// tokens there while keeping the spaces and comments between them (as a fix removing
/// tokens one by one does).
pub(crate) fn token_runs(ast: &Ast, src: &str, span: Span) -> Vec<Span> {
    let comments = ast.comments_in(span);
    let mut out = Vec::new();
    let mut run: Option<u32> = None;
    let mut ci = 0;
    let mut pos = span.start as usize;
    let end = (span.end as usize).min(src.len());
    while pos < end {
        if let Some(c) = comments.get(ci)
            && c.span.start as usize <= pos
        {
            if let Some(s) = run.take() {
                out.push(Span::new(s, pos as u32));
            }
            pos = (c.span.end as usize).max(pos + 1);
            ci += 1;
            continue;
        }
        let Some(ch) = src[pos..].chars().next() else {
            break;
        };
        if super::unicorn_ast::is_js_space(ch) {
            if let Some(s) = run.take() {
                out.push(Span::new(s, pos as u32));
            }
        } else if run.is_none() {
            run = Some(pos as u32);
        }
        pos += ch.len_utf8();
    }
    if let Some(s) = run {
        out.push(Span::new(s, end as u32));
    }
    out
}

/// A fix deleting each of `spans`.
pub(crate) fn delete_all(spans: impl IntoIterator<Item = Span>) -> Option<Fix> {
    let mut it = spans.into_iter();
    let mut fix = Fix::delete(it.next()?);
    for s in it {
        fix = fix.and(s, "");
    }
    Some(fix)
}

/// A JSX attribute string's value as ESLint's parser gives it: numeric character references
/// (`&#49;`, `&#x31;`, at most 10 characters before the `;`) decoded. Named references are
/// kept as written.
pub(crate) fn decode_jsx_numeric_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find("&#") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let (hex, digits) = match after.strip_prefix(['x', 'X']) {
            Some(d) => (true, d),
            None => (false, after),
        };
        let len = digits
            .bytes()
            .take(10)
            .take_while(|b| {
                if hex {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
            .count();
        let decoded = (len > 0 && digits[len..].starts_with(';'))
            .then(|| u32::from_str_radix(&digits[..len], if hex { 16 } else { 10 }).ok())
            .flatten()
            .and_then(char::from_u32);
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &digits[len + 1..];
            }
            None => {
                out.push_str("&#");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `containsNode` over what evaluating `root` runs: some node satisfies `pred`, not looking
/// inside functions (other than the function node itself) or the values of non-static class
/// fields, whose computed keys still count.
pub(crate) fn evaluates_node(ast: &Ast, root: NodeId, pred: &dyn Fn(&Ast, NodeId) -> bool) -> bool {
    struct Find<'p> {
        pred: &'p dyn Fn(&Ast, NodeId) -> bool,
        found: bool,
        /// Nesting of instance-field keys walked from inside the walk.
        depth: u32,
    }
    impl ts_r_ast::visit::Visitor for Find<'_> {
        fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
            use ts_r_ast::visit::Walk;
            if (self.pred)(ast, id) {
                self.found = true;
                return Walk::Stop;
            }
            match *ast.node(id) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    Walk::SkipChildren
                }
                Node::PropertyDef { modifiers, key, .. }
                    if !modifiers.contains(Modifiers::STATIC) =>
                {
                    if matches!(ast.node(key), Node::ComputedKey { .. }) && self.depth < 64 {
                        self.depth += 1;
                        ts_r_ast::visit::walk(ast, key, self);
                        self.depth -= 1;
                        if self.found {
                            return Walk::Stop;
                        }
                    }
                    Walk::SkipChildren
                }
                _ => Walk::Continue,
            }
        }
    }
    let mut f = Find {
        pred,
        found: false,
        depth: 0,
    };
    ts_r_ast::visit::walk(ast, root, &mut f);
    f.found
}

/// A direct `eval(...)` call (not `eval?.()`; parentheses and TS wrappers around the callee
/// allowed), which sees the calling scope.
pub(crate) fn is_direct_eval_call(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::Call {
            callee,
            optional: false,
            ..
        } => ident_name(ast, skip_wrappers(ast, callee)) == Some("eval"),
        _ => false,
    }
}

/// `child` is a type (a literal there is a literal type) as the direct child of `parent`.
pub(crate) fn is_type_slot(ast: &Ast, parent: NodeId, child: NodeId) -> bool {
    match *ast.node(parent) {
        Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } => {
            ty == child
        }
        Node::TypeAlias { ty, .. } => ty == child,
        Node::VarDeclarator { type_ann, .. }
        | Node::Param { type_ann, .. }
        | Node::PropertyDef { type_ann, .. }
        | Node::PropertySignature { type_ann, .. }
        | Node::IndexSignature { type_ann, .. } => type_ann == Some(child),
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. }
        | Node::MethodSignature { return_type, .. }
        | Node::CallSignature { return_type, .. }
        | Node::ConstructSignature { return_type, .. } => return_type == Some(child),
        Node::FunctionType { return_type, .. } | Node::ConstructorType { return_type, .. } => {
            return_type == child
        }
        Node::UnionType { .. }
        | Node::IntersectionType { .. }
        | Node::ArrayType { .. }
        | Node::TupleType { .. }
        | Node::NamedTupleMember { .. }
        | Node::OptionalType { .. }
        | Node::RestType { .. }
        | Node::ParenType { .. }
        | Node::TypeOperator { .. }
        | Node::IndexedAccessType { .. }
        | Node::ConditionalType { .. }
        | Node::MappedType { .. }
        | Node::TemplateLitType { .. }
        | Node::TypeArgs { .. }
        | Node::TypeParam { .. }
        | Node::TypePredicate { .. } => true,
        _ => false,
    }
}

// ---- `await` and single-use `const` look-through, JSX entities, strict side effects, indentation ----

/// `expr` without parentheses and any number of `await`s around it.
pub(crate) fn skip_awaits(ast: &Ast, mut expr: NodeId) -> NodeId {
    loop {
        expr = skip_parens(ast, expr);
        match *ast.node(expr) {
            Node::Await { arg } => expr = arg,
            _ => return expr,
        }
    }
}

/// `expr` without `await`s, or, when that is an identifier, the initialiser (without
/// `await`s) of the `const` it names, provided the binding is a plain identifier declared
/// once before `expr` and read only here. `None` for any other identifier.
pub(crate) fn direct_or_single_use_const(cx: &Ctx<'_, '_>, expr: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let expr = skip_awaits(ast, expr);
    if !matches!(ast.node(expr), Node::Ident { .. }) {
        return Some(expr);
    }
    let sem = cx.semantic();
    let sym = sem.reference(sem.reference_at(expr)?).symbol()?;
    let sym = sem.symbol(sym);
    let [decl] = sym.decls[..] else {
        return None;
    };
    // The declaration's own initialising write counts as one of the two references.
    if sym.refs.len() != 2 || decl.kind != DeclKind::Const {
        return None;
    }
    let Node::VarDeclarator { name, init, .. } = *ast.node(decl.node) else {
        return None;
    };
    if name != decl.name || cx.span(name).start > cx.span(expr).start {
        return None;
    }
    if !matches!(
        ast.node(decl.parent),
        Node::VarDecl {
            kind: VarKind::Const,
            ..
        }
    ) {
        return None;
    }
    Some(skip_awaits(ast, init?))
}

/// The value of a JSX attribute string as ESTree parsers give it: character references
/// (`&#45;`, `&#x2D;`, and the HTML 4 names plus `&apos;`) decoded. A reference is at most
/// ten characters before its `;`; anything else stays as written.
pub(crate) fn decode_jsx_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after
            .char_indices()
            .take(11)
            .find(|&(_, c)| c == ';')
            .and_then(|(semi, _)| Some((semi, entity(&after[..semi])?)));
        match decoded {
            Some((semi, c)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(body: &str) -> Option<char> {
    if let Some(num) = body.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix('x') {
            Some(hex) => (hex, 16),
            None => (num, 10),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        // `String.fromCharCode` keeps the low 16 bits; a lone surrogate cannot be kept.
        let v = digits.chars().fold(0u32, |v, c| {
            (v.wrapping_mul(radix) + c.to_digit(radix).unwrap_or(0)) & 0xffff
        });
        return Some(char::from_u32(v).unwrap_or('\u{fffd}'));
    }
    let i = HTML_ENTITIES
        .binary_search_by(|(n, _)| (*n).cmp(body))
        .ok()?;
    char::from_u32(HTML_ENTITIES[i].1)
}

/// The HTML 4 named character references and `apos`, sorted by name (byte order).
const HTML_ENTITIES: &[(&str, u32)] = &[
    ("AElig", 198),
    ("Aacute", 193),
    ("Acirc", 194),
    ("Agrave", 192),
    ("Alpha", 913),
    ("Aring", 197),
    ("Atilde", 195),
    ("Auml", 196),
    ("Beta", 914),
    ("Ccedil", 199),
    ("Chi", 935),
    ("Dagger", 8225),
    ("Delta", 916),
    ("ETH", 208),
    ("Eacute", 201),
    ("Ecirc", 202),
    ("Egrave", 200),
    ("Epsilon", 917),
    ("Eta", 919),
    ("Euml", 203),
    ("Gamma", 915),
    ("Iacute", 205),
    ("Icirc", 206),
    ("Igrave", 204),
    ("Iota", 921),
    ("Iuml", 207),
    ("Kappa", 922),
    ("Lambda", 923),
    ("Mu", 924),
    ("Ntilde", 209),
    ("Nu", 925),
    ("OElig", 338),
    ("Oacute", 211),
    ("Ocirc", 212),
    ("Ograve", 210),
    ("Omega", 937),
    ("Omicron", 927),
    ("Oslash", 216),
    ("Otilde", 213),
    ("Ouml", 214),
    ("Phi", 934),
    ("Pi", 928),
    ("Prime", 8243),
    ("Psi", 936),
    ("Rho", 929),
    ("Scaron", 352),
    ("Sigma", 931),
    ("THORN", 222),
    ("Tau", 932),
    ("Theta", 920),
    ("Uacute", 218),
    ("Ucirc", 219),
    ("Ugrave", 217),
    ("Upsilon", 933),
    ("Uuml", 220),
    ("Xi", 926),
    ("Yacute", 221),
    ("Yuml", 376),
    ("Zeta", 918),
    ("aacute", 225),
    ("acirc", 226),
    ("acute", 180),
    ("aelig", 230),
    ("agrave", 224),
    ("alefsym", 8501),
    ("alpha", 945),
    ("amp", 38),
    ("and", 8743),
    ("ang", 8736),
    ("apos", 39),
    ("aring", 229),
    ("asymp", 8776),
    ("atilde", 227),
    ("auml", 228),
    ("bdquo", 8222),
    ("beta", 946),
    ("brvbar", 166),
    ("bull", 8226),
    ("cap", 8745),
    ("ccedil", 231),
    ("cedil", 184),
    ("cent", 162),
    ("chi", 967),
    ("circ", 710),
    ("clubs", 9827),
    ("cong", 8773),
    ("copy", 169),
    ("crarr", 8629),
    ("cup", 8746),
    ("curren", 164),
    ("dArr", 8659),
    ("dagger", 8224),
    ("darr", 8595),
    ("deg", 176),
    ("delta", 948),
    ("diams", 9830),
    ("divide", 247),
    ("eacute", 233),
    ("ecirc", 234),
    ("egrave", 232),
    ("empty", 8709),
    ("emsp", 8195),
    ("ensp", 8194),
    ("epsilon", 949),
    ("equiv", 8801),
    ("eta", 951),
    ("eth", 240),
    ("euml", 235),
    ("euro", 8364),
    ("exist", 8707),
    ("fnof", 402),
    ("forall", 8704),
    ("frac12", 189),
    ("frac14", 188),
    ("frac34", 190),
    ("frasl", 8260),
    ("gamma", 947),
    ("ge", 8805),
    ("gt", 62),
    ("hArr", 8660),
    ("harr", 8596),
    ("hearts", 9829),
    ("hellip", 8230),
    ("iacute", 237),
    ("icirc", 238),
    ("iexcl", 161),
    ("igrave", 236),
    ("image", 8465),
    ("infin", 8734),
    ("int", 8747),
    ("iota", 953),
    ("iquest", 191),
    ("isin", 8712),
    ("iuml", 239),
    ("kappa", 954),
    ("lArr", 8656),
    ("lambda", 955),
    ("lang", 9001),
    ("laquo", 171),
    ("larr", 8592),
    ("lceil", 8968),
    ("ldquo", 8220),
    ("le", 8804),
    ("lfloor", 8970),
    ("lowast", 8727),
    ("loz", 9674),
    ("lrm", 8206),
    ("lsaquo", 8249),
    ("lsquo", 8216),
    ("lt", 60),
    ("macr", 175),
    ("mdash", 8212),
    ("micro", 181),
    ("middot", 183),
    ("minus", 8722),
    ("mu", 956),
    ("nabla", 8711),
    ("nbsp", 160),
    ("ndash", 8211),
    ("ne", 8800),
    ("ni", 8715),
    ("not", 172),
    ("notin", 8713),
    ("nsub", 8836),
    ("ntilde", 241),
    ("nu", 957),
    ("oacute", 243),
    ("ocirc", 244),
    ("oelig", 339),
    ("ograve", 242),
    ("oline", 8254),
    ("omega", 969),
    ("omicron", 959),
    ("oplus", 8853),
    ("or", 8744),
    ("ordf", 170),
    ("ordm", 186),
    ("oslash", 248),
    ("otilde", 245),
    ("otimes", 8855),
    ("ouml", 246),
    ("para", 182),
    ("part", 8706),
    ("permil", 8240),
    ("perp", 8869),
    ("phi", 966),
    ("pi", 960),
    ("piv", 982),
    ("plusmn", 177),
    ("pound", 163),
    ("prime", 8242),
    ("prod", 8719),
    ("prop", 8733),
    ("psi", 968),
    ("quot", 34),
    ("rArr", 8658),
    ("radic", 8730),
    ("rang", 9002),
    ("raquo", 187),
    ("rarr", 8594),
    ("rceil", 8969),
    ("rdquo", 8221),
    ("real", 8476),
    ("reg", 174),
    ("rfloor", 8971),
    ("rho", 961),
    ("rlm", 8207),
    ("rsaquo", 8250),
    ("rsquo", 8217),
    ("sbquo", 8218),
    ("scaron", 353),
    ("sdot", 8901),
    ("sect", 167),
    ("shy", 173),
    ("sigma", 963),
    ("sigmaf", 962),
    ("sim", 8764),
    ("spades", 9824),
    ("sub", 8834),
    ("sube", 8838),
    ("sum", 8721),
    ("sup", 8835),
    ("sup1", 185),
    ("sup2", 178),
    ("sup3", 179),
    ("supe", 8839),
    ("szlig", 223),
    ("tau", 964),
    ("there4", 8756),
    ("theta", 952),
    ("thetasym", 977),
    ("thinsp", 8201),
    ("thorn", 254),
    ("tilde", 732),
    ("times", 215),
    ("trade", 8482),
    ("uArr", 8657),
    ("uacute", 250),
    ("uarr", 8593),
    ("ucirc", 251),
    ("ugrave", 249),
    ("uml", 168),
    ("upsih", 978),
    ("upsilon", 965),
    ("uuml", 252),
    ("weierp", 8472),
    ("xi", 958),
    ("yacute", 253),
    ("yen", 165),
    ("yuml", 255),
    ("zeta", 950),
    ("zwj", 8205),
    ("zwnj", 8204),
];

/// eslint-utils' `hasSideEffect` with `considerGetters` and `considerImplicitTypeConversion`:
/// calls, `new`, assignments, updates, `await`, `yield`, `import()`, `delete` and every
/// member access are side effects, and so are the converting operators (`+x`, `a + b`,
/// `a < b`, `k in o`, ...) unless their operands are literals, and computed keys that are
/// not literals. Function bodies are not looked into (a function declaration's is).
pub(crate) fn has_side_effect_strict(ast: &Ast, id: NodeId) -> bool {
    strict_effect(ast, id, 0)
}

fn is_literal(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
    )
}

fn computed_non_literal(ast: &Ast, key: NodeId) -> bool {
    matches!(*ast.node(key), Node::ComputedKey { expr } if !is_literal(ast, expr))
}

fn strict_effect(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return true;
    }
    match *ast.node(id) {
        Node::Call { .. }
        | Node::New { .. }
        | Node::Assign { .. }
        | Node::Update { .. }
        | Node::Await { .. }
        | Node::Yield { .. }
        | Node::ImportCall { .. }
        | Node::Member { .. }
        | Node::Index { .. } => return true,
        Node::FunctionExpr { .. } | Node::Arrow { .. } => return false,
        Node::Unary { op, arg } => {
            if op == UnaryOp::Delete
                || (matches!(
                    op,
                    UnaryOp::Minus | UnaryOp::Plus | UnaryOp::Not | UnaryOp::BitNot
                ) && !is_literal(ast, arg))
            {
                return true;
            }
        }
        Node::Binary { left, op, right } => {
            let converts = !matches!(
                op,
                BinaryOp::StrictEq
                    | BinaryOp::StrictNotEq
                    | BinaryOp::Exp
                    | BinaryOp::InstanceOf
                    | BinaryOp::LogicalAnd
                    | BinaryOp::LogicalOr
                    | BinaryOp::Coalesce
            );
            if converts && !(is_literal(ast, left) && is_literal(ast, right)) {
                return true;
            }
        }
        Node::Property { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertyDef { key, .. }
            if computed_non_literal(ast, key) =>
        {
            return true;
        }
        _ => {}
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        if !found && !is_type_node(ast, c) && strict_effect(ast, c, depth + 1) {
            found = true;
        }
    });
    found
}

/// Type annotations and arguments are never evaluated.
fn is_type_node(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::TypeArgs { .. }
            | Node::TypeParams { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::TypeLiteral { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::FunctionType { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::ParenType { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::MappedType { .. }
            | Node::ImportType { .. }
            | Node::TemplateLitType { .. }
    )
}

/// The static name of a non-computed property key (`a`, `"a"`, `0`), as `getPropertyName`
/// gives it.
pub(crate) fn plain_key_name(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => Some(ast.atom(name).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::BigIntLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    }
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The offset where the line holding `pos` starts.
pub(crate) fn line_start(src: &str, pos: usize) -> usize {
    src[..pos]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_line_break(c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// The spaces and tabs a line starts with (ESLint's `getLineIndent`).
pub(crate) fn line_indent(src: &str, pos: usize) -> &str {
    let start = line_start(src, pos);
    let rest = &src[start..];
    &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()]
}

/// The file's lines as ESLint splits them (`\r\n`, `\n`, `\r`, U+2028, U+2029), with their
/// start offsets.
fn source_lines(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = src.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_line_break(c) {
            out.push((start, &src[start..i]));
            let mut next = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                chars.next();
                next += 1;
            }
            start = next;
        }
    }
    out.push((start, &src[start..]));
    out
}

/// One level of indentation in the file (the plugin's `getIndentUnit`, built on
/// `detect-indent`): the most common change of indentation between consecutive non-empty
/// lines, spaces or tabs, with continuation lines of multi-line tokens ignored. A tab when
/// the file indents with tabs or not at all.
pub(crate) fn indent_unit(cx: &Ctx<'_, '_>) -> String {
    let ast = cx.ast();
    let src = cx.src();
    let lines = source_lines(src);
    let line_of = |pos: usize| lines.partition_point(|&(s, _)| s <= pos).saturating_sub(1);
    let mut blank = vec![false; lines.len()];
    let mut mark = |span: Span| {
        let (a, b) = (line_of(span.start as usize), line_of(span.end as usize));
        for flag in blank.iter_mut().take(b + 1).skip(a + 1) {
            *flag = true;
        }
    };
    for c in &ast.comments {
        mark(c.span);
    }
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        if matches!(
            ast.node(id),
            Node::StringLit { .. } | Node::TemplateElement { .. } | Node::JsxText {}
        ) {
            mark(cx.span(id));
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    let text: Vec<&str> = lines
        .iter()
        .zip(&blank)
        .map(|(&(_, l), &b)| if b { "" } else { l })
        .collect();
    let mut best = indent_stats(&text, true);
    if best.is_empty() {
        best = indent_stats(&text, false);
    }
    let mut result: Option<((bool, usize), (u32, u32))> = None;
    for &(key, (used, weight)) in &best {
        if result.is_none_or(|(_, (u, w))| used > u || (used == u && weight > w)) {
            result = Some((key, (used, weight)));
        }
    }
    match result {
        Some(((true, n), _)) => " ".repeat(n),
        _ => "\t".to_owned(),
    }
}

/// `(is_space, size)` keys with `(uses, weight)`, in first-seen order.
fn indent_stats(lines: &[&str], ignore_single_spaces: bool) -> Vec<((bool, usize), (u32, u32))> {
    let mut stats: Vec<((bool, usize), (u32, u32))> = Vec::new();
    let mut previous = 0usize;
    let mut previous_type: Option<bool> = None;
    let mut key: Option<(bool, usize)> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let tabs = line.len() - line.trim_start_matches('\t').len();
        if spaces == 0 && tabs == 0 {
            previous = 0;
            previous_type = None;
            continue;
        }
        let (is_space, indent) = if spaces > 0 {
            (true, spaces)
        } else {
            (false, tabs)
        };
        if ignore_single_spaces && is_space && indent == 1 {
            continue;
        }
        if previous_type != Some(is_space) {
            previous = 0;
        }
        previous_type = Some(is_space);
        let diff = indent as isize - previous as isize;
        previous = indent;
        let (used, weight) = if diff == 0 {
            (0, 1)
        } else {
            let abs = diff.unsigned_abs();
            if ignore_single_spaces && is_space && abs == 1 {
                continue;
            }
            key = Some((is_space, abs));
            (1, 0)
        };
        let Some(k) = key else {
            continue;
        };
        match stats.iter_mut().find(|(sk, _)| *sk == k) {
            Some((_, (u, w))) => {
                *u += used;
                *w += weight;
            }
            None => stats.push((k, (1, 0))),
        }
    }
    stats
}

// ---- Method-call shapes, free names and renames, ESTree identifier ranges, statement edits ----

/// `obj.method(args)`: a call whose callee is a non-computed member named `method`.
/// `args` is the exact argument count (a spread among those arguments fails the match, as
/// in the plugin); `optional_call: Some(false)` rejects `obj.method?.()`.
pub(crate) fn is_method_call(
    ast: &Ast,
    call: NodeId,
    methods: &[&str],
    args: Option<usize>,
    optional_call: Option<bool>,
) -> bool {
    let Node::Call {
        callee,
        optional,
        args: list,
        ..
    } = *ast.node(call)
    else {
        return false;
    };
    let Node::Member { prop, .. } = *ast.node(callee) else {
        return false;
    };
    if !methods.is_empty() && !ident_name(ast, prop).is_some_and(|n| methods.contains(&n)) {
        return false;
    }
    if !methods.is_empty() && matches!(ast.node(prop), Node::PrivateName { .. }) {
        return false;
    }
    if let Some(o) = optional_call
        && o != optional
    {
        return false;
    }
    let items = ast.list(list);
    if let Some(n) = args {
        if items.len() != n {
            return false;
        }
        if items
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
        {
            return false;
        }
    }
    true
}

/// Words the plugin never uses as a generated name: ECMAScript reserved words, the
/// restricted `arguments`/`eval`, the global value properties, and TypeScript's reserved
/// words.
const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "arguments",
    "eval",
    "globalThis",
    "Infinity",
    "NaN",
    "undefined",
    // TypeScript
    "as",
    "any",
    "boolean",
    "constructor",
    "declare",
    "get",
    "module",
    "require",
    "number",
    "set",
    "string",
    "symbol",
    "type",
    "from",
    "of",
];

/// A syntactically valid identifier that is not a reserved word.
pub(crate) fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '$' || first == '_' || ts_r_lexer::is_id_start(first)) {
        return false;
    }
    if !chars.all(|c| {
        c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}' || ts_r_lexer::is_id_part(c)
    }) {
        return false;
    }
    !RESERVED.contains(&name)
}

/// True when `inner` is `outer` or nested in it.
pub(crate) fn scope_within(sem: &dyn Semantic, mut inner: ScopeId, outer: ScopeId) -> bool {
    for _ in 0..10_000 {
        if inner == outer {
            return true;
        }
        match sem.scope(inner).parent {
            Some(p) => inner = p,
            None => return false,
        }
    }
    false
}

/// The plugin's choice of a new variable name: `name` (or `name_` when `name` is not a
/// valid identifier), with `_` appended until it is free in every scope of `scopes` and
/// `extra_ok` accepts it. `None` when even `name_` is not an identifier.
pub(crate) fn available_name(
    cx: &Ctx<'_, '_>,
    name: &str,
    scopes: &[ScopeId],
    extra_ok: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let mut name = name.to_owned();
    if !is_valid_identifier(&name) {
        name.push('_');
        if !is_valid_identifier(&name) {
            return None;
        }
    }
    let sem = cx.semantic();
    let ast = cx.ast();
    let unresolved: Vec<(ScopeId, &str)> = sem
        .references()
        .iter()
        .filter(|r| r.resolution == Resolution::Unresolved)
        .map(|r| (r.scope, ast.atom(r.name)))
        .collect();
    for _ in 0..1000 {
        let taken = sem.is_global(&name)
            || scopes.iter().any(|&s| sem.lookup(s, &name).is_some())
            || unresolved
                .iter()
                .any(|&(rs, n)| n == name && scopes.iter().any(|&s| scope_within(sem, rs, s)));
        if !taken && extra_ok(&name) {
            return Some(name);
        }
        name.push('_');
    }
    None
}

/// The edit that renames one identifier of a binding to `new`: shorthand properties and
/// patterns become `old: new`, a shorthand import `old as new`, a shorthand export
/// `new as old`.
pub(crate) fn rename_identifier_edit(cx: &Ctx<'_, '_>, ident: NodeId, new: &str) -> (Span, String) {
    let ast = cx.ast();
    let span = cx.span(ident);
    let old = cx.text(ident);
    if let Some(p) = cx.parent(ident) {
        match *ast.node(p) {
            Node::ShorthandProp { name, .. } if name == ident => {
                return (span, format!("{old}: {new}"));
            }
            Node::BindingProperty {
                key, value: None, ..
            } if key == ident => {
                return (span, format!("{old}: {new}"));
            }
            Node::ImportSpecifier {
                imported: None,
                local,
                ..
            } if local == ident => {
                return (span, format!("{old} as {new}"));
            }
            Node::ExportSpecifier {
                exported: None,
                local,
                ..
            } if local == ident => {
                return (span, format!("{new} as {old}"));
            }
            _ => {}
        }
    }
    (span, new.to_owned())
}

/// A fix renaming every declaration and reference of `sym` to `new`.
pub(crate) fn rename_symbol_fix(cx: &Ctx<'_, '_>, sym: SymbolId, new: &str) -> Fix {
    let sem = cx.semantic();
    let s = sem.symbol(sym);
    let mut idents: Vec<NodeId> = s.decls.iter().map(|d| d.name).collect();
    for &r in &s.refs {
        let n = sem.reference(r).node;
        if !idents.contains(&n) {
            idents.push(n);
        }
    }
    let mut edits: Vec<_> = idents
        .into_iter()
        .map(|i| rename_identifier_edit(cx, i, new))
        .collect();
    edits.sort_by_key(|(s, _)| (s.start, s.end));
    edits.dedup_by_key(|(s, _)| (s.start, s.end));
    Fix {
        edits: edits
            .into_iter()
            .map(|(span, text)| crate::diagnostic::Edit { span, text })
            .collect(),
    }
}

/// The ESTree range of a binding identifier: typescript-estree's `Identifier` also covers
/// a `?` and the type annotation of a parameter or catch binding.
pub(crate) fn estree_ident_span(cx: &Ctx<'_, '_>, ident: NodeId) -> Span {
    let ast = cx.ast();
    let span = cx.span(ident);
    let Some(p) = cx.parent(ident) else {
        return span;
    };
    let end = match *ast.node(p) {
        Node::Param {
            pattern,
            type_ann,
            optional,
            ..
        } if pattern == ident => match type_ann {
            Some(t) => cx.span(t).end,
            None if optional => find_char(cx.src(), span.end, b'?').map_or(span.end, |q| q + 1),
            None => span.end,
        },
        Node::CatchClause {
            param: Some(param),
            type_ann: Some(t),
            ..
        } if param == ident => cx.span(t).end,
        Node::VarDeclarator {
            name,
            type_ann: Some(t),
            ..
        } if name == ident => cx.span(t).end,
        _ => span.end,
    };
    Span::new(span.start, end)
}

/// The last comment after `end` (before the next token) that starts on the line `end` is
/// on: the plugin's "trailing comment on the same line".
pub(crate) fn trailing_comment_same_line(cx: &Ctx<'_, '_>, end: u32) -> Option<Span> {
    let next = skip_trivia_forward(cx.src(), end as usize) as u32;
    let line = cx.file.lines.line_of(end);
    cx.ast()
        .comments_between(end, next)
        .iter()
        .rev()
        .find(|c| cx.file.lines.line_of(c.span.start) == line)
        .map(|c| c.span)
}

/// The whitespace right before `pos` on its line.
pub(crate) fn indent_before<'a>(cx: &Ctx<'_, 'a>, pos: u32) -> &'a str {
    let lines = cx.file.lines;
    let start = lines.line_start(lines.line_of(pos)).unwrap_or(0);
    let before = cx.src().get(start as usize..pos as usize).unwrap_or("");
    let trimmed = before.trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    &before[trimmed.len()..]
}

/// `@eslint-community/eslint-utils`' `hasSideEffect`: calls, `new`, assignments, updates,
/// `await`, `yield`, `import()` and `delete` anywhere in `id` (not inside nested functions).
/// `getters` makes every member access count; `conversions` makes implicit type
/// conversions count (operators on non-literals, computed non-literal keys).
pub(crate) fn has_side_effect_with(
    ast: &Ast,
    id: NodeId,
    getters: bool,
    conversions: bool,
) -> bool {
    side_effect_in(ast, id, getters, conversions, 0)
}

fn side_effect_in(ast: &Ast, id: NodeId, getters: bool, conversions: bool, depth: u32) -> bool {
    if depth > 256 {
        return true;
    }
    match *ast.node(id) {
        Node::Arrow { .. } | Node::FunctionExpr { .. } => return false,
        Node::Assign { .. }
        | Node::Await { .. }
        | Node::Call { .. }
        | Node::ImportCall { .. }
        | Node::New { .. }
        | Node::Update { .. }
        | Node::Yield { .. } => return true,
        Node::Unary { op, arg } => {
            if op == UnaryOp::Delete {
                return true;
            }
            if conversions
                && matches!(
                    op,
                    UnaryOp::Minus | UnaryOp::Plus | UnaryOp::Not | UnaryOp::BitNot
                )
                && !is_literal(ast, arg)
            {
                return true;
            }
        }
        Node::Binary {
            left, op, right, ..
        } => {
            use BinaryOp::*;
            if conversions
                && matches!(
                    op,
                    Eq | NotEq
                        | Lt
                        | LtEq
                        | Gt
                        | GtEq
                        | Shl
                        | Shr
                        | UShr
                        | Add
                        | Sub
                        | Mul
                        | Div
                        | Mod
                        | BitOr
                        | BitXor
                        | BitAnd
                        | In
                )
                && !(is_literal(ast, left) && is_literal(ast, right))
            {
                return true;
            }
        }
        Node::Member { .. } if getters => return true,
        Node::Index { index, .. } => {
            if getters || (conversions && !is_literal(ast, index)) {
                return true;
            }
        }
        Node::ComputedKey { expr } if conversions && !is_literal(ast, expr) => return true,
        _ => {}
    }
    let mut found = false;
    ast.for_each_child(id, |c| {
        if !found && side_effect_in(ast, c, getters, conversions, depth + 1) {
            found = true;
        }
    });
    found
}

/// A `case`/`default` clause's head: from its keyword to the end of its colon.
pub(crate) fn switch_case_head(cx: &Ctx<'_, '_>, case: NodeId) -> Span {
    let src = cx.src();
    let span = cx.span(case);
    let test = match *cx.node(case) {
        Node::SwitchCase { test, .. } => test,
        _ => None,
    };
    let mut pos = match test {
        Some(t) => cx.span(t).end as usize,
        None => (span.start as usize + "default".len()).min(src.len()),
    };
    for _ in 0..src.len() {
        pos = skip_trivia_forward(src, pos);
        match src.as_bytes().get(pos) {
            Some(b':') => return Span::new(span.start, pos as u32 + 1),
            Some(_) => pos += 1,
            None => break,
        }
    }
    span
}

/// The `return`, `throw` or non-delegating `yield` whose unparenthesized argument starts
/// with `node` (through enclosing expressions that start where it does).
pub(crate) fn keyword_argument_parent(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = node;
    for _ in 0..100_000 {
        let parent = cx.parent(cur)?;
        match *cx.node(parent) {
            Node::Paren { .. } => return None,
            Node::Return { arg: Some(a) } | Node::Throw { arg: a } if a == cur => {
                return Some(parent);
            }
            Node::Yield {
                delegate: false,
                arg: Some(a),
            } if a == cur => return Some(parent),
            _ => {}
        }
        if cx.span(parent).start != cx.span(cur).start {
            return None;
        }
        cur = parent;
    }
    None
}

/// Edits wrapping the argument of a `return`/`throw` in parentheses: ` (` after the
/// keyword and `)` before the statement's `;` (or after it).
pub(crate) fn parenthesize_keyword_argument(cx: &Ctx<'_, '_>, stmt: NodeId) -> Vec<(Span, String)> {
    let span = cx.span(stmt);
    let kw = match cx.node(stmt) {
        Node::Return { .. } => "return",
        Node::Throw { .. } => "throw",
        _ => return Vec::new(),
    };
    let close = if cx.text(stmt).ends_with(';') {
        span.end - 1
    } else {
        span.end
    };
    vec![
        (Span::at(span.start + kw.len() as u32), " (".to_owned()),
        (Span::at(close), ")".to_owned()),
    ]
}

/// The end of the token before `pos` (comments skipped).
pub(crate) fn prev_token_end(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    skip_trivia_backward(cx.ast(), cx.src(), pos as usize) as u32
}

/// The start of the token at or after `pos` (comments skipped).
pub(crate) fn next_token_start(cx: &Ctx<'_, '_>, pos: u32) -> u32 {
    skip_trivia_forward(cx.src(), pos as usize) as u32
}

/// A fix made of `edits` (kept in source order).
pub(crate) fn fix_of(mut edits: Vec<(Span, String)>) -> Fix {
    edits.sort_by_key(|(s, _)| (s.start, s.end));
    Fix {
        edits: edits
            .into_iter()
            .map(|(span, text)| crate::diagnostic::Edit { span, text })
            .collect(),
    }
}

/// `isTypeScriptFile`: the path ends in `.ts`, `.mts`, `.cts` or `.tsx` (any case).
pub(crate) fn is_typescript_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// The singular of an English plural name (`items` → `item`, `cities` → `city`,
/// `boxes` → `box`), or `None` when the name doesn't look plural. Only the end of the name
/// changes, so `visibleItems` becomes `visibleItem`.
pub(crate) fn singular(name: &str) -> Option<String> {
    const IRREGULAR: &[(&str, &str)] = &[
        ("children", "child"),
        ("people", "person"),
        ("men", "man"),
        ("women", "woman"),
        ("mice", "mouse"),
        ("geese", "goose"),
        ("feet", "foot"),
        ("teeth", "tooth"),
        ("indices", "index"),
        ("matrices", "matrix"),
        ("vertices", "vertex"),
        ("leaves", "leaf"),
        ("knives", "knife"),
        ("lives", "life"),
        ("wolves", "wolf"),
        ("halves", "half"),
    ];
    const UNCHANGED: &[&str] = &[
        "news",
        "series",
        "species",
        "data",
        "sheep",
        "fish",
        "deer",
        "information",
        "status",
        "alias",
        "bus",
        "gas",
        "lens",
        "canvas",
        "atlas",
        "this",
        "is",
        "was",
        "has",
        "us",
    ];
    let lower = name.to_ascii_lowercase();
    let keep_case = |stem_len: usize, tail: &str| {
        let mut out = name[..stem_len].to_owned();
        let upper =
            name[stem_len..].chars().all(|c| c.is_ascii_uppercase()) && name.len() > stem_len + 1;
        out.push_str(&if upper {
            tail.to_ascii_uppercase()
        } else {
            tail.to_owned()
        });
        out
    };
    for &(plural, single) in IRREGULAR {
        if lower.ends_with(plural) {
            let stem = name.len() - plural.len();
            // A whole word, or a camelCase segment: `childNodes` is not `childNode`s.
            let boundary = stem == 0 || name.as_bytes()[stem].is_ascii_uppercase();
            if boundary {
                let first = &name[stem..=stem];
                let mut tail = single.to_owned();
                if first.chars().all(|c| c.is_ascii_uppercase()) {
                    tail[..1].make_ascii_uppercase();
                }
                return Some(keep_case(stem, &tail));
            }
        }
    }
    if UNCHANGED.iter().any(|u| lower.ends_with(u)) || !name.is_ascii() {
        return None;
    }
    let n = lower.len();
    let ends = |s: &str| lower.ends_with(s);
    let result = if ends("ss") || ends("us") || ends("is") || !ends("s") || n < 2 {
        return None;
    } else if n > 3
        && ends("ies")
        && !matches!(lower.as_bytes()[n - 4], b'a' | b'e' | b'i' | b'o' | b'u')
    {
        keep_case(n - 3, "y")
    } else if ["sses", "xes", "ches", "shes", "zzes"]
        .iter()
        .any(|e| ends(e))
        || (ends("uses") && n > 4)
    {
        keep_case(n - 2, "")
    } else {
        keep_case(n - 1, "")
    };
    (result != name).then_some(result)
}

// ---- Fix text: comments, call parentheses, side effects, boolean contexts, member objects, semicolons ----

/// True when replacing `range` would drop a comment: one lies wholly inside `range` but
/// outside every `kept` range.
pub(crate) fn would_remove_comments(cx: &Ctx<'_, '_>, range: Span, kept: &[Span]) -> bool {
    cx.ast().comments_in(range).iter().any(|c| {
        c.span.end <= range.end
            && !kept
                .iter()
                .any(|k| c.span.start >= k.start && c.span.end <= k.end)
    })
}

/// True when `id` is wrapped in at least one pair of parentheses.
pub(crate) fn is_parenthesized(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    cx.parent(id)
        .is_some_and(|p| matches!(cx.node(p), Node::Paren { .. }))
}

/// The text of `id` to be moved somewhere else: without redundant parentheses unless they
/// hold comments (then the parenthesized text as written), wrapped in parentheses when
/// `needs_parens`.
pub(crate) fn expression_text(cx: &Ctx<'_, '_>, id: NodeId, needs_parens: bool) -> String {
    if is_parenthesized(cx, id) {
        let outer = parenthesized_span(cx, id);
        if would_remove_comments(cx, outer, &[cx.span(id)]) {
            return outer.text(cx.src()).to_owned();
        }
    }
    let text = cx.text(id);
    if needs_parens {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// [`expression_text`] for a single call argument: a sequence gets parentheses.
pub(crate) fn call_argument_text(cx: &Ctx<'_, '_>, id: NodeId) -> String {
    let seq = matches!(cx.node(id), Node::Seq { .. });
    expression_text(cx, id, seq)
}

/// The `(` and `)` of a call or `new` expression's argument list, as spans of one byte.
pub(crate) fn call_parens(cx: &Ctx<'_, '_>, call: NodeId) -> Option<(Span, Span)> {
    let src = cx.src();
    let (callee, type_args) = match *cx.node(call) {
        Node::Call {
            callee, type_args, ..
        } => (callee, type_args),
        Node::New {
            callee,
            type_args,
            has_args: true,
            ..
        } => (callee, type_args),
        _ => return None,
    };
    let mut from = parenthesized_span(cx, callee).end;
    if let Some(t) = type_args {
        from = from.max(cx.span(t).end);
    }
    let mut open = skip_trivia_forward(src, from as usize);
    if src.as_bytes().get(open..open + 2) == Some(b"?.") {
        open = skip_trivia_forward(src, open + 2);
    }
    if src.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let end = cx.span(call).end as usize;
    if end == 0 || src.as_bytes().get(end - 1) != Some(&b')') {
        return None;
    }
    Some((
        Span::new(open as u32, open as u32 + 1),
        Span::new(end as u32 - 1, end as u32),
    ))
}

/// The source between a call's parentheses (a trailing comma included).
pub(crate) fn call_arguments_text<'a>(cx: &Ctx<'_, 'a>, call: NodeId) -> Option<&'a str> {
    let (open, close) = call_parens(cx, call)?;
    cx.src().get(open.end as usize..close.start as usize)
}

/// The span of the operator `op` that follows `left` (with its parentheses).
pub(crate) fn operator_after(cx: &Ctx<'_, '_>, left: NodeId, op: &str) -> Option<Span> {
    let src = cx.src();
    let at = skip_trivia_forward(src, parenthesized_span(cx, left).end as usize);
    src[at..]
        .starts_with(op)
        .then(|| Span::new(at as u32, (at + op.len()) as u32))
}

/// A literal number with the given value (through parentheses).
pub(crate) fn is_number_literal(cx: &Ctx<'_, '_>, id: NodeId, value: f64) -> bool {
    matches!(*cx.node(skip_parens(cx.ast(), id)), Node::NumberLit { value: v } if v.get() == value)
}

/// unicorn's `hasPossibleSideEffect`: [`has_side_effect`], or a tagged template or spread
/// anywhere inside (even in nested functions).
pub(crate) fn has_possible_side_effect(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    fn contains(ast: &ts_r_ast::Ast, id: NodeId, depth: u32) -> bool {
        if depth > 500 {
            return true;
        }
        if matches!(
            ast.node(id),
            Node::TaggedTemplate { .. } | Node::Spread { .. }
        ) {
            return true;
        }
        let mut found = false;
        ast.for_each_child(id, |c| found = found || contains(ast, c, depth + 1));
        found
    }
    super::unicorn_static::has_side_effect(cx.ast(), id) || contains(cx.ast(), id, 0)
}

/// unicorn's `isSideEffectFreeFunction`: an inline arrow or function expression whose
/// parameters and body have no possible side effect.
pub(crate) fn is_side_effect_free_function(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let id = skip_parens(ast, id);
    let (params, body) = match *ast.node(id) {
        Node::Arrow { params, body, .. } => (params, Some(body)),
        Node::FunctionExpr { params, body, .. } => (params, body),
        _ => return false,
    };
    ast.list(params)
        .iter()
        .chain(body.as_ref())
        .all(|&c| !has_possible_side_effect(cx, c))
}

/// A call `obj.name(...)` in unicorn's `isMethodCall` sense: not an optional call, a
/// non-computed member (parentheses around the callee are looked through), `name` one of
/// `names`, an argument count in `min..=max`, and no spread among the first `max`
/// arguments when `max` is given. `optional_member: false` also rejects `obj?.name`.
pub(crate) fn method_call_of<'a>(
    ast: &'a ts_r_ast::Ast,
    id: NodeId,
    names: &[&str],
    min: usize,
    max: Option<usize>,
    optional_member: Option<bool>,
) -> Option<(NodeId, NodeId, &'a [NodeId])> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        obj,
        prop,
        optional,
    } = *ast.node(skip_parens(ast, callee))
    else {
        return None;
    };
    if optional_member == Some(false) && optional {
        return None;
    }
    let name = ident_name(ast, prop)?;
    if !names.contains(&name) {
        return None;
    }
    let args = ast.list(args);
    if args.len() < min || max.is_some_and(|m| args.len() > m) {
        return None;
    }
    if let Some(m) = max
        && args
            .iter()
            .take(m)
            .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some((obj, prop, args))
}

/// The parent of `id` through parentheses, and the outermost parenthesized form of `id`
/// (the node the parent holds).
pub(crate) fn parent_through_parens(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(NodeId, NodeId)> {
    let mut cur = id;
    for _ in 0..10_000 {
        let p = cx.parent(cur)?;
        if matches!(cx.node(p), Node::Paren { .. }) {
            cur = p;
            continue;
        }
        return Some((p, cur));
    }
    None
}

/// unicorn's `isGlobalBooleanCall`: `Boolean(x)` with one non-spread argument, not an
/// optional call, `Boolean` not declared in scope.
pub(crate) fn is_global_boolean_call(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let &[arg] = ast.list(args) else {
        return false;
    };
    if ident_name(ast, callee) != Some("Boolean") || matches!(ast.node(arg), Node::Spread { .. }) {
        return false;
    }
    let sem = cx.semantic();
    sem.symbol_of_ident(callee)
        .is_none_or(|s| sem.symbol(s).decls.is_empty())
}

fn is_logical(node: &Node) -> bool {
    matches!(
        node,
        Node::Binary {
            op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
            ..
        }
    )
}

/// unicorn's `isBooleanContext`: `id`'s value is only used for its truthiness: the operand
/// of `!`, the argument of `Boolean(...)`, or a control-flow test (`if`, `?:`, `while`,
/// `do-while`, `for`), through `&&` / `||` operands (TS wrappers are not looked through).
pub(crate) fn is_boolean_context(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..10_000 {
        if is_global_boolean_call(cx, cur) {
            return true;
        }
        let Some((parent, held)) = parent_through_parens(cx, cur) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Unary {
                op: UnaryOp::Not, ..
            } => return true,
            Node::Call { .. } if is_global_boolean_call(cx, parent) => {
                return cx.ast().list(match *cx.node(parent) {
                    Node::Call { args, .. } => args,
                    _ => return false,
                })[0]
                    == held;
            }
            Node::If { test, .. }
            | Node::Conditional { test, .. }
            | Node::While { test, .. }
            | Node::DoWhile { test, .. } => return test == held,
            Node::For { test, .. } => return test == Some(held),
            ref n if is_logical(n) => cur = parent,
            _ => return false,
        }
    }
    false
}

/// An optional link (`?.`) in the chain `id` ends, not cut off by parentheses (TS
/// wrappers without parentheses are looked through): `a?.b` yes, `(a?.b)` no.
pub(crate) fn has_unparenthesized_optional_chain(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let mut id = id;
    for _ in 0..100_000 {
        match *ast.node(id) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            _ => return false,
        }
    }
    false
}

/// unicorn's `getStaticStringValue`: a string literal or a template without expressions.
pub(crate) fn static_string(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value)),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [only] => match *ast.node(*only) {
                Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The name of member `id` (through parentheses): `a.b`, or `a["b"]` / ``a[`b`]`` with a
/// static string key.
pub(crate) fn static_member_name(ast: &ts_r_ast::Ast, id: NodeId) -> Option<&str> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { prop, .. } => ident_name(ast, prop),
        Node::Index { index, .. } => static_string(ast, index),
        _ => None,
    }
}

/// The object of member `id` (through parentheses).
pub(crate) fn member_object(ast: &ts_r_ast::Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(skip_parens(ast, id)) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => Some(obj),
        _ => None,
    }
}

/// `@eslint-community/eslint-utils`' `getFunctionHeadLocation` as a span: an arrow's `=>`;
/// otherwise from the start of the function (or of the property, method or field that
/// holds it, as its value or computed key) to the `(` of its parameters.
pub(crate) fn utils_function_head_span(cx: &Ctx<'_, '_>, f: NodeId) -> Span {
    let ast = cx.ast();
    let src = cx.src();
    let head = function_head_span(ast, src, cx.file.parents, f);
    if !matches!(cx.node(f), Node::Arrow { .. })
        && let Some(p) = cx.parent(f)
        && matches!(cx.node(p), Node::ComputedKey { .. })
        && let Some(holder) = cx.parent(p)
        && matches!(
            cx.node(holder),
            Node::Property { .. } | Node::MethodDef { .. } | Node::PropertyDef { .. }
        )
    {
        return Span::new(cx.span(holder).start, head.end);
    }
    head
}

/// `@eslint-community/eslint-utils`' `getFunctionNameWithKind` (which differs from
/// ESLint's: class fields holding functions are methods, `export default` functions are
/// named `'default'`, an object property's function is a method).
pub(crate) fn utils_function_name_with_kind(cx: &Ctx<'_, '_>, f: NodeId) -> String {
    let ast = cx.ast();
    let (is_async, is_gen, own_name, is_arrow) = match *cx.node(f) {
        Node::FunctionDecl { flags, name, .. } | Node::FunctionExpr { flags, name, .. } => (
            flags.contains(FnFlags::ASYNC),
            flags.contains(FnFlags::GENERATOR),
            name.and_then(|n| ident_name(ast, n)),
            false,
        ),
        Node::Arrow { flags, .. } => (flags.contains(FnFlags::ASYNC), false, None, true),
        _ => return String::new(),
    };
    let direct = parent_skip_parens(ast, cx.file.parents, f);
    // (holder, key, is class member, method kind)
    let holder = direct.and_then(|p| match *cx.node(p) {
        Node::Property { key, value, .. } if value == f => Some((p, key, false, None)),
        Node::MethodDef { key, kind, .. } => {
            let in_object = cx
                .parent(p)
                .is_some_and(|o| matches!(cx.node(o), Node::ObjectLit { .. }));
            Some((p, key, !in_object, Some(kind)))
        }
        Node::PropertyDef {
            key,
            value: Some(v),
            ..
        } if skip_parens(ast, v) == f => Some((p, key, true, None)),
        _ => None,
    });
    let mut tokens: Vec<String> = Vec::new();
    if let Some((p, key, true, _)) = holder {
        if cx.node(p).modifiers().contains(Modifiers::STATIC) {
            tokens.push("static".into());
        }
        if matches!(cx.node(key), Node::PrivateName { .. }) {
            tokens.push("private".into());
        }
    }
    if is_async {
        tokens.push("async".into());
    }
    if is_gen {
        tokens.push("generator".into());
    }
    match holder {
        Some((_, _, _, Some(MethodKind::Constructor))) => return "constructor".into(),
        Some((_, _, _, Some(MethodKind::Get))) => tokens.push("getter".into()),
        Some((_, _, _, Some(MethodKind::Set))) => tokens.push("setter".into()),
        Some(_) => tokens.push("method".into()),
        None => {
            if is_arrow {
                tokens.push("arrow".into());
            }
            tokens.push("function".into());
        }
    }
    if let Some((_, key, _, _)) = holder {
        if let Node::PrivateName { name } = *cx.node(key) {
            tokens.push(format!("#{}", ast.atom(name)));
        } else if let Some(name) = utils_property_name(cx, key) {
            tokens.push(format!("'{name}'"));
        } else {
            let text = match *cx.node(key) {
                Node::ComputedKey { expr } => cx.text(expr),
                _ => cx.text(key),
            };
            if !text.contains('\n') {
                tokens.push(format!("[{text}]"));
            }
        }
    } else if let Some(name) = own_name {
        tokens.push(format!("'{name}'"));
    } else if matches!(cx.node(f), Node::FunctionDecl { modifiers, .. } if modifiers.contains(Modifiers::DEFAULT))
    {
        tokens.push("'default'".into());
    } else if let Some(p) = direct {
        match *cx.node(p) {
            Node::VarDeclarator { name, .. } if ident_name(ast, name).is_some() => {
                tokens.push(format!("'{}'", ident_name(ast, name).unwrap_or_default()));
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                ..
            }
            | Node::AssignPattern { left, .. }
                if ident_name(ast, left).is_some() =>
            {
                tokens.push(format!("'{}'", ident_name(ast, left).unwrap_or_default()));
            }
            Node::Param {
                pattern,
                init: Some(_),
                ..
            } if ident_name(ast, pattern).is_some() => {
                tokens.push(format!(
                    "'{}'",
                    ident_name(ast, pattern).unwrap_or_default()
                ));
            }
            Node::BindingProperty {
                key,
                value: None,
                init: Some(_),
            } => {
                if let Some(n) = ident_name(ast, key) {
                    tokens.push(format!("'{n}'"));
                }
            }
            Node::ShorthandProp {
                name,
                init: Some(_),
                ..
            } => {
                if let Some(n) = ident_name(ast, name) {
                    tokens.push(format!("'{n}'"));
                }
            }
            Node::ExportDefaultExpr { .. } => tokens.push("'default'".into()),
            _ => {}
        }
    }
    tokens.join(" ")
}

/// eslint-utils' `getPropertyName` of a key without a scope: an identifier, a string or
/// number literal, or a computed key with a static value (literals, templates, `+`).
pub(crate) fn utils_property_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *cx.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::ComputedKey { expr } => static_value_string(ast, cx.src(), expr)
            .map(|c| c.into_owned())
            .or_else(|| literal_concat(cx, expr, 0)),
        _ => None,
    }
}

fn literal_concat(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let ast = cx.ast();
    match *ast.node(skip_parens(ast, id)) {
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } => {
            let l = static_value_string(ast, cx.src(), left)
                .map(|c| c.into_owned())
                .or_else(|| literal_concat(cx, left, depth + 1))?;
            let r = static_value_string(ast, cx.src(), right)
                .map(|c| c.into_owned())
                .or_else(|| literal_concat(cx, right, depth + 1))?;
            Some(l + &r)
        }
        _ => None,
    }
}

/// unicorn's `isNodeValueNotDomNode`: a literal, array, object, function, class, template
/// or `undefined` (through parentheses).
pub(crate) fn is_value_not_dom_node(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    let id = skip_parens(ast, id);
    matches!(
        ast.node(id),
        Node::ArrayLit { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionExpr { .. }
            | Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {}
            | Node::RegexLit { .. }
            | Node::ObjectLit { .. }
            | Node::TemplateLit { .. }
    ) || ident_name(ast, id) == Some("undefined")
}

/// unicorn's `isValueNotUsable`: the expression is a statement of its own, or a sequence
/// element whose value is dropped.
pub(crate) fn is_value_not_usable(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut id = id;
    for _ in 0..10_000 {
        let Some(p) = parent_skip_parens(ast, cx.file.parents, id) else {
            return false;
        };
        match *cx.node(p) {
            Node::ExprStmt { .. } => return true,
            Node::Seq { exprs } => {
                if ast.list(exprs).last().map(|&e| skip_parens(ast, e)) != Some(id) {
                    return true;
                }
                id = p;
            }
            _ => return false,
        }
    }
    false
}

/// The static name of a member access for reference comparison: `a.b` gives `b`, a
/// computed key gives the string form of its static value (`a[1]` and `a["1"]` agree).
fn reference_property_name(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => static_value_string(ast, cx.src(), index)
            .map(|c| c.into_owned())
            .or_else(|| literal_concat(cx, index, 0)),
        _ => None,
    }
}

/// unicorn's `isSameReference`: the same identifier, `this`, `super`, literal value, static
/// string, or member chain (static keys compared by value), through parentheses and TS
/// wrappers.
pub(crate) fn is_same_reference(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId) -> bool {
    fn same(cx: &Ctx<'_, '_>, a: NodeId, b: NodeId, depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        let ast = cx.ast();
        let a = skip_wrappers(ast, a);
        let b = skip_wrappers(ast, b);
        if let Some(sa) = static_string(ast, a) {
            return static_string(ast, b) == Some(sa);
        }
        match (*ast.node(a), *ast.node(b)) {
            (Node::This {}, Node::This {}) | (Node::Super {}, Node::Super {}) => true,
            (Node::Ident { name: x }, Node::Ident { name: y })
            | (Node::PrivateName { name: x }, Node::PrivateName { name: y }) => {
                ast.atom(x) == ast.atom(y)
            }
            (Node::NumberLit { value: x }, Node::NumberLit { value: y }) => x.get() == y.get(),
            (Node::BigIntLit { value: x }, Node::BigIntLit { value: y }) => {
                parse_bigint(ast.atom(x)) == parse_bigint(ast.atom(y))
            }
            (Node::BoolLit { value: x }, Node::BoolLit { value: y }) => x == y,
            (Node::NullLit {}, Node::NullLit {}) => true,
            (
                Node::RegexLit {
                    pattern: p1,
                    flags: f1,
                },
                Node::RegexLit {
                    pattern: p2,
                    flags: f2,
                },
            ) => ast.atom(p1) == ast.atom(p2) && ast.atom(f1) == ast.atom(f2),
            (
                Node::Member { obj: o1, .. } | Node::Index { obj: o1, .. },
                Node::Member { obj: o2, .. } | Node::Index { obj: o2, .. },
            ) => {
                if let Some(name) = reference_property_name(cx, a) {
                    return same(cx, o1, o2, depth + 1)
                        && reference_property_name(cx, b).as_deref() == Some(name.as_str());
                }
                match (*ast.node(a), *ast.node(b)) {
                    (Node::Index { index: i1, .. }, Node::Index { index: i2, .. }) => {
                        same(cx, o1, o2, depth + 1) && same(cx, i1, i2, depth + 1)
                    }
                    (Node::Member { prop: p1, .. }, Node::Member { prop: p2, .. }) => {
                        same(cx, o1, o2, depth + 1) && same(cx, p1, p2, depth + 1)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
    same(cx, a, b, 0)
}

/// The text of `id` with all its parentheses (unicorn's `getParenthesizedText`).
pub(crate) fn parenthesized_text<'a>(cx: &Ctx<'_, 'a>, id: NodeId) -> &'a str {
    let s = parenthesized_span(cx, id);
    &cx.src()[s.start as usize..s.end as usize]
}

/// True when the source at `start` begins with a token that would make a statement a block
/// or a declaration: `{`, `class`, `function`, `async function`, `let [`.
pub(crate) fn starts_like_declaration(src: &str, start: u32) -> bool {
    let rest = src.get(start as usize..).unwrap_or("");
    let word = |r: &'_ str, w: &str| -> Option<usize> {
        r.strip_prefix(w)
            .filter(|t| !t.starts_with(is_ident_char))
            .map(|_| w.len())
    };
    let after = |len: usize| {
        let p = skip_trivia_forward(rest, len);
        &rest[p..]
    };
    rest.starts_with('{')
        || word(rest, "class").is_some()
        || word(rest, "function").is_some()
        || word(rest, "async").is_some_and(|l| word(after(l), "function").is_some())
        || word(rest, "let").is_some_and(|l| after(l).starts_with('['))
}

/// The node whose text starts at `id`'s first token, through the left-hand sides.
fn first_token_node(ast: &ts_r_ast::Ast, id: NodeId) -> NodeId {
    let mut id = id;
    for _ in 0..10_000 {
        let next = match *ast.node(id) {
            Node::Paren { .. } => return id,
            _ => leading_child(ast, id),
        };
        match next {
            Some(n) if ast.span(n).start == ast.span(id).start => id = n,
            _ => return id,
        }
    }
    id
}

/// unicorn's `shouldAddParenthesesToExpressionStart`: `expr`, written where `replacement`
/// is, would begin a statement (or arrow body, `export default`, `for` initializer) with a
/// token that changes how it parses (a declaration-like start, or a regex starting a
/// statement is fine).
pub(crate) fn expression_start_needs_parens(
    cx: &Ctx<'_, '_>,
    expr: NodeId,
    replacement: NodeId,
) -> bool {
    let ast = cx.ast();
    let src = cx.src();
    let start = cx.span(expr).start;
    let regex = matches!(ast.node(first_token_node(ast, expr)), Node::RegexLit { .. });
    if !regex && !starts_like_declaration(src, start) {
        return false;
    }
    let first_is = |w: &str| {
        src[start as usize..].starts_with(w)
            && !src[start as usize + w.len()..].starts_with(is_ident_char)
    };
    let mut node = replacement;
    for _ in 0..10_000 {
        if is_statement(cx.node(node)) {
            return !regex;
        }
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Paren { .. } => return false,
            Node::Arrow { body, .. } if body == node => {
                return src[start as usize..].starts_with('{');
            }
            Node::ExportDefaultExpr { .. } => {
                return !regex && !src[start as usize..].starts_with('{') && !first_is("let");
            }
            Node::For {
                init: Some(init), ..
            } if init == node => return first_is("let"),
            Node::ExprStmt { .. } => return !regex,
            _ => {}
        }
        if cx.span(parent).start != cx.span(node).start {
            return false;
        }
        node = parent;
    }
    false
}

/// Statement nodes (ESTree's `*Statement` types).
fn is_statement(node: &Node) -> bool {
    matches!(
        node,
        Node::ExprStmt { .. }
            | Node::Block { .. }
            | Node::Empty {}
            | Node::If { .. }
            | Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
            | Node::Return { .. }
            | Node::Break { .. }
            | Node::Continue { .. }
            | Node::Throw { .. }
            | Node::Labeled { .. }
            | Node::Switch { .. }
            | Node::Try { .. }
            | Node::Debugger {}
            | Node::With { .. }
    )
}

/// `new Foo` without its own argument parentheses.
fn is_new_without_parens(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let Node::New {
        callee,
        has_args,
        args,
        ..
    } = *cx.node(id)
    else {
        return false;
    };
    if has_args && !cx.ast().list(args).is_empty() {
        return false;
    }
    !(has_args && cx.text(id).trim_end().ends_with(')') && cx.span(callee).end < cx.span(id).end)
}

/// unicorn's `shouldAddParenthesesToMemberExpressionObject`: `id` (not parenthesized) needs
/// parentheses as the object of a member access written where `replacement` is.
pub(crate) fn member_object_needs_parens(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    replacement: NodeId,
) -> bool {
    if expression_start_needs_parens(cx, id, replacement) {
        return true;
    }
    match *cx.node(id) {
        Node::Ident { .. }
        | Node::Member { .. }
        | Node::Index { .. }
        | Node::Call { .. }
        | Node::TemplateLit { .. }
        | Node::This {}
        | Node::Super {}
        | Node::ArrayLit { .. }
        | Node::MetaProperty { .. }
        | Node::ImportCall { .. }
        | Node::TaggedTemplate { .. }
        | Node::NonNull { .. } => false,
        Node::New { .. } => is_new_without_parens(cx, id),
        Node::NumberLit { .. } => cx.text(id).bytes().all(|b| b.is_ascii_digit()),
        Node::StringLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::BigIntLit { .. } => false,
        _ => true,
    }
}

/// unicorn's `getMemberExpressionObjectText`: `id` with its parentheses, wrapped in new ones
/// when it needs them as a member object written where `replacement` is.
pub(crate) fn member_object_text(cx: &Ctx<'_, '_>, id: NodeId, replacement: NodeId) -> String {
    let text = parenthesized_text(cx, id);
    if !is_parenthesized(cx, id) && member_object_needs_parens(cx, id, replacement) {
        format!("({text})")
    } else {
        text.to_owned()
    }
}

/// unicorn's `shouldAddParenthesesToNewExpressionCallee` for a member access on `object`:
/// the object chain (member objects, tags, TS wrappers; stopping at parentheses) ends in a
/// call, which `new` would take as its arguments.
pub(crate) fn member_on_needs_new_parens(cx: &Ctx<'_, '_>, object: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = object;
    if is_parenthesized(cx, node) {
        return false;
    }
    for _ in 0..10_000 {
        node = match *ast.node(node) {
            Node::Paren { .. } => return false,
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => expr,
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            Node::TaggedTemplate { tag, .. } => tag,
            Node::Call { .. } => return true,
            _ => return false,
        };
    }
    false
}

/// The deepest node whose span contains byte `pos`.
fn deepest_node_at(ast: &ts_r_ast::Ast, pos: u32) -> Option<NodeId> {
    let mut node = ast.root;
    let s = ast.span(node);
    if !(s.start <= pos && pos < s.end) {
        return None;
    }
    for _ in 0..100_000 {
        let mut next = None;
        ast.for_each_child(node, |c| {
            let s = ast.span(c);
            if next.is_none() && s.start <= pos && pos < s.end {
                next = Some(c);
            }
        });
        match next {
            Some(n) => node = n,
            None => return Some(node),
        }
    }
    Some(node)
}

/// unicorn's `needsSemicolon`: writing `code` at `pos` needs a `;` before it so it can't
/// continue the token before it (`foo\n[a]`), judged from that token and the node it ends.
pub(crate) fn needs_semicolon(cx: &Ctx<'_, '_>, pos: u32, code: &str) -> bool {
    if !code.starts_with(['[', '(', '/', '`', '+', '-', '*', ',', '.', '<']) {
        return false;
    }
    let ast = cx.ast();
    let src = cx.src();
    let end = skip_trivia_backward(ast, src, pos as usize) as u32;
    if end == 0 {
        return false;
    }
    let last = src[..end as usize].chars().next_back().unwrap_or(' ');
    if last == ';' {
        return false;
    }
    // The token's first byte: identifiers and numbers run back over word characters.
    let token_start = if is_ident_char(last) {
        let word = src[..end as usize]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_ident_char(c) || c == '.')
            .last()
            .map_or(0, |(i, _)| i);
        word as u32
    } else {
        end - last.len_utf8() as u32
    };
    let Some(deepest) = deepest_node_at(ast, token_start) else {
        return false;
    };
    let mut node = Some(deepest);
    while let Some(n) = node {
        if cx.span(n).end != end {
            break;
        }
        if matches!(
            cx.node(n),
            Node::ExprStmt { .. }
                | Node::VarDecl { .. }
                | Node::Return { .. }
                | Node::Throw { .. }
                | Node::ExportDefaultExpr { .. }
        ) {
            return true;
        }
        node = cx.parent(n);
    }
    let parent_kind = cx.parent(deepest).map(|p| cx.node(p));
    match last {
        ']' => return true,
        '}' => {
            if matches!(cx.node(deepest), Node::Block { .. })
                && matches!(parent_kind, Some(Node::FunctionExpr { .. }))
                || matches!(cx.node(deepest), Node::ClassExpr { .. })
            {
                return true;
            }
        }
        ')' => {
            let body = match *cx.node(deepest) {
                Node::If { cons, .. } => Some(cons),
                Node::For { body, .. }
                | Node::ForIn { body, .. }
                | Node::ForOf { body, .. }
                | Node::While { body, .. }
                | Node::With { body, .. } => Some(body),
                Node::DoWhile { .. } => None,
                _ => None,
            };
            if let Some(body) = body
                && skip_trivia_backward(ast, src, cx.span(body).start as usize) as u32 == end
            {
                return false;
            }
            return true;
        }
        _ => {}
    }
    match cx.node(deepest) {
        Node::StringLit { .. }
        | Node::NullLit {}
        | Node::BoolLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. } => return true,
        Node::TemplateLit { .. } | Node::TaggedTemplate { .. } if last == '`' => return true,
        Node::NonNull { .. } if last == '!' => return true,
        Node::TypeArgs { .. } if matches!(parent_kind, Some(Node::Instantiation { .. })) => {
            return true;
        }
        _ => {}
    }
    if is_ident_char(last) {
        let word = &src[token_start as usize..end as usize];
        if is_reserved_word(word) {
            return false;
        }
        if word == "of" && matches!(cx.node(deepest), Node::ForOf { .. }) {
            return false;
        }
        return !(word == "await" && matches!(cx.node(deepest), Node::Await { .. }));
    }
    false
}

/// Words espree tokenizes as keywords rather than identifiers.
fn is_reserved_word(w: &str) -> bool {
    matches!(
        w,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "true"
            | "false"
            | "null"
    )
}

/// unicorn's `isLeftHandSide`: `id` (with its parentheses and TS wrappers) is written to:
/// an assignment or update target, a destructuring element, a `for-in/of` head, or a
/// `delete` operand.
pub(crate) fn is_left_hand_side(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = id;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => node = parent,
            Node::Update { .. } => return true,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            Node::AssignPattern { left, .. } => return left == node,
            Node::Rest { .. } | Node::ArrayPattern { .. } => return true,
            _ => return is_assignment_target(ast, cx.file.parents, node),
        }
    }
    false
}

/// unicorn's `isNodeValueNotFunction`: literals, arrays, objects, classes, templates,
/// unary/update/arithmetic expressions, assignments, `await`, `new`, tagged templates,
/// `this`, `undefined` and calls other than `.bind(...)` can't (or most likely don't)
/// produce a function.
pub(crate) fn is_value_not_function(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::ArrayLit { .. }
        | Node::ClassExpr { .. }
        | Node::StringLit { .. }
        | Node::NumberLit { .. }
        | Node::BigIntLit { .. }
        | Node::BoolLit { .. }
        | Node::NullLit {}
        | Node::RegexLit { .. }
        | Node::ObjectLit { .. }
        | Node::TemplateLit { .. }
        | Node::Unary { .. }
        | Node::Update { .. }
        | Node::Assign { .. }
        | Node::Await { .. }
        | Node::New { .. }
        | Node::TaggedTemplate { .. }
        | Node::This {} => true,
        Node::Binary { op, .. } => !matches!(
            op,
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce
        ),
        Node::Call {
            callee, optional, ..
        } => {
            let bind = matches!(*ast.node(skip_parens(ast, callee)),
                Node::Member { prop, optional: false, .. } if ident_name(ast, prop) == Some("bind"));
            optional || !bind
        }
        _ => ident_name(ast, id) == Some("undefined"),
    }
}

/// unicorn's `isNodeMatches` for `Name` / `a.b.c` paths: identifiers (and `this`/`super`)
/// joined by non-optional, non-computed members, through parentheses.
pub(crate) fn node_matches_path(ast: &ts_r_ast::Ast, id: NodeId, path: &str) -> bool {
    let names: Vec<&str> = path.split('.').collect();
    let mut node = skip_parens(ast, id);
    for &name in names[1..].iter().rev() {
        match *ast.node(node) {
            Node::Member {
                obj,
                prop,
                optional: false,
            } if ident_name(ast, prop) == Some(name) => node = skip_parens(ast, obj),
            _ => return false,
        }
    }
    match (names[0], ast.node(node)) {
        ("this", Node::This {}) | ("super", Node::Super {}) => true,
        (n, _) => ident_name(ast, node) == Some(n),
    }
}

/// unicorn's `removeMethodCall`: deletes `.name(...)` from `obj.name(...)`, keeping the
/// object's parentheses.
pub(crate) fn remove_method_call(cx: &Ctx<'_, '_>, call: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *cx.node(call) else {
        return None;
    };
    let member = skip_parens(ast, callee);
    let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(member) else {
        return None;
    };
    let obj_end = parenthesized_span(cx, skip_parens(ast, obj)).end;
    let member_end = parenthesized_span(cx, member).end;
    Some(
        Fix::delete(Span::new(obj_end, cx.span(member).end))
            .and(Span::new(member_end, cx.span(call).end), ""),
    )
}

/// The call's explicit type arguments (`foo<T>()`).
pub(crate) fn has_type_arguments(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    matches!(
        *cx.node(id),
        Node::Call {
            type_args: Some(_),
            ..
        } | Node::New {
            type_args: Some(_),
            ..
        }
    )
}

/// unicorn's `escapeString`: `s` quoted with `quote` (`'` unless `"`), escaping the quote,
/// backslashes, control characters, DEL and the line/paragraph separators.
pub(crate) fn escape_string(s: &str, quote: Option<char>) -> String {
    let q = if quote == Some('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == q => {
                out.push('\\');
                out.push(q);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            '\0' => out.push_str("\\u{0}"),
            c if (c as u32) <= 0x1f || matches!(c as u32, 0x7f | 0x2028 | 0x2029) => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// An ECMAScript IdentifierName (reserved words included).
pub(crate) fn is_identifier_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c == '$' || c == '_' || c.is_alphabetic())
        && chars.all(|c| {
            c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}' || c.is_alphanumeric()
        })
}

#[cfg(test)]
mod tests {
    use super::{HTML_ENTITIES, decode_jsx_entities, singular};

    #[test]
    fn entities() {
        assert!(HTML_ENTITIES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(HTML_ENTITIES.len(), 253);
        assert_eq!(
            decode_jsx_entities("a&#45;b&#x2D;&amp;&nope;&"),
            "a-b-&&nope;&"
        );
        assert_eq!(decode_jsx_entities("&#39;x&#39;&#59;"), "'x';");
    }

    #[test]
    fn singular_names() {
        let s = |w: &str| singular(w);
        assert_eq!(s("items").as_deref(), Some("item"));
        assert_eq!(s("cities").as_deref(), Some("city"));
        assert_eq!(s("cases").as_deref(), Some("case"));
        assert_eq!(s("boxes").as_deref(), Some("box"));
        assert_eq!(s("xs").as_deref(), Some("x"));
        assert_eq!(s("visibleItems").as_deref(), Some("visibleItem"));
        assert_eq!(s("childNodes").as_deref(), Some("childNode"));
        assert_eq!(s("children").as_deref(), Some("child"));
        assert_eq!(s("allChildren").as_deref(), Some("allChild"));
        assert_eq!(s("arr"), None);
        assert_eq!(s("class"), None);
        assert_eq!(s("status"), None);
        assert_eq!(s("s"), None);
        assert_eq!(s("é"), None);
    }
}
