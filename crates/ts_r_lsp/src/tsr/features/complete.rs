//! Completions: members after `.`, names in scope (locals, globals, keywords) and
//! auto-imports from the project's own modules; `completionItem/resolve` adds the detail line
//! and documentation.

use super::hover::{describe, docs_of};
use super::locate::{meaning_at, node_at};
use super::{Ctx, LspLines};
use serde_json::{Value, json};
use std::collections::HashSet;
use ts_r_ast::{Ast, Modifiers, Node, NodeId};
use ts_r_binder::{SymbolFacts, SymbolFlags, meaning};
use ts_r_checker::checker::GlobalType;
use ts_r_checker::members::SigKind;
use ts_r_checker::{Checker, Ck, Loc};
use ts_r_config::{CompilerOptions, ModuleResolution, RealFs};
use ts_r_diagnostics::FileId;
use ts_r_resolver::{ModuleFormat, Resolver, ResolverOptions};
use ts_r_types::{SymId, TypeFlags, TypeId};

/// Sort groups (tsserver's: locals before globals before auto-imports).
const SORT_LOCAL: &str = "11";
const SORT_MEMBER: &str = "11";
const SORT_OPTIONAL: &str = "12";
const SORT_SPREAD_MEMBER: &str = "13";
const SORT_THIS_MEMBER: &str = "14";
const SORT_GLOBAL: &str = "15";
const SORT_AUTO_IMPORT: &str = "16";
const SORT_KEYWORD: &str = "15";

/// Auto-import candidates offered at most (the client filters further as the user types).
const MAX_AUTO_IMPORTS: usize = 500;

const KEYWORDS: &[&str] = &[
    "abstract",
    "any",
    "as",
    "async",
    "await",
    "bigint",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
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
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "module",
    "namespace",
    "never",
    "new",
    "null",
    "number",
    "object",
    "package",
    "readonly",
    "return",
    "satisfies",
    "string",
    "super",
    "switch",
    "symbol",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unique",
    "unknown",
    "using",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// What the cursor completes.
enum Context {
    /// After `obj.` (or `A.` in a type).
    Member { obj: NodeId, qualified: bool },
    /// A name in scope; `types_only` in a type reference, `values_only` in an expression;
    /// `jsx_tag` for a JSX tag name (intrinsic element names first).
    Scope {
        anchor: NodeId,
        types_only: bool,
        values_only: bool,
        jsx_tag: bool,
    },
    /// A member name in a class, interface or type literal body: the inherited members not
    /// written yet, with the member keywords.
    ClassMember {
        slot: super::class_items::MemberSlot,
    },
    /// An attribute name in a JSX opening tag.
    JsxAttrs { opening: NodeId },
    /// In a JSX closing tag: the tag it closes.
    JsxClosing { label: String },
    /// Inside a string literal: the literals, property names or module paths it may hold.
    String { lit: NodeId },
    /// Inside a comment: only `/// <reference path="|"` offers anything.
    Reference,
    /// After `import("m").A.` in a type: what `m`'s `A` exports.
    ImportTypeMember { node: NodeId, dot: u32 },
    /// Inside a JSDoc comment (`jsdoc_items.rs`).
    JsDoc(super::jsdoc_items::Spot),
    /// A name being declared that may start with a keyword (a constructor parameter).
    Keywords(&'static [&'static str]),
    /// A name in `import { | } from "m"` / `export { | } from "m"`: the module's exports
    /// not named yet (and `type` unless it was just written).
    ImportClause { decl: NodeId, after_type: bool },
    /// Nothing.
    None,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Where the identifier being typed at `offset` starts.
fn ws_of(c: &Checker<'_>, file: FileId, offset: u32) -> u32 {
    word_before(&c.file(file).text, offset).0
}

/// The identifier prefix being typed: `(start, text)`.
fn word_before(text: &str, offset: u32) -> (u32, &str) {
    let b = text.as_bytes();
    let mut s = offset as usize;
    while s > 0 && is_ident_byte(b[s - 1]) {
        s -= 1;
    }
    (s as u32, &text[s..offset as usize])
}

fn context(ck: &Ck<'_, '_>, file: FileId, offset: u32) -> (Context, String) {
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (ws, prefix) = word_before(text, offset);
    let prefix = prefix.to_string();
    if in_comment(ast, offset) {
        if let Some(spot) = super::jsdoc_items::spot(c, file, offset) {
            return (Context::JsDoc(spot), prefix);
        }
        return (Context::Reference, prefix);
    }
    if let Some(lit) = super::strings::literal_at(ck, file, offset) {
        return (Context::String { lit }, prefix);
    }
    let node = node_at(ast, offset);
    if let Some(n) = node
        && matches!(
            ast.node(n),
            // A template's own node holds the cursor only in a `${}` hole (its text parts
            // are `TemplateElement`s): that completes like any expression.
            Node::StringLit { .. }
                | Node::TemplateElement { .. }
                | Node::RegexLit { .. }
                | Node::Directive { .. }
        )
        && ast.span(n).start < offset
        && offset < ast.span(n).end
    {
        return (Context::None, prefix);
    }
    // JSX: attribute names, the closing tag, nothing in text.
    let anchor0 = node.unwrap_or(ast.root);
    let word_node0 = if ws < offset {
        super::position::node_covering(ast, ws).unwrap_or(anchor0)
    } else {
        anchor0
    };
    let mut jsx_tag = false;
    if c.file(file).path.ends_with("x") {
        match super::jsx_items::jsx_spot(c, file, word_node0, ws) {
            Some(super::jsx_items::JsxSpot::Attrs { opening }) => {
                return (Context::JsxAttrs { opening }, prefix);
            }
            Some(super::jsx_items::JsxSpot::Closing { label }) => {
                return (Context::JsxClosing { label }, prefix);
            }
            Some(super::jsx_items::JsxSpot::Text) => return (Context::None, prefix),
            Some(super::jsx_items::JsxSpot::Tag) => jsx_tag = true,
            None => {}
        }
    }
    if let Some((decl, after_type)) = import_clause_at(c, file, ws) {
        return (Context::ImportClause { decl, after_type }, prefix);
    }
    // `obj.|` / `obj.pre|`
    let b = text.as_bytes();
    let mut d = ws as usize;
    while d > 0 && b[d - 1].is_ascii_whitespace() {
        d -= 1;
    }
    // `1.5|` is a number being written, not a member.
    let number = prefix.as_bytes().first().is_some_and(u8::is_ascii_digit);
    if !number && d > 0 && b[d - 1] == b'.' && !(d > 1 && b[d - 2] == b'.') {
        let dot = (d - 1) as u32;
        let mut cur = node_at(ast, dot + 1);
        // `import("m").|` and `import("m").A.|` in a type.
        let mut up = cur;
        for _ in 0..8 {
            let Some(n) = up else { break };
            if let Node::ImportType { arg, .. } = *ast.node(n)
                && ast.span(arg).end <= dot
            {
                return (Context::ImportTypeMember { node: n, dot }, prefix);
            }
            up = c.file(file).parents().parent(n);
        }
        for _ in 0..4 {
            let Some(n) = cur else { break };
            match *ast.node(n) {
                Node::Member { obj, .. } if ast.span(obj).end <= dot => {
                    return (
                        Context::Member {
                            obj,
                            qualified: false,
                        },
                        prefix,
                    );
                }
                Node::JsxMemberExpr { obj, .. } if ast.span(obj).end <= dot => {
                    return (
                        Context::Member {
                            obj,
                            qualified: false,
                        },
                        prefix,
                    );
                }
                Node::QualifiedName { left, .. } if ast.span(left).end <= dot => {
                    return (
                        Context::Member {
                            obj: left,
                            qualified: true,
                        },
                        prefix,
                    );
                }
                _ => cur = c.file(file).parents().parent(n),
            }
        }
        // A number like `1.` or something unparsed: no member list.
        return (Context::None, prefix);
    }
    let anchor = node.unwrap_or(ast.root);
    // The word being typed decides where it is (the cursor may touch the next node too, as
    // in `b|() {}`).
    let word_node = if ws < offset {
        super::position::node_covering(ast, ws).unwrap_or(anchor)
    } else {
        anchor
    };
    if let Some(slot) = super::class_items::member_slot(c, file, word_node, ws) {
        return (Context::ClassMember { slot }, prefix);
    }
    // Elsewhere in a class or interface body, outside members (`a |`): nothing.
    if super::class_items::in_body(c, file, word_node, ws) {
        return (Context::None, prefix);
    }
    // A name being written (`var |`, `function f(|`, `import * as |`): nothing, or keywords.
    match super::definitions::definition_spot(c, file, word_node, ws, ws < offset) {
        Some(super::definitions::Spot::Nothing) => return (Context::None, prefix),
        Some(super::definitions::Spot::Keywords(k)) => return (Context::Keywords(k), prefix),
        None => {}
    }
    let anchor = scope_anchor(c, file, offset, ws, anchor);
    let parent = c.file(file).parents().parent(anchor);
    // A name being declared (`var a|`, `function f|`, `catch (e|`) gets no list.
    if let Some(p) = parent
        && matches!(ast.node(anchor), Node::Ident { .. })
        && declares(ast.node(p), anchor)
    {
        return (Context::None, prefix);
    }
    let types_only = super::position::is_type_location(c, file, anchor, ws);
    // `export { | }` names locals of any meaning.
    let in_export = [Some(anchor), parent].into_iter().flatten().any(|n| {
        matches!(
            ast.node(n),
            Node::ExportNamed { .. } | Node::ExportSpecifier { .. }
        )
    });
    (
        Context::Scope {
            anchor,
            types_only,
            values_only: !types_only && !in_export,
            jsx_tag,
        },
        prefix,
    )
}

/// The node whose scope an empty word at `offset` completes from. After a closing `}`, `)`,
/// `;` or `]` it is the innermost node that holds the position strictly (`f() {}|` is outside
/// `f`); after anything else, the node the token before ends (`(p) => |` at the end of the
/// file is in the arrow function, though the parser's placeholder body has no width).
fn scope_anchor(c: &Checker<'_>, file: FileId, offset: u32, ws: u32, anchor: NodeId) -> NodeId {
    if ws < offset {
        return anchor;
    }
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    let Some((at, tok)) = super::position::token_before(ast, text, ws) else {
        return anchor;
    };
    if matches!(tok, "}" | ")" | ";" | "]") {
        let mut n = anchor;
        for _ in 0..10_000 {
            if n == ast.root || ast.span(n).end > offset {
                break;
            }
            match parents.parent(n) {
                Some(p) => n = p,
                None => break,
            }
        }
        return n;
    }
    let Some(deeper) = node_at(ast, at + tok.len() as u32) else {
        return anchor;
    };
    // Only a node inside the anchor (the anchor is where the cursor is, at least), and only
    // when a scope opens between the two (`public d = |` stays in the class body).
    let b = c.bound(file);
    let mut cur = Some(deeper);
    let mut scoped = false;
    for _ in 0..10_000 {
        let Some(n) = cur else { break };
        if n == anchor {
            return if scoped { deeper } else { anchor };
        }
        scoped |= b.scope_of(n).is_some();
        cur = parents.parent(n);
    }
    anchor
}

/// The node the token before an empty word ends (the word's own node otherwise).
fn token_node(c: &Checker<'_>, file: FileId, offset: u32, anchor: NodeId) -> NodeId {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (ws, _) = word_before(text, offset);
    if ws < offset {
        return anchor;
    }
    super::position::token_before(ast, text, ws)
        .and_then(|(at, tok)| node_at(ast, at + tok.len() as u32))
        .unwrap_or(anchor)
}

/// Names of the parameters not yet declared at `l`: inside a parameter's type or default,
/// that parameter's and the later ones' (`f(a, b = |, c)` offers `a` only).
fn later_params(c: &Checker<'_>, l: Loc) -> HashSet<String> {
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut out = HashSet::new();
    let mut child = None;
    let mut cur = Some(l.node);
    for _ in 0..10_000 {
        let Some(n) = cur else { break };
        match *ast.node(n) {
            Node::Param { pattern, .. } if Some(pattern) != child => {
                let Some(func) = parents.parent(n) else { break };
                let params = match *ast.node(func) {
                    Node::FunctionDecl { params, .. }
                    | Node::FunctionExpr { params, .. }
                    | Node::Arrow { params, .. } => params,
                    _ => break,
                };
                let list = ast.list(params);
                if let Some(i) = list.iter().position(|&p| p == n) {
                    for &p in &list[i..] {
                        if let Node::Param { pattern, .. } = *ast.node(p) {
                            binding_names(ast, pattern, &mut out, 0);
                        }
                    }
                }
                break;
            }
            Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::Block { .. } => break,
            _ => {}
        }
        child = Some(n);
        cur = parents.parent(n);
    }
    out
}

/// The names a binding pattern declares.
fn binding_names(ast: &Ast, n: NodeId, out: &mut HashSet<String>, depth: u32) {
    if depth > 64 {
        return;
    }
    match *ast.node(n) {
        Node::Ident { name } => {
            out.insert(ast.atom(name).to_string());
        }
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                binding_names(ast, p, out, depth + 1);
            }
        }
        Node::ArrayPattern { elements } => {
            for &p in ast.list(elements) {
                binding_names(ast, p, out, depth + 1);
            }
        }
        Node::BindingProperty { key, value, .. } => {
            binding_names(ast, value.unwrap_or(key), out, depth + 1);
        }
        Node::AssignPattern { left, .. } => binding_names(ast, left, out, depth + 1),
        Node::Rest { arg } => binding_names(ast, arg, out, depth + 1),
        _ => {}
    }
}

