//! `@typescript-eslint/no-deprecated`: no references to declarations marked `@deprecated`
//! in their JSDoc.

use super::prelude::*;
use super::ts_no_useless_default_assignment::source_type;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-deprecated",
        "Disallow using code marked as `@deprecated`",
        TS_STRICT_TC,
        Category::Suspicious,
    )
    .typed(),
    create: create::<NoDeprecated>,
};

#[derive(Debug)]
struct NoDeprecated {
    allow: Vec<Specifier>,
}

impl FromOptions for NoDeprecated {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoDeprecated {
            allow: parse_specifiers(obj_opt(opts, 0).get("allow")),
        })
    }
}

impl Rule for NoDeprecated {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::Ident,
            NodeKind::PrivateName,
            NodeKind::Index,
            NodeKind::Super,
        ]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let Some(found) = (match *cx.node(node) {
            Node::Ident { .. } | Node::PrivateName { .. } => ident_use(cx, node),
            Node::Index { index, .. } => index_use(cx, node, index),
            Node::Super {} => super_use(cx, node),
            _ => None,
        }) else {
            return;
        };
        let ti = cx.types();
        let bare = found.name.strip_prefix('#').unwrap_or(&found.name);
        if !self.allow.is_empty() && symbol_matches(ti, found.target, bare, &self.allow) {
            return;
        }
        let Some(reason) = found.reason else { return };
        let name = &found.name;
        if reason.is_empty() {
            cx.report(found.span, format!("`{name}` is deprecated."))
                .id("deprecated");
        } else {
            cx.report(found.span, format!("`{name}` is deprecated. {reason}"))
                .id("deprecatedWithReason");
        }
    }
}

/// A use of a deprecated (or allowed) declaration.
struct Found {
    span: Span,
    name: String,
    /// The symbol after aliases are followed (checked against `allow`).
    target: Sym,
    /// The `@deprecated` text (empty without a reason); `None` when not deprecated.
    reason: Option<String>,
}

fn ident_use(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Found> {
    let ast = cx.ast();
    let ti = cx.types();
    let name = ast.name_of(id)?.to_owned();
    let name = if matches!(cx.node(id), Node::PrivateName { .. }) {
        format!("#{name}")
    } else {
        name
    };
    let span = cx.span(id);
    let parent = cx.parent(id)?;
    match *cx.node(parent) {
        Node::Member { obj, prop, .. } if prop == id => {
            let s = ti.symbol_at(parent)?;
            let via = namespace_object(cx, obj);
            let call = call_of(cx, parent);
            return check(cx, s, &name, span, call, via);
        }
        Node::QualifiedName { left, right } if right == id => {
            let left = match *cx.node(left) {
                Node::QualifiedName { right, .. } => right,
                _ => left,
            };
            let ns = ti.symbol_at(left)?;
            let ns = ti.resolve_alias(ns).unwrap_or(ns);
            let s = ti.export_of(ns, &name)?;
            return check(cx, s, &name, span, None, ti.symbol_facts(ns).module);
        }
        Node::BindingProperty { key, .. } if key == id => {
            let op = cx.parent(parent)?;
            let st = source_type(cx, op, 0)?;
            let via = ti
                .type_symbol(st)
                .is_some_and(|s| ti.symbol_facts(s).module);
            let s = property_of(ti, st, &name)?;
            return check(cx, s, &name, span, None, via);
        }
        Node::BindingProperty { value, .. } if value == Some(id) => return None,
        Node::ShorthandProp { name: n, .. } if n == id => {
            let sem = cx.semantic();
            let r = sem.reference_at(id)?;
            let local = sem.reference(r).symbol()?;
            let decl = *sem.symbol(local).decls.first()?;
            let s = ti.symbol_at(decl.name)?;
            return check(cx, s, &name, span, None, false);
        }
        Node::ExportSpecifier {
            local, exported, ..
        } => {
            if exported.is_some_and(|e| e != id) {
                return None;
            }
            if !matches!(cx.node(local), Node::Ident { .. }) {
                return None;
            }
            let from = cx.parent(parent).is_some_and(|d| {
                matches!(
                    cx.node(d),
                    Node::ExportNamed {
                        source: Some(_),
                        ..
                    }
                )
            });
            let s = if from {
                ti.file_export(&name)?
            } else {
                ti.symbol_at(local)?
            };
            return check(cx, s, &name, span, None, false);
        }
        Node::JsxAttr { name: n, .. } if n == id => {
            let s = jsx_attr_symbol(cx, parent, &name)?;
            return check(cx, s, &name, span, None, false);
        }
        Node::JsxOpening { name: n, .. } if n == id => {
            if name.starts_with(|c: char| c.is_ascii_lowercase()) {
                return None;
            }
            let s = ti.symbol_at(id)?;
            return check(cx, s, &name, span, None, false);
        }
        Node::JsxClosing { .. }
        | Node::JsxNamespacedName { .. }
        | Node::JsxMemberExpr { .. }
        | Node::ImportSpecifier { .. }
        | Node::ImportDefaultSpecifier { .. }
        | Node::ImportNamespaceSpecifier { .. }
        | Node::ImportEquals { .. }
        | Node::Labeled { .. }
        | Node::Break { .. }
        | Node::Continue { .. }
        | Node::MetaProperty { .. }
        | Node::NamedTupleMember { .. } => return None,
        Node::Property { key, .. }
        | Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. }
            if key == id =>
        {
            return None;
        }
        Node::EnumMember { name: n, .. } if n == id => return None,
        _ => {}
    }
    if cx.semantic().symbol_of_decl(id).is_some() {
        return None;
    }
    let s = ti.symbol_at(id)?;
    if is_declaration_name(cx, s, id) {
        return None;
    }
    let call = call_of(cx, id);
    check(cx, s, &name, span, call, false)
}

