//! Hover: a one-line description of the symbol under the cursor (in tsserver's display
//! style) plus its JSDoc.

use super::locate::{ident_at, is_expression_name, meaning_at, name_text, node_at};
use super::{Ctx, LspLines};
use serde_json::{Value, json};
use ts_r_ast::{Node, VarKind};
use ts_r_binder::{SymbolFacts, SymbolFlags};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_types::{SymId, TypeId, TypeKind};

pub fn hover(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, lines: &LspLines, offset: u32) -> Value {
    let c = cx.checker;
    let ast = c.ast(cx.file);
    let text = c.file(cx.file).text.as_str();
    let Some(n) = ident_at(ast, offset).or_else(|| node_at(ast, offset)) else {
        return Value::Null;
    };
    let l = Loc::new(cx.file, n);
    let span = ast.span(n);
    let (display, doc) = match ast.node(n) {
        Node::This {} => {
            let t = ck.check_expr(l);
            (format!("this: {}", ck.type_to_string(t)), String::new())
        }
        Node::Ident { .. } | Node::PrivateName { .. } => {
            let m = meaning_at(ck, l);
            let Some(s) = m.primary else {
                // A property of `any` (or one the checker cannot resolve): its type alone.
                if let Some(p) = c.parent(l)
                    && let Node::Member { prop, .. } = *c.node(p)
                    && prop == n
                {
                    let t = ck.check_expr(p);
                    let ts = ck.type_to_string(t);
                    let value = format!("```typescript\n{ts}\n```");
                    return json!({
                        "contents": {"kind": "markdown", "value": value},
                        "range": lines.range(text, span.start, span.end),
                    });
                }
                return Value::Null;
            };
            let display = describe(ck, s, Some(l));
            let target = c.resolve_alias(s).unwrap_or(s);
            (display, docs_of(ck, target))
        }
        _ => return Value::Null,
    };
    let mut value = format!("```typescript\n{display}\n```");
    if !doc.is_empty() {
        value.push('\n');
        value.push_str(&doc);
    }
    json!({
        "contents": {"kind": "markdown", "value": value},
        "range": lines.range(text, span.start, span.end),
    })
}

/// tsserver-style display string of a symbol (`const x: number`, `(property) A.b: string`,
/// `function f(a: string): void`). `at` is the name being described, for narrowed types.
pub fn describe(ck: &mut Ck<'_, '_>, s: SymId, at: Option<Loc>) -> String {
    describe_depth(ck, s, at, 0)
}

fn describe_depth(ck: &mut Ck<'_, '_>, s: SymId, at: Option<Loc>, depth: u32) -> String {
    let c = ck.c;
    let flags = c.sym_flags(s);
    let name = c.sym_name(s).to_string();
    if flags.intersects(SymbolFlags::ALIAS) {
        return match c.resolve_alias(s) {
            Some(t) if t != s && depth < 4 => {
                let inner = describe_depth(ck, t, None, depth + 1);
                // tsserver drops the `function` keyword of aliased functions.
                let inner = inner.strip_prefix("function ").unwrap_or(&inner);
                format!("(alias) {inner}\nimport {name}")
            }
            _ => format!("import {name}"),
        };
    }
    if flags.intersects(SymbolFlags::MODULE_FILE) {
        let path = c
            .first_decl(s)
            .map(|d| c.file(d.file).path.clone())
            .unwrap_or_default();
        let stem = strip_ts_ext(&path);
        return format!("module \"{stem}\"");
    }
    if flags.intersects(SymbolFlags::CLASS) {
        let t = ck.declared_type_of_symbol(s);
        let abs = if c.sym_facts(s).intersects(SymbolFacts::ABSTRACT) {
            "abstract "
        } else {
            ""
        };
        return format!("{abs}class {}", ck.type_to_string(t));
    }
    if flags.intersects(SymbolFlags::INTERFACE) {
        let t = ck.declared_type_of_symbol(s);
        return format!("interface {}", ck.type_to_string(t));
    }
    if flags.intersects(SymbolFlags::TYPE_ALIAS) {
        return type_alias_text(ck, s, &name);
    }
    if flags.intersects(SymbolFlags::ENUM) {
        let k = if flags.intersects(SymbolFlags::CONST_ENUM) {
            "const enum"
        } else {
            "enum"
        };
        return format!("{k} {name}");
    }
    if flags.intersects(SymbolFlags::ENUM_MEMBER) {
        let owner = owner_name(ck, s)
            .map(|o| format!("{o}."))
            .unwrap_or_default();
        let t = ck.type_of_symbol(s);
        let value = match ck.store().kind(t) {
            TypeKind::EnumLiteral { value, .. } => format!(" = {}", ck.type_to_string(value)),
            _ => String::new(),
        };
        return format!("(enum member) {owner}{name}{value}");
    }
    if flags.intersects(SymbolFlags::TYPE_PARAMETER) {
        let t = ck.declared_type_of_symbol(s);
        return match ck.type_param_constraint(t) {
            Some(cn) => format!("(type parameter) {name} extends {}", ck.type_to_string(cn)),
            None => format!("(type parameter) {name}"),
        };
    }
    if flags.intersects(SymbolFlags::FUNCTION) && !flags.intersects(SymbolFlags::VARIABLE) {
        let t = ck.type_of_symbol(s);
        return signatures_text(ck, t, &format!("function {name}"), SigKind::Call);
    }
    if flags.intersects(SymbolFlags::CONSTRUCTOR) {
        let owner = owner_name(ck, s).unwrap_or_default();
        let t = c
            .sym_parent(s)
            .map(|p| ck.type_of_symbol(p))
            .unwrap_or(TypeId::ANY);
        return signatures_text(ck, t, &format!("constructor {owner}"), SigKind::Construct);
    }
    if flags.intersects(SymbolFlags::METHOD) {
        let owner = owner_name(ck, s)
            .map(|o| format!("{o}."))
            .unwrap_or_default();
        let t = type_at(ck, s, at);
        return signatures_text(ck, t, &format!("(method) {owner}{name}"), SigKind::Call);
    }
    if flags.intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE)
        && !flags.intersects(SymbolFlags::VARIABLE | SymbolFlags::FUNCTION)
    {
        return format!("namespace {name}");
    }
    let optional = if flags.intersects(SymbolFlags::OPTIONAL) {
        "?"
    } else {
        ""
    };
    let t = type_at(ck, s, at);
    let ts = ck.type_to_string(t);
    if flags.intersects(SymbolFlags::PROPERTY | SymbolFlags::ACCESSOR) {
        let owner = owner_name(ck, s)
            .map(|o| format!("{o}."))
            .unwrap_or_default();
        return format!("(property) {owner}{name}{optional}: {ts}");
    }
    if flags.intersects(SymbolFlags::PARAMETER) {
        return format!("(parameter) {name}{optional}: {ts}");
    }
    if flags.intersects(SymbolFlags::VARIABLE) {
        let kw = var_keyword(ck, s);
        return format!("{kw} {name}: {ts}");
    }
    format!("{name}: {ts}")
}

