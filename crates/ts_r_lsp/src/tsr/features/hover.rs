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
                    let ts = super::multiline::multiline_objects(&ts);
                    let mut value = format!("```typescript\n{ts}\n```");
                    // Through an index signature: its documentation.
                    if let Node::Member { obj, .. } = *c.node(p)
                        && let Some(name) = name_text(ast, n)
                    {
                        let ot = ck.check_expr(Loc::new(cx.file, obj));
                        let doc = super::doc_source::index_documentation(ck, ot, name);
                        if !doc.is_empty() {
                            value.push('\n');
                            value.push_str(&doc);
                        }
                    }
                    return json!({
                        "contents": {"kind": "markdown", "value": value},
                        "range": lines.range(text, span.start, span.end),
                    });
                }
                return Value::Null;
            };
            let display =
                super::quick_info::quick_info(ck, s, l, m.is_decl || in_module_name(ck, l));
            (display, super::doc_source::documentation(ck, s, Some(l)))
        }
        // A keyword of a declaration (`export default class {}`, `function`): what it declares.
        Node::ClassDecl { .. }
        | Node::ClassExpr { .. }
        | Node::FunctionDecl { .. }
        | Node::Interface { .. }
        | Node::Enum { .. }
            if on_decl_keyword(text, offset) =>
        {
            let Some(s) = c.sym_of_decl(l) else {
                return Value::Null;
            };
            let display = super::quick_info::quick_info(ck, s, l, true);
            (display, super::doc_source::documentation(ck, s, Some(l)))
        }
        // A literal member name (`"a"() {}`, `1: x`) or element access key (`o["a"]`).
        Node::StringLit { .. } | Node::NumberLit { .. } => {
            let Some((s, is_decl)) = literal_name_symbol(ck, l) else {
                return Value::Null;
            };
            let display = super::quick_info::quick_info(ck, s, l, is_decl);
            (display, super::doc_source::documentation(ck, s, Some(l)))
        }
        _ => return Value::Null,
    };
    let display = super::multiline::multiline_objects(&display);
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

/// Whether `offset` is on a declaration keyword that tsc's quick info answers for
/// (`default`, `class`, `function`, `interface`, `enum`; not `export`).
fn on_decl_keyword(text: &str, offset: u32) -> bool {
    let b = text.as_bytes();
    let o = (offset as usize).min(b.len());
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let mut start = o;
    while start > 0 && word(b[start - 1]) {
        start -= 1;
    }
    let mut end = o;
    while end < b.len() && word(b[end]) {
        end += 1;
    }
    matches!(
        &text[start..end],
        "default" | "class" | "function" | "interface" | "enum"
    )
}

/// Whether `l` is part of a dotted namespace declaration name (`namespace a.b {}`).
fn in_module_name(ck: &Ck<'_, '_>, l: Loc) -> bool {
    let c = ck.c;
    let mut cur = l;
    for _ in 0..64 {
        let Some(p) = c.parent(cur) else {
            return false;
        };
        match *c.node(p) {
            Node::QualifiedName { .. } => cur = p,
            Node::Module { name, .. } => return name == cur.node && cur != l,
            _ => return false,
        }
    }
    false
}

/// The member a string or number literal names: as a declaration name, or as the key of
/// an element access (`this["m"]`, `C[1]`).
fn literal_name_symbol(ck: &mut Ck<'_, '_>, l: Loc) -> Option<(SymId, bool)> {
    let c = ck.c;
    let ast = c.ast(l.file);
    let p = c.parent(l)?;
    if ts_r_binder::decl_name_node(ast, p.node) == Some(l.node)
        && let Some(s) = c.sym_of_decl(p)
    {
        return Some((s, true));
    }
    let Node::Index { obj, index, .. } = *ast.node(p.node) else {
        return None;
    };
    if index != l.node {
        return None;
    }
    let key = match *ast.node(l.node) {
        Node::StringLit { value } => ast.atom(value).to_string(),
        Node::NumberLit { value } => ts_r_binder::js_number_to_string(value.get()),
        _ => return None,
    };
    let t = ck.check_expr(Loc::new(l.file, obj));
    super::locate::accessed_property(ck, t, &key).map(|s| (s, false))
}

/// `text` with the first whole-word `from` of its first line replaced by `to`.
fn rename_word(text: &str, from: &str, to: &str) -> String {
    let line = text.find('\n').unwrap_or(text.len());
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = text.as_bytes();
    let mut i = 0;
    while !from.is_empty() && from != to {
        let Some(at) = text[i..line].find(from).map(|a| a + i) else {
            break;
        };
        let end = at + from.len();
        if (at == 0 || !word(bytes[at - 1])) && (end >= bytes.len() || !word(bytes[end])) {
            return format!("{}{to}{}", &text[..at], &text[end..]);
        }
        i = end;
        if i >= line {
            break;
        }
    }
    text.to_string()
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
            // tsc 7 describes the target under the alias's own name: `(alias) interface I`,
            // `(alias) module m` (`import m = require()`), `(alias) namespace ns` (`* as ns`).
            Some(t) if t != s && depth < 4 => {
                if c.sym_flags(t).intersects(SymbolFlags::MODULE_FILE) {
                    let equals = c.first_decl(s).is_some_and(|d| {
                        let parents = c.file(d.file).parents();
                        [Some(d.node), parents.parent(d.node)]
                            .into_iter()
                            .flatten()
                            .any(|n| matches!(c.ast(d.file).node(n), Node::ImportEquals { .. }))
                    });
                    let kw = if equals { "module" } else { "namespace" };
                    return format!("(alias) {kw} {name}");
                }
                let inner = describe_depth(ck, t, None, depth + 1);
                format!("(alias) {}", rename_word(&inner, c.sym_name(t), &name))
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

pub(super) fn collapse_ws(s: &str) -> String {
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

/// The JSDoc of a symbol, as markdown (no location: completion details).
pub fn docs_of(ck: &mut Ck<'_, '_>, s: SymId) -> String {
    super::doc_source::documentation(ck, s, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_word_replaces_whole_words_on_the_first_line() {
        assert_eq!(rename_word("interface x", "x", "test"), "interface test");
        assert_eq!(rename_word("const xs: x", "x", "y"), "const xs: y");
        assert_eq!(rename_word("class A\nA", "A", "B"), "class B\nA");
        assert_eq!(rename_word("type T = U", "V", "W"), "type T = U");
    }
}