/// `obj[key]` with a key of a known literal type.
fn index_use(cx: &Ctx<'_, '_>, node: NodeId, index: NodeId) -> Option<Found> {
    let ti = cx.types();
    let Node::Index { obj, .. } = *cx.node(node) else {
        return None;
    };
    let kt = ti.type_at(index)?;
    if kt == Ty::UNRESOLVED {
        return None;
    }
    let name = ti
        .literal_text(kt)
        .or_else(|| template_key(cx, skip_parens(cx.ast(), index)))?;
    let ot = ti.type_at(obj)?;
    let s = ti
        .symbol_at(node)
        .filter(|&s| ti.symbol_name(s) == name)
        .or_else(|| property_of(ti, ot, &name))?;
    let via = namespace_object(cx, obj);
    let call = call_of(cx, node);
    check(cx, s, &name, cx.span(index), call, via)
}

/// A template literal key whose interpolations all have literal types.
fn template_key(cx: &Ctx<'_, '_>, e: NodeId) -> Option<String> {
    let ast = cx.ast();
    let ti = cx.types();
    let Node::TemplateLit { parts } = *cx.node(e) else {
        return static_value_string(ast, cx.src(), e).map(|s| s.into_owned());
    };
    let mut out = String::new();
    for (i, &p) in ast.list(parts).iter().enumerate() {
        if i % 2 == 0 {
            let Node::TemplateElement { cooked, .. } = *cx.node(p) else {
                return None;
            };
            out.push_str(ast.atom(cooked));
        } else {
            let t = ti.type_at(p)?;
            if t == Ty::UNRESOLVED {
                return None;
            }
            out.push_str(&ti.literal_text(t)?);
        }
    }
    Some(out)
}

fn super_use(cx: &Ctx<'_, '_>, node: NodeId) -> Option<Found> {
    let call = cx.parent(node)?;
    let Node::Call { callee, .. } = *cx.node(call) else {
        return None;
    };
    if callee != node {
        return None;
    }
    let ti = cx.types();
    let sig = ti.resolved_signature(call)?;
    let d = ti.signature_decl(sig)?;
    let reason = decl_deprecation(ti, &d);
    let s = ti.symbol_at(node).or_else(|| {
        let t = ti.type_at(node)?;
        ti.type_symbol(t)
    })?;
    Some(Found {
        span: cx.span(node),
        name: "super".to_owned(),
        target: s,
        reason,
    })
}

