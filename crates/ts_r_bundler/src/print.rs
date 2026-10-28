//! Printing one module as a fragment of a chunk, following its [`ModulePlan`]: rewrite
//! import references and `require()`/`import()` calls, rename symbols, drop dead and
//! module-syntax statements, wrap if needed, optionally compress, then print with a source
//! map relative to the fragment.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{AssignOp, Ast, MetaPropKind, Modifiers, Node, NodeId, NodeKind, Span, VarKind};
use ts_r_codegen::{CodegenOptions, Layout, PrintHints, SourceMapOptions};
use ts_r_minify::fold::binding_idents;
use ts_r_minify::rename::{self, RenameMode, RenameOptions};
use ts_r_minify::{ScopeId, Scopes, SymbolId, SymbolKind, build};

use crate::expr::Expr;
use crate::link::Wrap;
use crate::plan::{ModulePlan, PrologueStmt, Rewrite};
use crate::prepare::Prepared;

/// A printed module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fragment {
    pub code: String,
    /// Source map of the fragment (one source: the module).
    pub map: Option<FragmentMap>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FragmentMap {
    pub mappings: String,
    pub names: Vec<String>,
}

impl Fragment {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = crate::meta::Writer::default();
        w.str(&self.code);
        match &self.map {
            Some(m) => {
                w.u8(1);
                w.str(&m.mappings);
                w.u32(m.names.len() as u32);
                for n in &m.names {
                    w.str(n);
                }
            }
            None => w.u8(0),
        }
        w.0
    }

    pub fn decode(data: &[u8]) -> Option<Fragment> {
        let mut r = crate::meta::Reader { data, pos: 0 };
        let code = r.str()?;
        let map = match r.u8()? {
            1 => {
                let mappings = r.str()?;
                let mut names = Vec::new();
                for _ in 0..r.count()? {
                    names.push(r.str()?);
                }
                Some(FragmentMap { mappings, names })
            }
            _ => None,
        };
        (r.pos == data.len()).then_some(Fragment { code, map })
    }
}

