//! Document symbols (the outline) and semantic tokens.

use super::locate::{meaning_at, name_text, names_in};
use super::{Ctx, LspLines};
use serde_json::{Value, json};
use ts_r_ast::{Ast, MethodKind, Modifiers, ModuleKind, Node, NodeId, VarKind};
use ts_r_binder::{SymbolFacts, SymbolFlags};
use ts_r_checker::members::SigKind;
use ts_r_checker::{Ck, Loc};
use ts_r_types::SymId;

// LSP SymbolKind values.
const K_MODULE: u32 = 2;
const K_NAMESPACE: u32 = 3;
const K_CLASS: u32 = 5;
const K_METHOD: u32 = 6;
const K_PROPERTY: u32 = 7;
const K_CONSTRUCTOR: u32 = 9;
const K_ENUM: u32 = 10;
const K_INTERFACE: u32 = 11;
const K_FUNCTION: u32 = 12;
const K_VARIABLE: u32 = 13;
const K_CONSTANT: u32 = 14;
const K_ENUM_MEMBER: u32 = 22;
const K_TYPE_PARAMETER: u32 = 26;

const MAX_DEPTH: u32 = 64;

/// Hierarchical `DocumentSymbol`s of the file.
pub fn document_symbols(cx: &Ctx<'_, '_>, lines: &LspLines) -> Value {
    let f = cx.checker.file(cx.file);
    let ast = &f.ast;
    let b = Outline {
        ast,
        text: &f.text,
        lines,
    };
    let mut out = Vec::new();
    if let Node::Program { stmts } = *ast.node(ast.root) {
        for &s in ast.list(stmts) {
            b.statement(s, &mut out, 0);
        }
    }
    Value::Array(out)
}

struct Outline<'a> {
    ast: &'a Ast,
    text: &'a str,
    lines: &'a LspLines,
}