fn property_of(ti: &dyn TypeInfo, t: Ty, name: &str) -> Option<Sym> {
    if t == Ty::UNRESOLVED || is_any(ti, t) {
        return None;
    }
    let t = ti.apparent_type(constrained(ti, t));
    ti.property_of_type(t, name)
}

/// The object is a module namespace (`import * as ns`), whose exports may be re-exports
/// marked deprecated at the export specifier.
fn namespace_object(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
    let ti = cx.types();
    let Some(s) = ti.symbol_at(skip_parens(cx.ast(), obj)) else {
        return false;
    };
    let s = ti.resolve_alias(s).unwrap_or(s);
    ti.symbol_facts(s).module
}

/// The call, `new` or tagged template whose callee is `node`.
fn call_of(cx: &Ctx<'_, '_>, node: NodeId) -> Option<NodeId> {
    let mut cur = node;
    for _ in 0..32 {
        let p = cx.parent(cur)?;
        match *cx.node(p) {
            Node::Paren { .. } => cur = p,
            Node::Call { callee, .. } | Node::New { callee, .. } if callee == cur => {
                return Some(p);
            }
            Node::TaggedTemplate { tag, .. } if tag == cur => return Some(p),
            _ => return None,
        }
    }
    None
}

/// `id` names one of the symbol's declarations in this file.
fn is_declaration_name(cx: &Ctx<'_, '_>, s: Sym, id: NodeId) -> bool {
    let ti = cx.types();
    let ast = cx.ast();
    ti.symbol_decls(s)
        .iter()
        .filter(|d| d.in_this_file)
        .any(|d| d.node == id || declared_name(ast, d.node) == Some(id))
}

