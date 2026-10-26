//! `publicOnly` for the `jsdoc/*` rules: whether a node is exported, decided the way
//! eslint-plugin-jsdoc's export parser does, on the typescript-estree view of the file.
//!
//! What the parser does, in our words:
//! - Cheap ancestor tests first (ESM only): a node inside an `export` declaration (functions
//!   only when the export is at most two levels up) whose `export` has no JSDoc block, or a
//!   class member / interface member whose chain of class, interface or type parents reaches
//!   an `export`, and which has no block itself. Private and protected members are never
//!   exported.
//! - Otherwise a tiny symbol table is built from the top-level statements: every declared
//!   name gets a symbol; functions, classes, enums, interfaces and type aliases carry their
//!   node; object literals and class bodies hold one symbol per key; assignments to
//!   `module.exports`, `exports.x` (and with `window`, `window.x` and top-level `var`s) are
//!   recorded through member chains. `export` statements mark symbols exported.
//! - A node is exported when it is reachable from `module.exports` (CommonJS), `window`, or
//!   an exported symbol's node. From a node, the search only follows `body` (and every
//!   field of a `body` that is a node), looking for a `MethodDefinition` whose value is the
//!   node; from a symbol, it follows the symbol's keys.

use crate::esquery::{Tree, Val};
use crate::estree::EsTree;

/// The `publicOnly` options, after defaults.
#[derive(Debug, Clone, Copy)]
pub struct ExportOpts {
    pub ancestors_only: bool,
    pub esm: bool,
    pub cjs: bool,
    pub window: bool,
}

/// Recursion and work limits; deeper or larger input counts as "not exported".
const MAX_DEPTH: u32 = 400;
const MAX_STEPS: usize = 200_000;

type SymId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Object,
    Literal,
}

#[derive(Debug, Clone)]
struct Sym {
    kind: Kind,
    /// Keys in insertion order; `None` is a key set to `null`.
    props: Vec<(String, Option<SymId>)>,
    /// The ESTree node an object symbol stands for.
    value: Option<u32>,
    /// A literal's value as a property key, when truthy.
    key: Option<String>,
    exported: bool,
    special: bool,
}

impl Sym {
    fn new(kind: Kind) -> Sym {
        Sym {
            kind,
            props: Vec::new(),
            value: None,
            key: None,
            exported: false,
            special: false,
        }
    }
}

struct Parser<'t, 'a> {
    t: &'t EsTree<'a>,
    syms: Vec<Sym>,
    globals: SymId,
    opts: ExportOpts,
    steps: usize,
}

fn name_of(t: &EsTree<'_>, n: u32) -> Option<String> {
    match t.attr(n, &["name"]) {
        Val::Str(s) => Some(s.into_owned()),
        _ => None,
    }
}

/// A key name as JavaScript would use it: `undefined` when the key has no `name`.
fn key_name(t: &EsTree<'_>, n: Option<u32>) -> String {
    n.and_then(|k| name_of(t, k))
        .unwrap_or_else(|| "undefined".to_owned())
}

impl Parser<'_, '_> {
    fn add(&mut self, s: Sym) -> SymId {
        self.syms.push(s);
        self.syms.len() - 1
    }