/// Whether `name` is the name a declaration `parent` introduces.
fn declares(parent: &Node, name: NodeId) -> bool {
    match *parent {
        Node::VarDeclarator { name: n, .. }
        | Node::Interface { name: n, .. }
        | Node::TypeAlias { name: n, .. }
        | Node::Enum { name: n, .. }
        | Node::TypeParam { name: n, .. }
        | Node::Param { pattern: n, .. } => n == name,
        Node::FunctionDecl { name: n, .. }
        | Node::ClassDecl { name: n, .. }
        | Node::ClassExpr { name: n, .. }
        | Node::FunctionExpr { name: n, .. }
        | Node::CatchClause { param: n, .. } => n == Some(name),
        _ => false,
    }
}

fn in_comment(ast: &Ast, offset: u32) -> bool {
    let i = ast.comments.partition_point(|c| c.span.end < offset);
    ast.comments.get(i).is_some_and(|c| {
        c.span.start < offset
            && (offset < c.span.end
                || (c.kind == ts_r_ast::CommentKind::Line && offset == c.span.end))
    })
}

/// LSP CompletionItemKind of a symbol.
fn item_kind(c: &Checker<'_>, s: SymId) -> u32 {
    let s = c.resolve_alias(s).unwrap_or(s);
    let f = c.sym_flags(s);
    kind_of_flags(f)
}

fn kind_of_flags(f: SymbolFlags) -> u32 {
    if f.intersects(SymbolFlags::METHOD) {
        2
    } else if f.intersects(SymbolFlags::FUNCTION) {
        3
    } else if f.intersects(SymbolFlags::CONSTRUCTOR) {
        4
    } else if f.intersects(SymbolFlags::CLASS) {
        7
    } else if f.intersects(SymbolFlags::INTERFACE | SymbolFlags::TYPE_ALIAS) {
        8
    } else if f.intersects(SymbolFlags::ENUM) {
        13
    } else if f.intersects(SymbolFlags::ENUM_MEMBER) {
        20
    } else if f.intersects(
        SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE | SymbolFlags::MODULE_FILE,
    ) {
        9
    } else if f.intersects(SymbolFlags::TYPE_PARAMETER) {
        25
    } else if f.intersects(SymbolFlags::PROPERTY | SymbolFlags::ACCESSOR) {
        10
    } else {
        6
    }
}

/// A `#name` private name with an identifier after the `#`.
fn is_private_name(s: &str) -> bool {
    s.strip_prefix('#').is_some_and(is_identifier_name)
}

