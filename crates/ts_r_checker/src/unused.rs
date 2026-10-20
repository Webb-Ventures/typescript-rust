//! `noUnusedLocals` and `noUnusedParameters` (TS6133, TS6138, TS6192, TS6196, TS6198,
//! TS6199, TS6205).
//!
//! Rules observed from tsc 7.0.2 (probes in `tests/decls.rs`):
//! - A local is *read* when some identifier resolves to it, except a plain `=` assignment
//!   target (also inside destructuring assignments and `for-in/of` heads), and except
//!   references inside the declaration itself for functions, classes, interfaces, type
//!   aliases, enums and namespaces (`function f() { f(); }` is unused; `const g = () => g()`
//!   is not). Compound assignments and `++` count as reads.
//! - Exported, ambient and script-global declarations are never reported. `using`
//!   declarations, catch parameters and function/class expression names are not either.
//! - Parameters, type parameters, array-pattern elements and `for-in/of` variables whose
//!   name starts with `_` are exempt; other locals are not. Object-pattern siblings of a
//!   rest element are exempt.
//! - JavaScript files are checked too. A JSDoc `@import` is a value import (TS6133, TS6192
//!   from the `@`); a top-level `@typedef`/`@callback` is never reported (exported, as in a
//!   CommonJS module); TS6205 for a `@template` list starts just before the first tag's `@`.
//! - Types, classes and enums use TS6196 ("never used"); values TS6133. Type parameters are
//!   reported under `noUnusedParameters`; all type parameters of a list (more than one) give
//!   one TS6205 at the list. A declaration list (more than one declarator) whose variables
//!   are all unused gives TS6199 at the list; a binding pattern (more than one element) whose
//!   elements are all unused gives TS6198 at the pattern; an import with several names, all
//!   unused, gives TS6192 at the import.
//! - Private members (`private`, `#name`) are read by a property access with their name that
//!   is not a plain assignment target; method and accessor bodies do not read themselves.
//!   Outside the class, a literal element read (`new C()["x"]`) of a `private` member
//!   counts when its object's type resolves to that member.
//!   `private` parameter properties report TS6138.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{AssignOp, Ast, BinaryOp, MethodKind, Modifiers, Node, NodeId, Span, VarKind};
use ts_r_binder::{BoundFile, SymbolFacts, SymbolFlags, SymbolId};
use ts_r_diagnostics::FileId;

use crate::checker::Ck;
use crate::diag::dg;
use crate::diagnostics_generated::DiagnosticInfo;

type Report = (Span, DiagnosticInfo, String);

/// Per-file facts the rules need.
struct Unused<'x, 'a> {
    c: &'x crate::checker::Checker<'a>,
    file: FileId,
    ast: &'x Ast,
    b: &'x BoundFile,
    parents: &'x ts_r_ast::visit::ParentMap,
    read: FxHashSet<SymbolId>,
    /// Script-global symbols (never reported).
    global: FxHashSet<SymbolId>,
    /// Property names accessed in the file: name → (access node, is a read).
    accesses: FxHashMap<String, Vec<(NodeId, bool)>>,
    locals: bool,
    params: bool,
    has_jsx: bool,
    has_fragment: bool,
    /// Names after `@jsx` pragmas.
    pragma_factories: Vec<String>,
    /// Names after `@jsxFrag` pragmas (used by fragments only).
    pragma_fragments: Vec<String>,
    /// Names referenced by JSDoc `{@link X}` tags (count as used).
    jsdoc_names: FxHashSet<String>,
    /// Classes with an element access whose key is not a literal (`this[key]`): their
    /// private members may be read through it.
    dynamic_access: Vec<Span>,
    /// `private` members unread inside their class but named by literal element reads
    /// outside it (`new C()["x"]`): (key, name, member nodes, element accesses). Whether
    /// those reads reach the member needs types, so `check_unused` decides.
    escaped: Vec<(NodeId, String, Vec<NodeId>, Vec<NodeId>)>,
    out: Vec<Report>,
}

fn starts_underscore(s: &str) -> bool {
    s.starts_with('_')
}

