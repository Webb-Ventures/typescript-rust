//! Imported emit helpers under `importHelpers` (TS2354, TS2343).
//!
//! Rules observed from tsc 7.0.2, for module files that are not declaration files:
//! - The program imports `tslib` implicitly from every such file ([`helpers_specifier`]);
//!   `load_program` resolves it before the file's own requests, so a found `tslib` is part
//!   of the program.
//! - Syntax the emit lowers with a helper "requests" it. When the file needs any helper and
//!   `tslib` is neither an ambient `declare module "tslib"` nor resolvable from the file,
//!   TS2354 is reported once, at the first request. A shorthand ambient module satisfies
//!   everything. Otherwise each helper `tslib` does not export is TS2343, once per file, at
//!   its first request (every file reports its own, even with a shared `tslib`). An
//!   exported `__classPrivateFieldGet` / `__classPrivateFieldSet` whose signatures all take
//!   fewer than 4 / 5 parameters (pre-4.3 `tslib`) is TS2807 instead.
//! - Requests, in source order (`at` is where the error goes):
//!   - async functions below ES2017: `__awaiter`; async generators below ES2018: `__await`,
//!     `__asyncGenerator`. At the name (a method's key; an anonymous function expression
//!     takes its variable's or property's name), else the function's start (arrows).
//!   - `for await` below ES2018: `__asyncValues`, at the statement.
//!   - object rest below ES2018: `__rest`, at a binding element's name or at an assignment
//!     pattern's `...`.
//!   - private names below ES2022: `__classPrivateFieldGet` (reads), `__classPrivateFieldSet`
//!     (`=` targets; compound targets request both), at the access; `#x in o`:
//!     `__classPrivateFieldIn`, at the expression.
//!   - decorators, at the first decorator: legacy (`experimentalDecorators`) on classes,
//!     methods with a body, accessors and fields: `__decorate`; parameters of a class method
//!     or constructor with a body: `__decorate`, `__param`. ES decorators (any target) on
//!     classes and class elements: `__esDecorate`, `__runInitializers`.
//!   - `using` / `await using` declarations (any target): `__addDisposableResource`,
//!     `__disposeResources`, at the declaration.
//!   - for a file emitted as CommonJS: `import * as` / `export * as` with `esModuleInterop`:
//!     `__importStar`; a default import: `__importDefault` (at a `default as` specifier, else
//!     the statement); `export *`: `__exportStar`. Type-only declarations count.
//! - Ambient code (`declare`, bodiless functions and methods, type positions) requests
//!   nothing; object spread, rest parameters, class fields, static blocks, `accessor` fields
//!   and `import()` request nothing at ES2015+.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{AssignOp, Ast, BinaryOp, MethodKind, Modifiers, Node, NodeId};
use ts_r_config::{CompilerOptions, ScriptTarget};
use ts_r_diagnostics::FileId;

use crate::checker::{Ck, Loc};
use crate::diag::dg;
use crate::members::SigKind;
use crate::nodes::key_text;
use crate::program::SourceFile;

/// The helpers module a file imports implicitly: `tslib` under `importHelpers` for a module
/// file that is not a declaration file.
pub fn helpers_specifier(f: &SourceFile, options: &CompilerOptions) -> Option<&'static str> {
    (options.import_helpers() == Some(true) && f.bound.is_module && !f.kind.is_declaration())
        .then_some("tslib")
}

/// Options that decide which syntax requests a helper.
struct HelperOpts {
    target: ScriptTarget,
    legacy_decorators: bool,
}

/// Collects helper requests in source order.
struct Requests<'x> {
    ast: &'x Ast,
    o: HelperOpts,
    uses: Vec<(NodeId, &'static str)>,
    /// Nodes that report for a function instead of the function itself (method keys,
    /// variable names of anonymous function expressions).
    fn_at: FxHashMap<NodeId, NodeId>,
    /// Private-name accesses that are `=` targets (false) or compound targets (true).
    set_targets: FxHashMap<NodeId, bool>,
    /// Object literals that are assignment patterns.
    assign_patterns: FxHashSet<NodeId>,
}