/// Prints a module. `record_nodes` are the call nodes of its records (from its meta). Runs
/// on a worker thread (deep trees need its large stack).
pub fn print_module(prep: &Prepared, plan: &ModulePlan, record_nodes: &[u32]) -> Fragment {
    let mut ast = prep.ast.clone();
    // The chunk prints the entry's hashbang once, at the top.
    if ast
        .comments
        .first()
        .is_some_and(|c| c.kind == ts_r_ast::CommentKind::Hashbang)
    {
        ast.comments.remove(0);
    }
    let scopes = prep.scopes.clone();
    let src: &str = &prep.src;
    // 0. `--mangle-props`, on the module as written (before imports become other names).
    if !plan.props.is_empty() {
        let mangles = |n: &str| {
            plan.props
                .binary_search_by(|(k, _)| k.as_str().cmp(n))
                .is_ok()
        };
        ts_r_minify::props::apply(
            &mut ast,
            src,
            Some(&scopes),
            &ts_r_minify::props::PropOptions {
                mangles: &mangles,
                quoted: plan.props_quoted,
                skip_namespace_members: true,
            },
            &plan.props,
        );
    }
    let count = ast.nodes.len() as u32;

    let rewrites: FxHashMap<u32, &Rewrite> = plan.rewrites.iter().map(|(s, r)| (*s, r)).collect();
    let ns_rewrites: FxHashMap<(u32, &str), &Rewrite> = plan
        .ns_rewrites
        .iter()
        .map(|(s, p, r)| ((*s, p.as_str()), r))
        .collect();
    let ns_syms: FxHashSet<u32> = plan.ns_rewrites.iter().map(|(s, _, _)| *s).collect();

    // Callees and template tags: a member expression there would change `this`.
    let mut callees: FxHashSet<u32> = FxHashSet::default();
    for n in &ast.nodes {
        match *n {
            Node::Call { callee, .. } => {
                callees.insert(callee.raw());
            }
            Node::TaggedTemplate { tag, .. } => {
                callees.insert(tag.raw());
            }
            _ => {}
        }
    }
    let sym_of = |ast: &Ast, id: NodeId| -> Option<u32> {
        let _ = ast;
        scopes.symbol_of(id).map(|s| s.0)
    };

    // 1. Shorthand properties of rewritten imports: `{ a }` -> `{ a: import_x.a }`.
    if !rewrites.is_empty() {
        for raw in 1..count {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            if let Node::ShorthandProp {
                modifiers,
                name,
                init: None,
            } = *ast.node(id)
                && sym_of(&ast, name).is_some_and(|s| rewrites.contains_key(&s))
            {
                let n = *ast.node(name);
                let key = ast.add(n, ast.span(name));
                *ast.node_mut(id) = Node::Property {
                    modifiers,
                    key,
                    value: name,
                };
            }
        }
    }
    // 2. Import references that become member expressions or `undefined`.
    if !rewrites.is_empty() {
        for raw in 1..count {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            if !matches!(ast.node(id), Node::Ident { .. }) {
                continue;
            }
            let Some(r) = sym_of(&ast, id).and_then(|s| rewrites.get(&s)) else {
                continue;
            };
            let at = ast.span(id).start;
            let new = build_rewrite(&mut ast, r, at, callees.contains(&raw));
            build::replace(&mut ast, id, new);
        }
    }
    // 3. `ns.prop` reads.
    if !ns_rewrites.is_empty() {
        for raw in 1..count {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            let Node::Member { obj, prop, .. } = *ast.node(id) else {
                continue;
            };
            let Some(s) = sym_of(&ast, obj) else { continue };
            if !ns_syms.contains(&s) || !matches!(ast.node(obj), Node::Ident { .. }) {
                continue;
            }
            let Node::Ident { name } = *ast.node(prop) else {
                continue;
            };
            let pname = ast.atom(name).to_owned();
            let Some(r) = ns_rewrites.get(&(s, pname.as_str())) else {
                continue;
            };
            let at = ast.span(id).start;
            let new = build_rewrite(&mut ast, r, at, callees.contains(&raw));
            build::replace(&mut ast, id, new);
        }
    }
    // 4. `require()` / `import()`.
    for (i, e) in plan.records.iter().enumerate() {
        let (Some(e), Some(rec)) = (e, record_nodes.get(i).and_then(|&n| NodeId::from_raw(n)))
        else {
            continue;
        };
        let at = ast.span(rec).start;
        let mut new = e.build(&mut ast, at);
        if plan.globs.contains(&(i as u32)) {
            // The glob's lookup function, called with the path the code computes.
            let arg = match *ast.node(rec) {
                Node::Call { args, .. } => ast.list(args).first().copied(),
                Node::ImportCall { arg, .. } => Some(arg),
                _ => None,
            };
            let Some(arg) = arg else { continue };
            new = build::call(&mut ast, new, &[arg], at);
        }
        build::replace(&mut ast, rec, new);
    }
    // 5. `import.meta` outside ES modules or below its target: the module's empty object
    // (also as a JSX factory, which the transform builds as `import.meta`).
    if !plan.import_meta.is_empty() {
        for raw in 1..count {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            if is_import_meta(&ast, id) {
                let at = ast.span(id).start;
                let x = build::ident(&mut ast, &plan.import_meta, at);
                build::replace(&mut ast, id, x);
            }
        }
    }
    // 6. Names. When mangling a CommonJS wrapper, its `exports` and `module` parameters are
    // symbols of the module scope too, so they get short names like any other local
    // (`var x = e(function(t, o) { o.exports = ... })`).
    let mut scopes = scopes;
    let param_atoms = [ast.intern("exports"), ast.intern("module")];
    // `var exports` / `function module() {}` at the top redeclare the parameter (same
    // binding); any other top-level declaration of those names keeps the old printing.
    let redeclared = |a| scopes.lookup_in(ScopeId::MODULE, a);
    let cjs_params: Option<[SymbolId; 2]> = (plan.mangle
        && plan.wrap == Wrap::Cjs
        && !scopes.has_direct_eval
        && !scopes.has_with
        && param_atoms.iter().all(|&a| {
            redeclared(a).is_none_or(|s| {
                matches!(
                    scopes.symbol(s).kind,
                    SymbolKind::Var | SymbolKind::Function
                )
            })
        }))
    .then(|| {
        param_atoms.map(|a| {
            let sym = scopes
                .lookup_in(ScopeId::MODULE, a)
                .unwrap_or_else(|| scopes.add_synthetic(ScopeId::MODULE, a));
            scopes.adopt_global(a, sym);
            sym
        })
    });
    let fixed: FxHashMap<SymbolId, String> = plan
        .names
        .iter()
        .map(|(s, n)| (SymbolId(*s), n.clone()))
        .collect();
    let reserved: FxHashSet<&str> = plan.reserved.iter().map(String::as_str).collect();
    let top_reserved: FxHashSet<&str> = plan.top_reserved.iter().map(String::as_str).collect();
    let renames = rename::compute(
        &ast,
        &scopes,
        &RenameOptions {
            mode: if plan.mangle {
                RenameMode::Mangle
            } else {
                RenameMode::Deconflict
            },
            fixed: &fixed,
            include_module_scope: plan.wrap == Wrap::Cjs || plan.own_top_level,
            reserved: &|n| reserved.contains(n) || n == "exports" || n == "module",
            module_reserved: &|n| top_reserved.contains(n),
        },
    );
    rename::apply(&mut ast, &scopes, &renames);
    // `--keep-names`: renamed functions and classes get `__tsrName(f, "f")`; the calls
    // after program-level declarations are placed in step 7.
    let mut kept_after = if plan.keep_names.is_empty() {
        FxHashMap::default()
    } else {
        ts_r_minify::keep_names::apply(
            &mut ast,
            &scopes,
            &plan.keep_names,
            ts_r_minify::keep_names::KeepNamesOptions {
                program: false,
                top_classes: true,
            },
        )
        .after
    };
    if plan.mangle {
        ts_r_minify::private::mangle(&mut ast, scopes.has_direct_eval);
    }

    // 7. Statements.
    let stmts = build::program_stmts(&ast);
    let top_level_this = plan.wrap == Wrap::Cjs
        && (crate::scan::has_top_level_this(&ast, &stmts)
            || crate::scan::has_top_level_arguments(&ast, &scopes));
    let mut erased: Vec<Span> = prep.erased.clone();
    let mut directives = Vec::new();
    let mut body = Vec::new();
    for (i, &s) in stmts.iter().enumerate() {
        if !plan.live.get(i).copied().unwrap_or(false) {
            erased.push(ast.span(s));
            continue;
        }
        let kept = kept_after.remove(&s);
        match *ast.node(s) {
            Node::Directive { .. } => {
                if plan.wrap == Wrap::Cjs {
                    directives.push(s);
                } else {
                    erased.push(ast.span(s));
                }
            }
            Node::ImportDecl { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::Empty {} => {
                erased.push(ast.span(s));
            }
            Node::ExportDefaultExpr { expr } => {
                let at = ast.span(s).start;
                let name = build::ident(&mut ast, &plan.default_name, at);
                let decl = build::var_decl(&mut ast, VarKind::Var, name, Some(expr), at);
                body.push(decl);
            }
            Node::FunctionDecl { .. } | Node::ClassDecl { .. } => {
                let at = ast.span(s).start;
                let default_ident = build::ident(&mut ast, &plan.default_name, at);
                match ast.node_mut(s) {
                    Node::FunctionDecl {
                        modifiers, name, ..
                    }
                    | Node::ClassDecl {
                        modifiers, name, ..
                    } => {
                        modifiers.remove(Modifiers::EXPORT);
                        modifiers.remove(Modifiers::DEFAULT);
                        if name.is_none() {
                            *name = Some(default_ident);
                        }
                    }
                    _ => {}
                }
                body.push(class_to_var(&mut ast, &scopes, s, plan.mangle));
            }
            Node::VarDecl { .. } => {
                // Top-level `let` and `const` become `var` when bundling, as in esbuild: the
                // bundle moves code around, and top-level TDZ checks are slow.
                if let Node::VarDecl {
                    modifiers, kind, ..
                } = ast.node_mut(s)
                {
                    modifiers.remove(Modifiers::EXPORT);
                    if matches!(kind, VarKind::Let | VarKind::Const) {
                        *kind = VarKind::Var;
                    }
                }
                body.push(s);
            }
            _ => body.push(s),
        }
        body.extend(kept.into_iter().flatten());
    }
    let mut prologue = Vec::new();
    for p in &plan.prologue {
        prologue.push(match p {
            PrologueStmt::Expr(e) => {
                let x = e.build(&mut ast, 0);
                build::expr_stmt(&mut ast, x, 0)
            }
            PrologueStmt::Var(n, e) => {
                let x = e.build(&mut ast, 0);
                let id = build::ident(&mut ast, n, 0);
                build::var_decl(&mut ast, VarKind::Var, id, Some(x), 0)
            }
        });
    }
    let program: Vec<NodeId> = match plan.wrap {
        Wrap::None => prologue.into_iter().chain(body).collect(),
        Wrap::Cjs => {
            // Unused trailing parameters are dropped (`arguments` is unaffected).
            let params: Vec<String> = match cjs_params {
                Some(syms) => {
                    let used = syms.map(|s| scopes.symbol(s).refs > 0);
                    let keep = if used[1] { 2 } else { usize::from(used[0]) };
                    syms[..keep]
                        .iter()
                        .map(|s| {
                            renames
                                .get(s)
                                .cloned()
                                .unwrap_or_else(|| ast.atom(scopes.symbol(*s).name).to_owned())
                        })
                        .collect()
                }
                None => vec!["exports".into(), "module".into()],
            };
            let params: Vec<NodeId> = params
                .iter()
                .map(|n| {
                    let id = build::ident(&mut ast, n, 0);
                    build::param(&mut ast, id, 0)
                })
                .collect();
            let inner: Vec<NodeId> = directives.into_iter().chain(prologue).chain(body).collect();
            // An arrow is shorter; it needs a body without its own `this` or `arguments`.
            let f = if plan.minify_syntax && !top_level_this {
                build::arrow(&mut ast, &params, &inner, false, 0)
            } else {
                build::function_expr(&mut ast, &params, &inner, 0)
            };
            let call = Expr::ident(&plan.helper_cjs).build(&mut ast, 0);
            let call = build::call(&mut ast, call, &[f], 0);
            let name = build::ident(&mut ast, &plan.wrapper_name, 0);
            vec![build::var_decl(&mut ast, VarKind::Var, name, Some(call), 0)]
        }
        Wrap::Esm => {
            let all: Vec<NodeId> = prologue.into_iter().chain(body).collect();
            let (mut outside, inside) = hoist(&mut ast, &all);
            let f = build::arrow(&mut ast, &[], &inside, plan.wrap_async, 0);
            let call = Expr::ident(&plan.helper_esm).build(&mut ast, 0);
            let call = build::call(&mut ast, call, &[f], 0);
            let name = build::ident(&mut ast, &plan.wrapper_name, 0);
            outside.push(build::var_decl(&mut ast, VarKind::Var, name, Some(call), 0));
            outside
        }
    };
    build::set_program(&mut ast, &program);

    // 8. Compression.
    if plan.minify_syntax {
        let s = Scopes::analyze(&ast, true);
        ts_r_minify::compress::compress(
            &mut ast,
            src,
            &s,
            &ts_r_minify::compress::CompressOptions {
                names: plan.keep_names.is_empty(),
                ..Default::default()
            },
        );
    }

    // 9. Print.
    erased.retain(|s| s.start < s.end);
    erased.sort_by_key(|s| (s.start, s.end));
    let mut merged: Vec<Span> = Vec::with_capacity(erased.len());
    for s in erased {
        match merged.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    let opts = CodegenOptions {
        layout: if plan.compact {
            Layout::Compact
        } else {
            Layout::Readable
        },
        comments: plan.comments,
        source_map: (!plan.map_source.is_empty()).then(|| SourceMapOptions {
            file: None,
            source: plan.map_source.clone(),
            source_root: None,
            include_content: false,
        }),
        ..CodegenOptions::default()
    };
    let out = ts_r_codegen::print_with(
        &ast,
        src,
        &opts,
        &PrintHints {
            erased: &merged,
            prelude: "",
            ..Default::default()
        },
    );
    let mut code = out.code;
    // Compact fragments share the chunk's line (the printer ends them with `;` or `}`, or a
    // newline after a line comment).
    if !plan.compact && !code.is_empty() && !code.ends_with('\n') {
        code.push('\n');
    }
    Fragment {
        code,
        map: out.map.map(|m| FragmentMap {
            mappings: m.mappings,
            names: m.names,
        }),
    }
}

fn build_rewrite(ast: &mut Ast, r: &Rewrite, at: u32, callee: bool) -> NodeId {
    match r {
        Rewrite::Name(n) => build::ident(ast, n, at),
        Rewrite::Undefined => build::undefined(ast, at),
        Rewrite::Member(o, p) => {
            let obj = build::ident(ast, o, at);
            let m = build::prop_access(ast, obj, p, at);
            if callee {
                let zero = build::number(ast, 0.0, at);
                let s = build::seq(ast, &[zero, m], at);
                build::paren(ast, s, at)
            } else {
                m
            }
        }
    }
}

/// A top-level class declaration as `var C = class {}` when bundling, as esbuild prints it.
/// A class whose body refers to its own name keeps an inner name for those references,
/// `_C` (or, when mangling, the same short name): `var C = class _C { static c = new _C() }`.
/// Decorated classes stay declarations.
fn class_to_var(ast: &mut Ast, scopes: &Scopes, s: NodeId, mangle: bool) -> NodeId {
    let Node::ClassDecl {
        decorators,
        modifiers,
        name: Some(name),
        type_params,
        heritage,
        members,
    } = *ast.node(s)
    else {
        return s;
    };
    if !ast.list(decorators).is_empty() || modifiers.contains(Modifiers::DECLARE) {
        return s;
    }
    let at = ast.span(s).start;
    // References to the class inside it, and every name used there.
    let sym = scopes.symbol_of(name);
    let mut inner_refs = Vec::new();
    // `{ C }` shorthands of those references, expanded before the references are renamed.
    let mut shorthands = Vec::new();
    let mut used: FxHashSet<ts_r_ast::Atom> = FxHashSet::default();
    let mut stack: Vec<NodeId> = Vec::new();
    for &h in ast.list(heritage) {
        stack.push(h);
    }
    for &m in ast.list(members) {
        stack.push(m);
    }
    while let Some(id) = stack.pop() {
        match *ast.node(id) {
            Node::Ident { name: a } => {
                used.insert(a);
                if sym.is_some() && scopes.symbol_of(id) == sym {
                    inner_refs.push(id);
                }
            }
            Node::ShorthandProp { name: n, .. } if sym.is_some() && scopes.symbol_of(n) == sym => {
                shorthands.push(id);
            }
            _ => {}
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    let class_name = if inner_refs.is_empty() {
        None
    } else if mangle {
        let (n, sp) = (*ast.node(name), ast.span(name));
        Some(ast.add(n, sp))
    } else {
        let own = ast.name_of(name).unwrap_or_default().to_owned();
        let base = format!("_{own}");
        let mut candidate = base.clone();
        let mut i = 2;
        while used.contains(&ast.intern(&candidate)) {
            candidate = format!("{base}{i}");
            i += 1;
        }
        let a = ast.intern(&candidate);
        for &p in &shorthands {
            let Node::ShorthandProp {
                modifiers,
                name: n,
                init,
            } = *ast.node(p)
            else {
                continue;
            };
            let key = ast.add(*ast.node(n), ast.span(n));
            let value = match init {
                Some(init) => {
                    let at = ast.span(n).start;
                    ast.add(
                        Node::Assign {
                            left: n,
                            op: AssignOp::Assign,
                            right: init,
                        },
                        Span::new(at, at),
                    )
                }
                None => n,
            };
            *ast.node_mut(p) = Node::Property {
                modifiers,
                key,
                value,
            };
        }
        for &r in &inner_refs {
            *ast.node_mut(r) = Node::Ident { name: a };
        }
        let name_at = ast.span(name).start;
        Some(build::ident(ast, &candidate, name_at))
    };
    let class = ast.add(
        Node::ClassExpr {
            decorators,
            modifiers: Modifiers::NONE,
            name: class_name,
            type_params,
            heritage,
            members,
        },
        ast.span(s),
    );
    build::var_decl(ast, VarKind::Var, name, Some(class), at)
}

/// Splits a lazily initialized ES module's statements into declarations hoisted outside
/// the `init` closure (`var` names, functions) and the code that runs inside it.
fn hoist(ast: &mut Ast, stmts: &[NodeId]) -> (Vec<NodeId>, Vec<NodeId>) {
    let mut vars: Vec<(NodeId, Option<NodeId>)> = Vec::new();
    let mut fns = Vec::new();
    let mut inside = Vec::new();
    for &s in stmts {
        match *ast.node(s) {
            Node::FunctionDecl { .. } => fns.push(s),
            Node::VarDecl { kind, decls, .. }
                if !matches!(kind, VarKind::Using | VarKind::AwaitUsing) =>
            {
                let mut assigns = Vec::new();
                for &d in ast.list(decls).to_vec().iter() {
                    let Node::VarDeclarator { name, init, .. } = *ast.node(d) else {
                        continue;
                    };
                    let mut ids = Vec::new();
                    binding_idents(ast, name, &mut ids);
                    for id in ids {
                        let n = *ast.node(id);
                        let copy = ast.add(n, ast.span(id));
                        vars.push((copy, None));
                    }
                    if let Some(init) = init {
                        let at = ast.span(d).start;
                        let a = ast.add(
                            Node::Assign {
                                left: name,
                                op: AssignOp::Assign,
                                right: init,
                            },
                            Span::new(at, at),
                        );
                        let a = if ast.kind(name) == NodeKind::ObjectPattern {
                            build::paren(ast, a, at)
                        } else {
                            a
                        };
                        assigns.push(a);
                    }
                }
                if !assigns.is_empty() {
                    let at = ast.span(s).start;
                    let e = if assigns.len() == 1 {
                        assigns[0]
                    } else {
                        build::seq(ast, &assigns, at)
                    };
                    inside.push(build::expr_stmt(ast, e, at));
                }
            }
            Node::ClassDecl {
                decorators,
                name: Some(name),
                type_params,
                heritage,
                members,
                ..
            } => {
                let at = ast.span(s).start;
                let n = *ast.node(name);
                let outer = ast.add(n, ast.span(name));
                vars.push((outer, None));
                let left = ast.add(n, ast.span(name));
                let class = ast.add(
                    Node::ClassExpr {
                        decorators,
                        modifiers: Modifiers::NONE,
                        name: Some(name),
                        type_params,
                        heritage,
                        members,
                    },
                    ast.span(s),
                );
                let a = build::assign(ast, left, class, at);
                inside.push(build::expr_stmt(ast, a, at));
            }
            _ => inside.push(s),
        }
    }
    let mut outside = Vec::new();
    if !vars.is_empty() {
        outside.push(build::var_decl_many(ast, VarKind::Var, &vars, 0));
    }
    outside.extend(fns);
    (outside, inside)
}

/// `import.meta`, or the `import.meta` member a JSX factory is built as (no identifier is
/// named `import`).
pub(crate) fn is_import_meta(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::MetaProperty {
            kind: MetaPropKind::ImportMeta,
        } => true,
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            matches!(*ast.node(obj), Node::Ident { name } if ast.atom(name) == "import")
                && matches!(*ast.node(prop), Node::Ident { name } if ast.atom(name) == "meta")
        }
        _ => false,
    }
}

/// Non-bundle output with `import-meta` unsupported: every `import.meta` becomes a module-level
/// empty object, `const import_meta = {};` (`var` when `const` is unsupported too), declared
/// after the directives, as esbuild does.
pub(crate) fn empty_import_meta(ast: &mut Ast, kind: VarKind) {
    let count = ast.nodes.len() as u32;
    let uses: Vec<NodeId> = (1..count)
        .filter_map(NodeId::from_raw)
        .filter(|&id| is_import_meta(ast, id))
        .collect();
    if uses.is_empty() {
        return;
    }
    let taken = |n: &str| {
        ast.nodes
            .iter()
            .any(|x| matches!(*x, Node::Ident { name } if ast.atom(name) == n))
    };
    let mut name = "import_meta".to_owned();
    let mut i = 2;
    while taken(&name) {
        name = format!("import_meta{i}");
        i += 1;
    }
    for id in uses {
        let at = ast.span(id).start;
        let x = build::ident(ast, &name, at);
        build::replace(ast, id, x);
    }
    let o = build::object(ast, &[], 0);
    let id = build::ident(ast, &name, 0);
    let decl = build::var_decl(ast, kind, id, Some(o), 0);
    let mut stmts = build::program_stmts(ast);
    let at = stmts
        .iter()
        .take_while(|&&s| ast.kind(s) == NodeKind::Directive)
        .count();
    stmts.insert(at, decl);
    build::set_program(ast, &stmts);
}