impl<'c, 'a> Ck<'c, 'a> {
    /// Reports unused declarations of `file` (when either option is on).
    pub(crate) fn check_unused(&mut self, file: FileId) {
        let locals = self.c.opts.no_unused_locals;
        let params = self.c.opts.no_unused_parameters;
        if !locals && !params {
            return;
        }
        let sf = self.c.file(file);
        // JavaScript files too (tsc 7); JSDoc `@typedef`s are never reported.
        if sf.is_declaration() {
            return;
        }
        let mut u = Unused {
            c: self.c,
            file,
            ast: &sf.ast,
            b: &sf.bound,
            parents: sf.parents(),
            read: FxHashSet::default(),
            global: FxHashSet::default(),
            accesses: FxHashMap::default(),
            locals,
            params,
            has_jsx: false,
            has_fragment: false,
            pragma_fragments: Vec::new(),
            pragma_factories: Vec::new(),
            jsdoc_names: FxHashSet::default(),
            dynamic_access: Vec::new(),
            escaped: Vec::new(),
            out: Vec::new(),
        };
        u.collect();
        u.walk(u.ast.root, false, 0);
        let mut out = std::mem::take(&mut u.out);
        let escaped = std::mem::take(&mut u.escaped);
        let ast = &sf.ast;
        for (key, name, nodes, reads) in escaped {
            let read = reads.iter().any(|&a| {
                let Node::Index { obj, .. } = *ast.node(a) else {
                    return false;
                };
                let t = self.quietly(|ck| ck.check_expr(crate::checker::Loc::new(file, obj)));
                self.property_of_type(t, &name).is_some_and(|p| {
                    self.c
                        .decls(p)
                        .iter()
                        .any(|d| d.file == file && nodes.contains(&d.node))
                })
            });
            if !read {
                out.push((
                    ast.span(key),
                    dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                    name,
                ));
            }
        }
        out.sort_by_key(|r| r.0.start);
        for (span, info, arg) in out {
            self.error_span(file, span, info, &[&arg]);
        }
    }
}

const MAX_DEPTH: u32 = 400;