    fn get(&self, b: SymId, name: &str) -> Option<SymId> {
        self.syms[b]
            .props
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|&(_, v)| v)
    }

    fn set(&mut self, b: SymId, name: &str, v: Option<SymId>) {
        let props = &mut self.syms[b].props;
        match props.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = v,
            None => props.push((name.to_owned(), v)),
        }
    }

    fn tick(&mut self) -> bool {
        self.steps += 1;
        self.steps < MAX_STEPS
    }

    fn ty(&self, n: u32) -> &str {
        self.t.ty(n)
    }

    fn block_symbol(&mut self, block: SymId, name: &str, v: Option<SymId>, global: bool) {
        self.set(block, name, v);
        if global
            && let Some(w) = self.get(self.globals, "window")
            && self.syms[w].special
        {
            self.set(w, name, v);
        }
    }

    fn get_symbol(
        &mut self,
        n: u32,
        scope: Option<SymId>,
        simple: bool,
        depth: u32,
    ) -> Option<SymId> {
        if depth > MAX_DEPTH || !self.tick() {
            return None;
        }
        match self.ty(n) {
            "ArrowFunctionExpression"
            | "ClassDeclaration"
            | "FunctionDeclaration"
            | "FunctionExpression"
            | "TSEnumDeclaration"
            | "TSInterfaceDeclaration"
            | "TSTypeAliasDeclaration" => {
                let proto = self.add(Sym::new(Kind::Object));
                let mut s = Sym::new(Kind::Object);
                s.value = Some(n);
                s.props.push(("prototype".to_owned(), Some(proto)));
                Some(self.add(s))
            }
            "AssignmentExpression" => {
                let left = self.t.node_field(n, "left");
                let right = self.t.node_field(n, "right");
                self.create_symbol(left, right, scope, true, depth + 1)
            }
            "ClassBody" => {
                let mut s = Sym::new(Kind::Object);
                for m in self
                    .t
                    .list_field(n, "body")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    if !self.t.has_field(m, "key") {
                        continue;
                    }
                    let name = key_name(self.t, self.t.node_field(m, "key"));
                    let mut p = Sym::new(Kind::Object);
                    p.value = self.t.node_field(m, "value");
                    let id = self.add(p);
                    match s.props.iter_mut().find(|(k, _)| *k == name) {
                        Some(slot) => slot.1 = Some(id),
                        None => s.props.push((name, Some(id))),
                    }
                }
                s.value = self.t.parent(n);
                Some(self.add(s))
            }
            "ClassExpression" => {
                let body = self.t.node_field(n, "body")?;
                self.get_symbol(body, scope, simple, depth + 1)
            }
            "Identifier" => {
                let name = name_of(self.t, n)?;
                if simple {
                    let mut s = Sym::new(Kind::Literal);
                    s.key = (!name.is_empty()).then_some(name);
                    return Some(self.add(s));
                }
                let block = scope.unwrap_or(self.globals);
                self.get(block, &name)
                    .or_else(|| self.get(self.globals, &name))
            }
            "Literal" => {
                let mut s = Sym::new(Kind::Literal);
                s.value = Some(n);
                s.key = match self.t.attr(n, &["value"]) {
                    Val::Str(v) if !v.is_empty() => Some(v.into_owned()),
                    Val::Num(v) if v != 0.0 && !v.is_nan() => Some(js_number(v)),
                    Val::Bool(true) => Some("true".to_owned()),
                    _ => None,
                };
                Some(self.add(s))
            }
            "MemberExpression" => {
                let obj = self.t.node_field(n, "object")?;
                let obj = self.get_symbol(obj, scope, simple, depth + 1);
                let prop = self.t.node_field(n, "property")?;
                let computed = matches!(self.t.attr(n, &["computed"]), Val::Bool(true));
                let ps = self.get_symbol(prop, scope, !computed, depth + 1);
                let key = ps.and_then(|p| self.literal_key(p))?;
                self.get(obj?, &key)
            }
            "ObjectExpression" => {
                let mut s = Sym::new(Kind::Object);
                for p in self
                    .t
                    .list_field(n, "properties")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    if self.ty(p) == "SpreadElement" {
                        continue;
                    }
                    let Some(v) = self.t.node_field(p, "value") else {
                        continue;
                    };
                    if let Some(pv) = self.get_symbol(v, scope, simple, depth + 1) {
                        let name = key_name(self.t, self.t.node_field(p, "key"));
                        match s.props.iter_mut().find(|(k, _)| *k == name) {
                            Some(slot) => slot.1 = Some(pv),
                            None => s.props.push((name, Some(pv))),
                        }
                    }
                }
                Some(self.add(s))
            }
            _ => None,
        }
    }

    fn literal_key(&self, s: SymId) -> Option<String> {
        let s = &self.syms[s];
        (s.kind == Kind::Literal).then(|| s.key.clone()).flatten()
    }

    fn create_symbol(
        &mut self,
        n: Option<u32>,
        value: Option<u32>,
        scope: Option<SymId>,
        global: bool,
        depth: u32,
    ) -> Option<SymId> {
        let n = n?;
        if depth > MAX_DEPTH || !self.tick() {
            return None;
        }
        let block = scope.unwrap_or(self.globals);
        match self.ty(n) {
            "ClassDeclaration"
            | "FunctionDeclaration"
            | "TSEnumDeclaration"
            | "TSInterfaceDeclaration"
            | "TSTypeAliasDeclaration" => {
                let id = self.t.node_field(n, "id")?;
                if self.ty(id) != "Identifier" {
                    return None;
                }
                let g = self.globals;
                self.create_symbol(Some(id), Some(n), Some(g), false, depth + 1)
            }
            "Identifier" => {
                let name = name_of(self.t, n)?;
                match value {
                    Some(v) => {
                        let vs = self.get_symbol(v, Some(block), false, depth + 1)?;
                        self.block_symbol(block, &name, Some(vs), global);
                        self.get(block, &name)
                    }
                    None => {
                        let fresh = self.add(Sym::new(Kind::Plain));
                        self.block_symbol(block, &name, Some(fresh), global);
                        self.get(block, &name)
                    }
                }
            }
            "MemberExpression" => {
                let obj = self.t.node_field(n, "object")?;
                let sym = self.get_symbol(obj, Some(block), false, depth + 1);
                let prop = self.t.node_field(n, "property")?;
                let computed = matches!(self.t.attr(n, &["computed"]), Val::Bool(true));
                let ps = self.get_symbol(prop, Some(block), !computed, depth + 1);
                let key = ps.and_then(|p| self.literal_key(p));
                let (Some(sym), Some(key)) = (sym, key) else {
                    return None;
                };
                let v = match value {
                    Some(v) => self.get_symbol(v, Some(block), false, depth + 1),
                    None => None,
                };
                self.block_symbol(sym, &key, v, global);
                self.get(sym, &key)
            }
            _ => None,
        }
    }

    fn init_variables(&mut self, n: u32, depth: u32) {
        if depth > MAX_DEPTH || !self.tick() {
            return;
        }
        match self.ty(n) {
            "ExportNamedDeclaration" => {
                if let Some(d) = self.t.node_field(n, "declaration") {
                    self.init_variables(d, depth + 1);
                }
            }
            "ExpressionStatement" => {
                if let Some(e) = self.t.node_field(n, "expression") {
                    self.init_variables(e, depth + 1);
                }
            }
            "Program" => {
                for c in self
                    .t
                    .list_field(n, "body")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    self.init_variables(c, depth + 1);
                }
            }
            "VariableDeclaration" => {
                let var = matches!(self.t.attr(n, &["kind"]), Val::Str(ref k) if k == "var");
                for d in self
                    .t
                    .list_field(n, "declarations")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    let id = self.t.node_field(d, "id");
                    let g = self.globals;
                    let sym = self.create_symbol(id, None, Some(g), false, depth + 1);
                    if self.opts.window
                        && var
                        && let Some(w) = self.get(self.globals, "window")
                    {
                        let name = key_name(self.t, id);
                        self.set(w, &name, sym);
                    }
                }
            }
            _ => {}
        }
    }

    fn mark(&mut self, s: Option<SymId>) {
        if let Some(s) = s {
            self.syms[s].exported = true;
        }
    }

    fn map_variables(&mut self, n: u32, is_export: bool, depth: u32) -> bool {
        if depth > MAX_DEPTH || !self.tick() {
            return false;
        }
        let g = self.globals;
        match self.ty(n) {
            "AssignmentExpression" => {
                let left = self.t.node_field(n, "left");
                let right = self.t.node_field(n, "right");
                self.create_symbol(left, right, None, false, depth + 1);
            }
            "ClassDeclaration" => {
                let id = self.t.node_field(n, "id");
                let body = self.t.node_field(n, "body");
                self.create_symbol(id, body, Some(g), false, depth + 1);
            }
            "ExportDefaultDeclaration" => {
                let d = self.t.node_field(n, "declaration");
                let s = self.create_symbol(d, d, None, false, depth + 1);
                self.mark(s);
            }
            "ExportNamedDeclaration" => {
                if let Some(d) = self.t.node_field(n, "declaration") {
                    if self.ty(d) == "VariableDeclaration" {
                        self.map_variables(d, true, depth + 1);
                    } else {
                        let s = self.create_symbol(Some(d), Some(d), None, false, depth + 1);
                        self.mark(s);
                    }
                }
                for s in self
                    .t
                    .list_field(n, "specifiers")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    self.map_variables(s, false, depth + 1);
                }
            }
            "ExportSpecifier" => {
                if let Some(local) = self.t.node_field(n, "local") {
                    let s = self.get_symbol(local, Some(g), false, depth + 1);
                    self.mark(s);
                }
            }
            "ExpressionStatement" => {
                if let Some(e) = self.t.node_field(n, "expression") {
                    self.map_variables(e, false, depth + 1);
                }
            }
            "FunctionDeclaration" | "TSTypeAliasDeclaration" => {
                if let Some(id) = self.t.node_field(n, "id")
                    && self.ty(id) == "Identifier"
                {
                    self.create_symbol(Some(id), Some(n), Some(g), true, depth + 1);
                }
            }
            "Program" => {
                if self.opts.ancestors_only {
                    return false;
                }
                for c in self
                    .t
                    .list_field(n, "body")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    self.map_variables(c, false, depth + 1);
                }
            }
            "VariableDeclaration" => {
                let var = matches!(self.t.attr(n, &["kind"]), Val::Str(ref k) if k == "var");
                let global = self.opts.window && var && self.get(g, "window").is_some();
                for d in self
                    .t
                    .list_field(n, "declarations")
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                {
                    let id = self.t.node_field(d, "id");
                    let init = self.t.node_field(d, "init");
                    let s = self.create_symbol(id, init, Some(g), global, depth + 1);
                    if is_export {
                        self.mark(s);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn parse_recursive(&mut self, n: u32, depth: u32) -> bool {
        if depth > MAX_DEPTH * 4 {
            return false;
        }
        if let Some(p) = self.t.parent(n)
            && self.parse_recursive(p, depth + 1)
        {
            return true;
        }
        self.map_variables(n, false, 0)
    }

    /// `findNode` from a symbol: the symbol stands for `node`, or one of its keys leads there.
    fn find_in_sym(&mut self, node: u32, s: SymId, path: &mut Vec<SymId>) -> bool {
        if path.contains(&s) || path.len() as u32 > MAX_DEPTH || !self.tick() {
            return false;
        }
        let sym = &self.syms[s];
        if sym.kind == Kind::Object && sym.value == Some(node) {
            return true;
        }
        let kids: Vec<SymId> = sym.props.iter().filter_map(|&(_, v)| v).collect();
        path.push(s);
        let found = kids.into_iter().any(|k| self.find_in_sym(node, k, path));
        path.pop();
        found
    }

    /// `findNode` from an ESTree node: through `body` (and a `body` node's fields) to a
    /// `MethodDefinition` whose value is `node`.
    fn find_in_ast(&mut self, node: u32, x: u32, depth: u32) -> bool {
        if depth > MAX_DEPTH || !self.tick() {
            return false;
        }
        if self.ty(x) == "MethodDefinition" && self.t.node_field(x, "value") == Some(node) {
            return true;
        }
        if let Some(items) = self.t.list_field(x, "body") {
            return items
                .into_iter()
                .flatten()
                .any(|e| self.find_in_ast(node, e, depth + 1));
        }
        let Some(p) = self.t.node_field(x, "body") else {
            return false;
        };
        let kids = self.t.children(p).to_vec();
        kids.into_iter()
            .any(|c| self.find_in_ast(node, c, depth + 1))
    }

    fn exported(&mut self, node: u32) -> bool {
        let g = self.globals;
        if self.opts.cjs
            && let Some(m) = self.get(g, "module")
            && let Some(e) = self.get(m, "exports")
            && self.find_in_sym(node, e, &mut Vec::new())
        {
            return true;
        }
        if self.opts.window
            && let Some(w) = self.get(g, "window")
            && self.find_in_sym(node, w, &mut Vec::new())
        {
            return true;
        }
        if self.opts.esm {
            let vals: Vec<SymId> = self.syms[g].props.iter().filter_map(|&(_, v)| v).collect();
            for v in vals {
                let s = &self.syms[v];
                if !s.exported {
                    continue;
                }
                let Some(value) = s.value else { continue };
                if value == node || self.find_in_ast(node, value, 0) {
                    return true;
                }
            }
        }
        false
    }
}

/// JavaScript's `String(number)` for the common cases (integers and short decimals).
fn js_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e21 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn is_private(t: &EsTree<'_>, n: u32) -> bool {
    let member = matches!(t.ty(n), "MethodDefinition" | "PropertyDefinition")
        && matches!(t.attr(n, &["accessibility"]), Val::Str(ref a) if a != "public");
    member
        || t.node_field(n, "key")
            .is_some_and(|k| t.ty(k) == "PrivateIdentifier")
}

fn is_export_type(ty: &str) -> bool {
    matches!(ty, "ExportDefaultDeclaration" | "ExportNamedDeclaration")
}

fn export_ancestor(t: &EsTree<'_>, n: u32) -> Option<u32> {
    let ignorable = matches!(
        t.ty(n),
        "ArrowFunctionExpression" | "FunctionDeclaration" | "FunctionExpression"
    );
    let mut cur = Some(n);
    let mut idx = 0;
    while let Some(c) = cur {
        if idx >= 2 && ignorable {
            break;
        }
        if is_export_type(t.ty(c)) {
            return Some(c);
        }
        cur = t.parent(c);
        idx += 1;
        if idx > 10_000 {
            break;
        }
    }
    None
}

fn export_by_ancestor(t: &EsTree<'_>, n: u32) -> bool {
    if !matches!(
        t.ty(n),
        "ClassProperty"
            | "Method"
            | "PropertyDefinition"
            | "TSMethodSignature"
            | "TSPropertySignature"
    ) {
        return false;
    }
    let mut cur = t.parent(n);
    let mut guard = 0;
    while let Some(c) = cur {
        guard += 1;
        if guard > 10_000 {
            return false;
        }
        let ty = t.ty(c);
        if is_export_type(ty) {
            return true;
        }
        if !matches!(
            ty,
            "ClassBody"
                | "ClassDeclaration"
                | "ClassDefinition"
                | "ClassExpression"
                | "Program"
                | "TSInterfaceBody"
                | "TSInterfaceDeclaration"
                | "TSTypeAliasDeclaration"
                | "TSTypeLiteral"
                | "TSTypeParameterInstantiation"
                | "TSTypeReference"
        ) {
            return false;
        }
        cur = t.parent(c);
    }
    false
}

/// `isUncommentedExport`: `has_block(n)` says whether ESTree node `n` has a JSDoc block
/// right before it.
pub fn is_uncommented_export(
    t: &EsTree<'_>,
    node: u32,
    opts: ExportOpts,
    has_block: impl Fn(u32) -> bool,
) -> bool {
    if opts.esm {
        if is_private(t, node) || t.parent(node).is_some_and(|p| is_private(t, p)) {
            return false;
        }
        if let Some(e) = export_ancestor(t, node)
            && !has_block(e)
        {
            return true;
        }
        if export_by_ancestor(t, node) && !has_block(node) {
            return true;
        }
    }
    let mut p = Parser {
        t,
        syms: Vec::new(),
        globals: 0,
        opts,
        steps: 0,
    };
    p.globals = p.add(Sym::new(Kind::Plain));
    let g = p.globals;
    if opts.cjs {
        let m = p.add(Sym::new(Kind::Plain));
        let e = p.add(Sym::new(Kind::Plain));
        p.set(m, "exports", Some(e));
        p.set(g, "module", Some(m));
        p.set(g, "exports", Some(e));
    }
    if opts.window {
        let mut w = Sym::new(Kind::Plain);
        w.special = true;
        let w = p.add(w);
        p.set(g, "window", Some(w));
    }
    if opts.ancestors_only {
        p.parse_recursive(node, 0);
    } else if let Some(root) = (0..t.len() as u32).find(|&n| t.parent(n).is_none()) {
        p.init_variables(root, 0);
        p.map_variables(root, false, 0);
    }
    p.exported(node)
}