impl Outline<'_> {
    fn symbol(
        &self,
        name: String,
        kind: u32,
        node: NodeId,
        name_node: NodeId,
        children: Vec<Value>,
    ) -> Value {
        let s = self.ast.span(node);
        let n = self.ast.span(name_node);
        let mut v = json!({
            "name": name,
            "kind": kind,
            "range": self.lines.range(self.text, s.start, s.end),
            "selectionRange": self.lines.range(self.text, n.start, n.end),
        });
        if !children.is_empty() {
            v["children"] = Value::Array(children);
        }
        v
    }

    fn key_name(&self, key: NodeId) -> String {
        match *self.ast.node(key) {
            Node::Ident { name } | Node::PrivateName { name } => self.ast.atom(name).to_string(),
            Node::StringLit { value } => format!("\"{}\"", self.ast.atom(value)),
            _ => self.ast.span(key).text(self.text).to_string(),
        }
    }

    fn statement(&self, s: NodeId, out: &mut Vec<Value>, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        let ast = self.ast;
        match *ast.node(s) {
            Node::FunctionDecl {
                name: Some(n),
                body,
                ..
            } => {
                let mut kids = Vec::new();
                if let Some(b) = body {
                    self.block_functions(b, &mut kids, depth + 1);
                }
                out.push(self.symbol(self.key_name(n), K_FUNCTION, s, n, kids));
            }
            Node::ClassDecl { name, members, .. } | Node::ClassExpr { name, members, .. } => {
                let mut kids = Vec::new();
                for &m in ast.list(members) {
                    self.class_member(m, &mut kids);
                }
                let (label, sel) = match name {
                    Some(n) => (self.key_name(n), n),
                    None => ("default".to_string(), s),
                };
                out.push(self.symbol(label, K_CLASS, s, sel, kids));
            }
            Node::Interface { name, body, .. } => {
                let mut kids = Vec::new();
                for &m in ast.list(body) {
                    self.type_member(m, &mut kids);
                }
                out.push(self.symbol(self.key_name(name), K_INTERFACE, s, name, kids));
            }
            Node::TypeAlias { name, ty, .. } => {
                let mut kids = Vec::new();
                if let Node::TypeLiteral { members } = *ast.node(ty) {
                    for &m in ast.list(members) {
                        self.type_member(m, &mut kids);
                    }
                }
                out.push(self.symbol(self.key_name(name), K_TYPE_PARAMETER, s, name, kids));
            }
            Node::Enum { name, members, .. } => {
                let kids = ast
                    .list(members)
                    .iter()
                    .filter_map(|&m| match *ast.node(m) {
                        Node::EnumMember { name, .. } => Some(self.symbol(
                            self.key_name(name),
                            K_ENUM_MEMBER,
                            m,
                            name,
                            Vec::new(),
                        )),
                        _ => None,
                    })
                    .collect();
                out.push(self.symbol(self.key_name(name), K_ENUM, s, name, kids));
            }
            Node::Module {
                kind, name, body, ..
            } => {
                let mut kids = Vec::new();
                let mut b = body;
                // `namespace A.B {}` nests modules; show the innermost body's statements.
                for _ in 0..MAX_DEPTH {
                    match b.map(|x| ast.node(x)) {
                        Some(Node::Module { body: inner, .. }) => b = *inner,
                        _ => break,
                    }
                }
                if let Some(Node::ModuleBlock { stmts }) = b.map(|x| *ast.node(x)) {
                    for &st in ast.list(stmts) {
                        self.statement(st, &mut kids, depth + 1);
                    }
                }
                let label = match kind {
                    ModuleKind::Global => "global".to_string(),
                    _ => ast.span(name).text(self.text).to_string(),
                };
                let k = if matches!(ast.node(name), Node::StringLit { .. }) {
                    K_MODULE
                } else {
                    K_NAMESPACE
                };
                out.push(self.symbol(label, k, s, name, kids));
            }
            Node::VarDecl { kind, decls, .. } => {
                for &d in ast.list(decls) {
                    let Node::VarDeclarator { name, init, .. } = *ast.node(d) else {
                        continue;
                    };
                    if !matches!(ast.node(name), Node::Ident { .. }) {
                        continue;
                    }
                    let mut kids = Vec::new();
                    let mut k = if kind == VarKind::Const {
                        K_CONSTANT
                    } else {
                        K_VARIABLE
                    };
                    if let Some(i) = init {
                        match *ast.node(i) {
                            Node::Arrow { body, .. }
                            | Node::FunctionExpr {
                                body: Some(body), ..
                            } => {
                                k = K_FUNCTION;
                                self.block_functions(body, &mut kids, depth + 1);
                            }
                            Node::ClassExpr { members, .. } => {
                                k = K_CLASS;
                                for &m in ast.list(members) {
                                    self.class_member(m, &mut kids);
                                }
                            }
                            Node::ObjectLit { props } => {
                                for &p in ast.list(props) {
                                    self.object_member(p, &mut kids, depth + 1);
                                }
                            }
                            _ => {}
                        }
                    }
                    out.push(self.symbol(self.key_name(name), k, d, name, kids));
                }
            }
            Node::ExportDefaultExpr { expr } => match *ast.node(expr) {
                Node::FunctionExpr { .. } | Node::ClassExpr { .. } => {
                    self.statement(expr, out, depth + 1);
                }
                _ => {}
            },
            Node::FunctionExpr { name, body, .. } => {
                let mut kids = Vec::new();
                if let Some(b) = body {
                    self.block_functions(b, &mut kids, depth + 1);
                }
                let (label, sel) = match name {
                    Some(n) => (self.key_name(n), n),
                    None => ("default".to_string(), s),
                };
                out.push(self.symbol(label, K_FUNCTION, s, sel, kids));
            }
            _ => {}
        }
    }

    /// Nested function and class declarations in a body (as tsserver's navigation tree).
    fn block_functions(&self, body: NodeId, out: &mut Vec<Value>, depth: u32) {
        if let Node::Block { stmts } = *self.ast.node(body) {
            for &s in self.ast.list(stmts) {
                if matches!(
                    self.ast.node(s),
                    Node::FunctionDecl { .. }
                        | Node::ClassDecl { .. }
                        | Node::Interface { .. }
                        | Node::Enum { .. }
                        | Node::TypeAlias { .. }
                ) {
                    self.statement(s, out, depth);
                }
            }
        }
    }

    fn class_member(&self, m: NodeId, out: &mut Vec<Value>) {
        match *self.ast.node(m) {
            Node::PropertyDef { key, .. } => {
                out.push(self.symbol(self.key_name(key), K_PROPERTY, m, key, Vec::new()));
            }
            Node::MethodDef { kind, key, .. } => {
                let (label, k) = match kind {
                    MethodKind::Constructor => ("constructor".to_string(), K_CONSTRUCTOR),
                    MethodKind::Get | MethodKind::Set => (self.key_name(key), K_PROPERTY),
                    MethodKind::Method => (self.key_name(key), K_METHOD),
                };
                out.push(self.symbol(label, k, m, key, Vec::new()));
            }
            _ => {}
        }
    }

    fn type_member(&self, m: NodeId, out: &mut Vec<Value>) {
        match *self.ast.node(m) {
            Node::PropertySignature { key, .. } => {
                out.push(self.symbol(self.key_name(key), K_PROPERTY, m, key, Vec::new()));
            }
            Node::MethodSignature { key, .. } => {
                out.push(self.symbol(self.key_name(key), K_METHOD, m, key, Vec::new()));
            }
            _ => {}
        }
    }

    fn object_member(&self, p: NodeId, out: &mut Vec<Value>, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        match *self.ast.node(p) {
            Node::Property { key, value, .. } => {
                let mut kids = Vec::new();
                let k = match *self.ast.node(value) {
                    Node::Arrow { .. } | Node::FunctionExpr { .. } => K_METHOD,
                    Node::ObjectLit { props } => {
                        for &q in self.ast.list(props) {
                            self.object_member(q, &mut kids, depth + 1);
                        }
                        K_PROPERTY
                    }
                    _ => K_PROPERTY,
                };
                out.push(self.symbol(self.key_name(key), k, p, key, kids));
            }
            Node::ShorthandProp { name, .. } => {
                out.push(self.symbol(self.key_name(name), K_PROPERTY, p, name, Vec::new()));
            }
            Node::MethodDef { key, .. } => {
                out.push(self.symbol(self.key_name(key), K_METHOD, p, key, Vec::new()));
            }
            _ => {}
        }
    }
}