impl<'x, 'a> Unused<'x, 'a> {
    fn collect(&mut self) {
        let ast = self.ast;
        let b = self.b;
        if !b.is_module
            && let Some(s0) = b.scopes.first()
        {
            for &(_, s) in b.table(s0.locals) {
                self.global.insert(s);
            }
        }
        for &(node, sym) in &b.refs.resolved {
            if self.is_write_only(node) {
                continue;
            }
            let s = b.symbol(sym);
            if s.flags.intersects(
                SymbolFlags::FUNCTION
                    | SymbolFlags::CLASS
                    | SymbolFlags::INTERFACE
                    | SymbolFlags::TYPE_ALIAS,
            ) {
                // Enums and namespaces reading themselves count: tsc's baselines (taken after
                // emit) do not report them.
                let pos = ast.span(node).start;
                let inside = b.decls(sym).iter().any(|&d| {
                    let sp = ast.span(d);
                    sp.start <= pos && pos < sp.end
                });
                if inside {
                    continue;
                }
            }
            self.read.insert(sym);
        }
        // Property accesses by name.
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            match *ast.node(n) {
                Node::Member { prop, .. } => {
                    let name = match *ast.node(prop) {
                        Node::Ident { name } => Some(ast.atom(name).to_owned()),
                        Node::PrivateName { name } => Some(format!("#{}", ast.atom(name))),
                        _ => None,
                    };
                    if let Some(name) = name {
                        let r = !self.is_write_only(n);
                        self.accesses.entry(name).or_default().push((n, r));
                    }
                }
                Node::Index { index, .. } => {
                    if !matches!(
                        ast.node(index),
                        Node::StringLit { .. } | Node::NumberLit { .. }
                    ) {
                        self.dynamic_access.push(ast.span(n));
                    }
                    if let Node::StringLit { value } = *ast.node(index) {
                        let r = !self.is_write_only(n);
                        self.accesses
                            .entry(ast.atom(value).to_owned())
                            .or_default()
                            .push((n, r));
                    }
                }
                Node::BindingProperty { key, .. } => {
                    let name = match *ast.node(key) {
                        Node::Ident { name } | Node::StringLit { value: name } => {
                            Some(ast.atom(name).to_owned())
                        }
                        _ => None,
                    };
                    if let Some(name) = name {
                        self.accesses.entry(name).or_default().push((n, true));
                    }
                }
                Node::Binary {
                    left,
                    op: BinaryOp::In,
                    ..
                } => {
                    if let Node::PrivateName { name } = *ast.node(left) {
                        self.accesses
                            .entry(format!("#{}", ast.atom(name)))
                            .or_default()
                            .push((n, true));
                    }
                }
                // `({ x } = this)` reads property `x` of the source.
                Node::ObjectLit { props } if self.is_write_only(n) => {
                    for &p in ast.list(props) {
                        let key = match *ast.node(p) {
                            Node::ShorthandProp { name, .. } => name,
                            Node::Property { key, .. } => key,
                            _ => continue,
                        };
                        if let Node::Ident { name } | Node::StringLit { value: name } =
                            *ast.node(key)
                        {
                            self.accesses
                                .entry(ast.atom(name).to_owned())
                                .or_default()
                                .push((p, true));
                        }
                    }
                }
                Node::JsxElement { .. } => self.has_jsx = true,
                Node::JsxFragment { .. } => {
                    self.has_jsx = true;
                    self.has_fragment = true;
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        self.scan_comments();
    }

    /// `@jsx`/`@jsxFrag` pragmas and `{@link X}` references in comments.
    fn scan_comments(&mut self) {
        let ast = self.ast;
        let text = self.c.file(self.file).text.as_str();
        let all = Span::new(0, text.len() as u32);
        for c in ast.comments_in(all) {
            let Some(body) = text.get(c.span.start as usize..c.span.end as usize) else {
                continue;
            };
            if !body.contains('@') {
                continue;
            }
            let mut rest = body;
            while let Some(i) = rest.find('@') {
                rest = &rest[i + 1..];
                let tag_end = rest
                    .find(|ch: char| !ch.is_ascii_alphanumeric())
                    .unwrap_or(rest.len());
                let tag = &rest[..tag_end];
                let after = rest[tag_end..].trim_start();
                let name_end = after
                    .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                    .unwrap_or(after.len());
                let name = &after[..name_end];
                if name.is_empty() {
                    continue;
                }
                match tag {
                    t if t.eq_ignore_ascii_case("jsx") => {
                        self.pragma_factories.push(name.to_owned())
                    }
                    t if t.eq_ignore_ascii_case("jsxFrag") => {
                        self.pragma_fragments.push(name.to_owned())
                    }
                    "link" | "linkcode" | "linkplain" | "see" => {
                        self.jsdoc_names.insert(name.to_owned());
                    }
                    _ => {}
                }
            }
        }
    }

    /// `n` (an identifier or property access) is only written: the target of `=`, a
    /// destructuring assignment target or a `for-in/of` head expression.
    fn is_write_only(&self, n: NodeId) -> bool {
        let ast = self.ast;
        let mut cur = n;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 200 {
                return false;
            }
            let Some(p) = self.parents.parent(cur) else {
                return false;
            };
            match *ast.node(p) {
                Node::Paren { .. } => cur = p,
                Node::Assign { left, op, .. } => {
                    return left == cur && op == AssignOp::Assign;
                }
                Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == cur,
                // Inside a literal: a write only if the literal is itself a target.
                Node::ArrayLit { .. } => cur = p,
                Node::Spread { .. } => cur = p,
                Node::ShorthandProp { name, .. } if name == cur => {
                    let Some(o) = self.parents.parent(p) else {
                        return false;
                    };
                    if !matches!(ast.node(o), Node::ObjectLit { .. }) {
                        return false;
                    }
                    cur = o;
                }
                Node::Property { value, .. } if value == cur => {
                    let Some(o) = self.parents.parent(p) else {
                        return false;
                    };
                    if !matches!(ast.node(o), Node::ObjectLit { .. }) {
                        return false;
                    }
                    cur = o;
                }
                _ => return false,
            }
            // Only literals continue upwards.
            if !matches!(
                ast.node(cur),
                Node::Paren { .. }
                    | Node::ArrayLit { .. }
                    | Node::ObjectLit { .. }
                    | Node::Spread { .. }
            ) {
                return false;
            }
        }
    }

    fn sym_of(&self, decl: NodeId) -> Option<SymbolId> {
        self.b.symbol_of_decl(decl)
    }

    /// The symbol a binding identifier declares.
    fn sym_of_ident(&self, ident: NodeId) -> Option<SymbolId> {
        if let Some(s) = self.sym_of(ident) {
            return Some(s);
        }
        let p = self.parents.parent(ident)?;
        match *self.ast.node(p) {
            Node::VarDeclarator { name, .. } if name == ident => self.sym_of(p),
            Node::Param { pattern, .. } if pattern == ident => self.sym_of(p),
            _ => None,
        }
    }

    /// Whether a declared symbol may be reported at all.
    fn reportable(&self, s: SymbolId) -> bool {
        let sym = self.b.symbol(s);
        !sym.facts.intersects(
            SymbolFacts::EXPORTED
                | SymbolFacts::AMBIENT
                | SymbolFacts::EXPORT_SPECIFIER
                | SymbolFacts::DETACHED,
        ) && !self.global.contains(&s)
    }

    /// One declaration program-wide (merged symbols count every file's declarations).
    fn single_decl(&self, s: SymbolId) -> bool {
        self.b.decls(s).len() == 1 && self.c.decls(self.c.sym(self.file, s)).len() == 1
    }

    fn unread(&self, s: SymbolId) -> bool {
        self.reportable(s)
            && !self.read.contains(&s)
            && (self.jsdoc_names.is_empty()
                || !self
                    .jsdoc_names
                    .contains(self.b.name_text(self.ast, self.b.symbol(s).name)))
    }

    fn text(&self, ident: NodeId) -> &'x str {
        match *self.ast.node(ident) {
            Node::Ident { name } => self.ast.atom(name),
            _ => "",
        }
    }