impl Requests<'_> {
    fn push(&mut self, at: NodeId, helpers: &[&'static str]) {
        for &h in helpers {
            self.uses.push((at, h));
        }
    }

    fn mark_assign_pattern(&mut self, n: NodeId) {
        let n = crate::nodes::skip_parens(self.ast, n);
        if matches!(
            self.ast.node(n),
            Node::ObjectLit { .. } | Node::ArrayLit { .. }
        ) {
            self.assign_patterns.insert(n);
        }
    }

    fn function(&mut self, id: NodeId, flags: ts_r_ast::FnFlags, name: Option<NodeId>) {
        if !flags.contains(ts_r_ast::FnFlags::ASYNC) {
            return;
        }
        let at = self.fn_at.get(&id).copied().or(name).unwrap_or(id);
        if flags.contains(ts_r_ast::FnFlags::GENERATOR) {
            if self.o.target < ScriptTarget::ES2018 {
                self.push(at, &["__await", "__asyncGenerator"]);
            }
        } else if self.o.target < ScriptTarget::ES2017 {
            self.push(at, &["__awaiter"]);
        }
    }

    fn decorators(&mut self, decorators: ts_r_ast::NodeList, helpers: &[&'static str]) {
        if let Some(&first) = self.ast.list(decorators).first() {
            self.push(first, helpers);
        }
    }

    /// Class element decorators (`members` of a class).
    fn class_members(&mut self, members: ts_r_ast::NodeList) {
        let ast = self.ast;
        for &m in ast.list(members) {
            match *ast.node(m) {
                Node::MethodDef {
                    decorators,
                    value,
                    kind,
                    ..
                } => {
                    let has_body =
                        matches!(*ast.node(value), Node::FunctionExpr { body: Some(_), .. });
                    if !has_body {
                        continue;
                    }
                    if kind != MethodKind::Constructor {
                        let helpers: &[&str] = if self.o.legacy_decorators {
                            &["__decorate"]
                        } else {
                            &["__esDecorate", "__runInitializers"]
                        };
                        self.decorators(decorators, helpers);
                    }
                    if self.o.legacy_decorators
                        && let Node::FunctionExpr { params, .. } = *ast.node(value)
                    {
                        for &p in ast.list(params) {
                            if let Node::Param { decorators, .. } = *ast.node(p) {
                                self.decorators(decorators, &["__decorate", "__param"]);
                            }
                        }
                    }
                }
                Node::PropertyDef {
                    decorators,
                    modifiers,
                    ..
                } if !modifiers.intersects(Modifiers::DECLARE | Modifiers::ABSTRACT) => {
                    let helpers: &[&str] = if self.o.legacy_decorators {
                        &["__decorate"]
                    } else {
                        &["__esDecorate", "__runInitializers"]
                    };
                    self.decorators(decorators, helpers);
                }
                _ => {}
            }
        }
    }
}

impl ts_r_ast::visit::Visitor for Requests<'_> {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> ts_r_ast::visit::Walk {
        use ts_r_ast::visit::Walk;
        let target = self.o.target;
        match *ast.node(id) {
            Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::TypeLiteral { .. }
            | Node::IndexSignature { .. } => return Walk::SkipChildren,
            Node::VarDecl { modifiers, .. }
            | Node::Module { modifiers, .. }
            | Node::Enum { modifiers, .. }
                if modifiers.contains(Modifiers::DECLARE) =>
            {
                return Walk::SkipChildren;
            }
            Node::VarDecl { kind, decls, .. } => {
                if matches!(
                    kind,
                    ts_r_ast::VarKind::Using | ts_r_ast::VarKind::AwaitUsing
                ) {
                    self.push(id, &["__addDisposableResource", "__disposeResources"]);
                }
                for &d in ast.list(decls) {
                    if let Node::VarDeclarator {
                        name,
                        init: Some(init),
                        ..
                    } = *ast.node(d)
                        && matches!(*ast.node(init), Node::FunctionExpr { name: None, .. })
                        && matches!(ast.node(name), Node::Ident { .. })
                    {
                        self.fn_at.insert(init, name);
                    }
                }
            }
            Node::FunctionDecl { body: None, .. } => return Walk::SkipChildren,
            Node::FunctionDecl { modifiers, .. } if modifiers.contains(Modifiers::DECLARE) => {
                return Walk::SkipChildren;
            }
            Node::FunctionDecl { flags, name, .. } => self.function(id, flags, name),
            Node::FunctionExpr { body: None, .. } => return Walk::SkipChildren,
            Node::FunctionExpr { flags, name, .. } => self.function(id, flags, name),
            Node::Arrow { flags, .. } => self.function(id, flags, None),
            Node::MethodDef { key, value, .. } => {
                self.fn_at.insert(value, key);
            }
            Node::Property { key, value, .. } => {
                if matches!(*ast.node(value), Node::FunctionExpr { name: None, .. }) {
                    self.fn_at.insert(value, key);
                }
            }
            Node::ClassDecl { modifiers, .. } if modifiers.contains(Modifiers::DECLARE) => {
                return Walk::SkipChildren;
            }
            Node::ClassDecl {
                decorators,
                members,
                ..
            } => {
                let helpers: &[&str] = if self.o.legacy_decorators {
                    &["__decorate"]
                } else {
                    &["__esDecorate", "__runInitializers"]
                };
                self.decorators(decorators, helpers);
                self.class_members(members);
            }
            Node::ClassExpr {
                decorators,
                members,
                ..
            } => {
                if !self.o.legacy_decorators {
                    self.decorators(decorators, &["__esDecorate", "__runInitializers"]);
                }
                self.class_members(members);
            }
            Node::ForOf { is_await, left, .. } => {
                if is_await && target < ScriptTarget::ES2018 {
                    self.push(id, &["__asyncValues"]);
                }
                self.mark_assign_pattern(left);
            }
            Node::ForIn { left, .. } => self.mark_assign_pattern(left),
            Node::Assign { left, op, .. } => {
                let l = crate::nodes::skip_parens(ast, left);
                if let Node::Member { prop, .. } = *ast.node(l)
                    && matches!(ast.node(prop), Node::PrivateName { .. })
                {
                    self.set_targets.insert(l, op != AssignOp::Assign);
                } else if op == AssignOp::Assign {
                    self.mark_assign_pattern(left);
                }
            }
            Node::Update { arg, .. } => {
                let l = crate::nodes::skip_parens(ast, arg);
                self.set_targets.insert(l, true);
            }
            Node::ObjectPattern { props } if target < ScriptTarget::ES2018 => {
                if let Some(&r) = ast.list(props).last()
                    && let Node::Rest { arg } = *ast.node(r)
                {
                    self.push(arg, &["__rest"]);
                }
            }
            Node::ObjectLit { props } if self.assign_patterns.contains(&id) => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Spread { .. } if target < ScriptTarget::ES2018 => {
                            self.push(p, &["__rest"]);
                        }
                        Node::Property { value, .. } => self.mark_assign_pattern(value),
                        _ => {}
                    }
                }
            }
            Node::ArrayLit { elements } if self.assign_patterns.contains(&id) => {
                for &e in ast.list(elements) {
                    match *ast.node(e) {
                        Node::Spread { arg } => self.mark_assign_pattern(arg),
                        Node::Assign { .. } => {}
                        _ => self.mark_assign_pattern(e),
                    }
                }
            }
            Node::Member { prop, .. }
                if target < ScriptTarget::ES2022
                    && matches!(ast.node(prop), Node::PrivateName { .. }) =>
            {
                match self.set_targets.get(&id) {
                    Some(false) => self.push(id, &["__classPrivateFieldSet"]),
                    Some(true) => {
                        self.push(id, &["__classPrivateFieldGet", "__classPrivateFieldSet"])
                    }
                    None => self.push(id, &["__classPrivateFieldGet"]),
                }
            }
            Node::Binary {
                left,
                op: BinaryOp::In,
                ..
            } if target < ScriptTarget::ES2022
                && matches!(ast.node(left), Node::PrivateName { .. }) =>
            {
                self.push(id, &["__classPrivateFieldIn"]);
            }
            _ => {}
        }
        Walk::Continue
    }
}