/// The type of `s` as seen at `at`: narrowed for expression names, declared otherwise.
fn type_at(ck: &mut Ck<'_, '_>, s: SymId, at: Option<Loc>) -> TypeId {
    let c = ck.c;
    if let Some(l) = at {
        if let Some(p) = c.parent(l)
            && let Node::Member { prop, .. } = *c.node(p)
            && prop == l.node
            && c.parent(p).is_some()
        {
            return ck.check_expr(p);
        }
        if is_expression_name(ck, l) && matches!(c.node(l), Node::Ident { .. }) {
            return ck.check_expr(l);
        }
    }
    ck.type_of_symbol(s)
}

/// `prefix(sig)` for the first signature, with `(+N overloads)`.
fn signatures_text(ck: &mut Ck<'_, '_>, t: TypeId, prefix: &str, kind: SigKind) -> String {
    let sigs = ck.signatures_of(t, kind);
    let Some(&first) = sigs.first() else {
        let ts = ck.type_to_string(t);
        return format!("{prefix}: {ts}");
    };
    let mut stack = Vec::new();
    let sig = ck.print_signature(first, kind, false, 0, &mut stack);
    let sig = if kind == SigKind::Construct {
        sig.strip_prefix("new ").unwrap_or(&sig).to_string()
    } else {
        sig
    };
    match sigs.len() {
        1 => format!("{prefix}{sig}"),
        2 => format!("{prefix}{sig} (+1 overload)"),
        n => format!("{prefix}{sig} (+{} overloads)", n - 1),
    }
}

/// The class/interface/enum/namespace a member belongs to.
fn owner_name(ck: &Ck<'_, '_>, s: SymId) -> Option<String> {
    let c = ck.c;
    if let Some(p) = c.sym_parent(s) {
        let n = c.sym_name(p);
        if !n.is_empty() && !n.starts_with("__") {
            return Some(n.to_string());
        }
    }
    // Instantiated members have no binder parent: walk up from the declaration.
    let d = c.first_decl(s)?;
    let mut cur = c.parent(d);
    for _ in 0..64 {
        let l = cur?;
        match *c.node(l) {
            Node::ClassDecl { name: Some(n), .. } | Node::ClassExpr { name: Some(n), .. } => {
                return name_text(c.ast(l.file), n).map(str::to_string);
            }
            Node::Interface { name, .. } | Node::Enum { name, .. } => {
                return name_text(c.ast(l.file), name).map(str::to_string);
            }
            Node::TypeLiteral { .. } | Node::ObjectLit { .. } => {
                // `__type` / `__object` in tsserver; show the property alone.
                return None;
            }
            _ => cur = c.parent(l),
        }
    }
    None
}