pub(crate) fn is_identifier_name(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && it.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// The completion list at `offset`.
pub fn completion(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, offset: u32) -> Value {
    let c = cx.checker;
    let uri = crate::uri::path_to_uri(&c.file(cx.file).path);
    let (ctx, prefix) = context(ck, cx.file, offset);
    let data = |kind: &str, name: &str| json!({"uri": uri, "o": offset, "k": kind, "n": name});
    let mut items = Vec::new();
    let mut incomplete = false;
    match ctx {
        Context::None => {}
        Context::Keywords(words) => {
            for k in words {
                items.push(json!({"label": k, "kind": 14, "sortText": SORT_KEYWORD}));
            }
        }
        Context::ImportClause { decl, after_type } => {
            let Some(names) = import_clause_names(c, cx.file, decl, ws_of(c, cx.file, offset))
            else {
                // No module (yet): only `type` may start a name (TypeScript, not in
                // `import type { | }`).
                let type_only = matches!(
                    c.node(Loc::new(cx.file, decl)),
                    Node::ImportDecl {
                        type_only: true,
                        ..
                    }
                );
                if after_type || type_only || !c.ast(cx.file).source_kind.is_typescript() {
                    return Value::Null;
                }
                items.push(json!({"label": "type", "kind": 14, "sortText": SORT_KEYWORD}));
                return json!({"isIncomplete": false, "items": items});
            };
            if names.is_empty() {
                return Value::Null;
            }
            let text = c.file(cx.file).text.as_str();
            let is_import = matches!(c.node(Loc::new(cx.file, decl)), Node::ImportDecl { .. });
            // `import { "a b" as a_b }`: a quoted export needs a local name, unless the
            // specifier already has one (`import { | as x }`).
            let rest = text.get(offset as usize..).unwrap_or("").trim_start();
            let has_as = rest.starts_with("as")
                && !rest[2..].starts_with(|ch: char| ch.is_alphanumeric() || ch == '_');
            for (name, s) in names {
                // A re-exported name (`export { x as "y" }`) is an alias: tsc's kind for it.
                let kind = if c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                    6
                } else {
                    item_kind(c, s)
                };
                let mut item = json!({
                    "label": name,
                    "kind": kind,
                    "sortText": SORT_MEMBER,
                });
                if !is_identifier_name(&name) {
                    let quoted = ts_r_checker::print::quote_string(&name);
                    let insert = if is_import && !has_as {
                        let local: String = name
                            .chars()
                            .map(|ch| {
                                if ch.is_alphanumeric() || ch == '$' {
                                    ch
                                } else {
                                    '_'
                                }
                            })
                            .collect();
                        format!("{quoted} as {local}")
                    } else {
                        quoted
                    };
                    item["insertText"] = json!(insert);
                }
                items.push(item);
            }
            if !after_type {
                items.push(json!({"label": "type", "kind": 14, "sortText": SORT_KEYWORD}));
            }
        }
        Context::String { lit } => {
            let template = super::strings::is_template(c.ast(cx.file), lit);
            let found = super::strings::string_items(cx, ck, lit, offset).unwrap_or_default();
            if found.is_empty() {
                return Value::Null;
            }
            for (label, kind) in found {
                let label = super::strings::escape_label(&label, template);
                let optional = kind & super::strings::OPTIONAL != 0;
                let kind = kind & !super::strings::OPTIONAL;
                let mut item = json!({"label": label, "kind": kind, "sortText": SORT_LOCAL});
                if optional {
                    mark_optional(&mut item, &label);
                }
                items.push(item);
            }
        }
        Context::Reference => {
            let found = super::strings::reference_items(cx, ck, offset).unwrap_or_default();
            if found.is_empty() {
                return Value::Null;
            }
            for (label, kind) in found {
                items.push(json!({"label": label, "kind": kind, "sortText": SORT_LOCAL}));
            }
        }
        Context::ImportTypeMember { node, dot } => {
            for (name, s) in import_type_members(ck, cx.file, node, dot) {
                items
                    .push(json!({"label": name, "kind": item_kind(c, s), "sortText": SORT_MEMBER}));
            }
        }
        Context::JsDoc(spot) => {
            use super::jsdoc_items::{Spot, TAG_NAMES};
            match spot {
                Spot::Nothing => return Value::Null,
                Spot::TagName | Spot::TagStart => {
                    let at = matches!(spot, Spot::TagStart);
                    for t in TAG_NAMES {
                        let label = if at {
                            format!("@{t}")
                        } else {
                            (*t).to_string()
                        };
                        items.push(json!({"label": label, "kind": 14, "sortText": SORT_LOCAL}));
                    }
                }
                Spot::ParamName(names) => {
                    for n in names {
                        items.push(json!({"label": n, "kind": 6, "sortText": SORT_LOCAL}));
                    }
                }
                Spot::ImportModule(fragment) => {
                    let found =
                        super::strings::static_module_items(cx, ck, &fragment).unwrap_or_default();
                    if found.is_empty() {
                        return Value::Null;
                    }
                    for (label, kind) in found {
                        items.push(json!({"label": label, "kind": kind, "sortText": SORT_LOCAL}));
                    }
                }
                Spot::ImportAttribute(key) => {
                    let found = super::strings::import_attribute_items(ck, &key);
                    if found.is_empty() {
                        return Value::Null;
                    }
                    for (label, kind) in found {
                        items.push(json!({"label": label, "kind": kind, "sortText": SORT_LOCAL}));
                    }
                }
                Spot::TypeMember => {
                    items.push(json!({"label": "readonly", "kind": 14, "sortText": SORT_KEYWORD}));
                }
                Spot::Type { host, qualifier } => {
                    jsdoc_type_items(ck, cx.file, host, &qualifier, &mut items);
                }
            }
        }
        Context::ClassMember { slot } => {
            // A type literal in a type argument: what the argument is expected to have.
            if !slot.is_class
                && let Some(found) =
                    super::expected::type_literal_items(ck, cx.file, slot.container)
            {
                for (name, s) in found {
                    let optional = is_optional(c, s);
                    let mut item = json!({
                        "label": name,
                        "kind": item_kind(c, s),
                        "sortText": if optional { SORT_OPTIONAL } else { SORT_MEMBER },
                    });
                    if optional {
                        mark_optional(&mut item, &name);
                    }
                    items.push(item);
                }
                return json!({"isIncomplete": false, "items": items});
            }
            for (name, s) in super::class_items::inherited_items(ck, cx.file, &slot, offset) {
                if !is_identifier_name(&name) {
                    continue;
                }
                let mut item = json!({
                    "label": name,
                    "kind": item_kind(c, s),
                    "sortText": SORT_LOCAL,
                });
                if is_optional(c, s) {
                    mark_optional(&mut item, &name);
                }
                items.push(item);
            }
            let keywords = if slot.is_class {
                super::class_items::CLASS_ELEMENT_KEYWORDS
            } else {
                super::class_items::TYPE_ELEMENT_KEYWORDS
            };
            for k in keywords {
                items.push(json!({"label": k, "kind": 14, "sortText": SORT_KEYWORD}));
            }
        }
        Context::Member { obj, qualified } => {
            let text = c.file(cx.file).text.as_str();
            // After `?.` private names are not offered (`a?.#x` is not written).
            let question_dot = {
                let b = text.as_bytes();
                let mut d = word_before(text, offset).0 as usize;
                while d > 0 && b[d - 1].is_ascii_whitespace() {
                    d -= 1;
                }
                d >= 2 && b[d - 2] == b'?'
            };
            let in_type = qualified && qualified_in_type(c, Loc::new(cx.file, obj));
            for (name, s) in members(ck, Loc::new(cx.file, obj), qualified) {
                if in_type && !type_referable(c, s) {
                    continue;
                }
                let plain = is_identifier_name(&name) || is_private_name(&name);
                if !plain && (qualified || name.is_empty()) {
                    continue;
                }
                if question_dot && is_private_name(&name) {
                    continue;
                }
                let mut item = json!({
                    "label": name,
                    "kind": item_kind(c, s),
                    "sortText": SORT_MEMBER,
                    "data": data("m", &name),
                });
                if !plain {
                    // `o.|` for a member that is not an identifier becomes `o["a b"]`: the
                    // edit replaces the dot (tsc 7 does the same).
                    let (ws, _) = word_before(text, offset);
                    let b = text.as_bytes();
                    let mut dot = ws as usize;
                    while dot > 0 && b[dot - 1].is_ascii_whitespace() {
                        dot -= 1;
                    }
                    let dot = dot.saturating_sub(1) as u32;
                    let quoted = serde_json::to_string(&name).unwrap_or_default();
                    let lines = LspLines::new(text);
                    let insert = format!("[{quoted}]");
                    item["filterText"] = json!(format!(".{name}"));
                    item["insertText"] = json!(insert);
                    item["textEdit"] = json!({
                        "range": lines.range(text, dot, offset),
                        "newText": insert,
                    });
                } else if is_optional(c, s) {
                    mark_optional(&mut item, &name);
                }
                items.push(item);
            }
            if !qualified {
                let have: HashSet<String> = items
                    .iter()
                    .filter_map(|i| i["label"].as_str().map(str::to_string))
                    .collect();
                items.extend(awaited_member_items(ck, cx.file, obj, offset, &have));
            }
        }
        Context::JsxAttrs { opening } => {
            for (name, s, by_spread) in super::jsx_items::attr_items(ck, cx.file, opening, offset) {
                let optional = is_optional(c, s);
                let sort = if by_spread {
                    SORT_SPREAD_MEMBER
                } else if optional {
                    SORT_OPTIONAL
                } else {
                    SORT_MEMBER
                };
                let mut item = json!({
                    "label": name,
                    "kind": item_kind(c, s),
                    "sortText": sort,
                });
                if optional {
                    mark_optional(&mut item, &name);
                }
                items.push(item);
            }
        }
        Context::JsxClosing { label } => {
            items.push(json!({"label": label, "kind": 7, "sortText": SORT_LOCAL}));
        }
        Context::Scope {
            anchor,
            mut types_only,
            mut values_only,
            jsx_tag,
        } => {
            // `f<|` of a generic `f`: a type argument is being written.
            let ws = ws_of(c, cx.file, offset);
            let after_typeof =
                super::position::token_before(c.ast(cx.file), &c.file(cx.file).text, ws)
                    .is_some_and(|(_, t)| t == "typeof");
            if !types_only && !after_typeof && possibly_type_argument(ck, cx.file, ws, 0) {
                types_only = true;
                values_only = false;
            }
            if jsx_tag {
                for (name, s) in super::jsx_items::intrinsic_tags(ck, Loc::new(cx.file, anchor)) {
                    items.push(json!({
                        "label": name,
                        "kind": item_kind(c, s),
                        "sortText": SORT_LOCAL,
                    }));
                }
            }
            if let Some(found) = object_items(ck, cx.file, anchor, offset)
                .or_else(|| pattern_items(ck, cx.file, anchor, offset))
            {
                for (name, s) in found {
                    let optional = is_optional(c, s);
                    let mut item = json!({
                        "label": name,
                        "kind": item_kind(c, s),
                        "sortText": if optional { SORT_OPTIONAL } else { SORT_MEMBER },
                    });
                    if optional {
                        mark_optional(&mut item, &name);
                    }
                    items.push(item);
                }
                return json!({"isIncomplete": false, "items": items});
            }
            // A value position offers values only (an interface, type alias or namespace
            // without values is not one); a type position types and the namespaces that may
            // hold them (`tsc: tsc/internal/ls/completions.go`, the global symbol filter).
            let want = if types_only {
                meaning::TYPE | meaning::NAMESPACE
            } else if values_only {
                meaning::VALUE
            } else {
                meaning::ALL
            };
            let mut seen: HashSet<String> = HashSet::new();
            // Literal values the position expects (`"a"`, `2`).
            for label in
                super::expected::literal_labels(ck, cx.file, anchor, ws_of(c, cx.file, offset))
            {
                if seen.insert(label.clone()) {
                    items.push(json!({"label": label, "kind": 21, "sortText": SORT_LOCAL}));
                }
            }
            let own = own_initializer(c, Loc::new(cx.file, anchor), offset, !prefix.is_empty());
            let later = later_params(c, Loc::new(cx.file, token_node(c, cx.file, offset, anchor)));
            for (name, s) in scope_names(c, Loc::new(cx.file, anchor)) {
                if !has_meaning_through_alias(c, s, want) || later.contains(&name) {
                    continue;
                }
                if types_only && !type_referable(c, s) {
                    continue;
                }
                if !seen.insert(name.clone()) || own.as_deref() == Some(name.as_str()) {
                    continue;
                }
                items.push(json!({
                    "label": name,
                    "kind": item_kind(c, s),
                    "sortText": SORT_LOCAL,
                    "data": data("s", &name),
                }));
            }
            if !types_only {
                for (name, s) in super::class_items::this_items(ck, Loc::new(cx.file, anchor)) {
                    if seen.contains(&name) {
                        continue;
                    }
                    let insert = if is_identifier_name(&name) {
                        format!("this.{name}")
                    } else {
                        format!("this[{}]", serde_json::Value::String(name.clone()))
                    };
                    items.push(json!({
                        "label": name,
                        "kind": item_kind(c, s),
                        "sortText": SORT_THIS_MEMBER,
                        "insertText": insert,
                    }));
                }
            }
            if !types_only
                && has_arguments(c, Loc::new(cx.file, anchor))
                && seen.insert("arguments".to_string())
            {
                items.push(json!({
                    "label": "arguments",
                    "kind": 6,
                    "sortText": SORT_LOCAL,
                    "data": data("s", "arguments"),
                }));
            }
            // A CommonJS JavaScript module has `module` and `exports` as file locals.
            if !types_only && is_commonjs(c, cx.file) {
                for name in ["module", "exports"] {
                    if seen.insert(name.to_string()) {
                        items.push(json!({"label": name, "kind": 6, "sortText": SORT_LOCAL}));
                    }
                }
            }
            // Globals do not hide auto-imports (tsserver offers both); locals do.
            let locals = seen.clone();
            for (name, merged) in &c.link.globals {
                let name: &str = name;
                if !is_identifier_name(name) || seen.contains(name) {
                    continue;
                }
                let m = flags_meaning(merged.flags);
                if m & want == 0 {
                    continue;
                }
                seen.insert(name.to_string());
                items.push(json!({
                    "label": name,
                    "kind": kind_of_flags(merged.flags),
                    "sortText": SORT_GLOBAL,
                    "data": data("g", name),
                }));
            }
            if !types_only {
                for k in KEYWORDS {
                    if seen.insert((*k).to_string()) {
                        items.push(json!({"label": k, "kind": 14, "sortText": SORT_KEYWORD}));
                    }
                }
            }
            // Auto-imports need a typed prefix (as tsserver's list is filtered client-side,
            // the list is incomplete: typing more asks again).
            if !prefix.is_empty() {
                incomplete = true;
                items.extend(auto_imports(cx, ck, &prefix, &locals, want, offset, &uri));
            }
        }
    }
    json!({"isIncomplete": incomplete, "items": items})
}