fn declared_name(ast: &Ast, d: NodeId) -> Option<NodeId> {
    match *ast.node(d) {
        Node::VarDeclarator { name, .. } => Some(name),
        Node::FunctionDecl { name, .. }
        | Node::FunctionExpr { name, .. }
        | Node::ClassDecl { name, .. }
        | Node::ClassExpr { name, .. } => name,
        Node::Param { pattern, .. } => Some(pattern),
        Node::Interface { name, .. }
        | Node::TypeAlias { name, .. }
        | Node::Enum { name, .. }
        | Node::Module { name, .. }
        | Node::EnumMember { name, .. }
        | Node::TypeParam { name, .. } => Some(name),
        Node::PropertyDef { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodSignature { key, .. }
        | Node::Property { key, .. } => Some(key),
        Node::ShorthandProp { name, .. } => Some(name),
        Node::BindingProperty { key, value, .. } => Some(value.unwrap_or(key)),
        Node::ImportSpecifier { local, .. }
        | Node::ImportDefaultSpecifier { local }
        | Node::ImportNamespaceSpecifier { local } => Some(local),
        Node::ImportEquals { name, .. } => Some(name),
        Node::ExportSpecifier {
            local, exported, ..
        } => Some(exported.unwrap_or(local)),
        _ => None,
    }
}

/// Decides whether the use of `s` (through `name`, called by `call`) is deprecated.
fn check(
    cx: &Ctx<'_, '_>,
    s: Sym,
    name: &str,
    span: Span,
    call: Option<NodeId>,
    via_namespace: bool,
) -> Option<Found> {
    let ti = cx.types();
    let alias = ti.symbol_facts(s).alias;
    let target = if alias { ti.resolve_alias(s)? } else { s };
    let mut reason = None;
    if alias {
        reason = ti
            .symbol_decls(s)
            .iter()
            .filter(|d| !d.in_this_file)
            .find_map(|d| decl_deprecation(ti, d));
    }
    if reason.is_none() && (alias || via_namespace) {
        let export_name = if alias {
            imported_name(cx, s).unwrap_or_else(|| name.to_owned())
        } else {
            name.to_owned()
        };
        reason = reexport_deprecation(ti, target, &export_name);
    }
    if reason.is_none() {
        let facts = ti.symbol_facts(target);
        let callable = facts.function || facts.method;
        let sig_decl = call
            .and_then(|c| ti.resolved_signature(c))
            .and_then(|sig| ti.signature_decl(sig));
        if let Some(d) = &sig_decl {
            reason = decl_deprecation(ti, d);
        }
        if reason.is_none() && !(callable && sig_decl.is_some()) {
            reason = symbol_deprecation(ti, target);
        }
    }
    Some(Found {
        span,
        name: name.to_owned(),
        target,
        reason,
    })
}

/// The name an import alias imports (`default` for default imports).
fn imported_name(cx: &Ctx<'_, '_>, alias: Sym) -> Option<String> {
    let ti = cx.types();
    let d = ti.symbol_decls(alias).into_iter().next()?;
    let (ast, _) = ti.decl_file(&d)?;
    match *ast.node(d.node) {
        Node::ImportSpecifier {
            imported, local, ..
        } => ast.name_of(imported.unwrap_or(local)).map(str::to_owned),
        Node::ImportDefaultSpecifier { .. } => Some("default".to_owned()),
        Node::ExportSpecifier {
            local, exported, ..
        } => ast.name_of(exported.unwrap_or(local)).map(str::to_owned),
        _ => None,
    }
}

/// A deprecated `export { x as name }` in the file declaring `target`.
fn reexport_deprecation(ti: &dyn TypeInfo, target: Sym, name: &str) -> Option<String> {
    let d = ti
        .symbol_decls(target)
        .into_iter()
        .find(|d| !d.in_this_file)?;
    let (ast, src) = ti.decl_file(&d)?;
    let Node::Program { stmts } = *ast.node(ast.root) else {
        return None;
    };
    for &st in ast.list(stmts) {
        let Node::ExportNamed {
            specifiers,
            source: None,
            ..
        } = *ast.node(st)
        else {
            continue;
        };
        for &sp in ast.list(specifiers) {
            let Node::ExportSpecifier {
                local, exported, ..
            } = *ast.node(sp)
            else {
                continue;
            };
            if ast.name_of(exported.unwrap_or(local)) == Some(name) {
                return jsdoc_deprecation(ast, src, ast.span(sp).start);
            }
        }
    }
    None
}

/// The symbol is deprecated: its only declaration, or every one of them, has the tag.
fn symbol_deprecation(ti: &dyn TypeInfo, s: Sym) -> Option<String> {
    let decls = ti.symbol_decls(s);
    let facts = ti.symbol_facts(s);
    let mut found = None;
    for d in &decls {
        match decl_deprecation(ti, d) {
            Some(r) => {
                if found.is_none() {
                    found = Some(r);
                }
                if !(facts.function || facts.method) {
                    return found;
                }
            }
            None if facts.function || facts.method => return None,
            None => {}
        }
    }
    found
}

/// The `@deprecated` text of the JSDoc comment before a declaration.
fn decl_deprecation(ti: &dyn TypeInfo, d: &crate::types::Decl) -> Option<String> {
    let (ast, src) = ti.decl_file(d)?;
    let start = ast.span(d.node).start;
    jsdoc_deprecation(ast, src, start)
}

const SKIPPED_WORDS: &[&str] = &[
    "export",
    "declare",
    "default",
    "const",
    "let",
    "var",
    "using",
    "function",
    "class",
    "async",
    "abstract",
    "static",
    "public",
    "private",
    "protected",
    "readonly",
    "accessor",
    "override",
    "enum",
    "interface",
    "type",
    "namespace",
    "module",
    "get",
    "set",
];

/// Walks back from `start` over modifiers and keywords to the JSDoc comments before them.
fn jsdoc_deprecation(ast: &Ast, src: &str, start: u32) -> Option<String> {
    let b = src.as_bytes();
    let mut pos = (start as usize).min(b.len());
    if !src.is_char_boundary(pos) {
        return None;
    }
    for _ in 0..32 {
        // Any whitespace, including Unicode (U+00A0 between `const` and a name).
        while let Some(c) = src[..pos].chars().next_back()
            && c.is_whitespace()
        {
            pos -= c.len_utf8();
        }
        if pos >= 2 && &b[pos - 2..pos] == b"*/" {
            let c = ast
                .comments_between(0, pos as u32)
                .iter()
                .rev()
                .find(|c| c.span.end as usize == pos)?;
            let text = c.span.text(src);
            if text.starts_with("/**")
                && text.len() > 4
                && let Some(r) = deprecated_tag(text)
            {
                return Some(r);
            }
            pos = c.span.start as usize;
            continue;
        }
        if pos > 0 && b[pos - 1] == b'*' {
            pos -= 1;
            continue;
        }
        let word_start = src[..pos]
            .char_indices()
            .rev()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .map_or(0, |(i, c)| i + c.len_utf8());
        if word_start < pos && SKIPPED_WORDS.contains(&&src[word_start..pos]) {
            pos = word_start;
            continue;
        }
        return None;
    }
    None
}

/// The text of a `@deprecated` tag in a JSDoc comment (empty without a reason).
fn deprecated_tag(comment: &str) -> Option<String> {
    let body = comment.strip_prefix("/**")?.strip_suffix("*/")?;
    let mut lines = Vec::new();
    let mut in_tag = false;
    for line in body.lines() {
        let l = line.trim_start();
        let l = l.strip_prefix('*').unwrap_or(l).trim();
        if in_tag {
            if l.starts_with('@') {
                break;
            }
            lines.push(l.to_owned());
            continue;
        }
        let mut rest = l;
        while let Some(i) = rest.find("@deprecated") {
            let before_ok = i == 0 || rest[..i].ends_with(char::is_whitespace);
            let after = &rest[i + "@deprecated".len()..];
            let after_ok = after
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
            if before_ok && after_ok {
                in_tag = true;
                lines.push(after.trim().to_owned());
                break;
            }
            rest = after;
        }
    }
    if !in_tag {
        return None;
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    Some(lines.join("\n").trim().to_owned())
}

/// The property a JSX attribute sets, from the element's props type.
fn jsx_attr_symbol(cx: &Ctx<'_, '_>, attr: NodeId, name: &str) -> Option<Sym> {
    let ti = cx.types();
    if let Some(s) = ti.symbol_at(attr)
        && ti.symbol_name(s) == name
    {
        return Some(s);
    }
    let opening = cx.parent(attr)?;
    let Node::JsxOpening { name: tag, .. } = *cx.node(opening) else {
        return None;
    };
    let props = match *cx.node(tag) {
        Node::Ident { .. } if !cx.ast().name_of(tag)?.starts_with(char::is_lowercase) => {
            let t = ti.type_at(tag)?;
            if t == Ty::UNRESOLVED || is_any(ti, t) {
                return None;
            }
            let sig = ti.signatures(t, false).into_iter().next()?;
            ti.param_type(sig, 0)?
        }
        Node::Ident { .. } | Node::JsxNamespacedName { .. } => {
            let tag_name = cx.span(tag).text(cx.src());
            let jsx = ti.global_symbol("JSX")?;
            let elements = ti.export_of(jsx, "IntrinsicElements")?;
            let et = ti.declared_type_of_symbol(elements)?;
            let el = ti.property_of_type(et, tag_name)?;
            ti.type_of_symbol(el)
        }
        _ => return None,
    };
    property_of(ti, props, name)
}

#[cfg(test)]
mod tests {
    use super::jsdoc_deprecation;

    #[test]
    fn declaration_prefixes_with_unicode_whitespace() {
        for src in [
            "/** @deprecated */ const\u{a0}foo = 1; foo;\n",
            "/** @deprecated */\u{3000}const foo = 1; foo;\n",
            "/** @deprecated */ const é\u{a0}foo = 1;\n",
        ] {
            let env = crate::env::LintEnv::default();
            let parsed = crate::engine::parse_for_lint("a.ts", src, &env);
            let at = src.find("foo").unwrap() as u32;
            let r = jsdoc_deprecation(&parsed.ast, src, at);
            if src.contains('é') {
                // `é` is not a modifier: the walk stops there.
                assert_eq!(r, None, "{src:?}");
            } else {
                assert_eq!(r.as_deref(), Some(""), "{src:?}");
            }
        }
    }
}