/// Token types, in legend order (TypeScript's).
pub const TOKEN_TYPES: [&str; 12] = [
    "class",
    "enum",
    "interface",
    "namespace",
    "typeParameter",
    "type",
    "parameter",
    "variable",
    "enumMember",
    "property",
    "function",
    "method",
];
pub const TOKEN_MODIFIERS: [&str; 6] = [
    "declaration",
    "static",
    "async",
    "readonly",
    "defaultLibrary",
    "local",
];

const M_DECLARATION: u32 = 1;
const M_STATIC: u32 = 2;
const M_READONLY: u32 = 8;
const M_DEFAULT_LIBRARY: u32 = 16;
const M_LOCAL: u32 = 32;

/// Full semantic tokens of the file (identifiers classified by what they resolve to).
pub fn semantic_tokens(cx: &Ctx<'_, '_>, ck: &mut Ck<'_, '_>, lines: &LspLines) -> Value {
    let c = cx.checker;
    let f = c.file(cx.file);
    let ast = &f.ast;
    let text = f.text.as_str();
    let mut data: Vec<u32> = Vec::new();
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    for id in names_in(ast) {
        let l = Loc::new(cx.file, id);
        let m = meaning_at(ck, l);
        let Some(s) = m.primary.or(m.shorthand) else {
            continue;
        };
        let target = c.resolve_alias(s).unwrap_or(s);
        let Some((ty, mut mods)) = classify(ck, target) else {
            continue;
        };
        if m.is_decl && Some(s) == m.primary {
            mods |= M_DECLARATION;
        }
        let span = ast.span(id);
        let Some(name) = name_text(ast, id) else {
            continue;
        };
        let p = lines.pos(text, span.start);
        let len: u32 = name.chars().map(|ch| ch.len_utf16() as u32).sum();
        let (dl, dc) = if p.line == prev_line {
            (0, p.character - prev_col)
        } else {
            (p.line - prev_line, p.character)
        };
        data.extend([dl, dc, len, ty, mods]);
        prev_line = p.line;
        prev_col = p.character;
    }
    json!({ "data": data })
}