/// In an async function, `p.|` of a promise also lists the awaited value's members (not
/// already listed), each written `(await p).m` over `p.` (`;(await p).m` at the start of a
/// statement that would otherwise continue the previous line; an `await` right before `p`
/// is taken into the edit instead of doubled). Observed on tsc 7's `completionOfAwaitPromise*`.
fn awaited_member_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    obj: NodeId,
    offset: u32,
    have: &HashSet<String>,
) -> Vec<Value> {
    let c = ck.c;
    let l = Loc::new(file, obj);
    let ast = c.ast(file);
    let Some(func) = ck.containing_function(l) else {
        return Vec::new();
    };
    if !ts_r_checker::nodes::fn_parts(ast, func.node)
        .is_some_and(|p| p.flags.contains(ts_r_ast::FnFlags::ASYNC))
    {
        return Vec::new();
    }
    let t = ck.check_expr(l);
    let awaited = ck.awaited_type(t);
    if awaited == t || ck.store().is_error(awaited) {
        return Vec::new();
    }
    let a = ck.apparent_type(awaited);
    let mut props: Vec<(String, SymId)> = ck
        .properties_of(a)
        .into_iter()
        .map(|(n, s)| (ck.store().str(n).to_string(), s))
        .filter(|(n, _)| !n.starts_with("__@") && !n.starts_with('#') && !have.contains(n))
        .collect();
    if props.is_empty() {
        return Vec::new();
    }
    props.sort_by(|x, y| x.0.cmp(&y.0));
    let text = c.file(file).text.as_str();
    let parents = c.file(file).parents();
    let sp = ast.span(obj);
    let expr = text.get(sp.start as usize..sp.end as usize).unwrap_or("");
    // The edit starts at the expression, or at an `await` applied to the member access.
    let member = parents.parent(obj);
    let mut start = sp.start;
    let mut outer = member.unwrap_or(obj);
    if let Some(m) = member
        && let Some(p) = parents.parent(m)
        && matches!(ast.node(p), Node::Await { .. })
    {
        start = ast.span(p).start;
        outer = p;
    }
    // `;` when the expression starts a statement right after a line without one.
    let starts_statement = parents.parent(outer).is_some_and(|p| {
        matches!(ast.node(p), Node::ExprStmt { .. }) && ast.span(p).start == start
    });
    let semi = starts_statement
        && super::position::token_before(ast, text, start)
            .is_some_and(|(_, t)| !matches!(t, ";" | "{" | "}"));
    let lines = LspLines::new(text);
    let range = lines.range(text, start, offset);
    props
        .into_iter()
        .map(|(name, s)| {
            let access = if is_identifier_name(&name) {
                format!(".{name}")
            } else {
                format!("[{}]", serde_json::to_string(&name).unwrap_or_default())
            };
            let insert = format!("{}(await {expr}){access}", if semi { ";" } else { "" });
            json!({
                "label": name,
                "kind": item_kind(c, s),
                "sortText": SORT_MEMBER,
                "filterText": insert,
                "insertText": insert,
                "textEdit": {"range": range, "newText": insert},
            })
        })
        .collect()
}

/// What `import("m").A.B.|` (the names before `dot`) offers: with `typeof`, the members
/// of that value; otherwise the exports that lead to a type.
fn import_type_members(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    node: NodeId,
    dot: u32,
) -> Vec<(String, SymId)> {
    let c = ck.c;
    let ast = c.ast(file);
    let Node::ImportType {
        is_typeof,
        arg,
        qualifier,
        ..
    } = *ast.node(node)
    else {
        return Vec::new();
    };
    let lit = match *ast.node(arg) {
        Node::TypeRef { name, .. } => name,
        _ => arg,
    };
    let bound = &c.file(file).bound;
    let Some(ri) = bound
        .module_requests
        .iter()
        .position(|r| r.node == lit || r.node == arg)
    else {
        return Vec::new();
    };
    let target = c.link.module_target(file, ri as u32);
    let Some(mut s) = c.module_target_symbol(target) else {
        return Vec::new();
    };
    // The qualifier's names written before the dot, outermost first.
    let mut names = Vec::new();
    let mut q = qualifier;
    for _ in 0..256 {
        let Some(n) = q else { break };
        match *ast.node(n) {
            Node::QualifiedName { left, right } => {
                names.push(right);
                q = Some(left);
            }
            _ => {
                names.push(n);
                q = None;
            }
        }
    }
    names.reverse();
    for n in names {
        if ast.span(n).end > dot || ast.span(n).start == ast.span(n).end {
            break;
        }
        let Some(text) = super::locate::name_text(ast, n) else {
            return Vec::new();
        };
        let Some((_, x)) = c.exports_of(s).into_iter().find(|(m, _)| m == text) else {
            return Vec::new();
        };
        s = c.resolve_alias(x).unwrap_or(x);
    }
    let mut out: Vec<(String, SymId)> = if is_typeof {
        let t = ck.type_of_symbol(s);
        let a = ck.apparent_type(t);
        let props = ck.properties_of(a);
        let store = ck.store();
        props
            .into_iter()
            .map(|(n, p)| (store.str(n).to_string(), p))
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|(n, p)| {
                !n.starts_with("__@")
                    && super::class_items::member_visible(ck, *p, n, Loc::new(file, node))
            })
            .collect()
    } else {
        c.exports_of(s)
            .into_iter()
            .filter(|(n, x)| n != "export=" && type_referable(c, *x))
            .collect()
    };
    out.retain(|(n, _)| is_identifier_name(n));
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Type keywords offered where a type is written.
const TYPE_KEYWORDS: &[&str] = &[
    "any",
    "bigint",
    "boolean",
    "never",
    "null",
    "number",
    "object",
    "string",
    "symbol",
    "undefined",
    "unknown",
    "void",
];

/// The types a JSDoc tag's `{...}` may name at the documented node `host`: types and
/// namespaces in scope (then globals and type keywords), or, after `A.B.`, what that
/// namespace exports that leads to a type.
fn jsdoc_type_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    host: NodeId,
    qualifier: &[String],
    items: &mut Vec<Value>,
) {
    let c = ck.c;
    let at = Loc::new(file, host);
    let want = meaning::TYPE | meaning::NAMESPACE;
    if let Some((first, rest)) = qualifier.split_first() {
        let Some(mut s) = ck
            .resolve_name_at(at, first, meaning::NAMESPACE)
            .or_else(|| ck.resolve_name_at(at, first, meaning::ALL))
        else {
            return;
        };
        s = c.resolve_alias(s).unwrap_or(s);
        for name in rest {
            let Some((_, x)) = c.exports_of(s).into_iter().find(|(n, _)| n == name) else {
                return;
            };
            s = c.resolve_alias(x).unwrap_or(x);
        }
        let mut out = c.exports_of(s);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, x) in out {
            if !is_identifier_name(&name) || !type_referable(c, x) {
                continue;
            }
            items.push(json!({"label": name, "kind": item_kind(c, x), "sortText": SORT_MEMBER}));
        }
        return;
    }
    let mut seen: HashSet<String> = HashSet::new();
    for (name, s) in scope_names(c, at) {
        if !has_meaning_through_alias(c, s, want) || !type_referable(c, s) {
            continue;
        }
        if seen.insert(name.clone()) {
            items.push(json!({"label": name, "kind": item_kind(c, s), "sortText": SORT_LOCAL}));
        }
    }
    for (name, merged) in &c.link.globals {
        let name: &str = name;
        if !is_identifier_name(name) || flags_meaning(merged.flags) & want == 0 {
            continue;
        }
        if seen.insert(name.to_string()) {
            items.push(json!({
                "label": name,
                "kind": kind_of_flags(merged.flags),
                "sortText": SORT_GLOBAL,
            }));
        }
    }
    for k in TYPE_KEYWORDS {
        if seen.insert((*k).to_string()) {
            items.push(json!({"label": k, "kind": 14, "sortText": SORT_KEYWORD}));
        }
    }
}