    fn push(&mut self, at: NodeId, info: DiagnosticInfo, arg: &str) {
        let span = self.ast.span(at);
        self.out.push((span, info, arg.to_owned()));
    }

    // -----------------------------------------------------------------------------
    // The walk
    // -----------------------------------------------------------------------------

    fn walk(&mut self, n: NodeId, ambient: bool, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        let ast = self.ast;
        let mut ambient = ambient;
        match *ast.node(n) {
            Node::VarDecl {
                modifiers,
                kind,
                decls,
            } => {
                if modifiers.intersects(Modifiers::DECLARE) {
                    ambient = true;
                }
                if self.locals && !ambient && !matches!(kind, VarKind::Using | VarKind::AwaitUsing)
                {
                    self.var_decl(n, decls);
                }
            }
            Node::FunctionDecl {
                modifiers,
                name,
                type_params,
                params,
                body,
                ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE) {
                    ambient = true;
                }
                if !ambient {
                    if self.locals
                        && !modifiers.intersects(Modifiers::EXPORT | Modifiers::DEFAULT)
                        && let Some(nm) = name
                        && let Some(s) = self.sym_of(n)
                        && self.b.decls(s).len() == 1
                        && self.b.symbol(s).flags == SymbolFlags::FUNCTION
                        && self.unread(s)
                    {
                        self.push(
                            nm,
                            dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                            self.text(nm),
                        );
                    }
                    if let Some(tp) = type_params {
                        self.type_params(tp);
                    }
                    if body.is_some() {
                        self.params(params);
                    }
                }
            }
            Node::FunctionExpr {
                type_params,
                params,
                body,
                ..
            } => {
                if !ambient && body.is_some() {
                    if let Some(tp) = type_params {
                        self.type_params(tp);
                    }
                    self.params(params);
                }
            }
            Node::Arrow {
                type_params,
                params,
                ..
            } => {
                if !ambient {
                    if let Some(tp) = type_params {
                        self.type_params(tp);
                    }
                    self.params(params);
                }
            }
            Node::ClassDecl {
                decorators,
                modifiers,
                name,
                type_params,
                members,
                ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE) {
                    ambient = true;
                }
                if !ambient {
                    let single = self.sym_of(n).is_some_and(|s| self.single_decl(s));
                    if self.locals
                        && decorators.is_empty()
                        && single
                        && !modifiers.intersects(Modifiers::EXPORT | Modifiers::DEFAULT)
                        && let Some(nm) = name
                        && let Some(s) = self.sym_of(n)
                        && self.unread(s)
                    {
                        self.push(nm, dg::_0_IS_DECLARED_BUT_NEVER_USED_6196, self.text(nm));
                    }
                    if single && let Some(tp) = type_params {
                        self.type_params(tp);
                    }
                    if self.locals {
                        self.private_members(n, members);
                    }
                }
            }
            Node::ClassExpr {
                type_params,
                members,
                ..
            } => {
                if !ambient {
                    if let Some(tp) = type_params {
                        self.type_params(tp);
                    }
                    if self.locals {
                        self.private_members(n, members);
                    }
                }
            }
            Node::Interface {
                modifiers,
                name,
                type_params,
                ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE) {
                    ambient = true;
                }
                if !ambient && let Some(s) = self.sym_of(n) {
                    let all_ifaces = self
                        .b
                        .decls(s)
                        .iter()
                        .all(|&d| matches!(ast.node(d), Node::Interface { .. }));
                    if self.locals && all_ifaces && self.unread(s) {
                        self.push(
                            name,
                            dg::_0_IS_DECLARED_BUT_NEVER_USED_6196,
                            self.text(name),
                        );
                    }
                    if self.single_decl(s)
                        && let Some(tp) = type_params
                    {
                        self.type_params(tp);
                    }
                }
            }
            Node::TypeAlias {
                modifiers,
                name,
                type_params,
                ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE) {
                    ambient = true;
                }
                if !ambient && let Some(s) = self.sym_of(n) {
                    // A top-level JSDoc `@typedef`/`@callback` is never reported (tsc 7
                    // treats it as exported, CommonJS modules included).
                    let top_jsdoc = self.ast.is_jsdoc(n)
                        && self.b.scopes.first().is_some_and(|sc| {
                            self.b.lookup(sc.locals, self.b.symbol(s).name) == Some(s)
                        });
                    if self.locals && !top_jsdoc && self.b.decls(s).len() == 1 && self.unread(s) {
                        self.push(
                            name,
                            dg::_0_IS_DECLARED_BUT_NEVER_USED_6196,
                            self.text(name),
                        );
                    }
                    if let Some(tp) = type_params {
                        self.type_params(tp);
                    }
                }
            }
            Node::Enum {
                modifiers, name, ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE) {
                    ambient = true;
                }
                if self.locals
                    && !ambient
                    && let Some(s) = self.sym_of(n)
                    && self.b.decls(s).len() == 1
                    && self.unread(s)
                {
                    self.push(
                        name,
                        dg::_0_IS_DECLARED_BUT_NEVER_USED_6196,
                        self.text(name),
                    );
                }
            }
            Node::Module {
                modifiers,
                name,
                body,
                ..
            } => {
                if modifiers.intersects(Modifiers::DECLARE)
                    || !matches!(ast.node(name), Node::Ident { .. })
                {
                    ambient = true;
                }
                if self.locals
                    && !ambient
                    && body.is_some()
                    && matches!(
                        self.parents.parent(n).map(|p| ast.node(p)),
                        Some(Node::Program { .. } | Node::ModuleBlock { .. } | Node::Block { .. })
                    )
                    && let Some(s) = self.sym_of(n)
                    && self.b.decls(s).len() == 1
                    && self.unread(s)
                {
                    self.push(
                        name,
                        dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                        self.text(name),
                    );
                }
            }
            Node::ImportDecl {
                type_only,
                has_clause,
                specifiers,
                ..
            } => {
                if self.locals && !ambient && has_clause {
                    self.import_decl(n, type_only, specifiers);
                }
            }
            Node::ImportEquals {
                modifiers, name, ..
            } => {
                if self.locals
                    && !ambient
                    && !modifiers.intersects(Modifiers::EXPORT)
                    && let Some(s) = self.sym_of(n)
                    && self.unread(s)
                    && !self.jsx_factory_name(self.text(name))
                    && !starts_underscore(self.text(name))
                    && self.c.resolve_alias(self.c.sym(self.file, s)).is_some()
                {
                    self.push(
                        name,
                        dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                        self.text(name),
                    );
                }
            }
            Node::CallSignature { type_params, .. }
            | Node::ConstructSignature { type_params, .. }
            | Node::MethodSignature { type_params, .. }
            | Node::FunctionType { type_params, .. }
            | Node::ConstructorType { type_params, .. } => {
                if !ambient && let Some(tp) = type_params {
                    self.type_params(tp);
                }
            }
            Node::InferType { type_param } => {
                if self.params
                    && !ambient
                    && let Node::TypeParam { name, .. } = *ast.node(type_param)
                    && !starts_underscore(self.text(name))
                    && let Some(s) = self.sym_of(type_param).or_else(|| self.sym_of_ident(name))
                    && !self.read.contains(&s)
                {
                    self.push(
                        name,
                        dg::_0_IS_DECLARED_BUT_NEVER_USED_6196,
                        self.text(name),
                    );
                }
            }
            _ => {}
        }
        let mut kids = Vec::new();
        ast.for_each_child(n, |c| kids.push(c));
        for c in kids {
            self.walk(c, ambient, depth + 1);
        }
    }

    /// Whether `name` is a JSX factory the file's JSX uses: none under the automatic
    /// runtime (`react-jsx`), the `@jsx` pragma's (else the options') for elements, the
    /// `@jsxFrag` pragma's for fragments only.
    fn jsx_factory_name(&self, name: &str) -> bool {
        if !self.has_jsx {
            return false;
        }
        let text = self.c.file(self.file).text.as_str();
        if crate::jsx_runtime::file_runtime(self.c.opts.jsx, self.ast, text).is_some() {
            return false;
        }
        let elements = if self.pragma_factories.is_empty() {
            self.c.opts.jsx_factory_roots.iter().any(|r| r == name)
        } else {
            self.pragma_factories.iter().any(|r| r == name)
        };
        elements || (self.has_fragment && self.pragma_fragments.iter().any(|r| r == name))
    }

    // -----------------------------------------------------------------------------
    // Variables and binding patterns
    // -----------------------------------------------------------------------------

    fn var_decl(&mut self, n: NodeId, decls: ts_r_ast::NodeList) {
        let ast = self.ast;
        let list: Vec<NodeId> = ast.list(decls).to_vec();
        let for_in_of = self
            .parents
            .parent(n)
            .is_some_and(|p| matches!(ast.node(p), Node::ForIn { .. } | Node::ForOf { .. }));
        let names: Vec<NodeId> = list
            .iter()
            .filter_map(|&d| match *ast.node(d) {
                Node::VarDeclarator { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        if names.len() != list.len() {
            return;
        }
        if names.len() > 1 && names.iter().all(|&nm| self.binding_unused(nm, for_in_of)) {
            self.push(n, dg::ALL_VARIABLES_ARE_UNUSED_6199, "");
            return;
        }
        for nm in names {
            self.report_binding(nm, for_in_of);
        }
    }

    /// Elements of a binding pattern: `(target, is a rest element, a leading `_` exempts
    /// it)`; holes skipped. Array elements and renamed object elements (`a: _x`) may be
    /// exempt; shorthand object elements (`{ _a }`) are not.
    fn pattern_elements(&self, p: NodeId) -> Option<Vec<(NodeId, bool, bool)>> {
        let ast = self.ast;
        let mut out = Vec::new();
        match *ast.node(p) {
            Node::ObjectPattern { props } => {
                for &e in ast.list(props) {
                    match *ast.node(e) {
                        Node::BindingProperty { key, value, .. } => match value {
                            Some(v) => out.push((v, false, true)),
                            None => out.push((key, false, false)),
                        },
                        Node::Rest { arg } => out.push((arg, true, true)),
                        _ => return None,
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                for &e in ast.list(elements) {
                    match *ast.node(e) {
                        Node::Hole {} => {}
                        Node::AssignPattern { left, .. } => out.push((left, false, true)),
                        Node::Rest { arg } => out.push((arg, true, true)),
                        _ => out.push((e, false, true)),
                    }
                }
            }
            _ => return None,
        }
        Some(out)
    }

    /// Everything bound under `t` is unused (and nothing is exempt).
    fn binding_unused(&self, t: NodeId, underscore_ok: bool) -> bool {
        let ast = self.ast;
        match *ast.node(t) {
            Node::Ident { .. } => {
                if underscore_ok && starts_underscore(self.text(t)) {
                    return false;
                }
                self.sym_of_ident(t).is_some_and(|s| self.unread(s))
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => {
                let is_array = matches!(ast.node(t), Node::ArrayPattern { .. });
                let Some(els) = self.pattern_elements(t) else {
                    return false;
                };
                if els.is_empty() {
                    return false;
                }
                // Siblings of an object rest element are exempt.
                if !is_array && els.iter().any(|e| e.1) && els.len() > 1 {
                    return false;
                }
                els.iter().all(|&(e, _, ok)| self.binding_unused(e, ok))
            }
            _ => false,
        }
    }

    fn report_binding(&mut self, t: NodeId, underscore_ok: bool) {
        let ast = self.ast;
        match *ast.node(t) {
            Node::Ident { .. } => {
                if self.binding_unused(t, underscore_ok) {
                    self.push(
                        t,
                        dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                        self.text(t),
                    );
                }
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. } => {
                let is_array = matches!(ast.node(t), Node::ArrayPattern { .. });
                let Some(els) = self.pattern_elements(t) else {
                    return;
                };
                if !is_array && els.iter().any(|e| e.1) {
                    // `{ a, ...rest }`: only the rest element can be reported.
                    for &(e, is_rest, ok) in &els {
                        if is_rest {
                            self.report_binding(e, ok);
                        }
                    }
                    return;
                }
                if els.len() > 1 && self.binding_unused(t, underscore_ok) {
                    self.push(t, dg::ALL_DESTRUCTURED_ELEMENTS_ARE_UNUSED_6198, "");
                    return;
                }
                for &(e, _, ok) in &els {
                    self.report_binding(e, ok);
                }
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------
    // Parameters and type parameters
    // -----------------------------------------------------------------------------

    fn params(&mut self, params: ts_r_ast::NodeList) {
        if !self.params && !self.locals {
            return;
        }
        let ast = self.ast;
        for &p in ast.list(params) {
            let Node::Param {
                modifiers, pattern, ..
            } = *ast.node(p)
            else {
                continue;
            };
            let prop = modifiers.intersects(
                Modifiers::PUBLIC
                    | Modifiers::PRIVATE
                    | Modifiers::PROTECTED
                    | Modifiers::READONLY
                    | Modifiers::OVERRIDE,
            );
            if prop {
                if self.locals && modifiers.intersects(Modifiers::PRIVATE) {
                    self.param_property(p, pattern);
                }
                continue;
            }
            if !self.params {
                continue;
            }
            match *ast.node(pattern) {
                Node::Ident { .. } => {
                    let name = self.text(pattern);
                    if name == "this" || starts_underscore(name) {
                        continue;
                    }
                    if let Some(s) = self.sym_of(p).or_else(|| self.sym_of_ident(pattern))
                        && !self.read.contains(&s)
                    {
                        self.push(
                            pattern,
                            dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                            name,
                        );
                    }
                }
                _ => self.report_binding(pattern, false),
            }
        }
    }

    fn param_property(&mut self, p: NodeId, pattern: NodeId) {
        let name = self.text(pattern);
        if name.is_empty() {
            return;
        }
        // Reads are property accesses (the parameter itself does not count).
        let used = self
            .accesses
            .get(name)
            .is_some_and(|v| v.iter().any(|&(_, r)| r));
        let _ = p;
        if !used {
            self.push(
                pattern,
                dg::PROPERTY_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6138,
                name,
            );
        }
    }

    fn type_params(&mut self, tp: NodeId) {
        if !self.params {
            return;
        }
        let ast = self.ast;
        let Node::TypeParams { params } = *ast.node(tp) else {
            return;
        };
        let list: Vec<NodeId> = ast.list(params).to_vec();
        let mut unused = Vec::new();
        for &p in &list {
            let Node::TypeParam { name, .. } = *ast.node(p) else {
                return;
            };
            let text = self.text(name);
            if starts_underscore(text) {
                continue;
            }
            let Some(s) = self.sym_of(p).or_else(|| self.sym_of_ident(name)) else {
                return;
            };
            if !self.read.contains(&s) {
                unused.push(name);
            }
        }
        if list.len() > 1 && unused.len() == list.len() {
            // JSDoc `@template` lists: from just before the first tag's `@` (tsc 7).
            let span = self.ast.span(tp);
            let text = &self.c.file(self.file).text;
            let at = self
                .ast
                .is_jsdoc(tp)
                .then(|| text.get(..span.start as usize)?.rfind("@template"))
                .flatten()
                .filter(|&i| i > 0);
            match at {
                Some(i) => self.out.push((
                    Span::new(i as u32 - 1, span.end),
                    dg::ALL_TYPE_PARAMETERS_ARE_UNUSED_6205,
                    String::new(),
                )),
                None => self.push(tp, dg::ALL_TYPE_PARAMETERS_ARE_UNUSED_6205, ""),
            }
            return;
        }
        for name in unused {
            self.push(
                name,
                dg::_0_IS_DECLARED_BUT_NEVER_USED_6196,
                self.text(name),
            );
        }
    }

    // -----------------------------------------------------------------------------
    // Imports
    // -----------------------------------------------------------------------------

    fn import_decl(&mut self, n: NodeId, type_only: bool, specifiers: ts_r_ast::NodeList) {
        let ast = self.ast;
        let mut unused = Vec::new();
        let specs: Vec<NodeId> = ast.list(specifiers).to_vec();
        for &sp in &specs {
            let local = match *ast.node(sp) {
                Node::ImportDefaultSpecifier { local }
                | Node::ImportNamespaceSpecifier { local }
                | Node::ImportSpecifier { local, .. } => local,
                _ => return,
            };
            let Some(s) = self.sym_of(sp).or_else(|| self.sym_of_ident(local)) else {
                return;
            };
            if self.jsx_factory_name(self.text(local)) || starts_underscore(self.text(local)) {
                continue;
            }
            // Imports of unresolvable modules are not reported.
            if self.c.resolve_alias(self.c.sym(self.file, s)).is_none() {
                return;
            }
            if self.unread(s) {
                unused.push(local);
            }
        }
        // A JSDoc `@import` is reported from its `@`, and as a value import (tsc 7).
        let jsdoc = self.ast.is_jsdoc(n);
        if specs.len() > 1 && unused.len() == specs.len() {
            let span = self.ast.span(n);
            let span = if jsdoc && span.start > 0 {
                Span::new(span.start - 1, span.end)
            } else {
                span
            };
            self.out.push((
                span,
                dg::ALL_IMPORTS_IN_IMPORT_DECLARATION_ARE_UNUSED_6192,
                String::new(),
            ));
            return;
        }
        for local in unused {
            let info = if type_only && !jsdoc {
                dg::_0_IS_DECLARED_BUT_NEVER_USED_6196
            } else {
                dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133
            };
            self.push(local, info, self.text(local));
        }
    }

    // -----------------------------------------------------------------------------
    // Private class members
    // -----------------------------------------------------------------------------

    fn private_members(&mut self, class: NodeId, members: ts_r_ast::NodeList) {
        let class_span = self.ast.span(class);
        let dynamic = self
            .dynamic_access
            .iter()
            .any(|s| class_span.start <= s.start && s.start < class_span.end);
        let ast = self.ast;
        let list: Vec<NodeId> = ast.list(members).to_vec();
        // name → (first declaration's key, member nodes)
        let mut seen: Vec<(String, NodeId, Vec<NodeId>, bool)> = Vec::new();
        for &m in &list {
            let (modifiers, key, is_method, kind) = match *ast.node(m) {
                Node::PropertyDef { modifiers, key, .. } => (modifiers, key, false, None),
                Node::MethodDef {
                    modifiers,
                    key,
                    kind,
                    ..
                } => (modifiers, key, true, Some(kind)),
                _ => continue,
            };
            if kind == Some(MethodKind::Constructor) {
                continue;
            }
            if modifiers.intersects(Modifiers::DECLARE | Modifiers::ABSTRACT) {
                continue;
            }
            let name = match *ast.node(key) {
                Node::PrivateName { name } => format!("#{}", ast.atom(name)),
                Node::Ident { name } if modifiers.intersects(Modifiers::PRIVATE) && !dynamic => {
                    ast.atom(name).to_owned()
                }
                _ => continue,
            };
            match seen.iter_mut().find(|e| e.0 == name) {
                Some(e) => e.2.push(m),
                None => seen.push((name, key, vec![m], is_method)),
            }
        }
        for (name, key, nodes, _) in seen {
            // A get/set pair or a single member; overloads are not handled.
            let ok = nodes.len() == 1
                || (nodes.len() == 2
                    && nodes.iter().all(|&m| {
                        matches!(
                            ast.node(m),
                            Node::MethodDef {
                                kind: MethodKind::Get | MethodKind::Set,
                                ..
                            }
                        )
                    }));
            if !ok {
                continue;
            }
            let own: Vec<Span> = nodes
                .iter()
                .filter(|&&m| matches!(ast.node(m), Node::MethodDef { .. }))
                .map(|&m| ast.span(m))
                .collect();
            // Writes use accessors (a setter is "used" by an assignment).
            let accessor = nodes.iter().any(|&m| {
                matches!(
                    ast.node(m),
                    Node::MethodDef {
                        kind: MethodKind::Get | MethodKind::Set,
                        ..
                    }
                )
            });
            let used = self.accesses.get(&name).is_some_and(|v| {
                v.iter().any(|&(a, r)| {
                    let pos = ast.span(a).start;
                    (r || accessor)
                        && class_span.start <= pos
                        && pos < class_span.end
                        && !own.iter().any(|s| s.start <= pos && pos < s.end)
                })
            });
            // `private` is not enforced through literal element access, so reads like
            // `new C()["x"]` outside the class can reach the member.
            let escapes: Vec<NodeId> = if used || name.starts_with('#') {
                Vec::new()
            } else {
                self.accesses.get(&name).map_or(Vec::new(), |v| {
                    v.iter()
                        .filter(|&&(a, r)| {
                            let pos = ast.span(a).start;
                            (r || accessor)
                                && !(class_span.start <= pos && pos < class_span.end)
                                && matches!(ast.node(a), Node::Index { .. })
                        })
                        .map(|&(a, _)| a)
                        .collect()
                })
            };
            if !escapes.is_empty() {
                self.escaped.push((key, name, nodes, escapes));
            } else if !used {
                self.push(
                    key,
                    dg::_0_IS_DECLARED_BUT_ITS_VALUE_IS_NEVER_READ_6133,
                    &name,
                );
            }
        }
    }
}