impl<'c, 'a> Ck<'c, 'a> {
    /// TS2354 / TS2343 for the helpers the module file `file` needs (module doc).
    pub(crate) fn check_module_helpers(&mut self, file: FileId) {
        let f = self.c.file(file);
        if !self.c.opts.import_helpers || f.kind.is_declaration() || !f.bound.is_module {
            return;
        }
        let ast = self.ast(file);
        let mut req = Requests {
            ast,
            o: HelperOpts {
                target: self.c.opts.target,
                legacy_decorators: self.c.opts.experimental_decorators,
            },
            uses: Vec::new(),
            fn_at: FxHashMap::default(),
            set_targets: FxHashMap::default(),
            assign_patterns: FxHashSet::default(),
        };
        ts_r_ast::visit::walk(ast, ast.root, &mut req);
        let mut uses = req.uses;
        if self.emits_commonjs(file) {
            uses.extend(self.module_helper_uses(file));
        }
        if uses.is_empty() {
            return;
        }
        uses.sort_by_key(|&(n, _)| ast.span(n).start);
        let Some(tslib) = self.helpers_module(file) else {
            return;
        };
        let Some(tslib) = tslib else {
            self.error(
                Loc::new(file, uses[0].0),
                dg::THIS_SYNTAX_REQUIRES_AN_IMPORTED_HELPER_BUT_MODULE_0_CANNOT_BE_FOUND_2354,
                &["tslib"],
            );
            return;
        };
        let mut seen: Vec<&str> = Vec::new();
        for (node, helper) in uses {
            if seen.contains(&helper) {
                continue;
            }
            seen.push(helper);
            let Some(sym) = self.c.export_of(tslib, helper) else {
                self.error(
                    Loc::new(file, node),
                    dg::THIS_SYNTAX_REQUIRES_AN_IMPORTED_HELPER_NAMED_1_WHICH_DOES_NOT_EXIST_IN_0_CONSID_2343,
                    &["tslib", helper],
                );
                continue;
            };
            let needed = match helper {
                "__classPrivateFieldGet" => 4,
                "__classPrivateFieldSet" => 5,
                _ => continue,
            };
            let sym = self.c.resolve_alias(sym).unwrap_or(sym);
            let t = self.type_of_symbol(sym);
            let sigs = self.signatures_of(t, SigKind::Call);
            let fits = sigs.iter().any(|&sig| {
                let (_, max, rest) = self.sig_arity(sig);
                rest || max >= needed
            });
            if !sigs.is_empty() && !fits {
                self.error(
                    Loc::new(file, node),
                    dg::THIS_SYNTAX_REQUIRES_AN_IMPORTED_HELPER_NAMED_1_WITH_2_PARAMETERS_WHICH_IS_NOT_C_2807,
                    &["tslib", helper, &needed.to_string()],
                );
            }
        }
    }

