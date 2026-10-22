//! The per-file walk: statements and declarations, deferred function and class bodies,
//! type annotations (unresolved names, generic arity, constraints), module imports,
//! grammar errors for misplaced modifiers, and `@ts-ignore` / `@ts-expect-error`.

use ts_r_ast::{
    CommentKind, HeritageKind, JsDocSyntaxKind, MethodKind, Modifiers, Node, NodeId, NodeKind, Span,
};
use ts_r_binder::{RequestKind, SymbolFlags, meaning};
use ts_r_diagnostics::{Diagnostic, FileId};
use ts_r_types::{TypeFlags, TypeId, TypeKind};

use crate::checker::{Ck, Loc, Resolving};
use crate::diag::dg;
use crate::modifiers::modifier_text;
use crate::nodes::{fn_parts, ident_text, is_this_param, key_text, skip_parens};

/// Statement nesting limit (deeper statements are not checked).
const MAX_STMT_DEPTH: u32 = 500;

impl<'c, 'a> Ck<'c, 'a> {
    /// Checks one file and returns its semantic diagnostics (unsorted, with comment
    /// directives applied).
    pub fn check_source_file(&mut self, file: FileId) -> Vec<Diagnostic> {
        self.reset_for(file);
        if self.c.file(file).has_ts_nocheck() {
            return Vec::new();
        }
        let ast = self.ast(file);
        let root = ast.root;
        if let Node::Program { stmts } = *ast.node(root) {
            self.check_unreachable_stmts(file, ast.list(stmts));
            for &s in ast.list(stmts) {
                self.check_stmt(Loc::new(file, s), 0);
            }
        }
        self.drain_deferred();
        self.check_type_annotations(file);
        self.check_erasable_syntax(file);
        self.check_modifiers(file);
        self.check_module_requests(file);
        self.check_module_helpers(file);
        self.check_reference_modes(file);
        self.check_esm_export_assignment(file);
        self.check_export_assignment_conflicts(file);
        self.check_export_star_collisions(file);
        self.check_jsdoc_syntax(file);
        self.check_export_local_merges(file);
        self.check_redeclared_exports(file);
        self.check_import_assertions(file);
        self.check_js_type_only_aliases(file);
        self.check_esm_import_assignment(file);
        self.check_commonjs_generated_names(file);
        self.check_downlevel_reserved_names(file);
        self.check_top_level_await_names(file);
        self.check_decls(file);
        // Function and class bodies met by the declaration checks (decorator expressions).
        self.drain_deferred();
        if !self.c.opts.jsx_set {
            self.check_jsx_without_flag(file);
        }
        let mut diags = std::mem::take(&mut self.diags);
        // Bind-time diagnostics share the comment directives.
        diags.extend(
            self.c
                .bound(file)
                .diagnostics
                .iter()
                .cloned()
                .map(|d| d.with_file(file)),
        );
        diags.extend(
            self.c
                .file(file)
                .grammar
                .iter()
                .cloned()
                .map(|d| d.with_file(file)),
        );
        // JSDoc syntax errors (JavaScript) too: tsc 7 reports them with the file's
        // semantic errors, so `// @ts-ignore` hides them (jsDeclarationsMissingTypeParameters).
        diags.extend(
            self.c
                .file(file)
                .jsdoc
                .iter()
                .cloned()
                .map(|d| d.with_file(file)),
        );
        self.apply_comment_directives(file, diags)
    }

    /// TS2308: two `export *` of one module (or namespace) provide the same name for
    /// different entities and the module doesn't export the name itself. Reported at each
    /// later `export *`, naming the specifier of the first one (tsc:
    /// tsc/internal/checker/checker.go getExportsOfModuleWorker).
    fn check_export_star_collisions(&mut self, file: FileId) {
        let stars: Vec<ts_r_binder::ExportStar> = self.c.bound(file).export_stars.clone();
        if stars.len() < 2 {
            return;
        }
        let mut containers: Vec<ts_r_binder::SymbolId> =
            stars.iter().map(|s| s.container).collect();
        containers.sort_unstable();
        containers.dedup();
        for c in containers {
            // name -> (resolved entity, specifier text of the `export *` that brought it)
            let mut seen: rustc_hash::FxHashMap<Box<str>, (Option<ts_r_types::SymId>, String)> =
                rustc_hash::FxHashMap::default();
            let mut dups: Vec<(NodeId, String, Box<str>)> = Vec::new();
            for star in stars.iter().filter(|s| s.container == c) {
                let ast = self.ast(file);
                let Node::ExportAll { source, .. } = *ast.node(star.node) else {
                    continue;
                };
                let spec = self.c.text(Loc::new(file, source)).to_owned();
                let target = self.c.link.module_target(file, star.request);
                for (name, r) in self.c.link.export_names(target) {
                    if &*name == "default" || &*name == "export=" {
                        continue;
                    }
                    let s = self.c.sym_ref(r);
                    let resolved = if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                        self.c.resolve_alias(s)
                    } else {
                        Some(s)
                    };
                    match seen.get(&name) {
                        None => {
                            seen.insert(name, (resolved, spec.clone()));
                        }
                        Some((first, first_spec)) => {
                            if *first != resolved {
                                dups.push((star.node, first_spec.clone(), name));
                            }
                        }
                    }
                }
            }
            if dups.is_empty() {
                continue;
            }
            let b = self.c.bound(file);
            let ast = self.ast(file);
            let own = b.symbol(c).exports;
            dups.retain(|(_, _, n)| b.lookup_str(ast, own, n).is_none());
            for (node, spec, name) in dups {
                self.error(
                    Loc::new(file, node),
                    dg::MODULE_0_HAS_ALREADY_EXPORTED_A_MEMBER_NAMED_1_CONSIDER_EXPLICITLY_RE_EXPORTING_T_2308,
                    &[&spec, &name],
                );
            }
        }
    }

    fn drain_deferred(&mut self) {
        let mut i = 0;
        while i < self.deferred.len() {
            let d = self.deferred[i];
            i += 1;
            let ast = self.ast(d.file);
            match *ast.node(d.node) {
                Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    // An object literal method's `@type` without call signature: TS8030
                    // (tsc 7; not for function expressions and arrows).
                    if self
                        .c
                        .parent(d)
                        .is_some_and(|p| matches!(self.node(p), Node::MethodDef { .. }))
                    {
                        self.check_jsdoc_function_type(d);
                    }
                    self.check_function_like(d)
                }
                Node::ClassExpr { .. } => self.check_class(d),
                // `void x`'s operand (its type never matters, so a read of a variable in
                // its own initializer is not circular).
                Node::Unary {
                    op: ts_r_ast::UnaryOp::Void,
                    arg,
                } => {
                    self.check_expr(Loc::new(d.file, arg));
                }
                _ => {}
            }
        }
        // `deferred_set` keeps them from being queued again.
        self.deferred.clear();
    }

    // ---------------------------------------------------------------------------------
    // Statements
    // ---------------------------------------------------------------------------------

    fn check_stmt(&mut self, l: Loc, depth: u32) {
        if depth > MAX_STMT_DEPTH {
            return;
        }
        let file = l.file;
        let ast = self.ast(file);
        let at = |n: NodeId| Loc::new(file, n);
        self.inst_count = 0;
        let misplaced = self.check_misplaced_module_element(l);
        match *ast.node(l.node) {
            Node::Block { stmts } | Node::ModuleBlock { stmts } => {
                self.check_unreachable_stmts(file, ast.list(stmts));
                for &s in ast.list(stmts) {
                    self.check_stmt(at(s), depth + 1);
                }
            }
            Node::Directive { .. }
            | Node::Empty {}
            | Node::Debugger {}
            | Node::Break { .. }
            | Node::Continue { .. } => {}
            Node::ExprStmt { expr } => {
                self.check_expr(at(expr));
            }
            Node::If { test, cons, alt } => {
                self.check_truthiness_expr(at(test));
                self.check_stmt(at(cons), depth + 1);
                if let Some(a) = alt {
                    self.check_stmt(at(a), depth + 1);
                }
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                if let Some(i) = init {
                    if matches!(ast.node(i), Node::VarDecl { .. }) {
                        self.check_stmt(at(i), depth + 1);
                    } else {
                        self.check_expr(at(i));
                    }
                }
                if let Some(t) = test {
                    self.check_truthiness_expr(at(t));
                }
                if let Some(u) = update {
                    self.check_expr(at(u));
                }
                self.check_stmt(at(body), depth + 1);
            }
            Node::ForIn { left, right, body } if self.is_empty_var_list(at(left)) => {
                // `for (var in X)`: tsc reports TS1123 and checks only the object
                // expression (parserForInStatement2).
                self.check_expr(at(right));
                self.check_stmt(at(body), depth + 1);
            }
            Node::ForOf { left, body, .. } if self.is_empty_var_list(at(left)) => {
                // `for (var of X)` (parserForOfStatement2).
                self.check_stmt(at(body), depth + 1);
            }
            Node::ForIn { left, right, body } => {
                // The object without `null` and `undefined` must be `any`, a non-primitive
                // type variable or assignable to `object`.
                let rt = self.check_expr(at(right));
                let rt = if self.c.opts.strict_null_checks {
                    self.filter_type(rt, |ck, m| !ck.is_nullish_member(m))
                } else {
                    rt
                };
                let f = self.store().flags(rt);
                let ok = f.intersects(
                    TypeFlags::ANY
                        | TypeFlags::NON_PRIMITIVE
                        | TypeFlags::INSTANTIABLE_NON_PRIMITIVE,
                ) || self.store().is_error(rt)
                    || rt == TypeId::UNRESOLVED
                    || !f.contains(TypeFlags::NEVER) && self.is_assignable(rt, TypeId::OBJECT);
                if !ok {
                    let ts = self.type_to_string(rt);
                    self.error(at(right), dg::THE_RIGHT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_MUST_BE_OF_TYPE_ANY_AN_OBJECT_TYPE_OR_A_2407, &[&ts]);
                }
                let before = self.diags.len();
                let member = matches!(
                    ast.node(skip_parens(ast, left)),
                    Node::Member { .. } | Node::Index { .. }
                );
                if member {
                    // `for (o.x in y)` writes `o.x` (TS2540 when read-only).
                    self.assignment_target_type(at(left));
                } else {
                    self.check_for_left(at(left), depth);
                }
                // TS2405 comes first; the reference check (TS2406) runs only when the
                // target's type accepts the keys (tsc: checker.go, checkForInStatement).
                // A read-only member target is an error type, never TS2405.
                // `for (k in o)` where `k` is a constant, function, class, ...: only TS2588
                // and its siblings (the identifier's type is an error type in tsc).
                let assignable_ident =
                    !matches!(ast.node(skip_parens(ast, left)), Node::Ident { .. })
                        || self.check_reference_target(at(left), false);
                let mark = self.diags.len();
                if assignable_ident && (!member || self.diags.len() == before) {
                    self.check_for_in_head(at(left), rt);
                }
                if self.diags.len() == mark {
                    self.check_for_head_target(at(left), false);
                }
                self.check_stmt(at(body), depth + 1);
            }
            Node::ForOf {
                is_await,
                left,
                right,
                body,
            } => {
                if is_await && !self.builds_into_node_commonjs(file) && self.at_top_level(l) {
                    let text = &self.c.file(file).text;
                    let span = self.c.span(l);
                    let rest = text
                        .get(span.start as usize + "for".len()..span.end as usize)
                        .unwrap_or("");
                    if let Some(off) = rest.find("await") {
                        let start = span.start + ("for".len() + off) as u32;
                        self.top_level_await_error(file, start, true);
                    }
                } else if is_await && self.builds_into_node_commonjs(file) && self.at_top_level(l) {
                    // The `await` keyword after `for`.
                    let span = self.c.span(l);
                    let text = &self.c.file(file).text;
                    let rest = text
                        .get(span.start as usize + "for".len()..span.end as usize)
                        .unwrap_or("");
                    if let Some(off) = rest.find("await") {
                        let start = span.start + ("for".len() + off) as u32;
                        self.error_span(
                            file,
                            Span::new(start, start + "await".len() as u32),
                            dg::THE_CURRENT_FILE_IS_A_COMMONJS_MODULE_AND_CANNOT_USE_AWAIT_AT_THE_TOP_LEVEL_1309,
                            &[],
                        );
                    }
                }
                // `for (const x: T of xs)`: TS2483 at the name (one declarator, no
                // initializer: those are the parser's TS1188 and TS1190).
                if let Node::VarDecl { decls, .. } = *ast.node(left)
                    && let &[d] = ast.list(decls)
                    && let Node::VarDeclarator {
                        name,
                        type_ann: Some(_),
                        init: None,
                        ..
                    } = *ast.node(d)
                {
                    self.error(
                        at(name),
                        dg::THE_LEFT_HAND_SIDE_OF_A_FOR_OF_STATEMENT_CANNOT_USE_A_TYPE_ANNOTATION_2483,
                        &[],
                    );
                }
                let rt = self.check_expr(at(right));
                // The iterated value must not be nullish or `unknown` (TS18047, TS18046).
                let rt = self.check_non_null(at(right), rt);
                if !self.check_iterable(rt, at(right), is_await) {
                    self.check_iterator_takes_undefined(
                        rt,
                        at(right),
                        is_await,
                        dg::CANNOT_ITERATE_VALUE_BECAUSE_THE_NEXT_METHOD_OF_ITS_ITERATOR_EXPECTS_TYPE_1_BUT_F_2763,
                    );
                }
                let it = if self.store().flags(rt).intersects(TypeFlags::ANY) {
                    TypeId::UNRESOLVED
                } else {
                    self.quietly(|ck| ck.iterated_type(rt, is_await))
                        .unwrap_or(TypeId::UNRESOLVED)
                };
                self.check_for_of_left(at(left), it, depth);
                self.check_for_head_target(at(left), true);
                self.check_stmt(at(body), depth + 1);
            }
            Node::While { test, body } | Node::DoWhile { body, test } => {
                self.check_truthiness_expr(at(test));
                self.check_stmt(at(body), depth + 1);
            }
            Node::Return { arg } => self.check_return(l, arg),
            Node::Throw { arg } => {
                self.check_expr(at(arg));
            }
            Node::Labeled { body, .. } => self.check_stmt(at(body), depth + 1),
            Node::Switch { disc, cases } => {
                let dt = self.check_expr(at(disc));
                for &c in ast.list(cases) {
                    let Node::SwitchCase { test, body } = *ast.node(c) else {
                        continue;
                    };
                    if let Some(t) = test {
                        let tt = self.check_expr(at(t));
                        self.check_case_comparable(at(t), tt, dt);
                    }
                    self.check_unreachable_stmts(file, ast.list(body));
                    for &s in ast.list(body) {
                        self.check_stmt(at(s), depth + 1);
                    }
                    self.check_case_fallthrough(at(c));
                }
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                self.check_stmt(at(block), depth + 1);
                if let Some(h) = handler
                    && let Node::CatchClause {
                        param,
                        type_ann,
                        body,
                    } = *ast.node(h)
                {
                    if let Some(ta) = type_ann {
                        let t = self.type_from_node(at(ta));
                        if !self.store().flags(t).intersects(TypeFlags::ANY_OR_UNKNOWN)
                            && !self.store().is_error(t)
                        {
                            self.error(at(ta), dg::CATCH_CLAUSE_VARIABLE_TYPE_ANNOTATION_MUST_BE_ANY_OR_UNKNOWN_IF_SPECIFIED_1196, &[]);
                        }
                    }
                    // A destructured catch variable reads its (`unknown` or `any`) value.
                    if let Some(pat) = param
                        && matches!(
                            ast.node(pat),
                            Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                        )
                    {
                        self.check_binding_pattern_iterables(at(h));
                        self.check_binding_pattern_defaults(at(pat));
                    }
                    if let Some(pat) = param {
                        self.check_catch_redeclaration(file, pat, body);
                    }
                    self.check_stmt(at(body), depth + 1);
                }
                if let Some(f) = finalizer {
                    self.check_stmt(at(f), depth + 1);
                }
            }
            // tsc does not check a `with` body (its names are `any`); TS1300 directly in an
            // async function, TS2410 always.
            Node::With { obj, .. } => {
                self.check_expr(at(obj));
                if let Some(func) = self.containing_function(l)
                    && crate::nodes::fn_parts(ast, func.node)
                        .is_some_and(|p| p.flags.contains(ts_r_ast::FnFlags::ASYNC))
                {
                    self.error(
                        l,
                        dg::WITH_STATEMENTS_ARE_NOT_ALLOWED_IN_AN_ASYNC_FUNCTION_BLOCK_1300,
                        &[],
                    );
                }
                self.error(
                    l,
                    dg::THE_WITH_STATEMENT_IS_NOT_SUPPORTED_ALL_SYMBOLS_IN_A_WITH_BLOCK_WILL_HAVE_TYPE_A_2410,
                    &[],
                );
            }
            Node::VarDecl { decls, .. } => {
                self.check_await_using_context(l);
                for &d in ast.list(decls) {
                    self.check_var_declarator(at(d));
                }
            }
            Node::FunctionDecl { .. } => {
                self.check_jsdoc_function_type(l);
                self.check_function_merged_with_class(l);
                self.check_function_like(l)
            }
            Node::ClassDecl { .. } => self.check_class(l),
            Node::Interface { .. } | Node::TypeAlias { .. } => {}
            Node::Enum {
                members, modifiers, ..
            } => {
                // An ambient (non-const) enum's initializers must be constant (TS1066).
                // Initializers naming something (`E.a`, `x`) are left alone: the enum
                // evaluator does not follow every reference tsc does.
                if !modifiers.contains(Modifiers::CONST)
                    && self.is_ambient_node(l)
                    && let Some(es) = self.c.sym_of_decl(l)
                {
                    let values = self.enum_members(es);
                    for &m in ast.list(members) {
                        if let Node::EnumMember { init: Some(i), .. } = *ast.node(m)
                            && let Some(ms) = self.c.sym_of_decl(at(m))
                            && values.iter().any(|&(s, v)| s == ms && v.is_none())
                            && !names_a_value(ast, i)
                        {
                            self.error(
                                at(i),
                                dg::IN_AMBIENT_ENUM_DECLARATIONS_MEMBER_INITIALIZER_MUST_BE_CONSTANT_EXPRESSION_1066,
                                &[],
                            );
                        }
                    }
                }
                self.check_enum_auto_members(l);
                for &m in ast.list(members) {
                    if let Node::EnumMember { name, .. } = *ast.node(m)
                        && matches!(ast.node(name), Node::PrivateName { .. })
                    {
                        self.error(
                            at(name),
                            dg::AN_ENUM_MEMBER_CANNOT_BE_NAMED_WITH_A_PRIVATE_IDENTIFIER_18024,
                            &[],
                        );
                    }
                    if let Node::EnumMember { init: Some(i), .. } = *ast.node(m) {
                        let t = self.check_expr(at(i));
                        self.check_enum_initializer(l, at(m), at(i), t, modifiers);
                    }
                }
            }
            Node::Module { body, .. } => {
                self.check_global_augmentation_declare(l);
                if let Some(b) = body {
                    self.check_stmt(at(b), depth + 1);
                }
            }
            Node::ImportDecl {
                attributes,
                type_only,
                phase,
                specifiers,
                ..
            } => {
                // `import defer * as ns`: TS18060 from `defer` to the end of the bindings.
                if phase == ts_r_ast::ImportPhase::Defer
                    && !type_only
                    && !self.module_supports_defer(l.file)
                    && let Some(&last) = ast.list(specifiers).last()
                {
                    let span = self.c.span(l);
                    let text = &self.c.file(l.file).text;
                    let end = ast.span(last).end;
                    if let Some(off) = text
                        .get(span.start as usize..end as usize)
                        .and_then(|t| t.find("defer"))
                    {
                        let start = span.start + off as u32;
                        self.error_span(
                            l.file,
                            Span::new(start, end),
                            dg::DEFERRED_IMPORTS_ARE_ONLY_SUPPORTED_WHEN_THE_MODULE_FLAG_IS_SET_TO_ESNEXT_OR_PRE_18060,
                            &[],
                        );
                    }
                }
                self.check_string_module_names(l);
                self.check_circular_aliases(l);
                let bad_attributes = self.check_import_attributes(l.file, attributes, type_only);
                self.check_relative_in_ambient_module(l);
                self.check_augmentation_statement(l);
                self.check_type_modifier_in_type_only_list(l);
                if !misplaced && !bad_attributes && !self.check_namespace_module_statement(l) {
                    self.check_import_decl(l)
                }
            }
            Node::ExportNamed {
                attributes,
                type_only,
                ..
            }
            | Node::ExportAll {
                attributes,
                type_only,
                ..
            } => {
                self.check_circular_aliases(l);
                self.check_string_module_names(l);
                self.check_import_attributes(l.file, attributes, type_only);
                self.check_relative_in_ambient_module(l);
                self.check_augmentation_statement(l);
                self.check_type_modifier_in_type_only_list(l);
                // Re-exports with attributes may resolve in another mode: not checked.
                if !misplaced
                    && !self.check_namespace_module_statement(l)
                    && matches!(
                        ast.node(l.node),
                        Node::ExportNamed {
                            source: Some(_),
                            attributes: None,
                            ..
                        }
                    )
                {
                    self.check_import_decl(l);
                }
                if !misplaced && matches!(ast.node(l.node), Node::ExportAll { .. }) {
                    self.check_export_star_of_export_equals(l);
                }
                if !misplaced {
                    self.check_local_export_specifiers(l);
                }
            }
            Node::ImportEquals { .. } => {
                self.check_circular_aliases(l);
                self.check_relative_in_ambient_module(l);
                self.check_augmentation_statement(l);
                self.check_import_equals_entity(l);
                if !misplaced {
                    self.check_namespace_module_statement(l);
                }
            }
            Node::NamespaceExport { .. } => self.check_circular_aliases(l),
            Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => {
                // Misplaced, or directly in a namespace (TS1063/TS1319): the expression is
                // not checked, to avoid cascading errors (tsc: checker.go
                // checkExportAssignment).
                if misplaced {
                    return;
                }
                if self.enclosing_namespace(file, l.node).is_some() {
                    let info = if matches!(ast.node(l.node), Node::ExportAssignment { .. }) {
                        dg::AN_EXPORT_ASSIGNMENT_CANNOT_BE_USED_IN_A_NAMESPACE_1063
                    } else {
                        dg::A_DEFAULT_EXPORT_CAN_ONLY_BE_USED_IN_AN_ECMASCRIPT_STYLE_MODULE_1319
                    };
                    self.error(l, info, &[]);
                    return;
                }
                self.check_circular_aliases(l);
                self.check_augmentation_statement(l);
                if !misplaced {
                    self.check_isolated_export_assignment(l, expr);
                }
                // `export = X` / `export default X` of an identifier exports whatever `X`
                // names: a type or a namespace without values is no error (TS2693/TS2708).
                let only_type = matches!(ast.node(expr), Node::Ident { .. })
                    && match self.c.resolve_ident(at(expr), meaning::VALUE) {
                        None => self
                            .c
                            .resolve_ident(at(expr), meaning::TYPE | meaning::NAMESPACE)
                            .is_some(),
                        Some(s) => {
                            self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
                                && self
                                    .c
                                    .resolve_alias(s)
                                    .is_some_and(|t| !self.c.has_meaning(t, meaning::VALUE))
                        }
                    };
                if !only_type {
                    let t = self.check_expr(at(expr));
                    // `/** @type {T} */ export default v` (JavaScript): `v` must be a `T`.
                    if let Some(declared) = self.jsdoc_export_default_type(l)
                        && t != TypeId::UNRESOLVED
                        && !self.store().is_error(t)
                    {
                        let src = t;
                        self.check_assignable_with_elaboration(
                            src,
                            declared,
                            at(expr),
                            Some(at(expr)),
                        );
                    }
                }
            }
            _ => {
                if !ast.node(l.node).is_statement() {
                    self.check_expr(l);
                }
            }
        }
    }

    /// The head of `for (left in o)` (`rt` is the type of `o`): no destructuring (TS2491),
    /// no annotation (TS2404), and an expression target must accept the keys
    /// (`Extract<keyof O, string>`, or `string`; `string` when `o` is unresolved): TS2405,
    /// whatever errors the target itself had (`for (a = 1 in b)`).
    fn check_for_in_head(&mut self, left: Loc, rt: TypeId) {
        let ast = self.ast(left.file);
        match *ast.node(left.node) {
            Node::VarDecl { decls, .. } => {
                // (Several declarators are the parser's TS1091.)
                let &[d] = ast.list(decls) else {
                    return;
                };
                let Node::VarDeclarator { name, type_ann, .. } = *ast.node(d) else {
                    return;
                };
                if matches!(
                    ast.node(name),
                    Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                ) {
                    self.error(
                        Loc::new(left.file, name),
                        dg::THE_LEFT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_CANNOT_BE_A_DESTRUCTURING_PATTERN_2491,
                        &[],
                    );
                } else if type_ann.is_some() {
                    self.error(
                        Loc::new(left.file, name),
                        dg::THE_LEFT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_CANNOT_USE_A_TYPE_ANNOTATION_2404,
                        &[],
                    );
                }
            }
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => {
                self.error(
                    left,
                    dg::THE_LEFT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_CANNOT_BE_A_DESTRUCTURING_PATTERN_2491,
                    &[],
                );
            }
            // `for (let in o)` is an empty `let` list to tsc (TS1123), not a name.
            Node::Ident { .. } if self.c.text(left) == "let" => {}
            _ => {
                // A member of an imported module object is read-only (TS2540, not TS2405).
                let target = skip_parens(ast, left.node);
                if let Node::Member { obj, .. } | Node::Index { obj, .. } = *ast.node(target)
                    && matches!(ast.node(obj), Node::Ident { .. })
                    && let Some(s) = self
                        .c
                        .resolve_ident(Loc::new(left.file, obj), meaning::VALUE)
                    && self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
                    && self
                        .c
                        .resolve_alias(s)
                        .is_some_and(|t| self.c.sym_flags(t).intersects(SymbolFlags::VALUE_MODULE))
                {
                    return;
                }
                let lt = self.quietly(|ck| ck.check_expr(left));
                if lt == TypeId::UNRESOLVED
                    || self.store().is_error(lt)
                    || self.store().flags(lt).intersects(TypeFlags::ANY_OR_UNKNOWN)
                {
                    return;
                }
                // An unresolved object is `any`: its keys are `string`.
                let keys = if rt == TypeId::UNRESOLVED || self.store().is_error(rt) {
                    TypeId::STRING
                } else {
                    self.keyof_type(rt)
                };
                let keys = if self.store().flags(keys).intersects(TypeFlags::INSTANTIABLE) {
                    self.intersection(&[keys, TypeId::STRING])
                } else {
                    self.filter_type(keys, |ck, m| {
                        ck.store().flags(m).intersects(TypeFlags::STRING_LIKE)
                    })
                };
                let keys = if self.store().flags(keys).contains(TypeFlags::NEVER) {
                    TypeId::STRING
                } else {
                    keys
                };
                if !self.is_assignable(keys, lt) {
                    self.error(
                        left,
                        dg::THE_LEFT_HAND_SIDE_OF_A_FOR_IN_STATEMENT_MUST_BE_OF_TYPE_STRING_OR_ANY_2405,
                        &[],
                    );
                }
            }
        }
    }

    /// Where an `await using` list `l` may stand: TS2852 in a function that is not async;
    /// at the top level TS2853 in a file that is not a module, then TS1309 in a file
    /// node16..nodenext emit as CommonJS, or TS2854 when `module` or `target` does not
    /// allow top-level await. All at the `await` keyword; none in an ambient context (that is
    /// TS1546) or a file with syntax errors (awaitUsingDeclarations.1/2/13/16).
    fn check_await_using_context(&mut self, l: Loc) {
        use ts_r_config::{ModuleKind as M, ScriptTarget};
        if !matches!(
            *self.node(l),
            Node::VarDecl {
                kind: ts_r_ast::VarKind::AwaitUsing,
                ..
            }
        ) || !self.reporting(l.file)
            || self.is_ambient_node(l)
            || self.has_syntax_errors(l.file)
        {
            return;
        }
        let start = self.c.span(l).start;
        let span = Span::new(start, start + "await".len() as u32);
        if self.in_sync_function(l) {
            self.error_span(
                l.file,
                span,
                dg::AWAIT_USING_STATEMENTS_ARE_ONLY_ALLOWED_WITHIN_ASYNC_FUNCTIONS_AND_AT_THE_TOP_LE_2852,
                &[],
            );
            return;
        }
        if !self.at_top_level(l) {
            return;
        }
        if !self.c.file(l.file).bound.is_module {
            self.error_span(
                l.file,
                span,
                dg::AWAIT_USING_STATEMENTS_ARE_ONLY_ALLOWED_AT_THE_TOP_LEVEL_OF_A_FILE_WHEN_THAT_FIL_2853,
                &[],
            );
        }
        if self.builds_into_node_commonjs(l.file) {
            self.error_span(
                l.file,
                span,
                dg::THE_CURRENT_FILE_IS_A_COMMONJS_MODULE_AND_CANNOT_USE_AWAIT_AT_THE_TOP_LEVEL_1309,
                &[],
            );
        } else if !matches!(
            self.c.opts.module,
            M::ES2022
                | M::ESNext
                | M::System
                | M::Node16
                | M::Node18
                | M::Node20
                | M::NodeNext
                | M::Preserve
        ) || self.c.opts.target < ScriptTarget::ES2017
        {
            self.error_span(
                l.file,
                span,
                dg::TOP_LEVEL_AWAIT_USING_STATEMENTS_ARE_ONLY_ALLOWED_WHEN_THE_MODULE_OPTION_IS_SET_T_2854,
                &[],
            );
        }
    }

    fn check_for_left(&mut self, left: Loc, depth: u32) {
        let ast = self.ast(left.file);
        match *ast.node(left.node) {
            Node::VarDecl { decls, .. } => {
                for &d in ast.list(decls) {
                    let dl = Loc::new(left.file, d);
                    if let Node::VarDeclarator { name, .. } = *ast.node(d)
                        && matches!(ast.node(name), Node::Ident { .. })
                    {
                        // The variable's type comes from the iterated expression.
                        if let Some(s) = self.c.sym_of_decl(Loc::new(left.file, name)) {
                            self.type_of_symbol(s);
                        }
                        continue;
                    }
                    self.check_var_declarator(dl);
                }
                let _ = depth;
            }
            // `for ([a, b] of xs)`: the pattern's elements are assignment targets, not reads.
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => {
                self.check_destructuring_targets(left);
            }
            _ => {
                self.check_expr(left);
            }
        }
    }

    /// The left side of `for (left of xs)`: a declaration, or an assignment target (a
    /// reference or a destructuring pattern) that must accept the iterated type `it`.
    fn check_for_of_left(&mut self, left: Loc, it: TypeId, depth: u32) {
        let ast = self.ast(left.file);
        let n = crate::nodes::skip_parens(ast, left.node);
        match *ast.node(n) {
            Node::VarDecl { .. } => {
                self.check_await_using_context(left);
                self.check_for_left(left, depth)
            }
            Node::ObjectLit { .. } | Node::ArrayLit { .. } => {
                self.check_destructuring_assignment(Loc::new(left.file, n), it, 0);
            }
            _ => {
                let tt = if matches!(ast.node(n), Node::Member { .. } | Node::Index { .. }) {
                    // `for (o.x of xs)` writes `o.x` (TS2540 when read-only).
                    self.assignment_target_type(left)
                } else {
                    self.check_expr(left);
                    // A constant, function, class, ... head is only that error.
                    if matches!(ast.node(n), Node::Ident { .. })
                        && !self.check_reference_target(left, false)
                    {
                        return;
                    }
                    self.quietly(|ck| ck.assignment_target_type(left))
                };
                // A head that is not a reference is TS2487 (`check_for_head_target`) only.
                if it == TypeId::UNRESOLVED
                    || self.store().is_error(it)
                    || !crate::assign_targets::is_reference_shape(ast, left.node)
                {
                    return;
                }
                if !self.store().is_error(tt) && tt != TypeId::UNRESOLVED {
                    self.check_assignable_with_elaboration(it, tt, left, None);
                }
            }
        }
    }

    /// Reports TS2488 (TS2504 for `for await` and async `yield*`) at `at` when `t` can't be
    /// iterated (`unknown` can't); `any` and error types are not checked. Returns whether it was
    /// reported.
    pub(crate) fn check_iterable(&mut self, t: TypeId, at: Loc, is_await: bool) -> bool {
        let t = self.reduce_conflicting_intersections(t);
        if self.store().flags(t).intersects(TypeFlags::ANY)
            || self.store().is_error(t)
            || t == TypeId::UNRESOLVED
            // Without a lib (`noLib`) the global types are missing and tsc is silent.
            || self.c.global_symbol("Array", meaning::TYPE).is_none()
        {
            return false;
        }
        let members = self.union_members(t);
        if members
            .iter()
            .any(|&m| self.store().flags(m).intersects(TypeFlags::INSTANTIABLE))
        {
            // A generic member iterates by its constraint (an unconstrained one can't);
            // nullish constraint members are dropped since `typeof x !== "undefined"`
            // narrowing of generics isn't tracked here.
            let mut ok = true;
            for m in members {
                let c = if self.store().flags(m).intersects(TypeFlags::INSTANTIABLE) {
                    let c = self.base_constraint(m);
                    if c == TypeId::UNRESOLVED || self.store().is_error(c) {
                        return false;
                    }
                    if c == TypeId::UNKNOWN {
                        ok = false;
                        break;
                    }
                    self.non_nullable(c)
                } else {
                    m
                };
                if self.iterated_type(c, is_await).is_none() {
                    ok = false;
                    break;
                }
            }
            if ok {
                return false;
            }
        } else {
            let mut notes = crate::symtype::IterNotes::default();
            if self.iterated_type_noting(t, is_await, &mut notes).is_some() {
                if notes.no_value {
                    self.error(
                        at,
                        dg::THE_TYPE_RETURNED_BY_THE_0_METHOD_OF_AN_ITERATOR_MUST_HAVE_A_VALUE_PROPERTY_2490,
                        &["next"],
                    );
                }
                if let Some(name) = notes.not_method {
                    self.error(
                        at,
                        dg::THE_0_PROPERTY_OF_AN_ITERATOR_MUST_BE_A_METHOD_2767,
                        &[name],
                    );
                }
                return notes.no_value;
            }
        }
        let ts = self.type_to_string(t);
        // Without `Symbol.iterator` in the lib only arrays and strings iterate.
        let msg = if self.well_known_symbol_name("iterator").is_none() {
            dg::TYPE_0_IS_NOT_AN_ARRAY_TYPE_OR_A_STRING_TYPE_2495
        } else if is_await {
            dg::TYPE_0_MUST_HAVE_A_SYMBOL_ASYNCITERATOR_METHOD_THAT_RETURNS_AN_ASYNC_ITERATOR_2504
        } else {
            dg::TYPE_0_MUST_HAVE_A_SYMBOL_ITERATOR_METHOD_THAT_RETURNS_AN_ITERATOR_2488
        };
        self.error(at, msg, &[&ts]);
        true
    }

    fn check_case_comparable(&mut self, test: Loc, tt: TypeId, dt: TypeId) {
        // A case of type `null` or `undefined` itself (not a union holding them) is never
        // reported (observed, tsc 7: `case null:` and `case u:` with `u: undefined` against
        // `string` are fine; `case nl:` with `nl: null | undefined` is TS2678).
        if self.store().flags(dt).intersects(TypeFlags::ANY_OR_UNKNOWN)
            || self.store().flags(tt).intersects(TypeFlags::NULLABLE)
            || self.store().is_error(dt)
            || self.store().is_error(tt)
        {
            return;
        }
        let tr = self.regular_of(tt);
        let dr = self.regular_of(dt);
        // A fresh object literal case keeps its excess property check (`case { id: 1,
        // name: '' }:` against `C` reports `'name' does not exist in type 'C'`).
        let fresh = self
            .store()
            .object_of(tt)
            .is_some_and(|o| o.flags.contains(ts_r_types::ObjectFlags::FRESH_LITERAL));
        if fresh {
            if self.is_comparable(dr, tr) {
                return;
            }
            self.check_type_related(
                tt,
                dr,
                ts_r_types::RelationKind::Comparable,
                Some(test),
                None,
            );
            return;
        }
        if self.is_comparable(tr, dr) || self.is_comparable(dr, tr) {
            return;
        }
        // Elaborated case to switch (`Type '1' is not comparable to type 'undefined'`).
        self.check_type_related(
            tr,
            dr,
            ts_r_types::RelationKind::Comparable,
            Some(test),
            None,
        );
    }

    fn check_var_declarator(&mut self, l: Loc) {
        let file = l.file;
        let ast = self.ast(file);
        let Node::VarDeclarator {
            name,
            type_ann,
            init,
            ..
        } = *ast.node(l.node)
        else {
            return;
        };
        let nl = Loc::new(file, name);
        // The type an empty or rest-only array pattern implies is `Iterable<any>` from ES2015
        // on. tsc builds it only when the initializer asks for its contextual type (an array
        // literal does, a plain identifier does not), and the lookup reports a missing
        // `Iterable` (`tsc/internal/checker/checker.go`, `getTypeFromArrayBindingPattern`).
        if let Node::ArrayPattern { elements } = *ast.node(name)
            && type_ann.is_none()
            && init.is_some_and(|i| {
                matches!(
                    ast.node(crate::nodes::skip_parens(ast, i)),
                    Node::ArrayLit { .. }
                )
            })
            && self.c.opts.target >= ts_r_config::ScriptTarget::ES2015
            && self.reporting(file)
        {
            let elements = ast.list(elements);
            if elements.is_empty()
                || (elements.len() == 1 && matches!(ast.node(elements[0]), Node::Rest { .. }))
            {
                self.global_type_reported(crate::checker::GlobalType::Iterable);
            }
        }
        if matches!(ast.node(name), Node::Ident { .. }) {
            let Some(sym) = self.c.sym_of_decl(nl) else {
                if let Some(i) = init {
                    self.check_expr(Loc::new(file, i));
                }
                return;
            };
            let declared = self.type_of_symbol(sym);
            if init.is_none() && type_ann.is_none() {
                self.report_implicit_any_declaration(l, nl, declared);
            }
            if let Some(i) = init {
                let il = Loc::new(file, i);
                let mut t = self.check_expr(il);
                if let Some(ta) = type_ann {
                    // A function initializer carries the variable's expando properties
                    // (`const f: Fn = () => 1; f.only = ...`).
                    if matches!(
                        ast.node(crate::nodes::skip_parens(ast, i)),
                        Node::FunctionExpr { .. } | Node::Arrow { .. }
                    ) {
                        t = self.with_expando_props(sym, t);
                    }
                    // A redeclared `var` checks its initializer against its own annotation
                    // (a different first one is TS2403).
                    let target = if self.c.decls(sym).len() > 1 {
                        self.type_from_node(Loc::new(file, ta))
                    } else {
                        declared
                    };
                    self.check_assignable_with_elaboration(t, target, nl, Some(il));
                }
                match self.c.parent(l).map(|list| *ast.node(list.node)) {
                    Some(Node::VarDecl {
                        kind: ts_r_ast::VarKind::Using,
                        ..
                    }) => self.check_disposable_initializer(il, t, false),
                    Some(Node::VarDecl {
                        kind: ts_r_ast::VarKind::AwaitUsing,
                        ..
                    }) => self.check_disposable_initializer(il, t, true),
                    _ => {}
                }
            }
            return;
        }
        // Binding patterns.
        self.check_binding_pattern_iterables(l);
        if type_ann.is_none()
            && init.is_none()
            && self.c.opts.no_implicit_any
            && !self.c.file(file).is_js()
            && !self.in_for_in_of_head(l)
        {
            self.report_implicit_binding_elements(nl, true);
        }
        let declared = type_ann.map(|t| self.type_from_node(Loc::new(file, t)));
        if let Some(i) = init {
            let il = Loc::new(file, i);
            let t = self.check_expr(il);
            // An empty pattern under `strictNullChecks` only asks for a non-nullish value
            // (`const {}: undefined = 1` is TS2532 alone; tsc: `checkVariableLikeDeclaration`).
            let empty = match *ast.node(nl.node) {
                Node::ObjectPattern { props } => ast.list(props).is_empty(),
                Node::ArrayPattern { elements } => ast
                    .list(elements)
                    .iter()
                    .all(|&e| matches!(ast.node(e), Node::Hole { .. })),
                _ => false,
            };
            if let Some(d) = declared {
                if !(empty && self.c.opts.strict_null_checks) {
                    self.check_assignable_with_elaboration(t, d, nl, Some(il));
                }
            } else {
                self.check_pattern_default_excess(nl, il);
            }
        }
        let using = self.c.parent(l).is_some_and(|list| {
            matches!(
                ast.node(list.node),
                Node::VarDecl {
                    kind: ts_r_ast::VarKind::Using | ts_r_ast::VarKind::AwaitUsing,
                    ..
                }
            )
        });
        if !using && (init.is_some() || self.in_for_in_of_head(l)) {
            self.check_empty_binding_pattern(nl, true);
        }
        self.check_binding_pattern_defaults(nl);
    }

    /// TS2492 at a `let`/`const`/`using` declared directly in a catch block under the name of
    /// the catch variable (a `var` merges with it instead). tsc: `checkCatchClause` in
    /// tsc/internal/checker/checker.go.
    fn check_catch_redeclaration(
        &mut self,
        file: ts_r_diagnostics::FileId,
        param: NodeId,
        body: NodeId,
    ) {
        let ast = self.ast(file);
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let caught: Vec<ts_r_ast::Atom> = binding_name_nodes(ast, param)
            .into_iter()
            .filter_map(|n| match *ast.node(n) {
                Node::Ident { name } => Some(name),
                _ => None,
            })
            .collect();
        if caught.is_empty() {
            return;
        }
        // The first block-scoped declaration of each caught name (its value declaration).
        let mut seen: Vec<ts_r_ast::Atom> = Vec::new();
        for &s in ast.list(stmts) {
            let Node::VarDecl { kind, decls, .. } = *ast.node(s) else {
                continue;
            };
            if kind == ts_r_ast::VarKind::Var {
                continue;
            }
            for &d in ast.list(decls) {
                let Node::VarDeclarator { name: dn, .. } = *ast.node(d) else {
                    continue;
                };
                for b in binding_name_nodes(ast, dn) {
                    if let Node::Ident { name: n } = *ast.node(b)
                        && caught.contains(&n)
                        && !seen.contains(&n)
                    {
                        seen.push(n);
                        let text = ast.atom(n).to_owned();
                        self.error(
                            Loc::new(file, b),
                            dg::CANNOT_REDECLARE_IDENTIFIER_0_IN_CATCH_CLAUSE_2492,
                            &[&text],
                        );
                    }
                }
            }
        }
    }

    /// TS2850/TS2851 at the initializer `il` (of type `t`) of a `using` (`await using` when
    /// `is_await`) declaration whose widened type is not assignable to `Disposable | null |
    /// undefined` (`AsyncDisposable | Disposable | null | undefined`). Skipped when the lib
    /// lacks those interfaces.
    fn check_disposable_initializer(&mut self, il: Loc, t: TypeId, is_await: bool) {
        if !self.reporting(il.file) {
            return;
        }
        // tsc looks both up (reporting TS2318) before it gives up on a missing one.
        let async_disposable = if is_await {
            self.global_type_reported(crate::checker::GlobalType::AsyncDisposable)
        } else {
            None
        };
        let Some(disposable) = self.global_type_reported(crate::checker::GlobalType::Disposable)
        else {
            return;
        };
        let mut parts = vec![disposable, TypeId::NULL, TypeId::UNDEFINED];
        let msg = if is_await {
            let Some(async_disposable) = async_disposable else {
                return;
            };
            parts.insert(0, async_disposable);
            dg::THE_INITIALIZER_OF_AN_AWAIT_USING_DECLARATION_MUST_BE_EITHER_AN_OBJECT_WITH_A_SY_2851
        } else {
            dg::THE_INITIALIZER_OF_A_USING_DECLARATION_MUST_BE_EITHER_AN_OBJECT_WITH_A_SYMBOL_DI_2850
        };
        let target = self.union(&parts);
        let widened = self.widened_type(t);
        let source = self.regular_of(widened);
        if self.store().is_error(source) || source == TypeId::UNRESOLVED {
            return;
        }
        if !self.is_assignable(source, target) {
            self.report_assignability(
                source,
                target,
                il,
                None,
                Some(crate::relate::Head { info: msg }),
            );
        }
    }

    /// An empty binding pattern still needs an object: TS2571/TS2531/TS2532/TS2533 at the
    /// pattern for an `unknown`, nullable or `void` value (`const {} = f()` with
    /// `f(): unknown`); a pattern with elements reports its properties instead.
    /// Array patterns of parameters only need an iterable.
    fn check_empty_binding_pattern(&mut self, pattern: Loc, arrays: bool) {
        let empty = match *self.node(pattern) {
            Node::ObjectPattern { props } => self.ast(pattern.file).list(props).is_empty(),
            Node::ArrayPattern { elements } => {
                arrays && self.ast(pattern.file).list(elements).is_empty()
            }
            _ => false,
        };
        if !empty || !self.reporting(pattern.file) {
            return;
        }
        let t = self.pattern_parent_type(pattern);
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return;
        }
        // Here `void` counts as possibly `undefined` (observed: `const {} = v` and
        // `const [] = v` with `v: void` are TS2532).
        if self.c.opts.strict_null_checks
            && self.some_member(t, |m| self.store().flags(m).contains(TypeFlags::VOID))
        {
            let (_, null) = self.nullable_facts(t);
            self.report_possibly_nullish(pattern, true, null);
            return;
        }
        self.check_non_null(pattern, t);
    }

    /// TS7005 at an unannotated, uninitialized variable whose type is `any` rather than
    /// evolving: an ambient one (`declare var x;`, in a declaration file or an ambient
    /// namespace) or a `const` (observed with tsc 7). Not a `for`-`in`/`of` variable.
    fn report_implicit_any_declaration(&mut self, l: Loc, nl: Loc, declared: TypeId) {
        if declared != TypeId::ANY
            || !self.c.opts.no_implicit_any
            || self.c.file(l.file).is_js()
            || !self.reporting(l.file)
        {
            return;
        }
        let ast = self.ast(l.file);
        let Some(list) = self.c.parent(l) else {
            return;
        };
        if self.in_for_in_of_head(l) {
            return;
        }
        let is_const = matches!(
            ast.node(list.node),
            Node::VarDecl {
                kind: ts_r_ast::VarKind::Const,
                ..
            }
        );
        if !is_const && !self.is_ambient_node(l) {
            return;
        }
        let name = self.c.text(nl).to_owned();
        self.error(
            nl,
            dg::VARIABLE_0_IMPLICITLY_HAS_AN_1_TYPE_7005,
            &[&name, "any"],
        );
    }

    /// Whether declarator `l` is the variable of a `for`-`in` or `for`-`of` head.
    fn in_for_in_of_head(&self, l: Loc) -> bool {
        let ast = self.ast(l.file);
        self.c
            .parent(l)
            .and_then(|list| self.c.parent(list))
            .is_some_and(|h| matches!(ast.node(h.node), Node::ForIn { .. } | Node::ForOf { .. }))
    }

    /// Default values inside binding patterns.
    fn check_binding_pattern_defaults(&mut self, p: Loc) {
        let ast = self.ast(p.file);
        let mut stack = vec![p.node];
        let mut guard = 0;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 10_000 {
                break;
            }
            match *ast.node(n) {
                Node::ObjectPattern { props } => {
                    stack.extend(ast.list(props).iter().copied());
                }
                Node::ArrayPattern { elements } => {
                    self.check_array_pattern_bounds(Loc::new(p.file, n));
                    stack.extend(ast.list(elements).iter().copied());
                }
                Node::BindingProperty { key, value, init } => {
                    if let Node::ComputedKey { expr } = *ast.node(key) {
                        let kt = self.check_expr(Loc::new(p.file, expr));
                        self.check_destructured_computed_key(Loc::new(p.file, n), expr, kt);
                    }
                    let has_default = init.is_some()
                        || value.is_some_and(|v| matches!(ast.node(v), Node::AssignPattern { .. }));
                    self.check_destructured_property(Loc::new(p.file, n), key, has_default);
                    self.check_binding_property_access(Loc::new(p.file, n), key);
                    if let Some(v) = value {
                        stack.push(v);
                    }
                    if let Some(i) = init {
                        let it = self.check_expr(Loc::new(p.file, i));
                        let name = value.unwrap_or(key);
                        self.check_binding_default(Loc::new(p.file, name), it, i);
                        // A nested pattern's default is typed by that pattern when the
                        // destructured value has no such property to give it a type.
                        if let Some(v) = value
                            && matches!(ast.node(v), Node::ObjectPattern { .. })
                            && let Some(k) = key_text(ast, key)
                            && let Some(obj) = self.c.parent(Loc::new(p.file, n))
                        {
                            let parent_t = self.pattern_parent_type(obj);
                            if self.property_of_type(parent_t, &k).is_none() {
                                self.check_pattern_default_excess(
                                    Loc::new(p.file, v),
                                    Loc::new(p.file, i),
                                );
                            }
                        }
                    }
                }
                Node::AssignPattern { left, right } => {
                    stack.push(left);
                    let it = self.check_expr(Loc::new(p.file, right));
                    self.check_binding_default(Loc::new(p.file, left), it, right);
                }
                Node::Rest { arg } => stack.push(arg),
                _ => {}
            }
        }
    }

    /// TS2493 at the elements of an array binding pattern past the end of the tuple it
    /// destructures (`const [a, b] = t` with `t: [number]`), unless they have a default;
    /// TS2339 past the end of every tuple of a union. Not for a parameter typed only by
    /// its default (implicitly `any` there, TS7031), nor in JavaScript, ambient
    /// declarations or under a rest element.
    fn check_array_pattern_bounds(&mut self, pattern: Loc) {
        if !self.reporting(pattern.file)
            || self.c.file(pattern.file).is_js()
            || self.is_ambient_node(pattern)
        {
            return;
        }
        let ast = self.ast(pattern.file);
        let mut cur = pattern;
        for _ in 0..200 {
            let Some(p) = self.c.parent(cur) else {
                return;
            };
            match *ast.node(p.node) {
                Node::ArrayPattern { .. }
                | Node::ObjectPattern { .. }
                | Node::BindingProperty { .. }
                | Node::AssignPattern { .. } => cur = p,
                Node::VarDeclarator { .. } => break,
                Node::Param { init, type_ann, .. } => {
                    if type_ann.is_none() && init.is_some() {
                        return;
                    }
                    if let Some(f) = self.c.parent(p)
                        && fn_parts(ast, f.node).is_some_and(|parts| parts.body.is_none())
                    {
                        return;
                    }
                    break;
                }
                _ => return,
            }
        }
        let Node::ArrayPattern { elements } = *ast.node(pattern.node) else {
            return;
        };
        let els = ast.list(elements).to_vec();
        let parent = self.pattern_parent_type(pattern);
        for (i, e) in els.into_iter().enumerate() {
            if matches!(
                ast.node(e),
                Node::Hole {} | Node::Rest { .. } | Node::AssignPattern { .. }
            ) {
                continue;
            }
            self.check_tuple_element_bound(parent, i, Loc::new(pattern.file, e));
        }
    }

    /// TS2493 when element `i` is past the end of the tuple `t` (without rest elements),
    /// TS2339 when it is past the end of every tuple of the union `t`.
    pub(crate) fn check_tuple_element_bound(&mut self, t: TypeId, i: usize, at: Loc) -> bool {
        let s = self.store();
        if let Some(shape) = s.tuple_shape_of(t) {
            if shape.has_rest() || i < shape.elements.len() {
                return false;
            }
            let len = shape.elements.len();
            let ts = self.type_to_string(t);
            self.error(
                at,
                dg::TUPLE_TYPE_0_OF_LENGTH_1_HAS_NO_ELEMENT_AT_INDEX_2_2493,
                &[&ts, &len.to_string(), &i.to_string()],
            );
            return true;
        }
        if !s.flags(t).contains(TypeFlags::UNION) {
            return false;
        }
        let members = s.members_of_union(t);
        let past_all = members.iter().all(|&m| {
            s.tuple_shape_of(m)
                .is_some_and(|sh| !sh.has_rest() && i >= sh.elements.len())
        });
        if !past_all {
            return false;
        }
        let ts = self.type_to_string(t);
        self.error(
            at,
            dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
            &[&i.to_string(), &ts],
        );
        true
    }

    /// A binding element's default must be assignable to the element's type (TS2322 at the
    /// name; an object literal default elaborates). Without an annotation on the declaration
    /// the default is part of that type, so only annotated declarations can fail.
    fn check_binding_default(&mut self, name: Loc, it: TypeId, init: NodeId) {
        if !self.reporting(name.file) || self.c.file(name.file).is_js() {
            return;
        }
        let target = self.element_type_in_pattern(name);
        if target == TypeId::UNRESOLVED || self.store().is_error(target) {
            return;
        }
        self.check_assignable_with_elaboration(it, target, name, Some(Loc::new(name.file, init)));
    }

    /// A computed key of an object binding pattern without a literal name reads the parent
    /// through an index signature (observed rules): an `any` key (or one that failed to
    /// resolve) needs a string or number index signature (else TS2538 naming `any`), a
    /// `string` key a string one and a `number` key either (else TS2537). Reported at the
    /// key expression; `any`, `never`, `unknown` and generic parents are not checked. An
    /// untyped parameter's parent is the type its pattern implies (`{}`).
    fn check_destructured_computed_key(&mut self, prop: Loc, expr: NodeId, kt: TypeId) {
        if !self.reporting(prop.file) || self.c.file(prop.file).is_js() {
            return;
        }
        let e = Loc::new(prop.file, expr);
        let late_bound = self.late_bound_name(e);
        let symbol_key = self.store().flags(kt).intersects(TypeFlags::ES_SYMBOL_LIKE)
            && !self.store().flags(kt).contains(TypeFlags::UNION);
        if late_bound.is_some() && !symbol_key {
            return;
        }
        let Some(obj_pat) = self.c.parent(prop) else {
            return;
        };
        // A parameter of a function called in place without that argument receives
        // `undefined` (`any` without strictNullChecks).
        let parent = if self.called_in_place_without_argument(obj_pat) {
            if !self.c.opts.strict_null_checks {
                return;
            }
            TypeId::UNDEFINED
        } else {
            self.pattern_parent_type(obj_pat)
        };
        self.check_computed_key_read(e, kt, parent, late_bound);
    }

    /// The index-signature read of a computed destructuring key `e` of type `kt` (not a
    /// literal name) from `parent`: TS2464/TS2538/TS2537 as described at
    /// [`Self::check_destructured_computed_key`]. Shared with assignment patterns
    /// (`({ [k]: x } = o)`), where `parent` is the assigned value's type.
    pub(crate) fn check_computed_key_read(
        &mut self,
        e: Loc,
        kt: TypeId,
        parent: TypeId,
        late_bound: Option<String>,
    ) {
        let symbol_key = self.store().flags(kt).intersects(TypeFlags::ES_SYMBOL_LIKE)
            && !self.store().flags(kt).contains(TypeFlags::UNION);
        // Not a key type: TS2464 at the name, and TS2538 naming the type at the expression
        // unless the parent is `any` (observed, generic parents included).
        if let Some(key) = self.c.parent(e)
            && self.check_computed_key_type(key, kt)
        {
            if parent != TypeId::UNRESOLVED
                && !self.store().is_error(parent)
                && !self.store().flags(parent).intersects(TypeFlags::ANY)
            {
                let ks = self.type_to_string(kt);
                self.error(e, dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538, &[&ks]);
            }
            return;
        }
        let s = self.store();
        if parent == TypeId::UNRESOLVED
            || s.is_error(parent)
            || s.flags(parent).intersects(
                TypeFlags::ANY | TypeFlags::NEVER | TypeFlags::UNKNOWN | TypeFlags::INSTANTIABLE,
            )
        {
            return;
        }
        // A symbol key the parent has neither as a property nor through a symbol index
        // signature cannot index it (observed: TS2538 naming `unique symbol` or `symbol`,
        // even with a string index).
        if symbol_key {
            let unique = s.flags(kt).contains(TypeFlags::UNIQUE_ES_SYMBOL);
            if let Some(name) = &late_bound
                && self.property_of_type(parent, name).is_some()
            {
                return;
            }
            let apparent = self.apparent_type(parent);
            if apparent == TypeId::UNRESOLVED
                || self.store().flags(apparent).intersects(TypeFlags::ANY)
                || self.index_info_of(apparent, TypeId::SYMBOL).is_some()
            {
                return;
            }
            let found = late_bound.as_ref().is_some_and(|n| {
                let id = self.store().intern_str(n);
                self.property_of_type_id(apparent, id).is_some()
            });
            if !found {
                let ks = if late_bound.is_some() || unique {
                    "unique symbol".to_owned()
                } else {
                    self.type_to_string(kt)
                };
                self.error(e, dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538, &[&ks]);
            }
            return;
        }
        let any_key = s.is_error(kt) || s.flags(kt).intersects(TypeFlags::ANY);
        if !any_key && kt != TypeId::STRING && kt != TypeId::NUMBER {
            return;
        }
        let apparent = self.apparent_type(parent);
        if apparent == TypeId::UNRESOLVED || self.store().flags(apparent).intersects(TypeFlags::ANY)
        {
            return;
        }
        let string_index = self.index_info_of(apparent, TypeId::STRING).is_some();
        let number_index = self.index_info_of(apparent, TypeId::NUMBER).is_some();
        if any_key {
            if !string_index && !number_index {
                self.error(e, dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538, &["any"]);
            }
        } else if !string_index && (kt == TypeId::STRING || !number_index) {
            // A primitive parent is read through its wrapper (`Number`).
            let pf = self.store().flags(parent);
            let shown = if pf.intersects(TypeFlags::PRIMITIVE)
                && !pf.intersects(
                    TypeFlags::UNION | TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID,
                ) {
                apparent
            } else {
                parent
            };
            let ts = self.type_to_string(shown);
            let ks = self.type_to_string(kt);
            self.error(
                e,
                dg::TYPE_0_HAS_NO_MATCHING_INDEX_SIGNATURE_FOR_TYPE_1_2537,
                &[&ts, &ks],
            );
        }
    }

    /// TS2341/TS2445 for an object binding element that reads a private or protected
    /// member from outside its class (`let { priv } = new K()`), at the element. The name
    /// is the key's literal name (a computed key of a literal type included) looked up on
    /// the pattern's parent type (tsc: checker.go, checkVariableLikeDeclaration).
    fn check_binding_property_access(&mut self, prop: Loc, key: NodeId) {
        if !self.reporting(prop.file) {
            return;
        }
        let Some(obj_pat) = self.c.parent(prop) else {
            return;
        };
        if !matches!(
            self.ast(prop.file).node(obj_pat.node),
            Node::ObjectPattern { .. }
        ) {
            return;
        }
        let Some(name) = self.member_key_name(Loc::new(prop.file, key)) else {
            return;
        };
        let parent = self.pattern_parent_type(obj_pat);
        self.check_destructured_member_access(prop, parent, &name);
    }

    /// Whether `pattern` is the pattern of a parameter of a function expression or arrow
    /// that is called in place with no argument for it (`(({ a }) => {})()`).
    fn called_in_place_without_argument(&self, pattern: Loc) -> bool {
        let Some(param) = self.c.parent(pattern) else {
            return false;
        };
        let ast = self.ast(pattern.file);
        if !matches!(*ast.node(param.node), Node::Param { pattern: p, .. } if p == pattern.node) {
            return false;
        }
        let Some(func) = self.c.parent(param) else {
            return false;
        };
        let params = match *ast.node(func.node) {
            Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => params,
            _ => return false,
        };
        let Some(index) = ast.list(params).iter().position(|&x| x == param.node) else {
            return false;
        };
        let mut cur = func;
        for _ in 0..64 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            match *ast.node(p.node) {
                Node::Paren { .. } => cur = p,
                Node::Call { callee, args, .. } if callee == cur.node => {
                    let args = ast.list(args);
                    return index >= args.len()
                        && !args
                            .iter()
                            .any(|&a| matches!(ast.node(a), Node::Spread { .. }));
                }
                _ => return false,
            }
        }
        false
    }

    /// TS2339 for a property an object binding pattern reads that its parent type lacks
    /// (`const { b } = { a: 1 }`). Observed rules: the message names the parent type (a
    /// type parameter's constraint, `unknown` without one, a primitive's wrapper), has no
    /// chain or suggestion; an element with a default is allowed when the value comes from
    /// an object or array literal (the literal is typed by the pattern).
    fn check_destructured_property(&mut self, prop: Loc, key: NodeId, has_default: bool) {
        if !self.reporting(prop.file) {
            return;
        }
        let ast = self.ast(prop.file);
        let Some(obj_pat) = self.c.parent(prop) else {
            return;
        };
        if !matches!(ast.node(obj_pat.node), Node::ObjectPattern { .. }) {
            return;
        }
        let name = match key_text(ast, key) {
            Some(k) => k,
            None => match *ast.node(key) {
                Node::ComputedKey { expr } => {
                    match self.late_bound_name(Loc::new(prop.file, expr)) {
                        Some(n) => n,
                        None => return,
                    }
                }
                _ => return,
            },
        };
        if name.starts_with("__@") || name.starts_with('#') {
            return;
        }
        let root = self.pattern_root(obj_pat);
        // A literal default of an enclosing element is typed by the pattern; a literal
        // initializer gets the pattern's defaulted properties; rest elements and array
        // patterns over literals (tuples in tsc) are not modelled.
        if root.default_literal
            || root.rest
            || (root.literal && (has_default || root.in_array_pattern) && !root.rest_of_array)
        {
            return;
        }
        let mut parent = self.pattern_parent_type(obj_pat);
        if parent == TypeId::UNRESOLVED || self.store().is_error(parent) {
            return;
        }
        if root.optional_param && self.c.opts.strict_null_checks {
            parent = self.filter_type(parent, |ck, m| {
                !ck.store().flags(m).contains(TypeFlags::UNDEFINED)
            });
        }
        let s = self.store();
        if s.flags(parent)
            .intersects(TypeFlags::ANY | TypeFlags::NEVER)
        {
            return;
        }
        // A type parameter narrowed by its uses (`if (v.kind === 'a') { const { a } = v }`)
        // and `unknown` inferred for an unannotated callback parameter (tsc infers from the
        // pattern) are not modelled.
        if s.flags(parent).intersects(TypeFlags::INSTANTIABLE) && !root.param {
            return;
        }
        if parent == TypeId::UNKNOWN && root.contextual_param {
            return;
        }
        let shown = if s.flags(parent).intersects(TypeFlags::INSTANTIABLE) {
            let c = self.base_constraint(parent);
            if c == TypeId::UNRESOLVED || self.store().flags(c).intersects(TypeFlags::ANY) {
                return;
            }
            c
        } else {
            parent
        };
        let apparent = self.apparent_type(shown);
        if apparent == TypeId::UNRESOLVED || self.store().flags(apparent).intersects(TypeFlags::ANY)
        {
            return;
        }
        if shown != TypeId::UNKNOWN {
            if self.property_of_type(shown, &name).is_some() {
                return;
            }
            // `const { 1: b } = t` past the end of a tuple.
            if let Ok(i) = name.parse::<usize>()
                && i.to_string() == name
                && self.store().tuple_shape_of(shown).is_some()
            {
                let kl = Loc::new(prop.file, key);
                self.check_tuple_element_bound(shown, i, kl);
                return;
            }
            let key_t = if crate::expr::is_numeric_name(&name) {
                TypeId::NUMBER
            } else {
                TypeId::STRING
            };
            if self.index_info_of(shown, key_t).is_some()
                || (key_t == TypeId::NUMBER && self.index_info_of(shown, TypeId::STRING).is_some())
                || self.has_index_for_name(shown, &name)
            {
                return;
            }
        }
        let shown = if self.store().flags(shown).intersects(TypeFlags::PRIMITIVE)
            && !self.store().flags(shown).intersects(
                TypeFlags::UNION
                    | TypeFlags::NULL
                    | TypeFlags::UNDEFINED
                    | TypeFlags::UNKNOWN
                    | TypeFlags::NEVER
                    | TypeFlags::VOID,
            ) {
            apparent
        } else {
            shown
        };
        // Without strict null checks `unknown` reads as `{}`.
        let ts = if shown == TypeId::OBJECT
            || (shown == TypeId::UNKNOWN && !self.c.opts.strict_null_checks)
        {
            "{}".to_owned()
        } else {
            self.type_to_string(shown)
        };
        let kl = match *ast.node(key) {
            Node::ComputedKey { expr } => Loc::new(prop.file, expr),
            _ => Loc::new(prop.file, key),
        };
        self.error(
            kl,
            dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
            &[&name, &ts],
        );
    }

    /// Where the value an object pattern destructures comes from.
    fn pattern_root(&self, pattern: Loc) -> PatternRoot {
        let ast = self.ast(pattern.file);
        let is_lit = |e: NodeId| contains_literal_operand(ast, e, 0);
        let mut r = PatternRoot::default();
        if self.c.file(pattern.file).is_js() || self.is_ambient_node(pattern) {
            r.rest = true; // JSDoc types and ambient signatures: stay quiet
            return r;
        }
        let mut cur = pattern;
        for _ in 0..200 {
            let Some(p) = self.c.parent(cur) else {
                return r;
            };
            match *ast.node(p.node) {
                Node::BindingProperty { init, .. } => {
                    r.default_literal |= init.is_some_and(is_lit);
                }
                Node::AssignPattern { right, .. } => r.default_literal |= is_lit(right),
                Node::ArrayPattern { .. } => r.in_array_pattern = true,
                Node::ObjectPattern { .. } => {}
                Node::Rest { .. } => {
                    if cur == pattern
                        && self.c.parent(p).is_some_and(|ap| {
                            matches!(ast.node(ap.node), Node::ArrayPattern { .. })
                        })
                    {
                        r.rest_of_array = true;
                    } else {
                        r.rest = true;
                    }
                }
                Node::VarDeclarator { init, .. } => {
                    r.literal = init.is_some_and(is_lit);
                    // `= []` prints as tsc's `undefined[]`/`never[]`; not modelled.
                    // `= x || {}`: object literals in a union are normalized (missing
                    // properties become optional), not modelled.
                    // A rest element over a non-empty array literal reads an array.
                    let rest_of_array = r.rest_of_array;
                    r.rest |= init.is_some_and(|i| {
                        let n = skip_parens(ast, i);
                        let array_rest = rest_of_array
                            && matches!(ast.node(n), Node::ArrayLit { elements } if !ast.list(*elements).is_empty());
                        !matches!(ast.node(n), Node::ObjectLit { .. }) && !array_rest && is_lit(i)
                    });
                    let stmt = self.c.parent(p).and_then(|d| self.c.parent(d));
                    if let Some(st) = stmt
                        && let Node::ForOf { right, .. } = *ast.node(st.node)
                    {
                        // The values are the array literal's elements (`of [2, 3]` reads
                        // numbers, not literals typed by the pattern).
                        r.literal = match *ast.node(skip_parens(ast, right)) {
                            Node::ArrayLit { elements } => ast
                                .list(elements)
                                .iter()
                                .any(|&el| contains_literal_operand(ast, el, 0)),
                            _ => is_lit(right),
                        };
                    }
                    return r;
                }
                Node::Param {
                    init,
                    optional,
                    rest,
                    type_ann,
                    ..
                } => {
                    r.param = true;
                    // Overloads and other body-less signatures are not checked.
                    if let Some(f) = self.c.parent(p)
                        && fn_parts(ast, f.node).is_some_and(|parts| parts.body.is_none())
                    {
                        r.rest = true;
                    }
                    r.literal = init.is_some_and(is_lit);
                    r.optional_param = optional && init.is_none();
                    r.rest |= rest;
                    r.contextual_param = type_ann.is_none()
                        && self.c.parent(p).is_some_and(|f| {
                            matches!(
                                ast.node(f.node),
                                Node::FunctionExpr { .. } | Node::Arrow { .. }
                            )
                        });
                    return r;
                }
                Node::CatchClause { .. } => return r,
                _ => {
                    r.rest = true; // unknown context: stay quiet
                    return r;
                }
            }
            cur = p;
        }
        r
    }

    // ---------------------------------------------------------------------------------
    // Functions
    // ---------------------------------------------------------------------------------

    /// Parameters, body and return checks of a function-like node (declarations,
    /// expressions, arrows, method values).
    pub fn check_function_like(&mut self, l: Loc) {
        let file = l.file;
        let ast = self.ast(file);
        let Some(parts) = fn_parts(ast, l.node) else {
            return;
        };
        let is_expr = matches!(
            ast.node(l.node),
            Node::FunctionExpr { .. } | Node::Arrow { .. }
        );
        // Parameters.
        let params: Vec<NodeId> = ast.list(parts.params).to_vec();
        if parts.is_arrow
            && let Some(&first) = params.first()
            && is_this_param(ast, first)
        {
            self.error(
                Loc::new(file, first),
                dg::AN_ARROW_FUNCTION_CANNOT_HAVE_A_THIS_PARAMETER_2730,
                &[],
            );
        }
        for &p in &params {
            self.check_param(Loc::new(file, p), is_expr);
        }
        self.report_circular_return(l, parts);
        let Some(body) = parts.body else { return };
        let bl = Loc::new(file, body);
        let expression_body =
            parts.is_arrow && parts.flags.contains(ts_r_ast::FnFlags::EXPRESSION_BODY);
        if !expression_body {
            self.check_use_strict_parameters(file, &params, body);
        }
        if expression_body {
            let t = self.check_expr(bl);
            if let Some(rt) = parts.return_type
                && !parts.flags.contains(ts_r_ast::FnFlags::GENERATOR)
            {
                let sig = self.signature_from_decl(l);
                if let Some(declared) = self.declared_return_of(sig) {
                    let _ = rt;
                    let is_async = parts.flags.contains(ts_r_ast::FnFlags::ASYNC);
                    let (t, target) = if is_async {
                        let a = self.awaited_type(t);
                        let d = self.awaited_type(declared);
                        (a, d)
                    } else {
                        (t, declared)
                    };
                    self.check_return_expression(bl, t, target, is_async, bl, 0);
                }
            }
            return;
        }
        if let Node::Block { stmts } = *ast.node(body) {
            self.check_unreachable_stmts(file, ast.list(stmts));
            for &s in ast.list(stmts) {
                self.check_stmt(Loc::new(file, s), 1);
            }
        }
        self.check_getter_returns(l, parts);
        self.check_function_returns(l, parts);
        self.check_inferred_implicit_returns(l, parts);
        self.check_generator_fallback_type(l, parts);
    }

    /// TS2318 for an unannotated generator with a body when the lib has neither
    /// `Generator` nor the `IterableIterator` tsc falls back to (async: `AsyncGenerator`,
    /// `AsyncIterableIterator`): tsc infers every such generator's return type when it checks
    /// it (`tsc/internal/checker/checker.go`, `createGeneratorType`).
    fn check_generator_fallback_type(&mut self, l: Loc, parts: crate::nodes::FnParts) {
        use crate::checker::GlobalType as G;
        if parts.return_type.is_some()
            || parts.body.is_none()
            || !parts.flags.contains(ts_r_ast::FnFlags::GENERATOR)
            || !self.reporting(l.file)
        {
            return;
        }
        let (generator, fallback) = if parts.flags.contains(ts_r_ast::FnFlags::ASYNC) {
            (G::AsyncGenerator, G::AsyncIterableIterator)
        } else {
            (G::Generator, G::IterableIterator)
        };
        if self.global_type(generator).is_none() {
            self.global_type_reported(fallback);
        }
    }

    /// TS7023 under `noImplicitAny`: a function declaration, or a function expression
    /// initializing a variable (named by it), without a return type annotation whose return
    /// type inference needs that return type (`var f = () => f()`, mutually recursive `h`
    /// and `foo`).
    fn report_circular_return(&mut self, l: Loc, parts: crate::nodes::FnParts) {
        if !self.c.opts.no_implicit_any
            || parts.return_type.is_some()
            || parts.body.is_none()
            || parts.flags.contains(ts_r_ast::FnFlags::GENERATOR)
            || !self.reporting(l.file)
        {
            return;
        }
        let ast = self.ast(l.file);
        let parent = self.c.parent(l);
        if let Some(p) = parent
            && let Node::MethodDef {
                kind: ts_r_ast::MethodKind::Get,
                key,
                value,
                ..
            } = *ast.node(p.node)
            && value == l.node
        {
            self.report_circular_getter(p, key, l);
            return;
        }
        let name = match *ast.node(l.node) {
            Node::FunctionDecl { name, .. } => name,
            Node::FunctionExpr { name: Some(n), .. } => Some(n),
            // Methods and accessors are not reported: their `this` and member types
            // resolve in a different order from tsc's here, which finds no cycle.
            _ => match parent.map(|p| *ast.node(p.node)) {
                Some(Node::VarDeclarator {
                    name,
                    init: Some(i),
                    ..
                }) if i == l.node && matches!(ast.node(name), Node::Ident { .. }) => Some(name),
                _ => return,
            },
        };
        let sig = self.signature_from_decl(l);
        self.return_type_of(sig);
        if !self.circular.contains(&Resolving::Return(sig)) {
            return;
        }
        // Only cycles through the return types of such functions and expressions alone: one
        // through a nested function expression's return type is one tsc defers (its return
        // type is not needed to infer an argument for a bare type parameter), and one
        // through a declared or member type resolves in a different order there.
        let path = self.return_cycles.get(&sig).cloned().unwrap_or_default();
        let named_function = |ck: &Self, s: ts_r_types::SigId| {
            let Some(d) = ck.store().sig(s).decl.and_then(Loc::unpack) else {
                return false;
            };
            let ast = ck.ast(d.file);
            match *ast.node(d.node) {
                Node::FunctionDecl { .. } => true,
                Node::FunctionExpr { .. } | Node::Arrow { .. } => ck
                    .c
                    .parent(d)
                    .is_some_and(|p| matches!(*ast.node(p.node), Node::VarDeclarator { init: Some(i), .. } if i == d.node)),
                _ => false,
            }
        };
        if path.is_empty()
            || path.iter().any(|r| match *r {
                Resolving::Return(s) => !named_function(self, s),
                Resolving::Node(_) => false,
                _ => true,
            })
        {
            return;
        }
        let Some(n) = name else { return };
        let text = self.c.text(Loc::new(l.file, n)).to_owned();
        if text.is_empty() {
            return;
        }
        self.error(
            Loc::new(l.file, n),
            dg::_0_IMPLICITLY_HAS_RETURN_TYPE_ANY_BECAUSE_IT_DOES_NOT_HAVE_A_RETURN_TYPE_ANNOTATI_7023,
            &[&text],
        );
    }

    /// TS7023 at the name of a getter without a return type annotation whose property type,
    /// inferred from its body (no setter annotates it), needs that property type again
    /// (`get x() { return this.x; }`, mutually recursive getters).
    fn report_circular_getter(&mut self, getter: Loc, key: NodeId, value: Loc) {
        let ast = self.ast(getter.file);
        if !matches!(ast.node(key), Node::Ident { .. } | Node::PrivateName { .. }) {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(getter) else {
            return;
        };
        self.type_of_symbol(sym);
        let sig = self.signature_from_decl(value);
        if !self.circular.contains(&Resolving::Type(sym))
            && !self.circular.contains(&Resolving::Return(sig))
        {
            return;
        }
        // Only cycles through accessor and class field types and return types: one through
        // a variable, a parameter, a JavaScript `this.x =` property or a node (an object
        // literal's own type, which tsc builds without its accessors' types) is one tsc
        // doesn't find.
        let mine = |r: &Resolving| *r == Resolving::Type(sym) || *r == Resolving::Return(sig);
        let paths: Vec<Vec<Resolving>> = self
            .type_cycles
            .values()
            .chain(self.return_cycles.values())
            .filter(|p| p.iter().any(mine))
            .cloned()
            .collect();
        let through_members = |ck: &Self, r: &Resolving| match *r {
            Resolving::Return(_) => true,
            Resolving::Type(s) => {
                ck.c.sym_flags(s).contains(SymbolFlags::GET_ACCESSOR)
                    || ck
                        .c
                        .decls(s)
                        .first()
                        .is_some_and(|&d| matches!(ck.node(d), Node::PropertyDef { .. }))
            }
            _ => false,
        };
        if !paths
            .iter()
            .any(|p| p.iter().all(|r| through_members(self, r)))
        {
            return;
        }
        let text = self.c.text(Loc::new(getter.file, key)).to_owned();
        if text.is_empty() {
            return;
        }
        self.error(
            Loc::new(getter.file, key),
            dg::_0_IMPLICITLY_HAS_RETURN_TYPE_ANY_BECAUSE_IT_DOES_NOT_HAVE_A_RETURN_TYPE_ANNOTATI_7023,
            &[&text],
        );
    }

    /// `erasableSyntaxOnly`: TS1294 at syntax with runtime meaning outside ambient
    /// contexts: parameter properties, instantiated namespaces (at each name), enums,
    /// `import x =` aliases, `export =` and `<T>x` assertions (erasableSyntaxOnly).
    fn check_erasable_syntax(&mut self, file: FileId) {
        let f = self.c.file(file);
        if !self.c.opts.erasable_syntax_only || f.kind.is_declaration() || f.is_js() {
            return;
        }
        let ast = self.ast(file);
        let mut at: Vec<NodeId> = Vec::new();
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            let node = ast.node(n);
            if node.modifiers().contains(Modifiers::DECLARE) {
                continue;
            }
            match *node {
                Node::Param { modifiers, .. }
                    if modifiers.intersects(
                        Modifiers::PUBLIC
                            | Modifiers::PRIVATE
                            | Modifiers::PROTECTED
                            | Modifiers::READONLY
                            | Modifiers::OVERRIDE,
                    ) =>
                {
                    at.push(n);
                }
                Node::Module {
                    kind: ts_r_ast::ModuleKind::Namespace | ts_r_ast::ModuleKind::Module,
                    name,
                    body: Some(b),
                    ..
                } if erasable_instantiated(ast, b, 0) => {
                    let mut names = Vec::new();
                    let mut cur = name;
                    while let Node::QualifiedName { left, right } = *ast.node(cur) {
                        names.push(right);
                        cur = left;
                        if names.len() > 10_000 {
                            break;
                        }
                    }
                    if matches!(ast.node(cur), Node::Ident { .. }) {
                        names.push(cur);
                        at.extend(names.iter().rev());
                    }
                }
                Node::Enum { name, .. } => at.push(name),
                Node::ImportEquals { .. } | Node::ExportAssignment { .. } => at.push(n),
                Node::TypeAssertion { .. } => at.push(n),
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        for n in at {
            self.error(
                Loc::new(file, n),
                dg::THIS_SYNTAX_IS_NOT_ALLOWED_WHEN_ERASABLESYNTAXONLY_IS_ENABLED_1294,
                &[],
            );
        }
    }

    /// A `'use strict'` directive in the prologue of a body whose parameters are not
    /// simple (an initializer, a rest or a binding pattern): TS1346 at each such
    /// parameter and TS1347 at the directive, from ES2016 on (tsc 7).
    fn check_use_strict_parameters(&mut self, file: FileId, params: &[NodeId], body: NodeId) {
        if self.c.opts.target < ts_r_config::ScriptTarget::ES2016 {
            return;
        }
        let ast = self.ast(file);
        let Node::Block { stmts } = *ast.node(body) else {
            return;
        };
        let non_simple = |p: NodeId| {
            matches!(*ast.node(p), Node::Param { rest, pattern, init, .. }
                if rest || init.is_some() || !matches!(ast.node(pattern), Node::Ident { .. }))
        };
        if !params.iter().any(|&p| non_simple(p)) {
            return;
        }
        let directive = ast
            .list(stmts)
            .iter()
            .take_while(|&&s| matches!(ast.node(s), Node::Directive { .. }))
            .copied()
            .find(|&s| {
                let t = self.c.text(Loc::new(file, s));
                let t = t.trim_end().trim_end_matches(';').trim_end();
                t == "\"use strict\"" || t == "'use strict'"
            });
        let Some(directive) = directive else {
            return;
        };
        if self.has_syntax_errors(file) {
            return;
        }
        for &p in params {
            if non_simple(p) {
                self.error(
                    Loc::new(file, p),
                    dg::THIS_PARAMETER_IS_NOT_ALLOWED_WITH_USE_STRICT_DIRECTIVE_1346,
                    &[],
                );
            }
        }
        self.error(
            Loc::new(file, directive),
            dg::USE_STRICT_DIRECTIVE_CANNOT_BE_USED_WITH_NON_SIMPLE_PARAMETER_LIST_1347,
            &[],
        );
    }

    /// `allowUnreachableCode: false`: runs of statements the binder found reachable but
    /// whose flow is not (after a call returning `never` or an assertion, an exhaustive
    /// switch): TS7027 over each run, unless inside a run already reported.
    fn check_unreachable_stmts(&mut self, file: FileId, stmts: &[NodeId]) {
        if self.c.opts.allow_unreachable_code != Some(false)
            || stmts.is_empty()
            || self.c.bound(file).flow.stmt_flow.is_empty()
        {
            return;
        }
        let ast = self.ast(file);
        let mut run: Option<Span> = None;
        let mut runs = Vec::new();
        for &s in stmts {
            let dead = self
                .c
                .bound(file)
                .flow
                .stmt_flow_at(s)
                .is_some_and(|f| !f.is_unreachable() && !self.is_flow_reachable(file, f));
            if dead {
                let sp = ast.span(s);
                run = Some(run.map_or(sp, |r: Span| r.cover(sp)));
            } else if let Some(r) = run.take() {
                runs.push(r);
            }
        }
        runs.extend(run);
        for r in runs {
            let inside = self
                .unreachable_runs
                .iter()
                .any(|&(f, o)| f == file && o.start <= r.start && r.end <= o.end);
            if !inside {
                self.unreachable_runs.push((file, r));
                self.error_span(file, r, dg::UNREACHABLE_CODE_DETECTED_7027, &[]);
            }
        }
    }

    /// `noFallthroughCasesInSwitch`: a non-empty clause (not the last) whose end is
    /// reachable runs into the next one: TS7029 at the clause.
    fn check_case_fallthrough(&mut self, c: Loc) {
        if !self.c.opts.no_fallthrough_cases_in_switch {
            return;
        }
        let flow = &self.c.bound(c.file).flow;
        let Some(end) = flow
            .fallthroughs
            .iter()
            .find(|&&(n, _)| n == c.node)
            .map(|&(_, f)| f)
        else {
            return;
        };
        if self.is_flow_reachable(c.file, end) {
            self.error(c, dg::FALLTHROUGH_CASE_IN_SWITCH_7029, &[]);
        }
    }

    /// Whether the end of function `l`'s body can be reached.
    fn function_end_reachable(&mut self, l: Loc) -> bool {
        match self.c.bound(l.file).flow.function_flow(l.node) {
            Some(ff) => {
                let end = ff.end;
                !end.is_unreachable() && self.is_flow_reachable(l.file, end)
            }
            None => false,
        }
    }

    /// TS2378: a getter whose body has no `return` statement and whose end is reachable,
    /// at its name (observed: `return;` or a `throw`/endless loop at the end avoids it; an
    /// annotation does not).
    fn check_getter_returns(&mut self, l: Loc, parts: crate::nodes::FnParts) {
        let file = l.file;
        let ast = self.ast(file);
        let Some(body) = parts.body else { return };
        let Some(Node::MethodDef {
            kind: ts_r_ast::MethodKind::Get,
            key,
            ..
        }) = self.c.parent(l).map(|p| *ast.node(p.node))
        else {
            return;
        };
        let mut returns = Vec::new();
        crate::func::collect_returns(ast, body, &mut returns, 0);
        if returns.is_empty() && self.function_end_reachable(l) {
            self.error(
                Loc::new(file, key),
                dg::A_GET_ACCESSOR_MUST_RETURN_A_VALUE_2378,
                &[],
            );
        }
    }

    /// A generator's annotation must accept the generator it describes: a
    /// `Generator<Y, R, N>` (or `AsyncGenerator`) built from the annotation's own iteration
    /// types (`any`, `any`, `unknown` when it has none), TS2322/TS2741 at the annotation;
    /// `void` is TS2505.
    fn check_generator_annotation(&mut self, l: Loc, flags: ts_r_ast::FnFlags, rl: Loc) {
        use crate::checker::GlobalType as G;
        let ann = self.type_from_node(rl);
        let f = self.store().flags(ann);
        if f.intersects(TypeFlags::ANY) || self.store().is_error(ann) || ann == TypeId::UNRESOLVED {
            return;
        }
        if f == TypeFlags::VOID {
            self.error(
                rl,
                dg::A_GENERATOR_CANNOT_HAVE_A_VOID_TYPE_ANNOTATION_2505,
                &[],
            );
            return;
        }
        // Parse recovery can make annotations tsc doesn't see (`{ async * x: 1 }`).
        let sf = self.c.file(rl.file);
        if sf.syntax.len() > sf.grammar.len() {
            return;
        }
        let is_async = flags.contains(ts_r_ast::FnFlags::ASYNC);
        // An async generator reads iteration types through `next()`: a sync `Iterable`
        // (no `next`) gives none.
        let iterable = self.global_type(G::Iterable);
        let sync_iterable_only = is_async
            && self.union_members(ann).into_iter().all(|m| {
                self.generic_args(m)
                    .is_some_and(|(target, _)| Some(target) == iterable)
            });
        let unresolved_any = |t: TypeId| {
            if t == TypeId::UNRESOLVED {
                TypeId::ANY
            } else {
                t
            }
        };
        let ctx = if sync_iterable_only {
            None
        } else {
            self.quietly(|ck| ck.generator_context_full(l))
        };
        let (y, r, n) = match ctx {
            Some((y, r, n)) => (unresolved_any(y), unresolved_any(r), unresolved_any(n)),
            None if sync_iterable_only => (TypeId::ANY, TypeId::ANY, TypeId::UNKNOWN),
            None => match self.quietly(|ck| ck.iterated_type(ann, is_async)) {
                Some(y) => (y, TypeId::ANY, TypeId::ANY),
                None => (TypeId::ANY, TypeId::ANY, TypeId::UNKNOWN),
            },
        };
        let g = if is_async {
            G::AsyncGenerator
        } else {
            G::Generator
        };
        let Some(gt) = self.global_reference(g, &[y, r, n]) else {
            return;
        };
        self.check_assignable_with_elaboration(gt, ann, rl, None);
    }

    fn check_function_returns(&mut self, l: Loc, parts: crate::nodes::FnParts) {
        let file = l.file;
        let ast = self.ast(file);
        let Some(rt) = parts.return_type else {
            // A getter without annotation returns its setter's annotated parameter type,
            // reported at the getter's name.
            if parts.body.is_some()
                && let Some(t) = self.unannotated_getter_setter_type(l)
                && let Some(m) = self.c.parent(l)
                && let Node::MethodDef { key, .. } = *ast.node(m.node)
            {
                self.check_returns_against(l, parts, Loc::new(file, key), t);
                return;
            }
            self.check_jsdoc_typed_function_returns(l, parts);
            return;
        };
        if parts.flags.contains(ts_r_ast::FnFlags::GENERATOR) {
            if parts.body.is_some() {
                self.check_generator_annotation(l, parts.flags, Loc::new(file, rt));
            }
            return;
        }
        if parts.body.is_none() {
            return;
        }
        // A constructor's or setter's annotation is a grammar error (TS1093/TS1095), not a
        // return type (parserConstructorDeclaration10).
        if let Some(p) = self.c.parent(l)
            && matches!(
                *ast.node(p.node),
                Node::MethodDef {
                    kind: ts_r_ast::MethodKind::Constructor | ts_r_ast::MethodKind::Set,
                    ..
                }
            )
        {
            return;
        }
        let sig = self.signature_from_decl(l);
        let Some(declared) = self.declared_return_of(sig) else {
            return;
        };
        if matches!(ast.node(rt), Node::TypePredicate { asserts: true, .. }) {
            return;
        }
        if parts.flags.contains(ts_r_ast::FnFlags::ASYNC)
            && !self.is_global_promise_reference(declared)
        {
            // TS1064: an async function's annotation must be `Promise<T>` (a subclass or
            // `PromiseLike` is not), with the awaited type as the suggestion. The checks
            // below still run (asyncFunctionDeclaration15_es6: TS2355 for `(): number {}`).
            let awaited = self.awaited_type(declared);
            let text = self.type_to_string(awaited);
            self.error(
                Loc::new(file, rt),
                dg::THE_RETURN_TYPE_OF_AN_ASYNC_FUNCTION_OR_METHOD_MUST_BE_THE_GLOBAL_PROMISE_T_TYPE_1064,
                &[&text],
            );
        }
        self.check_returns_against(l, parts, Loc::new(file, rt), declared);
    }

    /// A JavaScript function typed by a JSDoc `@type {F}` with a single call signature:
    /// `F`'s return type is its declared one, and a missing return is reported at `F`
    /// (tsc: tsc/internal/checker/checker.go, checkAllCodePathsInNonVoidFunctionReturnOrThrow
    /// with the tag as the full signature).
    fn check_jsdoc_typed_function_returns(&mut self, l: Loc, parts: crate::nodes::FnParts) {
        if parts.body.is_none() || parts.flags.contains(ts_r_ast::FnFlags::GENERATOR) {
            return;
        }
        let Some(ty) = self.jsdoc_function_type_tag(l) else {
            return;
        };
        let t = self.type_from_node(Loc::new(l.file, ty));
        if t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return;
        }
        let [sig] = self.signatures_of(t, crate::members::SigKind::Call)[..] else {
            return;
        };
        if !self
            .signatures_of(t, crate::members::SigKind::Construct)
            .is_empty()
        {
            return;
        }
        let declared = self.return_type_of(sig);
        self.check_returns_against(l, parts, Loc::new(l.file, ty), declared);
    }

    /// The missing-return checks of a function with declared return type `declared`,
    /// reported at `rl`.
    fn check_returns_against(
        &mut self,
        l: Loc,
        parts: crate::nodes::FnParts,
        rl: Loc,
        declared: TypeId,
    ) {
        let file = l.file;
        let ast = self.ast(file);
        let Some(body) = parts.body else { return };
        let target = if parts.flags.contains(ts_r_ast::FnFlags::ASYNC) {
            self.awaited_type(declared)
        } else {
            declared
        };
        let tf = self.store().flags(target);
        if tf.intersects(TypeFlags::ANY | TypeFlags::VOID)
            || self.store().is_error(target)
            || target == TypeId::UNRESOLVED
        {
            return;
        }
        if !self.function_end_reachable(l) {
            return;
        }
        let mut returns = Vec::new();
        crate::func::collect_returns(ast, body, &mut returns, 0);
        if target == TypeId::NEVER {
            self.error(
                rl,
                dg::A_FUNCTION_RETURNING_NEVER_CANNOT_HAVE_A_REACHABLE_END_POINT_2534,
                &[],
            );
            return;
        }
        // `unknown` takes a missing value at the end, but not a body without any `return`
        // (observed: TS2355 for `(): unknown => {}`). Under `noImplicitReturns` a type that
        // takes `undefined` but is not exempt from it needs a return too.
        let unknown = tf.intersects(TypeFlags::UNKNOWN);
        let accepts_undefined = unknown || self.type_accepts_undefined(target);
        let implicit_returns =
            self.c.opts.no_implicit_returns && !self.exempt_from_implicit_returns(target);
        if returns.is_empty() {
            if unknown || !accepts_undefined || implicit_returns {
                self.error(rl, dg::A_FUNCTION_WHOSE_DECLARED_TYPE_IS_NEITHER_UNDEFINED_VOID_NOR_ANY_MUST_RETURN_A_V_2355, &[]);
            }
        } else if self.c.opts.strict_null_checks && !accepts_undefined {
            self.error(rl, dg::FUNCTION_LACKS_ENDING_RETURN_STATEMENT_AND_RETURN_TYPE_DOES_NOT_INCLUDE_UNDEFINE_2366, &[]);
        } else if implicit_returns {
            self.error(rl, dg::NOT_ALL_CODE_PATHS_RETURN_A_VALUE_7030, &[]);
        }
    }

    /// `noImplicitReturns` exempts return types `any`, exactly `undefined`, and those
    /// including `void` (`number | void`).
    fn exempt_from_implicit_returns(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::ANY | TypeFlags::VOID) || f == TypeFlags::UNDEFINED {
            return true;
        }
        f.contains(TypeFlags::UNION)
            && s.members_of_union(t)
                .iter()
                .any(|&m| s.flags(m).intersects(TypeFlags::ANY | TypeFlags::VOID))
    }

    /// TS7030 under `noImplicitReturns` for a function without a return type annotation
    /// whose end is reachable while its inferred return type (awaited, or a generator's
    /// return type) is not exempt; at its name, the name it is assigned to, or its start.
    fn check_inferred_implicit_returns(&mut self, l: Loc, parts: crate::nodes::FnParts) {
        if !self.c.opts.no_implicit_returns
            || parts.return_type.is_some()
            || parts.body.is_none()
            || !self.reporting(l.file)
            || self.jsdoc_declared_signature(l).is_some()
        {
            return;
        }
        let file = l.file;
        let ast = self.ast(file);
        let parent = self.c.parent(l).map(|p| *ast.node(p.node));
        if let Some(Node::MethodDef {
            kind: MethodKind::Constructor | MethodKind::Set,
            ..
        }) = parent
        {
            return;
        }
        if !self.function_end_reachable(l) {
            return;
        }
        let sig = self.signature_from_decl(l);
        let r = self.return_type_of(sig);
        let r = if parts.flags.contains(ts_r_ast::FnFlags::GENERATOR) {
            match self.c.type_args_of(r).get(1) {
                Some(&t) => t,
                None => return,
            }
        } else if parts.flags.contains(ts_r_ast::FnFlags::ASYNC) {
            self.awaited_type(r)
        } else {
            r
        };
        if self.exempt_from_implicit_returns(r)
            || self.store().is_error(r)
            || r == TypeId::UNRESOLVED
        {
            return;
        }
        let at = match *ast.node(l.node) {
            Node::FunctionDecl { name: Some(n), .. } | Node::FunctionExpr { name: Some(n), .. } => {
                n
            }
            Node::FunctionExpr { .. } => match parent {
                Some(Node::MethodDef { key, .. }) => key,
                Some(Node::VarDeclarator {
                    name,
                    init: Some(i),
                    ..
                }) if i == l.node && matches!(ast.node(name), Node::Ident { .. }) => name,
                Some(Node::Property { key, value, .. }) if value == l.node => key,
                Some(Node::Assign { left, right, .. }) if right == l.node => left,
                _ => l.node,
            },
            _ => l.node,
        };
        self.error(
            Loc::new(file, at),
            dg::NOT_ALL_CODE_PATHS_RETURN_A_VALUE_7030,
            &[],
        );
    }

    /// Whether `t` is `Promise<T>` for the global `Promise` (true when there is none, or
    /// for an error type: nothing to report then).
    fn is_global_promise_reference(&mut self, t: TypeId) -> bool {
        if self.store().is_error(t) || t == TypeId::UNRESOLVED {
            return true;
        }
        let Some(promise) = self.global_type(crate::checker::GlobalType::Promise) else {
            return true;
        };
        self.generic_args(t)
            .is_some_and(|(target, _)| target == promise)
    }

    fn type_accepts_undefined(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let check = |m: TypeId| {
            s.flags(m)
                .intersects(TypeFlags::UNDEFINED | TypeFlags::VOID | TypeFlags::ANY_OR_UNKNOWN)
        };
        if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).iter().any(|&m| check(m))
        } else {
            check(t)
        }
    }

    /// The order and spread checks of a tuple type's elements, stopping at the first error
    /// (tsc: `checker.go`, checkTupleType). `...T` whose `T` is neither an array or tuple
    /// nor assignable to `readonly any[]` is TS2574; `...X[]` and a spread of an array or
    /// of a tuple with a rest element count as rest elements, any other spread as neither.
    /// A rest after a rest is TS1265 and an optional element after a rest TS1266 (grammar
    /// errors, not in files with syntax errors); a required element after an optional one
    /// (TS1257) is the parser's.
    fn check_tuple_rest_elements(&mut self, file: FileId, elems: ts_r_ast::NodeList) {
        let sf = self.c.file(file);
        let grammar_ok = sf.syntax.len() <= sf.grammar.len();
        let ast = self.ast(file);
        let (mut seen_optional, mut seen_rest) = (false, false);
        for e in ast.list(elems).to_vec() {
            let ast = self.ast(file);
            let (optional, spread) = match *ast.node(e) {
                Node::OptionalType { .. } => (true, None),
                Node::RestType { ty } | Node::NamedTupleMember { rest: true, ty, .. } => {
                    (false, Some(ty))
                }
                Node::NamedTupleMember { optional, .. } => (optional, None),
                _ => (false, None),
            };
            let rest = match spread {
                None => false,
                Some(ty) if spread_array_element_node(ast, ty, 0) => true,
                // `...infer R` takes an array constraint from its position.
                Some(ty) if is_infer_type_node(ast, ty) => false,
                Some(ty) => {
                    let t = self.type_from_node(Loc::new(file, ty));
                    let s = self.store();
                    if t == TypeId::UNRESOLVED
                        || s.is_error(t)
                        || s.flags(t).intersects(TypeFlags::ANY)
                        || self.is_infer_type_param(t)
                    {
                        false
                    } else if self.c.is_array_type(t) {
                        true
                    } else if let Some(sh) = s.tuple_shape_of(t) {
                        sh.has_rest()
                    } else {
                        let nullable = s
                            .flags(t)
                            .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED | TypeFlags::VOID);
                        let fits = !nullable
                            && self
                                .global_reference(
                                    crate::checker::GlobalType::ReadonlyArray,
                                    &[TypeId::ANY],
                                )
                                .is_some_and(|ro| self.is_assignable(t, ro));
                        if !fits {
                            self.error(
                                Loc::new(file, e),
                                dg::A_REST_ELEMENT_TYPE_MUST_BE_AN_ARRAY_TYPE_2574,
                                &[],
                            );
                            return;
                        }
                        false
                    }
                }
            };
            if rest {
                if seen_rest {
                    if grammar_ok {
                        self.error(
                            Loc::new(file, e),
                            dg::A_REST_ELEMENT_CANNOT_FOLLOW_ANOTHER_REST_ELEMENT_1265,
                            &[],
                        );
                    }
                    return;
                }
                seen_rest = true;
            } else if optional {
                if seen_rest {
                    if grammar_ok {
                        self.error(
                            Loc::new(file, e),
                            dg::AN_OPTIONAL_ELEMENT_CANNOT_FOLLOW_A_REST_ELEMENT_1266,
                            &[],
                        );
                    }
                    return;
                }
                seen_optional = true;
            } else if spread.is_none() && seen_optional {
                // TS1257, reported by the parser.
                return;
            }
        }
    }

    /// A rest parameter's type must fit `readonly any[]` (a binding pattern there is a
    /// grammar error already; tsc: checkParameter). Signatures without a body too.
    fn check_rest_param_type(&mut self, p: Loc) {
        let ast = self.ast(p.file);
        let Node::Param {
            pattern,
            type_ann: Some(ann),
            rest: true,
            ..
        } = *ast.node(p.node)
        else {
            return;
        };
        if !matches!(ast.node(pattern), Node::Ident { .. }) || is_infer_type_node(ast, ann) {
            return;
        }
        let Some(ro) =
            self.global_reference(crate::checker::GlobalType::ReadonlyArray, &[TypeId::ANY])
        else {
            return;
        };
        let t = self.param_type(p);
        if t != TypeId::UNRESOLVED
            && !self.store().is_error(t)
            && !self.is_infer_type_param(t)
            && !self.is_assignable(t, ro)
        {
            self.error(p, dg::A_REST_PARAMETER_MUST_BE_OF_AN_ARRAY_TYPE_2370, &[]);
        }
    }

    fn check_param(&mut self, p: Loc, in_expr_fn: bool) {
        let file = p.file;
        let ast = self.ast(file);
        let Node::Param {
            pattern,
            type_ann,
            init,
            rest,
            ..
        } = *ast.node(p.node)
        else {
            return;
        };
        if is_this_param(ast, p.node) {
            return;
        }
        self.check_rest_param_type(p);
        self.check_binding_pattern_iterables(p);
        if let Some(i) = init {
            let il = Loc::new(file, i);
            let it = self.check_expr(il);
            if type_ann.is_some() {
                let declared = self.param_type(p);
                let declared = if self.c.opts.strict_null_checks {
                    self.declared_without_optionality(declared, p)
                } else {
                    declared
                };
                self.check_assignable_with_elaboration(it, declared, p, Some(il));
            } else if in_expr_fn
                && matches!(ast.node(pattern), Node::Ident { .. })
                && let Some(f) = self.c.parent(p)
                && self.contextual_signature(f).is_some()
                && !self.in_unresolved_context(f)
                && let Some(ctx) = self.contextual_param_type(p)
                && ctx != TypeId::UNRESOLVED
                && ctx != TypeId::NEVER
                && {
                    let base = self.base_of_literal(ctx);
                    !self.is_assignable(it, base)
                }
            {
                // A contextually typed parameter's default must fit the contextual type
                // with its literals widened (`f: (a: number) => void = (a = "") => {}` is an
                // error, `(a: 1 | 2) => void = (a = 3) => {}` is not; observed, tsc 7).
                self.check_assignable_with_elaboration(it, ctx, Loc::new(file, pattern), Some(il));
            }
        }
        if !matches!(ast.node(pattern), Node::Ident { .. }) {
            if !matches!(ast.node(p.node), Node::Param { optional: true, .. }) {
                self.check_empty_binding_pattern(Loc::new(file, pattern), false);
            } else if init.is_none()
                && self.c.parent(p).is_some_and(|f| {
                    matches!(
                        *ast.node(f.node),
                        Node::FunctionDecl { body: Some(_), .. }
                            | Node::FunctionExpr { body: Some(_), .. }
                            | Node::Arrow { .. }
                    )
                })
            {
                // `function f({ a }?) {}`, or JSDoc `@param [o]` on a pattern: an optional
                // pattern needs a default in a function with a body (tsc: checkParameter).
                self.error(
                    p,
                    dg::A_BINDING_PATTERN_PARAMETER_CANNOT_BE_OPTIONAL_IN_AN_IMPLEMENTATION_SIGNATURE_2463,
                    &[],
                );
            }
            self.check_binding_pattern_defaults(Loc::new(file, pattern));
            if type_ann.is_none()
                && let Some(i) = init
            {
                self.check_pattern_default_excess(Loc::new(file, pattern), Loc::new(file, i));
            }
        }
        if self.c.opts.no_implicit_any
            && type_ann.is_none()
            && let Some(i) = init
            && !self.c.file(file).is_js()
            && !(in_expr_fn && self.contextual_param_type(p).is_some())
        {
            self.report_padded_binding_elements(Loc::new(file, pattern), Loc::new(file, i));
        }
        if self.c.opts.no_implicit_any && type_ann.is_none() && init.is_none() {
            if in_expr_fn && self.contextual_param_type(p).is_some() {
                return;
            }
            // Private methods of ambient classes have no implicit-any errors.
            if let Some(f) = self.c.parent(p)
                && let Some(m) = self.c.parent(f)
                && let Node::MethodDef { modifiers, .. } = *ast.node(m.node)
                && modifiers.contains(ts_r_ast::Modifiers::PRIVATE)
                && self.is_ambient_node(m)
            {
                return;
            }
            // JSDoc types are not read yet.
            if self.c.file(file).is_js() && !matches!(ast.node(pattern), Node::Ident { .. }) {
                return;
            }
            if in_expr_fn
                && let Some(f) = self.c.parent(p)
                && self.in_unresolved_context(f)
            {
                return;
            }
            let t = self.param_type(p);
            let implicit = t == TypeId::IMPLICIT_ANY
                || (rest && self.element_type_of_array_like(t) == Some(TypeId::IMPLICIT_ANY));
            // A name the parser could not read prints as tsc's missing identifier.
            // (A `,` or `)` where a parameter should start is no parameter in tsc; `...`
            // starts one.)
            let name = match ast.node(pattern) {
                Node::Missing {} => {
                    let after = &self.c.file(file).text[ast.span(pattern).start as usize..];
                    let next = after.trim_start().chars().next();
                    (rest || !matches!(next, Some(',' | ')') | None)).then_some("(Missing)")
                }
                _ => ident_text(ast, pattern).map(|n| if n.is_empty() { "(Missing)" } else { n }),
            };
            if implicit
                && let Some(name) = name
                && !self.has_jsdoc_type(p)
            {
                if rest {
                    self.error(
                        p,
                        dg::REST_PARAMETER_0_IMPLICITLY_HAS_AN_ANY_TYPE_7019,
                        &[name],
                    );
                } else {
                    self.error(
                        p,
                        dg::PARAMETER_0_IMPLICITLY_HAS_AN_1_TYPE_7006,
                        &[name, "any"],
                    );
                }
            }
            if !matches!(ast.node(pattern), Node::Ident { .. }) {
                self.report_implicit_binding_elements(Loc::new(file, pattern), false);
            }
        }
    }

    /// In a JS file, whether an untyped parameter is typed by JSDoc after all: its function
    /// has a `@type` (the contextual signature), or `@overload` comments precede it (not
    /// modelled). `@param` types are annotations already.
    fn has_jsdoc_type(&mut self, p: Loc) -> bool {
        if !self.c.file(p.file).is_js() {
            return false;
        }
        // `/** @type {F} */ function f(a) {}`: `a` is typed by `F`, unless `F` has no call
        // signature (`Function`; tsc 7 reports TS7006 then).
        if let Some(f) = self.c.parent(p)
            && let Some(ty) = self.jsdoc_function_type_tag(f)
        {
            let t = self.type_from_node(Loc::new(f.file, ty));
            return t == TypeId::UNRESOLVED
                || self.store().is_error(t)
                || self.store().flags(t).intersects(TypeFlags::ANY)
                || !self
                    .signatures_of(t, crate::members::SigKind::Call)
                    .is_empty();
        }
        // `@overload` comments before the implementation (not modelled).
        let start = self.c.span(p).start as usize;
        let src = &self.c.file(p.file).text;
        let mut start = start.min(src.len());
        while !src.is_char_boundary(start) {
            start -= 1;
        }
        let before = &src[..start];
        // The comments before the enclosing statement: look back a bounded distance.
        let mut from = before.len().saturating_sub(4000);
        while !before.is_char_boundary(from) {
            from += 1;
        }
        let window = &before[from..];
        let Some(end) = window.rfind("*/") else {
            return false;
        };
        let Some(open) = window[..end].rfind("/**") else {
            return false;
        };
        window[open..end].contains("@overload")
    }

    /// `({ a } = { a: 1, z: 2 })`, `var { a } = { a: 1, z: 2 }`: an object literal
    /// initializing an object pattern may only name properties the pattern binds (TS2353
    /// against the pattern's type), unless the pattern has a rest. Nested patterns check
    /// their nested literals; every excess property is reported (spreads are skipped, a
    /// computed name of a non-literal type prints as `[expr]`).
    fn check_pattern_default_excess(&mut self, pattern: Loc, init: Loc) {
        self.pattern_literal_excess(pattern, init, 0);
    }

    fn pattern_literal_excess(&mut self, pattern: Loc, init: Loc, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        let ast = self.ast(pattern.file);
        let mut lit = skip_parens(ast, init.node);
        // `{ ... } as const` is still a fresh literal.
        if let Node::As { expr, ty } = *ast.node(lit)
            && matches!(
                ast.node(ty),
                Node::KeywordType {
                    kind: ts_r_ast::KeywordTypeKind::Const
                }
            )
        {
            lit = skip_parens(ast, expr);
        }
        match (*ast.node(pattern.node), *ast.node(lit)) {
            (Node::ObjectPattern { props: pprops }, Node::ObjectLit { props }) => {
                // An empty pattern accepts anything.
                if ast.list(pprops).is_empty() {
                    return false;
                }
                let mut bound: Vec<(String, Option<NodeId>)> = Vec::new();
                for &p in ast.list(pprops) {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, .. } => {
                            match self.binding_key_name(Loc::new(pattern.file, key)) {
                                Some(k) => bound.push((k, value)),
                                None => return false,
                            }
                        }
                        _ => return false,
                    }
                }
                let mut nested = Vec::new();
                let mut reported = false;
                for &p in ast.list(props) {
                    let (key, value) = match *ast.node(p) {
                        Node::Property { key, value, .. } => (key, Some(value)),
                        Node::MethodDef { key, .. } => (key, None),
                        Node::ShorthandProp { name, .. } => (name, None),
                        Node::Spread { .. } => continue,
                        _ => return false,
                    };
                    let k = match key_text(ast, key) {
                        Some(k) => k,
                        None => match self.computed_key_display(Loc::new(pattern.file, key)) {
                            Some(k) => k,
                            None => continue,
                        },
                    };
                    match bound.iter().find(|b| b.0 == k) {
                        None => {
                            let pt = self.type_from_binding_pattern(pattern);
                            let ts = self.type_to_string(pt);
                            self.error(
                                Loc::new(pattern.file, key),
                                dg::OBJECT_LITERAL_MAY_ONLY_SPECIFY_KNOWN_PROPERTIES_AND_0_DOES_NOT_EXIST_IN_TYPE_1_2353,
                                &[&k, &ts],
                            );
                            reported = true;
                        }
                        Some(&(_, Some(pv))) => {
                            if let Some(v) = value
                                && matches!(
                                    ast.node(pv),
                                    Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                                )
                            {
                                nested.push((pv, v));
                            }
                        }
                        Some(_) => {}
                    }
                }
                for (pv, v) in nested {
                    reported |= self.pattern_literal_excess(
                        Loc::new(pattern.file, pv),
                        Loc::new(pattern.file, v),
                        depth + 1,
                    );
                }
                reported
            }
            (Node::ArrayPattern { elements: pels }, Node::ArrayLit { elements }) => {
                let pels = ast.list(pels).to_vec();
                let els = ast.list(elements).to_vec();
                for (pe, e) in pels.into_iter().zip(els) {
                    if matches!(ast.node(e), Node::Spread { .. }) {
                        return false;
                    }
                    if matches!(
                        ast.node(pe),
                        Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                    ) && self.pattern_literal_excess(
                        Loc::new(pattern.file, pe),
                        Loc::new(pattern.file, e),
                        depth + 1,
                    ) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// How a computed property name prints in a diagnostic: its literal value, or
    /// `[expr]` for a name of a non-literal type (`[k]` with `k: any`). `None` for a
    /// unique symbol or an unreadable name.
    fn computed_key_display(&mut self, key: Loc) -> Option<String> {
        let ast = self.ast(key.file);
        let Node::ComputedKey { expr } = *ast.node(key.node) else {
            return None;
        };
        let t = self.quietly(|ck| ck.check_expr(Loc::new(key.file, expr)));
        let s = self.store();
        if s.flags(t).intersects(TypeFlags::ES_SYMBOL_LIKE) {
            return None;
        }
        if let Some(name) = self.property_name_of_type(t) {
            return Some(name);
        }
        let sp = ast.span(expr);
        let text = self
            .c
            .file(key.file)
            .text
            .get(sp.start as usize..sp.end as usize)?;
        Some(format!("[{text}]"))
    }

    /// `({ x } = { x: 0, y: 0 })`: an object literal assigned to an object assignment
    /// pattern is contextually typed by the pattern's type, so each property the pattern
    /// doesn't name is TS2353 against that type (every one, an empty pattern included).
    /// Patterns with a spread or a computed name accept anything; nested pattern/literal
    /// pairs are checked the same way.
    pub(crate) fn check_assignment_pattern_excess(&mut self, pattern: Loc, value: Loc, depth: u32) {
        if depth > 32 {
            return;
        }
        let file = pattern.file;
        let ast = self.ast(file);
        let (Node::ObjectLit { props: pprops }, Node::ObjectLit { props }) = (
            *ast.node(skip_parens(ast, pattern.node)),
            *ast.node(skip_parens(ast, value.node)),
        ) else {
            return;
        };
        let mut bound: Vec<(String, Option<NodeId>)> = Vec::new();
        for &p in ast.list(pprops) {
            let (key, target) = match *ast.node(p) {
                Node::Property { key, value, .. } => (key, Some(value)),
                Node::ShorthandProp { name, .. } => (name, None),
                _ => return,
            };
            if matches!(ast.node(key), Node::ComputedKey { .. }) {
                return;
            }
            let Some(k) = key_text(ast, key) else {
                return;
            };
            bound.push((k, target));
        }
        let mut pattern_type = None;
        let mut nested = Vec::new();
        for &m in ast.list(props) {
            let (key, v) = match *ast.node(m) {
                Node::Property { key, value, .. } => (key, Some(value)),
                Node::ShorthandProp { name, .. } => (name, None),
                Node::MethodDef { key, .. } => (key, None),
                _ => continue,
            };
            let Some(k) = key_text(ast, key) else {
                continue;
            };
            match bound.iter().find(|b| b.0 == k) {
                None => {
                    let pt = *pattern_type.get_or_insert_with(|| {
                        let t = self.quietly(|ck| ck.check_expr(pattern));
                        self.regular_of(t)
                    });
                    if pt == TypeId::UNRESOLVED || self.store().is_error(pt) {
                        return;
                    }
                    let ts = self.type_to_string(pt);
                    self.error(
                        Loc::new(file, key),
                        dg::OBJECT_LITERAL_MAY_ONLY_SPECIFY_KNOWN_PROPERTIES_AND_0_DOES_NOT_EXIST_IN_TYPE_1_2353,
                        &[&k, &ts],
                    );
                }
                Some(&(_, Some(target))) => {
                    if let Some(v) = v {
                        nested.push((target, v));
                    }
                }
                Some(_) => {}
            }
        }
        for (target, v) in nested {
            let target = match *ast.node(skip_parens(ast, target)) {
                Node::Assign { left, .. } => left,
                _ => target,
            };
            self.check_assignment_pattern_excess(
                Loc::new(file, target),
                Loc::new(file, v),
                depth + 1,
            );
        }
    }

    /// TS2700 at `arg` when the rest element `rest` of an object binding pattern takes the
    /// rest of a type that is not an object (tsc: tsc/internal/checker/checker.go,
    /// getTypeForBindingElement and isValidSpreadType).
    fn check_object_rest_source(&mut self, file: FileId, rest: NodeId, arg: NodeId) {
        let Some(pp) = self.c.parent(Loc::new(file, rest)) else {
            return;
        };
        if !matches!(self.ast(file).node(pp.node), Node::ObjectPattern { .. })
            || !self.reporting(file)
        {
            return;
        }
        let t = self.pattern_parent_type(pp);
        let t = self.reduced_type(t);
        if t != TypeId::UNKNOWN && self.is_valid_rest_source(t, 0) {
            return;
        }
        self.error(
            Loc::new(file, arg),
            dg::REST_TYPES_MAY_ONLY_BE_CREATED_FROM_OBJECT_TYPES_2700,
            &[],
        );
    }

    /// A type a rest can be taken from: after taking base constraints and dropping the
    /// always-falsy members, `any`, an object, `object`, a generic object, or a union or
    /// intersection of those.
    fn is_valid_rest_source(&mut self, t: TypeId, depth: u32) -> bool {
        if depth > 8 || t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return true;
        }
        let t = self.map_type(t, |ck, m| {
            if !ck.store().flags(m).intersects(TypeFlags::INSTANTIABLE) {
                return m;
            }
            match ck.base_constraint(m) {
                TypeId::UNKNOWN | TypeId::UNRESOLVED => m,
                c => c,
            }
        });
        let t = self.remove_definitely_falsy(t);
        let f = self.store().flags(t);
        if f.intersects(
            TypeFlags::ANY
                | TypeFlags::NON_PRIMITIVE
                | TypeFlags::OBJECT
                | TypeFlags::INSTANTIABLE_NON_PRIMITIVE,
        ) {
            return true;
        }
        if !f.intersects(TypeFlags::UNION_OR_INTERSECTION) {
            return false;
        }
        let members = self.store().members_of_union(t).to_vec();
        members
            .into_iter()
            .all(|m| self.is_valid_rest_source(m, depth + 1))
    }

    /// TS7031 on the names of an array pattern past the elements of its default's tuple
    /// type (`([x, y] = [1])`: `y`), unless they have a default of their own.
    fn report_padded_binding_elements(&mut self, pattern: Loc, init: Loc) {
        let ast = self.ast(pattern.file);
        let Node::ArrayPattern { elements } = *ast.node(pattern.node) else {
            return;
        };
        let t = self.quietly(|ck| ck.check_expr(init));
        let Some(shape) = self.store().tuple_shape_of(t) else {
            return;
        };
        if shape.has_rest() {
            return;
        }
        let n = shape.elements.len();
        let pels = ast.list(elements).to_vec();
        if pels
            .iter()
            .any(|&e| matches!(ast.node(e), Node::Rest { .. }))
        {
            return;
        }
        for &e in pels.iter().skip(n) {
            if let Some(name) = ident_text(ast, e) {
                self.error(
                    Loc::new(pattern.file, e),
                    dg::BINDING_ELEMENT_0_IMPLICITLY_HAS_AN_1_TYPE_7031,
                    &[name, "any"],
                );
            }
        }
    }

    /// TS7031 on each name of a parameter's binding pattern that is implicitly `any`
    /// (`pattern_typed`: a variable's pattern without annotation or initializer, whose
    /// names are all `any` from the pattern).
    /// The names of a binding pattern whose type can only come from the pattern itself
    /// (no default anywhere on the way down).
    fn untyped_binding_names(ast: &ts_r_ast::Ast, pattern: NodeId) -> Vec<NodeId> {
        let mut names: Vec<NodeId> = Vec::new();
        let mut stack = vec![pattern];
        while let Some(n) = stack.pop() {
            match *ast.node(n) {
                // An array pattern's rest is `any[]` from the pattern (reported as `any`).
                Node::Rest { arg } if matches!(ast.node(arg), Node::Ident { .. }) => {
                    names.push(arg);
                }
                Node::ObjectPattern { props } => {
                    for &p in ast.list(props).iter().rev() {
                        if let Node::BindingProperty { key, value, init } = *ast.node(p) {
                            if init.is_some() {
                                continue;
                            }
                            match value {
                                Some(v) => stack.push(v),
                                None => names.push(key),
                            }
                        }
                    }
                }
                Node::ArrayPattern { elements } => {
                    for &e in ast.list(elements).iter().rev() {
                        stack.push(e);
                    }
                }
                Node::Ident { .. } => names.push(n),
                _ => {}
            }
        }
        names
    }

    /// TS7031 on the bindings of a parameter pattern in a signature without a body
    /// (`m([a, b]): void` in an interface): the bindings declare nothing, so each one
    /// without a default is reported as `any`.
    pub(crate) fn report_signature_binding_elements(&mut self, pattern: Loc) {
        let ast = self.ast(pattern.file);
        for n in Self::untyped_binding_names(ast, pattern.node) {
            if let Some(name) = ident_text(ast, n) {
                self.error(
                    Loc::new(pattern.file, n),
                    dg::BINDING_ELEMENT_0_IMPLICITLY_HAS_AN_1_TYPE_7031,
                    &[name, "any"],
                );
            }
        }
    }

    fn report_implicit_binding_elements(&mut self, pattern: Loc, pattern_typed: bool) {
        let ast = self.ast(pattern.file);
        for n in Self::untyped_binding_names(ast, pattern.node) {
            let l = Loc::new(pattern.file, n);
            let Some(sym) = self.c.sym_of_decl(l) else {
                continue;
            };
            let t = self.type_of_symbol(sym);
            let is_rest = self
                .c
                .parent(l)
                .is_some_and(|p| matches!(ast.node(p.node), Node::Rest { .. }));
            let implicit = if is_rest {
                self.c.is_array_type(t)
                    && matches!(
                        self.element_type_of_array_like(t),
                        Some(TypeId::ANY | TypeId::IMPLICIT_ANY)
                    )
            } else {
                t == TypeId::IMPLICIT_ANY || (pattern_typed && t == TypeId::ANY)
            };
            if !implicit {
                continue;
            }
            if let Some(name) = ident_text(ast, n) {
                self.error(
                    l,
                    dg::BINDING_ELEMENT_0_IMPLICITLY_HAS_AN_1_TYPE_7031,
                    &[name, "any"],
                );
            }
        }
    }

    fn declared_without_optionality(&mut self, t: TypeId, p: Loc) -> TypeId {
        let ast = self.ast(p.file);
        if let Node::Param { optional: true, .. } = *ast.node(p.node) {
            return self.remove_missing(t);
        }
        t
    }

    /// A `var`/`let`/`const` list with no declarators (a syntax error the parser recovered).
    fn is_empty_var_list(&self, l: Loc) -> bool {
        matches!(*self.node(l), Node::VarDecl { decls, .. } if decls.is_empty())
    }

    fn check_return(&mut self, l: Loc, arg: Option<NodeId>) {
        let file = l.file;
        // Outside a function tsc reports TS1108 and does not check the expression
        // (parserErrorRecovery_ModuleElement1: no TS2304 for `return foo;`).
        let Some(f) = self.containing_function_or_method(l) else {
            return;
        };
        let ast = self.ast(file);
        let parts = fn_parts(ast, f.node);
        let at = arg.map(|a| self.check_expr(Loc::new(file, a)));
        let Some(parts) = parts else { return };
        // A setter's `return` takes no value (tsc: checkReturnStatement).
        if arg.is_some()
            && let Some(p) = self.c.parent(f)
            && let Node::MethodDef {
                kind: MethodKind::Set,
                ..
            } = *self.node(p)
        {
            self.error(l, dg::SETTERS_CANNOT_RETURN_A_VALUE_2408, &[]);
            return;
        }
        if parts.flags.contains(ts_r_ast::FnFlags::GENERATOR) {
            self.check_generator_return(f, l, parts, arg, at);
            return;
        }
        if let Some(m) = self.c.parent(f)
            && let Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            } = *self.node(m)
        {
            if let Some((a, t)) = arg.zip(at) {
                self.check_constructor_return(m, Loc::new(file, a), t, l);
            }
            return;
        }
        if parts.return_type.is_none()
            && let Some((a, t)) = arg.zip(at)
            && let Some(target) = self.unannotated_getter_setter_type(f)
        {
            // A getter without annotation returns its setter's parameter type.
            self.check_return_expression(Loc::new(file, a), t, target, false, l, 0);
            return;
        }
        if parts.return_type.is_none() && self.jsdoc_declared_signature(f).is_none() {
            // A bare `return;` in a function inferred to return a value (without
            // `strictNullChecks` its `undefined` adds nothing).
            if arg.is_none()
                && self.c.opts.no_implicit_returns
                && !self.c.opts.strict_null_checks
                && !matches!(
                    self.c.parent(f).map(|p| *self.node(p)),
                    Some(Node::MethodDef {
                        kind: MethodKind::Constructor | MethodKind::Set,
                        ..
                    })
                )
            {
                let sig = self.signature_from_decl(f);
                let r = self.return_type_of(sig);
                let r = if parts.flags.contains(ts_r_ast::FnFlags::ASYNC) {
                    self.awaited_type(r)
                } else {
                    r
                };
                if !self.exempt_from_implicit_returns(r)
                    && !self.store().is_error(r)
                    && r != TypeId::UNRESOLVED
                {
                    self.error(l, dg::NOT_ALL_CODE_PATHS_RETURN_A_VALUE_7030, &[]);
                }
            }
            return;
        }
        // Constructors and setters.
        if let Some(p) = self.c.parent(f)
            && let Node::MethodDef {
                kind: MethodKind::Constructor | MethodKind::Set,
                ..
            } = *self.node(p)
        {
            return;
        }
        let sig = self.signature_from_decl(f);
        let Some(declared) = self.declared_return_of(sig) else {
            return;
        };
        let is_async = parts.flags.contains(ts_r_ast::FnFlags::ASYNC);
        let target = if is_async {
            self.awaited_type(declared)
        } else {
            declared
        };
        match (arg, at) {
            (Some(a), Some(t)) => {
                let t = if is_async { self.awaited_type(t) } else { t };
                // Unelaborated errors go on the statement (its `return` keyword).
                self.check_return_expression(Loc::new(file, a), t, target, is_async, l, 0);
            }
            _ if !self.c.opts.strict_null_checks => {
                // A bare `return;` where a value is expected.
                if self.c.opts.no_implicit_returns
                    && !self.exempt_from_implicit_returns(target)
                    && !self.store().is_error(target)
                    && target != TypeId::UNRESOLVED
                {
                    self.error(l, dg::NOT_ALL_CODE_PATHS_RETURN_A_VALUE_7030, &[]);
                }
            }
            _ => {
                if self.c.opts.strict_null_checks
                    && !self.type_accepts_undefined(target)
                    && !self.store().is_error(target)
                    && target != TypeId::UNRESOLVED
                {
                    let span = ast.span(l.node);
                    let span = Span::new(span.start, (span.start + 6).min(span.end));
                    if self.reporting(file) {
                        // Elaborated like any relation (`'T' could be instantiated ...`).
                        let (chain, _) = self.relation_error_chain(
                            TypeId::UNDEFINED,
                            target,
                            ts_r_types::RelationKind::Assignable,
                            None,
                        );
                        self.push_chain(file, span, &chain);
                    }
                }
            }
        }
    }

    /// `return e` in the constructor `m`: `e` must be assignable to the class's instance
    /// type (its own type parameters as arguments). The relation error (elaborated into
    /// `e`, else on the statement) is followed by TS2409 on the statement.
    fn check_constructor_return(&mut self, m: Loc, e: Loc, t: TypeId, stmt: Loc) {
        let Some(class) = self.c.parent(m) else {
            return;
        };
        if !matches!(
            *self.node(class),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        ) {
            return;
        }
        let Some(sym) = self.c.sym_of_decl(class) else {
            return;
        };
        let local = self.local_type_params(sym);
        let instance = if local.is_empty() {
            self.declared_type_of_symbol(sym)
        } else {
            self.type_from_symbol_reference(sym, &local)
        };
        if instance == TypeId::UNRESOLVED || self.store().is_error(instance) {
            return;
        }
        if !self.check_assignable_with_elaboration(t, instance, stmt, Some(e)) {
            self.error(
                stmt,
                dg::RETURN_TYPE_OF_CONSTRUCTOR_SIGNATURE_MUST_BE_ASSIGNABLE_TO_THE_INSTANCE_TYPE_OF_T_2409,
                &[],
            );
        }
    }

    /// For the function `f` of a getter without return annotation: the annotated parameter
    /// type of its setter (class or object literal member).
    pub(crate) fn unannotated_getter_setter_type(&mut self, f: Loc) -> Option<TypeId> {
        let m = self.c.parent(f)?;
        let Node::MethodDef {
            kind: MethodKind::Get,
            ..
        } = *self.node(m)
        else {
            return None;
        };
        let t = match self.c.sym_of_decl(m).and_then(|s| self.setter_type(s)) {
            Some(t) => t,
            None => self.computed_sibling_setter_type(m)?,
        };
        (t != TypeId::UNRESOLVED && !self.store().is_error(t)).then_some(t)
    }

    /// The annotated parameter type of the setter next to getter `m` whose computed name
    /// is the same (`get [G.B]()` and `set [G.B](x: number)`).
    fn computed_sibling_setter_type(&mut self, m: Loc) -> Option<TypeId> {
        let ast = self.ast(m.file);
        let Node::MethodDef { key, modifiers, .. } = *ast.node(m.node) else {
            return None;
        };
        if !matches!(ast.node(key), Node::ComputedKey { .. }) {
            return None;
        }
        let owner = self.c.parent(m)?;
        let members = match *ast.node(owner.node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
            Node::ObjectLit { props } => props,
            _ => return None,
        };
        let name = self.member_key_name(Loc::new(m.file, key))?;
        let is_static = modifiers.contains(ts_r_ast::Modifiers::STATIC);
        // A duplicate (a method of the same name) leaves the getter alone.
        for &x in ast.list(members) {
            if let Node::MethodDef {
                kind: MethodKind::Method,
                key: mk,
                ..
            }
            | Node::PropertyDef { key: mk, .. } = *ast.node(x)
                && self.member_key_name(Loc::new(m.file, mk)).as_deref() == Some(name.as_str())
            {
                return None;
            }
        }
        for &x in ast.list(members) {
            let Node::MethodDef {
                kind: MethodKind::Set,
                key: sk,
                modifiers: sm,
                value,
                ..
            } = *ast.node(x)
            else {
                continue;
            };
            if sm.contains(ts_r_ast::Modifiers::STATIC) != is_static
                || self.member_key_name(Loc::new(m.file, sk)).as_deref() != Some(name.as_str())
            {
                continue;
            }
            let params = crate::nodes::fn_parts(ast, value)?.params;
            let param = *ast
                .list(params)
                .iter()
                .find(|&&p| !crate::nodes::is_this_param(ast, p))?;
            if let Node::Param {
                type_ann: Some(t), ..
            } = *ast.node(param)
            {
                return Some(self.type_from_node(Loc::new(m.file, t)));
            }
            return None;
        }
        None
    }

    /// A generator's `return` against the return type of its annotation
    /// (`Generator<Y, R>`: `R`; awaited in an async generator). Not for annotations without
    /// one (`Iterable<T>`) or whose one is `any`.
    fn check_generator_return(
        &mut self,
        f: Loc,
        l: Loc,
        parts: crate::nodes::FnParts,
        arg: Option<NodeId>,
        at: Option<TypeId>,
    ) {
        if parts.return_type.is_none() {
            return;
        }
        let Some((_, r)) = self.generator_context(f) else {
            return;
        };
        if r == TypeId::UNRESOLVED
            || self.store().is_error(r)
            || self.store().flags(r).intersects(TypeFlags::ANY)
        {
            return;
        }
        let is_async = parts.flags.contains(ts_r_ast::FnFlags::ASYNC);
        match (arg, at) {
            (Some(a), Some(t)) => {
                let t = if is_async { self.awaited_type(t) } else { t };
                self.check_return_expression(Loc::new(l.file, a), t, r, is_async, l, 0);
            }
            _ => {
                if self.c.opts.strict_null_checks && !self.type_accepts_undefined(r) {
                    self.check_assignable_with_elaboration(TypeId::UNDEFINED, r, l, None);
                }
            }
        }
    }

    /// TS2842 in a signature without a body (a function type, a call, construct or method
    /// signature, a bodiless function): a destructured property renamed to an identifier
    /// the signature never names in a type query (`({ a: string }) => void`), probably
    /// meant as a type annotation; with "We can only write a type for 'a' ..." at the end
    /// of an unannotated parameter. Not in declaration files or JavaScript.
    fn check_unused_renamings(&mut self, f: Loc) {
        let file = f.file;
        let ast = self.ast(file);
        let Some(parts) = fn_parts(ast, f.node) else {
            return;
        };
        if parts.body.is_some()
            || self.c.file(file).kind.is_declaration()
            || self.c.file(file).is_js()
            || !self.reporting(file)
        {
            return;
        }
        // Renamed bindings: (parameter, property key, renamed name).
        let mut renamed: Vec<(NodeId, NodeId, NodeId)> = Vec::new();
        for &p in ast.list(parts.params) {
            let Node::Param { pattern, .. } = *ast.node(p) else {
                continue;
            };
            let mut stack = vec![pattern];
            let mut budget = 10_000u32;
            while let Some(n) = stack.pop() {
                budget = budget.saturating_sub(1);
                if budget == 0 {
                    break;
                }
                match *ast.node(n) {
                    Node::ObjectPattern { props } => stack.extend(ast.list(props).iter().rev()),
                    Node::ArrayPattern { elements } => {
                        stack.extend(ast.list(elements).iter().rev())
                    }
                    Node::BindingProperty {
                        key,
                        value: Some(v),
                        ..
                    } => match *ast.node(v) {
                        Node::Ident { .. } | Node::Missing { .. } => renamed.push((p, key, v)),
                        _ => stack.push(v),
                    },
                    Node::AssignPattern { left, .. } => stack.push(left),
                    Node::Rest { arg } => stack.push(arg),
                    _ => {}
                }
            }
        }
        if renamed.is_empty() {
            return;
        }
        // Names read by type queries anywhere in the signature (`typeof string`).
        let mut queried: Vec<&str> = Vec::new();
        let mut stack = vec![f.node];
        let mut budget = 100_000u32;
        while let Some(n) = stack.pop() {
            budget = budget.saturating_sub(1);
            if budget == 0 {
                break;
            }
            if let Node::TypeQuery { expr_name, .. } = *ast.node(n) {
                let mut root = expr_name;
                for _ in 0..64 {
                    match *ast.node(root) {
                        Node::QualifiedName { left, .. } => root = left,
                        _ => break,
                    }
                }
                if let Some(t) = crate::nodes::ident_text(ast, root) {
                    queried.push(t);
                }
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
        for (param, key, v) in renamed {
            // A name the parser had to invent (`{ return 'x' }` read as a pattern) prints
            // as `(Missing)`.
            let name = match crate::nodes::ident_text(ast, v) {
                Some(n) if !n.is_empty() => n,
                _ => "(Missing)",
            };
            if queried.contains(&name) {
                continue;
            }
            let key_text = self.c.text(Loc::new(file, key)).to_owned();
            let name = name.to_owned();
            let mark = self.diags.len();
            self.error(
                Loc::new(file, v),
                dg::_0_IS_AN_UNUSED_RENAMING_OF_1_DID_YOU_INTEND_TO_USE_IT_AS_A_TYPE_ANNOTATION_2842,
                &[&name, &key_text],
            );
            if let Node::Param {
                type_ann: None,
                pattern,
                ..
            } = *ast.node(param)
            {
                let end = ast.span(pattern).end;
                self.relate_last_span(
                    mark,
                    file,
                    Span::new(end, end),
                    dg::WE_CAN_ONLY_WRITE_A_TYPE_FOR_0_BY_ADDING_A_TYPE_FOR_THE_ENTIRE_PARAMETER_HERE_2843,
                    &[&key_text],
                );
            }
        }
    }

    /// TS2303 at each alias declaration whose resolution comes back to itself (`import A =
    /// B; import B = A;` reports both; an import of a module whose `export =` is circular
    /// does not). Import and export specifiers report at the specifier, other declarations
    /// at the statement.
    fn check_circular_aliases(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let decls: Vec<NodeId> = match *ast.node(l.node) {
            Node::ImportDecl { specifiers, .. } | Node::ExportNamed { specifiers, .. } => {
                ast.list(specifiers).to_vec()
            }
            Node::ImportEquals { .. }
            | Node::NamespaceExport { .. }
            | Node::ExportAssignment { .. } => vec![l.node],
            _ => return,
        };
        for d in decls {
            let dl = Loc::new(l.file, d);
            let Some(s) = self.c.sym_of_decl(dl) else {
                continue;
            };
            if !self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                continue;
            }
            let Some(r) = self.c.parts(s).next() else {
                continue;
            };
            if !self.c.link.is_circular_alias(r) {
                continue;
            }
            // `export = self` is named by its expression.
            let name = match *ast.node(d) {
                Node::ExportAssignment { expr } => self.c.text(Loc::new(l.file, expr)).to_owned(),
                _ => self.c.sym_name(s).to_owned(),
            };
            self.error(dl, dg::CIRCULAR_DEFINITION_OF_IMPORT_ALIAS_0_2303, &[&name]);
        }
    }

    /// A returned expression (or arrow expression body) against the declared return type.
    /// Conditional expressions check each branch on its own, reporting at the branch,
    /// even when the union of the branches is assignable (`c ? anyValue : 1`).
    /// `t` is the (awaited) type of `e`; other errors go on `at`.
    fn check_return_expression(
        &mut self,
        e: Loc,
        t: TypeId,
        target: TypeId,
        is_async: bool,
        at: Loc,
        depth: u32,
    ) {
        let ast = self.ast(e.file);
        let n = skip_parens(ast, e.node);
        if depth < 64
            && let Node::Conditional { cons, alt, .. } = *ast.node(n)
        {
            for b in [cons, alt] {
                let bl = Loc::new(e.file, b);
                let bt = self.quietly(|ck| ck.check_expr(bl));
                let bt = if is_async { self.awaited_type(bt) } else { bt };
                self.check_return_expression(bl, bt, target, is_async, bl, depth + 1);
            }
            return;
        }
        // Reported at and elaborated into the expression inside parentheses and
        // `satisfies` (a `return` statement keeps its own position when not elaborated).
        let eff = Loc::new(e.file, crate::nodes::effective_check_node(ast, e.node));
        let at = if at == e { eff } else { at };
        self.check_assignable_with_elaboration(t, target, at, Some(eff));
    }

    /// The function-like node whose body contains `l` (method values included).
    fn containing_function_or_method(&self, l: Loc) -> Option<Loc> {
        let ast = self.ast(l.file);
        let parents = self.c.file(l.file).parents();
        let mut cur = parents.parent(l.node);
        while let Some(n) = cur {
            match ast.node(n) {
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                    return Some(Loc::new(l.file, n));
                }
                Node::ClassDecl { .. } | Node::ClassExpr { .. } | Node::StaticBlock { .. } => {
                    return None;
                }
                _ => {}
            }
            cur = parents.parent(n);
        }
        None
    }

    // ---------------------------------------------------------------------------------
    // Classes
    // ---------------------------------------------------------------------------------

    /// TS2699 for a static member of a non-ambient class named like a built-in property of
    /// functions: `prototype` always, and `name`, `length`, `caller`, `arguments` when class
    /// fields are not emitted with define semantics. Computed keys count by their literal.
    fn check_static_function_name_conflicts(&mut self, l: Loc, members: ts_r_ast::NodeList) {
        let file = l.file;
        if !self.reporting(file) || self.is_ambient_node(l) {
            return;
        }
        let ast = self.ast(file);
        let class_name = match *ast.node(l.node) {
            Node::ClassDecl { name, .. } | Node::ClassExpr { name, .. } => name,
            _ => return,
        };
        let define = self.c.opts.use_define_for_class_fields;
        let mut display: Option<String> = None;
        for &m in ast.list(members) {
            let (modifiers, key) = match *ast.node(m) {
                Node::PropertyDef { modifiers, key, .. } => (modifiers, key),
                Node::MethodDef {
                    modifiers,
                    kind,
                    key,
                    ..
                } if kind != MethodKind::Constructor => (modifiers, key),
                _ => continue,
            };
            if !modifiers.contains(Modifiers::STATIC) {
                continue;
            }
            let name = match *ast.node(key) {
                Node::ComputedKey { expr } => self.late_bound_name(Loc::new(file, expr)),
                Node::PrivateName { .. } => None,
                _ => key_text(ast, key),
            };
            let Some(name) = name else {
                continue;
            };
            let conflicts = match name.as_str() {
                "prototype" => true,
                "name" | "length" | "caller" | "arguments" => !define,
                _ => false,
            };
            if !conflicts {
                continue;
            }
            let class = display
                .get_or_insert_with(|| self.class_display_name(l, class_name))
                .clone();
            self.error(
                Loc::new(file, key),
                dg::STATIC_PROPERTY_0_CONFLICTS_WITH_BUILT_IN_PROPERTY_FUNCTION_0_OF_CONSTRUCTOR_FUN_2699,
                &[&name, &class],
            );
        }
    }

    /// TS2804 at every member named `#x` when the class declares `#x` both as a static and
    /// as an instance member (one lexical scope for both).
    /// The first getter and setter of each statically named class member: TS2676 at both
    /// names when only one is `abstract`, TS2808 when the getter is `protected` and the
    /// setter public, or the getter `private` and the setter not (tsc: checker/checker.go,
    /// checkAccessorDeclaration).
    fn check_accessor_pair_modifiers(&mut self, file: FileId, members: ts_r_ast::NodeList) {
        let ast = self.ast(file);
        // (name, static, getter (key, modifiers), setter (key, modifiers)).
        type Half = Option<(NodeId, Modifiers)>;
        let mut pairs: Vec<(String, bool, Half, Half)> = Vec::new();
        for &m in ast.list(members) {
            let Node::MethodDef {
                modifiers,
                kind: kind @ (MethodKind::Get | MethodKind::Set),
                key,
                ..
            } = *ast.node(m)
            else {
                continue;
            };
            if matches!(ast.node(key), Node::PrivateName { .. }) {
                continue;
            }
            let Some(name) = key_text(ast, key) else {
                continue;
            };
            let is_static = modifiers.contains(Modifiers::STATIC);
            let i = match pairs
                .iter()
                .position(|(n, st, _, _)| *n == name && *st == is_static)
            {
                Some(i) => i,
                None => {
                    pairs.push((name, is_static, None, None));
                    pairs.len() - 1
                }
            };
            let slot = if kind == MethodKind::Get {
                &mut pairs[i].2
            } else {
                &mut pairs[i].3
            };
            if slot.is_none() {
                *slot = Some((key, modifiers));
            }
        }
        for (_, _, get, set) in pairs {
            let (Some((gk, gm)), Some((sk, sm))) = (get, set) else {
                continue;
            };
            let mut report = |info: dg::DiagnosticInfo| {
                self.error(Loc::new(file, gk), info, &[]);
                self.error(Loc::new(file, sk), info, &[]);
            };
            if gm.contains(Modifiers::ABSTRACT) != sm.contains(Modifiers::ABSTRACT) {
                report(dg::ACCESSORS_MUST_BOTH_BE_ABSTRACT_OR_NON_ABSTRACT_2676);
            }
            if (gm.contains(Modifiers::PROTECTED)
                && !sm.intersects(Modifiers::PROTECTED | Modifiers::PRIVATE))
                || (gm.contains(Modifiers::PRIVATE) && !sm.contains(Modifiers::PRIVATE))
            {
                report(dg::A_GET_ACCESSOR_MUST_BE_AT_LEAST_AS_ACCESSIBLE_AS_THE_SETTER_2808);
            }
        }
    }

    fn check_private_name_static_clash(&mut self, file: FileId, members: ts_r_ast::NodeList) {
        let ast = self.ast(file);
        let named: Vec<(NodeId, String, u8)> = ast
            .list(members)
            .iter()
            .filter_map(|&m| match *ast.node(m) {
                Node::PropertyDef { modifiers, key, .. }
                | Node::MethodDef { modifiers, key, .. }
                    if matches!(ast.node(key), Node::PrivateName { .. }) =>
                {
                    let bit = if modifiers.contains(Modifiers::STATIC) {
                        2
                    } else {
                        1
                    };
                    Some((key, self.c.text(Loc::new(file, key)).to_owned(), bit))
                }
                _ => None,
            })
            .collect();
        // Per name: 1 = seen as instance, 2 = seen as static.
        let mut seen: Vec<(&str, u8)> = Vec::new();
        let mut clashing: Vec<&str> = Vec::new();
        for (_, name, bit) in &named {
            match seen.iter_mut().find(|(n, _)| n == name) {
                Some((_, flags)) => {
                    if *flags != 3 && *flags | bit == 3 {
                        clashing.push(name);
                    }
                    *flags |= bit;
                }
                None => seen.push((name, *bit)),
            }
        }
        for name in clashing {
            for (key, n, _) in &named {
                if n == name {
                    self.error(
                        Loc::new(file, *key),
                        dg::DUPLICATE_IDENTIFIER_0_STATIC_AND_INSTANCE_ELEMENTS_CANNOT_SHARE_THE_SAME_PRIVAT_2804,
                        &[name],
                    );
                }
            }
        }
    }

    pub fn check_class(&mut self, l: Loc) {
        self.check_class_named_object(l);
        let file = l.file;
        let ast = self.ast(file);
        let (heritage, members) = match *ast.node(l.node) {
            Node::ClassDecl {
                heritage, members, ..
            }
            | Node::ClassExpr {
                heritage, members, ..
            } => (heritage, members),
            _ => return,
        };
        // Experimental decorators on both accessors of a get/set pair: TS1207 at the first
        // decorator of the second one.
        if self.c.opts.experimental_decorators {
            let mut decorated: Vec<(String, bool, MethodKind)> = Vec::new();
            for &m in ast.list(members) {
                let Node::MethodDef {
                    decorators,
                    modifiers,
                    kind: kind @ (MethodKind::Get | MethodKind::Set),
                    key,
                    ..
                } = *ast.node(m)
                else {
                    continue;
                };
                let (Some(&first), Some(name)) = (ast.list(decorators).first(), key_text(ast, key))
                else {
                    continue;
                };
                let is_static = modifiers.contains(Modifiers::STATIC);
                if let Some(i) = decorated
                    .iter()
                    .position(|(n, s, k)| *n == name && *s == is_static && *k != kind)
                {
                    decorated.swap_remove(i);
                    self.error(
                        Loc::new(file, first),
                        dg::DECORATORS_CANNOT_BE_APPLIED_TO_MULTIPLE_GET_SET_ACCESSORS_OF_THE_SAME_NAME_1207,
                        &[],
                    );
                } else {
                    decorated.push((name, is_static, kind));
                }
            }
        }
        // `#constructor` names no member (TS18012).
        for &m in ast.list(members) {
            if let Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } = *ast.node(m)
                && matches!(ast.node(key), Node::PrivateName { .. })
                && self.c.text(Loc::new(file, key)) == "#constructor"
            {
                self.error(
                    Loc::new(file, key),
                    dg::CONSTRUCTOR_IS_A_RESERVED_WORD_18012,
                    &[],
                );
            }
        }
        self.check_private_name_static_clash(file, members);
        self.check_accessor_pair_modifiers(file, members);
        self.check_static_function_name_conflicts(l, members);
        for &h in ast.list(heritage) {
            let Node::HeritageClause { kind, types } = *ast.node(h) else {
                continue;
            };
            if kind == HeritageKind::Extends {
                for (i, &t) in ast.list(types).iter().enumerate() {
                    if let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(t) {
                        // `class C extends I` with `I` an interface (no value): TS2689
                        // instead of the expression's errors.
                        if self.extends_interface_only(Loc::new(file, expr)) {
                            if self.reporting(file) {
                                let text = self.c.text(Loc::new(file, expr)).to_owned();
                                self.error(
                                    Loc::new(file, expr),
                                    dg::CANNOT_EXTEND_AN_INTERFACE_0_DID_YOU_MEAN_IMPLEMENTS_2689,
                                    &[&text],
                                );
                            }
                            continue;
                        }
                        let et = self.check_expr(Loc::new(file, expr));
                        if i == 0 {
                            self.check_base_constructor_type(Loc::new(file, expr), et);
                            self.check_class_extends_type_args(Loc::new(file, t), expr, type_args);
                        }
                    }
                }
            }
        }
        if let Some(sym) = self.c.sym_of_decl(l) {
            let _ = self.declared_type_of_symbol(sym);
        }
        let ctor_body = ast.list(members).iter().find_map(|&m| match *ast.node(m) {
            Node::MethodDef {
                kind: MethodKind::Constructor,
                value,
                ..
            } => fn_parts(ast, value).and_then(|p| p.body),
            _ => None,
        });
        for &m in ast.list(members) {
            let ml = Loc::new(file, m);
            self.check_derived_constructor_super(ml);
            match *ast.node(m) {
                Node::PropertyDef {
                    modifiers,
                    key,
                    optional,
                    definite,
                    type_ann,
                    value,
                    ..
                } => {
                    if let Node::ComputedKey { expr } = *ast.node(key) {
                        let kt = self.check_expr(Loc::new(file, expr));
                        // `[P in K]: T` (a mapped type written as a member) is TS7061 there.
                        if !matches!(
                            ast.node(expr),
                            Node::Binary {
                                op: ts_r_ast::BinaryOp::In,
                                ..
                            }
                        ) {
                            let _ = self.check_computed_key_type(Loc::new(file, key), kt);
                        }
                    }
                    let declared = self.c.sym_of_decl(ml).map(|s| self.type_of_symbol(s));
                    // TS7008: no annotation, no initializer and no constructor assignment.
                    if value.is_none()
                        && type_ann.is_none()
                        && self.c.opts.no_implicit_any
                        && !self.c.file(file).is_js()
                        // (Ambient private members, `#private` included, are written untyped
                        // by declaration emit.)
                        && !((modifiers.contains(Modifiers::PRIVATE)
                            || matches!(ast.node(key), Node::PrivateName { .. }))
                            && self.is_ambient_node(ml))
                        && declared == Some(TypeId::IMPLICIT_ANY)
                        && let Some(name) = key_text(ast, key)
                        // A `static prototype` property merges with the class's own
                        // `prototype` (typed by the class), so it is not implicitly `any`
                        // (tsc: binder.go, bindClassLikeDeclaration; property excludes nothing).
                        && !(modifiers.contains(Modifiers::STATIC) && name == "prototype")
                    {
                        self.error(
                            Loc::new(file, key),
                            dg::MEMBER_0_IMPLICITLY_HAS_AN_1_TYPE_7008,
                            &[&name, "any"],
                        );
                    }
                    if value.is_none() {
                        self.check_base_property_overwrite(l, m, ctor_body);
                    }
                    if let Some(v) = value {
                        let vl = Loc::new(file, v);
                        let t = self.check_expr(vl);
                        if type_ann.is_some()
                            && let Some(d) = declared
                        {
                            self.check_assignable_with_elaboration(
                                t,
                                d,
                                Loc::new(file, key),
                                Some(vl),
                            );
                        }
                    } else if self.c.opts.strict_property_initialization
                        && self.c.opts.strict_null_checks
                        && type_ann.is_some()
                        && !optional
                        && !definite
                        && !modifiers.intersects(
                            Modifiers::STATIC | Modifiers::ABSTRACT | Modifiers::DECLARE,
                        )
                        && !self.is_ambient_node(ml)
                        && let Some(d) = declared
                        && !self.store().is_error(d)
                        && d != TypeId::UNRESOLVED
                        && !self.store().flags(d).intersects(TypeFlags::ANY_OR_UNKNOWN)
                        && !self.type_accepts_undefined(d)
                        // Identifier, `#private` and computed names (named by their source
                        // text, `[Symbol.iterator]`); string and numeric names are not checked.
                        && let Some(name) = match *ast.node(key) {
                            Node::Ident { .. } | Node::PrivateName { .. } => key_text(ast, key),
                            Node::ComputedKey { .. } => {
                                let sp = ast.span(key);
                                self.c
                                    .file(file)
                                    .text
                                    .get(sp.start as usize..sp.end as usize)
                                    .map(str::to_owned)
                            }
                            _ => None,
                        }
                        && !type_ann.is_some_and(|t| self.is_type_ref_with_bad_arity(Loc::new(file, t)))
                        && !ctor_body.is_some_and(|b| self.assigns_this_property(file, b, key))
                    {
                        self.error(Loc::new(file, key), dg::PROPERTY_0_HAS_NO_INITIALIZER_AND_IS_NOT_DEFINITELY_ASSIGNED_IN_THE_CONSTRUCTOR_2564, &[&name]);
                    }
                }
                Node::MethodDef {
                    key, value, kind, ..
                } => {
                    if let Node::ComputedKey { expr } = *ast.node(key) {
                        let kt = self.check_expr(Loc::new(file, expr));
                        let _ = self.check_computed_key_type(Loc::new(file, key), kt);
                    }
                    // A class method's JSDoc `@type` without a fitting call signature: TS8030.
                    if kind == MethodKind::Method {
                        self.check_jsdoc_function_type(Loc::new(file, value));
                    }
                    self.check_function_like(Loc::new(file, value));
                }
                Node::StaticBlock { body } => {
                    self.check_unreachable_stmts(file, ast.list(body));
                    for &s in ast.list(body) {
                        self.check_stmt(Loc::new(file, s), 1);
                    }
                }
                _ => {}
            }
        }
    }

    // ---------------------------------------------------------------------------------
    // Type annotations
    // ---------------------------------------------------------------------------------

    /// An `implements` / interface `extends` element must name an entity (`A`, `ns.A`, no
    /// optional chain, call or literal): TS2500 / TS2499 at the expression (tsc:
    /// checker/checker.go, checkClassLikeDeclaration and checkInterfaceDeclaration).
    fn check_heritage_entity_name(&mut self, expr: Loc, info: dg::DiagnosticInfo) {
        if !crate::nodes::is_entity_name_expr(self.ast(expr.file), expr.node) {
            self.error(expr, info, &[]);
        }
    }

    /// A `this` parameter that is not the first parameter is TS2680; on a constructor,
    /// construct signature or constructor type TS2681, on an accessor TS2784 (tsc:
    /// checker/checker.go, checkParameter). Arrows get TS2730 in `check_function_like`.
    fn check_this_param_placement(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        let (params, ctor, accessor) = match *ast.node(l.node) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionType { params, .. }
            | Node::CallSignature { params, .. } => (params, false, false),
            Node::ConstructorType { params, .. } | Node::ConstructSignature { params, .. } => {
                (params, true, false)
            }
            Node::MethodSignature { kind, params, .. } => (
                params,
                kind == ts_r_ast::MethodKind::Constructor,
                matches!(kind, ts_r_ast::MethodKind::Get | ts_r_ast::MethodKind::Set),
            ),
            Node::FunctionExpr { params, .. } => {
                let kind = match self.c.parent(l).map(|p| *ast.node(p.node)) {
                    Some(Node::MethodDef { kind, value, .. }) if value == l.node => Some(kind),
                    _ => None,
                };
                (
                    params,
                    kind == Some(ts_r_ast::MethodKind::Constructor),
                    matches!(
                        kind,
                        Some(ts_r_ast::MethodKind::Get | ts_r_ast::MethodKind::Set)
                    ),
                )
            }
            _ => return,
        };
        for (i, &p) in ast.list(params).iter().enumerate() {
            if !is_this_param(ast, p) {
                continue;
            }
            let pl = Loc::new(l.file, p);
            if i != 0 {
                self.error(
                    pl,
                    dg::A_0_PARAMETER_MUST_BE_THE_FIRST_PARAMETER_2680,
                    &["this"],
                );
            }
            if ctor {
                self.error(pl, dg::A_CONSTRUCTOR_CANNOT_HAVE_A_THIS_PARAMETER_2681, &[]);
            }
            if accessor {
                self.error(
                    pl,
                    dg::GET_AND_SET_ACCESSORS_CANNOT_DECLARE_THIS_PARAMETERS_2784,
                    &[],
                );
            }
        }
    }

    /// Checks every type node of the file (names, arity, constraints).
    fn check_type_annotations(&mut self, file: FileId) {
        let ast = self.ast(file);
        let mut stack = vec![ast.root];
        // JSDoc types that are not part of the tree (`@type` of a function).
        stack.extend(crate::jsdoc::detached_jsdoc_types(ast));
        self.check_jsdoc_overload_returns(file);
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            match *ast.node(n) {
                Node::TypeRef { name, type_args } => {
                    self.check_type_reference(Loc::new(file, n), name, type_args);
                    if let Some(ta) = type_args {
                        stack.push(ta);
                    }
                    continue;
                }
                Node::TypeQuery {
                    expr_name,
                    type_args,
                } => {
                    self.check_type_query_name(Loc::new(file, expr_name));
                    // A property path is checked like the expression it names, used or not
                    // (`type C = typeof foo.a.b` with `foo.a` possibly undefined: TS2532).
                    if type_args.is_none()
                        && matches!(ast.node(expr_name), Node::QualifiedName { .. })
                    {
                        self.type_of_entity_expression(Loc::new(file, expr_name));
                    }
                    if let Some(ta) = type_args {
                        // `typeof f<A>`: TS2635 when no signature takes the arguments.
                        let t = self.type_of_entity_expression(Loc::new(file, expr_name));
                        let args = self.type_args_from_node(Some(Loc::new(file, ta)), false);
                        self.instantiation_expression_type(t, &args, Some(Loc::new(file, ta)));
                        stack.push(ta);
                    }
                    continue;
                }
                Node::HeritageClause {
                    kind: HeritageKind::Implements,
                    types,
                } => {
                    for &t in ast.list(types) {
                        if let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(t) {
                            self.check_heritage_entity_name(
                                Loc::new(file, expr),
                                dg::A_CLASS_CAN_ONLY_IMPLEMENT_AN_IDENTIFIER_QUALIFIED_NAME_WITH_OPTIONAL_TYPE_ARGUM_2500,
                            );
                            self.check_type_reference(Loc::new(file, t), expr, type_args);
                            if let Some(ta) = type_args {
                                stack.push(ta);
                            }
                        }
                    }
                    continue;
                }
                Node::Interface { extends, .. } => {
                    for &t in ast.list(extends) {
                        if let Node::ExprWithTypeArgs { expr, type_args } = *ast.node(t) {
                            self.check_heritage_entity_name(
                                Loc::new(file, expr),
                                dg::AN_INTERFACE_CAN_ONLY_EXTEND_AN_IDENTIFIER_QUALIFIED_NAME_WITH_OPTIONAL_TYPE_ARG_2499,
                            );
                            self.check_type_reference(Loc::new(file, t), expr, type_args);
                        }
                    }
                }
                Node::ExprWithTypeArgs { .. } => {
                    // Class `extends` expressions are values; their type arguments are types.
                }
                Node::InferType { type_param } => {
                    self.check_infer_constraints(Loc::new(file, type_param));
                }
                // `import(x)` as a type takes a string literal (TS1141, also in files with
                // syntax errors: `typeof import.defer("m")`).
                Node::ImportType { arg, .. }
                    if !matches!(ast.node(arg), Node::StringLit { .. }) && !ast.is_jsdoc(n) =>
                {
                    self.error(Loc::new(file, arg), dg::STRING_LITERAL_EXPECTED_1141, &[]);
                }
                Node::ImportType {
                    is_typeof: false,
                    qualifier: None,
                    arg,
                    ..
                } => self.check_import_type_module(Loc::new(file, n), arg),
                Node::ImportType {
                    is_typeof: false,
                    qualifier: Some(q),
                    arg,
                    ..
                } => self.check_import_type_qualifier(Loc::new(file, n), arg, q),
                Node::TypeParam { .. } => {
                    self.check_type_param_modifiers(file, n);
                    self.check_circular_constraint(Loc::new(file, n));
                }
                Node::TypeParams { .. } => self.check_type_param_defaults(file, n),
                Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::FunctionType { .. }
                | Node::ConstructorType { .. }
                | Node::CallSignature { .. }
                | Node::ConstructSignature { .. }
                | Node::MethodSignature { .. } => {
                    self.check_unused_renamings(Loc::new(file, n));
                    self.check_this_param_placement(Loc::new(file, n));
                    let params = match *ast.node(n) {
                        Node::FunctionType { params, .. }
                        | Node::ConstructorType { params, .. }
                        | Node::CallSignature { params, .. }
                        | Node::ConstructSignature { params, .. }
                        | Node::MethodSignature { params, .. } => Some(params),
                        _ => None,
                    };
                    if let Some(params) = params {
                        for p in ast.list(params).to_vec() {
                            self.check_rest_param_type(Loc::new(file, p));
                        }
                    }
                }
                Node::ThisType {} => self.check_this_type_node(Loc::new(file, n)),
                Node::TypeOperator {
                    op: ts_r_ast::TypeOperatorKind::Unique,
                    ty,
                } => self.check_unique_symbol_type(Loc::new(file, n), ty),
                Node::IndexedAccessType { obj, index } => {
                    self.check_tuple_index_type(Loc::new(file, obj), Loc::new(file, index));
                    self.check_generic_indexed_access_type(Loc::new(file, n), Loc::new(file, obj));
                }
                // A JSDoc type predicate other than a return type (`/** @type {x is T} */`).
                Node::TypePredicate { .. } if ast.is_jsdoc(n) => {
                    let l = Loc::new(file, n);
                    if self
                        .c
                        .parent(l)
                        .is_none_or(|p| self.return_type_node(p) != Some(l))
                    {
                        self.error(
                            l,
                            dg::A_TYPE_PREDICATE_IS_ONLY_ALLOWED_IN_RETURN_TYPE_POSITION_FOR_FUNCTIONS_AND_METHO_1228,
                            &[],
                        );
                    }
                }
                Node::TemplateLitType { .. } => {
                    // Resolving reports a union that is too large (TS2590).
                    let _ = self.type_from_node(Loc::new(file, n));
                }
                Node::TupleType { elems } => {
                    self.check_tuple_rest_elements(file, elems);
                    // Spreads may produce a tuple over the length limit (TS2799 here).
                    let at = Loc::new(file, n);
                    self.inst_count = 0;
                    let _ = self.type_from_node(at);
                    self.report_instantiation_overflow(at, false);
                }
                Node::ComputedKey { expr } => {
                    // Computed names inside type literals and interfaces.
                    if let Some(p) = self.c.parent(Loc::new(file, n))
                        && matches!(
                            self.node(p),
                            Node::PropertySignature { .. } | Node::MethodSignature { .. }
                        )
                    {
                        let kt = self.check_expr(Loc::new(file, expr));
                        // `[P in K]: T` (a mapped type written as a member) is TS7061 there.
                        let mapped_like = matches!(self.node(p), Node::PropertySignature { .. })
                            && matches!(
                                ast.node(expr),
                                Node::Binary {
                                    op: ts_r_ast::BinaryOp::In,
                                    ..
                                }
                            );
                        if !mapped_like {
                            let _ = self.check_computed_key_type(Loc::new(file, n), kt);
                        }
                    }
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
    }

    /// Grammar of `unique symbol` (uniqueSymbolsErrors): the operand must be `symbol`
    /// (TS1005), and the type (through parentheses) may only annotate a `const` variable
    /// with an identifier name in a variable statement (TS1334, TS1333, TS1332), a
    /// `static readonly` class property (TS1331, at its name) or a `readonly` property
    /// signature (TS1330, at its name); anywhere else it is TS1335. Files with syntax
    /// errors get none of these.
    fn check_unique_symbol_type(&mut self, l: Loc, operand: NodeId) {
        if self.has_syntax_errors(l.file) {
            return;
        }
        let ast = self.ast(l.file);
        if !matches!(
            ast.node(operand),
            Node::KeywordType {
                kind: ts_r_ast::KeywordTypeKind::Symbol
            }
        ) {
            self.error(Loc::new(l.file, operand), dg::_0_EXPECTED_1005, &["symbol"]);
            return;
        }
        let mut child = l;
        let mut parent = self.c.parent(l);
        while let Some(p) = parent
            && matches!(ast.node(p.node), Node::ParenType { .. })
        {
            child = p;
            parent = self.c.parent(p);
        }
        let Some(p) = parent else {
            return;
        };
        match *ast.node(p.node) {
            Node::VarDeclarator {
                name,
                type_ann: Some(t),
                ..
            } if t == child.node => {
                let decl = self.c.parent(p);
                let (kind, in_statement) = match decl.map(|d| (d, ast.node(d.node))) {
                    Some((d, Node::VarDecl { kind, .. })) => (
                        Some(*kind),
                        !self.c.parent(d).is_some_and(|s| {
                            matches!(
                                ast.node(s.node),
                                Node::For { .. } | Node::ForIn { .. } | Node::ForOf { .. }
                            )
                        }),
                    ),
                    _ => (None, false),
                };
                if !in_statement {
                    self.error(
                        l,
                        dg::UNIQUE_SYMBOL_TYPES_ARE_ONLY_ALLOWED_ON_VARIABLES_IN_A_VARIABLE_STATEMENT_1334,
                        &[],
                    );
                } else if !matches!(ast.node(name), Node::Ident { .. }) {
                    self.error(
                        l,
                        dg::UNIQUE_SYMBOL_TYPES_MAY_NOT_BE_USED_ON_A_VARIABLE_DECLARATION_WITH_A_BINDING_NAM_1333,
                        &[],
                    );
                } else if kind != Some(ts_r_ast::VarKind::Const) {
                    self.error(
                        Loc::new(l.file, name),
                        dg::A_VARIABLE_WHOSE_TYPE_IS_A_UNIQUE_SYMBOL_TYPE_MUST_BE_CONST_1332,
                        &[],
                    );
                }
            }
            Node::PropertyDef {
                modifiers,
                key,
                type_ann: Some(t),
                ..
            } if t == child.node => {
                if !modifiers.contains(Modifiers::STATIC | Modifiers::READONLY) {
                    self.error(
                        Loc::new(l.file, key),
                        dg::A_PROPERTY_OF_A_CLASS_WHOSE_TYPE_IS_A_UNIQUE_SYMBOL_TYPE_MUST_BE_BOTH_STATIC_AND_1331,
                        &[],
                    );
                }
            }
            Node::PropertySignature {
                modifiers,
                key,
                type_ann: Some(t),
                ..
            } if t == child.node => {
                if !modifiers.contains(Modifiers::READONLY) {
                    self.error(
                        Loc::new(l.file, key),
                        dg::A_PROPERTY_OF_AN_INTERFACE_OR_TYPE_LITERAL_WHOSE_TYPE_IS_A_UNIQUE_SYMBOL_TYPE_MU_1330,
                        &[],
                    );
                }
            }
            _ => self.error(l, dg::UNIQUE_SYMBOL_TYPES_ARE_NOT_ALLOWED_HERE_1335, &[]),
        }
    }

    /// TS1340: `import("m")` as a type of a module that names no type (`module_as_type`).
    /// `import("m").A.B` as a type: TS2694 at the first name the module (or namespace) does
    /// not export, or at a last name that is only a value (tsc 7, JSDoc included). Names
    /// with other meanings are left alone; CommonJS JavaScript modules are not modelled.
    fn check_import_type_qualifier(&mut self, l: Loc, arg: NodeId, q: NodeId) {
        let ast = self.ast(l.file);
        let lit = match *ast.node(arg) {
            Node::TypeRef { name, .. } => name,
            _ => arg,
        };
        if !matches!(ast.node(lit), Node::StringLit { .. }) {
            return;
        }
        let bound = self.c.bound(l.file);
        let Some(req) = bound
            .module_requests
            .iter()
            .position(|r| r.node == lit || r.node == arg)
        else {
            return;
        };
        let target = self.c.link.module_target(l.file, req as u32);
        let Some(&first) = self.c.link.module_symbols(target).first() else {
            return;
        };
        let msym = self.c.sym_ref(first);
        let Some(d) = self.c.first_decl(msym) else {
            return;
        };
        if self.c.file(d.file).is_js() && self.c.file(d.file).is_commonjs_module() {
            return;
        }
        // Only a source file's module (not `declare module "m"`, augmented or `export =`).
        if !self.c.sym_flags(msym).intersects(SymbolFlags::MODULE_FILE)
            || self.c.export_of(msym, "export=").is_some()
        {
            return;
        }
        let mut names = Vec::new();
        let mut n = q;
        for _ in 0..256 {
            match *ast.node(n) {
                Node::QualifiedName { left, right } => {
                    names.push(right);
                    n = left;
                }
                _ => {
                    names.push(n);
                    break;
                }
            }
        }
        names.reverse();
        let mut shown = format!(
            "\"{}\"",
            crate::print::strip_ts_ext(&self.c.file(d.file).path)
        );
        let mut cur = msym;
        let count = names.len();
        for (k, nm) in names.into_iter().enumerate() {
            let Some(text) = ident_text(ast, nm) else {
                return;
            };
            // The root of a dotted `@typedef` name (`Ast::jsdoc_namespaces`, not modelled).
            let target_ast = self.ast(d.file);
            if k == 0
                && target_ast
                    .jsdoc_namespaces
                    .iter()
                    .any(|&a| target_ast.atom(a) == text)
            {
                return;
            }
            let found = self
                .c
                .export_of(cur, text)
                .and_then(|s| self.c.resolve_alias(s));
            let report = match found {
                None => true,
                Some(t) => {
                    let last = k + 1 == count;
                    let type_like = self.c.has_meaning(t, meaning::TYPE | meaning::NAMESPACE);
                    if !type_like {
                        true
                    } else if last || !self.c.has_meaning(t, meaning::NAMESPACE) {
                        // A type or namespace at the end, or a type in the middle
                        // (reported elsewhere, or not modelled).
                        return;
                    } else {
                        false
                    }
                }
            };
            if report {
                self.error(
                    Loc::new(l.file, nm),
                    dg::NAMESPACE_0_HAS_NO_EXPORTED_MEMBER_1_2694,
                    &[&shown, text],
                );
                return;
            }
            let Some(t) = found else {
                return;
            };
            cur = t;
            shown = format!("{shown}.{text}");
        }
    }

    fn check_import_type_module(&mut self, l: Loc, arg: NodeId) {
        let ast = self.ast(l.file);
        let lit = match *ast.node(arg) {
            Node::TypeRef { name, .. } => name,
            _ => arg,
        };
        let Node::StringLit { value } = *ast.node(lit) else {
            return;
        };
        let bound = self.c.bound(l.file);
        let Some(req) = bound
            .module_requests
            .iter()
            .position(|r| r.node == lit || r.node == arg)
        else {
            return;
        };
        let target = self.c.link.module_target(l.file, req as u32);
        let Some(&first) = self.c.link.module_symbols(target).first() else {
            return;
        };
        let msym = self.c.sym_ref(first);
        if self.module_as_type(msym).is_err() {
            let spec = ast.atom(value);
            self.error(
                l,
                dg::MODULE_0_DOES_NOT_REFER_TO_A_TYPE_BUT_IS_USED_AS_A_TYPE_HERE_DID_YOU_MEAN_TYPEOF_1340,
                &[spec],
            );
        }
    }

    /// TS2526: the `this` type is only available in a non-static member of a class or
    /// interface (observed tsc 7): not in type literals, object literal methods,
    /// functions, constructors, static members, heritage clauses or type parameters.
    /// Arrow functions and function types are transparent.
    fn check_this_type_node(&mut self, l: Loc) {
        if self.c.file(l.file).is_js() {
            return;
        }
        let ast = self.ast(l.file);
        // `this is T` names the `this` parameter, not the type.
        if let Some(p) = self.c.parent(l)
            && matches!(*ast.node(p.node), Node::TypePredicate { param, .. } if param == l.node)
        {
            return;
        }
        let ok = self.this_type_container(l).is_some();
        if !ok {
            self.error(
                l,
                dg::A_THIS_TYPE_IS_AVAILABLE_ONLY_IN_A_NON_STATIC_MEMBER_OF_A_CLASS_OR_INTERFACE_2526,
                &[],
            );
        }
    }

    /// `P[2]` on a tuple type without rest elements and of length 2: TS2493 at the index.
    fn check_tuple_index_type(&mut self, obj: Loc, index: Loc) {
        let o = self.type_from_node(obj);
        let i = self.type_from_node(index);
        let s = self.store();
        let Some(shape) = s.tuple_shape_of(o) else {
            if let Some(v) = s.number_value(i)
                && self.union_tuples_out_of_range(o, v)
            {
                let ts = self.type_to_string(o);
                let is = ts_r_binder::js_number_to_string(v);
                self.error(
                    index,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&is, &ts],
                );
                return;
            }
            if !self.in_true_branch_checking(obj, o) {
                self.check_indexed_access_type_key(o, i, index);
            }
            return;
        };
        if shape.has_rest() {
            return;
        }
        let Some(v) = s.number_value(i) else {
            return;
        };
        if v < 0.0 {
            self.error(
                index,
                dg::A_TUPLE_TYPE_CANNOT_BE_INDEXED_WITH_A_NEGATIVE_VALUE_2514,
                &[],
            );
            return;
        }
        let len = shape.elements.len();
        if v < len as f64 || v.fract() != 0.0 {
            return;
        }
        let ts = self.type_to_string(o);
        let is = ts_r_binder::js_number_to_string(v);
        self.error(
            index,
            dg::TUPLE_TYPE_0_OF_LENGTH_1_HAS_NO_ELEMENT_AT_INDEX_2_2493,
            &[&ts, &len.to_string(), &is],
        );
    }

    /// Whether `l` is inside the type arguments of an `import("m").X<...>` type node.
    fn in_import_type_args(&self, l: Loc) -> bool {
        let mut cur = l;
        while let Some(p) = self.c.parent(cur) {
            if let Node::ImportType {
                type_args: Some(args),
                ..
            } = *self.ast(p.file).node(p.node)
                && args == cur.node
            {
                return true;
            }
            cur = p;
        }
        false
    }

    /// An indexed access type node that stays generic (`T["0.0"]`): every member of its
    /// key must be assignable to `keyof` the object, or be a numeric key of an object with
    /// a number index signature, else TS2536 at the whole node (tsc: checker.go,
    /// checkIndexedAccessIndexType).
    fn check_generic_indexed_access_type(&mut self, l: Loc, obj: Loc) {
        let t = self.type_from_node(l);
        let TypeKind::IndexedAccess { obj: o, index: i } = self.store().kind(t) else {
            return;
        };
        let s = self.store();
        if s.is_error(o) || s.is_error(i) || o == TypeId::UNRESOLVED || i == TypeId::UNRESOLVED {
            return;
        }
        // The type arguments of `import("m").X<...>` are resolved but never walked by the
        // check pass, so their indexed accesses go unchecked (tsc: checker.go,
        // checkImportType checks the argument and constraints, not the type argument nodes).
        if self.in_import_type_args(l) {
            return;
        }
        // A mapped type with an `as` clause has its own key space (not modelled here).
        if self
            .mapped_info(o)
            .is_some_and(|info| info.name_node.is_some())
        {
            return;
        }
        let o0 = o;
        let mut o = self.with_implied_constraints(obj, o);
        // An unconstrained `...infer R` in a rest position is an array.
        if let Some(c) = self.rest_infer_array(o) {
            o = self.intersection(&[o, c]);
        }
        let index = self.indexed_access_index_node(l);
        let iw = match index {
            Some(index) => self.with_implied_constraints(index, i),
            None => i,
        };
        // A conditional object is keyed by its constraint, with an unconstrained `infer`
        // from a rest position read as an array (`(F extends (...a: infer P) => any ? P :
        // never)[0]`).
        if self.store().flags(o).contains(TypeFlags::CONDITIONAL)
            && let Some(c) = self.conditional_constraint(o)
            && c != o
        {
            let s = self.store();
            let ms = if s.flags(c).contains(TypeFlags::UNION) {
                s.members_of_union(c).to_vec()
            } else {
                vec![c]
            };
            let mut changed = false;
            let mut out = Vec::with_capacity(ms.len());
            for m in ms {
                match self.rest_infer_array(m) {
                    Some(rc) => {
                        out.push(rc);
                        changed = true;
                    }
                    None => out.push(m),
                }
            }
            if changed {
                o = self.union(&out);
            }
        }
        let keys = self.keyof_type(o);
        let has_number = self.index_info_of(o, TypeId::NUMBER).is_some();
        let s = self.store();
        let members = if iw == i && s.flags(i).contains(TypeFlags::UNION) {
            s.members_of_union(i).to_vec()
        } else {
            vec![iw]
        };
        for m in members {
            if self.is_assignable(m, keys) {
                continue;
            }
            if has_number
                && (self.is_assignable(m, TypeId::NUMBER)
                    || self
                        .store()
                        .string_value(m)
                        .is_some_and(crate::expr::is_numeric_name))
            {
                continue;
            }
            // A private or protected member is TS4105 in tsc (not reported here).
            if let Some(name) = self.property_name_of_type(m) {
                let a = self.apparent_type(o);
                let nid = self.store().intern_str(&name);
                if self.property_of_type_id(a, nid).is_some() {
                    return;
                }
            }
            let is = self.type_to_string(i);
            let os = self.type_to_string(o0);
            self.error(
                l,
                dg::TYPE_0_CANNOT_BE_USED_TO_INDEX_TYPE_1_2536,
                &[&is, &os],
            );
            return;
        }
    }

    /// `unknown[]` for an unconstrained `infer X` in a rest position (tsc:
    /// getInferredTypeParameterConstraint; kept out of the declared constraint here).
    fn rest_infer_array(&mut self, t: TypeId) -> Option<TypeId> {
        if !self.store().flags(t).contains(TypeFlags::TYPE_PARAMETER)
            || self.type_param_constraint(t).is_some()
        {
            return None;
        }
        let sym = self.store().type_param_data(t)?.sym;
        let d = self.c.decls(sym).first().copied()?;
        self.implied_infer_constraint(d, true)
    }

    fn indexed_access_index_node(&self, l: Loc) -> Option<Loc> {
        match *self.node(l) {
            Node::IndexedAccessType { index, .. } => Some(Loc::new(l.file, index)),
            _ => None,
        }
    }

    /// `t` written at `at` with the constraints implied by enclosing conditional types:
    /// in the true branch of `C extends E ? X : Y` (outside parameter positions that flip
    /// variance, type variables always) a type that is `C` reads as `C & E`; members of
    /// an intersection or union are substituted one by one (tsc: checker.go,
    /// getConditionalFlowTypeOfType, getImpliedConstraint).
    pub(crate) fn with_implied_constraints(&mut self, at: Loc, t: TypeId) -> TypeId {
        let mut constraints = Vec::new();
        let tv = self
            .store()
            .flags(t)
            .intersects(TypeFlags::TYPE_PARAMETER | TypeFlags::INDEXED_ACCESS);
        let mut covariant = true;
        let mut cur = at;
        for _ in 0..200 {
            let Some(p) = self.c.parent(cur) else {
                break;
            };
            match *self.node(p) {
                Node::Param { .. } => covariant = !covariant,
                Node::ConditionalType {
                    check,
                    extends,
                    true_ty,
                    ..
                } if true_ty == cur.node && (covariant || tv) => {
                    if let Some(c) = self.implied_constraint(
                        t,
                        Loc::new(at.file, check),
                        Loc::new(at.file, extends),
                    ) {
                        constraints.push(c);
                    }
                }
                // In the template of `{ [K in keyof T]: X }` with `T` constrained to arrays
                // or tuples, `K` is a numeric key.
                Node::MappedType {
                    name_type: None,
                    ty: Some(ty),
                    ..
                } if ty == cur.node
                    && self.store().flags(t).contains(TypeFlags::TYPE_PARAMETER) =>
                {
                    let mt = self.type_from_node(p);
                    if let Some(info) = self.mapped_info(mt)
                        && info.k == t
                        && let Some(tv) = self.homomorphic_type_variable(&info)
                        && let Some(c) = self.type_param_constraint(tv)
                        && self.every_array_or_tuple(c)
                    {
                        let ns = self.template_literal_type(
                            &[String::new(), String::new()],
                            &[TypeId::NUMBER],
                        );
                        let n = self.union(&[TypeId::NUMBER, ns]);
                        constraints.push(n);
                    }
                }
                _ => {}
            }
            if self.node(p).is_statement() {
                break;
            }
            cur = p;
        }
        if !constraints.is_empty() {
            constraints.insert(0, t);
            return self.intersection(&constraints);
        }
        let s = self.store();
        let f = s.flags(t);
        if f.intersects(TypeFlags::INTERSECTION | TypeFlags::UNION) && t != TypeId::BOOLEAN {
            let members = s.members_of_union(t).to_vec();
            let mut changed = false;
            let mut out = Vec::with_capacity(members.len());
            for m in members {
                if self
                    .store()
                    .flags(m)
                    .intersects(TypeFlags::INTERSECTION | TypeFlags::UNION)
                {
                    out.push(m);
                    continue;
                }
                let n = self.with_implied_constraints(at, m);
                changed |= n != m;
                out.push(n);
            }
            if changed {
                return if f.contains(TypeFlags::UNION) {
                    self.union(&out)
                } else {
                    self.intersection(&out)
                };
            }
        }
        // `C["a"]` where `C` is narrowed by an enclosing `C extends { a: X }` also reads
        // as `C["a"] & X`.
        if let TypeKind::IndexedAccess { obj: io, index: ii } = self.store().kind(t)
            && let Node::IndexedAccessType { obj: inner, .. } = *self.node(at)
        {
            let n = self.with_implied_constraints(Loc::new(at.file, inner), io);
            if n != io && self.store().flags(n).contains(TypeFlags::INTERSECTION) {
                let mut parts = vec![t];
                let ms = self.store().members_of_union(n).to_vec();
                for m in ms {
                    if !self.is_generic_type(m)
                        && let Some(x) = self.quietly(|ck| ck.indexed_access_type(m, ii, None))
                        && !self.store().is_error(x)
                    {
                        parts.push(x);
                    }
                }
                if parts.len() > 1 {
                    return self.intersection(&parts);
                }
            }
        }
        t
    }

    pub(crate) fn every_array_or_tuple(&mut self, t: TypeId) -> bool {
        let s = self.store();
        let ms = if s.flags(t).contains(TypeFlags::UNION) {
            s.members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        ms.into_iter()
            .all(|m| self.c.is_array_type(m) || self.store().tuple_shape_of(m).is_some())
    }

    fn implied_constraint(&mut self, t: TypeId, check: Loc, extends: Loc) -> Option<TypeId> {
        if let (Node::TupleType { elems: ce }, Node::TupleType { elems: ee }) =
            (*self.node(check), *self.node(extends))
        {
            let ast = self.ast(check.file);
            let (ce, ee) = (ast.list(ce), ast.list(ee));
            if ce.len() == 1 && ee.len() == 1 {
                return self.implied_constraint(
                    t,
                    Loc::new(check.file, ce[0]),
                    Loc::new(check.file, ee[0]),
                );
            }
        }
        (self.type_from_node(check) == t).then(|| self.type_from_node(extends))
    }

    /// Whether `at` is in the true branch of a conditional type that checks `t`
    /// (`A extends Record<'foo', string> ? A['foo'] : never` reads `A` as narrowed).
    fn in_true_branch_checking(&mut self, at: Loc, t: TypeId) -> bool {
        let mut cur = at;
        for _ in 0..200 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            if let Node::ConditionalType { check, true_ty, .. } = *self.node(p)
                && true_ty == cur.node
                && self.type_from_node(Loc::new(at.file, check)) == t
            {
                return true;
            }
            cur = p;
        }
        false
    }

    /// `O[K]` in a type with a key `O` lacks: TS2339 for a literal key, TS2537 for `string`
    /// or `number` without an index signature, TS2538 for a key that cannot index at all
    /// (`any` without a string index signature, `boolean`, objects). Each failing member of
    /// a union key is reported. Generic objects and keys are checked where instantiated.
    fn check_indexed_access_type_key(&mut self, o: TypeId, i: TypeId, index: Loc) {
        let s = self.store();
        // `any`, `unknown` and `never` take any key that can index (observed: `any[[]]`,
        // `any[boolean]` and `never[null]` are TS2538). So do an unresolved object and a
        // primitive one (`string[{}]`): a key that is no property name is TS2538 whatever
        // the object (tsc: checker.go getPropertyTypeForIndexType).
        let primitive_or_error = o == TypeId::UNRESOLVED
            || s.is_error(o)
            || (s.object_of(o).is_none()
                && !s.flags(o).intersects(
                    TypeFlags::INTERSECTION | TypeFlags::UNION | TypeFlags::INSTANTIABLE,
                ));
        if (o == TypeId::UNKNOWN
            || primitive_or_error
            || s.flags(o).intersects(TypeFlags::ANY | TypeFlags::NEVER))
            && i != TypeId::UNRESOLVED
            && !s.is_error(i)
            && !s.flags(i).intersects(TypeFlags::ANY | TypeFlags::NEVER)
            && !self.is_generic_type(i)
        {
            let keys = if i == TypeId::BOOLEAN || !s.flags(i).contains(TypeFlags::UNION) {
                vec![i]
            } else {
                s.members_of_union(i).to_vec()
            };
            for k in keys {
                if !self.store().flags(k).intersects(
                    TypeFlags::STRING_LIKE | TypeFlags::NUMBER_LIKE | TypeFlags::ES_SYMBOL_LIKE,
                ) {
                    let ks = self.type_to_string(k);
                    self.error(
                        index,
                        dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538,
                        &[&ks],
                    );
                }
            }
            return;
        }
        if s.flags(o).intersects(TypeFlags::ANY | TypeFlags::NEVER)
            || s.flags(i).intersects(TypeFlags::NEVER)
            || o == TypeId::UNRESOLVED
            || i == TypeId::UNRESOLVED
            || s.is_error(o)
            || s.is_error(i)
            || self.is_generic_type(o)
            || self.is_generic_type(i)
            || s.object_of(o).is_none()
                && !s
                    .flags(o)
                    .intersects(TypeFlags::INTERSECTION | TypeFlags::UNION)
        {
            return;
        }
        if i == TypeId::ANY {
            if self.index_info_of(o, TypeId::STRING).is_none() {
                self.error(
                    index,
                    dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538,
                    &["any"],
                );
            }
            return;
        }
        if s.flags(i).intersects(TypeFlags::ANY) {
            return;
        }
        // `boolean` is reported whole; inside a larger union, as `false` and `true`.
        let keys = if i == TypeId::BOOLEAN {
            vec![i]
        } else if s.flags(i).contains(TypeFlags::UNION) {
            s.members_of_union(i).to_vec()
        } else {
            vec![i]
        };
        for k in keys {
            let kf = self.store().flags(k);
            if k != TypeId::BOOLEAN && self.indexed_access_type(o, k, None).is_some() {
                continue;
            }
            if let Some(name) = self.property_name_of_type(k)
                && kf.intersects(TypeFlags::STRING_LITERAL | TypeFlags::NUMBER_LITERAL)
            {
                // `typeof globalThis` has itself as a property.
                if name == "globalThis" && o == self.global_this_type() {
                    continue;
                }
                let ts = self.type_to_string(o);
                self.error(
                    index,
                    dg::PROPERTY_0_DOES_NOT_EXIST_ON_TYPE_1_2339,
                    &[&name, &ts],
                );
            } else if k == TypeId::STRING || k == TypeId::NUMBER {
                let ts = self.type_to_string(o);
                let ks = self.type_to_string(k);
                self.error(
                    index,
                    dg::TYPE_0_HAS_NO_MATCHING_INDEX_SIGNATURE_FOR_TYPE_1_2537,
                    &[&ts, &ks],
                );
            } else if !kf.intersects(
                TypeFlags::STRING_LIKE | TypeFlags::NUMBER_LIKE | TypeFlags::ES_SYMBOL_LIKE,
            ) {
                let ks = self.type_to_string(k);
                self.error(
                    index,
                    dg::TYPE_0_CANNOT_BE_USED_AS_AN_INDEX_TYPE_2538,
                    &[&ks],
                );
            }
        }
    }

    /// JSX in a program without `jsx`: TS17004 at every opening element and fragment (the
    /// elements themselves are checked as expressions, `jsx_check.rs`).
    fn check_jsx_without_flag(&mut self, file: FileId) {
        let ast = self.ast(file);
        let mut stack = vec![ast.root];
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            match *ast.node(n) {
                Node::JsxOpening { .. } => {
                    self.error(
                        Loc::new(file, n),
                        dg::CANNOT_USE_JSX_UNLESS_THE_JSX_FLAG_IS_PROVIDED_17004,
                        &[],
                    );
                }
                Node::JsxFragment { .. } => {
                    let start = ast.span(n).start;
                    self.error_span(
                        file,
                        ts_r_ast::Span::new(start, start + 2),
                        dg::CANNOT_USE_JSX_UNLESS_THE_JSX_FLAG_IS_PROVIDED_17004,
                        &[],
                    );
                }
                _ => {}
            }
            ast.for_each_child(n, |c| stack.push(c));
        }
    }

    /// A global `JSX` namespace (its members are not inspected, so any declaration counts).
    pub(crate) fn has_global_jsx_intrinsics(&self) -> bool {
        self.c.link.global("JSX").is_some()
    }

    /// Whether the type annotation `ann` is a type reference with the wrong number of type
    /// arguments (TS2314/2315/2707), which tsc resolves to its error type. Reports nothing.
    fn is_type_ref_with_bad_arity(&mut self, ann: Loc) -> bool {
        let ast = self.ast(ann.file);
        let Node::TypeRef { name, type_args } = *ast.node(ann.node) else {
            return false;
        };
        let n_args = type_args.map_or(0, |ta| match *ast.node(ta) {
            Node::TypeArgs { args } => ast.list(args).len(),
            _ => 0,
        });
        let Some(sym) = self
            .resolve_type_name(Loc::new(ann.file, name))
            .and_then(|s| self.c.resolve_alias(s))
        else {
            return false;
        };
        if !self.c.has_meaning(sym, meaning::TYPE) {
            return false;
        }
        let flags = self.c.sym_flags(sym);
        if !flags.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE | SymbolFlags::TYPE_ALIAS)
        {
            return n_args > 0 && flags.intersects(SymbolFlags::ENUM | SymbolFlags::TYPE_PARAMETER);
        }
        let tps = self.local_type_params(sym);
        let mut min = tps.len();
        while min > 0 && self.has_type_param_default(tps[min - 1]) {
            min -= 1;
        }
        n_args < min || n_args > tps.len()
    }

    /// TS2838: every constrained `infer X` of one name in an `extends` clause has the same
    /// constraint (declarations without one are not compared).
    fn check_infer_constraints(&mut self, tp: Loc) {
        let Some(sym) = self.c.sym_of_decl(tp) else {
            return;
        };
        let decls = self.c.decls(sym).to_vec();
        if decls.len() < 2 {
            return;
        }
        let mut constraints = Vec::with_capacity(decls.len());
        for d in &decls {
            if let Node::TypeParam {
                constraint: Some(c),
                ..
            } = *self.node(*d)
            {
                constraints.push(self.type_from_node(Loc::new(d.file, c)));
            }
        }
        if constraints.iter().all(|&c| c == constraints[0]) {
            return;
        }
        if !matches!(
            self.node(tp),
            Node::TypeParam {
                constraint: Some(_),
                ..
            }
        ) {
            return;
        }
        if let Node::TypeParam { name, .. } = *self.node(tp) {
            let text = self.c.sym_name(sym).to_owned();
            self.error(
                Loc::new(tp.file, name),
                dg::ALL_DECLARATIONS_OF_0_MUST_HAVE_IDENTICAL_CONSTRAINTS_2838,
                &[&text],
            );
        }
    }

    /// The type argument count of a class's `extends` clause: a class base checks it like
    /// a type reference (TS2314, TS2707, TS2315, constraints); any other base needs a
    /// construct signature taking that many type arguments (TS2508).
    /// TS2507 when the `extends` expression `e` (of type `t`) is not a constructor: tsc 7
    /// accepts `any`, `null`, types with construct signatures (abstract ones too), type
    /// parameters constrained to those, and unions of accepted types; `undefined`,
    /// `unknown`, `never`, literals and plain functions or objects are errors.
    /// TS2612: with `useDefineForClassFields`, an instance field `m` of class `class` with
    /// no initializer, no `declare` and no assignment in the constructor redeclares a
    /// field (not an accessor, `accessor` field or method, not `declare`) of its base
    /// class, which the derived field will reset. `accessor` fields are not checked. The
    /// base prints as the `extends` type.
    fn check_base_property_overwrite(&mut self, class: Loc, m: NodeId, ctor_body: Option<NodeId>) {
        let file = class.file;
        if !self.c.opts.use_define_for_class_fields
            || !self.reporting(file)
            || self.c.file(file).is_js()
            || self.is_ambient_node(Loc::new(file, m))
        {
            return;
        }
        let ast = self.ast(file);
        let Node::PropertyDef { modifiers, key, .. } = *ast.node(m) else {
            return;
        };
        if modifiers.intersects(
            Modifiers::STATIC | Modifiers::DECLARE | Modifiers::ABSTRACT | Modifiers::ACCESSOR,
        ) {
            return;
        }
        let Some(name) = crate::nodes::ident_text(ast, key) else {
            return;
        };
        let Some(sym) = self.c.sym_of_decl(class) else {
            return;
        };
        let Some(&base) = self.base_types(sym).first() else {
            return;
        };
        if base == TypeId::UNRESOLVED || self.store().is_error(base) {
            return;
        }
        let Some(p) = self.property_of_type(base, name) else {
            return;
        };
        let p = match self.c.synth_sym(p) {
            Some(x) if x.origins.len() == 1 => x.origins[0],
            Some(_) => return,
            None => p,
        };
        let Some(d) = self.c.value_decl(p) else {
            return;
        };
        let base_field = match *self.ast(d.file).node(d.node) {
            Node::PropertyDef { modifiers, .. } => {
                !modifiers.intersects(Modifiers::DECLARE | Modifiers::ACCESSOR)
            }
            Node::Param { .. } => self.is_parameter_property(d),
            _ => false,
        };
        if !base_field || ctor_body.is_some_and(|b| self.assigns_this_property(file, b, key)) {
            return;
        }
        let mut base_name = self.type_to_string(base);
        // An unnamed class expression takes its variable's name.
        if let Some(bs) = self.class_symbol_of_instance(base)
            && let Some(bd) = self.c.first_decl(bs)
            && let Node::ClassExpr { name: None, .. } = *self.node(bd)
        {
            base_name = self.class_display_name(bd, None);
        }
        self.error(
            Loc::new(file, key),
            dg::PROPERTY_0_WILL_OVERWRITE_THE_BASE_PROPERTY_IN_1_IF_THIS_IS_INTENTIONAL_ADD_AN_I_2612,
            &[name, &base_name],
        );
    }

    /// `e` is an identifier or property access chain naming an interface, with no value
    /// of that name.
    fn extends_interface_only(&mut self, e: Loc) -> bool {
        let ast = self.ast(e.file);
        let mut cur = e.node;
        for _ in 0..64 {
            match *ast.node(cur) {
                Node::Ident { .. } => break,
                Node::Member {
                    obj,
                    optional: false,
                    prop,
                } if matches!(ast.node(prop), Node::Ident { .. }) => cur = obj,
                _ => return false,
            }
        }
        if !matches!(ast.node(cur), Node::Ident { .. }) {
            return false;
        }
        if matches!(ast.node(e.node), Node::Ident { .. })
            && self.c.resolve_ident(e, meaning::VALUE).is_some()
        {
            return false;
        }
        let Some(s) = self.resolve_entity(e, meaning::TYPE) else {
            return false;
        };
        let s = self.c.resolve_alias(s).unwrap_or(s);
        self.c.sym_flags(s).intersects(SymbolFlags::INTERFACE)
            && !self.c.has_meaning(s, meaning::VALUE)
    }

    fn check_base_constructor_type(&mut self, e: Loc, t: TypeId) {
        if !self.reporting(e.file) || t == TypeId::UNRESOLVED || self.store().is_error(t) {
            return;
        }
        if self.is_base_constructor(t, 0) {
            return;
        }
        let ts = self.type_to_string(t);
        self.error(
            e,
            dg::TYPE_0_IS_NOT_A_CONSTRUCTOR_FUNCTION_TYPE_2507,
            &[&ts],
        );
    }

    fn is_base_constructor(&mut self, t: TypeId, depth: u32) -> bool {
        let f = self.store().flags(t);
        if depth > 8 || f.intersects(TypeFlags::ANY | TypeFlags::NULL) || self.store().is_error(t) {
            return true;
        }
        if f.contains(TypeFlags::UNION) {
            let members = self.store().members_of_union(t).to_vec();
            return members
                .iter()
                .all(|&m| self.is_base_constructor(m, depth + 1));
        }
        // An unconstrained type parameter is an error; other instantiable types (and
        // constraints that are) are given the benefit of the doubt.
        if f.contains(TypeFlags::TYPE_PARAMETER) {
            return match self.type_param_constraint(t) {
                Some(c) if c != t => self.is_base_constructor(c, depth + 1),
                _ => false,
            };
        }
        if f.intersects(TypeFlags::INSTANTIABLE) {
            return true;
        }
        !self
            .signatures_of(t, crate::members::SigKind::Construct)
            .is_empty()
    }

    fn check_class_extends_type_args(&mut self, t: Loc, expr: NodeId, type_args: Option<NodeId>) {
        if self.c.file(t.file).is_js() {
            self.check_js_class_extends_type_args(t, expr, type_args);
            return;
        }
        if !self.reporting(t.file) {
            return;
        }
        let ast = self.ast(t.file);
        let n = type_args.map_or(0, |ta| match *ast.node(ta) {
            Node::TypeArgs { args } => ast.list(args).len(),
            _ => 0,
        });
        let el = Loc::new(t.file, expr);
        let class_sym = self
            .resolve_entity(el, meaning::VALUE)
            .and_then(|s| self.c.resolve_alias(s))
            .filter(|&s| self.c.sym_flags(s).intersects(SymbolFlags::CLASS));
        if let Some(cs) = class_sym {
            let ts = self.quietly(|ck| ck.resolve_type_name(el));
            if ts.and_then(|s| self.c.resolve_alias(s)) == Some(cs) {
                self.check_type_reference(t, expr, type_args);
            }
            return;
        }
        let ct = self.quietly(|ck| ck.check_expr(el));
        if self
            .store()
            .flags(ct)
            .intersects(TypeFlags::ANY | TypeFlags::NULL)
        {
            return;
        }
        let sigs = self.signatures_of(ct, crate::members::SigKind::Construct);
        if sigs.is_empty() {
            return;
        }
        let fits = sigs.iter().any(|&s| {
            let tps = self
                .store()
                .list_items(self.store().sig(s).type_params)
                .to_vec();
            self.type_arg_count_fits(&tps, n)
        });
        if !fits {
            self.error(
                t,
                dg::NO_BASE_CONSTRUCTOR_HAS_THE_SPECIFIED_NUMBER_OF_TYPE_ARGUMENTS_2508,
                &[],
            );
        }
    }

    /// JavaScript: a generic base class whose type arguments (from `@extends`/`@augments`)
    /// are missing or too many is TS8026 (TS8027 when some have defaults) at the `extends`
    /// expression, under `noImplicitAny` only (tsc 7; the missing ones are `any`).
    fn check_js_class_extends_type_args(
        &mut self,
        t: Loc,
        expr: NodeId,
        type_args: Option<NodeId>,
    ) {
        if !self.c.opts.no_implicit_any || !self.reporting(t.file) {
            return;
        }
        let ast = self.ast(t.file);
        let n = type_args.map_or(0, |ta| match *ast.node(ta) {
            Node::TypeArgs { args } => ast.list(args).len(),
            _ => 0,
        });
        let el = Loc::new(t.file, expr);
        let Some(cs) = self
            .quietly(|ck| ck.resolve_entity(el, meaning::VALUE))
            .and_then(|s| self.c.resolve_alias(s))
            .filter(|&s| self.c.sym_flags(s).intersects(SymbolFlags::CLASS))
        else {
            return;
        };
        let tps = self.local_type_params(cs);
        if tps.is_empty() {
            return;
        }
        let mut min = tps.len();
        while min > 0 && self.has_type_param_default(tps[min - 1]) {
            min -= 1;
        }
        if n >= min && n <= tps.len() {
            return;
        }
        let d = self.declared_type_of_symbol(cs);
        let ts = self.type_to_string_no_args(d, cs);
        if min == tps.len() {
            self.error(
                el,
                dg::EXPECTED_0_TYPE_ARGUMENTS_PROVIDE_THESE_WITH_AN_EXTENDS_TAG_8026,
                &[&ts],
            );
        } else {
            // tsc 7 prints the type and the minimum count: `Expected D<T, U>-1 ...`.
            let m = min.to_string();
            self.error(
                el,
                dg::EXPECTED_0_1_TYPE_ARGUMENTS_PROVIDE_THESE_WITH_AN_EXTENDS_TAG_8027,
                &[&ts, &m],
            );
        }
    }

    fn check_type_reference(&mut self, r: Loc, name: NodeId, type_args: Option<NodeId>) {
        let file = r.file;
        let ast = self.ast(file);
        let nl = Loc::new(file, name);
        // Resolving the reference may hit the instantiation limits (TS2589 here).
        if type_args.is_some() && matches!(ast.node(r.node), Node::TypeRef { .. }) {
            self.inst_count = 0;
            let _ = self.type_from_node(r);
            self.report_instantiation_overflow(r, false);
        }
        let n_args = type_args.map_or(0, |ta| match *ast.node(ta) {
            Node::TypeArgs { args } => ast.list(args).len(),
            _ => 0,
        });
        // JSDoc's own names (see `jsdoc_type_reference`): `Void`, `Undefined` and `Null`
        // are never generic; `array` and `promise` without arguments are `any[]` and
        // `Promise<any>` when `noImplicitAny` is off (observed tsc 7).
        if ast.is_jsdoc(name)
            && let Some(text) = ast.name_of(name)
            && matches!(text, "Void" | "Undefined" | "Null" | "array" | "promise")
            && self.c.resolve_ident(nl, meaning::TYPE).is_none()
        {
            match text {
                "array" | "promise" if n_args > 0 || self.c.opts.no_implicit_any => {}
                "array" | "promise" => return,
                _ => {
                    if n_args > 0 {
                        self.error(r, dg::TYPE_0_IS_NOT_GENERIC_2315, &[text]);
                    }
                    return;
                }
            }
        }
        let Some(sym) = self.resolve_type_name(nl) else {
            return;
        };
        let Some(sym) = self.c.resolve_alias(sym) else {
            return;
        };
        if !self.c.has_meaning(sym, meaning::TYPE) {
            return;
        }
        let flags = self.c.sym_flags(sym);
        if flags.intersects(SymbolFlags::TYPE_PARAMETER) {
            self.check_static_type_parameter_use(nl, sym);
        }
        if !flags.intersects(SymbolFlags::CLASS | SymbolFlags::INTERFACE | SymbolFlags::TYPE_ALIAS)
        {
            if n_args > 0 && flags.intersects(SymbolFlags::ENUM | SymbolFlags::TYPE_PARAMETER) {
                let text = crate::nodes::entity_text(ast, name);
                let _ = self.declared_type_of_symbol(sym);
                let shown = text;
                self.error(r, dg::TYPE_0_IS_NOT_GENERIC_2315, &[&shown]);
            }
            return;
        }
        let tps = self.local_type_params(sym);
        let min = {
            let mut m = tps.len();
            while m > 0 && self.has_type_param_default(tps[m - 1]) {
                m -= 1;
            }
            m
        };
        if tps.is_empty() {
            if n_args > 0 && !self.is_jsdoc_record(r) {
                let d = self.declared_type_of_symbol(sym);
                let ts = self.type_to_string(d);
                self.error(r, dg::TYPE_0_IS_NOT_GENERIC_2315, &[&ts]);
            }
            return;
        }
        // A JSDoc reference to a generic class or interface takes `any` for missing type
        // arguments and ignores extra ones; only `noImplicitAny` reports it (aliases always).
        if ast.is_jsdoc(r.node)
            && !self.c.opts.no_implicit_any
            && (n_args == 0 || !flags.intersects(SymbolFlags::TYPE_ALIAS))
        {
            return;
        }
        if n_args < min || n_args > tps.len() {
            // Inside the declaring class/interface a bare reference is allowed in JS only.
            let d = self.declared_type_of_symbol(sym);
            let ts = self.type_to_string_no_args(d, sym);
            if min == tps.len() {
                self.error(
                    r,
                    dg::GENERIC_TYPE_0_REQUIRES_1_TYPE_ARGUMENT_S_2314,
                    &[&ts, &min.to_string()],
                );
            } else {
                self.error(
                    r,
                    dg::GENERIC_TYPE_0_REQUIRES_BETWEEN_1_AND_2_TYPE_ARGUMENTS_2707,
                    &[&ts, &min.to_string(), &tps.len().to_string()],
                );
            }
            return;
        }
        // Constraints.
        let Some(ta) = type_args else { return };
        let Node::TypeArgs { args } = *ast.node(ta) else {
            return;
        };
        let arg_nodes = ast.list(args).to_vec();
        let arg_types: Vec<TypeId> = arg_nodes
            .iter()
            .map(|&a| self.type_from_node(Loc::new(file, a)))
            .collect();
        let filled = self.fill_type_args(&tps, &arg_types);
        let m = self.store().mapper(&tps, &filled);
        for (i, &a) in arg_nodes.iter().enumerate() {
            let Some(c) = self.type_param_constraint(tps[i]) else {
                continue;
            };
            let c = self.instantiate(c, m);
            let at = arg_types[i];
            if self.store().is_error(at) || at == TypeId::UNRESOLVED || c == TypeId::UNRESOLVED {
                continue;
            }
            // `infer X` parameters take implied constraints from where they appear.
            if self.is_infer_type_param(at) {
                continue;
            }
            if !self.is_assignable(at, c) {
                // Inside a conditional's true branch the argument (and the constraint,
                // instantiated with the other arguments) is narrowed first.
                let at = self.narrowed_in_true_branch(Loc::new(file, a), at);
                if self.is_assignable(at, c) {
                    continue;
                }
                let nc = self.narrowed_in_true_branch(Loc::new(file, a), c);
                if nc != c && self.is_assignable(at, nc) {
                    continue;
                }
                // A mapped type's key over arrays or tuples is a numeric key.
                let wi = self.with_implied_constraints(Loc::new(file, a), at);
                if wi != at && self.is_assignable(wi, c) {
                    continue;
                }
                let (chain, _) = self.relation_error_chain(
                    at,
                    c,
                    ts_r_types::RelationKind::Assignable,
                    Some(crate::relate::Head {
                        info: dg::TYPE_0_DOES_NOT_SATISFY_THE_CONSTRAINT_1_2344,
                    }),
                );
                self.error_chain(Loc::new(file, a), &chain);
                // Only the first failing type argument is reported (observed tsc 7).
                return;
            }
        }
    }

    /// The type argument `at` at `a` with each enclosing conditional type whose true branch
    /// holds it narrowing its check type parameter: inside `T extends X ? <here> : ...`, `T`
    /// is `X & T` (tsc's implied constraint).
    fn narrowed_in_true_branch(&mut self, a: Loc, at: TypeId) -> TypeId {
        let ast = self.ast(a.file);
        let mut cur = a;
        let mut params = Vec::new();
        let mut implied = Vec::new();
        for _ in 0..64 {
            let Some(p) = self.c.parent(cur) else { break };
            match *ast.node(p.node) {
                Node::ConditionalType {
                    check,
                    extends,
                    true_ty,
                    ..
                } if true_ty == cur.node => {
                    // `[T] extends [X]` narrows `T` like `T extends X`.
                    let (check, extends) = match (*ast.node(check), *ast.node(extends)) {
                        (Node::TupleType { elems: c }, Node::TupleType { elems: e })
                            if ast.list(c).len() == 1
                                && ast.list(e).len() == 1
                                && [ast.list(c)[0], ast.list(e)[0]].iter().all(|&n| {
                                    !matches!(
                                        ast.node(n),
                                        Node::RestType { .. }
                                            | Node::OptionalType { .. }
                                            | Node::NamedTupleMember { .. }
                                    )
                                }) =>
                        {
                            (ast.list(c)[0], ast.list(e)[0])
                        }
                        _ => (check, extends),
                    };
                    let ct = self.type_from_node(Loc::new(a.file, check));
                    if self.store().flags(ct).contains(TypeFlags::TYPE_PARAMETER) {
                        let et = self.type_from_node(Loc::new(a.file, extends));
                        if !self.store().is_error(et) && et != TypeId::UNRESOLVED {
                            // Nested branches on the same parameter narrow it further.
                            if let Some(i) = params.iter().position(|&p| p == ct) {
                                implied[i] = self.intersection(&[implied[i], et]);
                            } else {
                                let both = self.intersection(&[et, ct]);
                                params.push(ct);
                                implied.push(both);
                            }
                        }
                    } else if ct == at
                        && self
                            .store()
                            .traits(ct)
                            .contains(ts_r_types::TypeTraits::CONTAINS_TYPE_PARAMETER)
                    {
                        // `T["k"] extends X ? F<T["k"]> : ...`: the argument is `T["k"] & X`
                        // (so is any generic check type: `[A, B] extends unknown[][] ?
                        // Bar<[A, B]>`, `Set<A> extends Set<unknown[]> ? Bar<Set<A>>`).
                        let et = self.type_from_node(Loc::new(a.file, extends));
                        if !self.store().is_error(et) && et != TypeId::UNRESOLVED {
                            return self.intersection(&[et, at]);
                        }
                    }
                }
                Node::TypeAlias { .. }
                | Node::Interface { .. }
                | Node::FunctionDecl { .. }
                | Node::ClassDecl { .. } => break,
                _ => {}
            }
            cur = p;
        }
        if params.is_empty() {
            return at;
        }
        let m = self.store().mapper(&params, &implied);
        self.instantiate(at, m)
    }

    /// Whether the module export an import alias `s` names (before following further
    /// aliases) declares a type or namespace itself: `export default interface I` merged
    /// with `export { v as default }`. Other aliases count as having one.
    /// Whether alias `s` imports a whole module without `export =` (`import * as N from
    /// "m"`, `import W = require("m")`).
    fn is_whole_module_import(&self, s: ts_r_types::SymId) -> bool {
        let crate::symbols::SymKind::Binder(file, local) = self.c.sym_kind(s) else {
            return false;
        };
        let b = self.c.bound(file);
        let Some(info) = b.alias(local) else {
            return false;
        };
        let ts_r_binder::AliasTarget::Module {
            request,
            import: ts_r_binder::ImportName::Namespace | ts_r_binder::ImportName::ExportEquals,
        } = info.target
        else {
            return false;
        };
        let target = self.c.link.module_target(file, request);
        self.c.link.resolve_export(target, "export=").is_none()
    }

    fn import_export_has_type(&self, s: ts_r_types::SymId) -> bool {
        let crate::symbols::SymKind::Binder(file, local) = self.c.sym_kind(s) else {
            return true;
        };
        let b = self.c.bound(file);
        let Some(info) = b.alias(local) else {
            return true;
        };
        let ts_r_binder::AliasTarget::Module { request, import } = info.target else {
            return true;
        };
        let name = match import {
            ts_r_binder::ImportName::Default => "default",
            ts_r_binder::ImportName::Named(n) => b.name_text(self.ast(file), n),
            _ => return true,
        };
        let target = self.c.link.module_target(file, request);
        let Some(r) = self.c.link.resolve_export(target, name) else {
            return true;
        };
        let flags = self.c.sym_flags(self.c.sym_ref(r));
        crate::symbols::symbol_meaning(flags.without(SymbolFlags::ALIAS))
            & (meaning::TYPE | meaning::NAMESPACE)
            != 0
    }

    fn type_to_string_no_args(&mut self, d: TypeId, sym: ts_r_types::SymId) -> String {
        let tps = self.local_type_params(sym);
        let name = self.c.sym_name(sym).to_owned();
        if tps.is_empty() {
            return self.type_to_string(d);
        }
        // A type alias is named without its type parameters.
        if self.c.sym_flags(sym).intersects(SymbolFlags::TYPE_ALIAS) {
            return name;
        }
        let ps: Vec<String> = tps.iter().map(|&t| self.type_to_string(t)).collect();
        format!("{}<{}>", name, ps.join(", "))
    }

    /// Resolves a type name, reporting TS2304/2503/2694/2749.
    fn resolve_type_name(&mut self, l: Loc) -> Option<ts_r_types::SymId> {
        let ast = self.ast(l.file);
        // `M` for `const M = require("m")`: the module's class, or TS2749.
        if let Some(r) = self.jsdoc_required_module_type(l) {
            if r.is_err()
                && let Some(text) = ast.name_of(l.node)
            {
                self.error(l, dg::_0_REFERS_TO_A_VALUE_BUT_IS_BEING_USED_AS_A_TYPE_HERE_DID_YOU_MEAN_TYPEOF_0_2749, &[text]);
            }
            return r.ok().flatten();
        }
        if self.jsdoc_require_name(l) && self.c.resolve_ident(l, meaning::TYPE).is_none() {
            return None;
        }
        // JSDoc `BigInt` is `bigint` even without the global (see jsdoc.rs).
        if ast.is_jsdoc(l.node) && ast.name_of(l.node) == Some("BigInt") {
            return self.c.resolve_ident(l, meaning::TYPE);
        }
        match *ast.node(l.node) {
            Node::Ident { name } => {
                let text = ast.atom(name);
                if let Some(s) = self.c.resolve_ident(l, meaning::TYPE) {
                    if self.type_from_signature_to_body(l, s) || self.infer_in_own_extends(l, s) {
                        self.report_cannot_find_type_name(l, text);
                        return None;
                    }
                    // An import of a value only (`import { v }`, `@import { v }`), not merged
                    // with a local type: TS2749. (An `export =` class expression is a type.)
                    let flags = self.c.sym_flags(s);
                    if flags.intersects(SymbolFlags::ALIAS)
                        && crate::symbols::symbol_meaning(flags.without(SymbolFlags::ALIAS))
                            & (meaning::TYPE | meaning::NAMESPACE)
                            == 0
                        && let Some(t) = self.c.resolve_alias(s)
                        && t != s
                        && self.c.sym_name(t) != "export="
                        && !self.import_export_has_type(s)
                        && !self.c.has_meaning(t, meaning::TYPE | meaning::NAMESPACE)
                        && self.c.has_meaning(t, meaning::VALUE)
                    {
                        self.error(l, dg::_0_REFERS_TO_A_VALUE_BUT_IS_BEING_USED_AS_A_TYPE_HERE_DID_YOU_MEAN_TYPEOF_0_2749, &[text]);
                        return None;
                    }
                    // An import of a module or namespace with no type meaning (`import W =
                    // require("m")`, `import * as N from "m"`, `import n = N`,
                    // `import { N }`): TS2709.
                    if flags.intersects(SymbolFlags::ALIAS)
                        && crate::symbols::symbol_meaning(flags.without(SymbolFlags::ALIAS))
                            & meaning::TYPE
                            == 0
                        && let Some(t) = self.c.resolve_alias(s)
                        && t != s
                        && self.c.sym_name(t) != "export="
                        && !self.c.has_meaning(t, meaning::TYPE)
                        && self.c.has_meaning(t, meaning::NAMESPACE)
                        && (self.is_whole_module_import(s)
                            || (self.c.sym_flags(t).intersects(SymbolFlags::MODULE)
                                && !self.c.sym_flags(t).intersects(SymbolFlags::MODULE_FILE)))
                    {
                        self.error(l, dg::CANNOT_USE_NAMESPACE_0_AS_A_TYPE_2709, &[text]);
                        return None;
                    }
                    return Some(s);
                }
                // A namespace (instantiated or not) without a type meaning: TS2709. Aliases
                // and module files (a UMD global for an `export =`) are left as they were.
                if let Some(s) = self.c.resolve_ident(l, meaning::NAMESPACE)
                    && !self
                        .c
                        .sym_flags(s)
                        .intersects(SymbolFlags::ALIAS | SymbolFlags::MODULE_FILE)
                {
                    self.error(l, dg::CANNOT_USE_NAMESPACE_0_AS_A_TYPE_2709, &[text]);
                    return None;
                }
                // (`exports` and `module` are values in a CommonJS module.)
                let commonjs_value = matches!(text, "exports" | "module")
                    && self.c.file(l.file).is_js()
                    && self.is_commonjs_js_file(l.file);
                if commonjs_value || self.c.resolve_ident(l, meaning::VALUE).is_some() {
                    self.error(l, dg::_0_REFERS_TO_A_VALUE_BUT_IS_BEING_USED_AS_A_TYPE_HERE_DID_YOU_MEAN_TYPEOF_0_2749, &[text]);
                    return None;
                }
                self.report_cannot_find_type_name(l, text);
                None
            }
            Node::QualifiedName { left, right }
            | Node::Member {
                obj: left,
                prop: right,
                ..
            } => {
                let ll = Loc::new(l.file, left);
                if let Some(g) = self.global_this_member(ll, right, meaning::TYPE) {
                    return g;
                }
                let ls = self.resolve_namespace_name(ll, Some(right))?;
                let name = ident_text(ast, right)?;
                if let Some(r) = self.c.export_of(ls, name) {
                    // A member without a type meaning: a value is TS2749 (at the whole
                    // name), a namespace holding only types is not exported as a type.
                    if let Some(t) = self.c.resolve_alias(r)
                        && !self.c.has_meaning(t, meaning::TYPE)
                    {
                        let text = crate::nodes::entity_text(ast, l.node);
                        if self.c.has_meaning(t, meaning::VALUE) {
                            self.error(l, dg::_0_REFERS_TO_A_VALUE_BUT_IS_BEING_USED_AS_A_TYPE_HERE_DID_YOU_MEAN_TYPEOF_0_2749, &[&text]);
                        } else {
                            let left_text = self.namespace_display(ls, ll);
                            self.error(
                                Loc::new(l.file, right),
                                dg::NAMESPACE_0_HAS_NO_EXPORTED_MEMBER_1_2694,
                                &[&left_text, name],
                            );
                        }
                        return None;
                    }
                    return Some(r);
                }
                // JSDoc `@typedef {T} o.Name` puts types on a plain variable (not modelled).
                if ast.is_jsdoc(l.node)
                    && !self.c.sym_flags(ls).intersects(
                        SymbolFlags::NAMESPACE_MODULE
                            | SymbolFlags::VALUE_MODULE
                            | SymbolFlags::MODULE_FILE
                            | SymbolFlags::REGULAR_ENUM
                            | SymbolFlags::CONST_ENUM
                            | SymbolFlags::CLASS,
                    )
                {
                    return None;
                }
                let left_text = self.namespace_display(ls, ll);
                self.report_missing_namespace_member(
                    Loc::new(l.file, right),
                    ls,
                    &left_text,
                    name,
                    meaning::TYPE,
                );
                None
            }
            _ => None,
        }
    }

    /// Resolves the namespace part of an entity name. `member` is the name accessed on it
    /// (for TS2713/TS2749).
    pub(crate) fn resolve_namespace_name(
        &mut self,
        l: Loc,
        member: Option<NodeId>,
    ) -> Option<ts_r_types::SymId> {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::Ident { name } => {
                let text = ast.atom(name);
                // A namespace of the name wins over a nearer value (a global `var x`
                // declared next to the file's `import * as x`); values are found for the
                // diagnostics below.
                let found = self
                    .c
                    .resolve_ident(l, meaning::NAMESPACE)
                    .or_else(|| self.c.resolve_ident(l, meaning::NAMESPACE | meaning::VALUE));
                match found {
                    Some(s) => {
                        // A namespace merged with an export specifier of its name
                        // (`export namespace X {}` and `export { Y as X }`) is that
                        // namespace, not the alias's target.
                        let own = self.c.sym_flags(s).without(SymbolFlags::ALIAS);
                        let r = match self.c.resolve_alias(s) {
                            _ if crate::symbols::symbol_meaning(own) & meaning::NAMESPACE != 0 => s,
                            Some(r) => r,
                            // A default import of a module without a default export is the
                            // module itself (synthetic default).
                            None => return self.synthetic_default_module(s),
                        };
                        let f = self.c.sym_flags(r);
                        // An import of a type alias or interface has no namespace meaning:
                        // a global namespace of that name (a UMD `export as namespace`)
                        // is used instead, or it is TS2702.
                        if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
                            && f.intersects(SymbolFlags::TYPE_ALIAS | SymbolFlags::INTERFACE)
                            && !f.intersects(
                                SymbolFlags::NAMESPACE_MODULE
                                    | SymbolFlags::VALUE_MODULE
                                    | SymbolFlags::MODULE_FILE
                                    | SymbolFlags::REGULAR_ENUM
                                    | SymbolFlags::CONST_ENUM
                                    | SymbolFlags::CLASS
                                    | SymbolFlags::ALIAS,
                            )
                        {
                            if let Some(g) = self.c.global_symbol(text, meaning::NAMESPACE) {
                                return self.c.resolve_alias(g);
                            }
                            self.error(
                                l,
                                dg::_0_ONLY_REFERS_TO_A_TYPE_BUT_IS_BEING_USED_AS_A_NAMESPACE_HERE_2702,
                                &[text],
                            );
                            return None;
                        }
                        // An import of an `export =` that is a plain value (`var x; export =
                        // x`) has no namespace meaning.
                        if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS)
                            && f.intersects(
                                SymbolFlags::FUNCTION_SCOPED_VARIABLE
                                    | SymbolFlags::BLOCK_SCOPED_VARIABLE,
                            )
                            && !f.intersects(
                                SymbolFlags::NAMESPACE_MODULE
                                    | SymbolFlags::VALUE_MODULE
                                    | SymbolFlags::MODULE_FILE
                                    | SymbolFlags::REGULAR_ENUM
                                    | SymbolFlags::CONST_ENUM
                                    | SymbolFlags::ALIAS,
                            )
                        {
                            self.error(l, dg::CANNOT_FIND_NAMESPACE_0_2503, &[text]);
                            return None;
                        }
                        // In JavaScript a variable or function (an expando container,
                        // `var Outer = {}; Outer.Inner = ..`) is no namespace either (tsc 7),
                        // unless it holds `require("m")` or roots a dotted `@typedef` name.
                        if self.c.file(l.file).is_js()
                            && f.intersects(SymbolFlags::VARIABLE | SymbolFlags::FUNCTION)
                            && !f.intersects(
                                SymbolFlags::NAMESPACE_MODULE
                                    | SymbolFlags::VALUE_MODULE
                                    | SymbolFlags::MODULE_FILE
                                    | SymbolFlags::ENUM
                                    | SymbolFlags::CLASS
                                    | SymbolFlags::ALIAS
                                    | SymbolFlags::TYPE_ALIAS
                                    | SymbolFlags::INTERFACE,
                            )
                            && !self.is_require_binding(r)
                            && !self.is_jsdoc_namespace_root(r, text)
                        {
                            self.error(l, dg::CANNOT_FIND_NAMESPACE_0_2503, &[text]);
                            return None;
                        }
                        self.namespace_ident_without_namespace(l, s, r, text, member)
                    }
                    None => {
                        match self.c.resolve_ident(l, meaning::TYPE) {
                            None => self.report_cannot_find_namespace(l, text),
                            Some(t) => self.type_used_as_namespace(l, t, text, member),
                        }
                        None
                    }
                }
            }
            Node::QualifiedName { left, right }
            | Node::Member {
                obj: left,
                prop: right,
                ..
            } => {
                if let Some(g) =
                    self.global_this_member(Loc::new(l.file, left), right, meaning::NAMESPACE)
                {
                    return g;
                }
                let ll = Loc::new(l.file, left);
                let ls = self.resolve_namespace_name(ll, Some(right))?;
                let name = ident_text(ast, right)?;
                match self.c.export_of(ls, name) {
                    Some(r) => {
                        let t = self.c.resolve_alias(r)?;
                        self.namespace_member_without_namespace(l, ls, t, member)
                    }
                    None => {
                        let left_text = self.namespace_display(ls, ll);
                        self.report_missing_namespace_member(
                            Loc::new(l.file, right),
                            ls,
                            &left_text,
                            name,
                            meaning::NAMESPACE,
                        );
                        None
                    }
                }
            }
            _ => None,
        }
    }

    /// Whether a file declaring `s` has a dotted `@typedef`/`@callback` name rooted at
    /// `text` (`Ast::jsdoc_namespaces`), which gives it namespace meaning (not modelled).
    fn is_jsdoc_namespace_root(&self, s: ts_r_types::SymId, text: &str) -> bool {
        self.c.decls(s).into_iter().any(|d| {
            let ast = self.ast(d.file);
            ast.jsdoc_namespaces.iter().any(|&a| ast.atom(a) == text)
        })
    }

    /// The module a default import `s` stands for when the module has no default export
    /// and synthetic default imports are allowed.
    fn synthetic_default_module(&mut self, s: ts_r_types::SymId) -> Option<ts_r_types::SymId> {
        self.c.synthetic_default_module(s)
    }

    /// `globalThis.X` in a type position (when `globalThis` is not declared locally): the
    /// global `X` of that meaning. `Some(None)` reports TS2694 for a missing global.
    pub(crate) fn global_this_member(
        &mut self,
        left: Loc,
        right: NodeId,
        m: u8,
    ) -> Option<Option<ts_r_types::SymId>> {
        let ast = self.ast(left.file);
        if !matches!(*ast.node(left.node), Node::Ident { name } if ast.atom(name) == "globalThis")
            || self.c.resolve_ident(left, meaning::ALL).is_some()
        {
            return None;
        }
        let name = ident_text(ast, right)?;
        match self.c.global_symbol(name, m) {
            Some(s) => Some(Some(s)),
            None => {
                self.error(
                    Loc::new(left.file, right),
                    dg::NAMESPACE_0_HAS_NO_EXPORTED_MEMBER_1_2694,
                    &["globalThis", name],
                );
                Some(None)
            }
        }
    }

    fn report_cannot_find_type_name(&mut self, l: Loc, name: &str) {
        if let Some(info) = self.primitive_heritage_error(l, name) {
            self.error(l, info, &[name]);
            return;
        }
        // The missing-types hints apply to type names too (observed), except `require`,
        // which is spell-checked (to `Required`).
        if name != "require"
            && let Some((info, lib)) = crate::expr::missing_types_hint(name)
        {
            self.error(l, info, &[name, lib]);
            return;
        }
        if let Some(s) = self.type_name_suggestion(l, name) {
            self.error(l, dg::CANNOT_FIND_NAME_0_DID_YOU_MEAN_1_2552, &[name, &s]);
            return;
        }
        self.error(l, dg::CANNOT_FIND_NAME_0_2304, &[name]);
    }

    /// TS2840/TS2863/TS2864: `interface I extends number`, `class C extends number`,
    /// `class C implements string`.
    pub(crate) fn primitive_heritage_error(
        &self,
        l: Loc,
        name: &str,
    ) -> Option<dg::DiagnosticInfo> {
        if !matches!(
            name,
            "number" | "string" | "boolean" | "any" | "unknown" | "never"
        ) {
            return None;
        }
        Some(match self.heritage_kind_of(l)? {
            (HeritageKind::Implements, _) => {
                dg::A_CLASS_CANNOT_IMPLEMENT_A_PRIMITIVE_TYPE_LIKE_0_IT_CAN_ONLY_IMPLEMENT_OTHER_NAM_2864
            }
            (HeritageKind::Extends, false) => {
                dg::AN_INTERFACE_CANNOT_EXTEND_A_PRIMITIVE_TYPE_LIKE_0_IT_CAN_ONLY_EXTEND_OTHER_NAME_2840
            }
            (HeritageKind::Extends, true) => {
                dg::A_CLASS_CANNOT_EXTEND_A_PRIMITIVE_TYPE_LIKE_0_CLASSES_CAN_ONLY_EXTEND_CONSTRUCTA_2863
            }
        })
    }

    /// For a name that is a whole heritage clause entry: the clause kind, and whether the
    /// clause belongs to a class.
    pub(crate) fn heritage_kind_of(&self, l: Loc) -> Option<(HeritageKind, bool)> {
        let ast = self.ast(l.file);
        let ewta = self.c.parent(l)?;
        if !matches!(*ast.node(ewta.node), Node::ExprWithTypeArgs { expr, .. } if expr == l.node) {
            return None;
        }
        let clause = self.c.parent(ewta)?;
        let kind = match *ast.node(clause.node) {
            Node::HeritageClause { kind, .. } => kind,
            Node::Interface { .. } => return Some((HeritageKind::Extends, false)),
            _ => return None,
        };
        let owner = self.c.parent(clause)?;
        let is_class = matches!(
            *ast.node(owner.node),
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
        );
        Some((kind, is_class))
    }

    /// The target of alias `s` when it has no value meaning and no value merged along the
    /// way (`import x = N` of a namespace holding only types).
    fn alias_without_value(&self, s: ts_r_types::SymId) -> Option<ts_r_types::SymId> {
        let f = self.c.sym_flags(s);
        if !f.intersects(SymbolFlags::ALIAS)
            || crate::symbols::symbol_meaning(f.without(SymbolFlags::ALIAS)) & meaning::VALUE != 0
        {
            return None;
        }
        // Only `import x = N.M`: a named import from an `export =` module can also name a
        // property of the exported value, which the alias target does not show.
        let d = self.c.first_decl(s)?;
        if !matches!(self.ast(d.file).node(d.node), Node::ImportEquals { .. }) {
            return None;
        }
        let t = self.c.resolve_alias(s)?;
        (t != s
            && !self.c.has_meaning(t, meaning::VALUE)
            && self.c.value_merged_in_alias_chain(s).is_none()
            && self.alias_merged_value(s).is_none())
        .then_some(t)
    }

    /// `l` (text `text`) read as a value names `t`, which has no value: TS2708 for a
    /// namespace (also one merged with a type), TS2693 for a type.
    pub(crate) fn report_non_value(&mut self, l: Loc, t: ts_r_types::SymId, text: &str) {
        if self.c.has_meaning(t, meaning::TYPE)
            && !self
                .c
                .sym_flags(t)
                .intersects(SymbolFlags::NAMESPACE_MODULE)
        {
            self.error(l, crate::expr::type_as_value_info(text), &[text]);
        } else {
            self.error(l, dg::CANNOT_USE_NAMESPACE_0_AS_A_VALUE_2708, &[text]);
        }
    }

    /// `typeof a.b`: the entity must resolve as a value.
    fn check_type_query_name(&mut self, l: Loc) {
        let ast = self.ast(l.file);
        match *ast.node(l.node) {
            Node::Ident { name } => {
                let text = ast.atom(name);
                if text == "this" || text == "undefined" || text == "globalThis" {
                    return;
                }
                // A module's default export is no name in its scope (`typeof default`).
                if text == "default" {
                    self.error(l, dg::CANNOT_FIND_NAME_0_2304, &["default"]);
                    return;
                }
                // `arguments` exists in functions other than arrows, not in signatures.
                if text == "arguments" && self.c.resolve_ident(l, meaning::VALUE).is_none() {
                    if !self.in_non_arrow_function(l) {
                        self.error(l, dg::CANNOT_FIND_NAME_0_2304, &["arguments"]);
                    }
                    return;
                }
                if let Some(s) = self.c.resolve_ident(l, meaning::VALUE) {
                    let text = text.to_owned();
                    // An alias with no value is skipped by a value lookup, which goes on
                    // to the globals (`import {$}` of an interface next to a global `$`).
                    if let Some(t) = self.alias_without_value(s) {
                        let global = self.c.global_symbol(&text, meaning::VALUE);
                        if global.is_none_or(|g| g == s || self.alias_without_value(g).is_some()) {
                            self.report_non_value(l, t, &text);
                        }
                        return;
                    }
                    self.check_body_local_from_parameter(l, s, &text);
                    self.check_constructor_shadowed_reference(l, Some(s), &text);
                } else {
                    // A type or a namespace with no values (TS2693 / TS2708).
                    if let Some(t) = self.c.resolve_ident(l, meaning::TYPE | meaning::NAMESPACE) {
                        let t = self.c.resolve_alias(t).unwrap_or(t);
                        self.report_non_value(l, t, text);
                        return;
                    }
                    let text = text.to_owned();
                    if self.check_constructor_shadowed_reference(l, None, &text) {
                        return;
                    }
                    self.report_cannot_find_name(l, &text);
                }
            }
            Node::QualifiedName { left, .. } => {
                let mut root = left;
                for _ in 0..64 {
                    match *ast.node(root) {
                        Node::QualifiedName { left, .. } => root = left,
                        _ => break,
                    }
                }
                if let Node::Ident { name } = *ast.node(root) {
                    let text = ast.atom(name);
                    // `typeof module.exports.x` in a CommonJS JavaScript file (not modelled).
                    let cjs = matches!(text, "module" | "exports")
                        && self.c.file(l.file).is_js()
                        && self.is_commonjs_js_file(l.file);
                    let rl = Loc::new(l.file, root);
                    if text != "this" && text != "globalThis" && !cjs {
                        match self
                            .c
                            .resolve_ident(rl, meaning::VALUE | meaning::NAMESPACE)
                        {
                            None => self.report_cannot_find_name(rl, text),
                            // The root is read as a value: an `import x = N` alias of a
                            // namespace without values is TS2708.
                            Some(s) => {
                                if let Some(t) = self.alias_without_value(s) {
                                    let text = text.to_owned();
                                    self.report_non_value(rl, t, &text);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------------------------
    // Modules
    // ---------------------------------------------------------------------------------

    /// `true` inside the type arguments of an `import("m").X<...>` whose module has no
    /// types: tsc does not look at those arguments (observed: the second
    /// `import("react")` in `import("react").A<import("react").B>` gets no TS7016).
    /// The hint under TS7016 for `spec`, resolved to JavaScript of a package (tsc:
    /// tsc/internal/checker/utilities.go CreateModuleNotFoundChain): declarations found
    /// when `exports` is ignored are named; else the `@types` package other imports
    /// resolved into is to be amended; else, when imports of the package found its own
    /// `.d.ts`, a `declare module` is suggested; else `npm i @types/...`.
    fn untyped_package_hint(
        &self,
        chain: &mut crate::diag::Chain,
        spec: &str,
        r: &crate::program::Resolution,
    ) {
        let pkg = r.package_name.clone().unwrap_or_else(|| {
            let (name, _) = ts_r_resolver::split_package_name(spec);
            name.to_owned()
        });
        let mangled = ts_r_resolver::mangle_scoped_name(&pkg);
        if let Some(alt) = &r.alternate {
            let library = if alt.contains("/node_modules/@types/") {
                format!("@types/{mangled}")
            } else {
                pkg.clone()
            };
            chain.push_back(
                dg::THERE_ARE_TYPES_AT_0_BUT_THIS_RESULT_COULD_NOT_BE_RESOLVED_WHEN_RESPECTING_PACKA_6278,
                &[alt, &library],
            );
            return;
        }
        // Which packages the program's module resolutions entered, and whether any of
        // them found a `.d.ts` there.
        let entered = |name: &str| -> Option<bool> {
            let mut found = None;
            let all = self
                .c
                .resolutions
                .values()
                .chain(self.c.mode_resolutions.values());
            for res in all.flatten() {
                if res.package_name.as_deref() == Some(name) {
                    let dts = res.extension == ts_r_resolver::Extension::Dts;
                    found = Some(found.unwrap_or(false) || dts);
                }
            }
            found
        };
        if entered(&format!("@types/{mangled}")).is_some() {
            chain.push_back(
                dg::IF_THE_0_PACKAGE_ACTUALLY_EXPOSES_THIS_MODULE_CONSIDER_SENDING_A_PULL_REQUEST_TO_7040,
                &[&pkg, &mangled],
            );
        } else if entered(&pkg) == Some(true) {
            chain.push_back(
                dg::IF_THE_0_PACKAGE_ACTUALLY_EXPOSES_THIS_MODULE_TRY_ADDING_A_NEW_DECLARATION_D_TS_F_7058,
                &[&pkg, spec],
            );
        } else {
            chain.push_back(
                dg::TRY_NPM_I_SAVE_DEV_TYPES_1_IF_IT_EXISTS_OR_ADD_A_NEW_DECLARATION_D_TS_FILE_CONTA_7035,
                &[spec, &mangled],
            );
        }
    }

    fn in_untyped_import_type_args(&self, file: FileId, node: NodeId) -> bool {
        let ast = self.ast(file);
        let path = &self.c.file(file).path;
        let mut cur = Loc::new(file, node);
        let mut child = node;
        for _ in 0..256 {
            let Some(p) = self.c.parent(cur) else {
                return false;
            };
            if let Node::ImportType {
                arg,
                type_args: Some(ta),
                ..
            } = *ast.node(p.node)
                && ta == child
            {
                let lit = match *ast.node(arg) {
                    Node::TypeRef { name, .. } => name,
                    _ => arg,
                };
                if let Node::StringLit { value } = *ast.node(lit)
                    && let Some(Some(r)) = self
                        .c
                        .resolutions
                        .get(&(path.clone(), ast.atom(value).to_owned()))
                    && !r.included
                    && r.external
                {
                    return true;
                }
            }
            child = p.node;
            cur = p;
        }
        false
    }

    /// TS2846: a value import of a declaration file (`import { T } from "./x.d.ts"`), with
    /// the implementation file to import instead: extensionless in CommonJS mode, `.js`
    /// (`.mjs`, `.cjs`) in ESM mode, `.ts` under `allowImportingTsExtensions`. Type-only
    /// imports, side-effect imports and `import()` types are fine.
    fn check_declaration_file_import(&mut self, file: FileId, req: &ts_r_binder::ModuleRequest) {
        if req.type_only
            || !matches!(
                req.kind,
                RequestKind::Import | RequestKind::ExportFrom | RequestKind::DynamicImport
            )
        {
            return;
        }
        let ast = self.ast(file);
        let spec = ast.atom(req.specifier);
        let lower = spec.to_ascii_lowercase();
        let Some((stem_len, ts, js)) = [
            (".d.ts", ".ts", ".js"),
            (".d.mts", ".mts", ".mjs"),
            (".d.cts", ".cts", ".cjs"),
        ]
        .into_iter()
        .find(|(d, _, _)| lower.ends_with(d))
        .map(|(d, ts, js)| (spec.len() - d.len(), ts, js)) else {
            return;
        };
        if stem_len == 0 {
            return;
        }
        let path = self.c.file(file).path.clone();
        if !matches!(
            self.c.resolutions.get(&(path, spec.to_owned())),
            Some(Some(r)) if r.included
        ) {
            return;
        }
        // `import "./x.d.ts"` imports nothing.
        if req.kind == RequestKind::Import
            && let Some(p) = self.c.parent(Loc::new(file, req.node))
            && let Node::ImportDecl {
                has_clause: false, ..
            } = *ast.node(p.node)
        {
            return;
        }
        let node = matches!(
            self.c.opts.module,
            ts_r_config::ModuleKind::Node16
                | ts_r_config::ModuleKind::Node18
                | ts_r_config::ModuleKind::Node20
                | ts_r_config::ModuleKind::NodeNext
        );
        let esm = (node && req.kind == RequestKind::DynamicImport) || self.c.file(file).esm_format;
        let stem = &spec[..stem_len];
        let suggestion = if self.c.opts.allow_importing_ts_extensions {
            format!("{stem}{ts}")
        } else if esm {
            format!("{stem}{js}")
        } else {
            stem.to_owned()
        };
        self.error(
            Loc::new(file, req.node),
            dg::A_DECLARATION_FILE_CANNOT_BE_IMPORTED_WITHOUT_IMPORT_TYPE_DID_YOU_MEAN_TO_IMPORT_2846,
            &[&suggestion],
        );
    }

    /// TS5097, TS2876, TS2877: a specifier written with a TypeScript extension.
    /// - Found through that extension (`./a.ts` → `a.ts`) in an import that is emitted (an
    ///   import declaration with a value clause, a value `export ... from`, `import x =
    ///   require()`, `import()`), without `allowImportingTsExtensions` (or
    ///   `rewriteRelativeImportExtensions`) and outside declaration files: TS5097. A
    ///   declaration-file specifier is TS2846 instead ([`Self::check_declaration_file_import`]).
    /// - Under `rewriteRelativeImportExtensions`, outside ambient code, type-only imports and
    ///   import types: a relative `.ts`-looking specifier that the extension did not find
    ///   (`./foo.ts` → `./foo.ts/index.ts`) cannot be rewritten (TS2876, with the real path
    ///   relative to the importer); a non-relative one that the extension did find, to a file
    ///   that is emitted, will not be rewritten (TS2877).
    ///
    /// tsc: tsc/internal/checker/checker.go (resolveExternalModule), tsc/internal/ast/utilities.go
    /// (IsEmittableImport), tsc/internal/core/core.go (ShouldRewriteModuleSpecifier).
    fn check_ts_extension_import(
        &mut self,
        file: FileId,
        req: &ts_r_binder::ModuleRequest,
        spec: &str,
        resolved: &str,
        ts_extension: bool,
        emitted: bool,
    ) {
        const TS_EXTS: [&str; 7] = [".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts"];
        let spec_is_declaration = ts_r_config::path::ext::is_declaration(spec);
        if ts_extension && spec_is_declaration {
            return;
        }
        let importer_is_declaration = self.c.file(file).kind.is_declaration();
        let js_require = req.kind == RequestKind::ImportEquals
            && self
                .c
                .parent(Loc::new(file, req.node))
                .is_some_and(|p| matches!(self.ast(file).node(p.node), Node::Call { .. }));
        let emittable = !req.type_only
            && !js_require
            && match req.kind {
                RequestKind::Import => !self.is_side_effect_import(file, req),
                RequestKind::ExportFrom
                | RequestKind::ImportEquals
                | RequestKind::DynamicImport => true,
                RequestKind::ImportType | RequestKind::Augmentation => false,
            };
        if ts_extension && !(self.c.opts.allow_importing_ts_extensions || importer_is_declaration) {
            if !emittable {
                return;
            }
            // The extension may sit inside the specifier when a pattern's `*` brought it in
            // (`#/foo.ts.omg` through `"#/*.omg"`).
            let Some(ext) = TS_EXTS
                .iter()
                .find(|e| ts_r_config::path::ext::has(spec, e))
                .or_else(|| TS_EXTS[3..].iter().find(|e| spec.contains(*e)))
            else {
                return;
            };
            self.error(
                Loc::new(file, req.node),
                dg::AN_IMPORT_PATH_CAN_ONLY_END_WITH_A_0_EXTENSION_WHEN_ALLOWIMPORTINGTSEXTENSIONS_I_5097,
                &[ext],
            );
            return;
        }
        if !self.c.opts.rewrite_relative_import_extensions
            || importer_is_declaration
            || req.in_ambient_module
            || req.in_augmentation
            || spec_is_declaration
            || req.type_only
            || matches!(
                req.kind,
                RequestKind::ImportType | RequestKind::Augmentation
            )
        {
            return;
        }
        let relative = spec.starts_with("./")
            || spec.starts_with("../")
            || spec.starts_with('/')
            || spec == "."
            || spec == "..";
        let should_rewrite = relative
            && TS_EXTS[3..]
                .iter()
                .any(|e| ts_r_config::path::ext::has(spec, e));
        if !ts_extension && should_rewrite {
            let from = self.c.file(file).path.clone();
            let rel = ts_r_config::path::relative_dotted(
                ts_r_config::path::directory_of(&from),
                resolved,
                true,
            );
            self.error(
                Loc::new(file, req.node),
                dg::THIS_RELATIVE_IMPORT_PATH_IS_UNSAFE_TO_REWRITE_BECAUSE_IT_LOOKS_LIKE_A_FILE_NAME_2876,
                &[&rel],
            );
        } else if ts_extension && !should_rewrite && emitted {
            let name = spec.rsplit('/').next().unwrap_or(spec);
            let ext = name.rfind('.').map_or("", |i| &name[i..]);
            self.error(
                Loc::new(file, req.node),
                dg::THIS_IMPORT_USES_A_0_EXTENSION_TO_RESOLVE_TO_AN_INPUT_TYPESCRIPT_FILE_BUT_WILL_N_2877,
                &[ext],
            );
        }
    }

    /// TS2306 for `import x = require("./script")` and `export * from "./script"` of a
    /// program file that is not a module (import declarations and `export {} from` are
    /// checked in [`Self::check_import_decl`]). JavaScript CommonJS modules and JSON are
    /// modules; JavaScript `require` is not checked.
    fn check_script_import(
        &mut self,
        file: FileId,
        req: &ts_r_binder::ModuleRequest,
        resolved: &str,
    ) {
        let parent = self.c.parent(Loc::new(file, req.node)).map(|p| p.node);
        let ast = self.ast(file);
        let applies = match req.kind {
            RequestKind::ImportEquals => {
                parent.is_some_and(|p| !matches!(ast.node(p), Node::Call { .. }))
            }
            RequestKind::ExportFrom => {
                parent.is_some_and(|p| matches!(ast.node(p), Node::ExportAll { .. }))
            }
            _ => false,
        };
        if !applies {
            return;
        }
        let Some(target) = self.c.files.iter().find(|f| f.path == resolved) else {
            return;
        };
        if target.bound.is_module
            || target.is_json()
            || (target.is_js() && target.is_commonjs_module())
        {
            return;
        }
        // A `.tsx` file without `jsx` set is not reported (as for import declarations).
        if !self.c.opts.jsx_set && (resolved.ends_with(".tsx") || resolved.ends_with(".jsx")) {
            return;
        }
        let p = target.path.clone();
        self.error(
            Loc::new(file, req.node),
            dg::FILE_0_IS_NOT_A_MODULE_2306,
            &[&p],
        );
    }

    /// Whether `req` is the specifier of an import declaration without a clause.
    fn is_side_effect_import(&self, file: FileId, req: &ts_r_binder::ModuleRequest) -> bool {
        req.kind == RequestKind::Import
            && self.c.parent(Loc::new(file, req.node)).is_some_and(|p| {
                matches!(
                    *self.ast(file).node(p.node),
                    Node::ImportDecl {
                        has_clause: false,
                        ..
                    }
                )
            })
    }

    fn check_module_requests(&mut self, file: FileId) {
        let b = self.c.bound(file);
        let ast = self.ast(file);
        let path = self.c.file(file).path.clone();
        for req in &b.module_requests {
            let spec = ast.atom(req.specifier).to_owned();
            if matches!(req.kind, RequestKind::Augmentation) {
                // Augmenting a module that resolves to JavaScript without types (TS2665).
                if !self.is_ambient_module_name(&spec)
                    && let Some(Some(r)) = self.c.resolutions.get(&(path.clone(), spec.clone()))
                    && !r.included
                {
                    let resolved = r.resolved.clone();
                    self.error(
                        Loc::new(file, req.node),
                        dg::INVALID_MODULE_NAME_IN_AUGMENTATION_MODULE_0_RESOLVES_TO_AN_UNTYPED_MODULE_AT_1_W_2665,
                        &[&spec, &resolved],
                    );
                }
                continue;
            }
            if self.is_ambient_module_name(&spec) || self.request_in_namespace(file, req) {
                continue;
            }
            // Without `noUncheckedSideEffectImports`, `import "x"` is not resolved at all.
            if !self.c.opts.no_unchecked_side_effect_imports
                && self.is_side_effect_import(file, req)
            {
                continue;
            }
            // A relative specifier in a module augmentation is only found when the file
            // imports it elsewhere too (the program does not link it otherwise).
            let relative = spec == "."
                || spec == ".."
                || spec.starts_with("./")
                || spec.starts_with("../")
                || spec.starts_with('/');
            if req.in_augmentation
                && relative
                && !b.module_requests.iter().any(|o| {
                    !o.in_augmentation && !o.in_ambient_module && ast.atom(o.specifier) == spec
                })
            {
                self.error(
                    Loc::new(file, req.node),
                    dg::CANNOT_FIND_MODULE_0_OR_ITS_CORRESPONDING_TYPE_DECLARATIONS_2307,
                    &[&spec],
                );
                continue;
            }
            if req.kind == RequestKind::ImportType {
                self.check_import_type_mode(file, req.node);
            }
            self.check_declaration_file_import(file, req);
            match self.c.resolution(file, &spec, req) {
                // A referenced project's output that was not built (`references`).
                Some(Some(r))
                    if r.included
                        && r.reference_source.is_some()
                        && !self.c.files.iter().any(|f| f.path == r.resolved) =>
                {
                    let (out, src) = (
                        r.resolved.clone(),
                        r.reference_source.clone().unwrap_or_default(),
                    );
                    self.error(
                        Loc::new(file, req.node),
                        dg::OUTPUT_FILE_0_HAS_NOT_BEEN_BUILT_FROM_SOURCE_FILE_1_6305,
                        &[&out, &src],
                    );
                }
                Some(Some(r)) if r.arbitrary_extension => {
                    let resolved = r.resolved.clone();
                    self.error(
                        Loc::new(file, req.node),
                        dg::MODULE_0_WAS_RESOLVED_TO_1_BUT_ALLOWARBITRARYEXTENSIONS_IS_NOT_SET_6263,
                        &[&spec, &resolved],
                    );
                }
                Some(Some(r)) if r.included => {
                    let resolved = r.resolved.clone();
                    let (ts_extension, emitted) =
                        (r.ts_extension, !r.external && !r.extension.is_declaration());
                    self.check_ts_extension_import(
                        file,
                        req,
                        &spec,
                        &resolved,
                        ts_extension,
                        emitted,
                    );
                    self.check_require_of_esm(file, req, &spec, &resolved);
                    self.check_script_import(file, req, &resolved);
                    if !self.c.opts.jsx_set
                        && (r.resolved.ends_with(".tsx") || r.resolved.ends_with(".jsx"))
                    {
                        let resolved = r.resolved.clone();
                        self.error(
                            Loc::new(file, req.node),
                            dg::MODULE_0_WAS_RESOLVED_TO_1_BUT_JSX_IS_NOT_SET_6142,
                            &[&spec, &resolved],
                        );
                    }
                }
                // A `.jsx` file outside the program (no `allowJs`) is TS6142 too.
                Some(Some(r)) if !self.c.opts.jsx_set && r.resolved.ends_with(".jsx") => {
                    let resolved = r.resolved.clone();
                    self.error(
                        Loc::new(file, req.node),
                        dg::MODULE_0_WAS_RESOLVED_TO_1_BUT_JSX_IS_NOT_SET_6142,
                        &[&spec, &resolved],
                    );
                }
                Some(Some(r)) => {
                    // A side-effect import (`import "pkg/mode"`) of untyped JavaScript
                    // reads nothing, so it is not reported.
                    // JavaScript `require("pkg")` neither (observed).
                    // A relative import of JavaScript outside the program is untyped too.
                    use ts_r_resolver::Extension as E;
                    let untyped_js = matches!(r.extension, E::Js | E::Jsx | E::Mjs | E::Cjs);
                    if self.c.opts.no_implicit_any
                        && (r.external || untyped_js)
                        && !self.is_side_effect_import(file, req)
                        && !self.c.parent(Loc::new(file, req.node)).is_some_and(|p| {
                            matches!(self.ast(file).node(p.node), Node::Call { .. })
                        })
                        && !self.in_untyped_import_type_args(file, req.node)
                    {
                        let resolved = r.resolved.clone();
                        let mut chain = crate::diag::Chain::new();
                        chain.push_back(dg::COULD_NOT_FIND_A_DECLARATION_FILE_FOR_MODULE_0_1_IMPLICITLY_HAS_AN_ANY_TYPE_7016, &[&spec, &resolved]);
                        // A package specifier of a package with a `package.json` naming
                        // it gets a hint (observed).
                        if r.package && !spec.starts_with('.') && !spec.starts_with('/') {
                            self.untyped_package_hint(&mut chain, &spec, r);
                        }
                        self.error_chain(Loc::new(file, req.node), &chain);
                    }
                }
                _ if self.needs_explicit_extension(file, req, &spec) => {
                    match self.extension_suggestion(file, &spec) {
                        Some(s) => self.error(
                            Loc::new(file, req.node),
                            dg::RELATIVE_IMPORT_PATHS_NEED_EXPLICIT_FILE_EXTENSIONS_IN_ECMASCRIPT_IMPORTS_WHEN_M_2835,
                            &[&s],
                        ),
                        None => self.error(
                            Loc::new(file, req.node),
                            dg::RELATIVE_IMPORT_PATHS_NEED_EXPLICIT_FILE_EXTENSIONS_IN_ECMASCRIPT_IMPORTS_WHEN_M_2834,
                            &[],
                        ),
                    }
                }
                _ if self
                    .c
                    .parent(Loc::new(file, req.node))
                    .is_some_and(|p| is_side_effect_import(ast, p.node)) =>
                {
                    self.error(
                        Loc::new(file, req.node),
                        dg::CANNOT_FIND_MODULE_OR_TYPE_DECLARATIONS_FOR_SIDE_EFFECT_IMPORT_OF_0_2882,
                        &[&spec],
                    );
                }
                _ if is_node_builtin_module(&spec) => {
                    self.error(
                        Loc::new(file, req.node),
                        dg::CANNOT_FIND_NAME_0_DO_YOU_NEED_TO_INSTALL_TYPE_DEFINITIONS_FOR_NODE_TRY_NPM_I_SA_2591,
                        &[&spec],
                    );
                }
                _ => {
                    self.error(
                        Loc::new(file, req.node),
                        dg::CANNOT_FIND_MODULE_0_OR_ITS_CORRESPONDING_TYPE_DECLARATIONS_2307,
                        &[&spec],
                    );
                }
            }
        }
    }

    /// Whether unresolved request `req` of `file` is an extensionless relative specifier
    /// resolved in ECMAScript-module mode under `moduleResolution` `node16`/`nodenext`
    /// (TS2834/TS2835 instead of TS2307).
    fn needs_explicit_extension(
        &self,
        file: FileId,
        req: &ts_r_binder::ModuleRequest,
        spec: &str,
    ) -> bool {
        use ts_r_resolver::resolver::ResolutionMode;
        if !self.c.opts.node_resolution {
            return false;
        }
        if !(spec.starts_with("./") || spec.starts_with("../")) {
            return false;
        }
        // Any `.` in the last name counts as an extension (so `./` and `../`, whose last
        // name is `.`/`..`, are TS2307; `./dir/` is not).
        let last = spec.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        if last.contains('.') {
            return false;
        }
        let f = self.c.file(file);
        crate::program::request_mode(self.c.opts.module, &f.path, f.package_esm, req)
            == ResolutionMode::Import
    }

    /// The specifier TS2835 suggests for extensionless relative `spec` of `file`: `spec` with
    /// the output extension of a non-declaration program file found by adding an extension,
    /// `.mjs` outputs first, then `.js`, then `.cjs` (observed: `./foo` → `./foo.mjs` when
    /// `foo.ts` and `foo.mts` exist; declaration files alone get no suggestion, TS2834).
    fn extension_suggestion(&self, file: FileId, spec: &str) -> Option<String> {
        let dir = ts_r_config::path::directory_of(&self.c.file(file).path);
        let base = ts_r_config::path::normalize(&ts_r_config::path::combine(dir, spec));
        let tsx_out = if self.c.opts.jsx_preserve {
            ".jsx"
        } else {
            ".js"
        };
        let candidates: [(&str, &str); 8] = [
            (".mts", ".mjs"),
            (".mjs", ".mjs"),
            (".ts", ".js"),
            (".tsx", tsx_out),
            (".js", ".js"),
            (".jsx", ".jsx"),
            (".cts", ".cjs"),
            (".cjs", ".cjs"),
        ];
        candidates.iter().find_map(|(ext, out)| {
            let cand = format!("{base}{ext}");
            self.c
                .files
                .iter()
                .any(|f| !f.is_lib && f.path == cand)
                .then(|| format!("{spec}{out}"))
        })
    }

    /// TS1203 at a top-level `export =` of a file emitted as an ES module: under
    /// `es2015`..`esnext` any file but `.cts`/`.cjs` (declaration files only `.d.mts`),
    /// under `node16`..`nodenext` a file whose format is ESM (declaration files too).
    /// TS2880 at the `assert` of legacy import assertions on import and export declarations
    /// and `import()` types (in tsc a syntax error, so declaration files too).
    /// Checked JavaScript: an import of a name that is only a type is TS18042 (at the
    /// imported name; `import("m").name` suggested, `import("m")` for a default import), an
    /// export specifier naming only a type (an interface, a JSDoc `@typedef`) is TS18043.
    fn check_js_type_only_aliases(&mut self, file: FileId) {
        if !self.reporting(file) || !self.c.file(file).is_js() {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let type_only = |ck: &Self, sp: NodeId| {
            let target =
                ck.c.sym_of_decl(Loc::new(file, sp))
                    .and_then(|alias| ck.c.resolve_alias(alias));
            // A top-level JSDoc `@typedef` of an ES module is declared only in the module's
            // exports, so export names do not resolve to it.
            if target.is_none()
                && let Node::ExportSpecifier { local, .. } = *ck.ast(file).node(sp)
                && let Some(name) = ck.ast(file).name_of(local)
            {
                let f = ck.c.file(file);
                let exports = f.bound.symbol(f.bound.file_symbol).exports;
                if let Some(x) = f.bound.lookup_str(&f.ast, exports, name) {
                    let flags = f.bound.symbol(x).flags;
                    return flags.intersects(SymbolFlags::TYPE_ALIAS)
                        && !flags.intersects(SymbolFlags::VALUE);
                }
            }
            match target {
                Some(t) => {
                    let f = ck.c.sym_flags(t);
                    !f.is_empty() && !f.intersects(SymbolFlags::VALUE | SymbolFlags::ALIAS)
                }
                None => false,
            }
        };
        for &st in ast.list(stmts) {
            match *ast.node(st) {
                Node::ImportDecl {
                    type_only: false,
                    specifiers,
                    source,
                    ..
                } => {
                    let Node::StringLit { value } = *ast.node(source) else {
                        continue;
                    };
                    let spec = ast.atom(value);
                    for &sp in ast.list(specifiers) {
                        let (at, name, default) = match *ast.node(sp) {
                            Node::ImportSpecifier {
                                type_only: false,
                                imported,
                                local,
                            } => {
                                let n = imported.unwrap_or(local);
                                (n, ast.name_of(n).unwrap_or(""), false)
                            }
                            Node::ImportDefaultSpecifier { local } => {
                                (local, ast.name_of(local).unwrap_or(""), true)
                            }
                            _ => continue,
                        };
                        if name.is_empty() || !type_only(self, sp) {
                            continue;
                        }
                        let hint = if default {
                            format!("import(\"{spec}\")")
                        } else {
                            format!("import(\"{spec}\").{name}")
                        };
                        self.error(
                            Loc::new(file, at),
                            dg::_0_IS_A_TYPE_AND_CANNOT_BE_IMPORTED_IN_JAVASCRIPT_FILES_USE_1_IN_A_JSDOC_TYPE_ANN_18042,
                            &[name, &hint],
                        );
                    }
                }
                Node::ExportNamed {
                    type_only: false,
                    specifiers,
                    ..
                } => {
                    for &sp in ast.list(specifiers) {
                        let Node::ExportSpecifier {
                            type_only: false,
                            local,
                            ..
                        } = *ast.node(sp)
                        else {
                            continue;
                        };
                        if type_only(self, sp) {
                            self.error(
                                Loc::new(file, local),
                                dg::TYPES_CANNOT_APPEAR_IN_EXPORT_DECLARATIONS_IN_JAVASCRIPT_FILES_18043,
                                &[],
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether an import or export declaration or `import()` type of `file` has legacy
    /// `assert` attributes (TS2880, a syntax error in tsc).
    fn has_assert_attributes(&self, file: FileId) -> bool {
        let f = self.c.file(file);
        f.bound.module_requests.iter().any(|req| {
            matches!(
                req.kind,
                RequestKind::Import | RequestKind::ExportFrom | RequestKind::ImportType
            ) && self.request_attributes(file, req.node).is_some_and(|a| {
                matches!(f.ast.node(a), Node::ImportAttributes { assert: true, .. })
            })
        })
    }

    fn check_import_assertions(&mut self, file: FileId) {
        if !self.reporting(file) {
            return;
        }
        let f = self.c.file(file);
        let text = f.text.as_str();
        let mut found = Vec::new();
        for req in &f.bound.module_requests {
            if !matches!(
                req.kind,
                RequestKind::Import | RequestKind::ExportFrom | RequestKind::ImportType
            ) {
                continue;
            }
            let Some(a) = self.request_attributes(file, req.node) else {
                continue;
            };
            if !matches!(f.ast.node(a), Node::ImportAttributes { assert: true, .. }) {
                continue;
            }
            let span = f.ast.span(a);
            let body = text
                .get(span.start as usize..span.end as usize)
                .unwrap_or("");
            if let Some(i) = body.find("assert") {
                let start = span.start + i as u32;
                found.push(Span::new(start, start + 6));
            }
        }
        for span in found {
            self.error_span(
                file,
                span,
                dg::IMPORT_ASSERTIONS_HAVE_BEEN_REPLACED_BY_IMPORT_ATTRIBUTES_USE_WITH_INSTEAD_OF_AS_2880,
                &[],
            );
        }
    }

    /// TS2309 at each `export =` of a module file or `declare module "x"` body that also
    /// exports a value of its own (not through `export *`; types, interfaces and
    /// uninstantiated namespaces do not count).
    /// JSDoc type syntax in a TypeScript type (`Ast::jsdoc_syntax`, recorded by the parser
    /// outside JSDoc comments): TS17019 for a postfix `?`/`!`, TS17020 for a prefix one
    /// and TS8020 for `*`, suggesting the type it stands for (observed tsc 7).
    fn check_jsdoc_syntax(&mut self, file: FileId) {
        let ast = self.ast(file);
        if ast.jsdoc_syntax.is_empty() || !self.reporting(file) {
            return;
        }
        for j in &ast.jsdoc_syntax {
            let Some(inner) = j.ty else {
                self.error_span(
                    file,
                    j.span,
                    dg::JSDOC_TYPES_CAN_ONLY_BE_USED_INSIDE_DOCUMENTATION_COMMENTS_8020,
                    &[],
                );
                continue;
            };
            let t = self.type_from_node(Loc::new(file, inner));
            let (op, extra): (&str, &[TypeId]) = match j.kind {
                JsDocSyntaxKind::PostfixNonNull | JsDocSyntaxKind::PrefixNonNull => ("!", &[]),
                JsDocSyntaxKind::PostfixNullable => ("?", &[TypeId::UNDEFINED]),
                _ => ("?", &[TypeId::NULL, TypeId::UNDEFINED]),
            };
            let keep = !self.c.opts.strict_null_checks
                || extra.is_empty()
                || matches!(
                    t,
                    TypeId::ANY
                        | TypeId::UNKNOWN
                        | TypeId::NEVER
                        | TypeId::VOID
                        | TypeId::UNDEFINED
                );
            let suggested = if keep {
                t
            } else {
                let mut types = vec![t];
                types.extend_from_slice(extra);
                self.union(&types)
            };
            let text = self.type_to_string(suggested);
            let info = if matches!(
                j.kind,
                JsDocSyntaxKind::PostfixNonNull | JsDocSyntaxKind::PostfixNullable
            ) {
                dg::_0_AT_THE_END_OF_A_TYPE_IS_NOT_VALID_TYPESCRIPT_SYNTAX_DID_YOU_MEAN_TO_WRITE_1_17019
            } else {
                dg::_0_AT_THE_START_OF_A_TYPE_IS_NOT_VALID_TYPESCRIPT_SYNTAX_DID_YOU_MEAN_TO_WRITE_1_17020
            };
            self.error_span(file, j.span, info, &[op, &text]);
        }
    }

    fn check_export_assignment_conflicts(&mut self, file: FileId) {
        if !self.reporting(file) {
            return;
        }
        if self.c.file(file).is_js() {
            self.commonjs_export_assignment_conflicts(file);
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let bound = &self.c.file(file).bound;
        if bound.is_module {
            self.export_assignment_conflicts(file, stmts, bound.file_symbol);
        }
        for &st in ast.list(stmts) {
            if let Node::Module {
                name,
                body: Some(b),
                ..
            } = *ast.node(st)
                && matches!(ast.node(name), Node::StringLit { .. })
                && let Node::ModuleBlock { stmts: inner } = *ast.node(b)
                && let Some(sym) = self.c.file(file).bound.symbol_of_decl(st)
            {
                self.export_assignment_conflicts(file, inner, sym);
            }
        }
    }

    /// TS2309 at the first `module.exports = v` of a CommonJS module that also has
    /// `exports.x = ...` / `module.exports.x = ...` exports (not for `module.exports =
    /// exports`, observed).
    fn commonjs_export_assignment_conflicts(&mut self, file: FileId) {
        let bound = &self.c.file(file).bound;
        if !bound.is_module {
            return;
        }
        let ast = self.ast(file);
        let table = bound.symbol(bound.file_symbol).exports;
        let mut eq = None;
        let mut others = false;
        for &(n, s) in bound.table(table) {
            if bound.name_text(ast, n) == "export=" {
                eq = Some(s);
            } else if bound
                .symbol(s)
                .facts
                .contains(ts_r_binder::SymbolFacts::EXPANDO)
            {
                others = true;
            }
        }
        let (Some(eq), true) = (eq, others) else {
            return;
        };
        let Some(&d) = bound.decls(eq).iter().min() else {
            return;
        };
        if let Node::Assign { right, .. } = *ast.node(d)
            && !matches!(*ast.node(crate::nodes::skip_parens(ast, right)), Node::Ident { name } if ast.atom(name) == "exports")
        {
            self.error(
                Loc::new(file, d),
                dg::AN_EXPORT_ASSIGNMENT_CANNOT_BE_USED_IN_A_MODULE_WITH_OTHER_EXPORTED_ELEMENTS_2309,
                &[],
            );
        }
    }

    fn export_assignment_conflicts(
        &mut self,
        file: FileId,
        stmts: ts_r_ast::NodeList,
        container: ts_r_binder::SymbolId,
    ) {
        let ast = self.ast(file);
        let assignments: Vec<NodeId> = ast
            .list(stmts)
            .iter()
            .copied()
            .filter(|&st| matches!(ast.node(st), Node::ExportAssignment { .. }))
            .collect();
        if assignments.is_empty() {
            return;
        }
        let bound = &self.c.file(file).bound;
        let exports: Vec<ts_r_binder::SymbolId> = bound
            .table(bound.symbol(container).exports)
            .iter()
            .filter(|(n, _)| bound.name_text(ast, *n) != "export=")
            .map(|&(_, x)| x)
            .collect();
        let has_value = exports.into_iter().any(|x| {
            let sym = self.c.sym(file, x);
            let flags = self.c.sym_flags(sym);
            if flags.intersects(SymbolFlags::ALIAS) {
                // An alias of nothing (`export import a = x.missing`) counts as a value.
                self.c
                    .resolve_alias(sym)
                    .is_none_or(|t| self.c.sym_flags(t).intersects(SymbolFlags::VALUE))
            } else {
                flags.intersects(SymbolFlags::VALUE)
            }
        });
        if !has_value {
            return;
        }
        for st in assignments {
            self.error_span(
                file,
                ast.span(st),
                dg::AN_EXPORT_ASSIGNMENT_CANNOT_BE_USED_IN_A_MODULE_WITH_OTHER_EXPORTED_ELEMENTS_2309,
                &[],
            );
        }
    }

    fn check_esm_export_assignment(&mut self, file: FileId) {
        use ts_r_config::ModuleKind as M;
        if !self.reporting(file) {
            return;
        }
        let f = self.c.file(file);
        let lower = f.path.to_ascii_lowercase();
        let esm = match self.c.opts.module {
            M::ES2015 | M::ES2020 | M::ES2022 | M::ESNext => {
                if f.kind.is_declaration() {
                    lower.ends_with(".d.mts")
                } else {
                    !lower.ends_with(".cts") && !lower.ends_with(".cjs")
                }
            }
            M::Node16 | M::Node18 | M::Node20 | M::NodeNext => f.package_esm,
            _ => false,
        };
        if !esm {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let found: Vec<NodeId> = ast
            .list(stmts)
            .iter()
            .copied()
            .filter(|&st| matches!(ast.node(st), Node::ExportAssignment { .. }))
            .collect();
        for st in found {
            self.error(
                Loc::new(file, st),
                dg::EXPORT_ASSIGNMENT_CANNOT_BE_USED_WHEN_TARGETING_ECMASCRIPT_MODULES_CONSIDER_USIN_1203,
                &[],
            );
        }
    }

    /// TS1202 at a top-level `import x = require("m")` (not type-only) of a non-declaration
    /// file under `es2015`..`esnext`, `.cts` included (`node16`..`nodenext` allow it).
    fn check_esm_import_assignment(&mut self, file: FileId) {
        use ts_r_config::ModuleKind as M;
        if !self.reporting(file)
            || self.c.file(file).kind.is_declaration()
            || !matches!(
                self.c.opts.module,
                M::ES2015 | M::ES2020 | M::ES2022 | M::ESNext
            )
        {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let found: Vec<NodeId> = ast
            .list(stmts)
            .iter()
            .copied()
            .filter(|&st| {
                matches!(*ast.node(st), Node::ImportEquals { type_only: false, module_ref, .. }
                    if matches!(ast.node(module_ref), Node::ExternalModuleRef { .. }))
            })
            .collect();
        for st in found {
            self.error(
                Loc::new(file, st),
                dg::IMPORT_ASSIGNMENT_CANNOT_BE_USED_WHEN_TARGETING_ECMASCRIPT_MODULES_CONSIDER_USIN_1202,
                &[],
            );
        }
    }

    /// TS1262: in a module file `await` is reserved at the top level, so a declaration named
    /// `await` there (variables and binding patterns, also in blocks and loops; functions,
    /// classes, imports, `import =`, catch parameters) is an error. Function bodies,
    /// namespaces and ambient (`declare`) declarations are not the top level.
    fn check_top_level_await_names(&mut self, file: FileId) {
        let f = self.c.file(file);
        if !f.bound.is_module || f.kind.is_declaration() || !f.text.contains("await") {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let mut names: Vec<NodeId> = Vec::new();
        let mut bindings: Vec<NodeId> = Vec::new();
        let mut stack: Vec<NodeId> = ast.list(stmts).iter().rev().copied().collect();
        let mut guard = 0u32;
        while let Some(n) = stack.pop() {
            guard += 1;
            if guard > 1_000_000 {
                break;
            }
            match *ast.node(n) {
                Node::Block { stmts } => stack.extend(ast.list(stmts).iter().rev()),
                Node::If { cons, alt, .. } => {
                    stack.extend(alt);
                    stack.push(cons);
                }
                Node::For { init, body, .. } => {
                    stack.push(body);
                    stack.extend(init);
                }
                Node::ForIn { left, body, .. } | Node::ForOf { left, body, .. } => {
                    stack.push(body);
                    stack.push(left);
                }
                Node::While { body, .. }
                | Node::DoWhile { body, .. }
                | Node::Labeled { body, .. } => stack.push(body),
                Node::Switch { cases, .. } => {
                    for &c in ast.list(cases).iter().rev() {
                        if let Node::SwitchCase { body, .. } = *ast.node(c) {
                            stack.extend(ast.list(body).iter().rev());
                        }
                    }
                }
                Node::Try {
                    block,
                    handler,
                    finalizer,
                } => {
                    stack.extend(finalizer);
                    if let Some(h) = handler
                        && let Node::CatchClause { param, body, .. } = *ast.node(h)
                    {
                        stack.push(body);
                        bindings.extend(param);
                    }
                    stack.push(block);
                }
                Node::VarDecl {
                    modifiers, decls, ..
                } if !modifiers.contains(Modifiers::DECLARE) => {
                    for &d in ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                            bindings.push(name);
                        }
                    }
                }
                Node::FunctionDecl {
                    modifiers,
                    name: Some(name),
                    ..
                }
                | Node::ClassDecl {
                    modifiers,
                    name: Some(name),
                    ..
                } if !modifiers.contains(Modifiers::DECLARE) => names.push(name),
                Node::ImportDecl { specifiers, .. } => {
                    for &sp in ast.list(specifiers) {
                        match *ast.node(sp) {
                            Node::ImportDefaultSpecifier { local }
                            | Node::ImportNamespaceSpecifier { local }
                            | Node::ImportSpecifier { local, .. } => names.push(local),
                            _ => {}
                        }
                    }
                }
                Node::ImportEquals { name, .. } => names.push(name),
                _ => {}
            }
        }
        // Binding patterns, depth-first.
        let mut guard = 0u32;
        while let Some(b) = bindings.pop() {
            guard += 1;
            if guard > 1_000_000 {
                break;
            }
            match *ast.node(b) {
                Node::Ident { .. } => names.push(b),
                Node::ObjectPattern { props } => {
                    for &p in ast.list(props) {
                        match *ast.node(p) {
                            Node::BindingProperty { value: Some(v), .. } => bindings.push(v),
                            Node::BindingProperty {
                                key, value: None, ..
                            } => names.push(key),
                            Node::Rest { arg } => bindings.push(arg),
                            _ => {}
                        }
                    }
                }
                Node::ArrayPattern { elements } => bindings.extend(ast.list(elements)),
                Node::AssignPattern { left, .. } => bindings.push(left),
                Node::Rest { arg } => bindings.push(arg),
                _ => {}
            }
        }
        for n in names {
            if let Node::Ident { name } = *ast.node(n)
                && ast.atom(name) == "await"
            {
                self.error(
                    Loc::new(file, n),
                    dg::IDENTIFIER_EXPECTED_0_IS_A_RESERVED_WORD_AT_THE_TOP_LEVEL_OF_A_MODULE_1262,
                    &["await"],
                );
            }
        }
    }

    /// Whether `file` is emitted as CommonJS: under `commonjs` any file but `.mts`/`.mjs`,
    /// under `node16`..`nodenext` a file whose format is CommonJS.
    pub fn emits_commonjs(&self, file: FileId) -> bool {
        use ts_r_config::ModuleKind as M;
        let f = self.c.file(file);
        match self.c.opts.module {
            // `.mts`/`.mjs` emit as ES modules under any `module`.
            M::CommonJS => {
                let lower = f.path.to_ascii_lowercase();
                !lower.ends_with(".mts") && !lower.ends_with(".mjs")
            }
            M::Node16 | M::Node18 | M::Node20 | M::NodeNext => !f.package_esm,
            _ => false,
        }
    }

    /// TS2725 at the name of a class named `Object` (not ambient) in a file emitted as
    /// CommonJS.
    fn check_class_named_object(&mut self, l: Loc) {
        use ts_r_config::ModuleKind as M;
        let ast = self.ast(l.file);
        let name = match *ast.node(l.node) {
            Node::ClassDecl { name: Some(n), .. } | Node::ClassExpr { name: Some(n), .. } => n,
            _ => return,
        };
        if !matches!(*ast.node(name), Node::Ident { name: a } if ast.atom(a) == "Object")
            || !self.reporting(l.file)
            || !self.emits_commonjs(l.file)
            || self.is_ambient_node(l)
        {
            return;
        }
        let module = match self.c.opts.module {
            M::Node16 => "Node16",
            M::Node18 => "Node18",
            M::Node20 => "Node20",
            M::NodeNext => "NodeNext",
            _ => "CommonJS",
        };
        self.error(
            Loc::new(l.file, name),
            dg::CLASS_NAME_CANNOT_BE_OBJECT_WHEN_TARGETING_ES5_AND_ABOVE_WITH_MODULE_0_2725,
            &[module],
        );
    }

    /// When emitting a module file as CommonJS: TS2441 at top-level declarations named
    /// `require` or `exports` (functions, classes, variables, imports; not ambient), and
    /// TS1216 at an exported variable named `__esModule`. tsc reports neither under
    /// `noEmit`.
    fn check_commonjs_generated_names(&mut self, file: FileId) {
        let f = self.c.file(file);
        if self.c.opts.no_emit
            || !self.reporting(file)
            || f.kind.is_declaration()
            || !f.bound.is_module
            || !self.emits_commonjs(file)
        {
            return;
        }
        let ast = self.ast(file);
        let Node::Program { stmts } = *ast.node(ast.root) else {
            return;
        };
        let mut reserved: Vec<NodeId> = Vec::new();
        let mut marker: Vec<NodeId> = Vec::new();
        for &st in ast.list(stmts) {
            let node = ast.node(st);
            if node.modifiers().contains(Modifiers::DECLARE) {
                continue;
            }
            match *node {
                Node::FunctionDecl { name: Some(n), .. }
                | Node::ClassDecl { name: Some(n), .. }
                | Node::ImportEquals { name: n, .. } => reserved.push(n),
                Node::VarDecl {
                    decls, modifiers, ..
                } => {
                    for &d in ast.list(decls) {
                        if let Node::VarDeclarator { name, .. } = *ast.node(d)
                            && matches!(ast.node(name), Node::Ident { .. })
                        {
                            reserved.push(name);
                            if modifiers.contains(Modifiers::EXPORT) {
                                marker.push(name);
                            }
                        }
                    }
                }
                Node::ImportDecl { specifiers, .. } => {
                    for &sp in ast.list(specifiers) {
                        match *ast.node(sp) {
                            Node::ImportSpecifier { local, .. }
                            | Node::ImportDefaultSpecifier { local }
                            | Node::ImportNamespaceSpecifier { local } => reserved.push(local),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        for n in reserved {
            if let Node::Ident { name } = *ast.node(n) {
                let text = ast.atom(name);
                if text == "require" || text == "exports" {
                    self.error(
                        Loc::new(file, n),
                        dg::DUPLICATE_IDENTIFIER_0_COMPILER_RESERVES_NAME_1_IN_TOP_LEVEL_SCOPE_OF_A_MODULE_2441,
                        &[text, text],
                    );
                }
            }
        }
        for n in marker {
            if matches!(*ast.node(n), Node::Ident { name } if ast.atom(name) == "__esModule") {
                self.error(
                    Loc::new(file, n),
                    dg::IDENTIFIER_EXPECTED_ESMODULE_IS_RESERVED_AS_AN_EXPORTED_MARKER_WHEN_TRANSFORMING_1216,
                    &[],
                );
            }
        }
    }

    /// Under `node16`/`node18` (no `require` of ES modules), for a module that resolves to an
    /// ESM-format file: TS1479 for a static import or re-export in a CommonJS-format file,
    /// TS1541/TS1542 for a type-only import or `import()` type there without a
    /// `resolution-mode` attribute, TS1471 for `import x = require()` in any file. In a
    /// `.ts`/`.tsx`/`.js`/`.jsx` implementation file the first three say how to make the file
    /// ESM (extension, `package.json` `type`). JSON modules are never ESM here.
    fn check_require_of_esm(
        &mut self,
        file: FileId,
        req: &ts_r_binder::ModuleRequest,
        spec: &str,
        resolved: &str,
    ) {
        use ts_r_config::ModuleKind as M;
        if !matches!(self.c.opts.module, M::Node16 | M::Node18) {
            return;
        }
        let f = self.c.file(file);
        let info = match req.kind {
            RequestKind::ImportEquals if !req.type_only => {
                dg::MODULE_0_CANNOT_BE_IMPORTED_USING_THIS_CONSTRUCT_THE_SPECIFIER_ONLY_RESOLVES_TO_A_1471
            }
            _ if f.package_esm => return,
            RequestKind::Import | RequestKind::ExportFrom if !req.type_only => {
                dg::THE_CURRENT_FILE_IS_A_COMMONJS_MODULE_WHOSE_IMPORTS_WILL_PRODUCE_REQUIRE_CALLS_H_1479
            }
            RequestKind::Import | RequestKind::ExportFrom => {
                dg::TYPE_ONLY_IMPORT_OF_AN_ECMASCRIPT_MODULE_FROM_A_COMMONJS_MODULE_MUST_HAVE_A_RESO_1541
            }
            RequestKind::ImportType => {
                dg::TYPE_IMPORT_OF_AN_ECMASCRIPT_MODULE_FROM_A_COMMONJS_MODULE_MUST_HAVE_A_RESOLUTIO_1542
            }
            _ => return,
        };
        if !self
            .c
            .files
            .iter()
            .any(|t| t.path == resolved && t.package_esm && !t.is_json())
        {
            return;
        }
        if info.code != 1479 && info.code != 1471 && self.has_resolution_mode(file, req.node) {
            return;
        }
        let at = Loc::new(file, req.node);
        if info.code == 1471 {
            self.error(at, info, &[spec]);
            return;
        }
        let mut chain = crate::diag::Chain::new();
        chain.push_back(info, &[spec]);
        let lower = f.path.to_ascii_lowercase();
        let ext = if f.kind.is_declaration() || f.is_external {
            None
        } else if lower.ends_with(".ts") {
            Some(Some(".mts"))
        } else if lower.ends_with(".js") {
            Some(Some(".mjs"))
        } else if lower.ends_with(".tsx") || lower.ends_with(".jsx") {
            Some(None)
        } else {
            None
        };
        if let Some(ext) = ext {
            match f.package_scope {
                Some((up, false)) => {
                    let mut dir = ts_r_config::path::directory_of(&f.path);
                    for _ in 0..up {
                        dir = ts_r_config::path::directory_of(dir);
                    }
                    let json = ts_r_config::path::combine(dir, "package.json");
                    match ext {
                        Some(e) => chain.push_back(
                            dg::TO_CONVERT_THIS_FILE_TO_AN_ECMASCRIPT_MODULE_CHANGE_ITS_FILE_EXTENSION_TO_0_OR_A_1481,
                            &[e, &json],
                        ),
                        None => chain.push_back(
                            dg::TO_CONVERT_THIS_FILE_TO_AN_ECMASCRIPT_MODULE_ADD_THE_FIELD_TYPE_MODULE_TO_0_1482,
                            &[&json],
                        ),
                    }
                }
                _ => match ext {
                    Some(e) => chain.push_back(
                        dg::TO_CONVERT_THIS_FILE_TO_AN_ECMASCRIPT_MODULE_CHANGE_ITS_FILE_EXTENSION_TO_0_OR_C_1480,
                        &[e],
                    ),
                    None => chain.push_back(
                        dg::TO_CONVERT_THIS_FILE_TO_AN_ECMASCRIPT_MODULE_CREATE_A_LOCAL_PACKAGE_JSON_FILE_WI_1483,
                        &[],
                    ),
                },
            }
        }
        self.error_chain(at, &chain);
    }

    /// The import attributes of the import declaration or `import()` type of specifier
    /// `node`.
    fn request_attributes(&self, file: FileId, node: NodeId) -> Option<NodeId> {
        let ast = self.ast(file);
        let mut cur = Loc::new(file, node);
        for _ in 0..3 {
            let p = self.c.parent(cur)?;
            match *ast.node(p.node) {
                Node::ImportDecl { attributes, .. }
                | Node::ExportNamed { attributes, .. }
                | Node::ExportAll { attributes, .. }
                | Node::ImportType { attributes, .. } => return attributes,
                _ => cur = p,
            }
        }
        None
    }

    /// The value of the `resolution-mode` entry of import attributes `a`.
    fn resolution_mode_entry(&self, file: FileId, a: NodeId) -> Option<NodeId> {
        let ast = self.ast(file);
        let Node::ImportAttributes { entries, .. } = *ast.node(a) else {
            return None;
        };
        ast.list(entries).iter().find_map(|&e| match *ast.node(e) {
            Node::ImportAttribute { key, value }
                if key_text(ast, key).as_deref() == Some("resolution-mode") =>
            {
                Some(value)
            }
            _ => None,
        })
    }

    /// Whether the import declaration or `import()` type of specifier `node` has a
    /// `resolution-mode` attribute.
    fn has_resolution_mode(&self, file: FileId, node: NodeId) -> bool {
        self.request_attributes(file, node)
            .is_some_and(|a| self.resolution_mode_entry(file, a).is_some())
    }

    /// The attributes of an `import()` type: exactly one entry (TS1464 at the attributes),
    /// whose quoted key is `resolution-mode` (TS1463 at another quoted key) and whose
    /// string value is `import` or `require` (TS1453).
    fn check_import_type_mode(&mut self, file: FileId, node: NodeId) {
        let Some(a) = self.request_attributes(file, node) else {
            return;
        };
        // Grammar errors: none in a file where tsc's parser reported legacy `assert`.
        if self.has_assert_attributes(file) {
            return;
        }
        let ast = self.ast(file);
        let Node::ImportAttributes { entries, .. } = *ast.node(a) else {
            return;
        };
        let entries = ast.list(entries);
        let [e] = entries else {
            // The node spans `{ with: { ... } }`; tsc's spans the inner `{ ... }`.
            let span = ast.span(a);
            let text = &self.c.file(file).text;
            let open = text
                .get(span.start as usize + 1..span.end as usize)
                .and_then(|t| t.find('{'))
                .map(|o| span.start + 1 + o as u32);
            let after = entries
                .last()
                .map_or(open.map_or(span.end, |o| o + 1), |&l| ast.span(l).end);
            let close = text
                .get(after as usize..span.end as usize)
                .and_then(|t| t.find('}'))
                .map(|c| after + c as u32 + 1);
            let inner = match (open, close) {
                (Some(o), Some(c)) if o < c => Span::new(o, c),
                _ => span,
            };
            self.error_span(
                file,
                inner,
                dg::TYPE_IMPORT_ATTRIBUTES_SHOULD_HAVE_EXACTLY_ONE_KEY_RESOLUTION_MODE_WITH_VALUE_IM_1464,
                &[],
            );
            return;
        };
        let Node::ImportAttribute { key, value } = *ast.node(*e) else {
            return;
        };
        let Node::StringLit { value: k } = *ast.node(key) else {
            return;
        };
        if ast.atom(k) != "resolution-mode" {
            self.error(
                Loc::new(file, key),
                dg::RESOLUTION_MODE_IS_THE_ONLY_VALID_KEY_FOR_TYPE_IMPORT_ATTRIBUTES_1463,
                &[],
            );
            return;
        }
        if let Node::StringLit { value: v } = *ast.node(value)
            && !matches!(ast.atom(v), "import" | "require")
        {
            self.check_resolution_mode_value(Loc::new(file, value));
        }
    }

    /// TS1453 at the value of a `/// <reference types="..." resolution-mode="..." />`
    /// attribute that is not `import`/`require`.
    fn check_reference_modes(&mut self, file: FileId) {
        if !self.reporting(file) {
            return;
        }
        let f = self.c.file(file);
        let ast = &f.ast;
        let text = f.text.as_str();
        let first_stmt = match ast.node(ast.root) {
            Node::Program { stmts } => ast.list(*stmts).first().map(|&s| ast.span(s).start),
            _ => None,
        }
        .unwrap_or(u32::MAX);
        let mut found = Vec::new();
        for c in &ast.comments {
            if c.span.start >= first_stmt {
                break;
            }
            if c.kind != CommentKind::Line {
                continue;
            }
            let t = c.span.text(text);
            if crate::program::reference_attribute(t, "types").is_none() {
                continue;
            }
            let Some(at) = t.find("resolution-mode") else {
                continue;
            };
            let rest = &t[at + "resolution-mode".len()..];
            let Some(eq) = rest.find('=') else { continue };
            if !rest[..eq].trim().is_empty() {
                continue;
            }
            let after = rest[eq + 1..].trim_start();
            let Some(q) = after.chars().next().filter(|&q| q == '"' || q == '\'') else {
                continue;
            };
            let Some(len) = after[1..].find(q) else {
                continue;
            };
            let value = &after[1..1 + len];
            if value == "import" || value == "require" {
                continue;
            }
            let start = c.span.start as usize + (t.len() - after.len()) + 1;
            found.push(Span::new(start as u32, (start + len) as u32));
        }
        for span in found {
            self.error_span(
                file,
                span,
                dg::RESOLUTION_MODE_SHOULD_BE_EITHER_REQUIRE_OR_IMPORT_1453,
                &[],
            );
        }
    }

    fn is_ambient_module_name(&self, spec: &str) -> bool {
        for (name, _) in &self.c.link.ambient_modules {
            let n: &str = name;
            if n == spec {
                return true;
            }
            if let Some(star) = n.find('*') {
                let (pre, post) = (&n[..star], &n[star + 1..]);
                if spec.len() >= pre.len() + post.len()
                    && spec.starts_with(pre)
                    && spec.ends_with(post)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Import attributes of an import or export declaration.
    ///
    /// - TS2823: `module` is not `esnext`, `node18`, `node20`, `nodenext` or `preserve`
    ///   (removed `module` values report only the option error).
    /// - TS2856: under `node18`..`nodenext` in a file emitted as CommonJS.
    /// - TS2857 (type-only) and TS1454 (others with `resolution-mode`) elsewhere.
    /// - TS1464: a type-only form with no keys, or with `resolution-mode` and other keys.
    /// - TS1453: a type-only form whose `resolution-mode` is not `import`/`require`.
    ///
    /// A type-only form whose only attribute is a valid `resolution-mode` is fine.
    fn check_import_attributes(
        &mut self,
        file: FileId,
        attributes: Option<NodeId>,
        type_only: bool,
    ) -> bool {
        use ts_r_config::ModuleKind as M;
        let Some(a) = attributes else {
            return false;
        };
        if !self.reporting(file) {
            return false;
        }
        let ast = self.ast(file);
        let Node::ImportAttributes { entries, .. } = *ast.node(a) else {
            return false;
        };
        let entries = ast.list(entries);
        // Every value must be a string literal (TS2858); the attributes' object type must
        // fit the global `ImportAttributes`.
        let mut bad_value = false;
        for &e in entries {
            if let Node::ImportAttribute { value, .. } = *ast.node(e)
                && !matches!(ast.node(value), Node::StringLit { .. })
            {
                bad_value = true;
                self.error(
                    Loc::new(file, value),
                    dg::IMPORT_ATTRIBUTE_VALUES_MUST_BE_STRING_LITERAL_EXPRESSIONS_2858,
                    &[],
                );
            }
        }
        self.check_import_attributes_type(Loc::new(file, a));
        if self.c.file(file).kind.is_declaration() {
            return bad_value;
        }
        // The `resolution-mode` entry's value node and whether it is valid.
        let mode = entries.iter().find_map(|&e| match *ast.node(e) {
            Node::ImportAttribute { key, value }
                if key_text(ast, key).as_deref() == Some("resolution-mode") =>
            {
                let valid = matches!(*ast.node(value), Node::StringLit { value: v }
                    if matches!(ast.atom(v), "import" | "require"));
                Some((value, valid))
            }
            _ => None,
        });
        let exempt = type_only && entries.len() == 1 && mode.is_some_and(|(_, ok)| ok);
        let span = ast.span(a);
        // The rest are grammar errors (tsc: grammarErrorOnNode): none in a file with syntax
        // errors.
        if self.has_syntax_errors(file) {
            return bad_value;
        }
        if type_only && (entries.is_empty() || mode.is_some() && entries.len() > 1) {
            self.error_span(
                file,
                span,
                dg::TYPE_IMPORT_ATTRIBUTES_SHOULD_HAVE_EXACTLY_ONE_KEY_RESOLUTION_MODE_WITH_VALUE_IM_1464,
                &[],
            );
        }
        let info = match self.c.opts.module {
            M::Node18 | M::Node20 | M::NodeNext if self.builds_into_node_commonjs(file) => {
                (!exempt).then_some(
                    dg::IMPORT_ATTRIBUTES_ARE_NOT_ALLOWED_ON_STATEMENTS_THAT_COMPILE_TO_COMMONJS_REQUIRE_2856,
                )
            }
            M::ESNext | M::Node18 | M::Node20 | M::NodeNext | M::Preserve => {
                if type_only {
                    (!exempt).then_some(
                        dg::IMPORT_ATTRIBUTES_CANNOT_BE_USED_WITH_TYPE_ONLY_IMPORTS_OR_EXPORTS_2857,
                    )
                } else {
                    mode.map(|_| dg::RESOLUTION_MODE_CAN_ONLY_BE_SET_FOR_TYPE_ONLY_IMPORTS_1454)
                }
            }
            M::AMD | M::UMD | M::System => None,
            _ => (!exempt).then_some(
                dg::IMPORT_ATTRIBUTES_ARE_ONLY_SUPPORTED_WHEN_THE_MODULE_OPTION_IS_SET_TO_ESNEXT_NOD_2823,
            ),
        };
        if let Some(info) = info {
            self.error_span(file, span, info, &[]);
        }
        if type_only && let Some((value, false)) = mode {
            self.check_resolution_mode_value(Loc::new(file, value));
        }
        bad_value
    }

    /// The attributes `{ k: v, ... }` of an import or export as an object literal type
    /// (each value's regular literal type) checked assignable to `ImportAttributes |
    /// undefined` (TS2322 at the attributes).
    fn check_import_attributes_type(&mut self, a: Loc) {
        let Some(target) = self.global_type_reported(crate::checker::GlobalType::ImportAttributes)
        else {
            return;
        };
        let ast = self.ast(a.file);
        let Node::ImportAttributes { entries, .. } = *ast.node(a.node) else {
            return;
        };
        let mut props = Vec::new();
        for &e in ast.list(entries) {
            let Node::ImportAttribute { key, value } = *ast.node(e) else {
                continue;
            };
            let Some(name) = key_text(ast, key) else {
                continue;
            };
            let vt = self.check_expr(Loc::new(a.file, value));
            let vt = self.regular_of_literal(vt);
            let n = self.store().intern_str(&name);
            let p = self.c.new_synth(
                crate::symbols::SynthSym::new(
                    n,
                    SymbolFlags::PROPERTY,
                    crate::symbols::check_flags::OBJECT_LITERAL,
                )
                .with_type(vt),
            );
            props.push((n, p));
        }
        let source = self.store().literal_object(
            ts_r_types::SymId::NONE,
            ts_r_types::ObjectFlags::OBJECT_LITERAL | ts_r_types::ObjectFlags::NON_INFERRABLE,
            ts_r_types::ResolvedMembers::new(props, Vec::new(), Vec::new(), Vec::new()),
            ts_r_types::TypeTraits::CONTAINS_OBJECT_LITERAL,
        );
        let target = self.union2(target, TypeId::UNDEFINED);
        self.check_assignable_with_elaboration(source, target, a, None);
    }

    /// TS1453 at a `resolution-mode` attribute value that is not `"import"`/`"require"`.
    fn check_resolution_mode_value(&mut self, value: Loc) {
        self.error(
            value,
            dg::RESOLUTION_MODE_SHOULD_BE_EITHER_REQUIRE_OR_IMPORT_1453,
            &[],
        );
    }

    /// The namespace (`namespace N { }`, not `declare module "x" { }` or `declare global { }`)
    /// whose block directly holds statement `stmt`.
    fn enclosing_namespace(&self, file: FileId, stmt: NodeId) -> Option<NodeId> {
        let ast = self.ast(file);
        let parents = self.c.file(file).parents();
        let block = parents.parent(stmt)?;
        if !matches!(ast.node(block), Node::ModuleBlock { .. }) {
            return None;
        }
        let m = parents.parent(block)?;
        match *ast.node(m) {
            Node::Module { kind, name, .. }
                if kind != ts_r_ast::ModuleKind::Global
                    && !matches!(ast.node(name), Node::StringLit { .. }) =>
            {
                Some(m)
            }
            _ => None,
        }
    }

    /// Whether statement `stmt` is outside the top level of a file or module block (in a
    /// block, a function body, a labeled or `if` statement...).
    fn stmt_misplaced(&self, file: FileId, stmt: NodeId) -> bool {
        let ast = self.ast(file);
        self.c.file(file).parents().parent(stmt).is_some_and(|p| {
            !matches!(ast.node(p), Node::Program { .. } | Node::ModuleBlock { .. })
        })
    }

    /// A module element outside the top level of a file or module block: TS1235 (namespace),
    /// TS1234 (`declare module "x"`, `declare global`), TS1231 (`export =`), TS1258
    /// (`export default` of an expression), TS1233 (export declaration; TS1474 in JavaScript),
    /// TS1232 (import; TS1473 in JavaScript). (Modifiers on other declarations there are
    /// TS1184, see modifiers.rs.) True when a module element (whose specifier is therefore not checked) was reported.
    fn check_misplaced_module_element(&mut self, l: Loc) -> bool {
        let file = l.file;
        let ast = self.ast(file);
        let info = match *ast.node(l.node) {
            Node::Module { kind, name, .. } => {
                if kind == ts_r_ast::ModuleKind::Global
                    || matches!(ast.node(name), Node::StringLit { .. })
                {
                    dg::AN_AMBIENT_MODULE_DECLARATION_IS_ONLY_ALLOWED_AT_THE_TOP_LEVEL_IN_A_FILE_1234
                } else {
                    dg::A_NAMESPACE_DECLARATION_IS_ONLY_ALLOWED_AT_THE_TOP_LEVEL_OF_A_NAMESPACE_OR_MODUL_1235
                }
            }
            Node::ExportAssignment { .. } => {
                dg::AN_EXPORT_ASSIGNMENT_MUST_BE_AT_THE_TOP_LEVEL_OF_A_FILE_OR_MODULE_DECLARATION_1231
            }
            Node::ExportDefaultExpr { .. } => {
                dg::A_DEFAULT_EXPORT_MUST_BE_AT_THE_TOP_LEVEL_OF_A_FILE_OR_MODULE_DECLARATION_1258
            }
            // JavaScript has no namespaces: TS1474/TS1473 there.
            Node::ExportNamed { .. } | Node::ExportAll { .. } if self.c.file(file).is_js() => {
                dg::AN_EXPORT_DECLARATION_CAN_ONLY_BE_USED_AT_THE_TOP_LEVEL_OF_A_MODULE_1474
            }
            Node::ImportDecl { .. } if self.c.file(file).is_js() => {
                dg::AN_IMPORT_DECLARATION_CAN_ONLY_BE_USED_AT_THE_TOP_LEVEL_OF_A_MODULE_1473
            }
            Node::ExportNamed { .. } | Node::ExportAll { .. } => {
                dg::AN_EXPORT_DECLARATION_CAN_ONLY_BE_USED_AT_THE_TOP_LEVEL_OF_A_NAMESPACE_OR_MODULE_1233
            }
            Node::ImportDecl { .. } | Node::ImportEquals { .. } => {
                dg::AN_IMPORT_DECLARATION_CAN_ONLY_BE_USED_AT_THE_TOP_LEVEL_OF_A_NAMESPACE_OR_MODULE_1232
            }
            _ => return false,
        };
        if !self.stmt_misplaced(file, l.node) {
            return false;
        }
        let span = ast.span(l.node);
        self.error_span(file, span, info, &[]);
        true
    }

    /// Whether module request `req` belongs to an import or export statement directly in a
    /// namespace or outside the top level: tsc never resolves those (TS1147/TS1194/TS1232/
    /// TS1233 instead).
    fn request_in_namespace(&self, file: FileId, req: &ts_r_binder::ModuleRequest) -> bool {
        if !matches!(
            req.kind,
            RequestKind::Import | RequestKind::ExportFrom | RequestKind::ImportEquals
        ) {
            return false;
        }
        let ast = self.ast(file);
        let parents = self.c.file(file).parents();
        let Some(mut stmt) = parents.parent(req.node) else {
            return false;
        };
        // JavaScript `require("m")` is resolved wherever it is.
        if matches!(ast.node(stmt), Node::Call { .. }) {
            return false;
        }
        if matches!(ast.node(stmt), Node::ExternalModuleRef { .. }) {
            let Some(p) = parents.parent(stmt) else {
                return false;
            };
            stmt = p;
        }
        self.enclosing_namespace(file, stmt).is_some() || self.stmt_misplaced(file, stmt)
    }

    /// An import or export statement directly in a namespace: TS1147 at the module specifier
    /// of an import (`import ... from "x"`, `import x = require("x")`), TS1194 at a re-export
    /// (at its specifier) and at `export { a }` of a non-ambient namespace. True
    /// when the statement names a module that is therefore not checked further.
    fn check_namespace_module_statement(&mut self, l: Loc) -> bool {
        let file = l.file;
        if self.enclosing_namespace(file, l.node).is_none() {
            return false;
        }
        let ast = self.ast(file);
        match *ast.node(l.node) {
            Node::ImportDecl { source, .. } => {
                self.error(
                    Loc::new(file, source),
                    dg::IMPORT_DECLARATIONS_IN_A_NAMESPACE_CANNOT_REFERENCE_A_MODULE_1147,
                    &[],
                );
                true
            }
            Node::ImportEquals { module_ref, .. } => match *ast.node(module_ref) {
                Node::ExternalModuleRef { expr } => {
                    self.error(
                        Loc::new(file, expr),
                        dg::IMPORT_DECLARATIONS_IN_A_NAMESPACE_CANNOT_REFERENCE_A_MODULE_1147,
                        &[],
                    );
                    true
                }
                _ => false,
            },
            Node::ExportNamed { source, .. } => {
                if source.is_some() || !self.is_ambient_node(l) {
                    self.error(
                        Loc::new(file, source.unwrap_or(l.node)),
                        dg::EXPORT_DECLARATIONS_ARE_NOT_PERMITTED_IN_A_NAMESPACE_1194,
                        &[],
                    );
                }
                source.is_some()
            }
            Node::ExportAll { source, .. } => {
                self.error(
                    Loc::new(file, source),
                    dg::EXPORT_DECLARATIONS_ARE_NOT_PERMITTED_IN_A_NAMESPACE_1194,
                    &[],
                );
                true
            }
            _ => false,
        }
    }

    /// TS2439 (at the statement): an import or export inside `declare module "x" { }` (in a
    /// script file) names a relative module.
    fn check_relative_in_ambient_module(&mut self, l: Loc) {
        let file = l.file;
        if !self.reporting(file) || self.c.file(file).bound.is_module {
            return;
        }
        let ast = self.ast(file);
        let source = match *ast.node(l.node) {
            Node::ImportDecl { source, .. }
            | Node::ExportNamed {
                source: Some(source),
                ..
            }
            | Node::ExportAll { source, .. } => source,
            Node::ImportEquals { module_ref, .. } => match *ast.node(module_ref) {
                Node::ExternalModuleRef { expr } => expr,
                _ => return,
            },
            _ => return,
        };
        let Node::StringLit { value } = *ast.node(source) else {
            return;
        };
        let spec = ast.atom(value);
        let relative = spec == "."
            || spec == ".."
            || spec.starts_with("./")
            || spec.starts_with("../")
            || spec.starts_with(".\\")
            || spec.starts_with("..\\");
        if !relative {
            return;
        }
        let parents = self.c.file(file).parents();
        let mut cur = parents.parent(l.node);
        let mut guard = 0;
        while let Some(n) = cur {
            guard += 1;
            if guard > 10_000 {
                return;
            }
            if let Node::Module { name, .. } = *ast.node(n) {
                if matches!(ast.node(name), Node::StringLit { .. }) {
                    self.error(
                        l,
                        dg::IMPORT_OR_EXPORT_DECLARATION_IN_AN_AMBIENT_MODULE_DECLARATION_CANNOT_REFERENCE_M_2439,
                        &[],
                    );
                }
                return;
            }
            cur = parents.parent(n);
        }
    }

    /// TS2498 at the specifier of `export * from "m"` (also `export * as ns`) when `m`
    /// has an `export =` (tsc: tsc/internal/checker/checker.go checkExportDeclaration).
    fn check_export_star_of_export_equals(&mut self, l: Loc) {
        let file = l.file;
        if self.enclosing_namespace(file, l.node).is_some() {
            return;
        }
        let ast = self.ast(file);
        let Node::ExportAll { source, .. } = *ast.node(l.node) else {
            return;
        };
        let Some(req) = self
            .c
            .bound(file)
            .module_requests
            .iter()
            .position(|r| r.node == source)
        else {
            return;
        };
        let target = self.c.link.module_target(file, req as u32);
        let Some(&first) = self.c.link.module_symbols(target).first() else {
            return;
        };
        let module_sym = self.c.sym_ref(first);
        if self.c.export_of(module_sym, "export=").is_none() {
            return;
        }
        let name = match target {
            ts_r_binder::program::ModuleTarget::File(f) => {
                format!("\"{}\"", crate::print::strip_ts_ext(&self.c.file(f).path))
            }
            ts_r_binder::program::ModuleTarget::Ambient(i) => {
                match self.c.link.ambient_modules.get(i as usize) {
                    Some((n, _)) => format!("\"{n}\""),
                    None => return,
                }
            }
            ts_r_binder::program::ModuleTarget::Unresolved => return,
        };
        self.error(
            Loc::new(file, source),
            dg::MODULE_0_USES_EXPORT_AND_CANNOT_BE_USED_WITH_EXPORT_2498,
            &[&name],
        );
    }

    /// TS2206/TS2207 (grammar): a `type` specifier inside `import type { }` /
    /// `export type { }`, at the first such specifier's `type` keyword (tsc:
    /// tsc/internal/checker/grammarchecks.go checkGrammarTypeOnlyNamedImportsOrExports).
    fn check_type_modifier_in_type_only_list(&mut self, l: Loc) {
        let file = l.file;
        let ast = self.ast(file);
        let (specifiers, info) = match *ast.node(l.node) {
            Node::ImportDecl {
                type_only: true,
                specifiers,
                ..
            } => (
                specifiers,
                dg::THE_TYPE_MODIFIER_CANNOT_BE_USED_ON_A_NAMED_IMPORT_WHEN_IMPORT_TYPE_IS_USED_ON_I_2206,
            ),
            Node::ExportNamed {
                type_only: true,
                specifiers,
                ..
            } => (
                specifiers,
                dg::THE_TYPE_MODIFIER_CANNOT_BE_USED_ON_A_NAMED_EXPORT_WHEN_EXPORT_TYPE_IS_USED_ON_I_2207,
            ),
            _ => return,
        };
        // TS1363: `import type A, { B }` / `import type A, * as ns`, at the `type` keyword.
        // tsc reports it instead of TS2206 (tsc: grammarchecks.go checkGrammarImportClause).
        if let Node::ImportDecl { .. } = ast.node(l.node)
            && let [first, _, ..] = ast.list(specifiers)
            && matches!(ast.node(*first), Node::ImportDefaultSpecifier { .. })
        {
            if self.has_syntax_errors(file) {
                return;
            }
            let start = ast.span(l.node).start;
            let end = ast.span(*first).start;
            let text = &self.c.file(file).text;
            let head = text.get(start as usize..end as usize).unwrap_or("");
            if let Some(at) = head.get(6..).and_then(|rest| rest.find("type")) {
                let at = start + 6 + at as u32;
                self.error_span(
                    file,
                    Span::new(at, at + 4),
                    dg::A_TYPE_ONLY_IMPORT_CAN_SPECIFY_A_DEFAULT_IMPORT_OR_NAMED_BINDINGS_BUT_NOT_BOTH_1363,
                    &[],
                );
            }
            return;
        }
        let first = ast.list(specifiers).iter().copied().find(|&s| {
            matches!(
                ast.node(s),
                Node::ImportSpecifier {
                    type_only: true,
                    ..
                } | Node::ExportSpecifier {
                    type_only: true,
                    ..
                }
            )
        });
        let Some(s) = first else {
            return;
        };
        if self.has_syntax_errors(file) {
            return;
        }
        let start = ast.span(s).start;
        self.error_span(file, Span::new(start, start + 4), info, &[]);
    }

    fn check_import_decl(&mut self, l: Loc) {
        let file = l.file;
        let ast = self.ast(file);
        let (specifiers, source) = match *ast.node(l.node) {
            Node::ImportDecl {
                specifiers, source, ..
            } => (specifiers, source),
            Node::ExportNamed {
                specifiers,
                source: Some(source),
                ..
            } => (specifiers, source),
            _ => return,
        };
        // Misplaced imports (inside a block: a grammar error) are not checked further.
        let parents = self.c.file(file).parents();
        if !matches!(
            parents.parent(l.node).map(|p| ast.node(p)),
            Some(Node::Program { .. } | Node::ModuleBlock { .. })
        ) {
            return;
        }
        let spec = match *ast.node(source) {
            Node::StringLit { value } => ast.atom(value).to_owned(),
            _ => return,
        };
        // A relative import inside an ambient module declaration is never followed (TS2439).
        if let Some(i) = self
            .c
            .bound(file)
            .module_requests
            .iter()
            .position(|r| r.node == source && r.in_ambient_module)
            && self.c.link.module_target(file, i as u32)
                == ts_r_binder::program::ModuleTarget::Unresolved
        {
            return;
        }
        let req = self
            .c
            .bound(file)
            .module_requests
            .iter()
            .find(|r| r.node == source);
        let resolved = match req {
            Some(r) => self.c.resolution(file, &spec, r),
            None => {
                let path = self.c.file(file).path.clone();
                self.c.resolutions.get(&(path, spec.clone()))
            }
        };
        let resolved_file = match resolved {
            Some(Some(r)) if r.included => self.c.files.iter().position(|f| f.path == r.resolved),
            _ => None,
        };
        let module_sym = match resolved_file {
            Some(target_idx) => {
                let target = &self.c.files[target_idx];
                // A JavaScript script (no ECMAScript or CommonJS module syntax) is TS2306
                // too, a JavaScript ES module is checked as a TypeScript one; CommonJS
                // modules and JSON are not checked further here.
                if target.is_js() && (target.is_json() || target.is_commonjs_module()) {
                    return;
                }
                if !target.bound.is_module {
                    // Importing names from a script (`import * as m` of a declaration file
                    // without imports or exports): TS2306. Side-effect imports are fine.
                    let names = match *ast.node(l.node) {
                        Node::ImportDecl { has_clause, .. } => has_clause,
                        _ => true,
                    };
                    // A `.tsx` file without `jsx` set is not reported (observed tsc 7).
                    let unsupported_tsx = !self.c.opts.jsx_set
                        && (target.path.ends_with(".tsx") || target.path.ends_with(".jsx"));
                    if names && !unsupported_tsx && !self.is_ambient_module_name(&spec) {
                        let p = target.path.clone();
                        self.error(
                            Loc::new(file, source),
                            dg::FILE_0_IS_NOT_A_MODULE_2306,
                            &[&p],
                        );
                    }
                    return;
                }
                self.c
                    .sym(FileId(target_idx as u32), target.bound.file_symbol)
            }
            // `declare module "x" { ... }` (not the shorthand form, whose exports are `any`).
            None => {
                let bound = self.c.bound(file);
                let Some(req) = bound.module_requests.iter().position(|r| r.node == source) else {
                    return;
                };
                let target = self.c.link.module_target(file, req as u32);
                if !matches!(target, ts_r_binder::program::ModuleTarget::Ambient(_)) {
                    return;
                }
                let Some(&first) = self.c.link.module_symbols(target).first() else {
                    return;
                };
                let s = self.c.sym_ref(first);
                if self.c.is_shorthand_ambient_module(s) {
                    return;
                }
                s
            }
        };
        // `export =`: named imports are the entity's exports, or properties of its value
        // (tsc: tsc/internal/checker/checker.go getExternalModuleMember).
        let mut entity_value = None;
        if let Some(eq) = self.c.export_of(module_sym, "export=") {
            let entity = self.c.resolve_alias(eq).unwrap_or(eq);
            let f = self.c.sym_flags(entity);
            if f.intersects(SymbolFlags::ALIAS | SymbolFlags::MODULE_FILE) {
                return;
            }
            if f.intersects(SymbolFlags::VALUE) {
                entity_value = Some(entity);
            }
        }
        for &s in ast.list(specifiers) {
            let sl = Loc::new(file, s);
            match *ast.node(s) {
                Node::ImportSpecifier {
                    imported: Some(name_node),
                    ..
                }
                | Node::ImportSpecifier {
                    imported: None,
                    local: name_node,
                    ..
                }
                | Node::ExportSpecifier {
                    local: name_node, ..
                } => {
                    let name = match *ast.node(name_node) {
                        Node::Ident { name } | Node::StringLit { value: name } => {
                            ast.atom(name).to_owned()
                        }
                        _ => continue,
                    };
                    // A re-exported `default` may be synthesized, as for a default import.
                    if self.c.export_of(module_sym, &name).is_some()
                        || (name == "default"
                            && matches!(ast.node(s), Node::ExportSpecifier { .. })
                            && self.c.opts.allow_synthetic_default_imports
                            && (self.c.export_of(module_sym, "export=").is_some()
                                || self.c.can_have_synthetic_default(file, module_sym)))
                    {
                        continue;
                    }
                    if let Some(v) = entity_value
                        && self.has_own_property(v, &name)
                    {
                        continue;
                    }
                    self.report_missing_import_member(
                        Loc::new(file, name_node),
                        &spec,
                        &name,
                        module_sym,
                    );
                }
                Node::ImportDefaultSpecifier { local } => {
                    // With synthetic defaults allowed, a TypeScript source module or one
                    // exporting `__esModule` still needs a real default export.
                    if self.c.export_of(module_sym, "default").is_none()
                        && (!self.c.opts.allow_synthetic_default_imports
                            || ((self.c.export_of(module_sym, "export=").is_none()
                                || self.c.esm_imports_esm(file, module_sym))
                                && !self.c.can_have_synthetic_default(file, module_sym)))
                    {
                        let m = match resolved_file {
                            Some(i) => {
                                format!("\"{}\"", crate::print::strip_ts_ext(&self.c.files[i].path))
                            }
                            None => format!("\"{spec}\""),
                        };
                        let _ = sl;
                        // A module exporting the local's name itself: the import was
                        // likely meant to be named (tsc: tsc/internal/checker/checker.go
                        // reportNonDefaultExport; its own export table, not `export *`).
                        let local_name = match *ast.node(local) {
                            Node::Ident { name } => ast.atom(name).to_owned(),
                            _ => String::new(),
                        };
                        if !local_name.is_empty() && self.c.has_own_export(module_sym, &local_name)
                        {
                            self.error(
                                Loc::new(file, local),
                                dg::MODULE_0_HAS_NO_DEFAULT_EXPORT_DID_YOU_MEAN_TO_USE_IMPORT_1_FROM_0_INSTEAD_2613,
                                &[&m, &local_name],
                            );
                        } else {
                            self.error(
                                Loc::new(file, local),
                                dg::MODULE_0_HAS_NO_DEFAULT_EXPORT_1192,
                                &[&m],
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether the value of `v` (an `export =` entity) has property `name` of its own:
    /// `Object`'s and `Function`'s members do not count (`import { toString } from "./ns"`
    /// is TS2305); a union needs it in every member, an intersection in one.
    fn has_own_property(&mut self, v: ts_r_types::SymId, name: &str) -> bool {
        let t = self.type_of_symbol(v);
        let t = self.apparent_type(t);
        let key = self.store().intern_str(name);
        let f = self.store().flags(t);
        let parts = if f.intersects(TypeFlags::UNION | TypeFlags::INTERSECTION) {
            self.store().members_of_union(t).to_vec()
        } else {
            vec![t]
        };
        let has = |ck: &mut Self, m: TypeId| {
            let m = ck.apparent_type(m);
            ck.store().flags(m).contains(TypeFlags::OBJECT)
                && ck.resolved_members(m).find(key).is_some()
        };
        if f.contains(TypeFlags::UNION) {
            parts.iter().all(|&m| has(self, m))
        } else {
            parts.iter().any(|&m| has(self, m))
        }
    }

    /// TS2305 for a missing named import: TS2724 with a spelling suggestion among the
    /// module's exports, else TS2614 when the module has a default export.
    fn report_missing_import_member(
        &mut self,
        at: Loc,
        spec: &str,
        name: &str,
        module_sym: ts_r_types::SymId,
    ) {
        let m = format!("\"{spec}\"");
        let eq = self.c.export_of(module_sym, "export=");
        let holder = match eq {
            Some(e) => self.c.resolve_alias(e).unwrap_or(e),
            None => module_sym,
        };
        let names: Vec<String> = self
            .c
            .exports_of(holder)
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| n != "default" && n != "export=")
            .collect();
        if let Some(sug) =
            crate::relate::spelling_suggestion(name, names.iter().map(String::as_str))
        {
            self.error(
                at,
                dg::_0_HAS_NO_EXPORTED_MEMBER_NAMED_1_DID_YOU_MEAN_2_2724,
                &[&m, name, &sug],
            );
            return;
        }
        if eq.is_none() && self.c.export_of(module_sym, "default").is_some() {
            self.error(
                at,
                dg::MODULE_0_HAS_NO_EXPORTED_MEMBER_1_DID_YOU_MEAN_TO_USE_IMPORT_1_FROM_0_INSTEAD_2614,
                &[&m, name],
            );
            return;
        }
        // The `export =` entity itself, imported by its local name: only a default import
        // (or `import x = require`) reaches it (TS2595/TS2597/TS2616; tsc:
        // tsc/internal/checker/checker.go reportNonExportedMember).
        if let Some(e) = eq
            && let Some(local) = self.module_local(module_sym, name)
            && self.c.resolve_alias(local).unwrap_or(local) == self.c.resolve_alias(e).unwrap_or(e)
        {
            use ts_r_config::ModuleKind as M;
            if !matches!(
                self.c.opts.module,
                M::CommonJS | M::AMD | M::UMD | M::System
            ) {
                self.error(
                    at,
                    dg::_0_CAN_ONLY_BE_IMPORTED_BY_USING_A_DEFAULT_IMPORT_2595,
                    &[name],
                );
            } else if self.c.file(at.file).is_js() {
                self.error(
                    at,
                    dg::_0_CAN_ONLY_BE_IMPORTED_BY_USING_A_REQUIRE_CALL_OR_BY_USING_A_DEFAULT_IMPORT_2597,
                    &[name],
                );
            } else {
                self.error(
                    at,
                    dg::_0_CAN_ONLY_BE_IMPORTED_BY_USING_IMPORT_1_REQUIRE_2_OR_A_DEFAULT_IMPORT_2616,
                    &[name, name, &m],
                );
            }
            return;
        }
        // Declared in the module but not exported (TS2459), or exported under another name
        // (TS2460, first such name in export order).
        if eq.is_none()
            && let Some(local) = self.module_local(module_sym, name)
        {
            let target = self.c.resolve_alias(local).unwrap_or(local);
            let other = self
                .c
                .exports_of(module_sym)
                .into_iter()
                .find(|(n, e)| {
                    n != "default"
                        && n != "export="
                        && (*e == local || self.c.resolve_alias(*e).unwrap_or(*e) == target)
                })
                .map(|(n, _)| n);
            match other {
                Some(o) => self.error(
                    at,
                    dg::MODULE_0_DECLARES_1_LOCALLY_BUT_IT_IS_EXPORTED_AS_2_2460,
                    &[&m, name, &o],
                ),
                None => self.error(
                    at,
                    dg::MODULE_0_DECLARES_1_LOCALLY_BUT_IT_IS_NOT_EXPORTED_2459,
                    &[&m, name],
                ),
            }
            return;
        }
        self.error(at, dg::MODULE_0_HAS_NO_EXPORTED_MEMBER_1_2305, &[&m, name]);
    }

    /// A module-level local (not exported) named `name` in any declaration of the module
    /// symbol: a module file's top level or an ambient module's body.
    fn module_local(&self, module_sym: ts_r_types::SymId, name: &str) -> Option<ts_r_types::SymId> {
        for d in self.c.decls(module_sym) {
            let ast = self.ast(d.file);
            let body = match *ast.node(d.node) {
                Node::Program { .. } => d.node,
                Node::Module { body: Some(b), .. } => b,
                _ => continue,
            };
            let b = &self.c.file(d.file).bound;
            let Some(scope) = b.scope_of(body) else {
                continue;
            };
            let locals = b.scopes[scope.0 as usize].locals;
            if let Some(s) = b.lookup_str(ast, locals, name) {
                return Some(self.c.sym(d.file, s));
            }
        }
        None
    }

    // ---------------------------------------------------------------------------------
    // Grammar: misplaced modifiers
    // ---------------------------------------------------------------------------------

    fn check_modifiers(&mut self, file: FileId) {
        let ast = self.ast(file);
        self.check_dts_top_level_modifiers(file);
        let mut stack = vec![(ast.root, NodeKind::Program)];
        let mut guard = 0u32;
        while let Some((n, parent_kind)) = stack.pop() {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            let node = *ast.node(n);
            if matches!(
                node,
                Node::VarDecl { .. }
                    | Node::FunctionDecl { .. }
                    | Node::ClassDecl { .. }
                    | Node::Interface { .. }
                    | Node::TypeAlias { .. }
                    | Node::Enum { .. }
                    | Node::Module { .. }
                    | Node::PropertyDef { .. }
                    | Node::MethodDef { .. }
                    | Node::IndexSignature { .. }
                    | Node::PropertySignature { .. }
                    | Node::MethodSignature { .. }
                    | Node::Param { .. }
                    | Node::StaticBlock { .. }
                    | Node::ImportEquals { .. }
                    | Node::ImportDecl { .. }
                    | Node::ExportNamed { .. }
                    | Node::ExportAll { .. }
                    | Node::ExportDefaultExpr { .. }
                    | Node::TypeParam { .. }
            ) {
                self.check_declaration_modifiers(file, n);
            }
            if let Node::IndexSignature { .. } = node {
                self.check_index_key_type(Loc::new(file, n));
                self.check_duplicate_index_signatures(Loc::new(file, n));
            }
            if let Node::VarDecl { .. } = node {
                self.check_let_binding_names(file, n);
                self.check_var_shadowing_block_scoped(file, n);
            }
            if let Node::ObjectLit { .. } = node {
                self.check_object_literal_duplicates(file, n);
            }
            if let Node::VarDeclarator { .. }
            | Node::Param { .. }
            | Node::PropertyDef { .. }
            | Node::PropertySignature { .. }
            | Node::MethodDef { .. }
            | Node::MethodSignature { .. } = node
            {
                self.check_circular_annotation(file, n);
            }
            if let Node::TypeAlias { .. } = node {
                self.check_circular_type_alias(file, n);
                self.check_intrinsic_type_alias(file, n);
            }
            if let Node::MappedType { .. } = node {
                self.check_mapped_type_keys(Loc::new(file, n));
            }
            if let Node::Rest { arg } = node {
                self.check_object_rest_source(file, n, arg);
            }
            if let Node::VarDeclarator { .. } | Node::Assign { .. } = node {
                self.check_abstract_this_destructuring(file, n);
            }
            if let Node::PropertySignature { .. } = node {
                self.check_property_signature_implicit_any(file, n);
            }
            // A bigint key names no property (tsc gives the member no name at all).
            if let Node::PropertyDef { key, .. } | Node::PropertySignature { key, .. } = node
                && matches!(self.ast(file).node(key), Node::BigIntLit { .. })
            {
                self.error(
                    Loc::new(file, key),
                    dg::A_BIGINT_LITERAL_CANNOT_BE_USED_AS_A_PROPERTY_NAME_1539,
                    &[],
                );
            }
            if let Node::MethodSignature { .. } = node {
                self.check_accessor_signature_implicit_any(file, n);
                self.check_method_signature_optionality(file, n);
            }
            if let Node::Ident { .. } | Node::Member { .. } | Node::Call { .. } | Node::New { .. } =
                node
            {
                self.check_uncalled_function_test(file, n);
            }
            // Unannotated parameters of signatures in types (`(x) => void`, `m(x): void`
            // in an interface) are implicitly `any`.
            if let Node::Param {
                type_ann: None,
                init: None,
                pattern,
                ..
            } = node
                && self.c.opts.no_implicit_any
                && matches!(
                    parent_kind,
                    NodeKind::FunctionType
                        | NodeKind::ConstructorType
                        | NodeKind::MethodSignature
                        | NodeKind::CallSignature
                        | NodeKind::ConstructSignature
                )
                && matches!(
                    ast.node(pattern),
                    Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                )
            {
                self.report_signature_binding_elements(Loc::new(file, pattern));
            }
            if let Node::Param {
                type_ann: None,
                init: None,
                pattern,
                rest,
                ..
            } = node
                && self.c.opts.no_implicit_any
                && matches!(
                    parent_kind,
                    NodeKind::FunctionType
                        | NodeKind::ConstructorType
                        | NodeKind::MethodSignature
                        | NodeKind::CallSignature
                        | NodeKind::ConstructSignature
                )
                && let Some(name) = ident_text(ast, pattern)
                && !self.is_setter_param_typed_by_getter(file, n)
            {
                if let Some(index) = self.param_named_like_type(file, n, parent_kind, name) {
                    let new_name = format!("arg{index}");
                    let type_name = if rest {
                        format!("{name}[]")
                    } else {
                        name.to_owned()
                    };
                    self.error(
                        Loc::new(file, n),
                        dg::PARAMETER_HAS_A_NAME_BUT_NO_TYPE_DID_YOU_MEAN_0_1_7051,
                        &[&new_name, &type_name],
                    );
                } else if rest {
                    self.error(
                        Loc::new(file, n),
                        dg::REST_PARAMETER_0_IMPLICITLY_HAS_AN_ANY_TYPE_7019,
                        &[name],
                    );
                } else {
                    self.error(
                        Loc::new(file, n),
                        dg::PARAMETER_0_IMPLICITLY_HAS_AN_1_TYPE_7006,
                        &[name, "any"],
                    );
                }
            }
            // Their binding patterns' names are too, unless they have a default.
            if let Node::Param {
                type_ann: None,
                init: None,
                pattern,
                ..
            } = node
                && self.c.opts.no_implicit_any
                && matches!(
                    parent_kind,
                    NodeKind::FunctionType
                        | NodeKind::ConstructorType
                        | NodeKind::MethodSignature
                        | NodeKind::CallSignature
                        | NodeKind::ConstructSignature
                )
                && matches!(
                    ast.node(pattern),
                    Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                )
            {
                self.report_implicit_binding_elements(Loc::new(file, pattern), true);
            }
            match node {
                Node::Property { modifiers, .. } | Node::ShorthandProp { modifiers, .. }
                    if !modifiers.is_empty() =>
                {
                    for t in ast.modifier_tokens_of(n).to_vec() {
                        let m = modifier_text(t.flag);
                        self.error_span(
                            file,
                            t.span,
                            dg::_0_MODIFIER_CANNOT_BE_USED_HERE_1042,
                            &[m],
                        );
                    }
                }
                // Object-literal methods: TS1042 on each modifier but `async` (a second
                // `async` is TS1030), and TS1184 at the first one on a plain method.
                Node::MethodDef { kind, .. } if parent_kind == NodeKind::ObjectLit => {
                    let toks = ast.modifier_tokens_of(n);
                    if let Some(second) = toks.iter().filter(|t| t.flag == Modifiers::ASYNC).nth(1)
                    {
                        self.error_span(
                            file,
                            second.span,
                            dg::_0_MODIFIER_ALREADY_SEEN_1030,
                            &["async"],
                        );
                    }
                    let bad: Vec<_> = toks
                        .iter()
                        .copied()
                        .filter(|t| t.flag != Modifiers::ASYNC)
                        .collect();
                    for (i, t) in bad.iter().enumerate() {
                        let m = modifier_text(t.flag);
                        self.error_span(
                            file,
                            t.span,
                            dg::_0_MODIFIER_CANNOT_BE_USED_HERE_1042,
                            &[m],
                        );
                        if i == 0 && kind == MethodKind::Method {
                            self.error_span(
                                file,
                                t.span,
                                dg::MODIFIERS_CANNOT_APPEAR_HERE_1184,
                                &[],
                            );
                        }
                    }
                }
                Node::Param {
                    modifiers,
                    pattern,
                    init,
                    ..
                } => {
                    self.check_param_shape(Loc::new(file, n));
                    let bodiless = (init.is_some()
                        || matches!(
                            ast.node(pattern),
                            Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                        ))
                        && !self.param_owner_has_body(Loc::new(file, n));
                    if modifiers.intersects(
                        Modifiers::PUBLIC
                            | Modifiers::PRIVATE
                            | Modifiers::PROTECTED
                            | Modifiers::READONLY
                            | Modifiers::OVERRIDE,
                    ) {
                        if !self.is_constructor_implementation_param(Loc::new(file, n)) {
                            self.error(Loc::new(file, n), dg::A_PARAMETER_PROPERTY_IS_ONLY_ALLOWED_IN_A_CONSTRUCTOR_IMPLEMENTATION_2369, &[]);
                        } else if !matches!(ast.node(pattern), Node::Ident { .. }) {
                            self.error(
                                Loc::new(file, n),
                                dg::A_PARAMETER_PROPERTY_MAY_NOT_BE_DECLARED_USING_A_BINDING_PATTERN_1187,
                                &[],
                            );
                        } else if matches!(*ast.node(pattern), Node::Ident { name } if ast.atom(name) == "constructor")
                        {
                            self.error(
                                Loc::new(file, pattern),
                                dg::CONSTRUCTOR_CANNOT_BE_USED_AS_A_PARAMETER_PROPERTY_NAME_2398,
                                &[],
                            );
                        }
                    }
                    if bodiless {
                        let mut at: Vec<NodeId> = Vec::new();
                        if init.is_some() {
                            at.push(n);
                        }
                        binding_initializers(ast, pattern, &mut at);
                        for a in at {
                            self.error(
                                Loc::new(file, a),
                                dg::A_PARAMETER_INITIALIZER_IS_ONLY_ALLOWED_IN_A_FUNCTION_OR_CONSTRUCTOR_IMPLEMENTAT_2371,
                                &[],
                            );
                        }
                    }
                }
                // `readonly T` needs an array or tuple type written there.
                Node::TypeOperator {
                    op: ts_r_ast::TypeOperatorKind::Readonly,
                    ty,
                } if !matches!(
                    ast.node(ty),
                    Node::ArrayType { .. } | Node::TupleType { .. }
                ) =>
                {
                    self.error(
                        Loc::new(file, n),
                        dg::READONLY_TYPE_MODIFIER_IS_ONLY_PERMITTED_ON_ARRAY_AND_TUPLE_LITERAL_TYPES_1354,
                        &[],
                    );
                }
                _ => {}
            }
            let kind = ast.kind(n);
            ast.for_each_child(n, |c| stack.push((c, kind)));
        }
    }

    /// TS2463 at an optional binding-pattern parameter `p` without a default in a function
    /// with a body, and TS2370 at a rest parameter named by an identifier whose type is not
    /// assignable to `readonly any[]` (not for generic types). Under strictNullChecks an
    /// optional rest parameter counts its `undefined` when annotated, or when unannotated
    /// in a function expression.
    fn check_param_shape(&mut self, p: Loc) {
        let Node::Param {
            pattern,
            init,
            optional,
            rest,
            type_ann,
            ..
        } = *self.node(p)
        else {
            return;
        };
        if is_this_param(self.ast(p.file), p.node) {
            return;
        }
        let is_ident = matches!(self.node(Loc::new(p.file, pattern)), Node::Ident { .. });
        if optional && init.is_none() && !is_ident && self.param_owner_has_body(p) {
            self.error(
                p,
                dg::A_BINDING_PATTERN_PARAMETER_CANNOT_BE_OPTIONAL_IN_AN_IMPLEMENTATION_SIGNATURE_2463,
                &[],
            );
        }
        if !rest || !is_ident {
            return;
        }
        let mut t = self.param_type(p);
        // Generic rest types (`...a: T`, `...a: infer A`, `...a: Parameters<F>`) are left
        // alone: their constraints are not resolved precisely enough here.
        if self.store().is_error(t) || t == TypeId::UNRESOLVED || self.is_generic_type(t) {
            return;
        }
        if optional
            && init.is_none()
            && self.c.opts.strict_null_checks
            && (type_ann.is_some() || self.param_of_function_expression(p))
        {
            t = self.union(&[t, TypeId::UNDEFINED]);
        }
        let any_array = self.create_array(TypeId::ANY, true);
        if any_array != TypeId::UNRESOLVED && !self.is_assignable(t, any_array) {
            self.error(p, dg::A_REST_PARAMETER_MUST_BE_OF_AN_ARRAY_TYPE_2370, &[]);
        }
    }

    /// Whether `p` is a parameter of a function expression, an arrow or an object-literal
    /// method: those get their parameter types assigned with the expression, which makes
    /// an optional rest parameter `T[] | undefined` (class methods and declarations not).
    fn param_of_function_expression(&self, p: Loc) -> bool {
        let Some(f) = self.c.parent(p) else {
            return false;
        };
        match *self.node(f) {
            Node::Arrow { .. } => true,
            Node::FunctionExpr { .. } => match self.c.parent(f) {
                Some(m) if matches!(self.node(m), Node::MethodDef { .. }) => self
                    .c
                    .parent(m)
                    .is_some_and(|o| matches!(self.node(o), Node::ObjectLit { .. })),
                _ => true,
            },
            _ => false,
        }
    }

    /// Whether parameter `p` belongs to a function with a body (not an overload, signature,
    /// function type or index signature): TS2371 otherwise when it has an initializer.
    fn param_owner_has_body(&self, p: Loc) -> bool {
        let Some(f) = self.c.parent(p) else {
            return true;
        };
        match *self.node(f) {
            Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body.is_some(),
            Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::CallSignature { .. }
            | Node::ConstructSignature { .. }
            | Node::MethodSignature { .. }
            | Node::IndexSignature { .. } => false,
            _ => true,
        }
    }

    fn is_constructor_implementation_param(&self, p: Loc) -> bool {
        let Some(f) = self.c.parent(p) else {
            return false;
        };
        let Node::FunctionExpr { body: Some(_), .. } = *self.node(f) else {
            return false;
        };
        matches!(
            self.c.parent(f).map(|m| *self.node(m)),
            Some(Node::MethodDef {
                kind: MethodKind::Constructor,
                ..
            })
        )
    }

    // ---------------------------------------------------------------------------------
    // Comment directives
    // ---------------------------------------------------------------------------------

    fn apply_comment_directives(
        &mut self,
        file: FileId,
        diags: Vec<Diagnostic>,
    ) -> Vec<Diagnostic> {
        let sf = self.c.file(file);
        let ast = &sf.ast;
        let text = sf.text.as_str();
        // (line of the directive, span, is_expect, used)
        let mut directives: Vec<(u32, Span, bool, bool)> = Vec::new();
        let lines = sf.lines();
        for c in &ast.comments {
            let body = c.span.text(text);
            // A line comment is read after its slashes (`/// @ts-ignore` too); a block
            // comment only on its last line, after leading blanks and any run of `/` and
            // `*`, and the directive spans that last line (tsc: scanner.go,
            // processCommentDirective).
            let (inner, start) = match c.kind {
                CommentKind::Line => (body.trim_start_matches('/'), c.span.start),
                _ => {
                    let last = body
                        .char_indices()
                        .rev()
                        .find(|&(_, ch)| matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
                        .map_or(0, |(i, ch)| i + ch.len_utf8());
                    let line = body[last..]
                        .trim_start_matches([' ', '\t'])
                        .trim_start_matches(['/', '*']);
                    (line, c.span.start + last as u32)
                }
            };
            let inner = inner.trim_start_matches([' ', '\t']);
            // A prefix is enough: `// @ts-ignore-line` and `@ts-expect-errors` are directives
            // too (tsc 7, observed).
            let is_expect = inner.starts_with("@ts-expect-error");
            let is_ignore = inner.starts_with("@ts-ignore");
            if !is_expect && !is_ignore {
                continue;
            }
            let line = lines.line_of(c.span.end.saturating_sub(1));
            directives.push((line, Span::new(start, c.span.end), is_expect, false));
        }
        if directives.is_empty() {
            return diags;
        }
        let mut out = Vec::with_capacity(diags.len());
        for d in diags {
            if d.file != file || !d.is_error() {
                out.push(d);
                continue;
            }
            let line = lines.line_of(d.span.start);
            if let Some(i) = self.directive_for_line(file, line, &directives) {
                directives[i].3 = true;
                continue;
            }
            out.push(d);
        }
        for (_, span, is_expect, used) in directives {
            if is_expect && !used {
                out.push(
                    Diagnostic::new(
                        crate::diag::msg(dg::UNUSED_TS_EXPECT_ERROR_DIRECTIVE_2578),
                        span,
                        &[],
                    )
                    .with_file(file),
                );
            }
        }
        out
    }

    /// The directive governing `line`: on the closest preceding line, skipping blank lines
    /// and comment-only lines.
    fn directive_for_line(
        &self,
        file: FileId,
        line: u32,
        directives: &[(u32, Span, bool, bool)],
    ) -> Option<usize> {
        let sf = self.c.file(file);
        let lines = sf.lines();
        let text = sf.text.as_str();
        let mut l = line;
        for _ in 0..1000 {
            if l == 0 {
                return None;
            }
            l -= 1;
            if let Some(i) = directives.iter().position(|d| d.0 == l) {
                return Some(i);
            }
            let span = lines.line_span(l)?;
            let t = span.text(text).trim();
            if t.is_empty() || t.starts_with("//") || (t.starts_with("/*") && t.ends_with("*/")) {
                continue;
            }
            return None;
        }
        None
    }
}

/// The `this.<name>` / `this[<key>]` targets a constructor body assigns (outside nested
/// functions), destructuring included; `None` past the walk budget.
fn this_assignment_targets(ast: &ts_r_ast::Ast, body: NodeId) -> Option<Vec<NodeId>> {
    let is_target = |n: NodeId| match *ast.node(n) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            matches!(ast.node(obj), Node::This {})
        }
        _ => false,
    };
    let mut out = Vec::new();
    let mut stack = vec![body];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 200_000 {
            return None;
        }
        match *ast.node(n) {
            Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassExpr { .. }
            | Node::ClassDecl { .. } => continue,
            Node::Assign { left, .. } => {
                let l = skip_parens(ast, left);
                match *ast.node(l) {
                    Node::Member { .. } | Node::Index { .. } => {
                        if is_target(l) {
                            out.push(l);
                        }
                    }
                    // Destructuring assignment: assigned when a target is `this.<name>`.
                    Node::ObjectLit { .. } | Node::ArrayLit { .. } => {
                        let mut inner = vec![l];
                        let mut g = 0u32;
                        while let Some(x) = inner.pop() {
                            g += 1;
                            if g > 100_000 {
                                return None;
                            }
                            if is_target(x) {
                                out.push(x);
                            }
                            ast.for_each_child(x, |c| inner.push(c));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        ast.for_each_child(n, |c| stack.push(c));
    }
    Some(out)
}

impl Ck<'_, '_> {
    /// The rules on one enum member initializer `init` of type `t`: a const enum's value must
    /// be a finite number (TS2477, TS2478 for NaN) or a constant at all (TS2474); a computed
    /// member of a regular enum must be a number (TS18033). Ambient enums get TS1066 elsewhere.
    /// TS1061 at a member without an initializer whose previous member (in the same
    /// declaration block) has no numeric value (`A = "s", B`, `A = f(), B`). Ambient
    /// non-const enums have no auto values and are skipped.
    fn check_enum_auto_members(&mut self, enum_decl: Loc) {
        let ast = self.ast(enum_decl.file);
        let Node::Enum {
            members, modifiers, ..
        } = *ast.node(enum_decl.node)
        else {
            return;
        };
        if !modifiers.contains(Modifiers::CONST) && self.is_ambient_node(enum_decl) {
            return;
        }
        let Some(es) = self.c.sym_of_decl(enum_decl) else {
            return;
        };
        let values = self.enum_members(es);
        let mut prev_numeric = true;
        let mut prev_init: Option<NodeId> = None;
        for &m in ast.list(members) {
            let Node::EnumMember { name, init } = *ast.node(m) else {
                continue;
            };
            let ml = Loc::new(enum_decl.file, m);
            let value = self
                .c
                .sym_of_decl(ml)
                .and_then(|ms| values.iter().find(|&&(s, _)| s == ms))
                .and_then(|&(_, v)| v);
            let numeric = value.is_some_and(|v| self.store().number_value(v).is_some());
            match init {
                None if !prev_numeric => {
                    self.error(
                        Loc::new(enum_decl.file, name),
                        dg::ENUM_MEMBER_MUST_HAVE_INITIALIZER_1061,
                        &[],
                    );
                    prev_numeric = false;
                }
                None => {
                    // With `isolatedModules`, an auto value after an initializer that reads
                    // a constant of another file is TS18056.
                    if self.c.opts.isolated_modules
                        && let Some(pi) = prev_init
                        && self.enum_init_origin(Loc::new(enum_decl.file, pi), 0).1
                    {
                        self.error(
                            Loc::new(enum_decl.file, name),
                            dg::ENUM_MEMBER_FOLLOWING_A_NON_LITERAL_NUMERIC_MEMBER_MUST_HAVE_AN_INITIALIZER_WHEN_18056,
                            &[],
                        );
                    }
                    prev_numeric = numeric;
                }
                // A forward member reference is TS2651 and counts as 0.
                Some(i)
                    if !numeric
                        && self.names_later_enum_member(ml, Loc::new(enum_decl.file, i)) =>
                {
                    prev_numeric = true;
                }
                _ => prev_numeric = numeric,
            }
            prev_init = init;
        }
    }

    /// How an enum initializer `e` gets its value, for the `isolatedModules` checks: whether
    /// it is a string by its syntax alone (a string literal or template, `+` with one), and
    /// whether it reads a constant declared in another file. Only parentheses are looked
    /// through; references follow enum members and untyped constants with an initializer.
    fn enum_init_origin(&mut self, e: Loc, depth: u32) -> (bool, bool) {
        if depth > 32 {
            return (false, false);
        }
        let ast = self.ast(e.file);
        let n = skip_parens(ast, e.node);
        let at = |x: NodeId| Loc::new(e.file, x);
        match *ast.node(n) {
            Node::StringLit { .. } => (true, false),
            Node::TemplateLit { parts } => {
                let mut other = false;
                for (i, &p) in ast.list(parts).iter().enumerate() {
                    if i % 2 == 1 {
                        other |= self.enum_init_origin(at(p), depth + 1).1;
                    }
                }
                (true, other)
            }
            Node::Unary { arg, .. } => (false, self.enum_init_origin(at(arg), depth + 1).1),
            Node::Binary { left, op, right } => {
                let (ls, lo) = self.enum_init_origin(at(left), depth + 1);
                let (rs, ro) = self.enum_init_origin(at(right), depth + 1);
                ((ls || rs) && op == ts_r_ast::BinaryOp::Add, lo || ro)
            }
            Node::Ident { .. } | Node::Member { .. } | Node::Index { .. } => {
                let sym = match *ast.node(n) {
                    Node::Index { obj, index, .. } => match *ast.node(index) {
                        Node::StringLit { value } => {
                            let key = ast.atom(value);
                            self.resolve_entity(at(obj), meaning::VALUE)
                                .filter(|&s| self.c.sym_flags(s).intersects(SymbolFlags::ENUM))
                                .and_then(|s| self.c.export_of(s, key))
                        }
                        _ => None,
                    },
                    _ => self.resolve_entity(at(n), meaning::VALUE),
                };
                let Some(mut s) = sym else {
                    return (false, false);
                };
                if self.c.sym_flags(s).intersects(SymbolFlags::ALIAS) {
                    match self.c.resolve_alias(s) {
                        Some(t) => s = t,
                        None => return (false, false),
                    }
                }
                let Some(d) = self.c.value_decl(s) else {
                    return (false, false);
                };
                let dast = self.ast(d.file);
                let decl = if matches!(dast.node(d.node), Node::Ident { .. }) {
                    self.c.parent(d).map_or(d.node, |p| p.node)
                } else {
                    d.node
                };
                if self.c.sym_flags(s).intersects(SymbolFlags::ENUM_MEMBER) {
                    return match *dast.node(decl) {
                        Node::EnumMember { init: Some(i), .. } => {
                            self.enum_init_origin(Loc::new(d.file, i), depth + 1)
                        }
                        _ => (false, false),
                    };
                }
                if self
                    .c
                    .sym_flags(s)
                    .intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                    && self.is_const_declaration(Loc::new(d.file, decl))
                    && let Node::VarDeclarator {
                        type_ann: None,
                        init: Some(i),
                        ..
                    } = *dast.node(decl)
                {
                    if d.file != e.file {
                        return (false, true);
                    }
                    return self.enum_init_origin(Loc::new(d.file, i), depth + 1);
                }
                (false, false)
            }
            _ => (false, false),
        }
    }

    fn check_enum_initializer(
        &mut self,
        enum_decl: Loc,
        member: Loc,
        init: Loc,
        t: TypeId,
        modifiers: Modifiers,
    ) {
        let Some(es) = self.c.sym_of_decl(enum_decl) else {
            return;
        };
        let Some(ms) = self.c.sym_of_decl(member) else {
            return;
        };
        let values = self.enum_members(es);
        let Some(&(_, value)) = values.iter().find(|&&(s, _)| s == ms) else {
            return;
        };
        let is_const = modifiers.contains(Modifiers::CONST);
        // A member named before its declaration (`X = Y` with `Y` later, `E.Z` with `Z` in
        // a later block of `E`): TS2651, and tsc takes the value as 0 (no further error).
        if self.names_later_enum_member(member, init) {
            self.error(
                init,
                dg::A_MEMBER_INITIALIZER_IN_A_ENUM_DECLARATION_CANNOT_REFERENCE_MEMBERS_DECLARED_AFT_2651,
                &[],
            );
            return;
        }
        match value {
            Some(v) => {
                // With `isolatedModules`, a string value must be a string by its syntax
                // (not `a = bar` for a string constant).
                if self.c.opts.isolated_modules
                    && self.store().string_value(v).is_some()
                    && !self.enum_init_origin(init, 0).0
                {
                    let ast = self.ast(enum_decl.file);
                    let en = match *ast.node(enum_decl.node) {
                        Node::Enum { name, .. } => ident_text(ast, name).unwrap_or(""),
                        _ => "",
                    };
                    let mn = match *ast.node(member.node) {
                        Node::EnumMember { name, .. } => crate::nodes::key_text(ast, name),
                        _ => None,
                    };
                    let text = format!("{en}.{}", mn.unwrap_or_default());
                    self.error(
                        init,
                        dg::_0_HAS_A_STRING_TYPE_BUT_MUST_HAVE_SYNTACTICALLY_RECOGNIZABLE_STRING_SYNTAX_WHEN_I_18055,
                        &[&text],
                    );
                }
                if is_const && let Some(n) = self.store().number_value(v) {
                    if n.is_nan() {
                        self.error(
                            init,
                            dg::CONST_ENUM_MEMBER_INITIALIZER_WAS_EVALUATED_TO_DISALLOWED_VALUE_NAN_2478,
                            &[],
                        );
                    } else if n.is_infinite() {
                        self.error(
                            init,
                            dg::CONST_ENUM_MEMBER_INITIALIZER_WAS_EVALUATED_TO_A_NON_FINITE_VALUE_2477,
                            &[],
                        );
                    }
                }
            }
            None if is_const => {
                self.error(
                    init,
                    dg::CONST_ENUM_MEMBER_INITIALIZERS_MUST_BE_CONSTANT_EXPRESSIONS_2474,
                    &[],
                );
            }
            None => {
                if self.is_ambient_node(enum_decl)
                    || t == TypeId::UNRESOLVED
                    || self.store().is_error(t)
                {
                    return;
                }
                self.check_type_related(
                    t,
                    TypeId::NUMBER,
                    ts_r_types::RelationKind::Assignable,
                    Some(init),
                    Some(crate::relate::Head {
                        info: dg::TYPE_0_IS_NOT_ASSIGNABLE_TO_TYPE_1_AS_REQUIRED_FOR_COMPUTED_ENUM_MEMBER_VALUES_18033,
                    }),
                );
            }
        }
    }

    /// Whether initializer `init` of enum member `member` is a bare name of an enum member
    /// declared later in the same file.
    fn names_later_enum_member(&mut self, member: Loc, init: Loc) -> bool {
        let ast = self.ast(init.file);
        let n = skip_parens(ast, init.node);
        let sym = match *ast.node(n) {
            Node::Ident { .. } | Node::Member { .. } => {
                self.resolve_entity(Loc::new(init.file, n), meaning::VALUE)
            }
            // `E["Y"]`: a member of an enum named by a string.
            Node::Index { obj, index, .. } => {
                let key = match *ast.node(index) {
                    Node::StringLit { value } => Some(ast.atom(value)),
                    _ => None,
                };
                match key {
                    Some(key) => self
                        .resolve_entity(Loc::new(init.file, obj), meaning::VALUE)
                        .filter(|&e| self.c.sym_flags(e).intersects(SymbolFlags::ENUM))
                        .and_then(|e| self.c.export_of(e, key)),
                    None => None,
                }
            }
            _ => None,
        };
        let Some(sym) = sym else {
            return false;
        };
        if !self.c.sym_flags(sym).intersects(SymbolFlags::ENUM_MEMBER) {
            return false;
        }
        self.c.first_decl(sym).is_some_and(|d| {
            d.file == member.file && self.c.span(d).start > self.c.span(member).start
        })
    }

    /// Whether a constructor body assigns the field named by `key` (an identifier, `#private`
    /// or computed name). Names compare as tsc's flow references do: `this.p`, `this["p"]`
    /// and `this['p']` are one name, and so are `[k]` and `this.kk` for `const k = "kk"`;
    /// `[k]` and `this[k]` also match when `k` is one constant without a literal name
    /// (`const k = Symbol()`).
    fn assigns_this_property(&mut self, file: FileId, body: NodeId, key: NodeId) -> bool {
        let ast = self.ast(file);
        let (name, sym) = match *ast.node(key) {
            Node::ComputedKey { expr } => {
                let e = Loc::new(file, expr);
                let sym = match *ast.node(skip_parens(ast, expr)) {
                    Node::Ident { .. } => self.constant_symbol(e),
                    _ => None,
                };
                (self.access_name(e), sym)
            }
            _ => (key_text(ast, key), None),
        };
        let Some(targets) = this_assignment_targets(ast, body) else {
            return true;
        };
        if name.is_none() && sym.is_none() {
            return false;
        }
        targets.into_iter().any(|t| {
            let (got, got_sym) = match *ast.node(t) {
                Node::Member { prop, .. } => (key_text(ast, prop), None),
                Node::Index { index, .. } => {
                    let e = Loc::new(file, index);
                    let got_sym = match *ast.node(skip_parens(ast, index)) {
                        Node::Ident { .. } if sym.is_some() => self.constant_symbol(e),
                        _ => None,
                    };
                    (self.access_name(e), got_sym)
                }
                _ => (None, None),
            };
            match (&name, &got) {
                (Some(a), Some(b)) => a == b,
                _ => sym.is_some() && got_sym == sym,
            }
        })
    }

    /// The property name an element access argument or computed key names for definite
    /// assignment: a literal's value, or the literal or unique symbol type of a constant it
    /// refers to. `Symbol.iterator` is a property, so it names nothing (observed: tsc 7
    /// reports `[Symbol.iterator]` uninitialized).
    fn access_name(&mut self, e: Loc) -> Option<String> {
        let ast = self.ast(e.file);
        let n = skip_parens(ast, e.node);
        match *ast.node(n) {
            Node::StringLit { .. } | Node::NumberLit { .. } => key_text(ast, n),
            Node::TemplateLit { parts } => match ast.list(parts) {
                [p] => match *ast.node(*p) {
                    Node::TemplateElement { cooked, .. } => Some(ast.atom(cooked).to_owned()),
                    _ => None,
                },
                _ => None,
            },
            Node::Ident { .. } | Node::Member { .. } => {
                self.constant_symbol(e)?;
                self.late_bound_name(Loc::new(e.file, n))
            }
            _ => None,
        }
    }

    /// The symbol an identifier or property access `e` refers to, when it is a constant: a
    /// `const`, an enum member or an import.
    fn constant_symbol(&mut self, e: Loc) -> Option<ts_r_types::SymId> {
        let ast = self.ast(e.file);
        let n = Loc::new(e.file, skip_parens(ast, e.node));
        let sym = match *ast.node(n.node) {
            Node::Ident { .. } => self.c.resolve_ident(n, meaning::VALUE)?,
            Node::Member { .. } => self.property_symbol_of_access(n)?,
            _ => return None,
        };
        let f = self.c.sym_flags(sym);
        let constant = f.intersects(SymbolFlags::ENUM_MEMBER | SymbolFlags::ALIAS)
            || (f.intersects(SymbolFlags::BLOCK_SCOPED_VARIABLE)
                && self
                    .c
                    .value_decl(sym)
                    .is_some_and(|d| self.is_const_declaration(d)));
        constant.then_some(sym)
    }
}

/// Node's built-in modules: an unresolved import of one gets TS2591 ("install type
/// definitions for node") instead of TS2307 (list observed from tsc 7.0.2).
fn is_node_builtin_module(spec: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "assert",
        "assert/strict",
        "async_hooks",
        "buffer",
        "child_process",
        "cluster",
        "console",
        "constants",
        "crypto",
        "dgram",
        "diagnostics_channel",
        "dns",
        "dns/promises",
        "domain",
        "events",
        "fs",
        "fs/promises",
        "http",
        "http2",
        "https",
        "inspector",
        "inspector/promises",
        "module",
        "net",
        "os",
        "path",
        "path/posix",
        "path/win32",
        "perf_hooks",
        "process",
        "punycode",
        "querystring",
        "readline",
        "readline/promises",
        "repl",
        "stream",
        "stream/consumers",
        "stream/promises",
        "stream/web",
        "string_decoder",
        "sys",
        "timers",
        "timers/promises",
        "tls",
        "trace_events",
        "tty",
        "url",
        "util",
        "util/types",
        "v8",
        "vm",
        "wasi",
        "worker_threads",
        "zlib",
    ];
    const NODE_ONLY: &[&str] = &["sea", "sqlite", "test", "test/reporters"];
    match spec.strip_prefix("node:") {
        Some(rest) => BUILTINS.contains(&rest) || NODE_ONLY.contains(&rest),
        None => BUILTINS.contains(&spec),
    }
}

/// `import "x";` (no bindings): unresolved ones get TS2882 (noUncheckedSideEffectImports is
/// on by default in tsc 7).
fn is_side_effect_import(ast: &ts_r_ast::Ast, p: ts_r_ast::NodeId) -> bool {
    matches!(
        *ast.node(p),
        Node::ImportDecl {
            has_clause: false,
            ..
        }
    )
}

/// An intrinsic JSX tag (`div`, `my-el`, `svg:rect`): lowercase first letter, a dash, or a
/// namespaced name.
pub(crate) fn is_intrinsic_tag(ast: &ts_r_ast::Ast, name: NodeId) -> bool {
    match *ast.node(name) {
        Node::JsxNamespacedName { .. } => true,
        Node::Ident { name } => {
            let t = ast.atom(name);
            // `<this>` is the `this` value.
            t != "this"
                && (t.contains('-') || t.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
        }
        _ => false,
    }
}

/// Where a destructured value comes from (`Ck::pattern_root`).
#[derive(Default)]
struct PatternRoot {
    /// An enclosing element's default is an object or array literal.
    default_literal: bool,
    /// The declaration's initializer (or `for...of` expression) is a literal.
    literal: bool,
    in_array_pattern: bool,
    rest: bool,
    /// The object pattern is directly the rest element of an array pattern
    /// (`[...{ 0: a, b }]`), which reads an array (or the rest of a tuple).
    rest_of_array: bool,
    param: bool,
    optional_param: bool,
    /// An unannotated parameter of a function expression or arrow.
    contextual_param: bool,
}

/// Whether an initializer is an object or array literal, or picks one as an operand of
/// `||`, `??`, `&&` or `?:` (the literal is then typed by the binding pattern).
fn contains_literal_operand(ast: &ts_r_ast::Ast, e: NodeId, depth: u32) -> bool {
    if depth > 50 {
        return true;
    }
    let e = skip_parens(ast, e);
    match *ast.node(e) {
        Node::ObjectLit { .. } | Node::ArrayLit { .. } => true,
        Node::Binary { op, left, right } => {
            matches!(
                op,
                ts_r_ast::BinaryOp::LogicalOr
                    | ts_r_ast::BinaryOp::LogicalAnd
                    | ts_r_ast::BinaryOp::Coalesce
            ) && (contains_literal_operand(ast, left, depth + 1)
                || contains_literal_operand(ast, right, depth + 1))
        }
        Node::Conditional { cons, alt, .. } => {
            contains_literal_operand(ast, cons, depth + 1)
                || contains_literal_operand(ast, alt, depth + 1)
        }
        _ => false,
    }
}

/// The binding elements with a default value in parameter pattern `p`, outermost first
/// (TS2371 in signatures without a body). A renaming default (`{ b: c = 2 }`) is left to
/// TS2842.
fn binding_initializers(ast: &ts_r_ast::Ast, p: NodeId, out: &mut Vec<NodeId>) {
    let mut stack = vec![p];
    let mut budget = 10_000u32;
    while let Some(n) = stack.pop() {
        budget = budget.saturating_sub(1);
        if budget == 0 {
            return;
        }
        match *ast.node(n) {
            Node::ObjectPattern { props } => stack.extend(ast.list(props).iter().rev()),
            Node::ArrayPattern { elements } => stack.extend(ast.list(elements).iter().rev()),
            Node::BindingProperty { value, init, .. } => {
                let renaming = value.is_some_and(|v| matches!(ast.node(v), Node::Ident { .. }));
                if init.is_some() && !renaming {
                    out.push(n);
                }
                stack.extend(value);
            }
            Node::AssignPattern { left, .. } => {
                out.push(n);
                stack.push(left);
            }
            Node::Rest { arg } => stack.push(arg),
            _ => {}
        }
    }
}

/// Whether a namespace body has runtime content for `erasableSyntaxOnly`: anything but
/// types, non-exported `import x =` aliases, const enums and namespaces holding only those.
fn erasable_instantiated(ast: &ts_r_ast::Ast, body: NodeId, depth: u32) -> bool {
    if depth > 256 {
        return true;
    }
    let stmts = match *ast.node(body) {
        Node::ModuleBlock { stmts } => stmts,
        Node::Module { body: Some(b), .. } => return erasable_instantiated(ast, b, depth + 1),
        _ => return false,
    };
    ast.list(stmts).iter().any(|&s| match *ast.node(s) {
        Node::Interface { .. } | Node::TypeAlias { .. } | Node::ImportDecl { .. } => false,
        Node::ImportEquals {
            modifiers,
            type_only,
            ..
        } => !type_only && modifiers.contains(Modifiers::EXPORT),
        Node::Enum { modifiers, .. } => !modifiers.contains(Modifiers::CONST),
        Node::Module { body: Some(b), .. } => erasable_instantiated(ast, b, depth + 1),
        Node::Module { body: None, .. } => false,
        _ => true,
    })
}

/// Whether expression `n` refers to a declaration by name (an identifier other than a
/// property name).
fn names_a_value(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    let mut stack = vec![n];
    let mut guard = 0u32;
    while let Some(n) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            return true;
        }
        match *ast.node(n) {
            Node::Ident { .. } => return true,
            Node::Member { obj, .. } => stack.push(obj),
            _ => ast.for_each_child(n, |c| stack.push(c)),
        }
    }
    false
}

/// Whether a type node is `infer X` (through parentheses): in a rest position it is
/// implicitly constrained to an array, so it never needs a rest element or parameter check.
fn is_infer_type_node(ast: &ts_r_ast::Ast, mut ty: NodeId) -> bool {
    for _ in 0..32 {
        match *ast.node(ty) {
            Node::ParenType { ty: inner } => ty = inner,
            Node::InferType { .. } => return true,
            _ => return false,
        }
    }
    false
}

/// Whether a tuple's `...T` names an array type in its syntax (`...X[]`, also through
/// parentheses and a one-element `[...X[]]`): such an element is a rest, never TS2574.
pub(crate) fn spread_array_element_node(ast: &ts_r_ast::Ast, ty: NodeId, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    match *ast.node(ty) {
        Node::ArrayType { .. } => true,
        Node::ParenType { ty } => spread_array_element_node(ast, ty, depth + 1),
        Node::TupleType { elems } => match ast.list(elems) {
            [only] => match *ast.node(*only) {
                Node::RestType { ty } | Node::NamedTupleMember { rest: true, ty, .. } => {
                    spread_array_element_node(ast, ty, depth + 1)
                }
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
}

/// Every identifier a binding name `n` declares (`n` itself, or the names of a pattern,
/// defaults and rests included), in source order. Bounded by the pattern's size.
fn binding_name_nodes(ast: &ts_r_ast::Ast, n: NodeId) -> Vec<NodeId> {
    let mut names: Vec<NodeId> = Vec::new();
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::Ident { .. } => names.push(n),
            Node::Rest { arg } => stack.push(arg),
            Node::AssignPattern { left, .. } => stack.push(left),
            Node::ObjectPattern { props } => {
                for &p in ast.list(props).iter().rev() {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, .. } => {
                            stack.push(value.unwrap_or(key))
                        }
                        Node::Rest { arg } => stack.push(arg),
                        _ => {}
                    }
                }
            }
            Node::ArrayPattern { elements } => {
                for &e in ast.list(elements).iter().rev() {
                    stack.push(e);
                }
            }
            _ => {}
        }
    }
    names
}