/// Whether the word at `pos` may be a type argument of an unclosed `f<...`: the identifier
/// before the `<` is part of a type, has call (construct, after `new`) signatures with at
/// least as many type parameters as arguments come before, or is itself such an argument.
fn possibly_type_argument(ck: &mut Ck<'_, '_>, file: FileId, pos: u32, depth: u32) -> bool {
    const MAX_DEPTH: u32 = 16;
    let c = ck.c;
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let Some((start, n)) = super::position::possible_type_arguments(ast, text, pos) else {
        return false;
    };
    let Some(id) = super::position::node_covering(ast, start) else {
        return false;
    };
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return false;
    }
    let parent = c.file(file).parents().parent(id);
    if let Some(p) = parent {
        let pn = ast.node(p);
        if declares(pn, id) {
            return false;
        }
        if matches!(
            pn,
            Node::TypeRef { .. } | Node::QualifiedName { .. } | Node::TypeQuery { .. }
        ) {
            return true;
        }
    }
    // `o.m<|`: the type of the access.
    let callee = match parent.map(|p| (p, ast.node(p))) {
        Some((p, Node::Member { prop, .. })) if *prop == id => p,
        _ => id,
    };
    let parent = c.file(file).parents().parent(callee);
    // `new Bar<|` reads as type arguments whatever `Bar` is (observed tsc 7).
    if super::position::token_before(ast, text, start).is_some_and(|(_, t)| t == "new") {
        return true;
    }
    // The parser already made type arguments of it: a type position.
    if let Some(
        Node::New {
            callee: x,
            type_args: Some(ta),
            ..
        }
        | Node::Call {
            callee: x,
            type_args: Some(ta),
            ..
        },
    ) = parent.map(|p| ast.node(p))
        && *x == callee
        && ast.span(*ta).start <= pos
        && pos <= ast.span(*ta).end
    {
        return true;
    }
    let l = Loc::new(file, callee);
    let t = ck.check_expr(l);
    let a = ck.apparent_type(t);
    let kind = match parent.map(|p| ast.node(p)) {
        Some(Node::New { callee: x, .. }) if *x == callee => SigKind::Construct,
        _ => SigKind::Call,
    };
    let generic = ck.signatures_of(a, kind).into_iter().any(|sig| {
        let store = ck.store();
        let k = store.list_items(store.sig(sig).type_params).len();
        k > 0 && k >= n as usize
    });
    generic || (depth < MAX_DEPTH && possibly_type_argument(ck, file, start, depth + 1))
}

/// Whether `s` has a meaning in `want`; an alias by what it resolves to (an alias that
/// resolves to nothing keeps every meaning).
/// `import { | } from "m"` / `export { | } from "m"` (the token before the name is the
/// clause's `{`, a `,`, or `type`): the declaration, and whether that token is `type`
/// (`tsc: tsc/internal/ls/completions.go`, the import/export clause completions).
fn import_clause_at(c: &Checker<'_>, file: FileId, ws: u32) -> Option<(NodeId, bool)> {
    let ast = c.ast(file);
    let text = c.file(file).text.as_str();
    let (tok, t) = super::position::token_before(ast, text, ws)?;
    if !matches!(t, "{" | "," | "type") {
        return None;
    }
    let parents = c.file(file).parents();
    let mut cur = node_at(ast, tok);
    for _ in 0..8 {
        let n = cur?;
        match *ast.node(n) {
            Node::ImportDecl { source, .. }
            | Node::ExportNamed {
                source: Some(source),
                ..
            } => {
                // Between the clause's `{` and the module specifier (braces in comments and
                // quoted names do not count).
                let start = ast.span(n).start;
                if !inside_open_brace(ast, text, start, tok) || tok >= ast.span(source).start {
                    return None;
                }
                return Some((n, t == "type"));
            }
            Node::ExportNamed { source: None, .. } => return None,
            _ => cur = parents.parent(n),
        }
    }
    None
}

/// Whether the code from `start` up to the `{`, `,` or `type` token at `tok` opens a `{`
/// that no `}` closes, outside comments and string literals.
fn inside_open_brace(ast: &ts_r_ast::Ast, text: &str, start: u32, tok: u32) -> bool {
    let b = text.as_bytes();
    let end = (tok as usize + 1).min(b.len());
    let mut i = start as usize;
    let mut open = false;
    while i < end {
        // A comment starting here: jump past it.
        let k = ast
            .comments
            .partition_point(|cm| (cm.span.start as usize) < i);
        if let Some(cm) = ast.comments.get(k)
            && cm.span.start as usize == i
            && cm.span.end as usize > i
        {
            i = cm.span.end as usize;
            continue;
        }
        match b[i] {
            q @ (b'"' | b'\'') => {
                // To the closing quote, or the end of the line for an unterminated one.
                i += 1;
                while i < end && b[i] != q && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' => open = true,
            b'}' => return false,
            _ => {}
        }
        i += 1;
    }
    open
}

/// The exports of the module an import or export clause names, minus `default` and the
/// names its other specifiers already take. `None` when the module does not resolve.
fn import_clause_names(
    c: &Checker<'_>,
    file: FileId,
    decl: NodeId,
    ws: u32,
) -> Option<Vec<(String, SymId)>> {
    let ast = c.ast(file);
    let (source, specifiers) = match *ast.node(decl) {
        Node::ImportDecl {
            source, specifiers, ..
        } => (source, specifiers),
        Node::ExportNamed {
            source: Some(source),
            specifiers,
            ..
        } => (source, specifiers),
        _ => return None,
    };
    let bound = &c.file(file).bound;
    let ri = bound
        .module_requests
        .iter()
        .position(|r| r.node == source)?;
    let target = c.link.module_target(file, ri as u32);
    let module = c.module_target_symbol(target)?;
    let mut taken: HashSet<String> = HashSet::new();
    for &sp in ast.list(specifiers) {
        let span = ast.span(sp);
        if span.start <= ws && ws <= span.end {
            continue;
        }
        let name = match *ast.node(sp) {
            Node::ImportSpecifier {
                imported, local, ..
            } => imported.unwrap_or(local),
            Node::ExportSpecifier { local, .. } => local,
            _ => continue,
        };
        let n = match *ast.node(name) {
            // The literal's value (its text may be unterminated).
            Node::StringLit { value } => ast.atom(value).to_string(),
            _ => match super::locate::name_text(ast, name) {
                Some(t) => t.to_string(),
                None => continue,
            },
        };
        taken.insert(n);
    }
    Some(
        c.exports_of(module)
            .into_iter()
            .filter(|(n, _)| n != "default" && !taken.contains(n))
            .collect(),
    )
}

/// Whether `s` may be named in a type: a type, or a namespace with such a member somewhere
/// below it (`tsc: tsc/internal/ls/completions.go`, the type-location symbol filter; a
/// namespace of values only is not offered where a type goes).
fn type_referable(c: &Checker<'_>, s: SymId) -> bool {
    let mut seen: HashSet<SymId> = HashSet::new();
    type_referable_in(c, s, 0, &mut seen)
}

/// [`type_referable`]'s search; `seen` holds the namespaces already searched (namespaces
/// that re-export each other are searched once each, not once per path).
fn type_referable_in(c: &Checker<'_>, s: SymId, depth: u32, seen: &mut HashSet<SymId>) -> bool {
    let s = c.resolve_alias(s).unwrap_or(s);
    if c.has_meaning(s, meaning::TYPE) {
        return true;
    }
    if depth > 16
        || !c
            .sym_flags(s)
            .intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE)
        || !seen.insert(s)
    {
        return false;
    }
    c.exports_of(s)
        .into_iter()
        .any(|(_, x)| type_referable_in(c, x, depth + 1, seen))
}

/// Whether the qualified name whose left side is `left` (`a.b.|`) is a type reference, not
/// the entity of `typeof a.b` or `import x = a.b`.
fn qualified_in_type(c: &Checker<'_>, left: Loc) -> bool {
    let mut n = left;
    for _ in 0..64 {
        let Some(p) = c.parent(n) else { return true };
        match c.node(p) {
            Node::QualifiedName { .. } => n = p,
            Node::TypeQuery { .. } | Node::ImportEquals { .. } => return false,
            _ => return true,
        }
    }
    true
}

fn has_meaning_through_alias(c: &Checker<'_>, s: SymId, want: u8) -> bool {
    let f = c.sym_flags(s);
    if !f.intersects(SymbolFlags::ALIAS) {
        return c.has_meaning(s, want);
    }
    // A name both imported and declared here (`import type { A }` and `const A`) has the
    // declaration's meanings too.
    let mut own = f;
    own.remove(SymbolFlags::ALIAS);
    if flags_meaning(own) & want != 0 {
        return true;
    }
    match c.resolve_alias(s) {
        Some(t) if t != s => c.has_meaning(t, want),
        _ => true,
    }
}

fn flags_meaning(f: SymbolFlags) -> u8 {
    let mut m = 0;
    if f.intersects(SymbolFlags::VALUE) {
        m |= meaning::VALUE;
    }
    if f.intersects(SymbolFlags::TYPE) {
        m |= meaning::TYPE;
    }
    if f.intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE) {
        m |= meaning::NAMESPACE;
    }
    if f.intersects(SymbolFlags::ALIAS) {
        m |= meaning::ALL;
    }
    m
}