/// Token type index and modifiers of a symbol.
fn classify(ck: &mut Ck<'_, '_>, s: SymId) -> Option<(u32, u32)> {
    let c = ck.c;
    let flags = c.sym_flags(s);
    let facts = c.sym_facts(s);
    let mut mods = 0;
    if facts.intersects(SymbolFacts::STATIC) {
        mods |= M_STATIC;
    }
    if facts.intersects(SymbolFacts::READONLY) {
        mods |= M_READONLY;
    }
    let decl = c.first_decl(s);
    if decl.is_some_and(|d| c.file(d.file).is_lib) {
        mods |= M_DEFAULT_LIBRARY;
    }
    let ty = if flags.intersects(SymbolFlags::CLASS) {
        0
    } else if flags.intersects(SymbolFlags::ENUM) {
        1
    } else if flags.intersects(SymbolFlags::INTERFACE) {
        2
    } else if flags.intersects(SymbolFlags::VALUE_MODULE | SymbolFlags::NAMESPACE_MODULE) {
        3
    } else if flags.intersects(SymbolFlags::TYPE_PARAMETER) {
        4
    } else if flags.intersects(SymbolFlags::TYPE_ALIAS) {
        5
    } else if flags.intersects(SymbolFlags::PARAMETER) {
        6
    } else if flags.intersects(SymbolFlags::VARIABLE) {
        if decl.is_some_and(|d| is_const(ck, d)) {
            mods |= M_READONLY;
        }
        if decl.is_some_and(|d| is_local(ck, d)) {
            mods |= M_LOCAL;
        }
        // A variable holding a function is a function (as in TypeScript).
        let t = ck.type_of_symbol(s);
        if !ck.signatures_of(t, SigKind::Call).is_empty() && ck.properties_of(t).is_empty() {
            10
        } else {
            7
        }
    } else if flags.intersects(SymbolFlags::ENUM_MEMBER) {
        8
    } else if flags.intersects(SymbolFlags::METHOD) {
        11
    } else if flags.intersects(SymbolFlags::PROPERTY | SymbolFlags::ACCESSOR) {
        9
    } else if flags.intersects(SymbolFlags::FUNCTION) {
        10
    } else {
        return None;
    };
    Some((ty, mods))
}

fn is_const(ck: &Ck<'_, '_>, d: Loc) -> bool {
    let c = ck.c;
    let mut cur = Some(d);
    for _ in 0..8 {
        let Some(l) = cur else { break };
        if let Node::VarDecl { kind, .. } = *c.node(l) {
            return kind == VarKind::Const;
        }
        cur = c.parent(l);
    }
    false
}

/// Declared inside a function or block (not at the top of a module or namespace).
fn is_local(ck: &Ck<'_, '_>, d: Loc) -> bool {
    let c = ck.c;
    let mut cur = c.parent(d);
    for _ in 0..1024 {
        let Some(l) = cur else { return false };
        match c.node(l) {
            Node::Program { .. } | Node::ModuleBlock { .. } => return false,
            Node::Block { .. }
            | Node::Arrow { .. }
            | Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. } => {
                return true;
            }
            n if n.modifiers().contains(Modifiers::EXPORT) => return false,
            _ => cur = c.parent(l),
        }
    }
    false
}