    /// The `tslib` module for `file`: `Some(None)` when it cannot be found, `None` when
    /// nothing is to be reported (a shorthand ambient module, or a resolution outside the
    /// program).
    fn helpers_module(&self, file: FileId) -> Option<Option<ts_r_types::SymId>> {
        use ts_r_binder::program::ModuleTarget;
        if let Some(i) = self.c.link.find_ambient_module("tslib") {
            let s = self.c.module_target_symbol(ModuleTarget::Ambient(i))?;
            return (!self.c.is_shorthand_ambient_module(s)).then_some(Some(s));
        }
        let path = &self.c.file(file).path;
        let Some(Some(r)) = self.c.resolutions.get(&(path.clone(), "tslib".to_owned())) else {
            return Some(None);
        };
        let i = self.c.files.iter().position(|f| f.path == r.resolved)?;
        let s = self
            .c
            .module_target_symbol(ModuleTarget::File(FileId(i as u32)))?;
        Some(Some(s))
    }

    /// The import/export helpers of a file emitted as CommonJS (module doc).
    fn module_helper_uses(&self, file: FileId) -> Vec<(NodeId, &'static str)> {
        let interop = self.c.opts.es_module_interop;
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return Vec::new();
        };
        let is_default = |n: NodeId| key_text(ast, n).as_deref() == Some("default");
        let mut uses: Vec<(NodeId, &'static str)> = Vec::new();
        for &s in ast.list(stmts) {
            match *ast.node(s) {
                Node::ImportDecl { specifiers, .. } if interop => {
                    let specs = ast.list(specifiers);
                    if specs
                        .iter()
                        .any(|&x| matches!(ast.node(x), Node::ImportNamespaceSpecifier { .. }))
                    {
                        uses.push((s, "__importStar"));
                    } else if specs
                        .iter()
                        .any(|&x| matches!(ast.node(x), Node::ImportDefaultSpecifier { .. }))
                    {
                        uses.push((s, "__importDefault"));
                    } else if let Some(&x) = specs.iter().find(|&&x| {
                        matches!(*ast.node(x), Node::ImportSpecifier { imported: Some(i), .. } if is_default(i))
                    }) {
                        uses.push((x, "__importDefault"));
                    }
                }
                Node::ExportAll { exported, .. } => match exported {
                    Some(_) if interop => uses.push((s, "__importStar")),
                    Some(_) => {}
                    None => uses.push((s, "__exportStar")),
                },
                Node::ExportNamed {
                    specifiers,
                    source: Some(_),
                    ..
                } if interop => {
                    if let Some(&x) = ast.list(specifiers).iter().find(|&&x| {
                        matches!(*ast.node(x), Node::ExportSpecifier { local, .. } if is_default(local))
                    }) {
                        uses.push((x, "__importDefault"));
                    }
                }
                _ => {}
            }
        }
        uses
    }
}