/// Members of the object (or namespace) before the dot.
fn members(ck: &mut Ck<'_, '_>, obj: Loc, qualified: bool) -> Vec<(String, SymId)> {
    let c = ck.c;
    // A namespace, enum or module (`import * as ns`) lists its exports.
    let named = match c.node(obj) {
        Node::Ident { .. } => {
            let s = if qualified {
                c.resolve_ident(obj, meaning::NAMESPACE)
                    .or_else(|| c.resolve_ident(obj, meaning::ALL))
            } else {
                meaning_at(ck, obj).primary
            };
            s.map(|s| c.resolve_alias(s).unwrap_or(s))
        }
        Node::QualifiedName { right, .. } => meaning_at(ck, Loc::new(obj.file, *right))
            .primary
            .map(|s| c.resolve_alias(s).unwrap_or(s)),
        _ => None,
    };
    if let Some(s) = named {
        let f = c.sym_flags(s);
        if qualified {
            return c.exports_of(s);
        }
        // `ns.|` of a module in an expression: its values and namespaces, not its types.
        if f.intersects(SymbolFlags::MODULE_FILE) {
            return c
                .exports_of(s)
                .into_iter()
                .filter(|(_, x)| match c.resolve_alias(*x) {
                    Some(t) => c.has_meaning(t, meaning::VALUE | meaning::NAMESPACE),
                    None => true,
                })
                .collect();
        }
        if f.intersects(SymbolFlags::NAMESPACE_MODULE | SymbolFlags::VALUE_MODULE)
            && !f.intersects(
                SymbolFlags::CLASS
                    | SymbolFlags::FUNCTION
                    | SymbolFlags::VARIABLE
                    | SymbolFlags::ENUM,
            )
        {
            return c
                .exports_of(s)
                .into_iter()
                .filter(|(_, x)| {
                    c.has_meaning(
                        c.resolve_alias(*x).unwrap_or(*x),
                        meaning::VALUE | meaning::NAMESPACE,
                    )
                })
                .collect();
        }
    }
    if qualified {
        return Vec::new();
    }
    let t = ck.check_expr(obj);
    // `o.|` and `o?.|` on a possibly undefined value list the defined value's members.
    let t = ck.non_nullable(t);
    let a = ck.apparent_type(t);
    let mut props = ck.properties_of(a);
    // Callable and constructable values also have `Function`'s members (`apply`, `bind`,
    // `call`, ...), through `CallableFunction`/`NewableFunction` under `strictBindCallApply`.
    let call = !ck.signatures_of(a, SigKind::Call).is_empty();
    if call || !ck.signatures_of(a, SigKind::Construct).is_empty() {
        let strict = if !c.opts.strict_bind_call_apply {
            None
        } else if call {
            ck.global_type(GlobalType::CallableFunction)
        } else {
            ck.global_type(GlobalType::NewableFunction)
        };
        for ft in [strict, ck.global_type(GlobalType::Function)]
            .into_iter()
            .flatten()
        {
            if ft == a {
                continue;
            }
            for (n, s) in ck.properties_of(ft) {
                if !props.iter().any(|(m, _)| *m == n) {
                    props.push((n, s));
                }
            }
        }
    }
    let store = ck.store();
    let named: Vec<(String, SymId)> = props
        .into_iter()
        .map(|(n, s)| (store.str(n).to_string(), s))
        .filter(|(n, _)| !n.starts_with("__@"))
        .collect();
    // Private and protected members only where they may be read
    // (`tsc: tsc/internal/ls/completions.go`, the property-access validity filter).
    let mut out: Vec<(String, SymId)> = named
        .into_iter()
        .filter(|(n, s)| super::class_items::member_visible(ck, *s, n, obj))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Whether the property `s` is optional (declared with `?`, or made so by a mapped type or
/// a union).
pub(super) fn is_optional(c: &Checker<'_>, s: SymId) -> bool {
    c.sym_flags(s).intersects(SymbolFlags::OPTIONAL)
        || c.sym_check_flags(s) & ts_r_checker::symbols::check_flags::OPTIONAL != 0
}

/// An optional member's label ends in `?` while the typed and inserted text stays the plain
/// name (how VS Code's TypeScript extension shows it; tsc 7 does the same,
/// `tsc: tsc/internal/ls/completions.go`).
fn mark_optional(item: &mut Value, name: &str) {
    item["label"] = json!(format!("{name}?"));
    item["insertText"] = json!(name);
    item["filterText"] = json!(name);
}

/// The object literal whose property name the cursor is typing (`{ |`, `{ a: 1, pre|`):
/// the name follows the literal's `{` or a `,` between its members.
fn object_container(c: &Checker<'_>, file: FileId, anchor: NodeId, offset: u32) -> Option<NodeId> {
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    let obj = match *ast.node(anchor) {
        Node::ObjectLit { .. } => anchor,
        Node::Ident { .. } => {
            let p = parents.parent(anchor)?;
            match *ast.node(p) {
                Node::ShorthandProp { name, .. } if name == anchor => parents.parent(p)?,
                Node::Property { key, .. } if key == anchor => parents.parent(p)?,
                _ => return None,
            }
        }
        _ => return None,
    };
    if !matches!(ast.node(obj), Node::ObjectLit { .. }) || ast.span(obj).start >= offset {
        return None;
    }
    let text = c.file(file).text.as_bytes();
    let (ws, _) = word_before(&c.file(file).text, offset);
    let mut d = ws as usize;
    while d > 0 && text[d - 1].is_ascii_whitespace() {
        d -= 1;
    }
    if d == 0 || d as u32 <= ast.span(obj).start {
        return None;
    }
    if matches!(text[d - 1], b'{' | b',') {
        return Some(obj);
    }
    // A member just before the name without a comma between (`{ a: 1\n  b| }`, which the
    // completion's edit would fix): a shorthand one only from an earlier line.
    let Node::ObjectLit { props } = *ast.node(obj) else {
        return None;
    };
    let before = ast
        .list(props)
        .iter()
        .find(|&&p| ast.span(p).end == d as u32)?;
    if text[d - 1] == b':' {
        return None;
    }
    if matches!(ast.node(*before), Node::ShorthandProp { .. })
        && !text[d..ws as usize].contains(&b'\n')
    {
        return None;
    }
    Some(obj)
}

/// Property-name completions in an object literal: the properties of its contextual type
/// that no other member already writes. `None` when the literal has no contextual type (or
/// the type has no properties): the names in scope are offered instead.
/// (Rule from `tsc: tsc/internal/ls/completions.go`, the object-like completion symbols.)
/// The properties an object of contextual type `ct` may take: for a union, every property of
/// its object members (not of primitives, arrays or callable members).
pub(super) fn object_props(ck: &mut Ck<'_, '_>, ct: TypeId) -> Vec<(String, SymId)> {
    let c = ck.c;
    let mut props: Vec<(String, SymId)> = Vec::new();
    let members = ck.union_members(ct);
    let union = members.len() > 1;
    for m in members {
        let flags = ck.store().flags(m);
        if union
            && (flags.intersects(TypeFlags::PRIMITIVE)
                || c.is_array_like_type(m)
                || !ck.signatures_of(m, SigKind::Call).is_empty()
                || !ck.signatures_of(m, SigKind::Construct).is_empty())
        {
            continue;
        }
        if flags.intersects(TypeFlags::ANY_OR_UNKNOWN) {
            continue;
        }
        let a = ck.apparent_type(m);
        for (n, s) in ck.properties_of(a) {
            let name = ck.store().str(n).to_string();
            if !props.iter().any(|(x, _)| *x == name) {
                props.push((name, s));
            }
        }
    }
    props
}

fn object_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    anchor: NodeId,
    offset: u32,
) -> Option<Vec<(String, SymId)>> {
    let c = ck.c;
    let obj = object_container(c, file, anchor, offset)?;
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    let mut at = obj;
    let mut ct = ck.contextual_type(Loc::new(file, obj));
    // `f(() => ({ | }))`: a parenthesized literal takes its context from outside the parens.
    for _ in 0..32 {
        if ct.is_some() {
            break;
        }
        let Some(p) = parents.parent(at) else { break };
        if !matches!(ast.node(p), Node::Paren { .. }) {
            break;
        }
        at = p;
        ct = ck.contextual_type(Loc::new(file, p));
    }
    // A literal in a generic call also offers what the declared parameter allows beyond
    // what was inferred from it (`generic_context.rs`).
    let completions = super::generic_context::completions_type(ck, Loc::new(file, obj))
        .filter(|&t| Some(t) != ct);
    if ct.is_none() && completions.is_none() {
        return None;
    }
    let mut props = ct.map(|t| object_props(ck, t)).unwrap_or_default();
    if let Some(t) = completions {
        for (n, s) in object_props(ck, t) {
            match props.iter_mut().find(|(m, _)| *m == n) {
                Some(e) => e.1 = s,
                None => props.push((n, s)),
            }
        }
    }
    if props.is_empty() {
        return None;
    }
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    // Names other members already write (not the one being typed, not spreads).
    let Node::ObjectLit { props: list } = *ast.node(obj) else {
        return None;
    };
    let mut written: HashSet<String> = HashSet::new();
    for &p in ast.list(list) {
        let sp = ast.span(p);
        if sp.start <= offset && offset <= sp.end {
            continue;
        }
        if matches!(ast.node(p), Node::Spread { .. }) {
            continue;
        }
        if let Some(k) = ts_r_binder::decl_name_node(ast, p)
            && let Some(name) = ts_r_checker::nodes::key_text(ast, k)
        {
            written.insert(name);
        }
    }
    let mut out = Vec::new();
    let mut foreign = false;
    for (name, s) in props {
        // A member only this literal declares (`f<T>(x: T)` called with `{ abc| }`) is the
        // literal's own, not one to offer.
        let decls = c.decls(s);
        if !decls.is_empty()
            && decls
                .iter()
                .all(|d| d.file == file && parents.parent(d.node) == Some(obj))
        {
            continue;
        }
        foreign = true;
        if name.starts_with("__@") || name.starts_with('#') || written.contains(&name) {
            continue;
        }
        let label = if is_identifier_name(&name) {
            name
        } else {
            serde_json::to_string(&name).unwrap_or(name)
        };
        out.push((label, s));
    }
    // A context with only the literal's own members (inferred from it) offers the scope.
    if !foreign {
        return None;
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// Property-name completions in an object binding pattern (`const { | } = v`): the
/// properties of the destructured type it can reach, minus the names already bound. Always a
/// list (empty when the type is unknown): a pattern only binds properties.
/// (Rule from `tsc: tsc/internal/ls/completions.go`, the object-like completion symbols.)
fn pattern_items(
    ck: &mut Ck<'_, '_>,
    file: FileId,
    anchor: NodeId,
    offset: u32,
) -> Option<Vec<(String, SymId)>> {
    let c = ck.c;
    let ast = c.ast(file);
    let parents = c.file(file).parents();
    let pat = match *ast.node(anchor) {
        Node::ObjectPattern { .. } => anchor,
        Node::Ident { .. } => {
            let p = parents.parent(anchor)?;
            match *ast.node(p) {
                Node::BindingProperty {
                    key, value: None, ..
                } if key == anchor => parents.parent(p)?,
                _ => return None,
            }
        }
        _ => return None,
    };
    if !matches!(ast.node(pat), Node::ObjectPattern { .. }) || ast.span(pat).start >= offset {
        return None;
    }
    let text = c.file(file).text.as_bytes();
    let (ws, _) = word_before(&c.file(file).text, offset);
    let mut d = ws as usize;
    while d > 0 && text[d - 1].is_ascii_whitespace() {
        d -= 1;
    }
    if d == 0 || d as u32 <= ast.span(pat).start || !matches!(text[d - 1], b'{' | b',') {
        return None;
    }
    let Some(t) = pattern_type(ck, Loc::new(file, pat), 0) else {
        return Some(Vec::new());
    };
    let Node::ObjectPattern { props: list } = *ast.node(pat) else {
        return None;
    };
    let mut written: HashSet<String> = HashSet::new();
    for &p in ast.list(list) {
        let sp = ast.span(p);
        if sp.start <= offset && offset <= sp.end {
            continue;
        }
        if let Node::BindingProperty { key, .. } = *ast.node(p)
            && let Some(name) = ts_r_checker::nodes::key_text(ast, key)
        {
            written.insert(name);
        }
    }
    let a = ck.apparent_type(t);
    let mut out = Vec::new();
    for (n, s) in ck.properties_of(a) {
        let name = ck.store().str(n).to_string();
        if name.starts_with("__@") || name.starts_with('#') || written.contains(&name) {
            continue;
        }
        if !accessible_at(c, s, Loc::new(file, pat)) {
            continue;
        }
        out.push((name, s));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// The type an object binding pattern destructures, when it can be known without the
/// pattern's own shape: a declared or initialized variable, a `for-of` variable, a parameter
/// (declared or contextually typed), or a property of an outer pattern.
fn pattern_type(ck: &mut Ck<'_, '_>, pat: Loc, depth: u32) -> Option<TypeId> {
    if depth > 32 {
        return None;
    }
    let c = ck.c;
    let p = c.parent(pat)?;
    let ast = c.ast(p.file);
    match *ast.node(p.node) {
        Node::VarDeclarator {
            name,
            type_ann,
            init,
            ..
        } if name == pat.node => {
            if let Some(ta) = type_ann {
                return Some(ck.type_from_node(Loc::new(p.file, ta)));
            }
            if let Some(i) = init {
                return Some(ck.quietly(|ck| ck.check_expr(Loc::new(p.file, i))));
            }
            let list = c.parent(p)?;
            let stmt = c.parent(list)?;
            match *ast.node(stmt.node) {
                Node::ForOf {
                    left,
                    right,
                    is_await,
                    ..
                } if left == list.node => {
                    let t = ck.quietly(|ck| ck.check_expr(Loc::new(p.file, right)));
                    ck.iterated_type(t, is_await)
                }
                _ => None,
            }
        }
        Node::Param {
            pattern, type_ann, ..
        } if pattern == pat.node => {
            let t = ck.param_type(p);
            (type_ann.is_some() || !ck.store().flags(t).intersects(TypeFlags::ANY)).then_some(t)
        }
        Node::BindingProperty {
            key,
            value: Some(v),
            ..
        } if v == pat.node => {
            let outer = c.parent(p)?;
            let t = pattern_type(ck, outer, depth + 1)?;
            let name = ts_r_checker::nodes::key_text(ast, key)?;
            let a = ck.apparent_type(t);
            let prop = ck
                .properties_of(a)
                .into_iter()
                .find(|(n, _)| ck.store().str(*n) == name)?
                .1;
            Some(ck.type_of_symbol(prop))
        }
        _ => None,
    }
}

/// Whether member `s` may be read at `at`: a `private` or `protected` member only inside the
/// class body that declares it.
fn accessible_at(c: &Checker<'_>, s: SymId, at: Loc) -> bool {
    for d in c.decls(s) {
        let m = c.ast(d.file).node(d.node).modifiers();
        if !m.intersects(Modifiers::PRIVATE | Modifiers::PROTECTED) {
            continue;
        }
        let Some(class) = c.parent(d) else {
            return false;
        };
        if class.file != at.file {
            return false;
        }
        let span = c.ast(class.file).span(class.node);
        let here = c.ast(at.file).span(at.node);
        if !(span.start <= here.start && here.end <= span.end) {
            return false;
        }
    }
    true
}

/// Whether `arguments` is in scope at `l`: inside a function that is not an arrow function
/// (arrows see their parent's), and not reached through a class member initializer.
fn has_arguments(c: &Checker<'_>, l: Loc) -> bool {
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut cur = parents.parent(l.node);
    for _ in 0..100_000 {
        let Some(n) = cur else { return false };
        match ast.node(n) {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => return true,
            Node::PropertyDef { .. }
            | Node::StaticBlock { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => {
                return false;
            }
            _ => {}
        }
        cur = parents.parent(n);
    }
    false
}

/// A JavaScript file that uses `require`, `exports` or `module` without declaring them (and
/// has no `import`/`export`) is a CommonJS module.
fn is_commonjs(c: &Checker<'_>, file: FileId) -> bool {
    let b = c.bound(file);
    let ast = c.ast(file);
    b.commonjs
        && b.refs
            .free
            .iter()
            .any(|r| matches!(b.name_text(ast, r.name), "require" | "exports" | "module"))
}

/// The variable whose initializer holds `l` (`const a = |`): it is not offered there. A
/// function body, arrow body or binding pattern on the way up ends the search, as the
/// variable is usable inside a function it initializes. The declaration must hold `offset`
/// unless a word ends there (after `x[|` at the end of an unclosed `var y = x[` the
/// position is past the declaration, so `y` is offered). A JSX tag being typed
/// (`var y = <|`) is inside the declaration.
fn own_initializer(c: &Checker<'_>, l: Loc, offset: u32, after_word: bool) -> Option<String> {
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut cur = Some(l.node);
    let mut in_tag = false;
    for _ in 0..100_000 {
        let n = cur?;
        match *ast.node(n) {
            Node::Block { .. }
            | Node::Arrow { .. }
            | Node::ObjectPattern { .. }
            | Node::ArrayPattern { .. } => return None,
            Node::JsxOpening { .. } | Node::JsxClosing { .. } => in_tag = true,
            Node::VarDeclarator { name, .. } => {
                if !after_word && !in_tag && offset >= ast.span(n).end {
                    return None;
                }
                return match ast.node(name) {
                    Node::Ident { name } => Some(ast.atom(*name).to_string()),
                    _ => None,
                };
            }
            _ => {}
        }
        cur = parents.parent(n);
    }
    None
}

/// Names visible at `l`: every enclosing scope's locals, innermost first.
fn scope_names(c: &Checker<'_>, l: Loc) -> Vec<(String, SymId)> {
    let b = c.bound(l.file);
    let ast = c.ast(l.file);
    let parents = c.file(l.file).parents();
    let mut scope = None;
    let mut cur = Some(l.node);
    for _ in 0..100_000 {
        let Some(n) = cur else { break };
        if let Some(s) = b.scope_of(n) {
            scope = Some(s);
            break;
        }
        cur = parents.parent(n);
    }
    let mut scope = scope.or_else(|| (!b.scopes.is_empty()).then_some(ts_r_binder::ScopeId(0)));
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let Some(sid) = scope else { break };
        let Some(sc) = b.scopes.get(sid.0 as usize) else {
            break;
        };
        for &(name, sym) in b.table(sc.locals) {
            let text = b.name_text(ast, name);
            if !is_identifier_name(text) {
                continue;
            }
            let bs = b.symbol(sym);
            if bs
                .facts
                .intersects(SymbolFacts::EXPORT_SPECIFIER | SymbolFacts::DETACHED)
            {
                continue;
            }
            out.push((text.to_string(), c.sym(l.file, sym)));
        }
        // A file, namespace or enum also has its exports in scope (not re-exports, not names
        // in `declare global`, which are globals).
        if matches!(
            sc.kind,
            ts_r_binder::ScopeKind::File
                | ts_r_binder::ScopeKind::Module
                | ts_r_binder::ScopeKind::Enum
        ) && let Some(container) = sc.symbol
        {
            let exports = b.symbol(container).exports;
            if exports != b.global_augmentations {
                for &(name, sym) in b.table(exports) {
                    let text = b.name_text(ast, name);
                    if !is_identifier_name(text) || text == "default" {
                        continue;
                    }
                    // `export import a = N.b` is not offered (as tsc 7 does not).
                    let bs = b.symbol(sym);
                    if bs
                        .facts
                        .intersects(SymbolFacts::EXPORT_SPECIFIER | SymbolFacts::DETACHED)
                        || bs.flags.intersects(SymbolFlags::ALIAS)
                    {
                        continue;
                    }
                    out.push((text.to_string(), c.sym(l.file, sym)));
                }
            }
        }
        scope = sc.parent;
    }
    out
}

/// Exports of the project's own modules whose names start with `prefix` (case-insensitive)
/// and are not visible here, with the edit that imports them.
fn auto_imports(
    cx: &Ctx<'_, '_>,
    ck: &Ck<'_, '_>,
    prefix: &str,
    visible: &HashSet<String>,
    want: u8,
    offset: u32,
    uri: &str,
) -> Vec<Value> {
    let c = ck.c;
    let here = c.file(cx.file);
    let lower = prefix.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut style = ImportStyle::of(c, cx.file);
    if needs_js_extension(&cx.program.options, &here.path) {
        style.js_ext = true;
    }
    for (i, f) in c.files.iter().enumerate() {
        let fid = FileId(i as u32);
        if fid == cx.file
            || f.is_lib
            || f.path.contains("/node_modules/")
            || !f.bound.is_module
            || f.is_declaration()
        {
            continue;
        }
        let module_sym = c.sym(fid, f.bound.file_symbol);
        for (name, s) in c.exports_of(module_sym) {
            if name == "default"
                || visible.contains(&name)
                || !name.to_ascii_lowercase().starts_with(&lower)
                || !is_identifier_name(&name)
            {
                continue;
            }
            let target = c.resolve_alias(s).unwrap_or(s);
            if !c.has_meaning(target, want) {
                continue;
            }
            let type_only = !c.has_meaning(target, meaning::VALUE)
                && cx.program.options.verbatim_module_syntax() == Some(true);
            let spec = style.specifier(&here.path, &f.path);
            let edit = style.import_edit(c, cx.file, &spec, &name, type_only);
            out.push(json!({
                "label": name,
                "kind": item_kind(c, s),
                "sortText": SORT_AUTO_IMPORT,
                "detail": format!("Auto import from '{spec}'"),
                "labelDetails": {"description": spec},
                "additionalTextEdits": [edit],
                "data": {"uri": uri, "o": offset, "k": "a", "n": name, "f": f.path},
            }));
            if out.len() >= MAX_AUTO_IMPORTS {
                return out;
            }
        }
    }
    out
}

/// Whether relative imports in `path` must name the output extension: an ES module under
/// `node16`/`nodenext` resolution (TS2835 otherwise).
pub(super) fn needs_js_extension(options: &CompilerOptions, path: &str) -> bool {
    if !matches!(
        options.effective().resolution_algorithm(),
        ModuleResolution::Node16 | ModuleResolution::NodeNext
    ) {
        return false;
    }
    let fs = RealFs::new();
    let r = Resolver::new(&fs, ResolverOptions::from_compiler_options(options));
    r.file_format(path).0 == ModuleFormat::Esm
}

/// How the current file writes imports (quotes, semicolons, extensions).
pub(super) struct ImportStyle {
    quote: char,
    semi: bool,
    pub(super) js_ext: bool,
}

impl ImportStyle {
    pub(super) fn of(c: &Checker<'_>, file: FileId) -> ImportStyle {
        let f = c.file(file);
        let ast = &f.ast;
        let mut st = ImportStyle {
            quote: '"',
            semi: true,
            js_ext: false,
        };
        let mut first = true;
        if let Node::Program { stmts } = *ast.node(ast.root) {
            for &s in ast.list(stmts) {
                let Node::ImportDecl { source, .. } = *ast.node(s) else {
                    continue;
                };
                let src = ast.span(source).text(&f.text);
                if first {
                    st.quote = if src.starts_with('\'') { '\'' } else { '"' };
                    st.semi = ast.span(s).text(&f.text).trim_end().ends_with(';');
                    first = false;
                }
                let inner = src.trim_matches(|ch| ch == '"' || ch == '\'');
                if inner.starts_with('.') && inner.ends_with(".js") {
                    st.js_ext = true;
                }
            }
        }
        st
    }

    /// The quote the file's imports use (`"` when it has none).
    pub(super) fn quote_char(&self) -> char {
        self.quote
    }

    /// Relative specifier from `from` (a file) to `to`.
    fn specifier(&self, from: &str, to: &str) -> String {
        let from_dir: Vec<&str> = from
            .rsplit_once('/')
            .map_or("", |x| x.0)
            .split('/')
            .collect();
        let to_parts: Vec<&str> = to.split('/').collect();
        let mut common = 0;
        while common < from_dir.len()
            && common + 1 < to_parts.len()
            && from_dir[common] == to_parts[common]
        {
            common += 1;
        }
        let mut s = String::new();
        if common == from_dir.len() {
            s.push_str("./");
        } else {
            for _ in common..from_dir.len() {
                s.push_str("../");
            }
        }
        s.push_str(&to_parts[common..].join("/"));
        let stem = ["d.ts", "tsx", "ts", "mts", "cts", "jsx", "js", "mjs", "cjs"]
            .iter()
            .find_map(|e| {
                s.strip_suffix(&format!(".{e}"))
                    .map(|x| (x.to_string(), *e))
            });
        match stem {
            Some((base, ext)) if self.js_ext => {
                let js = match ext {
                    "mts" => "mjs",
                    "cts" => "cjs",
                    _ => "js",
                };
                format!("{base}.{js}")
            }
            Some((base, _)) => base
                .strip_suffix("/index")
                .map(str::to_string)
                .unwrap_or(base),
            None => s,
        }
    }

    /// The edit adding `name` to an import of `spec` (an existing one, or a new line after
    /// the last import).
    fn import_edit(
        &self,
        c: &Checker<'_>,
        file: FileId,
        spec: &str,
        name: &str,
        type_only: bool,
    ) -> Value {
        let f = c.file(file);
        let ast = &f.ast;
        let lines = LspLines::new(&f.text);
        let mut last_import_end = None;
        let mut directives_end = None;
        if let Node::Program { stmts } = *ast.node(ast.root) {
            for &s in ast.list(stmts) {
                match *ast.node(s) {
                    Node::ImportDecl {
                        source,
                        specifiers,
                        type_only: decl_type_only,
                        ..
                    } => {
                        last_import_end = Some(ast.span(s).end);
                        let src = ast.span(source).text(&f.text);
                        let inner = src.trim_matches(|ch| ch == '"' || ch == '\'');
                        if inner != spec || decl_type_only && !type_only {
                            continue;
                        }
                        // Add to `{ ... }` when the import has named specifiers.
                        let named: Vec<NodeId> = ast
                            .list(specifiers)
                            .iter()
                            .copied()
                            .filter(|&x| matches!(ast.node(x), Node::ImportSpecifier { .. }))
                            .collect();
                        if let Some(&last) = named.last() {
                            let at = ast.span(last).end;
                            let t = if type_only && !decl_type_only {
                                format!(", type {name}")
                            } else {
                                format!(", {name}")
                            };
                            return json!({"range": lines.range(&f.text, at, at), "newText": t});
                        }
                    }
                    Node::Directive { .. } => directives_end = Some(ast.span(s).end),
                    _ => {}
                }
            }
        }
        let q = self.quote;
        let semi = if self.semi { ";" } else { "" };
        let kw = if type_only { "import type" } else { "import" };
        let line = format!("{kw} {{ {name} }} from {q}{spec}{q}{semi}");
        match last_import_end.or(directives_end) {
            Some(end) => {
                // After the statement's line.
                let eol = f.text[end as usize..]
                    .find('\n')
                    .map_or(f.text.len(), |i| end as usize + i + 1)
                    as u32;
                let text = if eol as usize == f.text.len() && !f.text.ends_with('\n') {
                    format!("\n{line}")
                } else {
                    format!("{line}\n")
                };
                json!({"range": lines.range(&f.text, eol, eol), "newText": text})
            }
            None => json!({"range": lines.range(&f.text, 0, 0), "newText": format!("{line}\n")}),
        }
    }
}

/// `completionItem/resolve`: the detail line (as hover shows it) and documentation.
pub fn resolve(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, item: &Value) -> Value {
    let c = cx.checker;
    let mut out = item.clone();
    let data = &item["data"];
    let (Some(kind), Some(name), Some(offset)) = (
        data["k"].as_str(),
        data["n"].as_str(),
        data["o"].as_u64().map(|o| o as u32),
    ) else {
        return out;
    };
    let text_len = c.file(cx.file).text.len() as u32;
    let offset = offset.min(text_len);
    let sym = match kind {
        "m" => match context(ck, cx.file, offset).0 {
            Context::Member { obj, qualified } => members(ck, Loc::new(cx.file, obj), qualified)
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|x| x.1),
            _ => None,
        },
        "s" => {
            let anchor = node_at(c.ast(cx.file), offset).unwrap_or(c.ast(cx.file).root);
            scope_names(c, Loc::new(cx.file, anchor))
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|x| x.1)
        }
        "g" => c.global_symbol(name, meaning::ALL),
        "a" => {
            let path = data["f"].as_str().unwrap_or("");
            c.files.iter().position(|f| f.path == path).and_then(|i| {
                let fid = FileId(i as u32);
                let m = c.sym(fid, c.file(fid).bound.file_symbol);
                c.export_of(m, name)
            })
        }
        _ => None,
    };
    let Some(s) = sym else {
        return out;
    };
    let detail = describe(ck, s, None);
    let target = c.resolve_alias(s).unwrap_or(s);
    let docs = docs_of(ck, target);
    let detail = match item["detail"].as_str() {
        Some(auto) if kind == "a" => format!("{auto}\n{detail}"),
        _ => detail,
    };
    out["detail"] = json!(detail);
    if !docs.is_empty() {
        out["documentation"] = json!({"kind": "markdown", "value": docs});
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_specifiers() {
        let st = ImportStyle {
            quote: '"',
            semi: true,
            js_ext: false,
        };
        assert_eq!(st.specifier("/p/src/a.ts", "/p/src/b.ts"), "./b");
        assert_eq!(st.specifier("/p/src/x/a.ts", "/p/src/b.tsx"), "../b");
        assert_eq!(st.specifier("/p/src/a.ts", "/p/src/lib/index.ts"), "./lib");
        let js = ImportStyle { js_ext: true, ..st };
        assert_eq!(js.specifier("/p/src/a.ts", "/p/src/lib/x.ts"), "./lib/x.js");
        assert_eq!(js.specifier("/p/a/b/c.ts", "/p/d/e.mts"), "../../d/e.mjs");
    }
}