/// `const`, `let`, `var`, `using` from the variable's declaration.
fn var_keyword(ck: &Ck<'_, '_>, s: SymId) -> &'static str {
    let c = ck.c;
    let Some(d) = c.first_decl(s) else {
        return "var";
    };
    let mut cur = Some(d);
    for _ in 0..16 {
        let Some(l) = cur else { break };
        match *c.node(l) {
            Node::VarDecl { kind, .. } => {
                return match kind {
                    VarKind::Var => "var",
                    VarKind::Let => "let",
                    VarKind::Const => "const",
                    VarKind::Using => "using",
                    VarKind::AwaitUsing => "await using",
                };
            }
            Node::CatchClause { .. } | Node::Param { .. } => return "var",
            _ => cur = c.parent(l),
        }
    }
    if c.sym_flags(s)
        .intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
    {
        "let"
    } else {
        "var"
    }
}

/// `type A<T> = <source text of the aliased type>`.
fn type_alias_text(ck: &mut Ck<'_, '_>, s: SymId, name: &str) -> String {
    let c = ck.c;
    let Some(d) = c.first_decl(s) else {
        return format!("type {name}");
    };
    if let Node::TypeAlias {
        type_params, ty, ..
    } = *c.node(d)
    {
        let tps = type_params
            .map(|tp| c.text(Loc::new(d.file, tp)).to_string())
            .unwrap_or_default();
        let body = collapse_ws(c.text(Loc::new(d.file, ty)));
        return format!("type {name}{tps} = {}", truncate(&body, 400));
    }
    format!("type {name}")
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

fn strip_ts_ext(path: &str) -> &str {
    for ext in [
        ".d.ts", ".d.mts", ".d.cts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs",
    ] {
        if let Some(s) = path.strip_suffix(ext) {
            return s;
        }
    }
    path
}

/// The JSDoc of a symbol's first documented declaration, as markdown.
pub fn docs_of(ck: &Ck<'_, '_>, s: SymId) -> String {
    let c = ck.c;
    for d in c.decls(s) {
        if let Some(raw) = jsdoc_before(ck, d) {
            return jsdoc_markdown(&raw);
        }
    }
    String::new()
}

/// The `/** ... */` comment right before a declaration (or its statement).
fn jsdoc_before(ck: &Ck<'_, '_>, d: Loc) -> Option<String> {
    let c = ck.c;
    let ast = c.ast(d.file);
    let text = c.file(d.file).text.as_str();
    // From a binding name to the statement that carries the comment.
    let mut node = d;
    for _ in 0..4 {
        let Some(p) = c.parent(node) else { break };
        let climb = match *c.node(p) {
            Node::VarDeclarator { name, .. } => name == node.node,
            Node::VarDecl { decls, .. } => ast.list(decls).first() == Some(&node.node),
            Node::ExportDefaultExpr { .. } => true,
            _ => false,
        };
        if !climb {
            break;
        }
        node = p;
    }
    let start = ast.span(node.node).start;
    let comment = ast.comments_between(0, start).last()?;
    let between = text.get(comment.span.end as usize..start as usize)?;
    if !between.trim().is_empty() {
        return None;
    }
    let raw = comment.span.text(text);
    raw.starts_with("/**").then(|| raw.to_string())
}

/// JSDoc text to markdown: the description, then `*@tag*` lines like VS Code shows them.
pub fn jsdoc_markdown(raw: &str) -> String {
    let body = raw
        .strip_prefix("/**")
        .unwrap_or(raw)
        .strip_suffix("*/")
        .unwrap_or(raw);
    let mut desc = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    for line in body.lines() {
        let l = line.trim_start();
        let l = l.strip_prefix('*').unwrap_or(l);
        let l = l.strip_prefix(' ').unwrap_or(l);
        if let Some(rest) = l.strip_prefix('@') {
            let (tag, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let rest = rest.trim();
            let t = if matches!(tag, "param" | "property" | "prop" | "template") {
                let rest = rest.trim_start_matches('{');
                let rest = match rest.find('}') {
                    Some(i) if l.contains('{') => rest[i + 1..].trim(),
                    _ => rest,
                };
                let (n, d) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let d = d.trim().trim_start_matches('-').trim();
                if d.is_empty() {
                    format!("*@{tag}* `{n}`")
                } else {
                    format!("*@{tag}* `{n}` — {d}")
                }
            } else if rest.is_empty() {
                format!("*@{tag}*")
            } else {
                format!("*@{tag}* — {rest}")
            };
            tags.push(t);
        } else if tags.is_empty() {
            desc.push(l.trim_end().to_string());
        } else if let Some(last) = tags.last_mut() {
            let l = l.trim();
            if !l.is_empty() {
                last.push(' ');
                last.push_str(l);
            }
        }
    }
    while desc.last().is_some_and(|l| l.is_empty()) {
        desc.pop();
    }
    while desc.first().is_some_and(|l| l.is_empty()) {
        desc.remove(0);
    }
    let mut out = desc.join("\n");
    if !tags.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&tags.join("\n\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsdoc_to_markdown() {
        let md = jsdoc_markdown(
            "/**\n * Adds.\n *\n * @param a - first\n * @param {number} b second\n * @returns sum\n */",
        );
        assert_eq!(
            md,
            "Adds.\n\n*@param* `a` — first\n\n*@param* `b` — second\n\n*@returns* — sum"
        );
        assert_eq!(jsdoc_markdown("/** One line. */"), "One line.");
    }
}
